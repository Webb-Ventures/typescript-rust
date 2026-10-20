//! Source text, byte spans and line/column mapping.
//!
//! Positions everywhere in ts-r are UTF-8 byte offsets (`u32`). This crate converts them to
//! line/column pairs (UTF-8 bytes or UTF-16 code units) at the edges: diagnostics output and LSP.

use std::fmt;
use std::sync::OnceLock;

/// A half-open byte range `[start, end)` into a source text.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub const EMPTY: Span = Span { start: 0, end: 0 };

    #[inline]
    pub const fn new(start: u32, end: u32) -> Span {
        Span { start, end }
    }

    /// A zero-width span at `pos`.
    #[inline]
    pub const fn at(pos: u32) -> Span {
        Span {
            start: pos,
            end: pos,
        }
    }

    #[inline]
    pub const fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.end <= self.start
    }

    /// The smallest span covering both.
    #[inline]
    pub fn cover(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    #[inline]
    pub fn contains(self, pos: u32) -> bool {
        self.start <= pos && pos < self.end
    }

    #[inline]
    pub fn contains_span(self, other: Span) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    /// Returns the text this span covers, or `""` if it is out of range or not on a char
    /// boundary.
    pub fn text(self, src: &str) -> &str {
        src.get(self.start as usize..self.end as usize)
            .unwrap_or("")
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

/// A zero-based line and column. The column unit depends on the method that produced it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, PartialOrd, Ord)]
pub struct LineCol {
    pub line: u32,
    pub col: u32,
}

/// Line start offsets of a text, for byte offset <-> line/column conversion.
///
/// Line terminators follow ECMAScript: `\n`, `\r\n`, `\r`, U+2028 and U+2029.
#[derive(Clone, Debug, Default)]
pub struct LineIndex {
    /// Byte offset of the first byte of each line. Always starts with 0.
    line_starts: Vec<u32>,
    /// Total length of the text in bytes.
    len: u32,
    /// True if the text is pure ASCII, which makes UTF-16 columns equal to byte columns.
    ascii: bool,
}

impl LineIndex {
    pub fn new(text: &str) -> LineIndex {
        let bytes = text.as_bytes();
        let mut line_starts = Vec::with_capacity(bytes.len() / 32 + 1);
        line_starts.push(0);
        let mut ascii = true;
        let mut i = 0usize;
        while i < bytes.len() {
            let Some(off) = memchr::memchr3(b'\n', b'\r', 0xE2, &bytes[i..]) else {
                break;
            };
            let j = i + off;
            match bytes[j] {
                b'\n' => {
                    line_starts.push((j + 1) as u32);
                    i = j + 1;
                }
                b'\r' => {
                    if bytes.get(j + 1) == Some(&b'\n') {
                        line_starts.push((j + 2) as u32);
                        i = j + 2;
                    } else {
                        line_starts.push((j + 1) as u32);
                        i = j + 1;
                    }
                }
                _ => {
                    // U+2028 / U+2029 are E2 80 A8 / E2 80 A9.
                    if bytes.get(j + 1) == Some(&0x80)
                        && matches!(bytes.get(j + 2), Some(&0xA8) | Some(&0xA9))
                    {
                        line_starts.push((j + 3) as u32);
                        i = j + 3;
                    } else {
                        i = j + 1;
                    }
                }
            }
        }
        if !text.is_ascii() {
            ascii = false;
        }
        LineIndex {
            line_starts,
            len: bytes.len() as u32,
            ascii,
        }
    }

    /// Number of lines (at least 1).
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Byte offset where zero-based `line` starts, or `None` if out of range.
    pub fn line_start(&self, line: u32) -> Option<u32> {
        self.line_starts.get(line as usize).copied()
    }

    /// Byte range of zero-based `line`, including its terminator.
    pub fn line_span(&self, line: u32) -> Option<Span> {
        let start = self.line_start(line)?;
        let end = self
            .line_starts
            .get(line as usize + 1)
            .copied()
            .unwrap_or(self.len);
        Some(Span::new(start, end))
    }

    /// Zero-based line containing byte `offset` (clamped to the text).
    pub fn line_of(&self, offset: u32) -> u32 {
        let offset = offset.min(self.len);
        match self.line_starts.binary_search(&offset) {
            Ok(i) => i as u32,
            Err(i) => (i - 1) as u32,
        }
    }

    /// Line and UTF-8 byte column of `offset`.
    pub fn line_col(&self, offset: u32) -> LineCol {
        let offset = offset.min(self.len);
        let line = self.line_of(offset);
        LineCol {
            line,
            col: offset - self.line_starts[line as usize],
        }
    }

    /// Line and UTF-16 code-unit column of `offset`. `text` must be the text this index was
    /// built from. An offset inside a multi-byte character counts the character as complete.
    pub fn line_col_utf16(&self, text: &str, offset: u32) -> LineCol {
        let lc = self.line_col(offset);
        if self.ascii {
            return lc;
        }
        let start = self.line_starts[lc.line as usize] as usize;
        let end = (offset.min(self.len)) as usize;
        let mut col = 0u32;
        for (i, ch) in text.get(start..).unwrap_or("").char_indices() {
            if start + i >= end {
                break;
            }
            col += ch.len_utf16() as u32;
        }
        LineCol { line: lc.line, col }
    }

    /// Byte offset of a line and UTF-8 byte column, clamped to the line's end.
    pub fn offset(&self, lc: LineCol) -> Option<u32> {
        let span = self.line_span(lc.line)?;
        Some((span.start + lc.col).min(span.end))
    }

    /// Byte offset of a line and UTF-16 column, clamped to the line's end. A column that falls
    /// inside a surrogate pair maps to the start of that character.
    pub fn offset_utf16(&self, text: &str, lc: LineCol) -> Option<u32> {
        let span = self.line_span(lc.line)?;
        if self.ascii {
            return Some((span.start + lc.col).min(span.end));
        }
        let line_text = text
            .get(span.start as usize..span.end as usize)
            .unwrap_or("");
        let mut units = 0u32;
        for (i, ch) in line_text.char_indices() {
            let w = ch.len_utf16() as u32;
            if units + w > lc.col {
                return Some(span.start + i as u32);
            }
            units += w;
        }
        Some(span.end)
    }
}

/// A named source file with a lazily built line index.
#[derive(Debug)]
pub struct SourceText {
    name: String,
    text: String,
    lines: OnceLock<LineIndex>,
}

impl SourceText {
    /// Creates a source. Texts longer than `u32::MAX` bytes are not supported; callers should
    /// reject them before parsing.
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> SourceText {
        SourceText {
            name: name.into(),
            text: text.into(),
            lines: OnceLock::new(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn len(&self) -> u32 {
        self.text.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn line_index(&self) -> &LineIndex {
        self.lines.get_or_init(|| LineIndex::new(&self.text))
    }

    pub fn slice(&self, span: Span) -> &str {
        span.text(&self.text)
    }

    pub fn line_col(&self, offset: u32) -> LineCol {
        self.line_index().line_col(offset)
    }

    pub fn line_col_utf16(&self, offset: u32) -> LineCol {
        self.line_index().line_col_utf16(&self.text, offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_basics() {
        let s = Span::new(2, 5);
        assert_eq!(s.len(), 3);
        assert!(!s.is_empty());
        assert!(s.contains(2) && s.contains(4) && !s.contains(5));
        assert_eq!(s.cover(Span::new(7, 9)), Span::new(2, 9));
        assert_eq!(s.text("abcdefg"), "cde");
        assert_eq!(Span::new(5, 100).text("abc"), "");
        assert!(Span::at(3).is_empty());
    }

    #[test]
    fn line_starts_all_terminators() {
        let text = "a\nb\r\nc\rd\u{2028}e\u{2029}f";
        let idx = LineIndex::new(text);
        assert_eq!(idx.line_count(), 6);
        let f = text.find('f').unwrap() as u32;
        assert_eq!(idx.line_col(f), LineCol { line: 5, col: 0 });
        let c = text.find('c').unwrap() as u32;
        assert_eq!(idx.line_col(c), LineCol { line: 2, col: 0 });
        // `\r\n` is one terminator.
        assert_eq!(idx.line_col(3), LineCol { line: 1, col: 1 });
    }

    #[test]
    fn utf16_columns() {
        // 'é' is 2 bytes / 1 unit; '😀' is 4 bytes / 2 units.
        let text = "x\né😀z";
        let idx = LineIndex::new(text);
        let z = text.find('z').unwrap() as u32;
        assert_eq!(idx.line_col(z), LineCol { line: 1, col: 6 });
        assert_eq!(idx.line_col_utf16(text, z), LineCol { line: 1, col: 3 });
        assert_eq!(idx.offset_utf16(text, LineCol { line: 1, col: 3 }), Some(z));
        assert_eq!(idx.offset(LineCol { line: 1, col: 6 }), Some(z));
        // Column inside the surrogate pair maps to the char start.
        let emoji = text.find('😀').unwrap() as u32;
        assert_eq!(
            idx.offset_utf16(text, LineCol { line: 1, col: 2 }),
            Some(emoji)
        );
        // Past end of line clamps.
        assert_eq!(
            idx.offset_utf16(text, LineCol { line: 1, col: 99 }),
            Some(text.len() as u32)
        );
        assert_eq!(idx.offset(LineCol { line: 9, col: 0 }), None);
    }

    #[test]
    fn source_text() {
        let src = SourceText::new("a.ts", "let x;\nlet y;");
        assert_eq!(src.line_col(8), LineCol { line: 1, col: 1 });
        assert_eq!(src.slice(Span::new(4, 5)), "x");
        assert_eq!(src.line_index().line_span(0), Some(Span::new(0, 7)));
        assert_eq!(src.line_col(1000), LineCol { line: 1, col: 6 });
    }

    #[test]
    fn empty_text() {
        let idx = LineIndex::new("");
        assert_eq!(idx.line_count(), 1);
        assert_eq!(idx.line_col(0), LineCol::default());
        assert_eq!(idx.line_col_utf16("", 5), LineCol::default());
    }
}
