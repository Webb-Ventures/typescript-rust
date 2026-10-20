//! CommonJS output (`module: commonjs`, and `.cts`/CommonJS files under `node16`/`nodenext`).
//!
//! tsc 7 shapes:
//! ```js
//! "use strict";
//! Object.defineProperty(exports, "__esModule", { value: true });
//! exports.b = exports.a = void 0;            // every export but functions, reversed
//! exports.f = f;                             // function exports are hoisted
//! const fs_1 = __importDefault(require("fs"));     // import fs from "fs"
//! const d_1 = require("./d");                       // import { a } from "./d"
//! Object.defineProperty(exports, "y", { enumerable: true, get: function () { return x_1.x; } });
//! exports.a = 1;                             // export const a = 1;
//! (0, d_1.a)();                              // calls of imported functions
//! ```

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{Atom, Modifiers, Node, NodeId, NodeKind, VarKind};

use crate::builder::{Build, replace};
use crate::helpers::Helper;
use crate::scope::{BindingId, BindingKind, collect_pattern_idents};
use crate::transform::Tx;

#[derive(Clone, Debug)]
enum ImportRef {
    Named { module: Atom, name: String },
    Default { module: Atom },
}

#[derive(Default, Debug)]
pub(crate) struct State {
    /// Callees rewritten to `mod_1.f` (calls become `(0, mod_1.f)()`).
    pub detached: FxHashSet<NodeId>,
    imports: FxHashMap<BindingId, ImportRef>,
    /// Top-level `export`-declared variables: references become `exports.x`.
    exported_vars: FxHashSet<BindingId>,
    /// Names for `exports.a = exports.b = void 0;` in source order.
    void0: Vec<String>,
    /// The same names once written, with the statement (the `using` lowering reorders them).
    pub void0_written: Vec<String>,
    pub void0_stmt: Option<NodeId>,
    /// `Object.defineProperty(exports, "__esModule", ...)`.
    pub esmodule_stmt: Option<NodeId>,
    /// `exports.f = f;` hoisted function exports `(exported, local)`.
    hoisted: Vec<(String, String)>,
    /// `export { local as alias }` of local declarations: emitted after the declaration.
    local_exports: FxHashMap<BindingId, Vec<String>>,
    /// The same aliases, kept: assignments to the local update them too.
    live_exports: FxHashMap<BindingId, Vec<String>>,
    /// The expression of the expression statement being transformed (its value is unused).
    pub stmt_expr: Option<NodeId>,
    /// `export { imported as alias }`: getters emitted after the import.
    import_exports: FxHashMap<BindingId, Vec<(String, ts_r_span::Span)>>,
    /// Named and default import bindings with their declaration, source and imported name
    /// (`None` for `default`), for references met before the import statement.
    later_imports: FxHashMap<BindingId, (NodeId, NodeId, Option<String>)>,
    /// Module variables allocated early for such references, by import declaration.
    module_vars: FxHashMap<NodeId, Atom>,
    /// The name of an anonymous `export default function/class`.
    default_name: Option<Atom>,
    export_assignment: bool,
    /// `module.exports = x;` statements for `export =`, written at the end of the file.
    pub export_assignments: Vec<NodeId>,
    /// `tslib_1` once a helper is referenced under importHelpers.
    pub tslib: Option<Atom>,
    /// Last module prologue statement, before the helper import.
    helper_import_after: Option<NodeId>,
}

/// Collects the module's exports before the statements are transformed.
pub(crate) fn prepare(tx: &mut Tx, items: &[NodeId]) {
    for &s in items {
        let n = tx.n(s);
        let mods = n.modifiers();
        if mods.contains(Modifiers::DECLARE) {
            continue;
        }
        match n {
            Node::ExportAssignment { .. } => tx.cjs.export_assignment = true,
            Node::ImportDecl {
                type_only: false,
                specifiers,
                source,
                ..
            } => {
                for sp in tx.items(specifiers) {
                    let (local, imported) = match tx.n(sp) {
                        Node::ImportDefaultSpecifier { local } => (local, None),
                        Node::ImportSpecifier {
                            type_only: false,
                            imported,
                            local,
                        } => {
                            let imp = tx.name(imported.unwrap_or(local)).to_owned();
                            (local, (imp != "default").then_some(imp))
                        }
                        _ => continue,
                    };
                    if let Some(&b) = tx.sc.decl_of.get(&local) {
                        tx.cjs.later_imports.insert(b, (s, source, imported));
                    }
                }
            }
            Node::ExportNamed {
                type_only: false,
                specifiers,
                source,
                ..
            } => {
                for sp in tx.items(specifiers) {
                    let Node::ExportSpecifier {
                        type_only: false,
                        local,
                        exported,
                    } = tx.n(sp)
                    else {
                        continue;
                    };
                    let exported_name = tx.name(exported.unwrap_or(local)).to_owned();
                    if !tx.opts.verbatim_module_syntax && !tx.value_alias(sp) {
                        continue;
                    }
                    if source.is_some() {
                        tx.cjs.void0.push(exported_name);
                        continue;
                    }
                    if tx.is_type_only_local(local) {
                        continue;
                    }
                    let Some(name) = tx.atom_of(local) else {
                        continue;
                    };
                    match tx.sc.module_binding(name) {
                        Some(b) => match tx.sc.binding(b).kind {
                            BindingKind::Import => {
                                tx.cjs.void0.push(exported_name.clone());
                                let span = tx.span(sp);
                                tx.cjs
                                    .import_exports
                                    .entry(b)
                                    .or_default()
                                    .push((exported_name, span));
                            }
                            BindingKind::Function => {
                                let l = tx.name(local).to_owned();
                                tx.cjs.hoisted.push((exported_name, l));
                            }
                            _ => {
                                tx.cjs.void0.push(exported_name.clone());
                                tx.cjs
                                    .live_exports
                                    .entry(b)
                                    .or_default()
                                    .push(exported_name.clone());
                                tx.cjs
                                    .local_exports
                                    .entry(b)
                                    .or_default()
                                    .push(exported_name);
                            }
                        },
                        None => tx.cjs.void0.push(exported_name),
                    }
                }
            }
            Node::ExportAll {
                type_only: false,
                exported: Some(e),
                ..
            } => {
                let n = tx.name(e).to_owned();
                tx.cjs.void0.push(n);
            }
            Node::VarDecl { decls, .. } if mods.contains(Modifiers::EXPORT) => {
                for d in tx.items(decls) {
                    if let Node::VarDeclarator { name, .. } = tx.n(d) {
                        let mut ids = Vec::new();
                        collect_pattern_idents(tx.ast, name, &mut ids);
                        for i in ids {
                            let n = tx.name(i).to_owned();
                            tx.cjs.void0.push(n);
                            if let Some(&b) = tx.sc.decl_of.get(&i) {
                                tx.cjs.exported_vars.insert(b);
                            }
                        }
                    }
                }
            }
            Node::FunctionDecl {
                name,
                body: Some(_),
                ..
            } if mods.contains(Modifiers::EXPORT) => {
                let exported = if mods.contains(Modifiers::DEFAULT) {
                    "default".to_owned()
                } else {
                    name.map(|n| tx.name(n).to_owned()).unwrap_or_default()
                };
                let local = match name {
                    Some(n) => tx.name(n).to_owned(),
                    None => {
                        let a = tx.fresh("default", true);
                        tx.cjs.default_name = Some(a);
                        tx.ast.atom(a).to_owned()
                    }
                };
                tx.cjs.hoisted.push((exported, local));
            }
            Node::ClassDecl { name, .. } if mods.contains(Modifiers::EXPORT) => {
                if mods.contains(Modifiers::DEFAULT) {
                    if name.is_none() {
                        let a = tx.fresh("default", true);
                        tx.cjs.default_name = Some(a);
                    }
                } else if let Some(n) = name {
                    let n = tx.name(n).to_owned();
                    tx.cjs.void0.push(n);
                }
            }
            Node::Enum { name, .. } if mods.contains(Modifiers::EXPORT) => {
                if !mods.contains(Modifiers::CONST) || tx.opts.preserve_const_enums {
                    let n = tx.name(name).to_owned();
                    if !tx.cjs.void0.contains(&n) {
                        tx.cjs.void0.push(n);
                    }
                }
            }
            Node::Module { name, .. } if mods.contains(Modifiers::EXPORT) => {
                if crate::namespace::is_instantiated(tx, s) {
                    let mut first = name;
                    while let Node::QualifiedName { left, .. } = tx.n(first) {
                        first = left;
                    }
                    let n = tx.name(first).to_owned();
                    if !tx.cjs.void0.contains(&n) {
                        tx.cjs.void0.push(n);
                    }
                }
            }
            // `export import x = require("m")` is assigned in place without a `void 0`.
            Node::ImportEquals {
                type_only: false,
                name,
                module_ref,
                ..
            } if mods.contains(Modifiers::EXPORT)
                && tx.kind(module_ref) != NodeKind::ExternalModuleRef =>
            {
                let n = tx.name(name).to_owned();
                tx.cjs.void0.push(n);
            }
            _ => {}
        }
    }
}

/// Prepends `__esModule`, the `void 0` export list and hoisted function exports.
pub(crate) fn finish(tx: &mut Tx, out: Vec<NodeId>, runtime: Vec<NodeId>) -> Vec<NodeId> {
    let mut pre = Vec::new();
    // A JavaScript file that is a CommonJS module by its own syntax (`module.exports = `,
    // `require()`) and has no `import`/`export` gets no `__esModule` marker (observed).
    let cjs_js =
        !tx.ast.source_kind.is_typescript() && !tx.sc.has_module_syntax && tx.sc.has_cjs_syntax;
    if tx.is_module && !tx.cjs.export_assignment && !cjs_js {
        // Object.defineProperty(exports, "__esModule", { value: true });
        let e = tx.ident("exports", 0);
        let k = tx.string("__esModule", 0);
        let t = tx.bool_lit(true, 0);
        let p = tx.prop("value", t, 0);
        let o = tx.object(&[p], 0);
        let s = define_property(tx, e, k, o, 0);
        tx.cjs.esmodule_stmt = Some(s);
        pre.push(s);
    }
    let names = std::mem::take(&mut tx.cjs.void0);
    // tsc writes at most 50 names per statement.
    for chunk in names.chunks(50) {
        let s = void0_stmt(tx, chunk);
        tx.cjs.void0_stmt.get_or_insert(s);
        pre.push(s);
    }
    tx.cjs.void0_written = names;
    let hoisted = std::mem::take(&mut tx.cjs.hoisted);
    for (exported, local) in hoisted {
        let e = tx.ident("exports", 0);
        let target = tx.member_or_index(e, &exported, 0);
        let v = tx.ident(&local, 0);
        let a = tx.assign(target, v, 0);
        let s = tx.expr_stmt(a, 0);
        pre.push(s);
    }
    tx.cjs.helper_import_after = pre.last().copied();
    // They stand for no source statement (the one at offset 0 included).
    for &s in &pre {
        let none = ts_r_span::Span::new(ts_r_codegen::NO_MAP, ts_r_codegen::NO_MAP);
        tx.map_range(s, none);
    }
    pre.extend(runtime);
    pre.extend(out);
    pre.append(&mut tx.cjs.export_assignments);
    pre
}

/// All lowering passes may request helpers, so insert the import once they have finished.
pub(crate) fn finish_helper_import(tx: &mut Tx) {
    let Some(v) = tx.cjs.tslib else {
        return;
    };
    let root = tx.ast.root;
    let Node::Program { stmts } = tx.n(root) else {
        return;
    };
    let mut items = tx.items(stmts);
    let at = tx
        .cjs
        .helper_import_after
        .and_then(|s| items.iter().position(|&i| i == s).map(|i| i + 1))
        .unwrap_or_else(|| {
            items
                .iter()
                .take_while(|&&s| tx.kind(s) == NodeKind::Directive)
                .count()
        });
    let t = tx.string("tslib", 0);
    let r = tx.ident("require", 0);
    let c = tx.call(r, &[t], 0);
    let name = tx.ident_atom(v, 0);
    let d = tx.var(VarKind::Const, Modifiers::NONE, name, Some(c), 0);
    // It stands for no source statement.
    let none = ts_r_span::Span::new(ts_r_codegen::NO_MAP, ts_r_codegen::NO_MAP);
    tx.map_range(d, none);
    items.insert(at, d);
    let stmts = tx.new_list(&items);
    tx.set(root, Node::Program { stmts });
}

/// `exports.b = exports.a = void 0;` for `names` in source order.
pub(crate) fn void0_stmt(tx: &mut Tx, names: &[String]) -> NodeId {
    let mut value = tx.void0(0);
    for n in names {
        let e = tx.ident("exports", 0);
        let target = tx.member_or_index(e, n, 0);
        value = tx.assign(target, value, 0);
    }
    tx.expr_stmt(value, 0)
}

fn define_property(tx: &mut Tx, target: NodeId, key: NodeId, desc: NodeId, at: u32) -> NodeId {
    let f = tx.dotted("Object.defineProperty", at);
    let c = tx.call(f, &[target, key, desc], at);
    tx.expr_stmt(c, at)
}

/// `Object.defineProperty(exports, "name", { enumerable: true, get: function () { return v; } });`
fn export_getter(tx: &mut Tx, name: &str, value: NodeId, at: u32) -> NodeId {
    let e = tx.ident("exports", at);
    let k = tx.string(name, at);
    let t = tx.bool_lit(true, at);
    let en = tx.prop("enumerable", t, at);
    let r = tx.ret(Some(value), at);
    let f = tx.function_expr(None, &[], &[r], at);
    tx.mark_single_line_body(f);
    let g = tx.prop("get", f, at);
    let o = tx.object(&[en, g], at);
    define_property(tx, e, k, o, at)
}

/// `exports.name = value;`
pub(crate) fn export_assign(tx: &mut Tx, name: &str, value: NodeId, at: u32) -> NodeId {
    let e = tx.ident("exports", at);
    let target = tx.member_or_index(e, name, at);
    let a = tx.assign(target, value, at);
    tx.expr_stmt(a, at)
}

pub(crate) fn export_binding(
    tx: &mut Tx,
    local: &str,
    exported: &str,
    at: u32,
    out: &mut Vec<NodeId>,
) {
    if tx.cjs.export_assignment {
        // `export =` replaces the module object; tsc drops the other exports.
        return;
    }
    let v = tx.ident(local, at);
    let s = export_assign(tx, exported, v, at);
    out.push(s);
}

/// `mod_1` for a module specifier: the base name made into an identifier.
fn module_var(tx: &mut Tx, source: NodeId) -> Atom {
    let spec = tx.name(source).to_owned();
    let base = spec.rsplit('/').next().unwrap_or(&spec);
    let mut id = String::with_capacity(base.len() + 1);
    for (i, c) in base.chars().enumerate() {
        if i == 0 && c.is_ascii_digit() {
            id.push('_');
        }
        id.push(if c.is_ascii_alphanumeric() || c == '_' {
            c
        } else {
            '_'
        });
    }
    if id.is_empty() {
        id.push('_');
    }
    tx.fresh(&id, true)
}

fn require_call(tx: &mut Tx, source: NodeId, at: u32) -> NodeId {
    let r = tx.ident("require", at);
    let source = double_quoted(tx, source);
    tx.call(r, &[source], at)
}

/// tsc prints the specifier of a `require` it writes for an import in double quotes, however
/// the import quoted it (observed: `import x from 'y'` and `import x = require('y')` →
/// `require("y")`; `import('y')` keeps its quotes). A string literal becomes a synthesized
/// string with the same value; anything else is returned as is.
pub(crate) fn double_quoted(tx: &mut Tx, source: NodeId) -> NodeId {
    match tx.n(source) {
        Node::StringLit { value } => {
            let v = tx.ast.atom(value).to_owned();
            let at = tx.at(source);
            tx.string(&v, at)
        }
        _ => source,
    }
}

fn helper_call(tx: &mut Tx, h: Helper, arg: NodeId, at: u32) -> NodeId {
    let f = tx.helper_ref(h, at);
    tx.call(f, &[arg], at)
}

pub(crate) fn import_decl(tx: &mut Tx, id: NodeId, kept: &[NodeId], out: &mut Vec<NodeId>) {
    let Node::ImportDecl {
        has_clause, source, ..
    } = tx.n(id)
    else {
        return;
    };
    let at = tx.at(id);
    tx.rewrite_specifier(source);
    if !has_clause {
        let c = require_call(tx, source, at);
        let s = tx.expr_stmt(c, at);
        tx.ast.set_span(s, tx.span(id));
        out.push(s);
        return;
    }
    let mut default = None;
    let mut namespace = None;
    let mut named = Vec::new();
    for &s in kept {
        match tx.n(s) {
            Node::ImportDefaultSpecifier { local } => default = Some(local),
            Node::ImportNamespaceSpecifier { local } => namespace = Some(local),
            Node::ImportSpecifier {
                imported, local, ..
            } => {
                let imp = tx.name(imported.unwrap_or(local)).to_owned();
                if imp == "default" {
                    default = default.or(Some(local));
                    named.push((local, None));
                } else {
                    named.push((local, Some(imp)));
                }
            }
            _ => {}
        }
    }
    let req = require_call(tx, source, at);
    let mut decls = Vec::new();
    if let (Some(ns), None, true) = (namespace, default, named.is_empty()) {
        // import * as ns from "x"
        let v = helper_call(tx, Helper::ImportStar, req, at);
        decls.push((ns, v));
    } else {
        let module = match tx.cjs.module_vars.get(&id) {
            Some(&m) => m,
            None => module_var(tx, source),
        };
        // `{ default as x }` alone is a default import (observed: `__importDefault`).
        let star =
            namespace.is_some() || (default.is_some() && named.iter().any(|(_, n)| n.is_some()));
        let v = if star {
            helper_call(tx, Helper::ImportStar, req, at)
        } else if default.is_some() {
            helper_call(tx, Helper::ImportDefault, req, at)
        } else {
            req
        };
        let m = tx.ident_atom(module, at);
        decls.push((m, v));
        if let Some(ns) = namespace {
            let alias = tx.ident_atom(module, at);
            decls.push((ns, alias));
        }
        if let Some(d) = default
            && let Some(&b) = tx.sc.decl_of.get(&d)
        {
            tx.cjs.imports.insert(b, ImportRef::Default { module });
        }
        for (local, imp) in &named {
            if let (Some(imp), Some(&b)) = (imp, tx.sc.decl_of.get(local)) {
                tx.cjs.imports.insert(
                    b,
                    ImportRef::Named {
                        module,
                        name: imp.clone(),
                    },
                );
            }
        }
    }
    let mut ds = Vec::new();
    for (name, init) in decls {
        let d = tx.mk(
            Node::VarDeclarator {
                name,
                definite: false,
                type_ann: None,
                init: Some(init),
            },
            at,
        );
        ds.push(d);
    }
    let list = tx.new_list(&ds);
    let v = tx.mk(
        Node::VarDecl {
            modifiers: Modifiers::NONE,
            kind: VarKind::Const,
            decls: list,
        },
        at,
    );
    tx.ast.set_span(v, ts_r_span::Span::new(at, at));
    // Source maps (tsc's): the `require` statement maps to the import.
    let span = tx.span(id);
    tx.map_range(v, span);
    out.push(v);
    // Re-exported imports: getters right after the import.
    for &s in kept {
        let (local, namespace) = match tx.n(s) {
            Node::ImportNamespaceSpecifier { local } => (local, true),
            Node::ImportDefaultSpecifier { local } | Node::ImportSpecifier { local, .. } => {
                (local, false)
            }
            _ => continue,
        };
        let Some(&b) = tx.sc.decl_of.get(&local) else {
            continue;
        };
        let Some(names) = tx.cjs.import_exports.get(&b).cloned() else {
            continue;
        };
        for (n, span) in names {
            // A namespace object is assigned, not forwarded (observed: `exports.ns = ns;`;
            // source maps: `ns` after `exports.` maps to the export specifier, the value
            // not at all).
            let g = if namespace {
                let atom = tx.atom_of(local).unwrap_or(Atom::EMPTY);
                let value = tx.ident_atom(atom, at);
                let g = export_assign(tx, &n, value, at);
                if let Node::ExprStmt { expr } = tx.n(g)
                    && let Node::Assign { left, .. } = tx.n(expr)
                    && let Node::Member { prop, .. } = tx.n(left)
                {
                    tx.map_range(prop, span);
                }
                g
            } else {
                // Source maps (tsc's): the forwarded value maps to the import specifier.
                let value = import_value(tx, b, local, at);
                let spec = tx.span(s);
                tx.map_range(value, spec);
                export_getter(tx, &n, value, at)
            };
            out.push(g);
        }
    }
}

/// The expression an imported binding stands for (`d_1.a`, `e_1.default`, or the
/// namespace identifier).
fn import_value(tx: &mut Tx, b: BindingId, local: NodeId, at: u32) -> NodeId {
    match tx.cjs.imports.get(&b).cloned() {
        Some(ImportRef::Named { module, name }) => {
            let m = tx.ident_atom(module, at);
            tx.member_or_index(m, &name, at)
        }
        Some(ImportRef::Default { module }) => {
            let m = tx.ident_atom(module, at);
            tx.member(m, "default", at)
        }
        None => crate::builder::clone_node(tx.ast, local),
    }
}

/// Rewrites a module-level reference to an import or exported variable.
pub(crate) fn rewrite_ref(tx: &mut Tx, id: NodeId, b: BindingId) {
    let at = tx.at(id);
    if !tx.cjs.imports.contains_key(&b)
        && let Some((decl, source, imported)) = tx.cjs.later_imports.get(&b).cloned()
    {
        // Referenced (in a function) before its import statement: the import's module
        // variable is allocated now and used when the statement is reached.
        let module = match tx.cjs.module_vars.get(&decl) {
            Some(&m) => m,
            None => {
                tx.rewrite_specifier(source);
                let m = module_var(tx, source);
                tx.cjs.module_vars.insert(decl, m);
                m
            }
        };
        let r = match imported {
            Some(name) => ImportRef::Named { module, name },
            None => ImportRef::Default { module },
        };
        tx.cjs.imports.insert(b, r);
    }
    if tx.cjs.imports.contains_key(&b) {
        let local = id;
        let v = import_value(tx, b, local, at);
        replace(tx.ast, id, v);
        tx.cjs.detached.insert(id);
        return;
    }
    if tx.cjs.exported_vars.contains(&b) {
        let name = tx.name(id).to_owned();
        let e = tx.ident("exports", at);
        let m = tx.member_or_index(e, &name, at);
        // Source maps (tsc's): `x` in `exports.x` maps to the reference too (the member
        // does, through `Tx::ident_ref`).
        if let Node::Member { prop, .. } = tx.n(m) {
            let span = tx.span(id);
            tx.map_range(prop, span);
        }
        replace(tx.ast, id, m);
        tx.cjs.detached.insert(id);
    }
}

/// Locals exported by an `export { }` list that the assignment or update target `target`
/// (an identifier or a destructuring pattern) writes. Looked up before the target is
/// transformed.
pub(crate) fn live_exports_of(tx: &Tx, target: NodeId) -> Vec<BindingId> {
    let mut out = Vec::new();
    if tx.opts.is_cjs() && !tx.cjs.export_assignment && !tx.cjs.live_exports.is_empty() {
        collect_live_exports(tx, target, &mut out);
    }
    out
}

fn collect_live_exports(tx: &Tx, p: NodeId, out: &mut Vec<BindingId>) {
    match tx.n(p) {
        Node::Ident { .. } => {
            if let Some(b) = tx.sc.binding_of_ref(p)
                && tx.cjs.live_exports.contains_key(&b)
                && !out.contains(&b)
            {
                out.push(b);
            }
        }
        Node::ArrayLit { elements } => {
            for e in tx.items(elements) {
                collect_live_exports(tx, e, out);
            }
        }
        Node::ObjectLit { props } => {
            for pr in tx.items(props) {
                match tx.n(pr) {
                    Node::Property { value, .. } => collect_live_exports(tx, value, out),
                    Node::ShorthandProp { name, .. } => collect_live_exports(tx, name, out),
                    Node::Spread { arg } => collect_live_exports(tx, arg, out),
                    _ => {}
                }
            }
        }
        Node::Spread { arg } | Node::Paren { expr: arg } => collect_live_exports(tx, arg, out),
        Node::Assign { left, .. } => collect_live_exports(tx, left, out),
        _ => {}
    }
}

/// After `id`, an assignment or update writing the live-exported locals `bindings`, updates
/// their exports as tsc does: `exports.y = exports.x = x = 1`, `exports.x = ++x`, and for a
/// postfix update `exports.x = (x++, x)` (or `(exports.x = (_a = x++, x), _a)` when its
/// value is used). A destructuring assignment is followed by `exports.x = x` for each local.
pub(crate) fn update_live_exports(tx: &mut Tx, id: NodeId, bindings: &[BindingId]) {
    let at = tx.at(id);
    let used = tx.cjs.stmt_expr != Some(id);
    let inner = crate::builder::clone_node(tx.ast, id);
    let reexport = |tx: &mut Tx, b: BindingId, mut value: NodeId| {
        for a in tx.cjs.live_exports.get(&b).cloned().unwrap_or_default() {
            let e = tx.ident("exports", at);
            let target = tx.member_or_index(e, &a, at);
            value = tx.assign(target, value, at);
        }
        value
    };
    let simple = match tx.n(inner) {
        Node::Assign { left, .. } | Node::Update { arg: left, .. } => {
            tx.kind(left) == NodeKind::Ident
        }
        _ => false,
    };
    let value = if simple && let [b] = *bindings {
        match tx.n(inner) {
            Node::Update { prefix: false, .. } => {
                let temp = used.then(|| tx.temp());
                let first = match temp {
                    Some(t) => {
                        let t = tx.ident_atom(t, at);
                        tx.assign(t, inner, at)
                    }
                    None => inner,
                };
                let local = tx.ident_atom(tx.sc.binding(b).name, at);
                let s = tx.seq(&[first, local], at);
                let p = tx.paren(s, at);
                let value = reexport(tx, b, p);
                match temp {
                    Some(t) => {
                        let t = tx.ident_atom(t, at);
                        let s = tx.seq(&[value, t], at);
                        tx.paren(s, at)
                    }
                    None => value,
                }
            }
            _ => reexport(tx, b, inner),
        }
    } else {
        let temp = used.then(|| tx.temp());
        let mut parts = vec![match temp {
            Some(t) => {
                let t = tx.ident_atom(t, at);
                tx.assign(t, inner, at)
            }
            None => inner,
        }];
        for &b in bindings {
            let local = tx.ident_atom(tx.sc.binding(b).name, at);
            parts.push(reexport(tx, b, local));
        }
        if let Some(t) = temp {
            parts.push(tx.ident_atom(t, at));
        }
        let s = tx.seq(&parts, at);
        if used { tx.paren(s, at) } else { s }
    };
    replace(tx.ast, id, value);
}

pub(crate) fn export_all(tx: &mut Tx, id: NodeId, out: &mut Vec<NodeId>) {
    let Node::ExportAll {
        exported, source, ..
    } = tx.n(id)
    else {
        return;
    };
    let at = tx.at(id);
    tx.rewrite_specifier(source);
    let req = require_call(tx, source, at);
    let s = match exported {
        None => {
            let f = tx.helper_ref(Helper::ExportStar, at);
            let e = tx.ident("exports", at);
            let c = tx.call(f, &[req, e], at);
            tx.expr_stmt(c, at)
        }
        Some(name) => {
            let n = tx.name(name).to_owned();
            let v = helper_call(tx, Helper::ImportStar, req, at);
            let s = export_assign(tx, &n, v, at);
            // Source maps (tsc's): the `x` of `exports.x` maps to the name.
            if let Node::ExprStmt { expr } = tx.n(s)
                && let Node::Assign { left, .. } = tx.n(expr)
                && let Node::Member { prop, .. } = tx.n(left)
            {
                let name_span = tx.span(name);
                tx.map_range(prop, name_span);
            }
            s
        }
    };
    tx.ast.set_span(s, ts_r_span::Span::new(at, at));
    // Source maps (tsc's): `__exportStar(...)` and `exports.x = ...` map to the `export *`.
    let span = tx.span(id);
    tx.map_range(s, span);
    out.push(s);
}

pub(crate) fn export_default_expr(tx: &mut Tx, id: NodeId, out: &mut Vec<NodeId>) {
    let Node::ExportDefaultExpr { expr } = tx.n(id) else {
        return;
    };
    let at = tx.at(id);
    let s = export_assign(tx, "default", expr, at);
    tx.ast.set_span(s, ts_r_span::Span::new(at, at));
    out.push(s);
}

/// Top-level function and class declarations.
pub(crate) fn exported_function(tx: &mut Tx, id: NodeId, out: &mut Vec<NodeId>) {
    let n = tx.n(id);
    let mods = n.modifiers();
    if !mods.contains(Modifiers::EXPORT) {
        out.push(id);
        let names = decl_names(tx, id);
        local_exports(tx, &names, out);
        return;
    }
    let at = tx.at(id);
    match n {
        Node::FunctionDecl {
            flags,
            name,
            type_params,
            params,
            return_type,
            body,
            ..
        } => {
            let name = name.or_else(|| tx.cjs.default_name.map(|a| tx.ident_atom(a, at)));
            unmapped_name(tx, name);
            let mut m = mods;
            m.remove(Modifiers::EXPORT);
            m.remove(Modifiers::DEFAULT);
            tx.set(
                id,
                Node::FunctionDecl {
                    modifiers: m,
                    flags,
                    name,
                    type_params,
                    params,
                    return_type,
                    body,
                },
            );
            out.push(id);
        }
        Node::ClassDecl { .. } => {
            let Some(mut p) = crate::class::parts(tx, id) else {
                return;
            };
            if p.name.is_none() {
                p.name = tx.cjs.default_name.map(|a| tx.ident_atom(a, at));
            }
            p.modifiers.remove(Modifiers::EXPORT);
            p.modifiers.remove(Modifiers::DEFAULT);
            unmapped_name(tx, p.name);
            let local = p.name.map(|n| tx.name(n).to_owned()).unwrap_or_default();
            crate::class::set_parts(tx, id, p);
            out.push(id);
            let exported = if mods.contains(Modifiers::DEFAULT) {
                "default".to_owned()
            } else {
                local.clone()
            };
            export_binding(tx, &local, &exported, at, out);
        }
        _ => out.push(id),
    }
}

/// Source maps (tsc's): the name of an exported function or class tsc rewrote for CommonJS
/// has no segments (observed).
fn unmapped_name(tx: &mut Tx, name: Option<NodeId>) {
    if let Some(n) = name {
        let at = tx.span(n).start;
        tx.ast.set_span(n, ts_r_span::Span::new(at, at));
    }
}

fn decl_names(tx: &Tx, id: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    match tx.n(id) {
        Node::VarDecl { decls, .. } => {
            for &d in tx.ast.list(decls) {
                if let Node::VarDeclarator { name, .. } = tx.n(d) {
                    collect_pattern_idents(tx.ast, name, &mut out);
                }
            }
        }
        Node::ClassDecl { name: Some(n), .. }
        | Node::FunctionDecl { name: Some(n), .. }
        | Node::Enum { name: n, .. } => out.push(n),
        Node::Module { name, .. } => {
            let mut first = name;
            while let Node::QualifiedName { left, .. } = tx.n(first) {
                first = left;
            }
            out.push(first);
        }
        Node::ImportEquals { name, .. } => out.push(name),
        _ => {}
    }
    out
}

/// `exports.alias = local;` after a declaration exported by an `export { }` list.
fn local_exports(tx: &mut Tx, names: &[NodeId], out: &mut Vec<NodeId>) {
    for &n in names {
        let Some(&b) = tx.sc.decl_of.get(&n) else {
            continue;
        };
        if tx.sc.binding(b).kind == BindingKind::Function {
            continue;
        }
        let Some(aliases) = tx.cjs.local_exports.remove(&b) else {
            continue;
        };
        let local = tx.name(n).to_owned();
        let at = tx.at(n);
        for a in aliases {
            export_binding(tx, &local, &a, at, out);
        }
    }
}

/// The `export { name as alias }` aliases of the declaration named `name`, taken (the
/// caller emits them).
impl State {
    pub(crate) fn export_assignment(&self) -> bool {
        self.export_assignment
    }
}

/// The name given to an anonymous `export default` function or class (`default_1`).
pub(crate) fn default_name(tx: &Tx) -> Option<Atom> {
    tx.cjs.default_name
}

pub(crate) fn take_local_exports(tx: &mut Tx, name: NodeId) -> Vec<String> {
    tx.sc
        .decl_of
        .get(&name)
        .and_then(|b| tx.cjs.local_exports.remove(b))
        .unwrap_or_default()
}

/// Called after any other top-level declaration (enum, namespace, import alias).
pub(crate) fn after_decl(tx: &mut Tx, original_names: &[NodeId], out: &mut Vec<NodeId>) {
    local_exports(tx, original_names, out);
}

/// Names declared by a top-level statement (before it is transformed).
pub(crate) fn declared_names(tx: &Tx, id: NodeId) -> Vec<NodeId> {
    decl_names(tx, id)
}

/// Top-level variable statements: `export const a = 1` → `exports.a = 1;`.
pub(crate) fn exported_var(tx: &mut Tx, id: NodeId, out: &mut Vec<NodeId>) {
    let Node::VarDecl {
        modifiers, decls, ..
    } = tx.n(id)
    else {
        out.push(id);
        return;
    };
    if !modifiers.contains(Modifiers::EXPORT) {
        let names = decl_names(tx, id);
        out.push(id);
        local_exports(tx, &names, out);
        return;
    }
    let kind = match tx.n(id) {
        Node::VarDecl { kind, .. } => kind,
        _ => VarKind::Const,
    };
    let mut exprs = Vec::new();
    for d in tx.items(decls) {
        if let Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } = tx.n(d)
        {
            let at = tx.at(d);
            // Function and class initializers keep a local binding (for their `name`).
            if tx.kind(name) == NodeKind::Ident
                && matches!(
                    tx.kind(init),
                    NodeKind::FunctionExpr | NodeKind::Arrow | NodeKind::ClassExpr
                )
            {
                let local = crate::builder::clone_node(tx.ast, name);
                let v = tx.var(kind, Modifiers::NONE, local, Some(init), at);
                // It stands for the statement (its comments lead it); source maps (tsc's):
                // it maps from the `const` keyword to the statement's end.
                let stmt = tx.span(id);
                tx.ast
                    .set_span(v, ts_r_span::Span::new(stmt.start, stmt.start));
                let kw = match kind {
                    VarKind::Var => "var",
                    VarKind::Let => "let",
                    _ => "const",
                };
                if let Some(k) = tx
                    .src
                    .get(stmt.start as usize..at as usize)
                    .and_then(|t| t.rfind(kw))
                {
                    let k = stmt.start + k as u32;
                    tx.map_range(v, ts_r_span::Span::new(k, stmt.end));
                }
                out.push(v);
                let n = tx.name(name).to_owned();
                let name_span = tx.span(name);
                let e = tx.ident("exports", at);
                let target = tx.member_or_index(e, &n, at);
                let value = tx.ident(&n, at);
                // `exports.v = v`: the member, its name and the value map to the name.
                tx.map_range(target, name_span);
                if let Node::Member { prop, .. } = tx.n(target) {
                    tx.map_range(prop, name_span);
                }
                tx.map_range(value, name_span);
                let a = tx.assign(target, value, at);
                exprs.push(a);
                continue;
            }
            let range_start = tx.map_ranges.len();
            let target = crate::namespace::target_with(tx, name, &mut |tx, n, at| {
                let e = tx.ident("exports", at);
                tx.member_or_index(e, n, at)
            });
            // Source maps (tsc's): the `x` of `exports.x` maps to the name, as the member
            // does (`target_with`).
            let range_end = tx.map_ranges.len();
            for i in range_start..range_end {
                let (m, r) = tx.map_ranges[i];
                if let Node::Member { prop, .. } = tx.n(m) {
                    tx.map_range(prop, r);
                }
            }
            let a = tx.assign(target, init, at);
            // Its end is the declarator's (an erased `as T` included).
            let span = tx.span(d);
            tx.map_range(a, span);
            exprs.push(a);
        }
    }
    if exprs.is_empty() {
        tx.erase(id);
        return;
    }
    let at = tx.at(id);
    let e = tx.seq(&exprs, at);
    let s = tx.expr_stmt(e, at);
    tx.ast.set_span(s, ts_r_span::Span::new(at, at));
    let span = tx.span(id);
    tx.map_range(s, span);
    out.push(s);
}

pub(crate) fn export_named(tx: &mut Tx, id: NodeId, kept: &[NodeId], out: &mut Vec<NodeId>) {
    let Node::ExportNamed { source, .. } = tx.n(id) else {
        return;
    };
    let Some(source) = source else {
        // Local exports are emitted next to their declarations.
        tx.erase(id);
        return;
    };
    let at = tx.at(id);
    tx.rewrite_specifier(source);
    let module = module_var(tx, source);
    let req = require_call(tx, source, at);
    let m = tx.ident_atom(module, at);
    let v = tx.var(VarKind::Var, Modifiers::NONE, m, Some(req), at);
    tx.ast.set_span(v, ts_r_span::Span::new(at, at));
    // Source maps (tsc's): the `require` maps to the export declaration, each getter and
    // its value to the specifier.
    let span = tx.span(id);
    tx.map_range(v, span);
    out.push(v);
    for &s in kept {
        let Node::ExportSpecifier {
            local, exported, ..
        } = tx.n(s)
        else {
            continue;
        };
        let imported = tx.name(local).to_owned();
        let name = tx.name(exported.unwrap_or(local)).to_owned();
        let sat = tx.at(s);
        let value = if imported == "default" {
            let mm = tx.ident_atom(module, sat);
            let c = helper_call(tx, Helper::ImportDefault, mm, sat);
            tx.member(c, "default", sat)
        } else {
            let mm = tx.ident_atom(module, sat);
            tx.member_or_index(mm, &imported, sat)
        };
        // Source maps (tsc's): the value's name (`m_1.x`'s `x`) maps to the specifier.
        let spec = tx.span(s);
        match tx.n(value) {
            Node::Member { prop, .. } => tx.map_range(prop, spec),
            _ => tx.map_range(value, spec),
        }
        let g = export_getter(tx, &name, value, sat);
        tx.map_range(g, spec);
        out.push(g);
    }
}

/// `import("x")` → `Promise.resolve().then(() => __importStar(require("x")))`.
pub(crate) fn import_call(tx: &mut Tx, id: NodeId) {
    let Node::ImportCall { arg, .. } = tx.n(id) else {
        return;
    };
    let at = tx.at(id);
    let promise = tx.dotted("Promise.resolve", at);
    let literal = tx.kind(arg) == NodeKind::StringLit;
    let (resolved, param, req_arg) = if literal {
        let r = tx.call(promise, &[], at);
        (r, None, arg)
    } else {
        // Promise.resolve(`${arg}`).then(s => __importStar(require(s)))
        let s = tx.fresh("s", false);
        let empty = tx.ast.intern("");
        let e1 = tx.mk(
            Node::TemplateElement {
                cooked: empty,
                invalid_escape: false,
            },
            at,
        );
        let e2 = tx.mk(
            Node::TemplateElement {
                cooked: empty,
                invalid_escape: false,
            },
            at,
        );
        let parts = tx.new_list(&[e1, arg, e2]);
        let t = tx.mk(Node::TemplateLit { parts }, at);
        let r = tx.call(promise, &[t], at);
        let p = tx.ident_atom(s, at);
        let p = tx.param(p, at);
        let a = tx.ident_atom(s, at);
        (r, Some(p), a)
    };
    let r = tx.ident("require", at);
    let req = tx.call(r, &[req_arg], at);
    let body = helper_call(tx, Helper::ImportStar, req, at);
    let params: Vec<NodeId> = param.into_iter().collect();
    let arrow = tx.arrow_expr(&params, body, at);
    let then = tx.member(resolved, "then", at);
    let c = tx.call(then, &[arrow], at);
    replace(tx.ast, id, c);
}

/// The `createRequire` alias for `import x = require()` in Node ES modules:
/// `import { createRequire as _createRequire } from "module";
///  const __require = _createRequire(import.meta.url);`
pub(crate) fn create_require(tx: &mut Tx) -> Atom {
    if let Some(a) = tx.require_alias {
        return a;
    }
    let create = tx.fresh("_createRequire", false);
    let alias = tx.fresh("__require", false);
    let imported = tx.ident("createRequire", 0);
    let local = tx.ident_atom(create, 0);
    let spec = tx.mk(
        Node::ImportSpecifier {
            type_only: false,
            imported: Some(imported),
            local,
        },
        0,
    );
    let specifiers = tx.new_list(&[spec]);
    let source = tx.string("module", 0);
    let imp = tx.mk(
        Node::ImportDecl {
            type_only: false,
            phase: ts_r_ast::ImportPhase::Evaluation,
            has_clause: true,
            specifiers,
            source,
            attributes: None,
        },
        0,
    );
    let meta = tx.mk(
        Node::MetaProperty {
            kind: ts_r_ast::MetaPropKind::ImportMeta,
        },
        0,
    );
    let url = tx.member(meta, "url", 0);
    let f = tx.ident_atom(create, 0);
    let call = tx.call(f, &[url], 0);
    let name = tx.ident_atom(alias, 0);
    let v = tx.var(VarKind::Const, Modifiers::NONE, name, Some(call), 0);
    tx.top.push(imp);
    tx.top.push(v);
    tx.require_alias = Some(alias);
    alias
}

#[cfg(test)]
mod tests {
    use crate::{TransformOptions, transpile};

    /// tsc flattens a destructuring assignment to a live-exported local
    /// (`exports.x = x = [2][0]`); we keep it and re-export after it.
    #[test]
    fn destructuring_updates_live_exports() {
        let src = "let x = 0;\nexport { x, x as y };\n[x] = [2];\nconst v = ({ x } = { x: 3 });\n";
        let opts = TransformOptions::from_json(r#"{"module":"commonjs"}"#, "a.ts");
        let out = transpile("a.ts", src, &opts, &Default::default());
        assert!(
            out.code.contains("[x] = [2], exports.y = exports.x = x;"),
            "{}",
            out.code
        );
        assert!(
            out.code
                .contains("(_a = { x } = { x: 3 }, exports.y = exports.x = x, _a)"),
            "{}",
            out.code
        );
    }
}
