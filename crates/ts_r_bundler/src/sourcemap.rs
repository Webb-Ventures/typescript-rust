//! Chunk source maps: concatenates fragment maps (each relative to its fragment, one
//! source) into one map with many sources.
//!
//! Fragments are printed from the original-source tree after the TypeScript transform, so
//! their maps already point at the original `.ts`/`.tsx` text; no map composition is
//! needed.
//!
//! The expensive part runs per fragment and in parallel ([`encode_fragment`]): a fragment's
//! segments are re-encoded relative to its own first segment, so concatenating only
//! re-encodes that first segment (and the first name) against the chunk's running state
//! ([`ChunkMap::add_encoded`]). `sourcesContent` entries are escaped per source
//! ([`ChunkMap::add_source_json`]).

use std::fmt::Write as _;

use rustc_hash::FxHashMap;
use ts_r_codegen::sourcemap::{decode_mappings, for_each_mapping, json_string};

use crate::print::FragmentMap;

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn vlq(out: &mut String, v: i64) {
    let mut n: u64 = if v < 0 {
        ((v.unsigned_abs()) << 1) | 1
    } else {
        (v as u64) << 1
    };
    loop {
        let mut d = (n & 31) as usize;
        n >>= 5;
        if n > 0 {
            d |= 32;
        }
        out.push(B64[d] as char);
        if n == 0 {
            break;
        }
    }
}

/// Builds a multi-source map in generated order.
#[derive(Debug, Default)]
pub struct ChunkMap {
    mappings: String,
    line: u32,
    prev_col: i64,
    prev_src: i64,
    prev_line: i64,
    prev_scol: i64,
    prev_name: i64,
    first_on_line: bool,
    pub sources: Vec<String>,
    /// `sourcesContent` entries as JSON string literals (already escaped).
    pub contents: Vec<Option<String>>,
    names: Vec<String>,
    name_ids: FxHashMap<String, u32>,
}

impl ChunkMap {
    pub fn new() -> ChunkMap {
        ChunkMap {
            first_on_line: true,
            ..ChunkMap::default()
        }
    }

    pub fn add_source(&mut self, name: String, content: Option<&str>) -> u32 {
        let json = content.map(|c| {
            let mut j = String::new();
            json_string(&mut j, c);
            j
        });
        self.add_source_json(name, json)
    }

    /// [`ChunkMap::add_source`] with the content already a JSON string literal.
    pub fn add_source_json(&mut self, name: String, content_json: Option<String>) -> u32 {
        self.sources.push(name);
        self.contents.push(content_json);
        (self.sources.len() - 1) as u32
    }

    /// Chunk-wide ids for a fragment's `names` (shared by equal names), in its order.
    pub fn intern_names(&mut self, names: &[String]) -> Vec<u32> {
        names
            .iter()
            .map(|n| match self.name_ids.get(n) {
                Some(&id) => id,
                None => {
                    let id = self.names.len() as u32;
                    self.names.push(n.clone());
                    self.name_ids.insert(n.clone(), id);
                    id
                }
            })
            .collect()
    }

    /// Appends a fragment encoded by [`encode_fragment`] that starts at UTF-16 column `col`
    /// of chunk line `line`, mapping into source `source`. Same result as
    /// [`ChunkMap::add_fragment`].
    pub fn add_encoded(&mut self, line: u32, col: u32, source: u32, e: &EncodedFragment) {
        let Some(h) = e.head else {
            return;
        };
        let offset = |s: &Seg| s.col + if s.line == 0 { col as i64 } else { 0 };
        self.goto_line(line + h.line);
        if !self.first_on_line {
            self.mappings.push(',');
        }
        vlq(&mut self.mappings, offset(&h) - self.prev_col);
        vlq(&mut self.mappings, source as i64 - self.prev_src);
        vlq(&mut self.mappings, h.sl - self.prev_line);
        vlq(&mut self.mappings, h.sc - self.prev_scol);
        if let Some(n) = h.name {
            vlq(&mut self.mappings, n as i64 - self.prev_name);
        }
        self.mappings.push_str(&e.body1);
        if let Some(n) = e.patch_name {
            vlq(&mut self.mappings, n as i64 - self.prev_name);
        }
        self.mappings.push_str(&e.body2);
        let end = e.end;
        self.line = line + end.line;
        self.prev_col = offset(&end);
        self.prev_src = source as i64;
        self.prev_line = end.sl;
        self.prev_scol = end.sc;
        if let Some(n) = e.last_name {
            self.prev_name = n as i64;
        }
        self.first_on_line = false;
    }

    fn goto_line(&mut self, line: u32) {
        while self.line < line {
            self.mappings.push(';');
            self.line += 1;
            self.prev_col = 0;
            self.first_on_line = true;
        }
    }

    /// Adds a fragment that starts at UTF-16 column `col` of chunk line `line`.
    pub fn add_fragment(&mut self, line: u32, col: u32, source: u32, map: &FragmentMap) {
        let segs = decode_mappings(&map.mappings);
        let mut name_map: Vec<Option<u32>> = vec![None; map.names.len()];
        for m in segs {
            let Some((_, sl, sc, nm)) = m.source else {
                continue;
            };
            let gl = line + m.gen_line;
            if gl < self.line {
                continue;
            }
            self.goto_line(gl);
            let col = m.gen_col as i64 + if m.gen_line == 0 { col as i64 } else { 0 };
            if col < self.prev_col && !self.first_on_line {
                continue;
            }
            if !self.first_on_line {
                self.mappings.push(',');
            }
            vlq(&mut self.mappings, col - self.prev_col);
            vlq(&mut self.mappings, source as i64 - self.prev_src);
            vlq(&mut self.mappings, sl as i64 - self.prev_line);
            vlq(&mut self.mappings, sc as i64 - self.prev_scol);
            if let Some(n) = nm.and_then(|n| map.names.get(n as usize).map(|s| (n, s))) {
                let id = match name_map[n.0 as usize] {
                    Some(id) => id,
                    None => {
                        let id = match self.name_ids.get(n.1) {
                            Some(&id) => id,
                            None => {
                                let id = self.names.len() as u32;
                                self.names.push(n.1.clone());
                                self.name_ids.insert(n.1.clone(), id);
                                id
                            }
                        };
                        name_map[n.0 as usize] = Some(id);
                        id
                    }
                };
                vlq(&mut self.mappings, id as i64 - self.prev_name);
                self.prev_name = id as i64;
            }
            self.prev_col = col;
            self.prev_src = source as i64;
            self.prev_line = sl as i64;
            self.prev_scol = sc as i64;
            self.first_on_line = false;
        }
    }

    /// The map as JSON. `file` is the generated file name.
    pub fn to_json(&self, file: &str, include_content: bool) -> String {
        let content_len: usize = if include_content {
            let n = |c: &Option<String>| c.as_ref().map_or(4, String::len) + 6;
            self.contents.iter().map(n).sum()
        } else {
            0
        };
        let listed: usize = self
            .sources
            .iter()
            .chain(&self.names)
            .map(|n| n.len() + 4)
            .sum();
        let mut s = String::with_capacity(self.mappings.len() + content_len + listed + 256);
        s.push_str("{\n  \"version\": 3,\n  \"file\": ");
        json_string(&mut s, file);
        s.push_str(",\n  \"sources\": [");
        for (i, src) in self.sources.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            json_string(&mut s, src);
        }
        s.push(']');
        if include_content {
            s.push_str(",\n  \"sourcesContent\": [");
            for (i, c) in self.contents.iter().enumerate() {
                if i > 0 {
                    s.push_str(",\n    ");
                }
                match c {
                    Some(c) => s.push_str(c),
                    None => s.push_str("null"),
                }
            }
            s.push(']');
        }
        // Base64 VLQ, `,` and `;` only: nothing to escape.
        s.push_str(",\n  \"mappings\": \"");
        s.push_str(&self.mappings);
        s.push('"');
        s.push_str(",\n  \"names\": [");
        for (i, n) in self.names.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            json_string(&mut s, n);
        }
        let _ = write!(s, "]\n}}\n");
        s
    }
}

/// One kept segment of a fragment: generated line and column relative to the fragment,
/// source line and column, chunk-wide name id.
#[derive(Clone, Copy, Debug, Default)]
struct Seg {
    line: u32,
    col: i64,
    sl: i64,
    sc: i64,
    name: Option<u32>,
}

/// A fragment's mappings re-encoded relative to its first segment (see [`encode_fragment`]).
#[derive(Debug, Default)]
pub struct EncodedFragment {
    /// The first segment, encoded by [`ChunkMap::add_encoded`] against the chunk's state.
    head: Option<Seg>,
    /// The segments after `head`, up to the name field of the first named one (when `head`
    /// has no name): that name's delta depends on the chunk's state too.
    body1: String,
    patch_name: Option<u32>,
    body2: String,
    end: Seg,
    last_name: Option<u32>,
}

/// Re-encodes a fragment's mappings for [`ChunkMap::add_encoded`]; `ids` maps the
/// fragment's name indices to chunk-wide ids ([`ChunkMap::intern_names`]). Segments with no
/// source, and segments that go back on a line, are dropped as [`ChunkMap::add_fragment`]
/// drops them.
pub fn encode_fragment(map: &FragmentMap, ids: &[u32]) -> EncodedFragment {
    let mut e = EncodedFragment::default();
    let mut out = String::with_capacity(map.mappings.len());
    let mut prev: Option<Seg> = None;
    let mut prev_name: Option<u32> = None;
    for_each_mapping(&map.mappings, |m| {
        let Some((_, sl, sc, nm)) = m.source else {
            return;
        };
        let seg = Seg {
            line: m.gen_line,
            col: m.gen_col as i64,
            sl: sl as i64,
            sc: sc as i64,
            name: nm.and_then(|n| ids.get(n as usize).copied()),
        };
        if let Some(p) = prev {
            if seg.line == p.line && seg.col < p.col {
                return;
            }
            let base = if seg.line > p.line {
                for _ in p.line..seg.line {
                    out.push(';');
                }
                0
            } else {
                out.push(',');
                p.col
            };
            vlq(&mut out, seg.col - base);
            vlq(&mut out, 0);
            vlq(&mut out, seg.sl - p.sl);
            vlq(&mut out, seg.sc - p.sc);
            if let Some(n) = seg.name {
                match prev_name {
                    Some(pn) => vlq(&mut out, n as i64 - pn as i64),
                    None => {
                        e.body1 = std::mem::take(&mut out);
                        e.patch_name = Some(n);
                    }
                }
            }
        } else {
            e.head = Some(seg);
        }
        if seg.name.is_some() {
            prev_name = seg.name;
        }
        prev = Some(seg);
    });
    if e.patch_name.is_some() {
        e.body2 = out;
    } else {
        e.body1 = out;
    }
    e.end = prev.unwrap_or_default();
    e.last_name = prev_name;
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_fragments() {
        let mut m = ChunkMap::new();
        let a = m.add_source("a.ts".into(), Some("x"));
        let b = m.add_source("b.ts".into(), None);
        // "AAAA" = col 0 -> src 0 line 0 col 0.
        m.add_fragment(
            1,
            0,
            a,
            &FragmentMap {
                mappings: "AAAA".into(),
                names: vec![],
            },
        );
        m.add_fragment(
            3,
            0,
            b,
            &FragmentMap {
                mappings: "AAAA;AACA".into(),
                names: vec![],
            },
        );
        let d = decode_mappings(&m.mappings);
        let flat: Vec<(u32, u32, u32, u32)> = d
            .iter()
            .filter_map(|x| x.source.map(|(s, l, c, _)| (x.gen_line, s, l, c)))
            .collect();
        assert_eq!(flat, vec![(1, 0, 0, 0), (3, 1, 0, 0), (4, 1, 1, 0)]);
        assert!(m.to_json("out.js", true).contains("\"sourcesContent\""));
    }

    #[test]
    fn encoded_fragments_match_add_fragment() {
        let frags = [
            FragmentMap {
                mappings: "AAAA,CAAC,EAAEA;AACA,GAACC;;AAAA".into(),
                names: vec!["x".into(), "y".into()],
            },
            FragmentMap {
                mappings: "AAAA,CAACA,CAAC,CAACC;AACA,A,CAAC;CAAC".into(),
                names: vec!["y".into(), "z".into()],
            },
            FragmentMap {
                mappings: "".into(),
                names: vec![],
            },
            FragmentMap {
                mappings: "KAAK,EAAE;AAAA,CAAC".into(),
                names: vec![],
            },
        ];
        let places = [(0, 0), (3, 17), (5, 0), (5, 5)];
        let mut slow = ChunkMap::new();
        let mut fast = ChunkMap::new();
        for (i, f) in frags.iter().enumerate() {
            let (line, col) = places[i];
            let si = slow.add_source(format!("{i}.ts"), None);
            slow.add_fragment(line, col, si, f);
            let si = fast.add_source(format!("{i}.ts"), None);
            let ids = fast.intern_names(&f.names);
            fast.add_encoded(line, col, si, &encode_fragment(f, &ids));
        }
        assert_eq!(
            decode_mappings(&fast.mappings),
            decode_mappings(&slow.mappings)
        );
        assert_eq!(fast.mappings, slow.mappings);
        assert_eq!(fast.names, slow.names);
    }
}
