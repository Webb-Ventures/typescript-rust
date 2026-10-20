//! Module specifiers for `import("...")` types in declaration files (design record 0012,
//! E2), following tsc 7.0.2's output (observed):
//!
//! - a specifier the importing file already uses for the module is reused;
//! - a file in a package (inside `node_modules`, or a workspace package reached through a
//!   `node_modules` symlink) is named through the package: its name when the package's entry
//!   point is the file, else an `exports` key that maps to it, else `name/sub/path`;
//! - anything else is a relative path: without extension and `/index`, except in an ES
//!   module under `node16`..`nodenext` (or a file whose own relative imports end in `.js`),
//!   where it keeps the JavaScript extension (`./a.js`, `./b.mjs`).
//!
//! Package specifiers are generated as candidates and kept only when they resolve back to
//! the file from the importing file ([`ModuleSpecifierHost::resolve`]), so `exports`,
//! `types`, `typesVersions` and symlinks are honoured by the resolver itself.

use std::sync::Arc;

use ts_r_config::fs::FileSystem;
use ts_r_config::jsonc::Value;
use ts_r_resolver::{PackageJson, ResolutionMode, Resolver};

/// What package specifiers need from the file system: resolution from a file, and
/// `package.json` contents. Implemented by the program's [`Resolver`].
pub trait ModuleSpecifierHost: Sync {
    /// The real path `spec` resolves to from the file `from` in `mode`.
    fn resolve(&self, from: &str, spec: &str, mode: ResolutionMode) -> Option<String>;
    /// The `package.json` in `dir`.
    fn package_json(&self, dir: &str) -> Option<Arc<PackageJson>>;
    /// The export conditions resolution uses in `mode` (`import`, `types`, `node`, ...).
    fn conditions(&self, mode: ResolutionMode) -> Vec<String> {
        let first = match mode {
            ResolutionMode::Import => "import",
            ResolutionMode::Require => "require",
        };
        vec![first.to_owned(), "types".to_owned()]
    }
}

impl std::fmt::Debug for dyn ModuleSpecifierHost + '_ {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ModuleSpecifierHost")
    }
}

impl<F: FileSystem> ModuleSpecifierHost for Resolver<F> {
    fn resolve(&self, from: &str, spec: &str, mode: ResolutionMode) -> Option<String> {
        self.resolve_module(spec, from, mode)
            .map(|r| r.resolved_file_name.clone())
    }

    fn package_json(&self, dir: &str) -> Option<Arc<PackageJson>> {
        Resolver::package_json(self, dir)
    }

    fn conditions(&self, mode: ResolutionMode) -> Vec<String> {
        Resolver::conditions(self, mode).to_vec()
    }
}

/// The [`ModuleSpecifierHost`] of a program loaded from `fs` with `options`: a resolver
/// set up like the one that loaded it.
pub fn specifier_host<F: FileSystem>(fs: F, options: &ts_r_config::CompilerOptions) -> Resolver<F> {
    specifier_host_with_references(fs, options, None)
}

/// [`specifier_host`] for a program loaded with project `references`: a package specifier
/// resolves back to a referenced project's output declaration as the program's own
/// resolutions do (observed: `import("package-c").MyType` for a type of
/// `package-c/out/index.d.ts`, `tsbuild/inferredTypeFromMonorepoReference`).
pub fn specifier_host_with_references<F: FileSystem>(
    fs: F,
    options: &ts_r_config::CompilerOptions,
    references: Option<Arc<ts_r_resolver::ProjectReferences>>,
) -> Resolver<F> {
    let mut o = ts_r_resolver::ResolverOptions::from_compiler_options(options);
    o.project_references = references;
    Resolver::new(fs, o)
}

/// How a relative specifier ends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Ending {
    /// `./a`, `./dir` for `./dir/index`.
    Minimal,
    /// `./a.js`, `./dir/index.js`.
    Js,
    /// `./a.ts`, `./dir/index.ts`: the importing file's own relative imports name `.ts`
    /// files (`allowImportingTsExtensions`; observed in tsc 7). Declaration files still
    /// end in `.js`.
    Ts,
}

/// The directory part of `p` (`/a/b` for `/a/b/c.ts`).
fn dir_of(p: &str) -> &str {
    &p[..p.rfind('/').unwrap_or(0)]
}

/// `(package root, package name, path inside the package)` for a file under
/// `node_modules`: `@types/x` is `x`, `@types/a__b` is `@a/b`.
fn node_modules_package(path: &str) -> Option<(String, String, String)> {
    let i = path.rfind("/node_modules/")?;
    let rest = &path[i + "/node_modules/".len()..];
    let mut parts = rest.splitn(3, '/');
    let first = parts.next()?;
    let (name, consumed) = if first.starts_with('@') {
        let second = parts.next()?;
        (format!("{first}/{second}"), first.len() + 1 + second.len())
    } else {
        (first.to_owned(), first.len())
    };
    let inner = rest.get(consumed + 1..)?.to_owned();
    let root = path[..i + "/node_modules/".len() + consumed].to_owned();
    let name = match name.strip_prefix("@types/") {
        Some(t) => match t.split_once("__") {
            Some((scope, n)) => format!("@{scope}/{n}"),
            None => t.to_owned(),
        },
        None => name,
    };
    Some((root, name, inner))
}

/// The nearest `package.json` with a `name` at or above `dir`, as `(root, name)`.
fn named_package(host: &dyn ModuleSpecifierHost, dir: &str) -> Option<(String, String)> {
    let mut d = dir.to_owned();
    for _ in 0..64 {
        if let Some(pj) = host.package_json(&d)
            && let Some(name) = &pj.name
        {
            return Some((d, name.clone()));
        }
        let parent = dir_of(&d).to_owned();
        if parent == d || parent.is_empty() {
            return None;
        }
        d = parent;
    }
    None
}

/// The directory of the nearest `package.json` at or above `dir`.
pub(crate) fn package_root(host: &dyn ModuleSpecifierHost, dir: &str) -> Option<String> {
    let mut d = dir.to_owned();
    for _ in 0..64 {
        if host.package_json(&d).is_some() {
            return Some(d);
        }
        let parent = dir_of(&d).to_owned();
        if parent == d || parent.is_empty() {
            return None;
        }
        d = parent;
    }
    None
}

/// The forms of `rel` (a path inside a package) an `exports` target may name: the
/// JavaScript file the declaration stands for, then the file itself.
fn target_forms(rel: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (ts, js) in [
        (".d.ts", ".js"),
        (".d.mts", ".mjs"),
        (".d.cts", ".cjs"),
        (".tsx", ".js"),
        (".ts", ".js"),
        (".mts", ".mjs"),
        (".cts", ".cjs"),
    ] {
        if let Some(stem) = rel.strip_suffix(ts) {
            out.push(format!("{stem}{js}"));
            break;
        }
    }
    out.push(rel.to_owned());
    out
}

/// Every string target of an `exports` value (conditions and fallbacks flattened).
fn export_targets<'v>(v: &'v Value, out: &mut Vec<&'v str>, depth: u32) {
    if depth > 16 {
        return;
    }
    if let Some(s) = v.as_str() {
        out.push(s);
    } else if let Some(items) = v.as_array() {
        for i in items {
            export_targets(i, out, depth + 1);
        }
    } else if let Some(members) = v.as_object() {
        for m in members {
            export_targets(&m.value, out, depth + 1);
        }
    }
}

/// Subpaths (`.`, `./cls`, `./other.js`) the `exports` of a package may name `rel` with.
fn export_subpaths(exports: &Value, rel: &str) -> Vec<String> {
    let mut out = Vec::new();
    let members = match exports.as_object() {
        Some(m) if m.iter().all(|m| m.key.starts_with('.')) => m,
        // Conditions or a string at the top: the package's `.` export.
        _ => {
            out.push(".".to_owned());
            return out;
        }
    };
    let forms = target_forms(rel);
    for m in members {
        let key = m.key.as_str();
        let mut targets = Vec::new();
        export_targets(&m.value, &mut targets, 0);
        for t in targets {
            let Some(t) = t.strip_prefix("./") else {
                continue;
            };
            if let (Some((kp, ks)), Some((tp, ts))) = (key.split_once('*'), t.split_once('*')) {
                for f in &forms {
                    if f.len() >= tp.len() + ts.len()
                        && let Some(mid) = f.strip_prefix(tp).and_then(|x| x.strip_suffix(ts))
                    {
                        out.push(format!("{kp}{mid}{ks}"));
                    }
                }
            } else if let Some(dir) = t.strip_suffix('/').or(t.is_empty().then_some(""))
                && key.ends_with('/')
            {
                // A directory export (`"./": "./"`).
                for f in &forms {
                    if let Some(sub) = f.strip_prefix(dir).map(|s| s.trim_start_matches('/'))
                        && (dir.is_empty() || f.starts_with(&format!("{dir}/")))
                    {
                        out.push(format!("{key}{sub}"));
                    }
                }
            } else if !key.contains('*') {
                out.push(key.to_owned());
            }
        }
    }
    out
}

/// The file name a relative specifier writes for a module file: `x.d.html.ts` is `x.html`.
fn module_file_stem(path: &str, ending: Ending) -> String {
    let name_start = path.rfind('/').map_or(0, |i| i + 1);
    let name = &path[name_start..];
    if let Some(stem) = name.strip_suffix(".ts")
        && let Some(i) = stem.find(".d.")
    {
        return format!("{}{}{}", &path[..name_start], &stem[..i], &stem[i + 2..]);
    }
    let lower = path.to_ascii_lowercase();
    let js = |ext: &str| -> &'static str {
        match ext {
            ".d.mts" | ".mts" | ".mjs" => ".mjs",
            ".d.cts" | ".cts" | ".cjs" => ".cjs",
            _ => ".js",
        }
    };
    for ext in [
        ".d.ts", ".d.mts", ".d.cts", ".tsx", ".ts", ".mts", ".cts", ".jsx", ".js", ".mjs", ".cjs",
    ] {
        if lower.ends_with(ext) {
            let stem = &path[..path.len() - ext.len()];
            // `.mts`/`.cts` modules cannot be imported without their extension.
            let keep = matches!(ext, ".d.mts" | ".mts" | ".mjs" | ".d.cts" | ".cts" | ".cjs");
            if ending == Ending::Ts && matches!(ext, ".ts" | ".tsx" | ".mts" | ".cts") {
                return path.to_owned();
            }
            return if ending != Ending::Minimal || keep {
                format!("{stem}{}", js(ext))
            } else if let Some(dir) = stem.strip_suffix("/index") {
                dir.to_owned()
            } else {
                stem.to_owned()
            };
        }
    }
    path.to_owned()
}

/// `./x`, `../y/x` from directory `from_dir` to `target` (both absolute).
fn relative_specifier(from_dir: &str, target: &str, is_directory: bool) -> String {
    let a: Vec<&str> = from_dir.split('/').filter(|s| !s.is_empty()).collect();
    let b: Vec<&str> = target.split('/').filter(|s| !s.is_empty()).collect();
    // The importing directory or one above it (a directory's `index` module): `.`, `..`.
    if is_directory && !b.is_empty() && b.len() <= a.len() && a[..b.len()] == b[..] {
        return match a.len() - b.len() {
            0 => ".".to_owned(),
            up => vec![".."; up].join("/"),
        };
    }
    let mut i = 0;
    while i < a.len() && i + 1 < b.len() && a[i] == b[i] {
        i += 1;
    }
    let mut out = String::new();
    if i == a.len() {
        out.push_str("./");
    } else {
        for _ in i..a.len() {
            out.push_str("../");
        }
    }
    out.push_str(&b[i..].join("/"));
    out
}

/// A relative specifier for `target` from the file `from`.
pub(crate) fn relative_module_specifier(from: &str, target: &str, ending: Ending) -> String {
    let stem = module_file_stem(target, ending);
    let rel = relative_specifier(dir_of(from), &stem, stem == dir_of(target));
    // `./dir` for `./dir/index` at the importing file's own directory is `.`.
    match rel.as_str() {
        "./" => ".".to_owned(),
        _ => rel,
    }
}

/// Specifiers naming `target` through package specifiers other files import
/// (`(specifier, resolved path)`): the specifier of a package entry point in `target`'s
/// directory or above, with `target`'s path below it.
pub(crate) fn named_import_candidates<'s>(
    imports: impl Iterator<Item = (&'s str, &'s str)>,
    target: &str,
    ending: Ending,
) -> Vec<String> {
    let mut out = Vec::new();
    for (spec, resolved) in imports {
        if resolved == target {
            out.push(spec.to_owned());
            continue;
        }
        let root = dir_of(resolved);
        if let Some(rest) = target.strip_prefix(root)
            && rest.starts_with('/')
            && !rest.contains("/node_modules/")
        {
            let (name, sub) = ts_r_resolver::split_package_name(spec);
            if sub.is_empty() || sub == "." {
                out.push(format!("{name}{}", module_file_stem(rest, ending)));
            }
        }
    }
    out
}

/// A specifier naming `target` through its package from the file `from`, verified by
/// resolving it back. `None` when no package name of the file resolves to it.
pub(crate) fn package_specifier(
    host: &dyn ModuleSpecifierHost,
    from: &str,
    target: &str,
    mode: ResolutionMode,
    ending: Ending,
) -> Option<String> {
    let (root, name, rel) = match node_modules_package(target) {
        Some(p) => p,
        None => {
            // A workspace package (reached through a symlink in `node_modules`): only
            // from outside the package itself.
            let (root, name) = named_package(host, dir_of(target))?;
            if package_root(host, dir_of(from)).as_deref() == Some(root.as_str()) {
                return None;
            }
            let rel = target.get(root.len() + 1..)?.to_owned();
            (root, name, rel)
        }
    };
    let pj = host.package_json(&root);
    let mut candidates: Vec<String> = vec![name.clone()];
    // A directory export (`"./": "./"`) names the file even where resolution no longer
    // follows such mappings (node16 and later).
    let mut directory: Option<String> = None;
    match pj.as_ref().and_then(|p| p.exports.as_ref()) {
        Some(exports) => {
            for sub in export_subpaths(exports, &rel) {
                let spec = match sub.strip_prefix('.') {
                    Some("") => name.clone(),
                    Some(s) => format!("{name}{s}"),
                    None => continue,
                };
                if directory.is_none() && sub.len() > 2 && is_directory_export(exports, &sub) {
                    directory = Some(spec.clone());
                }
                candidates.push(spec);
            }
        }
        None => {
            let stem = module_file_stem(&format!("/{rel}"), ending);
            candidates.push(format!("{name}{stem}"));
            if ending == Ending::Minimal {
                let js = module_file_stem(&format!("/{rel}"), Ending::Js);
                candidates.push(format!("{name}{js}"));
            }
        }
    }
    let mut seen = Vec::new();
    for c in candidates {
        if seen.contains(&c) {
            continue;
        }
        if host.resolve(from, &c, mode).as_deref() == Some(target) {
            return Some(c);
        }
        seen.push(c);
    }
    if let Some(exports) = pj.as_ref().and_then(|p| p.exports.as_ref()) {
        let conditions = host.conditions(mode);
        if let Some(sub) = condition_branch_subpath(exports, &rel, &conditions) {
            return Some(match sub.strip_prefix('.') {
                Some("") | None => name,
                Some(s) => format!("{name}{s}"),
            });
        }
    }
    directory
}

/// The subpath of `exports` (an exact key, no pattern) one of whose branches names `rel`
/// when every applicable branch is tried in order, not only the first that resolution
/// takes: a key that is `default` or one of `conditions`. tsc 7 names a file through such
/// a branch (observed: `"types": { "import": "./index.d.mts", "default": "./index.d.ts" }`
/// names `index.d.ts` `"ws"` from an ES module, where resolution reaches `index.d.mts`).
fn condition_branch_subpath(exports: &Value, rel: &str, conditions: &[String]) -> Option<String> {
    let forms = target_forms(rel);
    let reaches = |v: &Value| branch_reaches(v, &forms, conditions, 0);
    match exports.as_object() {
        Some(m) if !m.is_empty() && m.iter().all(|m| m.key.starts_with('.')) => m
            .iter()
            .filter(|m| !m.key.contains('*') && !m.key.ends_with('/'))
            .find(|m| reaches(&m.value))
            .map(|m| m.key.clone()),
        _ => reaches(exports).then(|| ".".to_owned()),
    }
}

/// Whether an `exports` value names one of `forms` through a branch [`condition_branch_subpath`]
/// accepts.
fn branch_reaches(v: &Value, forms: &[String], conditions: &[String], depth: u32) -> bool {
    if depth > 16 {
        return false;
    }
    if let Some(s) = v.as_str() {
        return s
            .strip_prefix("./")
            .is_some_and(|t| forms.iter().any(|f| f == t));
    }
    if let Some(items) = v.as_array() {
        return items
            .iter()
            .any(|i| branch_reaches(i, forms, conditions, depth + 1));
    }
    v.as_object().is_some_and(|members| {
        members.iter().any(|m| {
            (m.key == "default" || conditions.contains(&m.key))
                && branch_reaches(&m.value, forms, conditions, depth + 1)
        })
    })
}

/// Whether the subpath `sub` comes from a directory key (`"./"`) of `exports`.
fn is_directory_export(exports: &Value, sub: &str) -> bool {
    exports.as_object().is_some_and(|m| {
        m.iter().any(|m| {
            m.key.ends_with('/') && sub.starts_with(m.key.as_str()) && !m.key.contains('*')
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_of_node_modules_paths() {
        assert_eq!(
            node_modules_package("/p/node_modules/@types/a__b/index.d.ts"),
            Some((
                "/p/node_modules/@types/a__b".to_owned(),
                "@a/b".to_owned(),
                "index.d.ts".to_owned()
            ))
        );
        assert_eq!(
            node_modules_package("/p/node_modules/x/dist/y.d.ts").map(|p| p.1),
            Some("x".to_owned())
        );
    }

    #[test]
    fn relative_forms() {
        let r = |f: &str, t: &str, e| relative_module_specifier(f, t, e);
        assert_eq!(r("/a/b.ts", "/a/c/index.ts", Ending::Minimal), "./c");
        assert_eq!(r("/a/b.ts", "/a/c/index.ts", Ending::Js), "./c/index.js");
        assert_eq!(r("/a/b.ts", "/a/c/index.ts", Ending::Ts), "./c/index.ts");
        assert_eq!(r("/a/b.ts", "/a/c.tsx", Ending::Ts), "./c.tsx");
        assert_eq!(r("/a/b.ts", "/a/c.d.ts", Ending::Ts), "./c.js");
        assert_eq!(r("/a/b.ts", "/a/c.d.mts", Ending::Minimal), "./c.mjs");
        assert_eq!(
            r("/a/b.ts", "/a/foo.d.html.ts", Ending::Minimal),
            "./foo.html"
        );
        assert_eq!(r("/a/b/x.ts", "/a/p/index.ts", Ending::Minimal), "../p");
        // A directory's `index` at or above the importing file (observed in tsc 7).
        assert_eq!(r("/a/index.ts", "/a/index.ts", Ending::Minimal), ".");
        assert_eq!(r("/a/b/c/x.ts", "/a/index.ts", Ending::Minimal), "../..");
        // An ordinary file stem can also name an importing directory.
        assert_eq!(r("/p/lib/index.ts", "/p/lib.ts", Ending::Minimal), "../lib");
        assert_eq!(
            r("/p/lib/sub/x.ts", "/p/lib.d.ts", Ending::Minimal),
            "../../lib"
        );
    }

    #[test]
    fn exports_subpaths() {
        let parse = |t: &str| ts_r_config::jsonc::parse(t, "p").root.expect("json");
        let v = parse(r#"{".": "./index.js", "./cls": "./foo.js"}"#);
        assert_eq!(export_subpaths(&v, "foo.d.ts"), vec![".", "./cls"]);
        let v = parse(r#"{"./*": "./*"}"#);
        assert_eq!(
            export_subpaths(&v, "other.d.ts"),
            vec!["./other.js", "./other.d.ts"]
        );
    }

    #[test]
    fn condition_branches() {
        let parse = |t: &str| ts_r_config::jsonc::parse(t, "p").root.expect("json");
        let conds = |m: &str| vec![m.to_owned(), "types".to_owned()];
        let v = parse(
            r#"{".": {"types": {"import": "./index.d.mts", "default": "./index.d.ts"}}, "./package.json": "./package.json"}"#,
        );
        assert_eq!(
            condition_branch_subpath(&v, "index.d.ts", &conds("import")).as_deref(),
            Some(".")
        );
        // A branch for the other mode only is not taken.
        let v = parse(r#"{"import": "./a.d.mts", "require": "./a.d.ts"}"#);
        assert_eq!(
            condition_branch_subpath(&v, "a.d.ts", &conds("import")),
            None
        );
        assert_eq!(
            condition_branch_subpath(&v, "a.d.ts", &conds("require")).as_deref(),
            Some(".")
        );
    }
}
