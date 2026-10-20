//! The edge between byte offsets (everything inside the ts-r backend) and LSP positions
//! (UTF-16 columns, lines split at `\n`, `\r\n` and lone `\r` as LSP counts them).

use crate::text::Pos;
use serde_json::{Value, json};
use ts_r_diagnostics::{Category, Diagnostic};

/// Line starts of a text under LSP's line-break rules.
#[derive(Clone, Debug, Default)]
pub struct LspLines {
    starts: Vec<u32>,
    ascii: bool,
}

impl LspLines {
    /// Indexes `text`.
    pub fn new(text: &str) -> LspLines {
        let b = text.as_bytes();
        let mut starts = Vec::with_capacity(b.len() / 32 + 1);
        starts.push(0);
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'\n' => starts.push(i as u32 + 1),
                b'\r' if b.get(i + 1) != Some(&b'\n') => starts.push(i as u32 + 1),
                _ => {}
            }
            i += 1;
        }
        LspLines {
            starts,
            ascii: text.is_ascii(),
        }
    }

    /// LSP position of byte `offset` (clamped to the text; an offset inside a character
    /// counts the character as complete).
    pub fn pos(&self, text: &str, offset: u32) -> Pos {
        let offset = offset.min(text.len() as u32);
        let line = self
            .starts
            .partition_point(|&s| s <= offset)
            .saturating_sub(1);
        let start = self.starts[line];
        if self.ascii {
            return Pos::new(line as u32, offset - start);
        }
        let mut col = 0u32;
        for (i, ch) in text.get(start as usize..).unwrap_or("").char_indices() {
            if start as usize + i >= offset as usize {
                break;
            }
            col += ch.len_utf16() as u32;
        }
        Pos::new(line as u32, col)
    }

    /// Byte offset of an LSP position (clamped: past the end of a line is its end, past the
    /// last line is the end of the text).
    pub fn offset(&self, text: &str, p: Pos) -> u32 {
        let Some(&start) = self.starts.get(p.line as usize) else {
            return text.len() as u32;
        };
        let end = self
            .starts
            .get(p.line as usize + 1)
            .copied()
            .unwrap_or(text.len() as u32);
        // Exclude the line break itself.
        let line = text.get(start as usize..end as usize).unwrap_or("");
        let content = line.trim_end_matches(['\n', '\r']);
        if self.ascii {
            return start + p.character.min(content.len() as u32);
        }
        let mut units = 0u32;
        for (i, ch) in content.char_indices() {
            if units >= p.character {
                return start + i as u32;
            }
            units += ch.len_utf16() as u32;
            if units > p.character {
                // Inside a surrogate pair: the start of the character.
                return start + i as u32;
            }
        }
        start + content.len() as u32
    }

    /// An LSP `Range` for a byte span.
    pub fn range(&self, text: &str, start: u32, end: u32) -> Value {
        json!({"start": self.pos(text, start).to_lsp(), "end": self.pos(text, end.max(start)).to_lsp()})
    }
}

/// Codes tsc marks as "unnecessary" (unused declarations, unreachable code).
const UNNECESSARY: [u32; 9] = [6133, 6138, 6192, 6196, 6198, 6199, 6205, 7027, 7028];
/// Codes tsc marks as "deprecated".
const DEPRECATED: [u32; 2] = [6385, 6387];

/// LSP severity of a tsc category.
pub fn severity(c: Category) -> u32 {
    match c {
        Category::Error => 1,
        Category::Warning => 2,
        Category::Message => 3,
        Category::Suggestion => 4,
    }
}

/// Which pass produced a diagnostic (kept in its `data` so the tsserver bridge can split
/// `syntaxDiag` from `semanticDiag`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagKind {
    /// Parser and grammar errors.
    Syntax,
    /// Binder and checker errors.
    Semantic,
}

impl DiagKind {
    /// Wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            DiagKind::Syntax => "syntax",
            DiagKind::Semantic => "semantic",
        }
    }
}

/// An LSP diagnostic for `d` in a file with `text` / `lines`. `related` maps a related
/// span's file to `(uri, text, lines)` when it is another file.
pub fn diagnostic_to_lsp(
    d: &Diagnostic,
    text: &str,
    lines: &LspLines,
    kind: DiagKind,
    related: &dyn Fn(&ts_r_diagnostics::RelatedInfo) -> Option<Value>,
) -> Value {
    let mut v = json!({
        "range": lines.range(text, d.span.start, d.span.end),
        "severity": severity(d.category),
        "code": d.code,
        "source": "ts-r",
        "message": d.message,
        "data": {"kind": kind.as_str()},
    });
    let mut tags = Vec::new();
    if UNNECESSARY.contains(&d.code) {
        tags.push(1);
    }
    if DEPRECATED.contains(&d.code) {
        tags.push(2);
    }
    if !tags.is_empty() {
        v["tags"] = json!(tags);
    }
    let rel: Vec<Value> = d.related.iter().filter_map(related).collect();
    if !rel.is_empty() {
        v["relatedInformation"] = Value::Array(rel);
    }
    v
}

/// A text edit in byte offsets of the text before it: replace `start..end` with `len` bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteEdit {
    /// Start (old text).
    pub start: u32,
    /// End (old text).
    pub end: u32,
    /// Length of the inserted text.
    pub new_len: u32,
}

/// Moves a span through a sequence of edits (each in the coordinates of the text before
/// it). `None` when an edit touches the span.
pub fn remap_span(mut start: u32, mut end: u32, edits: &[ByteEdit]) -> Option<(u32, u32)> {
    for e in edits {
        if end < e.start || (end == e.start && start < end) {
            continue;
        }
        if start > e.end || (start == e.end && e.start < e.end) {
            let delta = e.new_len as i64 - (e.end - e.start) as i64;
            start = (start as i64 + delta).max(0) as u32;
            end = (end as i64 + delta).max(0) as u32;
            continue;
        }
        return None;
    }
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_round_trip_utf16() {
        let text = "a😀b\r\nxy\rz";
        let l = LspLines::new(text);
        assert_eq!(l.pos(text, 0), Pos::new(0, 0));
        assert_eq!(l.pos(text, 5), Pos::new(0, 3)); // after the emoji (2 units)
        assert_eq!(l.pos(text, 6), Pos::new(0, 4));
        assert_eq!(l.pos(text, 8), Pos::new(1, 0));
        assert_eq!(l.pos(text, 11), Pos::new(2, 0));
        assert_eq!(l.offset(text, Pos::new(0, 3)), 5);
        assert_eq!(l.offset(text, Pos::new(0, 2)), 1); // inside the pair
        assert_eq!(l.offset(text, Pos::new(1, 9)), 10); // clamped to line end
        assert_eq!(l.offset(text, Pos::new(9, 0)), text.len() as u32);
    }

    #[test]
    fn remap_moves_and_drops() {
        let e = [ByteEdit {
            start: 10,
            end: 12,
            new_len: 5,
        }];
        assert_eq!(remap_span(0, 5, &e), Some((0, 5)));
        assert_eq!(remap_span(20, 25, &e), Some((23, 28)));
        assert_eq!(remap_span(9, 11, &e), None);
        assert_eq!(remap_span(12, 14, &e), Some((15, 17)));
    }
}
