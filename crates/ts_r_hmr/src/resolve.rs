//! The `Resolve` seam: import specifier + importer → file, dependency or external URL.
//!
//! [`SimpleResolver`] handles relative and root-absolute paths (with extension and
//! `index.*` probing, and TS-style `./x.js` → `./x.ts`) and bare specifiers through
//! `node_modules` with the common `package.json` fields (`exports` with conditions and
//! single-`*` patterns, `module`, `main`). It is a stand-in: `ts_r_resolver` replaces it
//! by implementing [`Resolve`].

use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

/// Where a specifier points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolved {
    /// A project file (or a file inside a package reached through a relative path).
    File(PathBuf),
    /// A bare specifier resolved into `node_modules`. Served via `/@modules/<specifier>`.
    Dep {
        /// Package name, e.g. `react` or `@scope/pkg`.
        package: String,
        /// The full bare specifier, e.g. `react` or `lodash/fp`.
        specifier: String,
        /// The entry file it resolved to.
        file: PathBuf,
    },
    /// Left untouched in the output (`http:`, `https:`, `data:`, `//cdn`).
    External(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolveError {
    pub message: String,
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ResolveError {}

fn err(message: String) -> ResolveError {
    ResolveError { message }
}

/// Resolves import specifiers. Must be thread-safe: the server resolves from several
/// connection threads and the watcher thread at once.
pub trait Resolve: Send + Sync + fmt::Debug {
    fn resolve(&self, specifier: &str, importer: &Path) -> Result<Resolved, ResolveError>;
}

/// The stand-in resolver (see module docs).
#[derive(Clone, Debug)]
pub struct SimpleResolver {
    root: PathBuf,
    extensions: Vec<&'static str>,
    conditions: Vec<&'static str>,
}

impl SimpleResolver {
    /// `root` is the dev server root: `/x.js` resolves to `<root>/x.js`.
    pub fn new(root: impl Into<PathBuf>) -> SimpleResolver {
        SimpleResolver {
            root: root.into(),
            extensions: vec!["ts", "tsx", "mts", "js", "jsx", "mjs", "json"],
            conditions: vec!["browser", "import", "module", "development", "default"],
        }
    }

    fn file(&self, path: &Path) -> Option<PathBuf> {
        if is_file(path) {
            return Some(path.to_path_buf());
        }
        let name = path.file_name()?.to_str()?;
        // TS convention: `./x.js` may mean `./x.ts`.
        if let Some(stem) = name
            .strip_suffix(".js")
            .or_else(|| name.strip_suffix(".jsx"))
        {
            for ext in ["ts", "tsx"] {
                let p = path.with_file_name(format!("{stem}.{ext}"));
                if is_file(&p) {
                    return Some(p);
                }
            }
        }
        if let Some(stem) = name.strip_suffix(".mjs") {
            let p = path.with_file_name(format!("{stem}.mts"));
            if is_file(&p) {
                return Some(p);
            }
        }
        for ext in &self.extensions {
            let p = path.with_file_name(format!("{name}.{ext}"));
            if is_file(&p) {
                return Some(p);
            }
        }
        None
    }

    fn file_or_dir(&self, path: &Path) -> Option<PathBuf> {
        if let Some(f) = self.file(path) {
            return Some(f);
        }
        if path.is_dir() {
            if let Some(entry) = self.package_entry(path) {
                return Some(entry);
            }
            for ext in &self.extensions {
                let p = path.join(format!("index.{ext}"));
                if is_file(&p) {
                    return Some(p);
                }
            }
        }
        None
    }

    /// The entry of a directory with a `package.json` (`module`/`main`; no `exports`).
    fn package_entry(&self, dir: &Path) -> Option<PathBuf> {
        let pkg = read_json(&dir.join("package.json"))?;
        for field in ["module", "main"] {
            if let Some(Value::String(rel)) = pkg.get(field)
                && let Some(f) = self.file_or_index(&normalize(&dir.join(rel)))
            {
                return Some(f);
            }
        }
        None
    }

    fn file_or_index(&self, path: &Path) -> Option<PathBuf> {
        self.file(path).or_else(|| {
            self.extensions.iter().find_map(|ext| {
                let p = path.join(format!("index.{ext}"));
                is_file(&p).then_some(p)
            })
        })
    }

    fn bare(&self, specifier: &str, importer: &Path) -> Result<Resolved, ResolveError> {
        let (package, rest) = split_package(specifier)
            .filter(|(name, _)| !name.split('/').any(|s| s == "." || s == ".."))
            .ok_or_else(|| err(format!("invalid package specifier \"{specifier}\"")))?;
        let subpath = subpath_key(rest);
        let start = importer.parent().unwrap_or(&self.root);
        let mut dir = Some(start);
        while let Some(d) = dir {
            let pkg_dir = d.join("node_modules").join(package);
            if pkg_dir.is_dir() {
                let file = self.package_file(&pkg_dir, &subpath).ok_or_else(|| {
                    err(format!(
                        "package \"{package}\" has no entry for \"{specifier}\" (looked in {})",
                        pkg_dir.display()
                    ))
                })?;
                // A linked package (workspace, `npm link`) whose real path is outside any
                // `node_modules` is source code: served and hot-updated like the app's own
                // files, not pre-bundled (Vite does the same).
                if let Ok(real) = crate::paths::canonicalize(&file)
                    && !real.components().any(|c| c.as_os_str() == "node_modules")
                {
                    return Ok(Resolved::File(real));
                }
                return Ok(Resolved::Dep {
                    package: package.to_owned(),
                    specifier: specifier.to_owned(),
                    file,
                });
            }
            dir = d.parent();
        }
        Err(err(format!(
            "cannot find package \"{package}\" (is it installed in node_modules?)"
        )))
    }

    /// [`Self::in_package`], refusing files outside `pkg_dir`: `..` in the subpath or in an
    /// `exports` target must not reach the rest of the disk.
    fn package_file(&self, pkg_dir: &Path, subpath: &str) -> Option<PathBuf> {
        self.in_package(pkg_dir, subpath)
            .filter(|f| normalize(f).starts_with(normalize(pkg_dir)))
    }

    /// `subpath` is `"."` or `"./x/y"`.
    fn in_package(&self, pkg_dir: &Path, subpath: &str) -> Option<PathBuf> {
        let pkg = read_json(&pkg_dir.join("package.json"));
        if let Some(exports) = pkg.as_ref().and_then(|p| p.get("exports"))
            && let Some(target) = self.exports_target(exports, subpath)
            && let Some(f) = self.file(&normalize(&pkg_dir.join(target)))
        {
            return Some(f);
        }
        if subpath == "." {
            if let Some(pkg) = &pkg {
                for field in ["module", "main"] {
                    if let Some(Value::String(rel)) = pkg.get(field)
                        && let Some(f) = self.file_or_index(&normalize(&pkg_dir.join(rel)))
                    {
                        return Some(f);
                    }
                }
            }
            return self.file_or_index(&pkg_dir.join("index"));
        }
        // Lenient: deep imports of unexported files still work in dev.
        self.file_or_dir(&normalize(&pkg_dir.join(subpath)))
    }

    fn exports_target(&self, exports: &Value, subpath: &str) -> Option<String> {
        let is_subpath_map = match exports {
            Value::Object(m) => m.keys().any(|k| k.starts_with('.')),
            _ => false,
        };
        if !is_subpath_map {
            return (subpath == ".")
                .then(|| self.conditional(exports))
                .flatten();
        }
        let map = exports.as_object()?;
        if let Some(v) = map.get(subpath) {
            return self.conditional(v);
        }
        // Single `*` patterns; the longest prefix wins.
        let mut best: Option<(&str, &str, &Value)> = None;
        for (key, v) in map {
            if let Some((pre, post)) = key.split_once('*')
                && subpath.starts_with(pre)
                && subpath.ends_with(post)
                && subpath.len() >= pre.len() + post.len()
                && best.is_none_or(|(p, _, _)| pre.len() > p.len())
            {
                best = Some((pre, post, v));
            }
        }
        let (pre, post, v) = best?;
        let star = &subpath[pre.len()..subpath.len() - post.len()];
        Some(self.conditional(v)?.replace('*', star))
    }

    fn conditional(&self, v: &Value) -> Option<String> {
        match v {
            Value::String(s) => Some(s.clone()),
            Value::Array(items) => items.iter().find_map(|i| self.conditional(i)),
            Value::Object(m) => m.iter().find_map(|(k, v)| {
                self.conditions
                    .contains(&k.as_str())
                    .then(|| self.conditional(v))
                    .flatten()
            }),
            _ => None,
        }
    }
}

impl Resolve for SimpleResolver {
    fn resolve(&self, specifier: &str, importer: &Path) -> Result<Resolved, ResolveError> {
        if specifier.is_empty() {
            return Err(err("empty import specifier".to_owned()));
        }
        if specifier.starts_with("http://")
            || specifier.starts_with("https://")
            || specifier.starts_with("data:")
            || specifier.starts_with("//")
        {
            return Ok(Resolved::External(specifier.to_owned()));
        }
        let spec = specifier.split(['?', '#']).next().unwrap_or(specifier);
        if spec.starts_with("./") || spec.starts_with("../") || spec == "." || spec == ".." {
            let base = importer.parent().unwrap_or(&self.root);
            let path = normalize(&base.join(spec));
            return self.file_or_dir(&path).map(Resolved::File).ok_or_else(|| {
                err(format!(
                    "Failed to resolve import \"{specifier}\" from \"{}\". Does the file exist?",
                    importer.display()
                ))
            });
        }
        if let Some(rest) = spec.strip_prefix('/') {
            if let Some(abs) = rest.strip_prefix("@fs/") {
                let p = normalize(&crate::paths::from_fs_url(abs));
                return self
                    .file_or_dir(&p)
                    .map(Resolved::File)
                    .ok_or_else(|| err(format!("Failed to resolve import \"{specifier}\"")));
            }
            let path = normalize(&self.root.join(rest));
            return self
                .file_or_dir(&path)
                .map(Resolved::File)
                .ok_or_else(|| err(format!("Failed to resolve import \"{specifier}\"")));
        }
        if spec.starts_with("node:") {
            return Err(err(format!(
                "\"{specifier}\" is a Node.js built-in and cannot be imported in browser code"
            )));
        }
        if spec.starts_with('#') {
            return Err(err(format!(
                "package \"imports\" (\"{specifier}\") are not supported by the stand-in resolver yet"
            )));
        }
        self.bare(spec, importer)
    }
}

/// Splits `@scope/pkg/sub/path` into (`@scope/pkg`, `/sub/path`); `pkg` gives (`pkg`, `""`).
pub fn split_package(specifier: &str) -> Option<(&str, &str)> {
    let mut idx = 0;
    if specifier.starts_with('@') {
        idx = specifier.find('/')? + 1;
    }
    let name_end = specifier[idx..]
        .find('/')
        .map_or(specifier.len(), |i| idx + i);
    let name = &specifier[..name_end];
    if name.is_empty() || name.ends_with('/') {
        return None;
    }
    Some((name, &specifier[name_end..]))
}

/// The `exports` key for the rest returned by [`split_package`]: `"."` or `"./sub"`.
fn subpath_key(rest: &str) -> String {
    if rest.is_empty() || rest == "/" {
        ".".to_owned()
    } else {
        format!(".{rest}")
    }
}

fn is_file(p: &Path) -> bool {
    fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
}

fn read_json(p: &Path) -> Option<Value> {
    let text = fs::read_to_string(p).ok()?;
    serde_json::from_str(&text).ok()
}

/// Lexically normalizes `.` and `..` (no filesystem access, no symlink resolution).
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn split_package_names() {
        assert_eq!(split_package("react"), Some(("react", "")));
        assert_eq!(split_package("lodash/fp"), Some(("lodash", "/fp")));
        assert_eq!(split_package("@s/p"), Some(("@s/p", "")));
        assert_eq!(split_package("@s/p/x/y"), Some(("@s/p", "/x/y")));
        assert_eq!(split_package("@s"), None);
        assert_eq!(subpath_key("/x/y"), "./x/y");
        assert_eq!(subpath_key(""), ".");
    }

    #[test]
    fn bare_specifiers_stay_inside_the_package() {
        let t = TempDir::new("resolve-escape");
        t.write("secret.txt", "");
        t.write("node_modules/other/index.js", "");
        t.write("node_modules/@x/y/index.js", "");
        t.write(
            "node_modules/p/package.json",
            r#"{"exports":{"./up":"../../secret.txt"}}"#,
        );
        t.write("node_modules/p/index.js", "");
        let root = t.path();
        let r = SimpleResolver::new(root);
        let importer = root.join("index.html");
        for spec in [
            "p/../../secret.txt",
            "p/../other/index.js",
            "p/up",
            "@x/../other",
        ] {
            assert!(r.resolve(spec, &importer).is_err(), "{spec}");
        }
        assert!(matches!(
            r.resolve("p/./index.js", &importer),
            Ok(Resolved::Dep { .. })
        ));
    }

    #[test]
    fn resolves_relative_root_and_bare() {
        let t = TempDir::new("resolve");
        t.write("src/a.js", "");
        t.write("src/b.ts", "");
        t.write("src/dir/index.js", "");
        t.write(
            "node_modules/plain/package.json",
            r#"{"main":"lib/main.js"}"#,
        );
        t.write("node_modules/plain/lib/main.js", "");
        t.write(
            "node_modules/@s/cond/package.json",
            r#"{"exports":{".":{"require":"./cjs.js","import":"./esm.js"},"./feature/*":"./src/features/*.js","./pkg":"./pkg.js"}}"#,
        );
        t.write("node_modules/@s/cond/esm.js", "");
        // jotai's shape: `import` before `default` (sorted keys would pick `default`).
        t.write(
            "node_modules/ordered/package.json",
            r#"{"main":"./index.js","exports":{".":{"import":{"types":"./esm/index.d.mts","default":"./esm/index.mjs"},"default":{"types":"./index.d.ts","default":"./index.js"}}}}"#,
        );
        t.write("node_modules/ordered/esm/index.mjs", "");
        t.write("node_modules/ordered/index.js", "");
        t.write("node_modules/@s/cond/src/features/x.js", "");
        t.write("node_modules/@s/cond/pkg.js", "");
        t.write("node_modules/@s/cond/internal/deep.js", "");
        let root = t.path();
        let r = SimpleResolver::new(root);
        let importer = root.join("src/main.js");
        let file = |s: &str| match r.resolve(s, &importer) {
            Ok(Resolved::File(p)) => crate::paths::id(p.strip_prefix(root).unwrap()).into_owned(),
            other => panic!("{s}: {other:?}"),
        };
        let dep = |s: &str| match r.resolve(s, &importer) {
            Ok(Resolved::Dep { package, file, .. }) => (
                package,
                crate::paths::id(file.strip_prefix(root).unwrap()).into_owned(),
            ),
            other => panic!("{s}: {other:?}"),
        };
        assert_eq!(file("./a.js"), "src/a.js");
        assert_eq!(file("./a"), "src/a.js");
        assert_eq!(file("./b.js"), "src/b.ts");
        assert_eq!(file("./dir"), "src/dir/index.js");
        assert_eq!(file("/src/a.js"), "src/a.js");
        assert_eq!(file("../src/a.js?raw"), "src/a.js");
        assert_eq!(
            dep("plain"),
            ("plain".into(), "node_modules/plain/lib/main.js".into())
        );
        assert_eq!(
            dep("@s/cond"),
            ("@s/cond".into(), "node_modules/@s/cond/esm.js".into())
        );
        assert_eq!(dep("ordered").1, "node_modules/ordered/esm/index.mjs");
        assert_eq!(
            dep("@s/cond/feature/x").1,
            "node_modules/@s/cond/src/features/x.js"
        );
        assert_eq!(dep("@s/cond/pkg").1, "node_modules/@s/cond/pkg.js");
        assert_eq!(
            dep("@s/cond/internal/deep.js").1,
            "node_modules/@s/cond/internal/deep.js"
        );
        assert!(matches!(
            r.resolve("https://cdn/x.js", &importer),
            Ok(Resolved::External(_))
        ));
        assert!(r.resolve("./missing.js", &importer).is_err());
        assert!(r.resolve("missing-pkg", &importer).is_err());
        assert!(r.resolve("node:fs", &importer).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn linked_workspace_packages_are_source() {
        let t = TempDir::new("resolve-linked");
        t.write(
            "packages/ui/package.json",
            r#"{"name":"@w/ui","exports":{".":"./src/index.ts"}}"#,
        );
        t.write("packages/ui/src/index.ts", "");
        t.write("app/src/main.ts", "");
        std::fs::create_dir_all(t.path().join("app/node_modules/@w")).unwrap();
        std::os::unix::fs::symlink(
            t.path().join("packages/ui"),
            t.path().join("app/node_modules/@w/ui"),
        )
        .unwrap();
        let root = t.path().join("app");
        let r = SimpleResolver::new(&root);
        let got = r.resolve("@w/ui", &root.join("src/main.ts")).unwrap();
        let want = std::fs::canonicalize(t.path().join("packages/ui/src/index.ts")).unwrap();
        assert_eq!(got, Resolved::File(want));
    }
}
