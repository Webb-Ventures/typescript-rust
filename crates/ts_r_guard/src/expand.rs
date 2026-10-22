//! `parse<T>` call sites: rewrites the `@ts-r.dev/guard` calls of one TypeScript file into calls
//! of generated validators (record 0007, "`parse<T>` runtime validators").
//!
//! The rewrite is source to source, before the TypeScript transform, so `ts-r-run`,
//! `ts-r-build` and `ts-r-dev` share it and print the same code:
//!
//! - every value import from `@ts-r.dev/guard` is removed (newlines kept);
//! - `parse<T>` (and `safeParse`, `is`, `assert`, `parseJson`, `fetchJson`, `guard`, `env`,
//!   `body`, `query`, `args`, `stored`, `readJson`), called or as an instantiation expression,
//!   becomes
//!   `$f$<form>$<entry>`, a small arrow over the type's validator;
//! - `expect`, `panic`, `ParseError`, `ExpectError`, `PanicError` and namespace imports become
//!   constants bound to the runtime's;
//! - the generated code goes on one line after the shebang and directive prologue, so line
//!   numbers (and stack traces through source maps) don't move.
//!
//! [`Layout::Inline`] puts the runtime and the validators in the file (`ts-r-run`, `ts-r-dev`,
//! unbundled output). [`Layout::Shared`] imports them from virtual modules (one per
//! declaration, plus the runtime) that [`crate::vfs::GuardFs`] serves to the bundler, so each
//! validator exists once per bundle and tree-shakes with its call sites.
//!
//! Types come from declarations only (the evaluator in `eval.rs`); this module never asks the
//! checker for a type (record 0007, invariant 6).

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{Ast, Node, NodeId};
use ts_r_checker::{ProgramFiles, SourceCache, load_program_cached};
use ts_r_config::fs::DirEntry;
use ts_r_config::{CompilerOptions, FileStamp, FileSystem, load_tsconfig, path};

use crate::emit::{Emitter, js_str};
use crate::eval::G;
use crate::rt::{runtime_for, used_names};
use crate::{Generator, GuardOptions, RUNTIME, parse_expr};

/// The package whose imports are expanded.
pub const PACKAGE: &str = "@ts-r.dev/guard";

/// `@ts-r.dev/guard` functions that take a type argument, and the runtime forms they become.
const TYPED: &[&str] = &[
    "parse",
    "safeParse",
    "is",
    "assert",
    "parseJson",
    "fetchJson",
    "guard",
    "env",
    "body",
    "query",
    "args",
    "stored",
    "readJson",
];

/// Values bound to runtime expressions (the error classes are getters).
const VALUES: &[(&str, &str)] = &[
    ("ParseError", "$ParseError()"),
    ("ExpectError", "$ExpectError()"),
    ("PanicError", "$PanicError()"),
    ("expect", "$expect"),
    ("panic", "$panic"),
];

/// Cheap pre-check: only files mentioning the package are expanded.
pub fn mentions_guard(src: &str) -> bool {
    // Every file read is checked: jump between `@`s (a memchr scan), which is several times
    // faster than a general substring search.
    let mut rest = src;
    while let Some(i) = rest.find('@') {
        rest = &rest[i..];
        if rest.starts_with(PACKAGE) {
            return true;
        }
        rest = &rest[1..];
    }
    false
}

/// Whether `path` is a TypeScript source the transform expands (not a declaration file).
pub fn is_expandable_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let ts = [".ts", ".tsx", ".mts", ".cts"]
        .iter()
        .any(|e| lower.ends_with(e));
    ts && ![".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .any(|e| lower.ends_with(e))
}

/// Where the generated code goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Runtime and validators in the file itself.
    Inline,
    /// Imports from the virtual modules under `vroot` (see [`crate::vfs`]): `<vroot>/rt.js`
    /// and one module per declaration. `project_root` is the directory `vroot` mirrors.
    Shared { project_root: String, vroot: String },
}

#[derive(Clone, Debug)]
pub struct ExpandOptions {
    pub guard: GuardOptions,
    pub layout: Layout,
}

/// A problem with a call site: TSR1501 (type not checkable), TSR1402 (no type argument),
/// TSR1503 (used as a value) or TSR1504 (validator over budget). Offsets are UTF-8 bytes
/// into the original source; `line` and `column` are 1-based.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuardError {
    pub code: u32,
    pub start: u32,
    pub end: u32,
    pub line: u32,
    pub column: u32,
    pub message: String,
}

impl GuardError {
    /// `TSR1501`.
    pub fn code_label(&self) -> String {
        format!("TSR{:04}", self.code)
    }

    /// `path(line,col): error TSR1501: message` (tsc's plain format).
    pub fn render(&self, path: &str) -> String {
        format!(
            "{path}({},{}): error {}: {}",
            self.line,
            self.column,
            self.code_label(),
            self.message
        )
    }
}

/// The rewritten source of one file.
#[derive(Clone, Debug, Default)]
pub struct Expanded {
    /// TypeScript source with the call sites rewritten and the generated code on its first
    /// line (after a shebang and directives).
    pub code: String,
    /// Files the validators were generated from (every other source file of the program the
    /// file was evaluated in), so caches and watchers can depend on them.
    pub deps: Vec<String>,
    pub errors: Vec<GuardError>,
    /// Validated types, in first-use order (for reports).
    pub types: Vec<String>,
}

/// One validated type of the file.
struct EntryInfo {
    /// `e0`, ...
    id: String,
    /// As written (whitespace collapsed).
    type_name: String,
    /// Predicate and descriptor function names: `$is_User` / `$d_User` for a plain
    /// reference to a non-generic declaration, else `$is$e0` / `$d$e0` (declarations are
    /// `$is_<Name>`, so the two can't clash).
    is_fn: String,
    d_fn: String,
    g: G,
    plain: bool,
    /// The first type argument that names it (where TSR1504 goes).
    at: (u32, u32),
}

struct Site {
    start: u32,
    end: u32,
    entry: usize,
    form: &'static str,
}

fn ident(ast: &Ast, n: NodeId) -> Option<&str> {
    match *ast.node(n) {
        Node::Ident { name } => Some(ast.atom(name)),
        Node::StringLit { value } => Some(ast.atom(value)),
        _ => None,
    }
}

fn line_col(text: &str, at: u32) -> (u32, u32) {
    let at = (at as usize).min(text.len());
    let before = &text[..at];
    let line = before.bytes().filter(|&b| b == b'\n').count() as u32 + 1;
    let col_start = before.rfind('\n').map_or(0, |i| i + 1);
    let column = text[col_start..at].chars().count() as u32 + 1;
    (line, column)
}

/// JavaScript on one line: full-line `//` comments dropped, lines joined with spaces. Only
/// for the runtime and generated code, which never break a statement across lines where a
/// newline matters (no ASI, no multi-line strings or templates).
pub(crate) fn one_line(js: &str) -> String {
    let mut out = String::with_capacity(js.len());
    for l in js.lines() {
        let t = l.trim();
        if t.is_empty() || t.starts_with("//") {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(t);
    }
    out
}

/// The shared runtime module (`<vroot>/rt.js`): the runtime plus its exports.
pub fn runtime_module(opts: &GuardOptions) -> String {
    let mut s = String::from(RUNTIME);
    let _ = writeln!(s, "const $MAX_DEPTH = {};", opts.max_depth);
    let _ = writeln!(s, "export {{ {} }};", crate::rt::names().join(", "));
    s
}

/// `import { <the runtime names `code` uses> } from "<vroot>/rt.js";` (empty when none).
fn runtime_import(code: &str, vroot: &str) -> String {
    let names = used_names(code);
    if names.is_empty() {
        return String::new();
    }
    format!(
        "import {{ {} }} from {};\n",
        names.join(", "),
        js_str(&format!("{vroot}/rt.js"))
    )
}

/// Every declaration id `g` references directly, in first-reference order.
fn refs(g: &G, out: &mut Vec<usize>) {
    match g {
        G::Ref(id, args) => {
            if !out.contains(id) {
                out.push(*id);
            }
            for a in args {
                refs(a, out);
            }
        }
        G::Obj(o) => {
            for p in &o.props {
                refs(&p.ty, out);
            }
            if let Some(i) = &o.index {
                refs(i, out);
            }
        }
        G::Arr(e) | G::Set(e) | G::Refine(e, _) => refs(e, out),
        G::Map(k, v) => {
            refs(k, out);
            refs(v, out);
        }
        G::Tuple(t) => {
            for e in &t.elems {
                refs(e, out);
            }
            if let Some(r) = &t.rest {
                refs(r, out);
            }
        }
        G::Union(ms) => {
            for m in ms {
                refs(m, out);
            }
        }
        _ => {}
    }
}

/// The user classes (`G::Class`) and refinements (`G::Refine`) `g` uses itself (not through
/// declarations, whose bodies import their own), in first-use order.
fn ext_refs(g: &G, classes: &mut Vec<usize>, refines: &mut Vec<usize>) {
    match g {
        G::Class(i) => {
            if !classes.contains(i) {
                classes.push(*i);
            }
        }
        G::Refine(b, r) => {
            if !refines.contains(r) {
                refines.push(*r);
            }
            ext_refs(b, classes, refines);
        }
        G::Ref(_, args) => {
            for a in args {
                ext_refs(a, classes, refines);
            }
        }
        G::Obj(o) => {
            for p in &o.props {
                ext_refs(&p.ty, classes, refines);
            }
            if let Some(i) = &o.index {
                ext_refs(i, classes, refines);
            }
        }
        G::Arr(e) | G::Set(e) => ext_refs(e, classes, refines),
        G::Map(k, v) => {
            ext_refs(k, classes, refines);
            ext_refs(v, classes, refines);
        }
        G::Tuple(t) => {
            for e in &t.elems {
                ext_refs(e, classes, refines);
            }
            if let Some(r) = &t.rest {
                ext_refs(r, classes, refines);
            }
        }
        G::Union(ms) => {
            for m in ms {
                ext_refs(m, classes, refines);
            }
        }
        _ => {}
    }
}

/// `import { <export> as <binding> } from "<spec>";` for the user classes and refinement
/// predicates the code for `gs` uses. Specifiers are written relative to `from_dir` (inline
/// output, as the user's own imports are), or as absolute paths when `from_dir` is `None`
/// (shared modules under the virtual root); packages keep their bare name. A class declared
/// in `local_file` uses its own binding there and is not imported.
pub(crate) fn ext_imports(
    generator: &Generator<'_>,
    gs: &[&G],
    from_dir: Option<&str>,
    local_file: Option<usize>,
) -> String {
    let mut classes = Vec::new();
    let mut refines = Vec::new();
    for g in gs {
        ext_refs(g, &mut classes, &mut refines);
    }
    let p = generator.eval.program();
    let mut out = String::new();
    for i in classes {
        let c = &generator.eval.classes[i];
        if Some(c.file) == local_file && c.module.is_none() && c.name != "default" {
            continue;
        }
        let spec = class_spec(p, c, from_dir);
        let _ = writeln!(
            out,
            "import {{ {} as {} }} from {};",
            c.export,
            c.binding,
            js_str(&spec)
        );
    }
    for r in refines {
        let Some(rf) = generator.opts.refinements.get(r) else {
            continue;
        };
        let spec = match from_dir {
            Some(d) if rf.is_path() => path::relative_dotted(d, &rf.module, true),
            _ => rf.module.clone(),
        };
        let _ = writeln!(
            out,
            "import {{ {} as {} }} from {};",
            rf.export,
            crate::refine_binding(&rf.name),
            js_str(&spec)
        );
    }
    out
}

/// The module specifier the generated code imports class `c` from. A class in a
/// `declare module "x"` block comes from `x`. Otherwise the specifier is one the program
/// already uses for the class's file: a package name as is, else the user's relative
/// specifier re-based onto `from_dir` (inline output: the extension convention stays the
/// user's), or for shared modules (`from_dir` `None`) the absolute path of the file itself
/// (of the specifier, for a declaration file, whose module is the JavaScript next to it).
fn class_spec(p: &ProgramFiles, c: &crate::eval::ClassRef, from_dir: Option<&str>) -> String {
    if let Some(m) = &c.module {
        return m.clone();
    }
    let target = &p.files[c.file].path;
    let lower = target.to_ascii_lowercase();
    let dts = [".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .any(|e| lower.ends_with(e));
    // Every (importing file, specifier) that resolved to the class's file, in a stable order:
    // package names first.
    let mut uses: Vec<(bool, &str, &str)> = p
        .resolutions
        .iter()
        .filter_map(|((from, spec), r)| {
            let r = r.as_ref()?;
            (r.resolved == *target).then_some((
                path::is_explicitly_relative(spec) || path::is_rooted(spec),
                from.as_str(),
                spec.as_str(),
            ))
        })
        .collect();
    uses.sort_unstable();
    let as_written = uses.first().map(|&(relative, from, spec)| {
        if relative {
            (
                true,
                path::normalize(&path::resolve(path::directory_of(from), spec)),
            )
        } else {
            (false, spec.to_owned())
        }
    });
    let (relative, abs) = match as_written {
        Some((false, bare)) => return bare,
        Some((true, abs)) if from_dir.is_some() || dts => (true, abs),
        _ => (true, target.clone()),
    };
    match from_dir {
        Some(d) if relative => path::relative_dotted(d, &abs, true),
        _ => abs,
    }
}

/// Encodes a path segment of a virtual module path (`/` and `%` escaped).
fn enc(s: &str) -> String {
    s.replace('%', "%25").replace('/', "%2F")
}

pub(crate) fn dec(s: &str) -> String {
    s.replace("%2F", "/").replace("%25", "%")
}

/// The virtual module path of declaration `name` (in ambient block `module`) of `file`.
pub fn decl_module_path(
    project_root: &str,
    vroot: &str,
    file: &str,
    module: Option<&str>,
    name: &str,
) -> String {
    let root = project_root.trim_end_matches('/');
    let rel = match file.strip_prefix(root).and_then(|r| r.strip_prefix('/')) {
        Some(r) => r.to_owned(),
        None => format!("~abs/{}", file.trim_start_matches('/')),
    };
    match module {
        Some(m) => format!("{vroot}/{rel}/~mod~{}/{name}.js", enc(m)),
        None => format!("{vroot}/{rel}/{name}.js"),
    }
}

/// Function names for the declarations a module references: `$is_<name>` with `_2`, `_3`
/// on clashes, in reference order, so a module's text depends only on what it validates.
fn local_names(generator: &Generator<'_>, ids: &[usize]) -> Vec<String> {
    let decls = &generator.eval.decls;
    let mut names: Vec<String> = decls.iter().map(|d| d.name.clone()).collect();
    let mut used: FxHashSet<String> = FxHashSet::default();
    for &id in ids {
        let base = base_name(generator, id);
        let mut n = base.clone();
        let mut k = 2;
        while used.contains(&n) {
            n = format!("{base}_{k}");
            k += 1;
        }
        used.insert(n.clone());
        names[id] = n;
    }
    names
}

/// The declared name of declaration `id` (without the evaluator's clash suffix).
fn base_name(generator: &Generator<'_>, id: usize) -> String {
    let d = &generator.eval.decls[id];
    let ast = &generator.eval.program().files[d.file].ast;
    match *ast.node(d.node) {
        Node::TypeAlias { name, .. } | Node::Interface { name, .. } => {
            ident(ast, name).unwrap_or("T").to_owned()
        }
        _ => d.name.clone(),
    }
}

impl Generator<'_> {
    /// The virtual module path of declaration `id` under [`Layout::Shared`].
    fn decl_path(&self, id: usize, project_root: &str, vroot: &str) -> String {
        let d = &self.eval.decls[id];
        let file = &self.eval.program().files[d.file].path;
        decl_module_path(
            project_root,
            vroot,
            file,
            d.module.as_deref(),
            &base_name(self, id),
        )
    }

    /// The module of declaration `name` of `file` (in its ambient block `module`): its
    /// predicate and descriptor as `$is` / `$d`, importing the runtime and every other
    /// declaration it references from their own modules. Deterministic: the text depends
    /// only on the declarations.
    pub fn decl_module(
        &mut self,
        file: usize,
        module: Option<&str>,
        name: &str,
        project_root: &str,
        vroot: &str,
    ) -> Result<String, String> {
        let mark = self.eval.mark();
        let id = match self.eval.decl_named(file, module, name) {
            Ok(id) => id,
            Err(e) => {
                self.eval.rollback(mark);
                return Err(e);
            }
        };
        let mut ids = vec![id];
        if let Some(b) = &self.eval.decls[id].body {
            refs(b, &mut ids);
        }
        let names = local_names(self, &ids);
        let mut imports = String::new();
        for &r in ids.iter().skip(1) {
            if r == id {
                continue;
            }
            let _ = writeln!(
                imports,
                "import {{ $is as $is_{n}, $d as $d_{n} }} from {};",
                js_str(&self.decl_path(r, project_root, vroot)),
                n = names[r]
            );
        }
        if let Some(b) = &self.eval.decls[id].body {
            imports.push_str(&ext_imports(self, &[b], None, None));
        }
        let mut body = String::new();
        let (classes, refines) = self.bindings(None);
        let mut em =
            Emitter::with_names(&self.eval.decls, names.clone()).with_bindings(classes, refines);
        em.decl_function(id, &mut body, self.opts.max_depth);
        let _ = writeln!(
            body,
            "export {{ $is_{n} as $is, $d_{n} as $d }};",
            n = names[id]
        );
        let mut out = runtime_import(&body, vroot);
        out.push_str(&imports);
        out.push_str(&body);
        Ok(out)
    }
}

/// What a file imports from `@ts-r.dev/guard`.
#[derive(Default)]
struct Imports {
    /// local name -> exported name.
    named: FxHashMap<String, String>,
    namespaces: Vec<String>,
    /// Spans of the import declarations to remove.
    remove: Vec<(u32, u32)>,
    errors: Vec<(u32, u32, u32, String)>,
}

fn guard_imports(ast: &Ast) -> Imports {
    let mut im = Imports::default();
    let Node::Program { stmts } = *ast.node(ast.root) else {
        return im;
    };
    for &st in ast.list(stmts) {
        let Node::ImportDecl {
            type_only,
            specifiers,
            source,
            ..
        } = *ast.node(st)
        else {
            continue;
        };
        if type_only || ident(ast, source) != Some(PACKAGE) {
            continue;
        }
        let sp = ast.span(st);
        im.remove.push((sp.start, sp.end));
        for &s in ast.list(specifiers) {
            match *ast.node(s) {
                Node::ImportSpecifier {
                    type_only: true, ..
                } => {}
                Node::ImportSpecifier {
                    imported, local, ..
                } => {
                    let l = ident(ast, local).unwrap_or("").to_owned();
                    let e = imported
                        .and_then(|i| ident(ast, i))
                        .map_or_else(|| l.clone(), str::to_owned);
                    im.named.insert(l, e);
                }
                Node::ImportNamespaceSpecifier { local } => {
                    if let Some(l) = ident(ast, local) {
                        im.namespaces.push(l.to_owned());
                    }
                }
                Node::ImportDefaultSpecifier { local } => {
                    let sp = ast.span(local);
                    im.errors.push((
                        1503,
                        sp.start,
                        sp.end,
                        format!("'{PACKAGE}' has no default export; import its functions by name."),
                    ));
                }
                _ => {}
            }
        }
    }
    im
}

/// Whether the file's `@ts-r.dev/guard` imports include more than runtime values (a typed
/// function or a namespace), so its call sites need the declarations it imports.
fn needs_types(ast: &Ast) -> bool {
    let im = guard_imports(ast);
    !im.namespaces.is_empty()
        || im
            .named
            .values()
            .any(|e| !VALUES.iter().any(|(v, _)| v == e))
}

/// Names declared by bindings in the file (variables, parameters, functions, classes), to
/// avoid reporting a shadowing local as a misused guard function.
fn bound_names(
    ast: &Ast,
    parents: &ts_r_ast::visit::ParentMap,
    ids: &[NodeId],
) -> FxHashSet<String> {
    let mut out = FxHashSet::default();
    for &n in ids {
        let Some(name) = (match *ast.node(n) {
            Node::Ident { name } => Some(ast.atom(name)),
            _ => None,
        }) else {
            continue;
        };
        let Some(p) = parents.parent(n) else {
            continue;
        };
        let binds = match *ast.node(p) {
            Node::VarDeclarator { name, .. } => name == n,
            Node::Param { pattern, .. } => pattern == n,
            Node::FunctionDecl { name, .. }
            | Node::FunctionExpr { name, .. }
            | Node::ClassDecl { name, .. } => name == Some(n),
            Node::ShorthandProp { .. } => {
                // In a destructuring pattern (`const { is } = x`).
                parents
                    .parent(p)
                    .is_some_and(|pp| matches!(*ast.node(pp), Node::ObjectPattern { .. }))
            }
            _ => false,
        };
        if binds {
            out.insert(name.to_owned());
        }
    }
    out
}

/// Expands file `file` of `program`. `None` when it imports nothing from `@ts-r.dev/guard`.
pub fn expand_program(
    program: &ProgramFiles,
    file: usize,
    opts: &ExpandOptions,
) -> Option<Expanded> {
    let sf = program.files.get(file)?;
    let ast = &sf.ast;
    let text = sf.text.as_str();
    let im = guard_imports(ast);
    if im.remove.is_empty() {
        return None;
    }
    let parents = sf.parents();
    let mut all: Vec<NodeId> = Vec::new();
    struct Collect<'a>(&'a mut Vec<NodeId>);
    impl ts_r_ast::visit::Visitor for Collect<'_> {
        fn enter(&mut self, _: &Ast, id: NodeId) -> ts_r_ast::visit::Walk {
            self.0.push(id);
            ts_r_ast::visit::Walk::Continue
        }
    }
    ts_r_ast::visit::walk(ast, ast.root, &mut Collect(&mut all));
    let shadowed = bound_names(ast, parents, &all);

    let mut generator = Generator::new(program, opts.guard.clone());
    let mut entries: Vec<EntryInfo> = Vec::new();
    let mut by_text: FxHashMap<String, Option<usize>> = FxHashMap::default();
    let mut sites: Vec<Site> = Vec::new();
    let mut errors: Vec<(u32, u32, u32, String)> = im.errors.clone();

    for &n in &all {
        // The guard export `n` names: `parse` (named import) or `g.parse` (namespace).
        let export = match *ast.node(n) {
            Node::Ident { name } => {
                let Some(p) = parents.parent(n) else { continue };
                match *ast.node(p) {
                    Node::ImportSpecifier { .. }
                    | Node::ImportNamespaceSpecifier { .. }
                    | Node::ImportDefaultSpecifier { .. }
                    | Node::TypeRef { .. }
                    | Node::QualifiedName { .. }
                    | Node::TypeQuery { .. }
                    | Node::ExportSpecifier { .. } => continue,
                    Node::Member { prop, obj, .. } if prop == n || obj == n => continue,
                    // Names that are not references: property keys (`{ body: x }`), labels.
                    Node::Property { key, .. }
                    | Node::PropertyDef { key, .. }
                    | Node::MethodDef { key, .. }
                    | Node::PropertySignature { key, .. }
                    | Node::MethodSignature { key, .. }
                    | Node::EnumMember { name: key, .. }
                    | Node::JsxAttr { name: key, .. }
                    | Node::Labeled { label: key, .. }
                        if key == n =>
                    {
                        continue;
                    }
                    Node::Break { label } | Node::Continue { label } if label == Some(n) => {
                        continue;
                    }
                    _ => {}
                }
                match im.named.get(ast.atom(name)) {
                    Some(e) => e.as_str(),
                    None => continue,
                }
            }
            Node::Member { obj, prop, .. } => {
                let (Some(o), Some(p)) = (ident(ast, obj), ident(ast, prop)) else {
                    continue;
                };
                if !im.namespaces.iter().any(|ns| ns == o) {
                    continue;
                }
                p
            }
            _ => continue,
        };
        let local = match *ast.node(n) {
            Node::Ident { name } => ast.atom(name),
            _ => export,
        };
        let sp = ast.span(n);
        if !TYPED.contains(&export) {
            continue;
        }
        let parent = parents.parent(n);
        let type_args = parent.and_then(|p| match *ast.node(p) {
            Node::Call {
                callee, type_args, ..
            } if callee == n => Some(type_args),
            Node::Instantiation { expr, type_args } if expr == n => Some(Some(type_args)),
            _ => None,
        });
        let Some(type_args) = type_args else {
            if !shadowed.contains(local) {
                errors.push((
                    1503,
                    sp.start,
                    sp.end,
                    format!(
                        "'{local}' from '{PACKAGE}' must be called with a type argument; it can't be used as a value."
                    ),
                ));
            }
            continue;
        };
        let Some(ta) = type_args else {
            if !shadowed.contains(local) {
                errors.push((
                    1402,
                    sp.start,
                    sp.end,
                    format!(
                        "'{local}' needs an explicit type argument in sound mode, e.g. '{local}<User>(...)'."
                    ),
                ));
            }
            continue;
        };
        let first = match *ast.node(ta) {
            Node::TypeArgs { args } => ast.list(args).first().copied(),
            _ => None,
        };
        let Some(arg) = first else { continue };
        let asp = ast.span(arg);
        // Call sites share an entry by the exact text: whitespace in a literal type counts.
        let written = text
            .get(asp.start as usize..asp.end as usize)
            .unwrap_or("")
            .to_owned();
        let entry = match by_text.get(&written) {
            Some(e) => *e,
            None => {
                let e = match generator.add(file, arg) {
                    Ok(e) => {
                        let g = generator
                            .entries
                            .last()
                            .map(|(_, g)| g.clone())
                            .unwrap_or(G::Unknown);
                        // A plain reference uses the declaration's functions; a type
                        // equal to an earlier entry's (written differently) shares them.
                        let same = entries.iter().find(|x| x.g == g);
                        let plain = same.is_some() || matches!(&g, G::Ref(_, a) if a.is_empty());
                        let (is_fn, d_fn) = match (&g, same) {
                            (_, Some(x)) => (x.is_fn.clone(), x.d_fn.clone()),
                            (G::Ref(id, a), None) if a.is_empty() => {
                                let name = &generator.eval.decls[*id].name;
                                (format!("$is_{name}"), format!("$d_{name}"))
                            }
                            _ => (format!("$is${}", e.entry), format!("$d${}", e.entry)),
                        };
                        entries.push(EntryInfo {
                            id: e.entry,
                            type_name: e.type_name,
                            is_fn,
                            d_fn,
                            g,
                            plain,
                            at: (asp.start, asp.end),
                        });
                        Some(entries.len() - 1)
                    }
                    Err(u) => {
                        errors.push((
                            1501,
                            asp.start,
                            asp.end,
                            format!(
                                "Type '{}' cannot be checked at runtime: {}.",
                                u.ty, u.reason
                            ),
                        ));
                        None
                    }
                };
                by_text.insert(written, e);
                e
            }
        };
        let Some(entry) = entry else { continue };
        let tsp = ast.span(ta);
        let form = TYPED
            .iter()
            .copied()
            .find(|f| *f == export)
            .unwrap_or("parse");
        sites.push(Site {
            start: sp.start,
            end: tsp.end.max(sp.end),
            entry,
            form,
        });
    }

    if opts.guard.max_validator_kb > 0 {
        over_budget(&generator, &entries, &opts.guard, &mut errors);
    }

    // ---- generated code ----
    let mut code = String::new();
    match &opts.layout {
        Layout::Inline => {
            let mut em = generator.emitter(Some(file));
            // `explain = "none"` never walks a descriptor; only the converting forms read one.
            em.descriptors = opts.guard.explain != crate::Explain::None
                || sites.iter().any(|s| CONVERTS.contains(&s.form));
            em.decl_functions(&mut code, opts.guard.max_depth);
            entry_functions(&mut em, &entries, &mut code);
        }
        Layout::Shared { .. } => {
            let mut em = generator.emitter(Some(file));
            entry_functions(&mut em, &entries, &mut code);
        }
    }
    // One arrow per (entry, form) used; every form but `is` goes through the entry's
    // `parse` arrow.
    let mut forms: Vec<(usize, &'static str)> = Vec::new();
    for s in &sites {
        forms.push((s.entry, s.form));
        if s.form != "is" {
            forms.push((s.entry, "parse"));
        }
    }
    forms.sort_unstable();
    forms.dedup();
    for (i, form) in &forms {
        let e = &entries[*i];
        let p = form_name("parse", &e.id);
        let body = match *form {
            "parse" => format!(
                "(v) => {}",
                parse_expr(opts.guard.explain, &e.is_fn, &e.d_fn, &e.type_name, "v")
            ),
            "safeParse" => format!("(v) => $safe({p}, v, {})", e.is_fn),
            "is" => format!("(v) => {}(v, 0)", e.is_fn),
            "assert" => format!("(v) => {{ {p}(v); }}"),
            "parseJson" => format!("(s) => {p}(JSON.parse(s))"),
            "fetchJson" => format!("(i, o) => fetch(i, o).then((r) => r.json()).then({p})"),
            "guard" => format!("() => ({{ parse: {p}, is: (v) => {}(v, 0) }})", e.is_fn),
            "env" => format!(
                "() => {p}($env(globalThis.process?.env ?? {{}}, {}()))",
                e.d_fn
            ),
            "body" => format!("(r) => {p}(r.body)"),
            "query" => format!("(r) => {p}($env($query(r), {}()))", e.d_fn),
            "args" => format!("(a) => {p}($args({}(), a))", e.d_fn),
            "stored" => format!("(k) => {p}(JSON.parse(globalThis.localStorage.getItem(k)))"),
            "readJson" => format!(
                "(f) => {p}(JSON.parse(globalThis.process.getBuiltinModule(\"node:fs\").readFileSync(f, \"utf8\")))"
            ),
            _ => continue,
        };
        let _ = writeln!(code, "const {} = {body};", form_name(form, &e.id));
    }
    // Value imports bound to the runtime.
    let mut locals: Vec<(&String, &String)> = im.named.iter().collect();
    locals.sort();
    for (local, export) in locals {
        if let Some((_, rt)) = VALUES.iter().find(|(e, _)| e == export) {
            let _ = writeln!(code, "const {local} = {rt};");
        }
    }
    for ns in &im.namespaces {
        let fields: Vec<String> = VALUES.iter().map(|(e, rt)| format!("{e}: {rt}")).collect();
        let _ = writeln!(code, "const {ns} = {{ {} }};", fields.join(", "));
    }
    let head = match &opts.layout {
        Layout::Inline => {
            // Every declaration's code is in the file: import what any of it uses.
            let gs: Vec<&G> = entries
                .iter()
                .map(|e| &e.g)
                .chain(generator.eval.decls.iter().filter_map(|d| d.body.as_ref()))
                .collect();
            let mut h = ext_imports(
                &generator,
                &gs,
                Some(path::directory_of(&sf.path)),
                Some(file),
            );
            h.push_str(&runtime_for(&code, opts.guard.max_depth));
            h
        }
        Layout::Shared {
            project_root,
            vroot,
        } => {
            let mut used = Vec::new();
            for e in &entries {
                refs(&e.g, &mut used);
            }
            let gs: Vec<&G> = entries.iter().map(|e| &e.g).collect();
            let mut h = runtime_import(&code, vroot);
            h.push_str(&ext_imports(&generator, &gs, None, Some(file)));
            for &id in &used {
                let _ = writeln!(
                    h,
                    "import {{ $is as $is_{n}, $d as $d_{n} }} from {};",
                    js_str(&generator.decl_path(id, project_root, vroot)),
                    n = generator.eval.decls[id].name
                );
            }
            h
        }
    };
    let prelude = one_line(&(head + &code));

    // ---- edits ----
    let mut edits: Vec<(u32, u32, String)> = Vec::new();
    for &(s, e) in &im.remove {
        let blank: String = text
            .get(s as usize..e as usize)
            .unwrap_or("")
            .chars()
            .filter(|&c| c == '\n')
            .collect();
        edits.push((s, e, blank));
    }
    for s in &sites {
        edits.push((s.start, s.end, form_name(s.form, &entries[s.entry].id)));
    }
    let at = insertion_point(ast, text);
    let before = &text[..at as usize];
    let sep = if at == 0 || before.ends_with(char::is_whitespace) {
        ""
    } else if before.ends_with(';') {
        " "
    } else {
        // A directive ended by ASI (`"use strict"` then a newline): the prelude follows it
        // on its line.
        "; "
    };
    edits.push((at, at, format!("{sep}{prelude} ")));
    // Insertions before replacements at the same offset; nested sites can't overlap
    // (a call's callee and type arguments contain no other call site).
    edits.sort_by_key(|(s, e, _)| (*s, *e != *s));
    let mut out = String::with_capacity(text.len() + prelude.len() + 64);
    let mut pos = 0usize;
    for (s, e, r) in edits {
        let (s, e) = (s as usize, e as usize);
        if s < pos || e > text.len() {
            continue;
        }
        out.push_str(&text[pos..s]);
        out.push_str(&r);
        pos = e;
    }
    out.push_str(&text[pos..]);

    let mut deps: Vec<String> = program
        .files
        .iter()
        .enumerate()
        .filter(|(i, f)| *i != file && !f.is_lib)
        .map(|(_, f)| f.path.clone())
        .collect();
    deps.sort();
    errors.sort_by_key(|&(code, start, end, _)| (start, end, code));
    errors.dedup();
    Some(Expanded {
        code: out,
        deps,
        errors: errors
            .into_iter()
            .map(|(code, start, end, message)| {
                let (line, column) = line_col(text, start);
                GuardError {
                    code,
                    start,
                    end,
                    line,
                    column,
                    message,
                }
            })
            .collect(),
        types: entries.iter().map(|e| e.type_name.clone()).collect(),
    })
}

/// The constant a call site of `form` for entry `id` becomes (`$f$parse$e0`).
fn form_name(form: &str, id: &str) -> String {
    format!("$f${form}${id}")
}

/// Forms whose input is strings converted by the descriptor.
const CONVERTS: &[&str] = &["env", "query", "args"];

/// TSR1504: entries whose validator (entry functions plus every declaration they reach, as
/// generated) is over `max-validator-kb`.
fn over_budget(
    generator: &Generator<'_>,
    entries: &[EntryInfo],
    opts: &GuardOptions,
    errors: &mut Vec<(u32, u32, u32, String)>,
) {
    let decls = &generator.eval.decls;
    let mut em = Emitter::new(decls);
    let mut sizes: FxHashMap<usize, usize> = FxHashMap::default();
    let budget = opts.max_validator_kb as usize * 1024;
    for e in entries {
        let mut ids = Vec::new();
        refs(&e.g, &mut ids);
        let mut i = 0;
        while i < ids.len() {
            if let Some(b) = &decls[ids[i]].body {
                refs(b, &mut ids);
            }
            i += 1;
        }
        let mut bytes = 0;
        if !e.plain {
            let mut s = String::new();
            em.entry_functions(&e.g, &e.is_fn, &e.d_fn, &mut s);
            bytes += s.len();
        }
        for id in ids {
            bytes += *sizes.entry(id).or_insert_with(|| {
                let mut s = String::new();
                em.decl_function(id, &mut s, opts.max_depth);
                s.len()
            });
        }
        if bytes > budget {
            errors.push((
                1504,
                e.at.0,
                e.at.1,
                format!(
                    "The validator for '{}' is {:.1} KB, over the {} KB budget. Validate a smaller type or raise '[sound.guard] max-validator-kb'.",
                    e.type_name,
                    bytes as f64 / 1024.0,
                    opts.max_validator_kb
                ),
            ));
        }
    }
}

/// `$is$eN` / `$d$eN` for entries that are not a plain reference.
fn entry_functions(em: &mut Emitter<'_>, entries: &[EntryInfo], code: &mut String) {
    for e in entries.iter().filter(|e| !e.plain) {
        em.entry_functions(&e.g, &e.is_fn, &e.d_fn, code);
    }
}

/// Where the generated line goes: after a shebang line and the directive prologue.
fn insertion_point(ast: &Ast, text: &str) -> u32 {
    let mut at = 0u32;
    if text.starts_with("#!") {
        at = text.find('\n').map_or(text.len(), |i| i + 1) as u32;
    }
    if let Node::Program { stmts } = *ast.node(ast.root) {
        for &st in ast.list(stmts) {
            match *ast.node(st) {
                Node::Directive { .. } => at = at.max(ast.span(st).end),
                _ => break,
            }
        }
    }
    at
}

// ---- loading programs ----

/// `inner` with one file's text replaced (the text being transformed may be newer than the
/// disk, e.g. in the dev server). `alone`: no other file exists (its imports are not loaded).
struct Overlay<'a> {
    inner: &'a dyn FileSystem,
    path: &'a str,
    text: &'a str,
    alone: bool,
}

impl FileSystem for Overlay<'_> {
    fn read_file(&self, p: &str) -> Option<String> {
        if p == self.path {
            Some(self.text.to_owned())
        } else if self.alone {
            None
        } else {
            self.inner.read_file(p)
        }
    }
    fn file_exists(&self, p: &str) -> bool {
        p == self.path || (!self.alone && self.inner.file_exists(p))
    }
    fn directory_exists(&self, p: &str) -> bool {
        self.inner.directory_exists(p)
    }
    fn read_dir(&self, p: &str) -> Vec<DirEntry> {
        self.inner.read_dir(p)
    }
    fn realpath(&self, p: &str) -> String {
        self.inner.realpath(p)
    }
    fn case_sensitive(&self) -> bool {
        self.inner.case_sensitive()
    }
    fn cwd(&self) -> String {
        self.inner.cwd()
    }
    fn stamp(&self, p: &str) -> Option<FileStamp> {
        // The overlaid text is not the disk's: it is always compared, never stamped.
        if p == self.path || self.alone {
            None
        } else {
            self.inner.stamp(p)
        }
    }
}

/// Expands files read from a file system: finds each file's nearest `tsconfig.json` (for
/// module resolution), loads the program the file's imports reach (no lib files) and
/// rewrites it. Thread-safe; tsconfig options are cached per directory, parsed files per
/// contents (shared by every expansion of this expander, so a tool keeps one per process).
pub struct Expander {
    fs: Arc<dyn FileSystem>,
    pub opts: ExpandOptions,
    /// Options for every file (`ts-r-run` has one tsconfig), else per nearest tsconfig.
    fixed: Option<Arc<CompilerOptions>>,
    /// The tsconfig for every file (`--tsconfig`), loaded through `configs`.
    tsconfig: Option<String>,
    /// Directory (and tsconfig path) -> options of its nearest tsconfig.
    configs: Mutex<FxHashMap<String, Arc<CompilerOptions>>>,
    /// Parsed and bound files of every program loaded, reused while their text is unchanged:
    /// overlapping import closures are parsed once, not once per expanded file.
    sources: SourceCache,
}

/// `inner` without directory listings: loading a tsconfig for its options must not glob
/// its `include`.
struct NoListing<'a>(&'a dyn FileSystem);

impl FileSystem for NoListing<'_> {
    fn read_file(&self, p: &str) -> Option<String> {
        self.0.read_file(p)
    }
    fn file_exists(&self, p: &str) -> bool {
        self.0.file_exists(p)
    }
    fn directory_exists(&self, p: &str) -> bool {
        self.0.directory_exists(p)
    }
    fn read_dir(&self, _: &str) -> Vec<DirEntry> {
        Vec::new()
    }
    fn realpath(&self, p: &str) -> String {
        self.0.realpath(p)
    }
    fn case_sensitive(&self) -> bool {
        self.0.case_sensitive()
    }
    fn cwd(&self) -> String {
        self.0.cwd()
    }
}

impl std::fmt::Debug for Expander {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Expander")
            .field("opts", &self.opts)
            .finish_non_exhaustive()
    }
}

impl Expander {
    pub fn new(fs: Arc<dyn FileSystem>, opts: ExpandOptions) -> Expander {
        Expander {
            fs,
            opts,
            fixed: None,
            tsconfig: None,
            configs: Mutex::new(FxHashMap::default()),
            sources: SourceCache::new(),
        }
    }

    /// An expander that resolves every file with `options`.
    pub fn with_options(
        fs: Arc<dyn FileSystem>,
        opts: ExpandOptions,
        options: CompilerOptions,
    ) -> Expander {
        Expander {
            fixed: Some(Arc::new(options)),
            ..Expander::new(fs, opts)
        }
    }

    /// This expander resolving every file with the options of `tsconfig` (`--tsconfig`).
    pub fn with_tsconfig(self, tsconfig: &str) -> Expander {
        Expander {
            tsconfig: Some(tsconfig.to_owned()),
            ..self
        }
    }

    pub fn fs(&self) -> &dyn FileSystem {
        &*self.fs
    }

    /// Drops the cached tsconfig options (a config file changed): the next expansion reloads
    /// them. Parsed files go too (their binding depends on the options).
    pub fn forget_configs(&self) {
        self.configs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.sources.clear();
    }

    /// Drops the parsed files of `paths` (changed or deleted; watch mode). Not needed for
    /// correctness: an entry is only reused while the file's text is unchanged.
    pub fn forget_files<S: AsRef<str>>(&self, paths: &[S]) {
        for p in paths {
            self.sources.forget(p.as_ref());
        }
    }

    /// Files parsed so far (each at most once per contents and binding options).
    pub fn parsed_files(&self) -> usize {
        self.sources.parses()
    }

    /// Compiler options of the nearest `tsconfig.json` above `file` (defaults when none).
    pub fn options_for(&self, file: &str) -> Arc<CompilerOptions> {
        if let Some(o) = &self.fixed {
            return o.clone();
        }
        if let Some(t) = &self.tsconfig {
            return self.config(t);
        }
        let dir = path::directory_of(file).to_owned();
        if let Some(o) = self
            .configs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&dir)
        {
            return o.clone();
        }
        let mut d = dir.clone();
        let mut found = None;
        for _ in 0..64 {
            let cand = format!("{}/tsconfig.json", d.trim_end_matches('/'));
            if self.fs.file_exists(&cand) {
                found = Some(cand);
                break;
            }
            let up = path::directory_of(&d).to_owned();
            if up == d || up.is_empty() {
                break;
            }
            d = up;
        }
        let opts = match &found {
            Some(c) => self.config(c),
            None => Arc::new(CompilerOptions::default()),
        };
        self.configs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(dir, opts.clone());
        opts
    }

    /// The options of tsconfig `c` (cached).
    fn config(&self, c: &str) -> Arc<CompilerOptions> {
        let mut cache = self.configs.lock().unwrap_or_else(|e| e.into_inner());
        match cache.get(c) {
            Some(o) => o.clone(),
            None => {
                let o = Arc::new(load_tsconfig(&NoListing(&*self.fs), c).options);
                cache.insert(c.to_owned(), o.clone());
                o
            }
        }
    }

    /// Loads the program rooted at `file` (with `text` as its contents).
    pub fn program(&self, file: &str, text: &str) -> ProgramFiles {
        self.load(file, text, false)
    }

    fn load(&self, file: &str, text: &str, alone: bool) -> ProgramFiles {
        let options = self.options_for(file);
        let overlay = Overlay {
            inner: &*self.fs,
            path: file,
            text,
            alone,
        };
        load_program_cached(
            &overlay,
            &[file.to_owned()],
            &options,
            None,
            1,
            &self.sources,
        )
    }

    /// Rewrites `src` (the contents of `file`). `None` when the file has nothing to expand.
    pub fn expand(&self, file: &str, src: &str) -> Option<Expanded> {
        if !mentions_guard(src) || !is_expandable_path(file) {
            return None;
        }
        let index = |p: &ProgramFiles| {
            p.file_index(file)
                .or_else(|| p.files.iter().position(|f| !f.is_lib))
        };
        // A file that only uses runtime values (`expect`, `panic`, the error classes) needs
        // no declarations: it is expanded alone, without loading what it imports.
        let alone = self.load(file, src, true);
        let idx = index(&alone)?;
        if !needs_types(&alone.files[idx].ast) {
            return expand_program(&alone, idx, &self.opts);
        }
        let program = self.program(file, src);
        let idx = index(&program)?;
        expand_program(&program, idx, &self.opts)
    }
}
