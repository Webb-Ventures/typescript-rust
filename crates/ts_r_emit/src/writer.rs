//! Writing outputs. Everything is produced in memory first ([`crate::emit`]); a writer runs
//! only after every diagnostic that could stop it is known, so `noEmitOnError` writes nothing
//! at all. Writes are direct (no temp-and-rename, so a watcher of `dist/` sees one change per
//! file) and happen even when the bytes are unchanged, as tsc does (make-like tools rely on
//! mtimes).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Where outputs go.
pub trait Writer {
    /// Writes `bytes` to the absolute path `path`, creating directories.
    fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), String>;

    /// Writes every file, returning one result per file in order. The default writes them
    /// one by one; a writer may write in parallel (the order of the writes is then not
    /// defined, the results still are).
    fn write_all(&mut self, files: &[(&str, &[u8])]) -> Vec<Result<(), String>> {
        files.iter().map(|(p, b)| self.write(p, b)).collect()
    }
}

/// The real file system. Writes are irreversible, so the writer refuses any path that is,
/// after following symbolic links, one of the protected inputs (an `outDir` that is a link
/// back into the sources must not overwrite a JavaScript input; the textual TS5055 check in
/// [`crate::emit`] cannot see through links).
///
/// Files are compared by identity ([`file_key`]: device and inode on Unix, so hard links
/// count too; the canonical path elsewhere). Only an output that already exists can be an
/// input, so the inputs' identities are read the first time an output exists (never on a
/// fresh output directory). Files are written on up to [`FsWriter::with_threads`] threads
/// (default 4).
#[derive(Debug)]
pub struct FsWriter {
    inputs: Vec<String>,
    /// Identity → index into `inputs`.
    protected: OnceLock<HashMap<FileKey, usize>>,
    threads: usize,
}

#[cfg(unix)]
type FileKey = (u64, u64);
#[cfg(not(unix))]
type FileKey = std::path::PathBuf;

/// The identity of the file `p` names, after following symbolic links.
fn file_key(p: &Path) -> Option<FileKey> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(p).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        std::fs::canonicalize(p).ok()
    }
}

impl Default for FsWriter {
    fn default() -> Self {
        FsWriter {
            inputs: Vec::new(),
            protected: OnceLock::new(),
            threads: 4,
        }
    }
}

impl FsWriter {
    /// A writer that never overwrites `inputs` (absolute paths of the program's files).
    pub fn protecting<'a>(inputs: impl IntoIterator<Item = &'a str>) -> FsWriter {
        FsWriter {
            inputs: inputs.into_iter().map(str::to_owned).collect(),
            ..FsWriter::default()
        }
    }

    /// Writes on up to `threads` threads (1 to 4).
    #[must_use]
    pub fn with_threads(mut self, threads: usize) -> FsWriter {
        self.threads = threads.clamp(1, 4);
        self
    }

    fn write_one(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        let p = Path::new(path);
        if let Some(key) = file_key(p) {
            let protected = self.protected.get_or_init(|| {
                let mut keys = HashMap::with_capacity(self.inputs.len());
                for (i, input) in self.inputs.iter().enumerate() {
                    if let Some(k) = file_key(Path::new(input)) {
                        keys.entry(k).or_insert(i);
                    }
                }
                keys
            });
            if let Some(&i) = protected.get(&key) {
                return Err(format!(
                    "it is the input file '{}' (reached through a symbolic link)",
                    self.inputs[i]
                ));
            }
        }
        // The directory usually exists (a rebuild); create it only when the write says so.
        match std::fs::write(p, bytes) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some(dir) = p.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                }
                std::fs::write(p, bytes).map_err(|e| e.to_string())
            }
            r => r.map_err(|e| e.to_string()),
        }
    }
}

impl Writer for FsWriter {
    fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), String> {
        self.write_one(path, bytes)
    }

    fn write_all(&mut self, files: &[(&str, &[u8])]) -> Vec<Result<(), String>> {
        // Writing is system calls (create, write, close, mkdir): a few threads overlap them.
        let threads = self.threads.min(files.len() / 16).max(1);
        let this = &*self;
        if threads <= 1 {
            return files.iter().map(|(p, b)| this.write_one(p, b)).collect();
        }
        let next = AtomicUsize::new(0);
        let work = || {
            let mut done = Vec::new();
            loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                let Some((p, b)) = files.get(k) else { break };
                done.push((k, this.write_one(p, b)));
            }
            done
        };
        let mut results: Vec<Option<Result<(), String>>> = files.iter().map(|_| None).collect();
        std::thread::scope(|s| {
            let handles: Vec<_> = (1..threads)
                .filter_map(|_| std::thread::Builder::new().spawn_scoped(s, work).ok())
                .collect();
            // This thread writes too, and takes whatever no spawned thread did.
            for (k, r) in work() {
                results[k] = Some(r);
            }
            for h in handles {
                if let Ok(done) = h.join() {
                    for (k, r) in done {
                        results[k] = Some(r);
                    }
                }
            }
        });
        results
            .into_iter()
            .zip(files)
            .map(|(r, (p, b))| r.unwrap_or_else(|| this.write_one(p, b)))
            .collect()
    }
}

/// Outputs kept in memory (tests, the emit oracle).
#[derive(Debug, Default)]
pub struct MemoryWriter {
    pub files: BTreeMap<String, Vec<u8>>,
    /// Paths in write order.
    pub order: Vec<String>,
}

impl Writer for MemoryWriter {
    fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), String> {
        self.files.insert(path.to_owned(), bytes.to_vec());
        self.order.push(path.to_owned());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Many files in new directories, written in parallel: every result in order, an input
    /// (here an existing file) refused by identity, the others written.
    #[test]
    fn parallel_writes_keep_order_and_protect_inputs() {
        let dir = std::env::temp_dir().join(format!("ts-r-emit-writer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("input.ts");
        std::fs::write(&input, "x").unwrap();
        let input = input.to_string_lossy().into_owned();
        let paths: Vec<String> = (0..40)
            .map(|i| {
                dir.join(format!("d{i}/sub/f{i}.d.ts"))
                    .to_string_lossy()
                    .into_owned()
            })
            .chain(std::iter::once(input.clone()))
            .collect();
        let files: Vec<(&str, &[u8])> = paths.iter().map(|p| (p.as_str(), &b"y"[..])).collect();
        let mut w = FsWriter::protecting([input.as_str()]).with_threads(4);
        let results = w.write_all(&files);
        assert_eq!(results.len(), 41);
        assert!(results[..40].iter().all(Result::is_ok), "{results:?}");
        assert!(results[40].is_err());
        assert_eq!(std::fs::read_to_string(&paths[7]).unwrap(), "y");
        assert_eq!(std::fs::read_to_string(&input).unwrap(), "x");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
