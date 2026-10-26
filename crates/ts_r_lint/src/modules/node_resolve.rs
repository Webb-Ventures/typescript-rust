//! The default resolver (`eslint-import-resolver-node` over `resolve`), as observed:
//!
//! - Builtins (`fs`, `node:fs`) are found with no path.
//! - The importing file's directory must exist; it is resolved through symlinks first.
//! - A relative or absolute specifier (`./x`, `../x`, `.`, `..`, `/x`) is tried as a file
//!   (as written, then with each extension: `.mjs`, `.js`, `.json`, `.node` unless the
//!   resolver's `extensions` say otherwise), then as a directory.
//! - A bare specifier is looked up in `node_modules` (or the `moduleDirectory` names) of
//!   every ancestor directory, nearest first, then in the `paths` directories; a candidate
//!   counts only when its parent directory exists.
//! - A directory loads its `package.json` `main` (`.` and `./` mean `index`) as a file, then
//!   as a directory, then `index`; a `main` that leads nowhere fails the whole resolution.
//!   Without a usable `main` the directory's `index` is tried. The `module` and
//!   `jsnext:main` fields never apply (the plugin's package filter gets the `package.json`
//!   path as its directory, so its lookups always fail).
//! - The result is the real path (symlinks resolved).

use serde_json::Value;
use ts_r_config::FileSystem;
use ts_r_config::path;

use super::{Resolved, is_core_module};

#[derive(Debug, Clone)]
pub(crate) struct NodeOptions {
    extensions: Vec<String>,
    module_directory: Vec<String>,
    paths: Vec<String>,
}

impl NodeOptions {
    pub(crate) fn from_config(cfg: &Value) -> NodeOptions {
        let list = |v: &Value| -> Option<Vec<String>> {
            match v {
                Value::Array(a) => Some(
                    a.iter()
                        .filter_map(|s| s.as_str().map(str::to_owned))
                        .collect(),
                ),
                Value::String(s) => Some(vec![s.clone()]),
                _ => None,
            }
        };
        NodeOptions {
            extensions: list(&cfg["extensions"]).unwrap_or_else(|| {
                [".mjs", ".js", ".json", ".node"]
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect()
            }),
            module_directory: list(&cfg["moduleDirectory"])
                .unwrap_or_else(|| vec!["node_modules".to_owned()]),
            paths: list(&cfg["paths"]).unwrap_or_default(),
        }
    }
}

/// A package whose `main` leads nowhere: the resolution fails outright.
struct BadMain;

struct Lookup<'a> {
    fs: &'a dyn FileSystem,
    opts: &'a NodeOptions,
    /// Directories loaded so far, to stop `main` cycles.
    depth: u32,
}

fn is_relative(x: &str) -> bool {
    x == "."
        || x == ".."
        || x.starts_with("./")
        || x.starts_with("../")
        || x.starts_with('/')
        || x.starts_with('\\')
        || {
            let b = x.as_bytes();
            b.len() >= 3
                && b[0].is_ascii_alphabetic()
                && b[1] == b':'
                && matches!(b[2], b'/' | b'\\')
        }
}

pub(crate) fn resolve(
    fs: &dyn FileSystem,
    spec: &str,
    basedir: &str,
    opts: &NodeOptions,
    cwd: &str,
) -> Resolved {
    if is_core_module(spec) {
        return Resolved::Core;
    }
    if !fs.directory_exists(basedir) {
        return Resolved::NotFound;
    }
    let start = path::normalize(&fs.realpath(basedir));
    let mut l = Lookup { fs, opts, depth: 0 };
    let found = if is_relative(spec) {
        let mut res = path::resolve(&start, &path::normalize_slashes(spec));
        if spec == "." || spec == ".." || spec.ends_with('/') {
            res.push('/');
        }
        match l.file(&res) {
            Some(f) => Ok(Some(f)),
            None => l.directory(&res),
        }
    } else {
        l.node_modules(spec, &start, cwd)
    };
    match found {
        Ok(Some(f)) => Resolved::File(real_path(fs, &f).into()),
        _ => Resolved::NotFound,
    }
}

/// The file's real path, keeping the case it was asked for when no symlink is involved
/// (as macOS' `realpath` does, where the plugin's tests were recorded).
fn real_path(fs: &dyn FileSystem, f: &str) -> String {
    let f = path::normalize(f);
    let real = path::normalize(&fs.realpath(&f));
    if path::eq(&real, &f, false) { f } else { real }
}

impl Lookup<'_> {
    fn is_file(&self, p: &str) -> bool {
        // `x/` names a directory: never a file.
        !p.ends_with('/') && self.fs.file_exists(p)
    }

    fn file(&self, x: &str) -> Option<String> {
        if self.is_file(x) {
            return Some(x.to_owned());
        }
        self.opts
            .extensions
            .iter()
            .map(|e| format!("{x}{e}"))
            .find(|f| self.is_file(f))
    }

    fn directory(&mut self, x: &str) -> Result<Option<String>, BadMain> {
        self.depth += 1;
        if self.depth > 32 {
            return Ok(None);
        }
        let dir = x.trim_end_matches('/');
        let real = if self.fs.directory_exists(dir) {
            path::normalize(&self.fs.realpath(dir))
        } else {
            dir.to_owned()
        };
        let pkgfile = path::combine(&real, "package.json");
        if self.fs.file_exists(&pkgfile) {
            let pkg = self
                .fs
                .read_file(&pkgfile)
                .and_then(|t| serde_json::from_str::<Value>(&t).ok());
            if let Some(main) = pkg.as_ref().map(|p| &p["main"])
                && is_truthy(main)
            {
                let Some(main) = main.as_str() else {
                    return Err(BadMain);
                };
                let main = if main == "." || main == "./" {
                    "index"
                } else {
                    main
                };
                let main_path = path::resolve(dir, main);
                if let Some(f) = self.file(&main_path) {
                    return Ok(Some(f));
                }
                if let Ok(Some(f)) = self.directory(&main_path) {
                    return Ok(Some(f));
                }
                if let Some(f) = self.file(&path::combine(dir, "index")) {
                    return Ok(Some(f));
                }
                return Err(BadMain);
            }
        }
        Ok(self.file(&path::combine(dir, "index")))
    }

    fn node_modules(
        &mut self,
        spec: &str,
        start: &str,
        cwd: &str,
    ) -> Result<Option<String>, BadMain> {
        let mut dirs = Vec::new();
        let mut cur = start.to_owned();
        loop {
            for m in &self.opts.module_directory {
                dirs.push(path::resolve(&cur, m));
            }
            let parent = path::directory_of(&cur).to_owned();
            if parent == cur || parent.is_empty() {
                break;
            }
            cur = parent;
        }
        for p in &self.opts.paths {
            dirs.push(path::resolve(cwd, p));
        }
        for d in dirs {
            let candidate = path::combine(&d, spec);
            let candidate = path::normalize(&candidate);
            if !self.fs.directory_exists(path::directory_of(&candidate)) {
                continue;
            }
            if let Some(f) = self.file(&candidate) {
                return Ok(Some(f));
            }
            if let Some(f) = self.directory(&candidate)? {
                return Ok(Some(f));
            }
        }
        Ok(None)
    }
}

fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        _ => true,
    }
}
