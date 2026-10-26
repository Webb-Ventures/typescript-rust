//! The files that hand a compiled graph to Node, and the stamp that lets a warm run skip
//! everything but `stat`.
//!
//! Manifest (`<hash>.graph`): one JSON line, then every module's code back to back.
//!
//! ```text
//! {"f":{"/abs/a.ts":[0,0,1234],...},"r":{"/abs/a.ts":{"./b.js":"/abs/b.ts"},...}}\n<code...>
//! ```
//!
//! `f` maps a file to `[format (0 ESM, 1 CJS), start, end]`, byte offsets into the code
//! after the newline; an ES module also compiled to CommonJS for `require()` has that under
//! its path plus `\0cjs` (`graph::CJS_VARIANT`), in `f` and `r`; `r` holds the specifiers ts-r-run resolved; `e`, when present, maps a
//! module only loaded lazily that failed to compile to its rendered errors. The name is the BLAKE3
//! hash of the contents, so an unchanged graph is never rewritten.
//!
//! Stamp (`<key>.stamp`, key from the tool build, entry and flags): the manifest path, then
//! `size\tmtime_ns\tpath` for every input. When every input still matches, the run starts
//! Node right away.

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use ts_r_cache::{Key, KeyBuilder};

use crate::compile::Format;
use crate::graph::Graph;

/// `<cache root>/run-graphs`.
pub fn default_dir() -> PathBuf {
    ts_r_cache::cache_root().join("run-graphs")
}

pub fn json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The manifest bytes for `g`.
pub fn render(g: &Graph) -> Vec<u8> {
    let mut index = String::from("{\"f\":{");
    let mut offset = 0usize;
    for (i, m) in g.modules.iter().enumerate() {
        if i > 0 {
            index.push(',');
        }
        json_string(&mut index, &m.key());
        let fmt = match m.format {
            Format::Esm => 0,
            Format::Cjs => 1,
        };
        let _ = write!(index, ":[{fmt},{offset},{}]", offset + m.code.len());
        offset += m.code.len();
    }
    index.push_str("},\"r\":{");
    let mut first = true;
    for m in g.modules.iter().filter(|m| !m.resolved.is_empty()) {
        if !first {
            index.push(',');
        }
        first = false;
        json_string(&mut index, &m.key());
        index.push_str(":{");
        for (j, (spec, target)) in m.resolved.iter().enumerate() {
            if j > 0 {
                index.push(',');
            }
            json_string(&mut index, spec);
            index.push(':');
            json_string(&mut index, target);
        }
        index.push('}');
    }
    index.push('}');
    if !g.deferred.is_empty() {
        index.push_str(",\"e\":{");
        for (i, (path, error)) in g.deferred.iter().enumerate() {
            if i > 0 {
                index.push(',');
            }
            json_string(&mut index, path);
            index.push(':');
            json_string(&mut index, error);
        }
        index.push('}');
    }
    index.push_str("}\n");
    let mut out = Vec::with_capacity(index.len() + offset);
    out.extend_from_slice(index.as_bytes());
    for m in &g.modules {
        out.extend_from_slice(m.code.as_bytes());
    }
    out
}

/// Writes `bytes` to `dir/<name>` atomically (temp file + rename) unless it already exists.
pub fn write_once(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let path = dir.join(name);
    if path.exists() {
        return Ok(path);
    }
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Writes the manifest for `g` into `dir`; returns its path.
pub fn write(dir: &Path, g: &Graph) -> io::Result<PathBuf> {
    let bytes = render(g);
    let name = format!("{}.graph", Key::of(&bytes).to_hex());
    let path = write_once(dir, &name, &bytes)?;
    prune(dir);
    Ok(path)
}

/// Deletes manifests and stamps not used for a day once there are more than 256 of them.
fn prune(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let entries: Vec<_> = rd.filter_map(Result::ok).collect();
    if entries.len() <= 256 {
        return;
    }
    let day = std::time::Duration::from_secs(24 * 3600);
    for e in entries {
        let old = e
            .metadata()
            .and_then(|m| m.accessed().or_else(|_| m.modified()))
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > day);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The stamp key for a run.
pub fn stamp_key(fingerprint: &str, entry: &str, tsconfig: Option<&str>, source_maps: bool) -> Key {
    KeyBuilder::new("ts-r-run/stamp")
        .str(fingerprint)
        .str(entry)
        .str(tsconfig.unwrap_or(""))
        .u64(u64::from(source_maps))
        .key()
}

fn file_stamp(path: &str) -> Option<(u64, u128)> {
    let m = std::fs::metadata(path).ok()?;
    let mtime = m
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((m.len(), mtime))
}

/// Records `inputs` for the manifest at `manifest`.
pub fn write_stamp(dir: &Path, key: &Key, manifest: &Path, inputs: &[&str]) -> io::Result<()> {
    let mut s = String::new();
    s.push_str(&manifest.to_string_lossy());
    s.push('\n');
    for p in inputs {
        // A file that can't be stat'ed now never matches later, so the stamp is never used.
        let (len, mtime) = file_stamp(p).unwrap_or((u64::MAX, 0));
        let _ = writeln!(s, "{len}\t{mtime}\t{p}");
    }
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.stamp", key.to_hex()));
    let tmp = dir.join(format!(".{}.{}.tmp", key.to_hex(), std::process::id()));
    std::fs::write(&tmp, s)?;
    std::fs::rename(tmp, path)
}

/// The manifest recorded under `key` if no input changed since.
pub fn check_stamp(dir: &Path, key: &Key) -> Option<PathBuf> {
    let text = std::fs::read_to_string(dir.join(format!("{}.stamp", key.to_hex()))).ok()?;
    let mut lines = text.lines();
    let manifest = PathBuf::from(lines.next()?);
    for line in lines {
        let mut parts = line.splitn(3, '\t');
        let len: u64 = parts.next()?.parse().ok()?;
        let mtime: u128 = parts.next()?.parse().ok()?;
        let path = parts.next()?;
        if file_stamp(path)? != (len, mtime) {
            return None;
        }
    }
    manifest.exists().then_some(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Module;
    use std::sync::Arc;

    #[test]
    fn manifest_layout() {
        let g = Graph {
            modules: vec![
                Module {
                    path: "/p/a.ts".into(),
                    format: Format::Esm,
                    code: "import './b.js';\n".into(),
                    resolved: vec![("./b.js".into(), Arc::from("/p/b.ts"))],
                    eager: vec![Arc::from("/p/b.ts")],
                    forced: false,
                    imported: Vec::new(),
                    required: Vec::new(),
                },
                Module {
                    path: "/p/\"b\".ts".into(),
                    format: Format::Cjs,
                    code: "x\n".into(),
                    resolved: Vec::new(),
                    eager: Vec::new(),
                    forced: false,
                    imported: Vec::new(),
                    required: Vec::new(),
                },
                Module {
                    path: "/p/a.ts".into(),
                    format: Format::Cjs,
                    code: "y\n".into(),
                    resolved: vec![("./b.js".into(), Arc::from("/p/b.ts"))],
                    eager: Vec::new(),
                    forced: true,
                    imported: Vec::new(),
                    required: Vec::new(),
                },
            ],
            ..Graph::default()
        };
        let bytes = render(&g);
        let text = String::from_utf8(bytes).unwrap();
        let (index, code) = text.split_once('\n').unwrap();
        assert_eq!(
            index,
            r#"{"f":{"/p/a.ts":[0,0,17],"/p/\"b\".ts":[1,17,19],"/p/a.ts\u0000cjs":[1,19,21]},"r":{"/p/a.ts":{"./b.js":"/p/b.ts"},"/p/a.ts\u0000cjs":{"./b.js":"/p/b.ts"}}}"#
        );
        assert_eq!(&code[17..19], "x\n");
    }

    #[test]
    fn stamp_detects_changes() {
        let dir = std::env::temp_dir().join(format!("ts-r-run-stamp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("a.ts");
        std::fs::write(&src, "1").unwrap();
        let manifest = write_once(&dir, "m.graph", b"{}\n").unwrap();
        let key = stamp_key("fp", "/e.ts", None, true);
        let src_s = src.to_string_lossy().to_string();
        write_stamp(&dir, &key, &manifest, &[&src_s]).unwrap();
        assert_eq!(check_stamp(&dir, &key), Some(manifest.clone()));
        std::fs::write(&src, "22").unwrap();
        assert_eq!(check_stamp(&dir, &key), None);
        assert_eq!(
            check_stamp(&dir, &stamp_key("fp", "/other.ts", None, true)),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
