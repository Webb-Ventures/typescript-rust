//! `ts-r doctor`: where each tool is found, the cache location and size, the project setup.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ts_r_cache::inspect::{self, human_bytes};

use crate::resolve::{self, Env, Lookup};
use crate::tools::TOOLS;
use crate::version::{Origin, Selected, VERSION};

/// Builds the report. `cwd` is where the project files are looked for.
pub fn report(selected: &Selected, env: &Env, cwd: Option<&Path>) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "ts-r {VERSION}");
    let exe = env
        .exe
        .as_ref()
        .map_or_else(|| "unknown".to_owned(), |p| p.display().to_string());
    let _ = writeln!(s, "  launcher   {exe}");
    let _ = writeln!(s, "  version    {selected}");
    if selected.origin == Origin::Launcher {
        let _ = writeln!(
            s,
            "             (pin another with a .ts-r-version file or TS_R_VERSION)"
        );
    }

    let _ = writeln!(s, "\ntools");
    for tool in TOOLS {
        let line = match resolve::resolve(tool, selected, env) {
            Lookup::Found { path, source } => format!("{}  ({source})", path.display()),
            Lookup::Missing { .. } => {
                format!("not installed (ships in milestone {})", tool.milestone)
            }
        };
        let _ = writeln!(s, "  {:<6} {line}", tool.name);
    }

    let (_, root_source) = ts_r_cache::cache_root_with_source();
    let root = &env.cache_root;
    let _ = writeln!(s, "\ncache");
    let _ = writeln!(
        s,
        "  dir        {} ({})",
        root.display(),
        root_source.describe()
    );
    if root.is_dir() {
        let (files, bytes) = inspect::dir_size(root);
        let _ = writeln!(s, "  size       {} in {files} files", human_bytes(bytes));
        for ns in inspect::namespaces(root) {
            let _ = writeln!(
                s,
                "  {:<10} {} entries, {} live, {} on disk",
                format!("{}/v{}", ns.namespace, ns.format_version),
                ns.entries,
                human_bytes(ns.live_bytes),
                human_bytes(ns.bytes)
            );
        }
        let versions = installed_versions(root);
        if !versions.is_empty() {
            let _ = writeln!(
                s,
                "  bin        installed versions: {}",
                versions.join(", ")
            );
        }
    } else {
        let _ = writeln!(s, "  size       empty (not created yet)");
    }

    let _ = writeln!(s, "\nproject");
    match cwd {
        Some(cwd) => {
            for (label, file) in [("tsconfig", "tsconfig.json"), ("package", "package.json")] {
                let found = find_up(cwd, file).map_or_else(
                    || format!("none found from {}", cwd.display()),
                    |p| p.display().to_string(),
                );
                let _ = writeln!(s, "  {label:<10} {found}");
            }
        }
        None => {
            let _ = writeln!(s, "  (current directory is unavailable)");
        }
    }
    s
}

fn installed_versions(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root.join("bin"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    v.sort();
    v
}

/// The nearest `name` in `start` or its ancestors.
pub fn find_up(start: &Path, name: &str) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_lists_everything() {
        let dir = std::env::temp_dir().join(format!("ts-r-doctor-test-{}", std::process::id()));
        let project = dir.join("proj").join("src");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(dir.join("proj").join("tsconfig.json"), "{}").unwrap();
        let env = Env {
            exe: None,
            cache_root: dir.join("cache"),
            path: None,
        };
        let selected = Selected {
            version: VERSION.into(),
            origin: Origin::Launcher,
        };
        let r = report(&selected, &env, Some(&project));
        assert!(r.contains("not installed (ships in milestone M1)"), "{r}");
        assert!(
            r.contains(&dir.join("proj").join("tsconfig.json").display().to_string()),
            "{r}"
        );
        assert!(r.contains("empty (not created yet)"), "{r}");
        {
            let c = ts_r_cache::Cache::open_in(&dir.join("cache"), "lint", 1).unwrap();
            c.put(&ts_r_cache::Key::of(b"k"), b"v").unwrap();
        }
        let r = report(&selected, &env, Some(&project));
        assert!(r.contains("lint/v1"), "{r}");
        assert!(r.contains("1 entries"), "{r}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
