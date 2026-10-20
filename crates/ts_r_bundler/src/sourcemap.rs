//! Chunk source maps: concatenates fragment maps (each relative to its fragment, one
//! source) into one map with many sources.
//!
//! Fragments are printed from the original-source tree after the TypeScript transform, so
//! their maps already point at the original `.ts`/`.tsx` text; no map composition is
//! needed.

use std::fmt::Write as _;

use rustc_hash::FxHashMap;
use ts_r_codegen::sourcemap::{decode_mappings, json_string};

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

    pub fn add_source(&mut self, name: String, content: Option<String>) -> u32 {
        self.sources.push(name);
        self.contents.push(content);
        (self.sources.len() - 1) as u32
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
        let mut s = String::with_capacity(self.mappings.len() + 256);
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
                    Some(c) => json_string(&mut s, c),
                    None => s.push_str("null"),
                }
            }
            s.push(']');
        }
        s.push_str(",\n  \"mappings\": ");
        json_string(&mut s, &self.mappings);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_fragments() {
        let mut m = ChunkMap::new();
        let a = m.add_source("a.ts".into(), Some("x".into()));
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
}
