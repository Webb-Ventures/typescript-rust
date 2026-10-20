//! Finding a tool binary.
//!
//! For the launcher's own version: next to the launcher executable (also its symlink-resolved
//! location), then `<cache>/bin/<version>/`, then `PATH`. For a pinned other version only
//! `<cache>/bin/<version>/` is searched, since binaries elsewhere are of unknown version.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::tools::Tool;
use crate::version::Selected;

/// Everything resolution depends on, captured once so it can be faked in tests.
#[derive(Clone, Debug)]
pub struct Env {
    /// The launcher executable, if known.
    pub exe: Option<PathBuf>,
    /// Cache root (`~/.cache/ts-r`).
    pub cache_root: PathBuf,
    /// The `PATH` variable.
    pub path: Option<OsString>,
}

impl Env {
    /// Reads the real environment.
    pub fn from_process() -> Env {
        Env {
            exe: std::env::current_exe().ok(),
            cache_root: ts_r_cache::cache_root(),
            path: std::env::var_os("PATH"),
        }
    }
}

/// Where a tool was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// In the launcher's directory.
    NextToLauncher,
    /// In `<cache>/bin/<version>/`.
    Cache,
    /// On `PATH`.
    Path,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Source::NextToLauncher => "next to the launcher",
            Source::Cache => "version cache",
            Source::Path => "PATH",
        })
    }
}

/// Result of a lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lookup {
    /// Found the binary.
    Found {
        /// Path to run.
        path: PathBuf,
        /// Where it was found.
        source: Source,
    },
    /// Not found; `searched` lists the directories (and `PATH`) that were tried.
    Missing {
        /// Places searched, for the error message.
        searched: Vec<String>,
    },
}

/// Finds `tool` for the selected version.
pub fn resolve(tool: &Tool, selected: &Selected, env: &Env) -> Lookup {
    let name = tool.binary_name();
    let mut searched = Vec::new();
    let cache_dir = ts_r_cache::bin_dir(&env.cache_root, &selected.version);

    if selected.is_own() {
        for dir in launcher_dirs(env.exe.as_deref()) {
            let candidate = dir.join(&name);
            if is_executable(&candidate) {
                return Lookup::Found {
                    path: candidate,
                    source: Source::NextToLauncher,
                };
            }
            searched.push(dir.display().to_string());
        }
    }
    let candidate = cache_dir.join(&name);
    if is_executable(&candidate) {
        return Lookup::Found {
            path: candidate,
            source: Source::Cache,
        };
    }
    searched.push(cache_dir.display().to_string());
    if selected.is_own() {
        if let Some(path) = env.path.as_ref() {
            for dir in std::env::split_paths(path) {
                let candidate = dir.join(&name);
                if is_executable(&candidate) {
                    return Lookup::Found {
                        path: candidate,
                        source: Source::Path,
                    };
                }
            }
        }
        searched.push("PATH".to_owned());
    }
    Lookup::Missing { searched }
}

/// The launcher's directory, plus its symlink-resolved directory when different (Homebrew and
/// npm link the launcher from elsewhere).
fn launcher_dirs(exe: Option<&Path>) -> Vec<PathBuf> {
    let Some(exe) = exe else { return Vec::new() };
    let mut dirs = Vec::with_capacity(2);
    if let Some(d) = exe.parent() {
        dirs.push(d.to_path_buf());
    }
    if let Some(real) = std::fs::canonicalize(exe)
        .ok()
        .and_then(|r| r.parent().map(Path::to_path_buf))
        && !dirs.contains(&real)
    {
        dirs.push(real);
    }
    dirs
}

/// A regular file we may execute.
pub fn is_executable(path: &Path) -> bool {
    let Ok(md) = std::fs::metadata(path) else {
        return false;
    };
    if !md.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        md.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools;
    use crate::version::{Origin, VERSION};

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tmp(tag: &str) -> Tmp {
        let p = std::env::temp_dir().join(format!("ts-r-resolve-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }

    fn fake_tool(dir: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    fn own() -> Selected {
        Selected {
            version: VERSION.to_owned(),
            origin: Origin::Launcher,
        }
    }

    #[test]
    fn search_order() {
        let t = tmp("order");
        let launcher = t.0.join("launcher");
        let cache = t.0.join("cache");
        let on_path = t.0.join("path");
        std::fs::create_dir_all(&launcher).unwrap();
        let env = Env {
            exe: Some(launcher.join("ts-r")),
            cache_root: cache.clone(),
            path: Some(std::env::join_paths([&on_path]).unwrap()),
        };
        let run = tools::find("run").unwrap();
        assert!(
            matches!(resolve(run, &own(), &env), Lookup::Missing { ref searched } if searched.len() == 3)
        );

        let p = fake_tool(&on_path, "ts-r-run");
        assert_eq!(
            resolve(run, &own(), &env),
            Lookup::Found {
                path: p,
                source: Source::Path
            }
        );
        let p = fake_tool(&ts_r_cache::bin_dir(&cache, VERSION), "ts-r-run");
        assert_eq!(
            resolve(run, &own(), &env),
            Lookup::Found {
                path: p,
                source: Source::Cache
            }
        );
        let p = fake_tool(&launcher, "ts-r-run");
        assert_eq!(
            resolve(run, &own(), &env),
            Lookup::Found {
                path: p,
                source: Source::NextToLauncher
            }
        );
    }

    #[test]
    fn pinned_version_only_uses_its_cache_dir() {
        let t = tmp("pinned");
        let launcher = t.0.join("launcher");
        let cache = t.0.join("cache");
        fake_tool(&launcher, "ts-r-lint");
        let env = Env {
            exe: Some(launcher.join("ts-r")),
            cache_root: cache.clone(),
            path: None,
        };
        let pinned = Selected {
            version: "9.9.9".into(),
            origin: Origin::Env,
        };
        let lint = tools::find("lint").unwrap();
        match resolve(lint, &pinned, &env) {
            Lookup::Missing { searched } => assert_eq!(
                searched,
                vec![cache.join("bin").join("9.9.9").display().to_string()]
            ),
            other => panic!("{other:?}"),
        }
        let p = fake_tool(&cache.join("bin").join("9.9.9"), "ts-r-lint");
        assert_eq!(
            resolve(lint, &pinned, &env),
            Lookup::Found {
                path: p,
                source: Source::Cache
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_executable_files_are_skipped() {
        let t = tmp("noexec");
        std::fs::write(t.0.join("ts-r-run"), "").unwrap();
        assert!(!is_executable(&t.0.join("ts-r-run")));
        assert!(!is_executable(&t.0));
    }
}
