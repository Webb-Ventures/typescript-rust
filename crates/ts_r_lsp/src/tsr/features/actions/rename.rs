//! `workspace/willRenameFiles`: import and tsconfig updates when files or folders move, as
//! tsserver 6.0.3's `getEditsForFileRename` makes them on the `rename-*` fixtures (rules
//! observed by running it, not taken from its source):
//!
//! - **Relative specifiers** that resolve to a moved file, and relative specifiers inside a
//!   moved file, are recomputed for the new locations and stay relative. The specifier keeps
//!   its style: an extension (`.js`, `.ts`) stays an extension, `/index` stays explicit or
//!   implicit.
//! - **Non-relative specifiers** that resolve to a moved file (through `paths` or a package)
//!   are recomputed from scratch with the program's (old) options, in this order: the first
//!   `paths` pattern, in the object's order, whose substitution covers the new path (a
//!   `paths` entry naming the old path exactly no longer does, even though the tsconfig edit
//!   below rewrites it); then the package the old specifier named, when the new path is
//!   still inside it (`pkg/sub/path` without `exports`; with `exports`, a key whose target
//!   matches); else a relative specifier. Extensions and `/index` follow the old specifier's
//!   style. Non-relative specifiers whose target did not move are left alone, even in moved
//!   files.
//! - A renamed directory moves every program file under it.
//! - **tsconfig:** in each loaded project's config, strings in `files`, `include`, `exclude`,
//!   the values of `compilerOptions.paths` and every path-typed compiler option (`rootDir`,
//!   `outDir`, `baseUrl`, `declarationDir`, `tsBuildInfoFile`, `outFile`, `rootDirs`,
//!   `typeRoots`, ...) that name the old path, or a path under the old folder, are rewritten
//!   to the new path relative to the config's directory, without `./` (`./src/util.ts` →
//!   `src/lib/util.ts`; glob tails such as `/**/*.ts` are kept). Strings that only share a
//!   prefix (`src/library` for `src/lib`), `extends`, `references`, `types`, `lib` and
//!   `mapRoot`/`sourceRoot` are not touched. A config inside a moved folder is the exception
//!   (`@differs`): its relative strings are rebased onto its new directory, where tsserver
//!   keeps the old one and the rewritten strings name paths that do not exist.
//! - **Projects:** tsserver answers for the projects it has loaded: an importer in a project
//!   whose files are not open is not edited. The coordinator does the same: every loaded
//!   project (and the projects owning the renamed files) computes its edits; [`merge`] joins
//!   them, a file in several projects edited once.
//!
//! The program may see the files before the move (LSP `willRenameFiles`: edits keyed by the
//! old paths, the client moves the files afterwards) or after it (tsserver's
//! `getEditsForFileRename`, sent once the files moved and the watcher may have reloaded the
//! program: edits keyed by the new paths). After the move, specifiers no longer resolve
//! through the program's resolutions, so relative and `paths` specifiers are also resolved
//! by path against the program's files and the old paths.

use super::edit::{Edit, lsp_edits};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashSet};
use ts_r_ast::Node;
use ts_r_checker::{Checker, ProgramFiles};
use ts_r_config::options::{OptionType, find_option};
use ts_r_config::{FileSystem, PathsMap, jsonc, path};
use ts_r_resolver::ResolverOptions;

/// `p` moved by the rename `a` → `b` (the file itself, or a path under the folder `a`).
fn mapped(p: &str, a: &str, b: &str) -> Option<String> {
    if p == a {
        return Some(b.to_string());
    }
    let rest = p.strip_prefix(a)?;
    rest.starts_with('/').then(|| format!("{b}{rest}"))
}

/// The `WorkspaceEdit` updating module specifiers of the program's files for `renames` (old
/// path, new path). `fs` reads package manifests (bare specifiers into a moved file).
pub fn will_rename_files(
    program: &ProgramFiles,
    c: &Checker<'_>,
    fs: &dyn FileSystem,
    renames: &[(String, String)],
) -> Value {
    // Old path ↔ new path of every moved program file, whether the program has it at the old
    // path (not moved yet) or at the new one (moved).
    let mut new_of: BTreeMap<String, String> = BTreeMap::new();
    let mut old_of: BTreeMap<String, String> = BTreeMap::new();
    let current: HashSet<&str> = c
        .files
        .iter()
        .filter(|f| !f.is_lib)
        .map(|f| f.path.as_str())
        .collect();
    for f in c.files.iter().filter(|f| !f.is_lib) {
        for (from, to) in renames {
            if let Some(n) = mapped(&f.path, from, to) {
                new_of.insert(f.path.clone(), n.clone());
                old_of.insert(n, f.path.clone());
            } else if let Some(o) = mapped(&f.path, to, from)
                && !current.contains(o.as_str())
            {
                new_of.insert(o.clone(), f.path.clone());
                old_of.insert(f.path.clone(), o);
            }
        }
    }
    let mut changes = Map::new();
    let ropts = ResolverOptions::from_compiler_options(&program.options);
    // tsserver 6 still resolves `paths` against `baseUrl` when it is set.
    let aliases = Aliases {
        paths: ropts.paths.as_ref(),
        base: program.options.base_url().or(ropts.paths_base.as_deref()),
    };
    let known = |p: &str| current.contains(p) || new_of.contains_key(p);
    // After the move, a project that never had the moved files (an importer in another
    // project whose import no longer resolves) finds them on disk at their new paths.
    let on_disk = |p: &str| {
        renames
            .iter()
            .find_map(|(a, b)| mapped(p, a, b))
            .filter(|q| fs.file_exists(q))
    };
    let moved_on_disk = |p: &str| on_disk(p).is_some();
    for f in c.files.iter() {
        if f.is_lib || f.is_external {
            continue;
        }
        // Where the file was and where it will be.
        let (old_at, new_at) = match (old_of.get(&f.path), new_of.get(&f.path)) {
            (Some(o), _) => (o.as_str(), f.path.as_str()),
            (None, Some(n)) => (f.path.as_str(), n.as_str()),
            (None, None) => (f.path.as_str(), f.path.as_str()),
        };
        let moved_after = old_at != f.path;
        let ast = &f.ast;
        let mut edits = Vec::new();
        for r in &f.bound.module_requests {
            let Some(spec) = ast.name_of(r.node) else {
                continue;
            };
            let relative = path::is_explicitly_relative(spec);
            if !relative && path::is_rooted(spec) {
                continue;
            }
            let resolved = if moved_after {
                None
            } else {
                program
                    .resolutions
                    .get(&(f.path.clone(), spec.to_string()))
                    .and_then(|r| r.as_ref())
                    .map(|r| r.resolved.clone())
            };
            let by_path = |known: &dyn Fn(&str) -> bool| {
                if relative {
                    resolve_by_path(old_at, spec, known)
                } else {
                    aliases
                        .resolve(spec, known)
                        .or_else(|| package_resolve(fs, spec, old_at, known))
                }
            };
            // The target's old and new paths.
            let (target_old, target_new) = match resolved.or_else(|| by_path(&known)) {
                Some(t) => {
                    let old = old_of.get(&t).cloned().unwrap_or(t);
                    let new = new_of.get(&old).cloned().unwrap_or_else(|| old.clone());
                    (old, new)
                }
                None => {
                    let Some(t) = by_path(&moved_on_disk) else {
                        continue;
                    };
                    let Some(n) = on_disk(&t) else {
                        continue;
                    };
                    (t, n)
                }
            };
            let target_new = target_new.as_str();
            let target_moved = target_new != target_old;
            let ns = if relative {
                if old_at == new_at && !target_moved {
                    continue;
                }
                respecify(spec, new_at, target_new)
            } else {
                if !target_moved {
                    continue;
                }
                aliases
                    .specifier(spec, target_new)
                    .or_else(|| package_specifier(fs, spec, old_at, &target_old, target_new))
                    .unwrap_or_else(|| respecify(spec, new_at, target_new))
            };
            if ns == spec {
                continue;
            }
            if !matches!(ast.node(r.node), Node::StringLit { .. }) {
                continue;
            }
            let sp = ast.span(r.node);
            if sp.end < sp.start + 2 {
                continue;
            }
            edits.push(Edit::new(sp.start + 1, sp.end - 1, ns));
        }
        if !edits.is_empty() {
            changes.insert(
                crate::uri::path_to_uri(&f.path),
                Value::Array(lsp_edits(&f.text, &edits)),
            );
        }
    }
    json!({ "changes": changes })
}

/// The `paths` mapping of a program, absolute substitutions resolved against `base`.
struct Aliases<'a> {
    paths: Option<&'a PathsMap>,
    base: Option<&'a str>,
}

impl Aliases<'_> {
    fn entries(&self) -> impl Iterator<Item = (&str, String)> {
        let base = self.base.unwrap_or("");
        self.paths
            .into_iter()
            .flat_map(|m| m.0.iter())
            .flat_map(move |(pat, subs)| {
                subs.iter()
                    .map(move |s| (pat.as_str(), path::resolve(base, s)))
            })
    }

    /// The file `spec` names through `paths`, by path (`known` decides which candidates
    /// exist): the program after the move no longer resolves it.
    fn resolve(&self, spec: &str, known: &dyn Fn(&str) -> bool) -> Option<String> {
        for (pat, sub) in self.entries() {
            let star = match pat.split_once('*') {
                Some((pre, suf)) => {
                    match spec.strip_prefix(pre).and_then(|r| r.strip_suffix(suf)) {
                        Some(s) => s,
                        None => continue,
                    }
                }
                None if pat == spec => "",
                None => continue,
            };
            let base = sub.replacen('*', star, 1);
            if let Some(p) = candidates(&base).into_iter().find(|p| known(p)) {
                return Some(p);
            }
        }
        None
    }

    /// The first `paths` pattern whose substitution covers `to`, in the style of `old`.
    fn specifier(&self, old: &str, to: &str) -> Option<String> {
        for (pat, sub) in self.entries() {
            match (pat.split_once('*'), sub.split_once('*')) {
                (Some((ppre, psuf)), Some((spre, ssuf))) => {
                    let Some(star) = to.strip_prefix(spre).and_then(|r| r.strip_suffix(ssuf))
                    else {
                        continue;
                    };
                    if star.is_empty() {
                        continue;
                    }
                    let star = if ssuf.is_empty() {
                        styled(old, star)
                    } else {
                        star.to_string()
                    };
                    return Some(format!("{ppre}{star}{psuf}"));
                }
                (None, None) => {
                    if candidates(&sub).iter().any(|c| c == to) {
                        return Some(pat.to_string());
                    }
                }
                _ => {}
            }
        }
        None
    }
}

/// `old`, a `paths` alias, rewritten to name `to` (Move to a new file retargeting an
/// importer), when an alias covers `to`.
pub(super) fn alias_specifier(program: &ProgramFiles, old: &str, to: &str) -> Option<String> {
    let ropts = ResolverOptions::from_compiler_options(&program.options);
    let aliases = Aliases {
        paths: ropts.paths.as_ref(),
        base: program.options.base_url().or(ropts.paths_base.as_deref()),
    };
    aliases.specifier(old, to)
}

/// Whether `spec` matches one of the program's `paths` patterns.
pub(super) fn is_alias(program: &ProgramFiles, spec: &str) -> bool {
    let ropts = ResolverOptions::from_compiler_options(&program.options);
    let aliases = Aliases {
        paths: ropts.paths.as_ref(),
        base: program.options.base_url().or(ropts.paths_base.as_deref()),
    };
    aliases.resolve(spec, &|_| true).is_some()
}

/// A bare specifier's package name and subpath (`@s/p/a/b` → `@s/p`, `a/b`).
fn split_package(spec: &str) -> Option<(String, &str)> {
    let name_len = if spec.starts_with('@') { 2 } else { 1 };
    let mut ends = spec.match_indices('/').map(|(i, _)| i);
    let end = match name_len {
        1 => ends.next(),
        _ => ends.nth(1),
    };
    match end {
        Some(i) => Some((spec[..i].to_string(), &spec[i + 1..])),
        None if name_len == 2 && !spec.contains('/') => None,
        None => Some((spec.to_string(), "")),
    }
}

/// The real directory of package `name` seen from a file at `from` (`node_modules` above it).
fn package_dir(fs: &dyn FileSystem, name: &str, from: &str) -> Option<String> {
    let mut dir = path::directory_of(from).to_string();
    for _ in 0..128 {
        let cand = format!("{dir}/node_modules/{name}");
        if fs.directory_exists(&cand) {
            return Some(fs.realpath(&cand));
        }
        let up = path::directory_of(&dir).to_string();
        if up == dir || up.is_empty() {
            break;
        }
        dir = up;
    }
    None
}

fn manifest(fs: &dyn FileSystem, dir: &str) -> Option<jsonc::Value> {
    fs.read_file(&format!("{dir}/package.json"))
        .and_then(|t| jsonc::parse(&t, "package.json").root)
}

/// `exports` entries of a manifest: (key, value), a sugar form as `"."`.
fn exports_entries(exports: &jsonc::Value) -> Vec<(String, &jsonc::Value)> {
    match exports.as_object() {
        Some(m) if m.iter().any(|x| x.key.starts_with('.')) => {
            m.iter().map(|x| (x.key.clone(), &x.value)).collect()
        }
        _ => vec![(".".into(), exports)],
    }
}

/// The file a bare specifier names, by path through its package (the program after the
/// move no longer resolves it).
fn package_resolve(
    fs: &dyn FileSystem,
    spec: &str,
    from: &str,
    known: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let (name, sub) = split_package(spec)?;
    let real = package_dir(fs, &name, from)?;
    let m = manifest(fs, &real);
    let Some(exports) = m.as_ref().and_then(|m| m.get("exports")) else {
        if sub.is_empty() {
            return None;
        }
        return candidates(&format!("{real}/{sub}"))
            .into_iter()
            .find(|p| known(p));
    };
    let want = if sub.is_empty() {
        ".".to_string()
    } else {
        format!("./{sub}")
    };
    for (key, v) in exports_entries(exports) {
        let star = match key.split_once('*') {
            Some((pre, suf)) => match want.strip_prefix(pre).and_then(|r| r.strip_suffix(suf)) {
                Some(s) => s,
                None => continue,
            },
            None if key == want => "",
            None => continue,
        };
        let mut targets = Vec::new();
        collect_strings(v, &mut targets);
        for t in targets {
            let p = path::resolve(&real, &t.replacen('*', star, 1));
            if let Some(f) = candidates(&p).into_iter().find(|p| known(p)) {
                return Some(f);
            }
        }
    }
    None
}

/// The package specifier for `to` when the bare specifier `old` (of a file at `from`, naming
/// `target_old`) named a package that still contains `to`.
fn package_specifier(
    fs: &dyn FileSystem,
    old: &str,
    from: &str,
    target_old: &str,
    to: &str,
) -> Option<String> {
    let (name, _) = split_package(old)?;
    // The package's (real) directory: through `node_modules` above the importer, or the
    // directory above the old target whose manifest has that name.
    let mut real = package_dir(fs, &name, from);
    if real.is_none() {
        let mut d = path::directory_of(target_old).to_string();
        for _ in 0..128 {
            if let Some(t) = fs.read_file(&format!("{d}/package.json"))
                && let Some(root) = jsonc::parse(&t, "package.json").root
                && root.get("name").and_then(jsonc::Value::as_str) == Some(name.as_str())
            {
                real = Some(d);
                break;
            }
            let up = path::directory_of(&d).to_string();
            if up == d || up.is_empty() {
                break;
            }
            d = up;
        }
    }
    let real = real?;
    let manifest = manifest(fs, &real);
    if let Some(exports) = manifest.as_ref().and_then(|m| m.get("exports")) {
        return exports_specifier(&name, &real, exports, to);
    }
    let rest = to.strip_prefix(&real)?.strip_prefix('/')?;
    Some(format!("{name}/{}", styled(old, rest)))
}

/// `name` + the `exports` key whose target is `to`.
fn exports_specifier(name: &str, real: &str, exports: &jsonc::Value, to: &str) -> Option<String> {
    for (key, v) in exports_entries(exports) {
        let mut targets = Vec::new();
        collect_strings(v, &mut targets);
        for t in targets {
            let abs = path::resolve(real, t);
            let found = match (key.split_once('*'), abs.split_once('*')) {
                (Some((kpre, ksuf)), Some((tpre, tsuf))) => to
                    .strip_prefix(tpre)
                    .and_then(|r| r.strip_suffix(tsuf))
                    .filter(|s| !s.is_empty())
                    .map(|star| format!("{kpre}{star}{ksuf}")),
                (None, None) if abs == to => Some(key.clone()),
                _ => None,
            };
            if let Some(k) = found {
                let sub = k.strip_prefix('.').unwrap_or(&k);
                return Some(format!("{name}{sub}"));
            }
        }
    }
    None
}

fn collect_strings<'v>(v: &'v jsonc::Value, out: &mut Vec<&'v str>) {
    match &v.kind {
        jsonc::ValueKind::String(s) => out.push(s),
        jsonc::ValueKind::Array(a) => a.iter().for_each(|x| collect_strings(x, out)),
        jsonc::ValueKind::Object(m) => m.iter().for_each(|x| collect_strings(&x.value, out)),
        _ => {}
    }
}

/// Files a module path (without extension handling by the caller) may name, in lookup
/// order: itself, `.js` → TypeScript, an added extension, a directory's `index`.
fn candidates(base: &str) -> Vec<String> {
    let mut cands = vec![base.to_string()];
    for (js, tss) in [
        (".js", &[".ts", ".tsx", ".d.ts"][..]),
        (".jsx", &[".tsx"][..]),
        (".mjs", &[".mts", ".d.mts"][..]),
        (".cjs", &[".cts", ".d.cts"][..]),
    ] {
        if let Some(stem) = base.strip_suffix(js) {
            cands.extend(tss.iter().map(|t| format!("{stem}{t}")));
        }
    }
    for ext in [".ts", ".tsx", ".d.ts", ".js", ".jsx"] {
        cands.push(format!("{base}{ext}"));
    }
    for ext in [".ts", ".tsx", ".d.ts", ".js", ".jsx"] {
        cands.push(format!("{base}/index{ext}"));
    }
    cands
}

/// The file a relative specifier of a file at `from` names, by path: `known` decides which
/// candidates exist (`.js` may name a `.ts` file; a directory its `index`).
fn resolve_by_path(from: &str, spec: &str, known: &dyn Fn(&str) -> bool) -> Option<String> {
    let dir = from.rsplit_once('/').map_or("", |x| x.0);
    let mut parts: Vec<&str> = dir.split('/').collect();
    for seg in spec.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    candidates(&parts.join("/")).into_iter().find(|p| known(p))
}

const EXTS: &[&str] = &[
    ".d.ts", ".d.mts", ".d.cts", ".tsx", ".ts", ".mts", ".cts", ".jsx", ".js", ".mjs", ".cjs",
    ".json",
];

/// The relative specifier for file `to` from file `from`, in the style of `old`.
fn respecify(old: &str, from: &str, to: &str) -> String {
    let out = styled(old, &raw_relative(from, to));
    // A relative specifier for a directory index keeps its `./`.
    match out.as_str() {
        "." | ".." => format!("{out}/"),
        _ => out,
    }
}

/// `raw` (a path with the target's extension) in the style of the specifier `old`: its
/// extension or none, `.js` for TypeScript files when it used one, `/index` explicit only
/// when it named the index.
fn styled(old: &str, raw: &str) -> String {
    let (stem, ext) = match EXTS.iter().find(|e| raw.ends_with(*e)) {
        Some(e) => (&raw[..raw.len() - e.len()], *e),
        None => (raw, ""),
    };
    let old_ext = EXTS
        .iter()
        .find(|e| old.ends_with(*e))
        .copied()
        .unwrap_or("");
    let mut out = if old_ext.is_empty() {
        stem.to_string()
    } else if matches!(old_ext, ".js" | ".jsx" | ".mjs" | ".cjs")
        && !matches!(ext, ".js" | ".jsx" | ".mjs" | ".cjs")
    {
        // A `.js` specifier of a TypeScript file.
        let js = match ext {
            ".mts" | ".d.mts" => ".mjs",
            ".cts" | ".d.cts" => ".cjs",
            ".tsx" => old_ext,
            _ => ".js",
        };
        format!("{stem}{js}")
    } else {
        format!("{stem}{ext}")
    };
    // `./dir` for `./dir/index.ts` unless the old specifier named the index.
    let named_index = old_ext.is_empty() && (old.ends_with("/index") || old == "index");
    if old_ext.is_empty()
        && !named_index
        && let Some(dir) = out.strip_suffix("/index")
    {
        out = dir.to_string();
    }
    out
}

/// `./x/y.ts`-style path of `to` relative to the directory of `from`.
fn raw_relative(from: &str, to: &str) -> String {
    let from_dir: Vec<&str> = from
        .rsplit_once('/')
        .map_or("", |x| x.0)
        .split('/')
        .collect();
    let to_parts: Vec<&str> = to.split('/').collect();
    let mut common = 0;
    while common < from_dir.len()
        && common + 1 < to_parts.len()
        && from_dir[common] == to_parts[common]
    {
        common += 1;
    }
    let mut s = String::new();
    if common == from_dir.len() {
        s.push_str("./");
    } else {
        for _ in common..from_dir.len() {
            s.push_str("../");
        }
    }
    s.push_str(&to_parts[common..].join("/"));
    s
}

// ---- tsconfig ----

/// Edits of the tsconfig at `config` (text `text`) for `renames`: the strings of `files`,
/// `include`, `exclude`, `compilerOptions.paths` values and path-typed compiler options that
/// name a moved path (see the module docs).
pub fn config_edits(config: &str, text: &str, renames: &[(String, String)]) -> Vec<Edit> {
    let Some(root) = jsonc::parse(text, config).root else {
        return Vec::new();
    };
    let dir = path::directory_of(config);
    let mut strings: Vec<&jsonc::Value> = Vec::new();
    for key in ["files", "include", "exclude"] {
        if let Some(a) = root.get(key).and_then(jsonc::Value::as_array) {
            strings.extend(a.iter());
        }
    }
    if let Some(opts) = root
        .get("compilerOptions")
        .and_then(jsonc::Value::as_object)
    {
        for m in opts {
            if m.key == "paths" {
                for p in m.value.as_object().into_iter().flatten() {
                    strings.extend(p.value.as_array().into_iter().flatten());
                }
                continue;
            }
            match find_option(&m.key).map(|d| d.ty) {
                Some(OptionType::Path) => strings.push(&m.value),
                Some(OptionType::PathList) => {
                    strings.extend(m.value.as_array().into_iter().flatten());
                }
                _ => {}
            }
        }
    }
    // A config inside a moved folder moves too: its relative strings are rebased onto its
    // new directory, whether or not what they name moved.
    let moved_config = renames.iter().find_map(|(a, b)| mapped(config, a, b));
    let new_dir = moved_config.as_deref().map_or(dir, path::directory_of);
    let mut edits = Vec::new();
    for v in strings {
        let Some(s) = v.as_str() else {
            continue;
        };
        let abs = path::resolve(dir, s);
        let new = renames.iter().find_map(|(a, b)| mapped(&abs, a, b));
        let rel = match new {
            Some(new) if path::is_rooted(s) => new,
            None if moved_config.is_none() || path::is_rooted(s) => continue,
            // Still naming the same path from the new directory (`.` in a moved config).
            new if new.as_ref().unwrap_or(&abs) == &path::resolve(new_dir, s) => continue,
            new => match path::relative(new_dir, new.as_ref().unwrap_or(&abs), true) {
                r if r.is_empty() => ".".to_string(),
                r => r,
            },
        };
        let mut out = String::new();
        jsonc::write_json_string(&rel, &mut out);
        edits.push(Edit::new(v.span.0, v.span.1, out));
    }
    edits
}

/// The `WorkspaceEdit` of [`config_edits`] (`None` when nothing changes).
pub fn config_workspace_edit(
    config: &str,
    text: &str,
    renames: &[(String, String)],
) -> Option<Value> {
    let edits = config_edits(config, text, renames);
    if edits.is_empty() {
        return None;
    }
    let mut changes = Map::new();
    changes.insert(
        crate::uri::path_to_uri(config),
        Value::Array(lsp_edits(text, &edits)),
    );
    Some(json!({ "changes": changes }))
}

// ---- merging the answers of several projects ----

/// One `WorkspaceEdit` (`changes` form) from several projects' answers, in order: for each
/// document, an edit already present is not repeated and an edit overlapping one taken from
/// an earlier answer is dropped (a file in two projects is edited once).
pub fn merge(answers: impl IntoIterator<Item = Value>) -> Value {
    let mut out: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for a in answers {
        let Some(ch) = a.get("changes").and_then(Value::as_object) else {
            continue;
        };
        for (uri, list) in ch {
            let taken = out.entry(uri.clone()).or_default();
            let before = taken.len();
            for e in list.as_array().into_iter().flatten() {
                let Some(r) = crate::text::lsp_range(&e["range"]) else {
                    continue;
                };
                let key = |p: crate::text::Pos| (p.line, p.character);
                let (s, t) = (key(r.0), key(r.1));
                let clash = taken[..before].iter().any(|x| {
                    let Some(xr) = crate::text::lsp_range(&x["range"]) else {
                        return false;
                    };
                    let (xs, xt) = (key(xr.0), key(xr.1));
                    x == e || (s < xt && xs < t) || (s == xs && (s == t || xs == xt))
                });
                if !clash && !taken.contains(e) {
                    taken.push(e.clone());
                }
            }
        }
    }
    let mut changes = Map::new();
    for (uri, mut list) in out {
        if list.is_empty() {
            continue;
        }
        list.sort_by_key(|e| {
            crate::text::lsp_range(&e["range"])
                .map(|(s, t)| (s.line, s.character, t.line, t.character))
        });
        changes.insert(uri, Value::Array(list));
    }
    json!({ "changes": changes })
}

#[cfg(test)]
mod tests {
    use super::{config_edits, merge, resolve_by_path, respecify};
    use serde_json::json;

    #[test]
    fn resolution_by_path() {
        let known = |p: &str| ["/p/src/util.ts", "/p/src/lib/index.ts"].contains(&p);
        assert_eq!(
            resolve_by_path("/p/src/main.ts", "./util.js", &known).as_deref(),
            Some("/p/src/util.ts")
        );
        assert_eq!(
            resolve_by_path("/p/src/a/b.ts", "../util", &known).as_deref(),
            Some("/p/src/util.ts")
        );
        assert_eq!(
            resolve_by_path("/p/src/main.ts", "./lib", &known).as_deref(),
            Some("/p/src/lib/index.ts")
        );
        assert_eq!(resolve_by_path("/p/src/main.ts", "./nope", &known), None);
    }

    #[test]
    fn specifiers_keep_their_style() {
        assert_eq!(
            respecify("./util", "/p/src/main.ts", "/p/src/lib/util.ts"),
            "./lib/util"
        );
        assert_eq!(
            respecify("./util.js", "/p/src/main.ts", "/p/src/lib/util.ts"),
            "./lib/util.js"
        );
        assert_eq!(
            respecify("./util.ts", "/p/src/main.ts", "/p/util.ts"),
            "../util.ts"
        );
        assert_eq!(
            respecify("./lib", "/p/src/main.ts", "/p/src/core/index.ts"),
            "./core"
        );
        assert_eq!(
            respecify("./lib/index", "/p/src/main.ts", "/p/src/core/index.ts"),
            "./core/index"
        );
        assert_eq!(
            respecify("./other", "/p/src/lib/util.ts", "/p/src/other.ts"),
            "../other"
        );
        assert_eq!(
            respecify("./lib", "/p/src/main.ts", "/p/src/index.ts"),
            "./"
        );
    }

    #[test]
    fn tsconfig_strings() {
        let text = r#"{"files": ["./src/a.ts", "src/b.ts"], "include": ["src/lib/**/*", "src/library"], "compilerOptions": {"outDir": "src/lib/out", "lib": ["src/lib"]}}"#;
        let renames = [("/p/src/lib".to_string(), "/p/src/core".to_string())];
        let edits = config_edits("/p/tsconfig.json", text, &renames);
        let got: Vec<&str> = edits.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(got, ["\"src/core/**/*\"", "\"src/core/out\""]);
        let renames = [("/p/src/a.ts".to_string(), "/p/x/a.ts".to_string())];
        let edits = config_edits("/p/tsconfig.json", text, &renames);
        assert_eq!(edits[0].text, "\"x/a.ts\"");
        // A config inside the moved folder: strings are rebased onto its new directory.
        let text = r#"{"files": ["a.ts"], "include": ["../shared", "."], "compilerOptions": {"rootDir": ".."}}"#;
        let renames = [("/p/src".to_string(), "/p/deep/src".to_string())];
        let edits = config_edits("/p/src/tsconfig.json", text, &renames);
        let got: Vec<&str> = edits.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(got, ["\"../../shared\"", "\"../..\""]);
        // Malformed config: no edits, no panic.
        assert!(config_edits("/p/tsconfig.json", "{\"files\": [", &renames).is_empty());
    }

    #[test]
    fn merging_drops_repeats_and_overlaps() {
        let r = |l: u32, a: u32, b: u32| json!({"start": {"line": l, "character": a}, "end": {"line": l, "character": b}});
        let a = json!({"changes": {"file:///a.ts": [{"range": r(0, 5, 9), "newText": "x"}]}});
        let b = json!({"changes": {
            "file:///a.ts": [{"range": r(0, 5, 9), "newText": "x"}, {"range": r(0, 6, 7), "newText": "y"}, {"range": r(1, 0, 1), "newText": "z"}],
            "file:///b.ts": [{"range": r(0, 0, 1), "newText": "w"}],
        }});
        let m = merge([a, b]);
        assert_eq!(m["changes"]["file:///a.ts"].as_array().unwrap().len(), 2);
        assert_eq!(m["changes"]["file:///b.ts"].as_array().unwrap().len(), 1);
    }
}
