//! `import/no-unused-modules`: modules without exports (`missingExports`) and exports no
//! other module imports (`unusedExports`).
//!
//! - `missingExports`: the file's top-level `export default`, `export { names }` and
//!   exported declarations (functions, classes, variables with their pattern names, TS
//!   interfaces, aliases and enums) are counted; `export * from` is not. With none, "No
//!   exports found" goes on the first statement (the program when there is none).
//! - `unusedExports`: the files named by `src` (default the cwd: globs, directories walked
//!   for `getFileExtensions`, files; `node_modules` left out) are read through the module
//!   host. Each one's export map gives its exported names (`default` as
//!   `ImportDefaultSpecifier`; an ignored file, from `ignoreExports`, lists only its
//!   re-exports), its imports (imported names, `ImportDefaultSpecifier`,
//!   `ImportNamespaceSpecifier`, a namespace for `import()`) and its re-exports (an import
//!   of the re-exported local name from the source). An export is used when some file
//!   imports it; every export counts as used when a file imports the module as a namespace,
//!   and every one but `default` when a file has `export * from` it. Each named export
//!   specifier, exported declaration name and `export default` of the linted file is
//!   checked against that table; files outside `src`, ignored files and package entry
//!   points (`main`, `bin`, `browser` of a non-private `package.json`) are not.
//!
//! The table is built from the files as they are on disk, once per module host (a project
//! run; an editor lint has a fresh host). Upstream also patches it with the text of every
//! file it lints, so in one ESLint process a file linted earlier with different contents
//! changes later results; we do not keep state between lints.
//!
//! Written from the plugin's docs, its tests and `src/rules/no-unused-modules.js`
//! (eslint-plugin-import 2.32.0), read for the table and the special keys above.

use super::import_util::walk;
use super::prelude::*;
use crate::modules::ModCtx;
use crate::modules::minimatch::{MatchOptions, glob_match};
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;
use ts_r_ast::Ast;
use ts_r_config::path;
use ts_r_config::{EntryKind, FileSystem};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-unused-modules",
        "Forbid modules without exports, or exports without matching import in another module.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<NoUnusedModules>,
};

const EXPORT_ALL: &str = "ExportAllDeclaration";
const NAMESPACE: &str = "ImportNamespaceSpecifier";
const DEFAULT: &str = "ImportDefaultSpecifier";

#[derive(Debug)]
struct NoUnusedModules {
    src: Option<Vec<String>>,
    ignore_exports: Vec<String>,
    missing_exports: bool,
    unused_exports: bool,
    ignore_type_exports: bool,
}

impl FromOptions for NoUnusedModules {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let list = |k: &str| {
            o.get(k).and_then(Value::as_array).map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
            })
        };
        Ok(NoUnusedModules {
            src: list("src"),
            ignore_exports: list("ignoreExports").unwrap_or_default(),
            missing_exports: bool_field(o, "missingExports", false),
            unused_exports: bool_field(o, "unusedExports", false),
            ignore_type_exports: bool_field(o, "ignoreUnusedTypeExports", false),
        })
    }
}

/// The project table: which files are analysed, which are ignored, and for each file
/// which of its exports some module uses.
#[derive(Debug, Default)]
struct Table {
    src: FxHashSet<String>,
    ignored: FxHashSet<String>,
    used: FxHashMap<String, FxHashMap<String, bool>>,
}

fn is_node_module(p: &str) -> bool {
    p.contains("/node_modules/")
}

fn has_magic(p: &str) -> bool {
    p.contains(['*', '?', '[', '{'])
}

/// The deepest directory walk for `src` patterns.
const MAX_WALK_DEPTH: usize = 64;

/// Files under `dir` (not in `node_modules` or dot-directories, not dotfiles) passing
/// `keep`, with a visited set against symlink cycles.
fn walk_dir(fs: &dyn FileSystem, dir: &str, keep: &dyn Fn(&str) -> bool, out: &mut Vec<String>) {
    let mut seen = FxHashSet::default();
    let mut stack = vec![(dir.to_owned(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        if depth > MAX_WALK_DEPTH || !seen.insert(fs.realpath(&d)) {
            continue;
        }
        for e in fs.read_dir(&d) {
            if e.name.starts_with('.') {
                continue;
            }
            let p = path::combine(&d, &e.name);
            match e.kind {
                EntryKind::Directory => {
                    if e.name != "node_modules" {
                        stack.push((p, depth + 1));
                    }
                }
                EntryKind::File => {
                    if keep(&p) {
                        out.push(p);
                    }
                }
            }
        }
    }
}

/// ESLint's file enumeration for one pattern: a glob, a directory (files with one of
/// `extensions`) or a file.
fn list_files(m: &ModCtx, patterns: &[String], extensions: &[String]) -> Vec<String> {
    let fs = m.fs();
    let mut out = Vec::new();
    for pattern in patterns {
        let abs = path::resolve(&m.cwd, pattern);
        if has_magic(&abs) {
            let segs: Vec<&str> = abs.split('/').collect();
            let fixed = segs.iter().take_while(|s| !has_magic(s)).count();
            let base = segs[..fixed].join("/");
            let base = if base.is_empty() {
                "/".to_owned()
            } else {
                base
            };
            let opts = MatchOptions::default();
            walk_dir(fs, &base, &|p| glob_match(p, &abs, opts), &mut out);
        } else if fs.file_exists(&abs) {
            out.push(abs);
        } else if fs.directory_exists(&abs) {
            let has_ext = |p: &str| {
                let ext = crate::modules::extname(p);
                extensions.iter().any(|e| e == ext)
            };
            walk_dir(fs, &abs, &has_ext, &mut out);
        }
    }
    out
}

/// Imported keys by target file, in first-seen order.
type Targets = Vec<(Arc<str>, Vec<String>)>;

fn add_import(list: &mut Targets, target: Arc<str>, key: Option<String>) {
    let at = match list.iter().position(|(p, _)| *p == target) {
        Some(i) => i,
        None => {
            list.push((target, Vec::new()));
            list.len() - 1
        }
    };
    if let Some(k) = key {
        list[at].1.push(k);
    }
}

fn build_table(m: &ModCtx, src: &[String], ignore: &[String]) -> Table {
    let extensions = m.settings.file_extensions();
    let src_files: Vec<String> = list_files(m, src, &extensions)
        .into_iter()
        .filter(|p| !is_node_module(p))
        .collect();
    let mut t = Table {
        src: src_files.iter().cloned().collect(),
        ignored: list_files(m, ignore, &extensions).into_iter().collect(),
        used: FxHashMap::default(),
    };
    // importer -> (target -> imported keys); exporter -> `export *` targets.
    let mut imports: Vec<(String, Targets)> = Vec::new();
    let mut export_all: Vec<(String, Vec<Arc<str>>)> = Vec::new();
    let mut files: Vec<&String> = t.src.iter().collect();
    files.sort();
    for file in files {
        let mut exports: FxHashMap<String, bool> = FxHashMap::default();
        if let Some(map) = m.map_for(file) {
            export_all.push((file.clone(), map.dependencies.clone()));
            let mut mine: Targets = Vec::new();
            for (name, re) in &map.reexports {
                let key = if name == "default" { DEFAULT } else { name };
                exports.insert(key.to_owned(), false);
                if let Some(target) = &re.path {
                    let local = if re.local == "default" {
                        DEFAULT.to_owned()
                    } else {
                        re.local.clone()
                    };
                    add_import(&mut mine, target.clone(), Some(local));
                }
            }
            for (target, infos) in &map.imports {
                if is_node_module(target) {
                    continue;
                }
                add_import(&mut mine, target.clone(), None);
                for info in infos {
                    for s in &info.specifiers {
                        add_import(&mut mine, target.clone(), Some(s.clone()));
                    }
                }
            }
            imports.push((file.clone(), mine));
            if !t.ignored.contains(file) {
                for name in map.namespace_names() {
                    let key = if name == "default" { DEFAULT } else { name };
                    exports.insert(key.to_owned(), false);
                }
            }
        }
        exports.insert(EXPORT_ALL.to_owned(), false);
        exports.insert(NAMESPACE.to_owned(), false);
        t.used.insert(file.clone(), exports);
    }
    for (_, targets) in &export_all {
        for target in targets {
            if let Some(e) = t.used.get_mut(&**target)
                && let Some(u) = e.get_mut(EXPORT_ALL)
            {
                *u = true;
            }
        }
    }
    for (_, targets) in &imports {
        for (target, keys) in targets {
            if let Some(e) = t.used.get_mut(&**target) {
                for k in keys {
                    if let Some(u) = e.get_mut(k) {
                        *u = true;
                    }
                }
            }
        }
    }
    t
}

/// The file is the `main`, a `bin` or a `browser` entry of the nearest non-private
/// `package.json`.
fn file_is_in_pkg(m: &ModCtx, file: &str) -> bool {
    let host = m.host();
    let Some(dir) = host.package_dir(path::directory_of(file)) else {
        return false;
    };
    let Some(pkg) = host.package_json(&dir) else {
        return false;
    };
    if pkg.get("private") == Some(&Value::Bool(true)) {
        return false;
    }
    let is = |v: &str| path::resolve(&dir, v) == file;
    let field = |v: Option<&Value>| match v {
        Some(Value::String(s)) => is(s),
        Some(Value::Object(o)) => o.values().any(|v| v.as_str().is_some_and(is)),
        _ => false,
    };
    field(pkg.get("bin"))
        || field(pkg.get("browser"))
        || pkg.get("main").and_then(Value::as_str).is_some_and(is)
}

/// Names an exported declaration declares, with whether it is a type (interface, alias,
/// enum).
fn declared_names(ast: &Ast, decl: NodeId, out: &mut Vec<(String, bool)>) {
    match *ast.node(decl) {
        Node::FunctionDecl {
            name: Some(n),
            body: Some(_),
            ..
        }
        | Node::ClassDecl { name: Some(n), .. } => {
            if let Some(s) = ident_name(ast, n) {
                out.push((s.to_owned(), false));
            }
        }
        Node::Interface { name, .. } | Node::TypeAlias { name, .. } | Node::Enum { name, .. } => {
            if let Some(s) = ident_name(ast, name) {
                out.push((s.to_owned(), true));
            }
        }
        Node::VarDecl { decls, .. } => {
            for &d in ast.list(decls) {
                let Node::VarDeclarator { name, .. } = *ast.node(d) else {
                    continue;
                };
                match *ast.node(name) {
                    Node::ObjectPattern { .. } => {
                        let mut ids = Vec::new();
                        binding_idents(ast, name, &mut ids);
                        for id in ids {
                            if let Some(s) = ident_name(ast, id) {
                                out.push((s.to_owned(), false));
                            }
                        }
                    }
                    Node::ArrayPattern { elements } => {
                        for &e in ast.list(elements) {
                            let n = ident_name(ast, e).unwrap_or("undefined");
                            out.push((n.to_owned(), false));
                        }
                    }
                    _ => {
                        if let Some(s) = ident_name(ast, name) {
                            out.push((s.to_owned(), false));
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// An exported name as ESTree spells it: an identifier's name or a string's value.
fn export_name(ast: &Ast, id: NodeId) -> String {
    crate::modules::export_map::module_name(ast, id)
        .unwrap_or("undefined")
        .to_owned()
}

enum Export {
    Default(NodeId),
    Specifier(NodeId, String),
    Declared(NodeId, String, bool),
}

/// The exports in `stmt` (and whether it is an export at all).
fn exports_of(ast: &Ast, stmt: NodeId, out: &mut Vec<Export>) {
    let node = ast.node(stmt);
    match *node {
        Node::ExportDefaultExpr { .. } => out.push(Export::Default(stmt)),
        Node::ExportNamed { specifiers, .. } => {
            for &s in ast.list(specifiers) {
                if let Node::ExportSpecifier {
                    local, exported, ..
                } = *ast.node(s)
                {
                    out.push(Export::Specifier(
                        s,
                        export_name(ast, exported.unwrap_or(local)),
                    ));
                }
            }
        }
        _ => {
            let mods = node.modifiers();
            if !mods.contains(Modifiers::EXPORT) {
                return;
            }
            if mods.contains(Modifiers::DEFAULT) {
                out.push(Export::Default(stmt));
                return;
            }
            let mut names = Vec::new();
            declared_names(ast, stmt, &mut names);
            for (n, ty) in names {
                out.push(Export::Declared(stmt, n, ty));
            }
        }
    }
}

impl NoUnusedModules {
    fn table(&self, m: &ModCtx) -> Arc<Table> {
        let src = self.src.clone().unwrap_or_else(|| vec![m.cwd.clone()]);
        let mut src_key = src.clone();
        src_key.sort();
        let mut ignore_key = self.ignore_exports.clone();
        ignore_key.sort();
        let key = format!("no-unused-modules\0{src_key:?}\0{ignore_key:?}");
        m.memo(&key, || build_table(m, &src, &self.ignore_exports))
    }
}

impl Rule for NoUnusedModules {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return;
        };
        let m = cx.file.modules();
        let file = m.file.clone();
        let table = (self.unused_exports || !self.ignore_exports.is_empty()).then(|| self.table(m));
        let ignored = table.as_ref().is_some_and(|t| t.ignored.contains(&file));

        if self.unused_exports
            && let Some(t) = &table
            && !ignored
            && !file_is_in_pkg(m, &file)
            && t.src.contains(&file)
        {
            let mut found = Vec::new();
            walk(ast, |id| exports_of(ast, id, &mut found));
            let used = t.used.get(&file);
            let flag = |k: &str| used.and_then(|u| u.get(k)).copied();
            for e in found {
                let (node, value, is_type) = match e {
                    Export::Default(n) => (n, DEFAULT.to_owned(), false),
                    Export::Specifier(n, v) | Export::Declared(n, v, false) => (n, v, false),
                    Export::Declared(n, v, true) => (n, v, true),
                };
                if is_type && self.ignore_type_exports {
                    continue;
                }
                if value != DEFAULT && flag(EXPORT_ALL) == Some(true) {
                    continue;
                }
                if flag(NAMESPACE) == Some(true) {
                    continue;
                }
                let key = if value == "default" { DEFAULT } else { &value };
                if flag(key) != Some(true) {
                    let shown = if key == DEFAULT { "default" } else { key };
                    cx.report_node(
                        node,
                        format!("exported declaration '{shown}' not used within other modules"),
                    );
                }
            }
        }

        if self.missing_exports && !ignored {
            let mut found = Vec::new();
            for &s in ast.list(stmts) {
                exports_of(ast, s, &mut found);
            }
            if found.is_empty() {
                let at = ast.list(stmts).first().copied().unwrap_or(ast.root);
                cx.report_node(at, "No exports found");
            }
        }
    }
}
