//! Open-document text with a line index and UTF-16 positions (the LSP and tsserver default).
//!
//! Storage is UTF-8; positions are converted only at this edge, as the root rules ask.
//! Line breaks are `\n`, `\r\n` and lone `\r` (LSP counts all three). Out-of-range positions
//! clamp: a line past the end means end of text, a character past the line end means line end.

/// A 0-based line / UTF-16 column position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    /// 0-based line.
    pub line: u32,
    /// 0-based UTF-16 code unit column.
    pub character: u32,
}

impl Pos {
    /// Builds a position.
    pub fn new(line: u32, character: u32) -> Pos {
        Pos { line, character }
    }

    /// Reads an LSP `Position` object.
    pub fn from_lsp(v: &serde_json::Value) -> Option<Pos> {
        Some(Pos {
            line: u32::try_from(v.get("line")?.as_u64()?).ok()?,
            character: u32::try_from(v.get("character")?.as_u64()?).ok()?,
        })
    }

    /// Reads a tsserver `Location` (`{line, offset}`, both 1-based).
    pub fn from_ts(v: &serde_json::Value) -> Option<Pos> {
        let line = v.get("line")?.as_u64()?;
        let offset = v.get("offset")?.as_u64()?;
        Some(Pos {
            line: u32::try_from(line.saturating_sub(1)).ok()?,
            character: u32::try_from(offset.saturating_sub(1)).ok()?,
        })
    }

    /// As an LSP `Position`.
    pub fn to_lsp(self) -> serde_json::Value {
        serde_json::json!({ "line": self.line, "character": self.character })
    }

    /// As a tsserver `Location`.
    pub fn to_ts(self) -> serde_json::Value {
        serde_json::json!({ "line": self.line + 1, "offset": self.character + 1 })
    }
}

/// A text document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextDoc {
    text: String,
    /// Byte offset of the start of each line.
    line_starts: Vec<u32>,
}

impl TextDoc {
    /// Builds a document from its full text.
    pub fn new(text: impl Into<String>) -> TextDoc {
        let text = text.into();
        let line_starts = compute_line_starts(&text);
        TextDoc { text, line_starts }
    }

    /// The full text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Number of lines (always at least 1).
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Byte range of a line's content, without its line break.
    fn line_bounds(&self, line: usize) -> (usize, usize) {
        let start = self.line_starts[line] as usize;
        let mut end = self
            .line_starts
            .get(line + 1)
            .map_or(self.text.len(), |&s| s as usize);
        let bytes = self.text.as_bytes();
        if end > start && bytes[end - 1] == b'\n' {
            end -= 1;
        }
        if end > start && bytes[end - 1] == b'\r' {
            end -= 1;
        }
        (start, end)
    }

    /// Byte offset of a UTF-16 position (clamped).
    pub fn offset_at(&self, pos: Pos) -> usize {
        let line = pos.line as usize;
        if line >= self.line_starts.len() {
            return self.text.len();
        }
        let (start, end) = self.line_bounds(line);
        let mut units = 0u32;
        for (i, ch) in self.text[start..end].char_indices() {
            if units >= pos.character {
                return start + i;
            }
            units += ch.len_utf16() as u32;
            if units > pos.character {
                // Inside a surrogate pair: snap to the start of the character.
                return start + i;
            }
        }
        end
    }

    /// UTF-16 position of a byte offset (clamped to the text, snapped to a char boundary).
    pub fn position_at(&self, offset: usize) -> Pos {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        let line = match self.line_starts.binary_search(&(offset as u32)) {
            Ok(l) => l,
            Err(l) => l - 1,
        };
        let (start, end) = self.line_bounds(line);
        let col_end = offset.min(end);
        let character = self.text[start..col_end].encode_utf16().count() as u32;
        Pos {
            line: line as u32,
            character,
        }
    }

    /// [`TextDoc::position_at`] of many offsets (any order) in one pass over each line:
    /// offsets on one line are converted from the previous one, not from the line start.
    pub fn positions_at(&self, offsets: &[usize]) -> Vec<Pos> {
        let mut order: Vec<usize> = (0..offsets.len()).collect();
        order.sort_by_key(|&i| offsets[i]);
        let mut out = vec![Pos::default(); offsets.len()];
        // (line, byte offset reached on it, UTF-16 units up to there)
        let mut at: Option<(usize, usize, u32)> = None;
        for i in order {
            let mut offset = offsets[i].min(self.text.len());
            while !self.text.is_char_boundary(offset) {
                offset -= 1;
            }
            let line = match self.line_starts.binary_search(&(offset as u32)) {
                Ok(l) => l,
                Err(l) => l - 1,
            };
            let (start, end) = self.line_bounds(line);
            let (from, units) = match at {
                Some((l, b, u)) if l == line => (b, u),
                _ => (start, 0),
            };
            let col_end = offset.min(end).max(from);
            let units = units + self.text[from..col_end].encode_utf16().count() as u32;
            at = Some((line, col_end, units));
            out[i] = Pos {
                line: line as u32,
                character: units,
            };
        }
        out
    }

    /// The position just past the last character.
    pub fn end_pos(&self) -> Pos {
        self.position_at(self.text.len())
    }

    /// Replaces `[start, end)` (UTF-16 positions) with `new_text`. `None` replaces everything.
    pub fn apply_change(&mut self, range: Option<(Pos, Pos)>, new_text: &str) {
        match range {
            None => {
                self.text.clear();
                self.text.push_str(new_text);
            }
            Some((a, b)) => {
                let (a, b) = if a <= b { (a, b) } else { (b, a) };
                let start = self.offset_at(a);
                let end = self.offset_at(b).max(start);
                self.text.replace_range(start..end, new_text);
            }
        }
        self.line_starts = compute_line_starts(&self.text);
    }

    /// Applies an LSP `TextDocumentContentChangeEvent` (`{range?, text}`).
    /// Returns false (and changes nothing) if the event is malformed.
    pub fn apply_lsp_change(&mut self, change: &serde_json::Value) -> bool {
        let Some(text) = change.get("text").and_then(|t| t.as_str()) else {
            return false;
        };
        match change.get("range") {
            None | Some(serde_json::Value::Null) => {
                self.apply_change(None, text);
                true
            }
            Some(r) => match lsp_range(r) {
                Some(range) => {
                    self.apply_change(Some(range), text);
                    true
                }
                None => false,
            },
        }
    }

    /// Applies a tsserver `CodeEdit` (`{start, end, newText}`, 1-based line/offset).
    pub fn apply_ts_edit(&mut self, edit: &serde_json::Value) -> bool {
        let (Some(start), Some(end), Some(text)) = (
            edit.get("start").and_then(Pos::from_ts),
            edit.get("end").and_then(Pos::from_ts),
            edit.get("newText").and_then(|t| t.as_str()),
        ) else {
            return false;
        };
        self.apply_change(Some((start, end)), text);
        true
    }
}

/// Reads an LSP `Range`.
pub fn lsp_range(v: &serde_json::Value) -> Option<(Pos, Pos)> {
    Some((
        Pos::from_lsp(v.get("start")?)?,
        Pos::from_lsp(v.get("end")?)?,
    ))
}

/// Builds an LSP `Range`.
pub fn to_lsp_range(start: Pos, end: Pos) -> serde_json::Value {
    serde_json::json!({ "start": start.to_lsp(), "end": end.to_lsp() })
}

/// Reads a tsserver span (`{start, end}` Locations) as an LSP range.
pub fn ts_span_to_lsp_range(v: &serde_json::Value) -> Option<serde_json::Value> {
    let start = Pos::from_ts(v.get("start")?)?;
    let end = Pos::from_ts(v.get("end")?)?;
    Some(to_lsp_range(start, end))
}

fn compute_line_starts(text: &str) -> Vec<u32> {
    let mut starts = vec![0u32];
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => starts.push(i as u32 + 1),
            b'\r' => {
                if bytes.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                starts.push(i as u32 + 1);
            }
            _ => {}
        }
        i += 1;
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn positions_ascii_and_line_breaks() {
        let d = TextDoc::new("ab\ncd\r\nef\rg");
        assert_eq!(d.line_count(), 4);
        assert_eq!(d.offset_at(Pos::new(1, 1)), 4);
        assert_eq!(d.offset_at(Pos::new(1, 99)), 5); // clamped before \r\n
        assert_eq!(d.offset_at(Pos::new(3, 0)), 10);
        assert_eq!(d.offset_at(Pos::new(9, 0)), 11);
        assert_eq!(d.position_at(4), Pos::new(1, 1));
        assert_eq!(d.position_at(6), Pos::new(1, 2)); // inside \r\n -> line end
        assert_eq!(d.end_pos(), Pos::new(3, 1));
    }

    #[test]
    fn utf16_columns() {
        // 'é' is 2 bytes / 1 unit, '😀' is 4 bytes / 2 units.
        let d = TextDoc::new("é😀x");
        assert_eq!(d.offset_at(Pos::new(0, 1)), 2);
        assert_eq!(d.offset_at(Pos::new(0, 3)), 6);
        assert_eq!(d.offset_at(Pos::new(0, 2)), 2); // inside the surrogate pair
        assert_eq!(d.position_at(6), Pos::new(0, 3));
        assert_eq!(d.position_at(3), Pos::new(0, 1)); // not a char boundary -> snapped
    }

    #[test]
    fn positions_at_matches_position_at() {
        let d = TextDoc::new("é😀x\r\nab\rc😀\n\nz");
        let offsets: Vec<usize> = (0..=d.text().len() + 2).rev().chain([3, 0, 9, 9]).collect();
        let want: Vec<Pos> = offsets.iter().map(|&o| d.position_at(o)).collect();
        assert_eq!(d.positions_at(&offsets), want);
    }

    #[test]
    fn lsp_incremental_changes() {
        let mut d = TextDoc::new("const a = 1;\nconst b = 2;\n");
        assert!(d.apply_lsp_change(&json!({
            "range": {"start": {"line":1,"character":6}, "end": {"line":1,"character":7}},
            "text": "bee"
        })));
        assert_eq!(d.text(), "const a = 1;\nconst bee = 2;\n");
        assert!(d.apply_lsp_change(&json!({
            "range": {"start": {"line":0,"character":12}, "end": {"line":1,"character":0}},
            "text": " "
        })));
        assert_eq!(d.text(), "const a = 1; const bee = 2;\n");
        assert!(d.apply_lsp_change(&json!({"text": "x"})));
        assert_eq!(d.text(), "x");
        assert!(!d.apply_lsp_change(&json!({"range": 3, "text": "y"})));
        assert_eq!(d.text(), "x");
    }

    #[test]
    fn ts_edits_are_one_based() {
        let mut d = TextDoc::new("let x = 1;\n");
        assert!(d.apply_ts_edit(&json!({
            "start": {"line":1,"offset":5}, "end": {"line":1,"offset":6}, "newText": "yy"
        })));
        assert_eq!(d.text(), "let yy = 1;\n");
        assert!(!d.apply_ts_edit(&json!({"start": {"line":1}})));
    }

    #[test]
    fn reversed_range_and_insert_at_end() {
        let mut d = TextDoc::new("abc");
        d.apply_change(Some((Pos::new(0, 2), Pos::new(0, 1))), "");
        assert_eq!(d.text(), "ac");
        d.apply_change(Some((Pos::new(5, 0), Pos::new(5, 0))), "\nz");
        assert_eq!(d.text(), "ac\nz");
        assert_eq!(d.line_count(), 2);
    }
}
