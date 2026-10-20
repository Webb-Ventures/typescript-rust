//! Source map v3 plumbing for the dev server (written for ts-r from the source map v3
//! spec): VLQ `mappings`, composing a chain of maps (plugin `transform` hooks around the
//! built-in transform, so the browser sees the original file), and shifting generated
//! columns when the server edits a module's text at render time (import URLs, the
//! `import.meta.hot`/`env` prelude on line 1, `process.env.NODE_ENV`, glob expansions).
//!
//! Columns are UTF-16 code units, as browsers read them. Edits never change a module's
//! line count (the server pads multi-line replacements with newlines), so shifting only
//! rewrites the lines an edit touches; every other line of `mappings` is copied verbatim.

use serde_json::{Value, json};

/// One mapping segment with absolute values. `src == NONE` means a generated-only segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seg {
    pub col: u32,
    pub src: u32,
    pub line: u32,
    pub ocol: u32,
    pub name: u32,
}

/// "No value" for [`Seg::src`] and [`Seg::name`].
pub const NONE: u32 = u32::MAX;

/// A decoded source map (no index maps / sections).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceMap {
    pub file: Option<String>,
    pub sources: Vec<String>,
    pub sources_content: Vec<Option<String>>,
    pub names: Vec<String>,
    /// One entry per generated line.
    pub lines: Vec<Vec<Seg>>,
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_value(c: u8) -> Option<u32> {
    Some(match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    } as u32)
}

/// Decodes one VLQ value at `*pos`; `None` on malformed input.
fn read_vlq(s: &[u8], pos: &mut usize) -> Option<i64> {
    let mut value: i64 = 0;
    let mut shift = 0u32;
    loop {
        let digit = b64_value(*s.get(*pos)?)?;
        *pos += 1;
        if shift > 60 {
            return None;
        }
        value |= i64::from(digit & 31) << shift;
        shift += 5;
        if digit & 32 == 0 {
            break;
        }
    }
    let neg = value & 1 == 1;
    value >>= 1;
    Some(if neg { -value } else { value })
}

fn write_vlq(out: &mut String, value: i64) {
    let mut v: u64 = if value < 0 {
        ((value.unsigned_abs()) << 1) | 1
    } else {
        (value as u64) << 1
    };
    loop {
        let mut digit = (v & 31) as usize;
        v >>= 5;
        if v > 0 {
            digit |= 32;
        }
        out.push(B64[digit] as char);
        if v == 0 {
            break;
        }
    }
}

fn add(base: u32, delta: i64) -> Option<u32> {
    u32::try_from(i64::from(base) + delta).ok()
}

/// Decodes a `mappings` string. Malformed segments end decoding of that line.
pub fn decode_mappings(mappings: &str) -> Vec<Vec<Seg>> {
    let s = mappings.as_bytes();
    let mut lines = Vec::new();
    let (mut src, mut line, mut ocol, mut name) = (0u32, 0u32, 0u32, 0u32);
    for text in s.split(|&c| c == b';') {
        let mut segs = Vec::new();
        let mut col = 0u32;
        for seg in text.split(|&c| c == b',') {
            if seg.is_empty() {
                continue;
            }
            let mut p = 0;
            let mut fields = [0i64; 5];
            let mut n = 0;
            while p < seg.len() && n < 5 {
                match read_vlq(seg, &mut p) {
                    Some(v) => fields[n] = v,
                    None => break,
                }
                n += 1;
            }
            let Some(c) = add(col, fields[0]).filter(|_| n >= 1) else {
                break;
            };
            col = c;
            let mut out = Seg {
                col,
                src: NONE,
                line: 0,
                ocol: 0,
                name: NONE,
            };
            if n >= 4 {
                let (Some(s2), Some(l2), Some(c2)) = (
                    add(src, fields[1]),
                    add(line, fields[2]),
                    add(ocol, fields[3]),
                ) else {
                    break;
                };
                (src, line, ocol) = (s2, l2, c2);
                out.src = src;
                out.line = line;
                out.ocol = ocol;
                if n == 5 {
                    let Some(n2) = add(name, fields[4]) else {
                        break;
                    };
                    name = n2;
                    out.name = name;
                }
            }
            segs.push(out);
        }
        lines.push(segs);
    }
    lines
}

/// Encodes decoded lines back to a `mappings` string.
pub fn encode_mappings(lines: &[Vec<Seg>]) -> String {
    let mut out = String::new();
    let (mut src, mut line, mut ocol, mut name) = (0i64, 0i64, 0i64, 0i64);
    for (i, segs) in lines.iter().enumerate() {
        if i > 0 {
            out.push(';');
        }
        let mut col = 0i64;
        for (j, s) in segs.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            write_vlq(&mut out, i64::from(s.col) - col);
            col = i64::from(s.col);
            if s.src != NONE {
                write_vlq(&mut out, i64::from(s.src) - src);
                write_vlq(&mut out, i64::from(s.line) - line);
                write_vlq(&mut out, i64::from(s.ocol) - ocol);
                src = i64::from(s.src);
                line = i64::from(s.line);
                ocol = i64::from(s.ocol);
                if s.name != NONE {
                    write_vlq(&mut out, i64::from(s.name) - name);
                    name = i64::from(s.name);
                }
            }
        }
    }
    out
}

impl SourceMap {
    /// Parses a v3 map (JSON text). `None` for index maps or malformed JSON.
    pub fn parse(text: &str) -> Option<SourceMap> {
        let v: Value = serde_json::from_str(text).ok()?;
        SourceMap::from_value(&v)
    }

    pub fn from_value(v: &Value) -> Option<SourceMap> {
        if !v["sections"].is_null() {
            return None;
        }
        let strings = |v: &Value| -> Vec<String> {
            v.as_array()
                .map(|a| {
                    a.iter()
                        .map(|s| s.as_str().unwrap_or_default().to_owned())
                        .collect()
                })
                .unwrap_or_default()
        };
        let sources = strings(&v["sources"]);
        let root = v["sourceRoot"].as_str().unwrap_or("");
        let sources = sources
            .into_iter()
            .map(|s| {
                if root.is_empty() {
                    s
                } else {
                    format!("{}/{s}", root.trim_end_matches('/'))
                }
            })
            .collect::<Vec<_>>();
        let contents = (0..sources.len())
            .map(|i| v["sourcesContent"][i].as_str().map(str::to_owned))
            .collect();
        Some(SourceMap {
            file: v["file"].as_str().map(str::to_owned),
            sources,
            sources_content: contents,
            names: strings(&v["names"]),
            lines: decode_mappings(v["mappings"].as_str()?),
        })
    }

    pub fn to_json(&self) -> String {
        let mut v = json!({
            "version": 3,
            "sources": self.sources,
            "sourcesContent": self.sources_content,
            "names": self.names,
            "mappings": encode_mappings(&self.lines),
        });
        if let Some(f) = &self.file {
            v["file"] = Value::from(f.as_str());
        }
        v.to_string()
    }

    /// The segment covering generated `(line, col)`: the last one at or before `col`.
    fn lookup(&self, line: u32, col: u32) -> Option<&Seg> {
        let segs = self.lines.get(line as usize)?;
        let i = segs.partition_point(|s| s.col <= col);
        segs[..i].last().filter(|s| s.src != NONE)
    }
}

/// Composes a chain of maps given in application order (`maps[0]` maps the first
/// transform's output to the original file; the last maps the final code to the previous
/// step's output). The result maps the final code straight to the original file(s).
/// Segments with no counterpart in an earlier map are dropped.
pub fn compose(maps: &[SourceMap]) -> Option<SourceMap> {
    let (last, rest) = maps.split_last()?;
    let mut out = last.clone();
    for inner in rest.iter().rev() {
        out = compose_two(&out, inner);
    }
    Some(out)
}

/// `outer` maps code C → B (its sources are B, treated as one file); `inner` maps B → A.
fn compose_two(outer: &SourceMap, inner: &SourceMap) -> SourceMap {
    let mut sources: Vec<String> = Vec::new();
    let mut contents: Vec<Option<String>> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut src_index: Vec<u32> = vec![NONE; inner.sources.len()];
    let mut name_index: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
    let mut lines = Vec::with_capacity(outer.lines.len());
    for segs in &outer.lines {
        let mut out = Vec::with_capacity(segs.len());
        for s in segs {
            if s.src == NONE {
                continue;
            }
            let Some(t) = inner.lookup(s.line, s.ocol) else {
                continue;
            };
            let si = t.src as usize;
            if si >= inner.sources.len() {
                continue;
            }
            if src_index[si] == NONE {
                src_index[si] = sources.len() as u32;
                sources.push(inner.sources[si].clone());
                contents.push(inner.sources_content.get(si).cloned().flatten());
            }
            // The innermost name wins (the original identifier), else the outer one.
            let name_text = inner
                .names
                .get(t.name as usize)
                .filter(|_| t.name != NONE)
                .or_else(|| outer.names.get(s.name as usize).filter(|_| s.name != NONE));
            let name = match name_text {
                Some(n) => *name_index.entry(n.as_str()).or_insert_with(|| {
                    names.push(n.clone());
                    (names.len() - 1) as u32
                }),
                None => NONE,
            };
            out.push(Seg {
                col: s.col,
                src: src_index[si],
                line: t.line,
                ocol: t.ocol,
                name,
            });
        }
        lines.push(out);
    }
    SourceMap {
        file: outer.file.clone(),
        sources,
        sources_content: contents,
        names,
        lines,
    }
}

/// One text replacement the server makes in a module at render time, in byte offsets of
/// the code before the edit. `text_newlines` must equal the newlines in the replaced range
/// (line count is preserved).
#[derive(Clone, Copy, Debug)]
pub struct TextEdit<'a> {
    pub start: usize,
    pub end: usize,
    pub text: &'a str,
}

fn utf16_len(s: &str) -> u32 {
    if s.is_ascii() {
        return s.len() as u32;
    }
    s.chars().map(|c| c.len_utf16() as u32).sum()
}

/// A column change on one generated line, in UTF-16 units of the old line: segments in
/// `[from, to)` collapse onto `at` (new column), segments at or after `to` move by `delta`.
#[derive(Clone, Copy, Debug)]
struct Shift {
    from: u32,
    to: u32,
    at: u32,
    delta: i64,
}

/// Shifts the generated columns of `map_json` for `edits` applied to `code` with `prelude`
/// prepended to line 1 (the server's render step). `edits` must be sorted and
/// non-overlapping, with replacements that keep the line count. Returns the new map JSON
/// (unchanged when there is nothing to shift, or the map's `mappings` can't be found).
pub fn shift_for_edits(
    map_json: &str,
    code: &str,
    prelude: &str,
    edits: &[TextEdit<'_>],
) -> String {
    if prelude.is_empty() && edits.is_empty() {
        return map_json.to_owned();
    }
    let Some((m_start, m_end)) = mappings_span(map_json) else {
        return map_json.to_owned();
    };
    // Per old line, the shifts (old columns, ascending).
    let mut by_line: Vec<(u32, Vec<Shift>)> = Vec::new();
    let mut push = |line: u32, s: Shift| match by_line.last_mut() {
        Some((l, v)) if *l == line => v.push(s),
        _ => by_line.push((line, vec![s])),
    };
    let mut line = 0u32;
    let mut line_start = 0usize;
    let mut scanned = 0usize;
    // Column delta on the current line from earlier edits on it (and the prelude).
    let mut line_delta: i64 = 0;
    if !prelude.is_empty() {
        line_delta = i64::from(utf16_len(prelude));
        push(
            0,
            Shift {
                from: 0,
                to: 0,
                at: 0,
                delta: line_delta,
            },
        );
    }
    for e in edits {
        let (Some(between), Some(old)) = (code.get(scanned..e.start), code.get(e.start..e.end))
        else {
            continue; // overlapping or out of range: the render step skipped it too
        };
        for (i, b) in between.bytes().enumerate() {
            if b == b'\n' {
                line += 1;
                line_start = scanned + i + 1;
                line_delta = 0;
            }
        }
        let start16 = utf16_len(&code[line_start..e.start]);
        let at = (i64::from(start16) + line_delta).max(0) as u32;
        let old_lines = old.bytes().filter(|&b| b == b'\n').count();
        if old_lines != e.text.bytes().filter(|&b| b == b'\n').count() {
            return map_json.to_owned(); // would change the line count: leave the map alone
        }
        if old_lines == 0 {
            let old16 = utf16_len(old);
            let d = i64::from(utf16_len(e.text)) - i64::from(old16);
            push(
                line,
                Shift {
                    from: start16,
                    to: start16 + old16,
                    at,
                    delta: line_delta + d,
                },
            );
            line_delta += d;
        } else {
            // Everything inside the replaced range collapses onto its start; the rest of
            // the last line follows the replacement's last line.
            push(
                line,
                Shift {
                    from: start16,
                    to: u32::MAX,
                    at,
                    delta: 0,
                },
            );
            let last_nl = old.rfind('\n').map_or(0, |i| i + 1);
            for _ in 1..old_lines {
                line += 1;
                push(
                    line,
                    Shift {
                        from: 0,
                        to: u32::MAX,
                        at: 0,
                        delta: 0,
                    },
                );
            }
            line += 1;
            line_start = e.start + last_nl;
            let end16 = utf16_len(&code[line_start..e.end]);
            let tail = utf16_len(e.text.rsplit('\n').next().unwrap_or(""));
            line_delta = i64::from(tail) - i64::from(end16);
            push(
                line,
                Shift {
                    from: 0,
                    to: end16,
                    at: 0,
                    delta: line_delta,
                },
            );
        }
        scanned = e.end;
    }
    let mappings = &map_json[m_start..m_end];
    let mut out = String::with_capacity(map_json.len() + 16);
    out.push_str(&map_json[..m_start]);
    let mut shifts = by_line.iter().peekable();
    for (i, text) in mappings.split(';').enumerate() {
        if i > 0 {
            out.push(';');
        }
        match shifts.peek() {
            Some((l, v)) if *l as usize == i => {
                shift_line(&mut out, text, v);
                shifts.next();
            }
            _ => out.push_str(text),
        }
    }
    out.push_str(&map_json[m_end..]);
    out
}

/// Rewrites the generated-column field of each segment on one line; the other fields are
/// relative to the previous segment and stay byte-for-byte.
fn shift_line(out: &mut String, text: &str, shifts: &[Shift]) {
    let mut old_col = 0i64;
    let mut new_prev = 0i64;
    for (i, seg) in text.split(',').enumerate() {
        if i > 0 {
            out.push(',');
        }
        if seg.is_empty() {
            continue;
        }
        let mut p = 0;
        let Some(delta) = read_vlq(seg.as_bytes(), &mut p) else {
            out.push_str(seg);
            continue;
        };
        old_col += delta;
        let c = old_col.max(0) as u32;
        let mut new_col = i64::from(c);
        for s in shifts {
            if c >= s.to {
                new_col = i64::from(c) + s.delta;
            } else if c >= s.from {
                new_col = i64::from(s.at);
                break;
            } else {
                break;
            }
        }
        // Columns never decrease within a line.
        let new_col = new_col.max(new_prev);
        write_vlq(out, new_col - new_prev);
        new_prev = new_col;
        out.push_str(&seg[p..]);
    }
}

/// Byte range of the `mappings` string's contents in a map's JSON text.
pub fn mappings_span(json: &str) -> Option<(usize, usize)> {
    let key = json.find("\"mappings\"")?;
    let rest = &json[key + 10..];
    let colon = rest.find(':')?;
    let after = &rest[colon + 1..];
    let quote = after.find('"')?;
    if !after[..quote].trim().is_empty() {
        return None;
    }
    let start = key + 10 + colon + 1 + quote + 1;
    let len = json[start..].find('"')?;
    Some((start, start + len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(lines: Vec<Vec<Seg>>, sources: &[&str]) -> SourceMap {
        SourceMap {
            file: None,
            sources: sources.iter().map(|s| s.to_string()).collect(),
            sources_content: vec![None; sources.len()],
            names: vec![],
            lines,
        }
    }

    fn seg(col: u32, line: u32, ocol: u32) -> Seg {
        Seg {
            col,
            src: 0,
            line,
            ocol,
            name: NONE,
        }
    }

    #[test]
    fn vlq_round_trips() {
        for m in [
            "AAAA,SAAS,CAAC;AACA",
            "AAAA;;;AACA,EAAE",
            "",
            "AAAAA,CAACA",
            "gBAAgB,kCAAkC",
        ] {
            assert_eq!(encode_mappings(&decode_mappings(m)), m, "{m}");
        }
        let lines = decode_mappings("AAAA,SAAS;AACA");
        assert_eq!(lines[0][1].col, 9);
        assert_eq!(lines[1][0].line, 1);
        // Garbage never panics.
        let _ = decode_mappings("!!!,;;\u{0}zz//++");
    }

    #[test]
    fn composes_two_maps() {
        // inner: B line 0 col 0 → A line 2 col 4; B line 0 col 5 → A line 3 col 0.
        let inner = map(vec![vec![seg(0, 2, 4), seg(5, 3, 0)]], &["a.ts"]);
        // outer: C line 1 col 2 → B line 0 col 6 (inside the second inner segment).
        let outer = map(vec![vec![], vec![seg(2, 0, 6), seg(9, 4, 0)]], &["b.js"]);
        let c = compose(&[inner, outer]).unwrap();
        assert_eq!(c.sources, vec!["a.ts"]);
        assert_eq!(c.lines[1], vec![seg(2, 3, 0)]); // the B line 4 segment has no inner line
    }

    #[test]
    fn shifts_columns_for_edits() {
        let code = "import a from './a';\nlet x = 1;\n";
        // Segments at col 0 and 14 and 19 on line 0, col 4 on line 1.
        let m = map(
            vec![
                vec![seg(0, 0, 0), seg(14, 0, 14), seg(19, 0, 19)],
                vec![seg(4, 1, 4)],
            ],
            &["a.ts"],
        );
        let json = m.to_json();
        let edits = [TextEdit {
            start: 15,
            end: 18,
            text: "/src/a.ts?t=1",
        }];
        let out = SourceMap::parse(&shift_for_edits(&json, code, "PRE;", &edits)).unwrap();
        // Prelude shifts line 1 by 4; the edit grows the specifier by 10.
        assert_eq!(out.lines[0][0].col, 4);
        assert_eq!(out.lines[0][1].col, 18); // col 14 is before the edit (15..18)
        assert_eq!(out.lines[0][2].col, 19 + 4 + 10);
        assert_eq!(out.lines[1][0].col, 4);
        // Unchanged originals.
        assert_eq!(out.lines[0][2].ocol, 19);
    }

    #[test]
    fn multi_line_edits_keep_later_lines() {
        let code = "a(\n1,\n2);\nz;\n";
        let m = map(
            vec![
                vec![seg(0, 0, 0)],
                vec![seg(0, 1, 0)],
                vec![seg(0, 2, 0), seg(3, 2, 3)],
                vec![seg(0, 3, 0)],
            ],
            &["a.ts"],
        );
        let edits = [TextEdit {
            start: 0,
            end: 8,
            text: "R\n\n",
        }];
        let out = SourceMap::parse(&shift_for_edits(&m.to_json(), code, "", &edits)).unwrap();
        assert_eq!(out.lines[3][0], seg(0, 3, 0));
        assert_eq!(out.lines[2][1].col, 1); // `;` after `2)` now right after the (empty) last line
    }

    #[test]
    fn finds_mappings() {
        let j = r#"{"version":3,"mappings" : "AAAA;AACA","sources":[]}"#;
        let (s, e) = mappings_span(j).unwrap();
        assert_eq!(&j[s..e], "AAAA;AACA");
        assert!(mappings_span("{}").is_none());
    }
}
