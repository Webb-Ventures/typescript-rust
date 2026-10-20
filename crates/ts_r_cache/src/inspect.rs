//! Read-only inspection of a cache root, for `ts-r doctor`. Never creates or modifies anything.

use std::fs;
use std::path::{Path, PathBuf};

use crate::index;

/// One `<namespace>/v<format_version>` directory found under a cache root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceInfo {
    /// Namespace name (tool or data kind).
    pub namespace: String,
    /// Format version.
    pub format_version: u32,
    /// The directory.
    pub path: PathBuf,
    /// Live entries according to the index.
    pub entries: u64,
    /// Bytes of live entries according to the index.
    pub live_bytes: u64,
    /// Bytes used on disk by everything in the directory (entries, dead space in pack
    /// segments, index, journal, temp files).
    pub bytes: u64,
}

/// Lists every namespace/version under `root`, sorted by name then version.
pub fn namespaces(root: &Path) -> Vec<NamespaceInfo> {
    let mut out = Vec::new();
    for ns in entries(root) {
        let Some(name) = ns.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name == "bin" || !ns.path().is_dir() {
            continue;
        }
        for v in entries(&ns.path()) {
            let file_name = v.file_name();
            let Some(version) = file_name
                .to_str()
                .and_then(|s| s.strip_prefix('v'))
                .and_then(|s| s.parse().ok())
            else {
                continue;
            };
            let path = v.path();
            let (entries, live_bytes) = indexed_usage(&path);
            let (_, bytes) = dir_size(&path);
            out.push(NamespaceInfo {
                namespace: name.clone(),
                format_version: version,
                path,
                entries,
                live_bytes,
                bytes,
            });
        }
    }
    out.sort_by(|a, b| {
        a.namespace
            .cmp(&b.namespace)
            .then(a.format_version.cmp(&b.format_version))
    });
    out
}

/// Recursively counts regular files and their total size. Missing directory = `(0, 0)`.
/// Symlinks are not followed.
pub fn dir_size(path: &Path) -> (u64, u64) {
    let (mut files, mut bytes) = (0, 0);
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in entries(&dir) {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                files += 1;
                bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    (files, bytes)
}

/// Reads the index without locking or writing anything.
fn indexed_usage(ns_dir: &Path) -> (u64, u64) {
    let snapshot = index::Snapshot::load(&ns_dir.join("index"));
    let (recs, _) = index::read_journal(&ns_dir.join("journal"));
    let mut overlay = index::Overlay::new();
    index::fold_journal(&recs, &mut overlay, |k| snapshot.get(k));
    let all = index::merged(&snapshot, &overlay);
    (all.len() as u64, all.iter().map(|(_, r)| r.size).sum())
}

fn entries(path: &Path) -> impl Iterator<Item = fs::DirEntry> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
}

/// Formats a byte count as `12.3 MB` (decimal units, like most package managers).
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1000.0 && unit < UNITS.len() - 1 {
        v /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1500), "1.5 kB");
        assert_eq!(human_bytes(2_000_000_000), "2.0 GB");
    }

    #[test]
    fn lists_namespaces_with_index_counts() {
        let root = std::env::temp_dir().join(format!("ts-r-inspect-{}", std::process::id()));
        {
            let c = crate::Cache::open_in(&root, "lint", 2).unwrap();
            c.put(&crate::Key::of(b"a"), b"hello").unwrap();
            c.put(&crate::Key::of(b"b"), b"world").unwrap();
        }
        std::fs::create_dir_all(root.join("bin").join("0.1.0")).unwrap();
        let ns = namespaces(&root);
        assert_eq!(ns.len(), 1);
        assert_eq!(
            (ns[0].namespace.as_str(), ns[0].format_version),
            ("lint", 2)
        );
        assert_eq!(ns[0].entries, 2);
        assert_eq!(ns[0].live_bytes, 2 * (crate::HEADER_LEN as u64 + 5));
        assert!(ns[0].bytes >= ns[0].live_bytes);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_root_is_empty() {
        assert!(namespaces(Path::new("/definitely/not/here/ts-r")).is_empty());
        assert_eq!(dir_size(Path::new("/definitely/not/here/ts-r")), (0, 0));
    }
}
