//! What a module exports, as eslint-plugin-import's export map sees it (observed from its
//! tests; our own model):
//!
//! - Only files with a valid extension that are not ignored, whose text may hold an
//!   `import`/`export` (a cheap scan, [`maybe_module`]), get a map. After parsing, a file
//!   with no top-level import or export declaration (or `export =`) is no module: no map,
//!   unless it has a dynamic `import()` (then an "ambiguous" map, which `named` skips).
//! - `namespace`: names declared here (`export const/function/class/...`, `export { a }`,
//!   `export default`, `export * as ns`), each with the doc comment before it (for
//!   `no-deprecated`) and, when it is a namespace import of this file re-exported
//!   (`import * as ns from "x"; export { ns }`), the module it stands for.
//! - `reexports`: `export { a as b } from "x"` (name `b`, local `a`, the resolved file).
//! - `dependencies`: `export * from "x"` targets (also `export * as ns from "x"`, as
//!   upstream does), searched for names other than `default`.
//! - `imports`: every resolved import, re-export and dynamic import (for `no-cycle`).
//! - A parse error leaves only `errors` (rules report "Parse errors in imported module").
//! - `export = X` exports the members of a namespace `X`, or `X` as the default.

use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{Ast, CommentKind, Modifiers, Node, NodeId};
use ts_r_span::LineIndex;

use super::{MAX_DEPTH, ModCtx, Resolved};
use crate::ast_util::binding_idents;

/// A parsed doc comment: the tags rules ask about.
#[derive(Debug, Clone, Default)]
pub(crate) struct Doc {
    /// `(title, description)` per tag.
    pub tags: Vec<(String, String)>,
}

impl Doc {
    pub(crate) fn tag(&self, title: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|(t, _)| t == title)
            .map(|(_, d)| d.as_str())
    }
}

/// One exported name's metadata.
#[derive(Debug, Clone, Default)]
pub(crate) struct ExportMeta {
    pub doc: Option<Arc<Doc>>,
    /// The file a re-exported namespace import stands for.
    pub namespace: Option<Arc<str>>,
}

#[derive(Debug, Clone)]
pub(crate) struct Reexport {
    pub local: String,
    /// The resolved source (`None`: a builtin or unresolved).
    pub path: Option<Arc<str>>,
}

/// One import of a module (for `no-cycle`).
#[derive(Debug, Clone)]
pub(crate) struct ImportInfo {
    pub source: String,
    /// 1-based line of the specifier.
    pub line: u32,
    pub only_types: bool,
    pub dynamic: bool,
    /// What it imports (`importedSpecifiers`): imported names, `ImportDefaultSpecifier`,
    /// `ImportNamespaceSpecifier` (also for `import()`); empty for re-exports.
    pub specifiers: Vec<String>,
}

#[derive(Debug, Default)]
pub(crate) struct ExportMap {
    pub path: Arc<str>,
    namespace: Vec<(String, ExportMeta)>,
    ns_index: FxHashMap<String, usize>,
    pub reexports: Vec<(String, Reexport)>,
    re_index: FxHashMap<String, usize>,
    pub dependencies: Vec<Arc<str>>,
    pub imports: Vec<(Arc<str>, Vec<ImportInfo>)>,
    pub errors: Vec<String>,
    /// Unambiguously an ES module (`parseGoal === "Module"`).
    pub is_module: bool,
    /// The file's `@module` doc comment.
    pub doc: Option<Arc<Doc>>,
}

/// Upstream's quick test that a file may be a module: `import`/`export` at the start of a
/// line or after `;`, `}` or `)` (whitespace between), followed by whitespace and a word
/// character or by `{`, `*` or `=`; or `import(` anywhere.
pub(crate) fn maybe_module(text: &str) -> bool {
    if text.contains("import(") {
        return true;
    }
    let b = text.as_bytes();
    let mut from = 0;
    while let Some(off) = memchr_kw(&b[from..]) {
        let at = from + off;
        from = at + 6;
        // Before: whitespace back to a line start, the text start or `;`, `}`, `)`.
        let mut i = at;
        let mut ok = false;
        loop {
            if i == 0 {
                ok = true;
                break;
            }
            let c = b[i - 1];
            if c == b'\n' || c == b'\r' {
                ok = true;
                break;
            }
            if matches!(c, b';' | b'}' | b')') {
                ok = true;
                break;
            }
            if c == b' ' || c == b'\t' || c == 0x0b || c == 0x0c {
                i -= 1;
                continue;
            }
            break;
        }
        if !ok {
            continue;
        }
        // After: `\s+\w` or `\s*[{*=]`.
        let rest = &b[at + 6..];
        let ws = rest
            .iter()
            .take_while(|c| c.is_ascii_whitespace() || **c == 0x0b)
            .count();
        match rest.get(ws) {
            Some(b'{' | b'*' | b'=') => return true,
            Some(c) if ws > 0 && (c.is_ascii_alphanumeric() || *c == b'_') => return true,
            _ => {}
        }
    }
    false
}

/// The offset of the next `import` or `export`.
fn memchr_kw(b: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 6 <= b.len() {
        let w = &b[i..i + 6];
        if w == b"import" || w == b"export" {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// The name of an export/import specifier part: an identifier or a string.
pub(crate) fn module_name(ast: &Ast, id: NodeId) -> Option<&str> {
    match *ast.node(id) {
        Node::Ident { name } => Some(ast.atom(name)),
        Node::StringLit { value } => Some(ast.atom(value)),
        _ => None,
    }
}

fn string_value(ast: &Ast, id: NodeId) -> Option<&str> {
    match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value)),
        _ => None,
    }
}

struct Builder<'a> {
    cx: &'a ModCtx,
    ast: &'a Ast,
    src: &'a str,
    lines: LineIndex,
    map: ExportMap,
    /// Namespace imports seen so far: local name, source.
    ns_imports: FxHashMap<String, String>,
}

impl ExportMap {
    fn set_ns(&mut self, name: &str, meta: ExportMeta) {
        match self.ns_index.get(name) {
            Some(&i) => self.namespace[i].1 = meta,
            None => {
                self.ns_index.insert(name.to_owned(), self.namespace.len());
                self.namespace.push((name.to_owned(), meta));
            }
        }
    }
    fn set_re(&mut self, name: &str, re: Reexport) {
        match self.re_index.get(name) {
            Some(&i) => self.reexports[i].1 = re,
            None => {
                self.re_index.insert(name.to_owned(), self.reexports.len());
                self.reexports.push((name.to_owned(), re));
            }
        }
    }
    /// The names the module itself exports (not its re-exports), in order.
    pub(crate) fn namespace_names(&self) -> impl Iterator<Item = &str> {
        self.namespace.iter().map(|(n, _)| n.as_str())
    }
    pub(crate) fn namespace_has(&self, name: &str) -> bool {
        self.ns_index.contains_key(name)
    }
    pub(crate) fn namespace_get(&self, name: &str) -> Option<&ExportMeta> {
        self.ns_index.get(name).map(|&i| &self.namespace[i].1)
    }
    pub(crate) fn reexport(&self, name: &str) -> Option<&Reexport> {
        self.re_index.get(name).map(|&i| &self.reexports[i].1)
    }
    pub(crate) fn has_default(&self, cx: &ModCtx) -> bool {
        !matches!(get(cx, self, "default"), Lookup::Undefined | Lookup::Null)
    }
}

/// The result of looking a name up (`ExportMap.get`).
#[derive(Debug, Clone)]
pub(crate) enum Lookup {
    /// Not exported.
    Undefined,
    /// Re-exported from a module that has no map (ignored, CommonJS, unresolved).
    Null,
    Found(ExportMeta),
}

/// The export map a re-export's source stands for.
fn import_of(cx: &ModCtx, re: &Reexport) -> Option<Arc<ExportMap>> {
    re.path.as_deref().and_then(|p| cx.map_for(p))
}

/// Maps (or map and name pairs) one lookup has already walked. Every recursive walk over
/// `export *` and re-exports goes through one, so cycles (`a`, `b` and `c` each
/// `export *` the others) end and shared targets are walked once: the work is linear in
/// the maps reached, not exponential in the paths to them. The plugin itself recurses
/// without a check (and overflows its stack on such cycles); on acyclic graphs the answers
/// are the same, since a second walk of the same map and name gives the same answer.
#[derive(Default)]
struct Seen {
    maps: FxHashSet<Arc<str>>,
    names: FxHashSet<(Arc<str>, String)>,
}

impl Seen {
    fn first_map(&mut self, m: &ExportMap) -> bool {
        self.maps.insert(m.path.clone())
    }
    fn first_name(&mut self, m: &ExportMap, name: &str) -> bool {
        self.names.insert((m.path.clone(), name.to_owned()))
    }
}

/// `name` is exported by `m` (explicitly, or through `export *` for names other than
/// `default`).
pub(crate) fn has(cx: &ModCtx, m: &ExportMap, name: &str) -> bool {
    has_at(cx, m, name, 0, &mut Seen::default())
}

fn has_at(cx: &ModCtx, m: &ExportMap, name: &str, depth: usize, seen: &mut Seen) -> bool {
    if !seen.first_map(m) {
        return false;
    }
    if m.namespace_has(name) || m.re_index.contains_key(name) {
        return true;
    }
    if name != "default" && depth < MAX_DEPTH {
        for d in &m.dependencies {
            if let Some(inner) = cx.map_for(d)
                && has_at(cx, &inner, name, depth + 1, seen)
            {
                return true;
            }
        }
    }
    false
}

/// `hasDeep`: whether `name` resolves through re-exports to a declaration, and the chain of
/// files followed.
pub(crate) fn has_deep(cx: &ModCtx, m: &ExportMap, name: &str) -> (bool, Vec<Arc<str>>) {
    let mut chain = Vec::new();
    let found = has_deep_at(cx, m, name, &mut chain, 0, &mut Seen::default());
    (found, chain)
}

fn has_deep_at(
    cx: &ModCtx,
    m: &ExportMap,
    name: &str,
    chain: &mut Vec<Arc<str>>,
    depth: usize,
    seen: &mut Seen,
) -> bool {
    chain.push(m.path.clone());
    if !seen.first_name(m, name) {
        return false;
    }
    if m.namespace_has(name) {
        return true;
    }
    if let Some(re) = m.reexport(name) {
        let Some(imported) = import_of(cx, re) else {
            return true;
        };
        if imported.path == m.path && re.local == name {
            return false;
        }
        if depth >= MAX_DEPTH {
            return true;
        }
        let local = re.local.clone();
        return has_deep_at(cx, &imported, &local, chain, depth + 1, seen);
    }
    if name != "default" && depth < MAX_DEPTH {
        for d in &m.dependencies {
            let Some(inner) = cx.map_for(d) else {
                // An `export *` of a module without a map: assume it has the name.
                return true;
            };
            if inner.path == m.path {
                continue;
            }
            let mut sub = Vec::new();
            if has_deep_at(cx, &inner, name, &mut sub, depth + 1, seen) {
                chain.extend(sub);
                return true;
            }
        }
    }
    false
}

/// `ExportMap.get`.
pub(crate) fn get(cx: &ModCtx, m: &ExportMap, name: &str) -> Lookup {
    get_at(cx, m, name, 0, &mut Seen::default())
}

fn get_at(cx: &ModCtx, m: &ExportMap, name: &str, depth: usize, seen: &mut Seen) -> Lookup {
    if !seen.first_name(m, name) {
        return Lookup::Undefined;
    }
    if let Some(meta) = m.namespace_get(name) {
        return Lookup::Found(meta.clone());
    }
    if let Some(re) = m.reexport(name) {
        let Some(imported) = import_of(cx, re) else {
            return Lookup::Null;
        };
        if (imported.path == m.path && re.local == name) || depth >= MAX_DEPTH {
            return Lookup::Undefined;
        }
        let local = re.local.clone();
        return get_at(cx, &imported, &local, depth + 1, seen);
    }
    if name != "default" && depth < MAX_DEPTH {
        for d in &m.dependencies {
            let Some(inner) = cx.map_for(d) else {
                continue;
            };
            if inner.path == m.path {
                continue;
            }
            match get_at(cx, &inner, name, depth + 1, seen) {
                Lookup::Undefined => {}
                v => return v,
            }
        }
    }
    Lookup::Undefined
}

/// `size`: explicit names plus every `export *` target's size. Each map counts once (the
/// rules only ask whether it is zero).
pub(crate) fn size(cx: &ModCtx, m: &ExportMap) -> usize {
    size_at(cx, m, 0, &mut Seen::default())
}

fn size_at(cx: &ModCtx, m: &ExportMap, depth: usize, seen: &mut Seen) -> usize {
    if !seen.first_map(m) {
        return 0;
    }
    let mut n = m.namespace.len() + m.reexports.len();
    if depth < MAX_DEPTH {
        for d in &m.dependencies {
            if let Some(inner) = cx.map_for(d) {
                n += size_at(cx, &inner, depth + 1, seen);
            }
        }
    }
    n
}

/// Every exported name (`forEach`): explicit ones, then those of `export *` targets other
/// than `default`. Each map is walked once, so a name reached twice is listed once (the
/// `export` rule collects them in a set).
pub(crate) fn names(cx: &ModCtx, m: &ExportMap) -> Vec<String> {
    let mut out = Vec::new();
    names_at(cx, m, &mut out, 0, &mut Seen::default());
    out
}

fn names_at(cx: &ModCtx, m: &ExportMap, out: &mut Vec<String>, depth: usize, seen: &mut Seen) {
    if !seen.first_map(m) {
        return;
    }
    out.extend(m.namespace.iter().map(|(n, _)| n.clone()));
    out.extend(m.reexports.iter().map(|(n, _)| n.clone()));
    if depth < MAX_DEPTH {
        for d in &m.dependencies {
            if let Some(inner) = cx.map_for(d) {
                let mut sub = Vec::new();
                names_at(cx, &inner, &mut sub, depth + 1, seen);
                out.extend(sub.into_iter().filter(|n| n != "default"));
            }
        }
    }
}

/// Parses `text` (the file `p`) and builds its map. `None` when it is not a module.
pub(crate) fn build(
    cx: &ModCtx,
    p: &str,
    text: &str,
    typescript: bool,
    module: bool,
) -> Option<ExportMap> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let kind = if typescript {
        ts_r_ast::SourceKind::from_path(p)
    } else if p.ends_with(".jsx") {
        ts_r_ast::SourceKind::Jsx
    } else {
        ts_r_ast::SourceKind::Js
    };
    let parsed = ts_r_parser::parse_with(
        text,
        kind,
        ts_r_parser::ParseOptions {
            ts_in_js: if typescript {
                ts_r_parser::TsInJs::Allow
            } else {
                ts_r_parser::TsInJs::Reject
            },
            module,
            assertion_continues: true,
        },
    );
    let lines = LineIndex::new(text);
    let mut map = ExportMap {
        path: p.into(),
        ..ExportMap::default()
    };
    let errors = parsed.syntax_errors();
    let syntax = errors
        .iter()
        .find(|d| d.is_error() && !crate::engine::is_ts_only_in_js_error(d.code))
        .map(|d| (d.span.start, d.message.clone()));
    // espree stops at a top-level `return` as soon as it reads the keyword.
    let first = match (syntax, espree_only_error(&parsed.ast, typescript)) {
        (Some(a), Some(b)) => Some(if b.0 <= a.0 { b } else { a }),
        (a, b) => a.or(b),
    };
    if let Some((at, msg)) = first {
        let lc = lines.line_col_utf16(text, at);
        map.errors
            .push(format!("{msg} ({}:{})", lc.line + 1, lc.col + 1));
        return Some(map);
    }
    let ast = &parsed.ast;
    let mut b = Builder {
        cx,
        ast,
        src: text,
        lines,
        map,
        ns_imports: FxHashMap::default(),
    };
    let has_dynamic = b.dynamic_imports();
    let stmts: Vec<NodeId> = match *ast.node(ast.root) {
        Node::Program { stmts } => ast.list(stmts).to_vec(),
        _ => Vec::new(),
    };
    let is_module = stmts.iter().any(|&s| is_module_statement(ast, s));
    if !is_module && !has_dynamic {
        return None;
    }
    b.module_doc();
    let mut prev_end = 0;
    for &s in &stmts {
        b.statement(s, prev_end, &stmts);
        prev_end = ast.span(s).end;
    }
    b.map.is_module = is_module;
    Some(b.map)
}

/// Errors espree reports that our parser leaves to the checker: a top-level `return`.
fn espree_only_error(ast: &Ast, typescript: bool) -> Option<(u32, String)> {
    if typescript {
        return None;
    }
    let Node::Program { stmts } = *ast.node(ast.root) else {
        return None;
    };
    ast.list(stmts)
        .iter()
        .find(|&&s| matches!(ast.node(s), Node::Return { .. }))
        .map(|&s| (ast.span(s).start, "'return' outside of function".to_owned()))
}

/// An ESTree import/export declaration or `export =` (what makes a file a module).
fn is_module_statement(ast: &Ast, s: NodeId) -> bool {
    match ast.node(s) {
        Node::ImportDecl { .. }
        | Node::ExportNamed { .. }
        | Node::ExportAll { .. }
        | Node::ExportDefaultExpr { .. }
        | Node::ExportAssignment { .. } => true,
        Node::ImportEquals { .. } => false,
        n => n.modifiers().contains(Modifiers::EXPORT),
    }
}

impl Builder<'_> {
    fn loc_line(&self, at: u32) -> u32 {
        self.lines.line_col(at).line + 1
    }

    fn resolve(&self, spec: &str) -> Option<Arc<str>> {
        match self.cx.resolve_from(spec, &self.map.path) {
            Ok(Resolved::File(p)) => Some(p),
            _ => None,
        }
    }

    fn add_import(&mut self, spec: &str, info: ImportInfo) -> Option<Arc<str>> {
        let p = self.resolve(spec)?;
        match self.map.imports.iter_mut().find(|(q, _)| *q == p) {
            Some((_, v)) => v.push(info),
            None => self.map.imports.push((p.clone(), vec![info])),
        }
        Some(p)
    }

    /// Records every `import("x")` with a string literal; true when there is any
    /// `import()`.
    fn dynamic_imports(&mut self) -> bool {
        let ast = self.ast;
        let mut found = Vec::new();
        let mut any = false;
        crate::rules::import_util::walk(ast, |id| {
            if let Node::ImportCall { arg, .. } = *ast.node(id) {
                any = true;
                if let Some(v) = string_value(ast, arg) {
                    found.push((v.to_owned(), ast.span(arg).start));
                }
            }
        });
        for (v, at) in found {
            let info = ImportInfo {
                source: v.clone(),
                line: self.loc_line(at),
                only_types: false,
                dynamic: true,
                specifiers: vec!["ImportNamespaceSpecifier".to_owned()],
            };
            self.add_import(&v, info);
        }
        any
    }

    fn module_doc(&mut self) {
        for c in &self.ast.comments {
            if c.kind != CommentKind::Block {
                continue;
            }
            let Some(text) = c.span.text(self.src).get(2..) else {
                continue;
            };
            let body = text.strip_suffix("*/").unwrap_or(text);
            let doc = parse_jsdoc(body);
            if doc.tag("module").is_some() {
                self.map.doc = Some(Arc::new(doc));
                return;
            }
        }
    }

    /// The doc of the first of `nodes` (as `(previous token end, node start)` pairs) with
    /// comments right before it.
    fn capture_doc(&self, nodes: &[(u32, u32)]) -> Option<Arc<Doc>> {
        for &(after, start) in nodes {
            let comments = self.ast.comments_between(after, start);
            if comments.is_empty() {
                continue;
            }
            let mut doc = None;
            if self.cx.settings.jsdoc {
                for c in comments {
                    if c.kind == CommentKind::Block
                        && let Some(t) = c.span.text(self.src).get(2..)
                    {
                        doc = Some(parse_jsdoc(t.strip_suffix("*/").unwrap_or(t)));
                    }
                }
            }
            if self.cx.settings.tomdoc
                && let Some(d) = tomdoc(self.src, comments)
            {
                doc = Some(d);
            }
            return doc.map(Arc::new);
        }
        None
    }

    fn ns_meta(&self, local: Option<&str>, doc: Option<Arc<Doc>>) -> ExportMeta {
        let namespace = local
            .and_then(|l| self.ns_imports.get(l))
            .and_then(|src| self.resolve(src));
        ExportMeta { doc, namespace }
    }

    fn statement(&mut self, s: NodeId, prev_end: u32, all: &[NodeId]) {
        let ast = self.ast;
        let start = ast.span(s).start;
        match *ast.node(s) {
            Node::ImportDecl {
                type_only,
                specifiers,
                source,
                ..
            } => {
                let Some(v) = string_value(ast, source) else {
                    return;
                };
                let specs = ast.list(specifiers);
                let mut all_types = !specs.is_empty();
                let mut imported_names = Vec::new();
                for &sp in specs {
                    match *ast.node(sp) {
                        Node::ImportSpecifier {
                            type_only: t,
                            imported,
                            local,
                        } => {
                            all_types &= t;
                            if let Some(n) = module_name(ast, imported.unwrap_or(local)) {
                                imported_names.push(n.to_owned());
                            }
                        }
                        Node::ImportDefaultSpecifier { .. } => {
                            all_types = false;
                            imported_names.push("ImportDefaultSpecifier".to_owned());
                        }
                        Node::ImportNamespaceSpecifier { local } => {
                            all_types = false;
                            imported_names.push("ImportNamespaceSpecifier".to_owned());
                            if let Some(n) = module_name(ast, local) {
                                self.ns_imports.insert(n.to_owned(), v.to_owned());
                            }
                        }
                        _ => {}
                    }
                }
                let info = ImportInfo {
                    source: v.to_owned(),
                    line: self.loc_line(ast.span(source).start),
                    only_types: type_only || all_types,
                    dynamic: false,
                    specifiers: imported_names,
                };
                self.add_import(v, info);
            }
            Node::ExportAll {
                type_only,
                exported,
                source,
                ..
            } => {
                let Some(v) = string_value(ast, source) else {
                    return;
                };
                let info = ImportInfo {
                    source: v.to_owned(),
                    line: self.loc_line(ast.span(source).start),
                    only_types: type_only,
                    dynamic: false,
                    specifiers: Vec::new(),
                };
                if let Some(p) = self.add_import(v, info)
                    && !self.map.dependencies.contains(&p)
                {
                    self.map.dependencies.push(p);
                }
                if let Some(e) = exported.and_then(|e| module_name(ast, e)) {
                    let e = e.to_owned();
                    self.map.set_ns(&e, ExportMeta::default());
                }
            }
            Node::ExportNamed {
                type_only,
                specifiers,
                source,
                ..
            } => {
                let specs = ast.list(specifiers);
                let src = source.and_then(|s| string_value(ast, s));
                if let (Some(v), Some(source)) = (src, source) {
                    let mut all_types = !specs.is_empty();
                    for &sp in specs {
                        if let Node::ExportSpecifier { type_only: t, .. } = *ast.node(sp) {
                            all_types &= t;
                        }
                    }
                    let info = ImportInfo {
                        source: v.to_owned(),
                        line: self.loc_line(ast.span(source).start),
                        only_types: type_only || all_types,
                        dynamic: false,
                        specifiers: Vec::new(),
                    };
                    self.add_import(v, info);
                }
                let resolved = src.and_then(|v| self.resolve(v));
                for &sp in specs {
                    let Node::ExportSpecifier {
                        local, exported, ..
                    } = *ast.node(sp)
                    else {
                        continue;
                    };
                    let Some(local_name) = module_name(ast, local) else {
                        continue;
                    };
                    let exported_name = exported
                        .and_then(|e| module_name(ast, e))
                        .unwrap_or(local_name)
                        .to_owned();
                    if src.is_some() {
                        self.map.set_re(
                            &exported_name,
                            Reexport {
                                local: local_name.to_owned(),
                                path: resolved.clone(),
                            },
                        );
                    } else {
                        let meta = self.ns_meta(Some(local_name), None);
                        self.map.set_ns(&exported_name, meta);
                    }
                }
            }
            Node::ExportDefaultExpr { expr } => {
                let doc = self.capture_doc(&[(prev_end, start)]);
                let local = ident_of(ast, expr);
                let meta = self.ns_meta(local, doc);
                self.map.set_ns("default", meta);
            }
            Node::ExportAssignment { expr } => self.export_assignment(s, expr, prev_end, all),
            _ => {
                let mods = ast.node(s).modifiers();
                if !mods.contains(Modifiers::EXPORT) {
                    return;
                }
                if mods.contains(Modifiers::DEFAULT) {
                    let doc = self.capture_doc(&[(prev_end, start)]);
                    self.map.set_ns(
                        "default",
                        ExportMeta {
                            doc,
                            namespace: None,
                        },
                    );
                    return;
                }
                if let Node::VarDecl { decls, .. } = *ast.node(s) {
                    let mut after = start;
                    for &d in ast.list(decls) {
                        let Node::VarDeclarator { name, .. } = *ast.node(d) else {
                            continue;
                        };
                        let doc =
                            self.capture_doc(&[(after, ast.span(d).start), (prev_end, start)]);
                        let mut ids = Vec::new();
                        binding_idents(ast, name, &mut ids);
                        for id in ids {
                            if let Some(n) = ident_name_of(ast, id) {
                                let n = n.to_owned();
                                self.map.set_ns(
                                    &n,
                                    ExportMeta {
                                        doc: doc.clone(),
                                        namespace: None,
                                    },
                                );
                            }
                        }
                        after = ast.span(d).end;
                    }
                    return;
                }
                if let Some(n) = declaration_name(ast, s) {
                    let doc = self.capture_doc(&[(prev_end, start)]);
                    let n = n.to_owned();
                    self.map.set_ns(
                        &n,
                        ExportMeta {
                            doc,
                            namespace: None,
                        },
                    );
                }
            }
        }
    }

    /// `export = X`: the members of a namespace `X`, or `X` as the default export.
    fn export_assignment(&mut self, s: NodeId, expr: NodeId, prev_end: u32, all: &[NodeId]) {
        let ast = self.ast;
        let name = ident_of(ast, expr);
        let decls: Vec<NodeId> = all
            .iter()
            .copied()
            .filter(|&d| match *ast.node(d) {
                Node::VarDecl { decls, .. } => ast.list(decls).iter().any(|&v| {
                    matches!(*ast.node(v), Node::VarDeclarator { name: n, .. }
                        if ident_name_of(ast, n).is_some() && ident_name_of(ast, n) == name)
                }),
                Node::FunctionDecl { body: None, .. }
                | Node::ClassDecl { .. }
                | Node::Enum { .. }
                | Node::TypeAlias { .. }
                | Node::Interface { .. }
                | Node::Module { .. } => name.is_some() && declaration_name(ast, d) == name,
                _ => false,
            })
            .collect();
        if decls.is_empty() {
            let doc = self.capture_doc(&[(prev_end, ast.span(s).start)]);
            self.map.set_ns(
                "default",
                ExportMeta {
                    doc,
                    namespace: None,
                },
            );
            return;
        }
        for d in decls {
            if let Node::Module { body, .. } = *ast.node(d) {
                let Some(body) = body else { continue };
                match *ast.node(body) {
                    Node::Module { name, .. } => {
                        if let Some(n) = ident_name_of(ast, name) {
                            let n = n.to_owned();
                            self.map.set_ns(&n, ExportMeta::default());
                        }
                    }
                    Node::ModuleBlock { stmts } => {
                        let mut after = ast.span(body).start;
                        for &m in ast.list(stmts) {
                            let doc = self.capture_doc(&[(after, ast.span(m).start)]);
                            if let Node::VarDecl { decls, .. } = *ast.node(m) {
                                for &v in ast.list(decls) {
                                    if let Node::VarDeclarator { name, .. } = *ast.node(v) {
                                        let mut ids = Vec::new();
                                        binding_idents(ast, name, &mut ids);
                                        for id in ids {
                                            if let Some(n) = ident_name_of(ast, id) {
                                                let n = n.to_owned();
                                                self.map.set_ns(
                                                    &n,
                                                    ExportMeta {
                                                        doc: doc.clone(),
                                                        namespace: None,
                                                    },
                                                );
                                            }
                                        }
                                    }
                                }
                            } else if let Some(n) = declaration_name(ast, m) {
                                let n = n.to_owned();
                                self.map.set_ns(
                                    &n,
                                    ExportMeta {
                                        doc,
                                        namespace: None,
                                    },
                                );
                            }
                            after = ast.span(m).end;
                        }
                    }
                    _ => {}
                }
            } else {
                let doc = self.capture_doc(&[(prev_end, ast.span(d).start)]);
                self.map.set_ns(
                    "default",
                    ExportMeta {
                        doc,
                        namespace: None,
                    },
                );
            }
        }
    }
}

fn ident_of(ast: &Ast, id: NodeId) -> Option<&str> {
    match *ast.node(crate::ast_util::skip_parens(ast, id)) {
        Node::Ident { name } => Some(ast.atom(name)),
        _ => None,
    }
}

fn ident_name_of(ast: &Ast, id: NodeId) -> Option<&str> {
    match *ast.node(id) {
        Node::Ident { name } => Some(ast.atom(name)),
        _ => None,
    }
}

/// The declared name of a function, class, enum, type alias, interface or namespace.
pub(crate) fn declaration_name(ast: &Ast, d: NodeId) -> Option<&str> {
    match *ast.node(d) {
        Node::FunctionDecl { name, .. } | Node::ClassDecl { name, .. } => {
            name.and_then(|n| ident_name_of(ast, n))
        }
        Node::Enum { name, .. } | Node::TypeAlias { name, .. } | Node::Interface { name, .. } => {
            ident_name_of(ast, name)
        }
        Node::Module { name, .. } => match *ast.node(name) {
            Node::Ident { name } => Some(ast.atom(name)),
            Node::QualifiedName { left, .. } => leftmost(ast, left),
            _ => None,
        },
        _ => None,
    }
}

fn leftmost(ast: &Ast, mut id: NodeId) -> Option<&str> {
    for _ in 0..MAX_DEPTH {
        match *ast.node(id) {
            Node::Ident { name } => return Some(ast.atom(name)),
            Node::QualifiedName { left, .. } => id = left,
            _ => return None,
        }
    }
    None
}

/// Tags of a JSDoc block (the text between `/*` and `*/`): `@title description`, the
/// description running to the next tag; leading `*`s are dropped.
pub(crate) fn parse_jsdoc(body: &str) -> Doc {
    let mut tags: Vec<(String, String)> = Vec::new();
    for raw in body.lines() {
        let line = raw.trim_start();
        let line = line.strip_prefix('*').unwrap_or(line);
        let line = line.trim();
        if let Some(rest) = line.strip_prefix('@') {
            let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
            let (title, desc) = rest.split_at(end);
            tags.push((title.to_owned(), desc.trim().to_owned()));
        } else if let Some((_, d)) = tags.last_mut()
            && !line.is_empty()
        {
            if !d.is_empty() {
                d.push('\n');
            }
            d.push_str(line);
        }
    }
    Doc { tags }
}

/// TomDoc: the leading comment lines up to the first blank one, joined; `Public:`,
/// `Internal:` or `Deprecated:` starts a status tag.
fn tomdoc(src: &str, comments: &[ts_r_ast::Comment]) -> Option<Doc> {
    let mut lines = Vec::new();
    for c in comments {
        let text = c.span.text(src);
        let value = match c.kind {
            CommentKind::Line => text.get(2..).unwrap_or(""),
            _ => text.get(2..text.len().saturating_sub(2)).unwrap_or(""),
        };
        if value.trim().is_empty() {
            break;
        }
        lines.push(value.trim());
    }
    let joined = lines.join(" ");
    for (status, title) in [
        ("Public:", "public"),
        ("Internal:", "internal"),
        ("Deprecated:", "deprecated"),
    ] {
        if let Some(rest) = joined.strip_prefix(status) {
            let desc = rest.trim_start();
            if desc.is_empty() {
                return None;
            }
            return Some(Doc {
                tags: vec![(title.to_owned(), desc.to_owned())],
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maybe_module_scan() {
        assert!(maybe_module("import x from 'y'"));
        assert!(maybe_module("a();\nexport {a}"));
        assert!(maybe_module("foo(import('x'))"));
        assert!(maybe_module("}export *"));
        assert!(!maybe_module("const imports = 1; // export"));
        assert!(!maybe_module("module.exports = {}"));
        assert!(!maybe_module("x.import = 1"));
        assert!(!maybe_module("// ü export\u{2028}"));
        assert!(!maybe_module(""));
    }

    #[test]
    fn jsdoc_tags() {
        let d =
            parse_jsdoc("*\n * Old.\n * @deprecated use ü instead\n *   of this\n * @param x\n ");
        assert_eq!(d.tag("deprecated"), Some("use ü instead\nof this"));
        assert_eq!(d.tag("param"), Some("x"));
        assert_eq!(d.tag("returns"), None);
        assert!(parse_jsdoc("@").tag("").is_some());
    }
}
