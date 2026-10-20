//! Platform files: a Rust module named `windows`, `unix`, `macos` or `linux` (`windows.rs`,
//! `windows/mod.rs`, or a `windows/main.rs` test crate, under `crates/` or `xtask/`) is compiled
//! only on that platform, with every `.rs` file in a folder of that name, so CI tests a change
//! to them only on that platform (the per-OS hashes in `.github/workflows/ci.yml`).
//!
//! That is only safe when the module really is gated, so `agents-check` fails unless the module
//! is declared under the matching `#[cfg(..)]` in its parent, or carries it as an inner
//! `#![cfg(..)]` attribute (an integration test, or a module gated in the file itself).

use std::path::{Path, PathBuf};

use crate::util::{read_to_string, rel, walk_files};

/// The module names CI treats as platform-only, and the `cfg` predicates that gate each.
const PLATFORMS: &[(&str, &[&str])] = &[
    ("windows", &["windows", "target_os=\"windows\""]),
    ("unix", &["unix"]),
    ("macos", &["target_os=\"macos\""]),
    ("linux", &["target_os=\"linux\""]),
];

/// Problems with platform files under `root` (one line each).
pub fn check(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    walk_files(&root.join("crates"), "rs", &mut files);
    walk_files(&root.join("xtask"), "rs", &mut files);
    let mut problems = Vec::new();
    for file in files {
        let Some((name, preds)) = platform_module(&file) else {
            continue;
        };
        if gated_inside(&file, preds) || gated_by_parent(&file, name, preds) {
            continue;
        }
        problems.push(format!(
            "{}: a `{name}` module must be declared under #[cfg({})] (or start with #![cfg({0})]): CI only tests it on {name}",
            rel(&file),
            preds[0],
        ));
    }
    problems
}

/// The platform this file is the root of (`windows.rs`, `windows/mod.rs` or `windows/main.rs`),
/// if any.
fn platform_module(file: &Path) -> Option<(&'static str, &'static [&'static str])> {
    let stem = file.file_stem()?.to_str()?;
    let module = if stem == "mod" || stem == "main" {
        file.parent()?.file_name()?.to_str()?
    } else {
        stem
    };
    PLATFORMS.iter().find(|(name, _)| *name == module).copied()
}

/// The file starts (after comments) with `#![cfg(<pred>)]`.
fn gated_inside(file: &Path, preds: &[&str]) -> bool {
    let Ok(text) = read_to_string(file) else {
        return false;
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .take_while(|l| l.starts_with("#!["))
        .any(|l| is_cfg(l.trim_start_matches("#!["), preds))
}

/// The parent module declares `mod <name>;` with `#[cfg(<pred>)]` among its attributes.
fn gated_by_parent(file: &Path, name: &str, preds: &[&str]) -> bool {
    parents(file).iter().any(|parent| {
        let Ok(text) = read_to_string(parent) else {
            return false;
        };
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        lines.iter().enumerate().any(|(i, line)| {
            declares(line, name)
                && lines[..i]
                    .iter()
                    .rev()
                    .take_while(|l| l.starts_with("#[") || l.starts_with("//"))
                    .any(|l| is_cfg(l.trim_start_matches("#["), preds))
        })
    })
}

/// The files that can declare the module rooted at `file`.
fn parents(file: &Path) -> Vec<PathBuf> {
    // `a/windows.rs` and `a/windows/mod.rs` are both declared from `a.rs`, `a/mod.rs`,
    // `a/lib.rs` or `a/main.rs` (the last two when `a` is a crate's `src`).
    let dir = if matches!(
        file.file_stem().and_then(|s| s.to_str()),
        Some("mod" | "main")
    ) {
        file.parent().and_then(Path::parent)
    } else {
        file.parent()
    };
    let Some(dir) = dir else { return Vec::new() };
    let mut out = vec![dir.join("mod.rs"), dir.join("lib.rs"), dir.join("main.rs")];
    if let (Some(up), Some(name)) = (dir.parent(), dir.file_name()) {
        out.push(up.join(format!("{}.rs", name.to_string_lossy())));
    }
    out
}

/// `line` is a `mod <name>;` declaration, with any visibility.
fn declares(line: &str, name: &str) -> bool {
    let line = line.split("//").next().unwrap_or("").trim();
    let Some(rest) = line.strip_suffix(';') else {
        return false;
    };
    let mut words: Vec<&str> = rest.split_whitespace().collect();
    if words.last() != Some(&name) {
        return false;
    }
    words.pop();
    words.last() == Some(&"mod")
}

/// `attr` (the text after `#[` or `#![`) is `cfg(<pred>)` for one of `preds`.
fn is_cfg(attr: &str, preds: &[&str]) -> bool {
    let compact: String = attr.chars().filter(|c| !c.is_whitespace()).collect();
    preds
        .iter()
        .any(|p| compact.starts_with(&format!("cfg({p})]")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_names() {
        assert_eq!(
            platform_module(Path::new("a/src/ipc/windows.rs")).map(|p| p.0),
            Some("windows")
        );
        assert_eq!(
            platform_module(Path::new("a/src/ipc/unix/mod.rs")).map(|p| p.0),
            Some("unix")
        );
        assert_eq!(
            platform_module(Path::new("a/examples/two_windows.rs")),
            None
        );
        assert_eq!(
            platform_module(Path::new("a/tests/linux/main.rs")).map(|p| p.0),
            Some("linux")
        );
        assert_eq!(platform_module(Path::new("a/src/windows/pipe.rs")), None);
        assert_eq!(platform_module(Path::new("a/src/main.rs")), None);
    }

    #[test]
    fn declarations_and_cfgs() {
        assert!(declares("pub(crate) mod windows;", "windows"));
        assert!(declares("mod unix; // pipes", "unix"));
        assert!(!declares("mod windows {", "windows"));
        assert!(!declares("use windows;", "windows"));
        assert!(is_cfg(
            "cfg(target_os = \"macos\")]",
            &["target_os=\"macos\""]
        ));
        assert!(is_cfg("cfg(windows)]", &["windows"]));
        assert!(!is_cfg("cfg(not(windows))]", &["windows"]));
        assert!(!is_cfg("cfg(any(windows, unix))]", &["windows"]));
    }

    #[test]
    fn gating() {
        let dir = std::env::temp_dir().join(format!("xtask-platform-{}", std::process::id()));
        let src = dir.join("crates/a/src");
        std::fs::create_dir_all(src.join("ipc")).unwrap();
        std::fs::write(src.join("lib.rs"), "mod ipc;\n").unwrap();
        std::fs::write(
            src.join("ipc.rs"),
            "#[cfg(windows)]\nmod windows;\n#[cfg(unix)]\n// pipes\nmod unix;\nmod linux;\n",
        )
        .unwrap();
        for name in ["windows", "unix", "linux"] {
            std::fs::write(src.join(format!("ipc/{name}.rs")), "").unwrap();
        }
        std::fs::create_dir_all(dir.join("crates/a/tests")).unwrap();
        std::fs::write(
            dir.join("crates/a/tests/macos.rs"),
            "//! Mac only.\n#![cfg(target_os = \"macos\")]\n",
        )
        .unwrap();
        let problems = check(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("linux"), "{problems:?}");
    }
}
