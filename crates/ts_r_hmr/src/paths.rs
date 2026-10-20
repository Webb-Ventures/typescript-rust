//! File system paths at the server's edges, the same on every OS.
//!
//! - [`canonicalize`]: `fs::canonicalize` without Windows' verbatim prefix (`\\?\C:\x` →
//!   `C:\x`, `\\?\UNC\srv\share` → `\\srv\share`). A verbatim path breaks every place that
//!   splits an id at `?` (plugin ids `file?query`) and is not what Vite or Node show users.
//! - [`id`]: a path as a plugin id / display string with `/` separators on Windows (Vite's
//!   `normalizePath`); on Unix the path unchanged (`\` is a valid file name character there).

use std::borrow::Cow;
use std::io;
use std::path::{Path, PathBuf};

/// `fs::canonicalize`, minus the Windows verbatim prefix when the plain form means the same.
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    std::fs::canonicalize(path).map(strip_verbatim)
}

/// `\\?\C:\x` → `C:\x` and `\\?\UNC\srv\share\x` → `\\srv\share\x`; anything else unchanged.
pub fn strip_verbatim(path: PathBuf) -> PathBuf {
    let Some(s) = path.to_str() else {
        return path;
    };
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        let b = rest.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            return PathBuf::from(rest);
        }
    }
    path
}

/// A path as a string id: `/` separators on Windows, unchanged elsewhere.
pub fn id(path: &Path) -> Cow<'_, str> {
    let s = path.to_string_lossy();
    if cfg!(windows) && s.contains('\\') {
        Cow::Owned(s.replace('\\', "/"))
    } else {
        s
    }
}

/// Whether `s` starts with a Windows drive root (`C:/` or `C:\\`).
pub fn is_drive_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'/' | b'\\')
}

/// The file behind a `/@fs/<abs>` URL path (`abs` without the leading `/`): `/<abs>` on
/// Unix, `C:/x` as is (a drive path keeps its drive).
pub fn from_fs_url(abs: &str) -> PathBuf {
    if is_drive_absolute(abs) {
        PathBuf::from(abs)
    } else {
        PathBuf::from(format!("/{abs}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_prefixes_are_stripped() {
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\C:\Users\a")),
            PathBuf::from(r"C:\Users\a")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\UNC\srv\share\a")),
            PathBuf::from(r"\\srv\share\a")
        );
        // Not a drive path: keep the verbatim form (it may need it).
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\Volume{x}\a")),
            PathBuf::from(r"\\?\Volume{x}\a")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from("/tmp/a")),
            PathBuf::from("/tmp/a")
        );
    }

    #[test]
    fn fs_urls_keep_drives() {
        assert_eq!(from_fs_url("p/lib/x.js"), PathBuf::from("/p/lib/x.js"));
        assert_eq!(from_fs_url("C:/p/x.js"), PathBuf::from("C:/p/x.js"));
        assert!(
            is_drive_absolute(r"c:\x") && !is_drive_absolute("c:x") && !is_drive_absolute("/x")
        );
    }

    #[test]
    fn ids_use_forward_slashes_on_windows() {
        let p = Path::new("/r/src/a.js");
        assert_eq!(id(p), "/r/src/a.js");
        if cfg!(windows) {
            assert_eq!(id(Path::new(r"C:\r\src/a.js")), "C:/r/src/a.js");
        } else {
            assert_eq!(id(Path::new(r"/r/a\b.js")), r"/r/a\b.js");
        }
    }
}
