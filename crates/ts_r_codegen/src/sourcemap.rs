//! Source map V3: VLQ mappings, `names`, `sourcesContent`, JSON output and decoding (for tests
//! and for composing maps).
//!
//! Columns are UTF-16 code units, as browsers, Node and every source-map consumer expect.
//! Lines and columns are 0-based.

use std::collections::HashMap;
use std::fmt::Write as _;

/// A decoded mapping segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub gen_line: u32,
    pub gen_col: u32,
    /// `(source index, line, column, name index)`; `None` for a segment with no source.
    pub source: Option<(u32, u32, u32, Option<u32>)>,
}

/// A finished source map.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceMap {
    pub file: Option<String>,
    pub source_root: Option<String>,
    pub sources: Vec<String>,
    pub sources_content: Vec<Option<String>>,
    pub names: Vec<String>,
    /// The VLQ `mappings` string.
    pub mappings: String,
}

impl SourceMap {
    /// Serializes to JSON (keys in the conventional order).
    pub fn to_json(&self) -> String {
        let mut s = String::with_capacity(self.mappings.len() + 128);
        s.push_str("{\"version\":3");
        if let Some(f) = &self.file {
            s.push_str(",\"file\":");
            json_string(&mut s, f);
        }
        if let Some(r) = &self.source_root {
            s.push_str(",\"sourceRoot\":");
            json_string(&mut s, r);
        }
        s.push_str(",\"sources\":[");
        for (i, src) in self.sources.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            json_string(&mut s, src);
        }
        s.push(']');
        if self.sources_content.iter().any(Option::is_some) {
            s.push_str(",\"sourcesContent\":[");
            for (i, c) in self.sources_content.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                match c {
                    Some(c) => json_string(&mut s, c),
                    None => s.push_str("null"),
                }
            }
            s.push(']');
        }
        s.push_str(",\"names\":[");
        for (i, n) in self.names.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            json_string(&mut s, n);
        }
        s.push_str("],\"mappings\":");
        json_string(&mut s, &self.mappings);
        s.push('}');
        s
    }

    /// `data:application/json;base64,...` for an inline `//# sourceMappingURL=` comment.
    pub fn to_data_url(&self) -> String {
        let json = self.to_json();
        let mut s = String::with_capacity(json.len() * 4 / 3 + 40);
        s.push_str("data:application/json;base64,");
        base64_encode(json.as_bytes(), &mut s);
        s
    }

    /// Decodes `mappings` into segments. Malformed input yields the segments decoded so far.
    pub fn decode(&self) -> Vec<Mapping> {
        decode_mappings(&self.mappings)
    }
}

/// Decodes a VLQ `mappings` string.
pub fn decode_mappings(mappings: &str) -> Vec<Mapping> {
    let mut out = Vec::new();
    let (mut src, mut line, mut col, mut name) = (0i64, 0i64, 0i64, 0i64);
    for (gen_line, line_str) in mappings.split(';').enumerate() {
        let mut gen_col = 0i64;
        for seg in line_str.split(',') {
            if seg.is_empty() {
                continue;
            }
            let mut fields = [0i64; 5];
            let mut n = 0;
            let mut bytes = seg.bytes();
            while n < 5 {
                match vlq_decode(&mut bytes) {
                    Some(v) => {
                        fields[n] = v;
                        n += 1;
                    }
                    None => break,
                }
            }
            if n == 0 {
                return out;
            }
            gen_col += fields[0];
            let source = if n >= 4 {
                src += fields[1];
                line += fields[2];
                col += fields[3];
                let nm = if n == 5 {
                    name += fields[4];
                    Some(name as u32)
                } else {
                    None
                };
                Some((src as u32, line as u32, col as u32, nm))
            } else {
                None
            };
            out.push(Mapping {
                gen_line: gen_line as u32,
                gen_col: gen_col as u32,
                source,
            });
        }
    }
    out
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn vlq_encode(out: &mut String, v: i64) {
    let mut n: u64 = if v < 0 {
        ((-v as u64) << 1) | 1
    } else {
        (v as u64) << 1
    };
    loop {
        let mut digit = (n & 31) as usize;
        n >>= 5;
        if n > 0 {
            digit |= 32;
        }
        out.push(B64[digit] as char);
        if n == 0 {
            break;
        }
    }
}

fn vlq_decode(bytes: &mut std::str::Bytes<'_>) -> Option<i64> {
    let mut result: u64 = 0;
    let mut shift = 0;
    loop {
        let b = bytes.next()?;
        let d = B64.iter().position(|&c| c == b)? as u64;
        result |= (d & 31) << shift;
        if d & 32 == 0 {
            break;
        }
        shift += 5;
        if shift > 60 {
            return None;
        }
    }
    let neg = result & 1 == 1;
    let v = (result >> 1) as i64;
    Some(if neg { -v } else { v })
}

/// Appends `s` as a JSON string literal.
pub fn json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Standard base64 with padding.
pub fn base64_encode(data: &[u8], out: &mut String) {
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
}

/// Maps byte offsets in one source text to 0-based (line, UTF-16 column).
#[derive(Debug)]
pub struct LineLookup<'a> {
    src: &'a str,
    line_starts: Vec<u32>,
    /// Per line: true if the line is pure ASCII (column = byte offset).
    ascii: Vec<bool>,
    last_line: usize,
    /// Last column counted on a non-ASCII line: (line, char-boundary offset, UTF-16 column).
    /// Increasing lookups on that line count on from it instead of from the line start.
    mark: (usize, u32, u32),
}

impl<'a> LineLookup<'a> {
    pub fn new(src: &'a str) -> LineLookup<'a> {
        let bytes = src.as_bytes();
        let mut line_starts = vec![0u32];
        let mut ascii = Vec::new();
        let mut line_ascii = true;
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if b == b'\n' {
                ascii.push(line_ascii);
                line_ascii = true;
                line_starts.push(i as u32 + 1);
            } else if b == b'\r' {
                ascii.push(line_ascii);
                line_ascii = true;
                if bytes.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                line_starts.push(i as u32 + 1);
            } else if b >= 0x80 {
                line_ascii = false;
                // U+2028 / U+2029 are line terminators too.
                if b == 0xE2
                    && bytes.get(i + 1) == Some(&0x80)
                    && matches!(bytes.get(i + 2), Some(0xA8 | 0xA9))
                {
                    ascii.push(false);
                    i += 2;
                    line_starts.push(i as u32 + 1);
                }
            }
            i += 1;
        }
        ascii.push(line_ascii);
        LineLookup {
            src,
            line_starts,
            ascii,
            last_line: 0,
            mark: (0, 0, 0),
        }
    }

    /// 0-based line and UTF-16 column of `offset`. Offsets past the end count on from the
    /// last column (tsc maps the end of a missing token there).
    pub fn lookup(&mut self, offset: u32) -> (u32, u32) {
        let past = offset.saturating_sub(self.src.len() as u32);
        let offset = offset.min(self.src.len() as u32);
        let n = self.line_starts.len();
        // Mappings are mostly increasing: try the cached line and its successor first.
        let mut line = self.last_line.min(n - 1);
        let fits = |l: usize, starts: &[u32]| {
            starts[l] <= offset && (l + 1 == starts.len() || starts[l + 1] > offset)
        };
        if !fits(line, &self.line_starts) {
            if line + 1 < n && fits(line + 1, &self.line_starts) {
                line += 1;
            } else {
                line = self.line_starts.partition_point(|&s| s <= offset) - 1;
            }
        }
        self.last_line = line;
        let start = self.line_starts[line];
        let col = if self.ascii[line] {
            offset - start
        } else {
            let mut end = offset as usize;
            while end > start as usize && !self.src.is_char_boundary(end) {
                end -= 1;
            }
            let (mark_line, mark_off, mark_col) = self.mark;
            let (from, base) = if mark_line == line && mark_off as usize <= end {
                (mark_off as usize, mark_col)
            } else {
                (start as usize, 0)
            };
            let col = base
                + self.src[from..end]
                    .chars()
                    .map(|c| c.len_utf16() as u32)
                    .sum::<u32>();
            self.mark = (line, end as u32, col);
            col
        };
        (line as u32, col + past)
    }
}

/// Accumulates mappings in generated order and produces the VLQ string.
#[derive(Debug, Default)]
pub struct SourceMapBuilder {
    mappings: String,
    gen_line: u32,
    prev_gen_col: i64,
    prev_src: i64,
    prev_line: i64,
    prev_col: i64,
    prev_name: i64,
    names: Vec<String>,
    name_ids: HashMap<String, u32>,
    /// Last segment written, to drop exact duplicates.
    last: Option<(u32, u32, u32, u32)>,
    any_on_line: bool,
    /// [`SourceMapBuilder::add_tsc`]: the segment not yet written (generated line and
    /// column, source line and column).
    pending: Option<(u32, u32, u32, u32)>,
}

impl SourceMapBuilder {
    pub fn new() -> SourceMapBuilder {
        SourceMapBuilder::default()
    }

    /// Adds a segment. Generated positions must be non-decreasing.
    pub fn add(
        &mut self,
        gen_line: u32,
        gen_col: u32,
        src_line: u32,
        src_col: u32,
        name: Option<&str>,
    ) {
        if gen_line < self.gen_line {
            return;
        }
        while self.gen_line < gen_line {
            self.mappings.push(';');
            self.gen_line += 1;
            self.prev_gen_col = 0;
            self.any_on_line = false;
        }
        if (gen_col as i64) < self.prev_gen_col {
            return;
        }
        if let Some((gl, gc, sl, sc)) = self.last
            && gl == gen_line
            && (gc == gen_col || (sl == src_line && sc == src_col))
            && name.is_none()
        {
            // Same generated column (keep the first) or same source position (redundant).
            return;
        }
        if self.any_on_line {
            self.mappings.push(',');
        }
        vlq_encode(&mut self.mappings, gen_col as i64 - self.prev_gen_col);
        vlq_encode(&mut self.mappings, -self.prev_src);
        self.prev_src = 0;
        vlq_encode(&mut self.mappings, src_line as i64 - self.prev_line);
        vlq_encode(&mut self.mappings, src_col as i64 - self.prev_col);
        if let Some(name) = name {
            let id = match self.name_ids.get(name) {
                Some(&id) => id,
                None => {
                    let id = self.names.len() as u32;
                    self.names.push(name.to_owned());
                    self.name_ids.insert(name.to_owned(), id);
                    id
                }
            };
            vlq_encode(&mut self.mappings, id as i64 - self.prev_name);
            self.prev_name = id as i64;
        }
        self.prev_gen_col = gen_col as i64;
        self.prev_line = src_line as i64;
        self.prev_col = src_col as i64;
        self.last = Some((gen_line, gen_col, src_line, src_col));
        self.any_on_line = true;
    }

    /// Adds a segment the way tsc 7 merges them (observed): a segment at the generated
    /// position of the previous one replaces it unless its source position is earlier (a
    /// backtracking position starts a new segment at the same generated position). Names
    /// are never recorded (tsc's `names` is always empty). Generated positions must be
    /// non-decreasing.
    pub fn add_tsc(&mut self, gen_line: u32, gen_col: u32, src_line: u32, src_col: u32) {
        if let Some((gl, gc, sl, sc)) = self.pending {
            if gl == gen_line && gc == gen_col && (src_line, src_col) >= (sl, sc) {
                self.pending = Some((gen_line, gen_col, src_line, src_col));
                return;
            }
            self.write_raw(gl, gc, sl, sc);
        }
        self.pending = Some((gen_line, gen_col, src_line, src_col));
    }

    /// Writes one segment as is (no merging), after the ones written so far.
    fn write_raw(&mut self, gen_line: u32, gen_col: u32, src_line: u32, src_col: u32) {
        if gen_line < self.gen_line {
            return;
        }
        while self.gen_line < gen_line {
            self.mappings.push(';');
            self.gen_line += 1;
            self.prev_gen_col = 0;
            self.any_on_line = false;
        }
        if (gen_col as i64) < self.prev_gen_col {
            return;
        }
        if self.any_on_line {
            self.mappings.push(',');
        }
        vlq_encode(&mut self.mappings, gen_col as i64 - self.prev_gen_col);
        vlq_encode(&mut self.mappings, -self.prev_src);
        self.prev_src = 0;
        vlq_encode(&mut self.mappings, src_line as i64 - self.prev_line);
        vlq_encode(&mut self.mappings, src_col as i64 - self.prev_col);
        self.prev_gen_col = gen_col as i64;
        self.prev_line = src_line as i64;
        self.prev_col = src_col as i64;
        self.last = Some((gen_line, gen_col, src_line, src_col));
        self.any_on_line = true;
    }

    /// Finishes the map for one source.
    pub fn finish(
        mut self,
        file: Option<String>,
        source: String,
        content: Option<String>,
        source_root: Option<String>,
    ) -> SourceMap {
        if let Some((gl, gc, sl, sc)) = self.pending.take() {
            self.write_raw(gl, gc, sl, sc);
        }
        SourceMap {
            file,
            source_root,
            sources: vec![source],
            sources_content: vec![content],
            names: self.names,
            mappings: self.mappings,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vlq_round_trip() {
        for v in [0i64, 1, -1, 15, 16, -16, 31, 32, 1000, -1000, 123456789] {
            let mut s = String::new();
            vlq_encode(&mut s, v);
            assert_eq!(vlq_decode(&mut s.bytes()), Some(v), "{v} -> {s}");
        }
        let mut s = String::new();
        vlq_encode(&mut s, 16);
        assert_eq!(s, "gB");
    }

    #[test]
    fn builder_and_decode() {
        let mut b = SourceMapBuilder::new();
        b.add(0, 0, 0, 0, None);
        b.add(0, 6, 0, 4, Some("x"));
        b.add(2, 2, 1, 0, None);
        let map = b.finish(
            Some("a.js".into()),
            "a.ts".into(),
            Some("let x".into()),
            None,
        );
        assert_eq!(map.mappings, "AAAA,MAAIA;;EACJ");
        let d = map.decode();
        assert_eq!(d.len(), 3);
        assert_eq!(d[1].source, Some((0, 0, 4, Some(0))));
        assert_eq!(d[2].gen_line, 2);
        assert!(map.to_json().contains("\"sourcesContent\":[\"let x\"]"));
    }

    #[test]
    fn line_lookup_utf16() {
        let src = "ab\ncé𝄞x\r\ny";
        let mut l = LineLookup::new(src);
        assert_eq!(l.lookup(0), (0, 0));
        assert_eq!(l.lookup(3), (1, 0));
        let x = src.find('x').unwrap() as u32;
        assert_eq!(l.lookup(x), (1, 4));
        let y = src.find('y').unwrap() as u32;
        assert_eq!(l.lookup(y), (2, 0));
        assert_eq!(l.lookup(1), (0, 1));
    }

    /// Increasing lookups on one long non-ASCII line count on from the previous column.
    #[test]
    fn line_lookup_long_non_ascii_line() {
        let src = format!("\"é\";{}", " f();".repeat(200_000));
        let mut l = LineLookup::new(&src);
        let start = std::time::Instant::now();
        for i in 0..200_000u32 {
            assert_eq!(l.lookup(5 + 5 * i), (0, 4 + 5 * i));
        }
        assert!(start.elapsed().as_secs() < 2, "took {:?}", start.elapsed());
        // Going back still counts from the line start.
        assert_eq!(l.lookup(5), (0, 4));
        assert_eq!(l.lookup(2), (0, 1));
        assert_eq!(l.lookup(1), (0, 1));
    }

    #[test]
    fn base64() {
        let mut s = String::new();
        base64_encode(b"Man", &mut s);
        base64_encode(b"Ma", &mut s);
        base64_encode(b"M", &mut s);
        assert_eq!(s, "TWFuTWE=TQ==");
    }
}
