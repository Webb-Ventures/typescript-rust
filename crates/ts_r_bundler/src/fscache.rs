//! A caching [`FileSystem`] wrapper for builds: every directory is listed once and existence
//! checks are answered from the listing; directory real paths are cached and a file's real
//! path is its directory's real path plus its name. On macOS this replaces thousands of
//! `stat`/`open`/`getattrlist` calls per build with one `readdir` per directory.
//!
//! File contents are never cached here. Call [`CachedFs::clear`] when files may have been
//! added or removed (the build context does on `invalidate`).

use std::sync::{Arc, RwLock};

use rustc_hash::FxHashMap;
use ts_r_config::fs::{DirEntry, EntryKind};
use ts_r_config::{FileSystem, path};

type Listing = Option<Arc<FxHashMap<String, EntryKind>>>;

pub struct CachedFs {
    inner: Arc<dyn FileSystem>,
    dirs: RwLock<FxHashMap<String, Listing>>,
    real_dirs: RwLock<FxHashMap<String, String>>,
}

impl std::fmt::Debug for CachedFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedFs").finish_non_exhaustive()
    }
}

impl CachedFs {
    pub fn new(inner: Arc<dyn FileSystem>) -> CachedFs {
        CachedFs {
            inner,
            dirs: RwLock::default(),
            real_dirs: RwLock::default(),
        }
    }

    /// Forgets every listing and real path.
    pub fn clear(&self) {
        self.dirs.write().unwrap_or_else(|e| e.into_inner()).clear();
        self.real_dirs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// Forgets one directory's listing (a file in it was added, removed or changed).
    pub fn forget(&self, dir: &str) {
        self.dirs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(dir);
    }

    fn listing(&self, dir: &str) -> Listing {
        if let Some(l) = self.dirs.read().unwrap_or_else(|e| e.into_inner()).get(dir) {
            return l.clone();
        }
        let l = if self.inner.directory_exists(dir) {
            Some(Arc::new(
                self.inner
                    .read_dir(dir)
                    .into_iter()
                    .map(|e| (e.name, e.kind))
                    .collect(),
            ))
        } else {
            None
        };
        self.dirs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(dir.to_owned(), l.clone());
        l
    }

    /// The kind of `p`, from its parent's listing. `None` for the root (ask the inner FS).
    fn kind(&self, p: &str) -> Option<Option<EntryKind>> {
        let dir = path::directory_of(p);
        if dir.is_empty() || dir == p {
            return None;
        }
        let name = &p[dir.len()..].trim_start_matches('/');
        if name.is_empty() || name.contains('/') {
            return None;
        }
        Some(self.listing(dir).and_then(|l| l.get(*name).copied()))
    }

    fn real_dir(&self, dir: &str) -> String {
        if let Some(r) = self
            .real_dirs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(dir)
        {
            return r.clone();
        }
        let r = self.inner.realpath(dir);
        self.real_dirs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(dir.to_owned(), r.clone());
        r
    }
}

impl FileSystem for CachedFs {
    fn read_file(&self, path: &str) -> Option<String> {
        if self.kind(path) == Some(None) {
            return None;
        }
        self.inner.read_file(path)
    }

    fn file_exists(&self, path: &str) -> bool {
        match self.kind(path) {
            Some(k) => k == Some(EntryKind::File),
            None => self.inner.file_exists(path),
        }
    }

    fn directory_exists(&self, path: &str) -> bool {
        match self.kind(path) {
            Some(k) => k == Some(EntryKind::Directory),
            None => self.inner.directory_exists(path),
        }
    }

    fn read_dir(&self, path: &str) -> Vec<DirEntry> {
        let Some(l) = self.listing(path) else {
            return Vec::new();
        };
        let mut v: Vec<DirEntry> = l
            .iter()
            .map(|(name, kind)| DirEntry {
                name: name.clone(),
                kind: *kind,
            })
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    fn realpath(&self, p: &str) -> String {
        match self.kind(p) {
            Some(Some(EntryKind::Directory)) => self.real_dir(p),
            Some(Some(EntryKind::File)) => {
                let dir = path::directory_of(p);
                let name = p[dir.len()..].trim_start_matches('/');
                let real = self.real_dir(dir);
                if real.ends_with('/') {
                    format!("{real}{name}")
                } else {
                    format!("{real}/{name}")
                }
            }
            _ => self.inner.realpath(p),
        }
    }

    fn case_sensitive(&self) -> bool {
        self.inner.case_sensitive()
    }

    fn cwd(&self) -> String {
        self.inner.cwd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_r_config::MemoryFs;

    #[test]
    fn answers_from_listings() {
        let mut m = MemoryFs::new();
        m.add_file("/p/a.js", "1").add_file("/p/d/b.js", "2");
        let fs = CachedFs::new(Arc::new(m));
        assert!(fs.file_exists("/p/a.js"));
        assert!(!fs.file_exists("/p/x.js"));
        assert!(fs.directory_exists("/p/d"));
        assert!(!fs.file_exists("/p/d"));
        assert_eq!(fs.read_file("/p/d/b.js").as_deref(), Some("2"));
        assert_eq!(fs.read_file("/p/nope.js"), None);
        assert_eq!(fs.realpath("/p/d/b.js"), "/p/d/b.js");
        assert_eq!(fs.read_dir("/p").len(), 2);
    }
}
