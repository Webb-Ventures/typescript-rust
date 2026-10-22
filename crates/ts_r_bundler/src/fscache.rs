//! A caching [`FileSystem`] wrapper for builds: every directory is listed once and existence
//! checks are answered from the listing; directory real paths are cached and a file's real
//! path is its directory's real path plus its name. On macOS this replaces thousands of
//! `stat`/`open`/`getattrlist` calls per build with one `readdir` per directory.
//!
//! File contents are never cached here, except briefly while a build reads ahead
//! ([`CachedFs::read_ahead`]): a file read ahead is handed to the first `read_file` of it and
//! forgotten. Call [`CachedFs::clear`] when files may have been added or removed (the build
//! context does on `invalidate`).

use std::sync::{Arc, Mutex, RwLock};

use rustc_hash::FxHashMap;
use ts_r_config::fs::{DirEntry, EntryKind};
use ts_r_config::{FileSystem, path};

type Listing = Option<Arc<FxHashMap<String, EntryKind>>>;

/// A file in the read-ahead table.
enum Ahead {
    /// The reader thread is reading it.
    Reading,
    /// Read, waiting for its `read_file`.
    Read(Option<String>),
    /// A `read_file` came first and read it itself: the reader skips or drops it.
    Taken,
}

pub struct CachedFs {
    inner: Arc<dyn FileSystem>,
    dirs: RwLock<FxHashMap<String, Listing>>,
    real_dirs: RwLock<FxHashMap<String, String>>,
    /// While reading ahead: files queued, being read or read (see [`CachedFs::read_ahead`]).
    ahead: Mutex<Option<FxHashMap<String, Ahead>>>,
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
            ahead: Mutex::new(None),
        }
    }

    /// Runs `f` while one extra thread reads files ahead of their `read_file`: `f` gets a
    /// function that queues a path. Opening files is costly and contended in the kernel on
    /// macOS (one thread opening the files in turn beats every worker opening its own), and
    /// the reads overlap the workers' parsing. Each file is still read once through the
    /// inner file system, by whichever of the reader and the `read_file` comes first; what
    /// was read ahead and never asked for is dropped when `f` returns.
    pub fn read_ahead<R>(&self, f: impl FnOnce(&(dyn Fn(String) + Sync)) -> R) -> R {
        *self.ahead.lock().unwrap_or_else(|e| e.into_inner()) = Some(FxHashMap::default());
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let r = std::thread::scope(|s| {
            s.spawn(|| {
                for p in rx {
                    self.read_one_ahead(p);
                }
            });
            let r = f(&|p: String| {
                let _ = tx.send(p);
            });
            // Closing the channel ends the reader.
            drop(tx);
            r
        });
        *self.ahead.lock().unwrap_or_else(|e| e.into_inner()) = None;
        r
    }

    fn read_one_ahead(&self, p: String) {
        {
            let mut g = self.ahead.lock().unwrap_or_else(|e| e.into_inner());
            let Some(map) = g.as_mut() else { return };
            if map.contains_key(&p) {
                return;
            }
            map.insert(p.clone(), Ahead::Reading);
        }
        let text = self.read_file_now(&p);
        let mut g = self.ahead.lock().unwrap_or_else(|e| e.into_inner());
        let Some(map) = g.as_mut() else { return };
        match map.get_mut(&p) {
            Some(slot @ Ahead::Reading) => *slot = Ahead::Read(text),
            _ => {
                map.remove(&p);
            }
        }
    }

    fn read_file_now(&self, path: &str) -> Option<String> {
        if self.kind(path) == Some(None) {
            return None;
        }
        self.inner.read_file(path)
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
        {
            let mut g = self.ahead.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(map) = g.as_mut() {
                match map.remove(path) {
                    Some(Ahead::Read(text)) => return text,
                    // Reading now, or not yet: read it here and let the reader skip it.
                    Some(Ahead::Reading | Ahead::Taken) | None => {
                        map.insert(path.to_owned(), Ahead::Taken);
                    }
                }
            }
        }
        self.read_file_now(path)
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

    /// Files read ahead are handed out once; files asked for first are read directly; the
    /// table is gone afterwards.
    #[test]
    fn reads_ahead() {
        let mut m = MemoryFs::new();
        for i in 0..50 {
            m.add_file(&format!("/p/{i}.js"), &format!("{i}"));
        }
        let fs = CachedFs::new(Arc::new(m));
        let got = fs.read_ahead(|queue| {
            for i in 0..50 {
                queue(format!("/p/{i}.js"));
            }
            queue("/p/missing.js".to_owned());
            let mut got: Vec<Option<String>> = (0..50)
                .rev()
                .map(|i| fs.read_file(&format!("/p/{i}.js")))
                .collect();
            got.push(fs.read_file("/p/missing.js"));
            // A second read of the same file reads it again.
            got.push(fs.read_file("/p/7.js"));
            got
        });
        let want: Vec<Option<String>> = (0..50)
            .rev()
            .map(|i| Some(format!("{i}")))
            .chain([None, Some("7".to_owned())])
            .collect();
        assert_eq!(got, want);
        assert!(fs.ahead.lock().unwrap().is_none());
        assert_eq!(fs.read_file("/p/3.js").as_deref(), Some("3"));
    }
}
