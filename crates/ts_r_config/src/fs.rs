//! File-system abstraction shared by config loading, file discovery and module resolution.
//!
//! Tools use [`RealFs`]; tests use [`MemoryFs`]. All paths are absolute, normalised,
//! `/`-separated strings (see [`crate::path`]).

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::path;

/// Kind of a directory entry, after following symlinks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file (or a symlink to one).
    File,
    /// A directory (or a symlink to one).
    Directory,
}

/// One entry of [`FileSystem::read_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// Entry name (no separators), with the case stored on disk.
    pub name: String,
    /// What the entry is once symlinks are followed.
    pub kind: EntryKind,
}

/// A cheap change stamp of a file: its length and modification time. Equal stamps mean
/// "probably unchanged"; caches that trust them must still re-read a file whose
/// modification time is too close to when they last read it (coarse timestamps).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileStamp {
    pub len: u64,
    /// Nanoseconds since the Unix epoch.
    pub modified_ns: u128,
}

/// The file-system operations ts-r needs. Implementations must be cheap to call repeatedly;
/// callers cache results where it matters (see the resolver's cache).
pub trait FileSystem: Send + Sync {
    /// Reads a UTF-8 text file. `None` when missing or unreadable.
    fn read_file(&self, path: &str) -> Option<String>;
    /// Reads original file bytes. Text-backed file systems use their UTF-8 contents.
    fn read_bytes(&self, path: &str) -> Option<Vec<u8>> {
        self.read_file(path).map(String::into_bytes)
    }
    /// True when `path` is a file (following symlinks).
    fn file_exists(&self, path: &str) -> bool;
    /// True when `path` is a directory (following symlinks).
    fn directory_exists(&self, path: &str) -> bool;
    /// Entries of a directory, sorted by name (ordinal). Empty when missing.
    fn read_dir(&self, path: &str) -> Vec<DirEntry>;
    /// Resolves symlinks. Returns `path` itself when it cannot be resolved.
    fn realpath(&self, path: &str) -> String;
    /// Whether `path` itself (its last component) may be a symlink. `false` promises
    /// that `realpath(path)` is `realpath(parent)` joined with the entry's name, so a
    /// directory walk need not resolve every directory. The default, `true`, promises
    /// nothing.
    fn may_be_symlink(&self, _path: &str) -> bool {
        true
    }
    /// Whether file names are compared case-sensitively.
    fn case_sensitive(&self) -> bool;
    /// Current working directory (absolute, normalised).
    fn cwd(&self) -> String {
        "/".to_owned()
    }
    /// The change stamp of file `path`, without reading it. `None` (the default) when the
    /// file system has none (callers read the file and compare). A wrapper whose
    /// `read_file` changes contents must not forward its inner stamp.
    fn stamp(&self, _path: &str) -> Option<FileStamp> {
        None
    }
    /// The name entry `name` of directory `dir` has on disk: an exact match, else the first
    /// match ignoring case. Recovers a path's stored case; a file system that records what
    /// it is asked (watch mode) need not record this as a listing of `dir`.
    fn disk_name(&self, dir: &str, name: &str) -> Option<String> {
        let entries = self.read_dir(dir);
        entries
            .iter()
            .find(|e| e.name == name)
            .or_else(|| entries.iter().find(|e| path::eq(&e.name, name, false)))
            .map(|e| e.name.clone())
    }
}

impl<T: FileSystem + ?Sized> FileSystem for Arc<T> {
    fn read_file(&self, path: &str) -> Option<String> {
        (**self).read_file(path)
    }
    fn read_bytes(&self, path: &str) -> Option<Vec<u8>> {
        (**self).read_bytes(path)
    }
    fn file_exists(&self, path: &str) -> bool {
        (**self).file_exists(path)
    }
    fn directory_exists(&self, path: &str) -> bool {
        (**self).directory_exists(path)
    }
    fn read_dir(&self, path: &str) -> Vec<DirEntry> {
        (**self).read_dir(path)
    }
    fn realpath(&self, path: &str) -> String {
        (**self).realpath(path)
    }
    fn may_be_symlink(&self, path: &str) -> bool {
        (**self).may_be_symlink(path)
    }
    fn case_sensitive(&self) -> bool {
        (**self).case_sensitive()
    }
    fn cwd(&self) -> String {
        (**self).cwd()
    }
    fn stamp(&self, path: &str) -> Option<FileStamp> {
        (**self).stamp(path)
    }
    fn disk_name(&self, dir: &str, name: &str) -> Option<String> {
        (**self).disk_name(dir, name)
    }
}

impl<T: FileSystem + ?Sized> FileSystem for &T {
    fn read_file(&self, path: &str) -> Option<String> {
        (**self).read_file(path)
    }
    fn read_bytes(&self, path: &str) -> Option<Vec<u8>> {
        (**self).read_bytes(path)
    }
    fn file_exists(&self, path: &str) -> bool {
        (**self).file_exists(path)
    }
    fn directory_exists(&self, path: &str) -> bool {
        (**self).directory_exists(path)
    }
    fn read_dir(&self, path: &str) -> Vec<DirEntry> {
        (**self).read_dir(path)
    }
    fn realpath(&self, path: &str) -> String {
        (**self).realpath(path)
    }
    fn may_be_symlink(&self, path: &str) -> bool {
        (**self).may_be_symlink(path)
    }
    fn case_sensitive(&self) -> bool {
        (**self).case_sensitive()
    }
    fn cwd(&self) -> String {
        (**self).cwd()
    }
    fn stamp(&self, path: &str) -> Option<FileStamp> {
        (**self).stamp(path)
    }
    fn disk_name(&self, dir: &str, name: &str) -> Option<String> {
        (**self).disk_name(dir, name)
    }
}

/// The real file system via `std::fs`.
#[derive(Debug, Clone)]
pub struct RealFs {
    case_sensitive: bool,
}

impl Default for RealFs {
    fn default() -> Self {
        Self::new()
    }
}

/// Source text from a file's bytes, as tsc reads it: a UTF-16 byte order mark (`FE FF` big
/// endian, `FF FE` little endian) decodes the rest as UTF-16 without the mark; anything else is
/// UTF-8 (invalid sequences become U+FFFD, which the lexer reports as a binary file).
pub fn decode_text(bytes: Vec<u8>) -> String {
    let utf16 = |rest: &[u8], big: bool| {
        let units = rest.chunks_exact(2).map(|p| {
            if big {
                u16::from_be_bytes([p[0], p[1]])
            } else {
                u16::from_le_bytes([p[0], p[1]])
            }
        });
        char::decode_utf16(units)
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect::<String>()
    };
    match bytes.as_slice() {
        [0xFE, 0xFF, rest @ ..] => utf16(rest, true),
        [0xFF, 0xFE, rest @ ..] => utf16(rest, false),
        _ => match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        },
    }
}

impl RealFs {
    /// Uses the platform default: case-insensitive on macOS and Windows, sensitive elsewhere.
    pub fn new() -> Self {
        Self {
            case_sensitive: !(cfg!(target_os = "macos") || cfg!(windows)),
        }
    }

    /// Overrides case sensitivity.
    pub fn with_case_sensitivity(case_sensitive: bool) -> Self {
        Self { case_sensitive }
    }

    fn native(path: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(path)
    }
}

impl FileSystem for RealFs {
    fn read_file(&self, path: &str) -> Option<String> {
        let bytes = std::fs::read(Self::native(path)).ok()?;
        Some(decode_text(bytes))
    }

    fn read_bytes(&self, path: &str) -> Option<Vec<u8>> {
        std::fs::read(Self::native(path)).ok()
    }

    fn file_exists(&self, path: &str) -> bool {
        std::fs::metadata(Self::native(path))
            .map(|m| m.is_file())
            .unwrap_or(false)
    }

    fn directory_exists(&self, path: &str) -> bool {
        std::fs::metadata(Self::native(path))
            .map(|m| m.is_dir())
            .unwrap_or(false)
    }

    fn read_dir(&self, path: &str) -> Vec<DirEntry> {
        let Ok(rd) = std::fs::read_dir(Self::native(path)) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in rd.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let Ok(ft) = entry.file_type() else { continue };
            let kind = if ft.is_symlink() {
                match std::fs::metadata(entry.path()) {
                    Ok(m) if m.is_dir() => EntryKind::Directory,
                    Ok(m) if m.is_file() => EntryKind::File,
                    _ => continue,
                }
            } else if ft.is_dir() {
                EntryKind::Directory
            } else if ft.is_file() {
                EntryKind::File
            } else {
                continue;
            };
            out.push(DirEntry { name, kind });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    fn may_be_symlink(&self, path: &str) -> bool {
        std::fs::symlink_metadata(Self::native(path)).map_or(true, |m| m.file_type().is_symlink())
    }

    fn realpath(&self, path: &str) -> String {
        match std::fs::canonicalize(Self::native(path)) {
            Ok(p) => {
                let s = path::normalize_slashes(&p.to_string_lossy());
                // Strip the Windows verbatim prefix.
                s.strip_prefix("//?/").map(str::to_owned).unwrap_or(s)
            }
            Err(_) => path.to_owned(),
        }
    }

    fn case_sensitive(&self) -> bool {
        self.case_sensitive
    }

    fn cwd(&self) -> String {
        std::env::current_dir()
            .map(|p| path::normalize(&p.to_string_lossy()))
            .unwrap_or_else(|_| "/".to_owned())
    }

    fn stamp(&self, path: &str) -> Option<FileStamp> {
        let m = std::fs::metadata(Self::native(path)).ok()?;
        if !m.is_file() {
            return None;
        }
        let modified = m.modified().ok()?;
        let modified_ns = modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        Some(FileStamp {
            len: m.len(),
            modified_ns,
        })
    }
}

#[derive(Debug, Clone)]
enum MemNode {
    File(String),
    Dir,
    Symlink(String),
}

/// In-memory file system for tests. Supports files, directories and symlinks
/// (absolute or relative targets), and optional case-insensitivity.
#[derive(Debug, Clone)]
pub struct MemoryFs {
    case_sensitive: bool,
    cwd: String,
    /// key(path) -> (path as written, node)
    nodes: BTreeMap<String, (String, MemNode)>,
}

impl Default for MemoryFs {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryFs {
    /// An empty, case-sensitive file system with cwd `/`.
    pub fn new() -> Self {
        let mut fs = Self {
            case_sensitive: true,
            cwd: "/".into(),
            nodes: BTreeMap::new(),
        };
        fs.nodes.insert("/".into(), ("/".into(), MemNode::Dir));
        fs
    }

    /// An empty case-insensitive file system.
    pub fn case_insensitive() -> Self {
        let mut fs = Self::new();
        fs.case_sensitive = false;
        fs
    }

    /// Sets the working directory.
    pub fn set_cwd(&mut self, cwd: &str) {
        self.cwd = path::normalize(cwd);
    }

    fn k(&self, p: &str) -> String {
        path::key(p, self.case_sensitive)
    }

    fn ensure_dirs(&mut self, dir: &str) {
        let dir = path::normalize(dir);
        let mut cur = dir.as_str();
        let mut chain = Vec::new();
        loop {
            chain.push(cur.to_owned());
            let parent = path::directory_of(cur);
            if parent == cur || parent.is_empty() {
                break;
            }
            cur = parent;
        }
        for d in chain.into_iter().rev() {
            let key = self.k(&d);
            self.nodes.entry(key).or_insert((d, MemNode::Dir));
        }
    }

    /// Adds (or replaces) a file, creating parent directories.
    pub fn add_file(&mut self, file: &str, contents: &str) -> &mut Self {
        let file = path::normalize(file);
        self.ensure_dirs(path::directory_of(&file));
        let key = self.k(&file);
        self.nodes
            .insert(key, (file, MemNode::File(contents.to_owned())));
        self
    }

    /// Adds a directory (and its parents).
    pub fn add_dir(&mut self, dir: &str) -> &mut Self {
        self.ensure_dirs(dir);
        self
    }

    /// Adds a symlink at `link` pointing at `target` (absolute, or relative to the link's
    /// directory).
    pub fn add_symlink(&mut self, link: &str, target: &str) -> &mut Self {
        let link = path::normalize(link);
        self.ensure_dirs(path::directory_of(&link));
        let key = self.k(&link);
        self.nodes
            .insert(key, (link, MemNode::Symlink(target.to_owned())));
        self
    }

    /// Resolves every symlink in `p`. Returns `(real path, exists)`.
    fn resolve(&self, p: &str) -> Option<String> {
        let p = path::normalize(p);
        let (root, comps) = path::components(&p);
        let mut cur = root.to_owned();
        let mut pending: Vec<String> = comps.iter().rev().map(|s| (*s).to_owned()).collect();
        let mut hops = 0;
        while let Some(c) = pending.pop() {
            let next = path::combine(&cur, &c);
            let next = path::normalize(&next);
            match self.nodes.get(&self.k(&next)) {
                Some((stored, MemNode::Symlink(target))) => {
                    hops += 1;
                    if hops > 40 {
                        return None;
                    }
                    let base = path::directory_of(stored).to_owned();
                    let target = path::resolve(&base, target);
                    let (troot, tcomps) = path::components(&target);
                    cur = troot.to_owned();
                    for t in tcomps.iter().rev() {
                        pending.push((*t).to_owned());
                    }
                }
                Some((stored, _)) => cur = stored.clone(),
                None => return None,
            }
        }
        Some(cur)
    }

    fn node(&self, p: &str) -> Option<&MemNode> {
        let real = self.resolve(p)?;
        self.nodes.get(&self.k(&real)).map(|(_, n)| n)
    }
}

impl FileSystem for MemoryFs {
    fn read_file(&self, path: &str) -> Option<String> {
        match self.node(path)? {
            MemNode::File(s) => Some(s.clone()),
            _ => None,
        }
    }

    fn file_exists(&self, path: &str) -> bool {
        matches!(self.node(path), Some(MemNode::File(_)))
    }

    fn directory_exists(&self, path: &str) -> bool {
        matches!(self.node(path), Some(MemNode::Dir))
    }

    fn read_dir(&self, dir: &str) -> Vec<DirEntry> {
        let Some(real) = self.resolve(dir) else {
            return Vec::new();
        };
        if !matches!(self.nodes.get(&self.k(&real)), Some((_, MemNode::Dir))) {
            return Vec::new();
        }
        let prefix = {
            let k = self.k(&real);
            if k.ends_with('/') { k } else { format!("{k}/") }
        };
        let mut out = Vec::new();
        for (key, (stored, _)) in self.nodes.range(prefix.clone()..) {
            if !key.starts_with(&prefix) {
                break;
            }
            let rest = &key[prefix.len()..];
            if rest.is_empty() || rest.contains('/') {
                continue;
            }
            let name = path::file_name(stored).to_owned();
            let kind = match self.node(stored) {
                Some(MemNode::File(_)) => EntryKind::File,
                Some(MemNode::Dir) => EntryKind::Directory,
                _ => continue,
            };
            out.push(DirEntry { name, kind });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    fn realpath(&self, path: &str) -> String {
        self.resolve(path).unwrap_or_else(|| path.to_owned())
    }

    fn case_sensitive(&self) -> bool {
        self.case_sensitive
    }

    fn cwd(&self) -> String {
        self.cwd.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_text_reads_utf16_byte_order_marks() {
        assert_eq!(decode_text(vec![0xFE, 0xFF, 0, b'x', 0, b';']), "x;");
        assert_eq!(decode_text(vec![0xFF, 0xFE, b'x', 0, b';', 0]), "x;");
        // an odd trailing byte is dropped, a lone surrogate is U+FFFD
        assert_eq!(decode_text(vec![0xFF, 0xFE, b'x', 0, 7]), "x");
        assert_eq!(decode_text(vec![0xFF, 0xFE, 0x00, 0xD8]), "\u{FFFD}");
        assert_eq!(decode_text(b"a\xFFb".to_vec()), "a\u{FFFD}b");
        assert_eq!(decode_text("\u{FEFF}x".into()), "\u{FEFF}x");
    }

    #[test]
    fn memory_fs_basics() {
        let mut fs = MemoryFs::new();
        fs.add_file("/p/a.ts", "x")
            .add_file("/p/sub/b.ts", "y")
            .add_dir("/p/empty");
        assert!(fs.file_exists("/p/a.ts"));
        assert!(fs.directory_exists("/p/sub"));
        assert!(!fs.file_exists("/p/sub"));
        let names: Vec<_> = fs.read_dir("/p").into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["a.ts", "empty", "sub"]);
        assert_eq!(fs.read_file("/p/sub/b.ts").as_deref(), Some("y"));
    }

    #[test]
    fn memory_fs_symlinks() {
        let mut fs = MemoryFs::new();
        fs.add_file("/store/pkg@1/node_modules/pkg/index.d.ts", "")
            .add_symlink(
                "/app/node_modules/pkg",
                "../../store/pkg@1/node_modules/pkg",
            );
        assert!(fs.file_exists("/app/node_modules/pkg/index.d.ts"));
        assert_eq!(
            fs.realpath("/app/node_modules/pkg/index.d.ts"),
            "/store/pkg@1/node_modules/pkg/index.d.ts"
        );
        let kinds: Vec<_> = fs
            .read_dir("/app/node_modules")
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, [EntryKind::Directory]);
    }

    #[test]
    fn memory_fs_case_insensitive() {
        let mut fs = MemoryFs::case_insensitive();
        fs.add_file("/P/Src/A.ts", "");
        assert!(fs.file_exists("/p/src/a.ts"));
        assert_eq!(fs.realpath("/p/src/a.ts"), "/P/Src/A.ts");
    }
}
