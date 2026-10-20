//! Where the cache lives on disk.
//!
//! Resolution order (first match wins):
//!
//! 1. `TS_R_CACHE_DIR` (used as-is)
//! 2. `$XDG_CACHE_HOME/ts-r` (only when the variable is an absolute path)
//! 3. Windows: `%LOCALAPPDATA%\ts-r`
//! 4. `$HOME/.cache/ts-r` (also on macOS: the plan documents `~/.cache/ts-r` everywhere)
//! 5. Windows: `%USERPROFILE%\.cache\ts-r`
//! 6. `<temp dir>/ts-r-cache` as a last resort, so tools still work in odd sandboxes.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Why a particular cache root was chosen. Shown by `ts-r doctor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootSource {
    /// `TS_R_CACHE_DIR` was set.
    TsRCacheDir,
    /// `XDG_CACHE_HOME` was set.
    XdgCacheHome,
    /// `%LOCALAPPDATA%` on Windows.
    LocalAppData,
    /// `$HOME/.cache` (or `%USERPROFILE%\.cache`).
    Home,
    /// Nothing else was available; the system temp dir is used.
    TempDir,
}

impl RootSource {
    /// A short human-readable description.
    pub fn describe(self) -> &'static str {
        match self {
            RootSource::TsRCacheDir => "from TS_R_CACHE_DIR",
            RootSource::XdgCacheHome => "from XDG_CACHE_HOME",
            RootSource::LocalAppData => "from LOCALAPPDATA",
            RootSource::Home => "default",
            RootSource::TempDir => "temp dir fallback: no HOME set",
        }
    }
}

/// The cache root directory (`~/.cache/ts-r` by default). Does not create it.
pub fn cache_root() -> PathBuf {
    cache_root_with_source().0
}

/// The cache root and the reason it was chosen.
pub fn cache_root_with_source() -> (PathBuf, RootSource) {
    resolve_root(|name| env::var_os(name), cfg!(windows))
}

/// Pure resolution logic, parameterised over the environment for tests.
pub(crate) fn resolve_root(
    var: impl Fn(&str) -> Option<OsString>,
    windows: bool,
) -> (PathBuf, RootSource) {
    let non_empty = |name: &str| var(name).filter(|v| !v.is_empty());
    if let Some(dir) = non_empty("TS_R_CACHE_DIR") {
        return (PathBuf::from(dir), RootSource::TsRCacheDir);
    }
    if let Some(xdg) = non_empty("XDG_CACHE_HOME") {
        let xdg = PathBuf::from(xdg);
        // The XDG spec says relative paths are invalid and must be ignored.
        if xdg.is_absolute() {
            return (xdg.join("ts-r"), RootSource::XdgCacheHome);
        }
    }
    if windows && let Some(local) = non_empty("LOCALAPPDATA") {
        return (PathBuf::from(local).join("ts-r"), RootSource::LocalAppData);
    }
    let home = non_empty("HOME").or_else(|| {
        if windows {
            non_empty("USERPROFILE")
        } else {
            None
        }
    });
    if let Some(home) = home {
        return (
            PathBuf::from(home).join(".cache").join("ts-r"),
            RootSource::Home,
        );
    }
    (env::temp_dir().join("ts-r-cache"), RootSource::TempDir)
}

/// Directory holding the tool binaries of one ts-r version: `<root>/bin/<version>/`.
pub fn bin_dir(root: &Path, version: &str) -> PathBuf {
    root.join("bin").join(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn explicit_dir_wins() {
        let (p, s) = resolve_root(
            env_of(&[
                ("TS_R_CACHE_DIR", "/x/y"),
                ("XDG_CACHE_HOME", "/xdg"),
                ("HOME", "/h"),
            ]),
            false,
        );
        assert_eq!(p, PathBuf::from("/x/y"));
        assert_eq!(s, RootSource::TsRCacheDir);
    }

    #[test]
    fn xdg_then_home() {
        // XDG_CACHE_HOME must be absolute on the host OS: `/xdg` has no drive on Windows.
        let xdg = if cfg!(windows) { r"C:\xdg" } else { "/xdg" };
        let (p, s) = resolve_root(env_of(&[("XDG_CACHE_HOME", xdg), ("HOME", "/h")]), false);
        assert_eq!(p, PathBuf::from(xdg).join("ts-r"));
        assert_eq!(s, RootSource::XdgCacheHome);
        let (p, s) = resolve_root(env_of(&[("XDG_CACHE_HOME", "rel"), ("HOME", "/h")]), false);
        assert_eq!(p, PathBuf::from("/h/.cache/ts-r"));
        assert_eq!(s, RootSource::Home);
    }

    #[test]
    fn empty_vars_are_ignored() {
        let (p, _) = resolve_root(env_of(&[("TS_R_CACHE_DIR", ""), ("HOME", "/h")]), false);
        assert_eq!(p, PathBuf::from("/h/.cache/ts-r"));
    }

    #[test]
    fn windows_local_app_data() {
        let (p, s) = resolve_root(env_of(&[("LOCALAPPDATA", "/lad"), ("HOME", "/h")]), true);
        assert_eq!(p, PathBuf::from("/lad").join("ts-r"));
        assert_eq!(s, RootSource::LocalAppData);
        // LOCALAPPDATA is ignored off Windows.
        let (p, _) = resolve_root(env_of(&[("LOCALAPPDATA", "/lad"), ("HOME", "/h")]), false);
        assert_eq!(p, PathBuf::from("/h/.cache/ts-r"));
    }

    #[test]
    fn temp_fallback() {
        let (_, s) = resolve_root(env_of(&[]), false);
        assert_eq!(s, RootSource::TempDir);
    }

    #[test]
    fn bin_dir_layout() {
        assert_eq!(
            bin_dir(Path::new("/c"), "1.2.3"),
            PathBuf::from("/c/bin/1.2.3")
        );
    }
}
