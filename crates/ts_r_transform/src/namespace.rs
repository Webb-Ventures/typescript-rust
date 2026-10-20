//! Namespaces (`namespace N {}` / `module N {}`) as tsc's IIFEs.
//!
//! ```js
//! var N;
//! (function (N) {
//!     N.a = 1;                       // export const a = 1;
//!     function f() { return N.a; }
//!     N.f = f;                       // export function f
//!     let M;
//!     (function (M) { })(M = N.M || (N.M = {}));
//! })(N || (N = {}));
//! ```
//! Exported variables live only on the namespace object, so references to them are rewritten
//! to `N.a` (see `Tx::ident_ref`). Non-instantiated namespaces (types only) are erased.

use ts_r_ast::{Atom, Modifiers, ModuleKind, Node, NodeId, NodeKind, VarKind};

use crate::builder::Build;
use crate::scope::{BindingId, BindingKind};
use crate::transform::{NsFrame, Tx};

pub(crate) fn module_decl(tx: &mut Tx, id: NodeId, out: &mut Vec<NodeId>) {
    let Node::Module {
        modifiers,
        kind,
        name,
        body,
    } = tx.n(id)
    else {
        return;
    };
    if kind == ModuleKind::Global
        || tx.kind(name) == NodeKind::StringLit
        || modifiers.contains(Modifiers::DECLARE)
        || !is_instantiated(tx, id)
    {
        tx.erase(id);
        return;
    }
    let Some(body) = body else {
        tx.erase(id);
        return;
    };
    let mut segs = Vec::new();
    segments(tx, name, &mut segs);
    let exported = modifiers.contains(Modifiers::EXPORT);
    let at = tx.at(id);
    let span = tx.span(id);
    let first = out.len();
    // Source maps: the IIFE's `}` is the namespace body's.
    let block = innermost_body(tx, body);
    emit(tx, &segs, 0, body, exported, at, (span, block), out);
    if let Some(&s) = out.get(first) {
        tx.ast
            .set_span(s, ts_r_span::Span::new(span.start, span.start));
    }
}

fn segments(tx: &Tx, name: NodeId, out: &mut Vec<NodeId>) {
    match tx.n(name) {
        Node::QualifiedName { left, right } => {
            segments(tx, left, out);
            out.push(right);
        }
        _ => out.push(name),
    }
}

/// The `{ }` block of a namespace body (`A.B.C` nests declarations).
fn innermost_body(tx: &Tx, mut body: NodeId) -> ts_r_span::Span {
    while let Node::Module { body: Some(b), .. } = tx.n(body) {
        body = b;
    }
    tx.span(body)
}

/// Emits `var N; (function (N) { ... })(N || (N = {}));` for segment `idx`. Source maps
/// (tsc's): the statements map to the declaration from the segment's name on, the names
/// to the segment, the body's `}` to `block`'s.
#[allow(clippy::too_many_arguments)]
fn emit(
    tx: &mut Tx,
    segs: &[NodeId],
    idx: usize,
    body: NodeId,
    exported: bool,
    at: u32,
    maps: (ts_r_span::Span, ts_r_span::Span),
    out: &mut Vec<NodeId>,
) {
    let block = maps.1;
    let seg = segs[idx];
    let name_span = tx.span(seg);
    let decl_span = {
        let whole = maps.0;
        let start = if idx == 0 {
            whole.start
        } else {
            name_span.start
        };
        ts_r_span::Span::new(start, whole.end)
    };
    let name_atom = tx.atom_of(seg).unwrap_or(Atom::EMPTY);
    let name = tx.ast.atom(name_atom).to_owned();
    let scope = tx.sc.ns_scope_of.get(&seg).copied().unwrap_or(0);
    let parent = tx.ns_stack.last().copied();
    let top_level = parent.is_none() && tx.list_depth == 0;

    // The variable, unless an earlier declaration (class, function, enum or the same
    // namespace) already made one.
    if idx > 0 || needs_var(tx, seg) {
        let kind = if idx > 0 || top_level {
            VarKind::Var
        } else {
            VarKind::Let
        };
        let mods = if idx == 0 && exported && top_level && !tx.opts.is_cjs() && parent.is_none() {
            Modifiers::EXPORT
        } else {
            Modifiers::NONE
        };
        let n = tx.ident_atom(name_atom, at);
        let v = tx.var(kind, mods, n, None, at);
        tx.map_range(n, name_span);
        tx.map_range(v, decl_span);
        out.push(v);
    }

    // Body.
    let param = name_atom;
    tx.ns_stack.push(NsFrame { scope, param });
    let stmts: Vec<NodeId> = if idx + 1 < segs.len() {
        let mut inner = Vec::new();
        emit(tx, segs, idx + 1, body, true, at, maps, &mut inner);
        inner
    } else {
        let Node::ModuleBlock { stmts } = tx.n(body) else {
            tx.ns_stack.pop();
            return;
        };
        // tsc drops the comments on the body's `{` line (`namespace N { // c`): the brace
        // counts as erased code, and those comments go with it.
        let open = tx.span(body).start;
        if tx.src.as_bytes().get(open as usize) == Some(&b'{') {
            tx.erased.push(ts_r_span::Span::new(open, open + 1));
        }
        let saved = tx.list_depth;
        tx.list_depth += 1;
        tx.temps.push(Vec::new());
        let mut v = Vec::new();
        for s in tx.items(stmts) {
            tx.stmt(s, &mut v);
        }
        let temps = tx.temps.pop().unwrap_or_default();
        if let Some(t) = temps_decl(tx, temps) {
            v.insert(0, t);
        }
        tx.list_depth = saved;
        v
    };
    tx.ns_stack.pop();

    // Argument: `N || (N = {})`, `N = P.N || (P.N = {})`, `N || (exports.N = N = {})`.
    let arg = if let Some(p) = parent.filter(|_| exported || idx > 0) {
        let left = tx.ident_atom(name_atom, at);
        let pobj = tx.ident_atom(p.param, at);
        let pm = tx.member(pobj, &name, at);
        let pobj2 = tx.ident_atom(p.param, at);
        let pm2 = tx.member(pobj2, &name, at);
        let empty = tx.object(&[], at);
        let a = tx.assign(pm2, empty, at);
        let pa = tx.paren(a, at);
        let or = tx.binary(pm, ts_r_ast::BinaryOp::LogicalOr, pa, at);
        map_names(tx, &[left, pm, pm2], name_span);
        tx.assign(left, or, at)
    } else {
        let l = tx.ident_atom(name_atom, at);
        let empty = tx.object(&[], at);
        let n2 = tx.ident_atom(name_atom, at);
        let mut rhs = tx.assign(n2, empty, at);
        if exported && top_level && tx.opts.is_cjs() {
            let e = tx.ident("exports", at);
            let em = tx.member(e, &name, at);
            if let Node::Member { prop, .. } = tx.n(em) {
                tx.map_range(prop, name_span);
            }
            rhs = tx.assign(em, rhs, at);
        }
        let pa = tx.paren(rhs, at);
        map_names(tx, &[l, n2], name_span);
        tx.binary(l, ts_r_ast::BinaryOp::LogicalOr, pa, at)
    };
    let iife = iife(tx, param, &stmts, arg, at);
    map_iife(tx, iife, decl_span, name_span, block);
    out.push(iife);
}

/// `(function (param) { stmts })(arg);`
pub(crate) fn iife(tx: &mut Tx, param: Atom, stmts: &[NodeId], arg: NodeId, at: u32) -> NodeId {
    let p = tx.ident_atom(param, at);
    let p = tx.param(p, at);
    let f = tx.function_expr(None, &[p], stmts, at);
    let pf = tx.paren(f, at);
    let call = tx.call(pf, &[arg], at);
    tx.expr_stmt(call, at)
}

/// Source maps (tsc's): the names in an IIFE argument (`N`, `P.N`) map to the declared name.
pub(crate) fn map_names(tx: &mut Tx, ids: &[NodeId], name: ts_r_span::Span) {
    for &id in ids {
        tx.map_range(id, name);
        if let Node::Member { prop, .. } = tx.n(id) {
            tx.map_range(prop, name);
        }
    }
}

/// Source maps (tsc's) for an [`iife`]: the statement maps to the declaration, the
/// parameter to its name and the body's `}` to the declaration's.
pub(crate) fn map_iife(
    tx: &mut Tx,
    stmt: NodeId,
    decl: ts_r_span::Span,
    name: ts_r_span::Span,
    block: ts_r_span::Span,
) {
    tx.map_range(stmt, decl);
    let Node::ExprStmt { expr: call } = tx.n(stmt) else {
        return;
    };
    let Node::Call { callee, .. } = tx.n(call) else {
        return;
    };
    let Node::Paren { expr: f } = tx.n(callee) else {
        return;
    };
    let Node::FunctionExpr {
        params,
        body: Some(body),
        ..
    } = tx.n(f)
    else {
        return;
    };
    if let Some(&p) = tx.ast.list(params).first()
        && let Node::Param { pattern, .. } = tx.n(p)
    {
        tx.map_range(pattern, name);
    }
    tx.map_range(body, block);
}

fn temps_decl(tx: &mut Tx, temps: Vec<Atom>) -> Option<NodeId> {
    if temps.is_empty() {
        return None;
    }
    let mut decls = Vec::new();
    for t in temps {
        let name = tx.ident_atom(t, 0);
        let d = tx.mk(
            Node::VarDeclarator {
                name,
                definite: false,
                type_ann: None,
                init: None,
            },
            0,
        );
        decls.push(d);
    }
    let decls = tx.new_list(&decls);
    Some(tx.mk(
        Node::VarDecl {
            modifiers: Modifiers::NONE,
            kind: VarKind::Var,
            decls,
        },
        0,
    ))
}

/// True unless the name was already declared by a class, function, enum or an earlier
/// declaration of the same namespace.
fn needs_var(tx: &mut Tx, seg: NodeId) -> bool {
    let Some(&b) = tx.sc.decl_of.get(&seg) else {
        return true;
    };
    let bnd = tx.sc.binding(b);
    match bnd.kind {
        BindingKind::Class | BindingKind::Function | BindingKind::Enum => false,
        _ => tx.ns_declared.insert(b),
    }
}

/// The IIFE parameter for a namespace body scope on the stack.
pub(crate) fn param_for_scope(tx: &Tx, scope: u32) -> Option<Atom> {
    tx.ns_stack
        .iter()
        .rev()
        .find(|f| f.scope == scope)
        .map(|f| f.param)
}

/// `function f() {}` + `N.f = f;` (also classes).
pub(crate) fn exported_decl(tx: &mut Tx, id: NodeId, name: Option<NodeId>, out: &mut Vec<NodeId>) {
    // Drop `export` from the declaration itself.
    match tx.n(id) {
        Node::FunctionDecl {
            modifiers,
            flags,
            name,
            type_params,
            params,
            return_type,
            body,
        } => {
            let mut m = modifiers;
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
        }
        Node::ClassDecl { .. } => {
            if let Some(mut p) = crate::class::parts(tx, id) {
                p.modifiers.remove(Modifiers::EXPORT);
                p.modifiers.remove(Modifiers::DEFAULT);
                crate::class::set_parts(tx, id, p);
            }
        }
        _ => {}
    }
    out.push(id);
    if let Some(n) = name {
        let local = tx.name(n).to_owned();
        let at = tx.at(id);
        let len = out.len();
        export_binding(tx, &local, &local, at, out);
        // Source maps (tsc's): `N.f = f;` maps to the declaration, `N.f` to its name.
        if out.len() > len
            && let Some(&s) = out.last()
            && let Node::ExprStmt { expr: a } = tx.n(s)
            && let Node::Assign { left: target, .. } = tx.n(a)
        {
            let decl = tx.span(id);
            tx.map_range(s, decl);
            tx.map_range(a, decl);
            let name = tx.span(n);
            map_names(tx, &[target], name);
        }
    }
}

/// `N.exported = local;`
pub(crate) fn export_binding(
    tx: &mut Tx,
    local: &str,
    exported: &str,
    at: u32,
    out: &mut Vec<NodeId>,
) {
    let Some(frame) = tx.ns_stack.last().copied() else {
        return;
    };
    let obj = tx.ident_atom(frame.param, at);
    let target = tx.member_or_index(obj, exported, at);
    let value = tx.ident(local, at);
    let a = tx.assign(target, value, at);
    let s = tx.expr_stmt(a, at);
    out.push(s);
}

/// `export const a = 1, b = f();` → `N.a = 1, N.b = f();` (uninitialized names vanish).
pub(crate) fn exported_var(tx: &mut Tx, id: NodeId, out: &mut Vec<NodeId>) {
    let Node::VarDecl { decls, .. } = tx.n(id) else {
        out.push(id);
        return;
    };
    let Some(frame) = tx.ns_stack.last().copied() else {
        out.push(id);
        return;
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
            let target = pattern_to_target(tx, name, frame.param);
            let a = tx.assign(target, init, at);
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
    tx.ast.set_span(s, tx.span(id));
    out.push(s);
}

/// A binding pattern as an assignment target whose names are properties of `obj`.
pub(crate) fn pattern_to_target(tx: &mut Tx, pat: NodeId, obj: Atom) -> NodeId {
    target_with(tx, pat, &mut |tx, name, at| {
        let o = tx.ident_atom(obj, at);
        tx.member(o, name, at)
    })
}

/// A binding pattern as an assignment target; identifiers become `leaf(name)`.
pub(crate) fn target_with(
    tx: &mut Tx,
    pat: NodeId,
    leaf: &mut dyn FnMut(&mut Tx, &str, u32) -> NodeId,
) -> NodeId {
    let at = tx.at(pat);
    match tx.n(pat) {
        Node::Ident { name } => {
            let n = tx.ast.atom(name).to_owned();
            let t = leaf(tx, &n, at);
            // Source maps (tsc's): `N.x` maps to the name it replaces.
            let span = tx.span(pat);
            tx.map_range(t, span);
            t
        }
        Node::ObjectPattern { props } => {
            let mut out = Vec::new();
            for p in tx.items(props) {
                let pa = tx.at(p);
                match tx.n(p) {
                    Node::BindingProperty { key, value, init } => {
                        let (k, v) = match value {
                            Some(v) => (key, target_with(tx, v, leaf)),
                            None => {
                                let n = tx.name(key).to_owned();
                                let k = crate::builder::clone_node(tx.ast, key);
                                let t = leaf(tx, &n, pa);
                                let span = tx.span(key);
                                tx.map_range(t, span);
                                (k, t)
                            }
                        };
                        let v = match init {
                            Some(i) => tx.assign(v, i, pa),
                            None => v,
                        };
                        let prop = tx.prop_node(k, v, pa);
                        out.push(prop);
                    }
                    Node::Rest { arg } => {
                        let t = target_with(tx, arg, leaf);
                        let s = tx.spread(t, pa);
                        out.push(s);
                    }
                    _ => {}
                }
            }
            let o = tx.object(&out, at);
            let span = tx.span(pat);
            tx.map_range(o, span);
            o
        }
        Node::ArrayPattern { elements } => {
            let mut out = Vec::new();
            for e in tx.items(elements) {
                let t = if tx.kind(e) == NodeKind::Hole {
                    e
                } else {
                    target_with(tx, e, leaf)
                };
                out.push(t);
            }
            let a = tx.array(&out, at);
            let span = tx.span(pat);
            tx.map_range(a, span);
            a
        }
        Node::AssignPattern { left, right } => {
            let l = target_with(tx, left, leaf);
            tx.assign(l, right, at)
        }
        Node::Rest { arg } => {
            let t = target_with(tx, arg, leaf);
            tx.spread(t, at)
        }
        _ => pat,
    }
}

/// True if the namespace declaration contains runtime code.
pub(crate) fn is_instantiated(tx: &Tx, id: NodeId) -> bool {
    let Node::Module {
        modifiers,
        kind,
        name,
        body,
    } = tx.n(id)
    else {
        return false;
    };
    if modifiers.contains(Modifiers::DECLARE)
        || kind == ModuleKind::Global
        || tx.kind(name) == NodeKind::StringLit
    {
        return false;
    }
    let Some(body) = body else {
        return false;
    };
    let Node::ModuleBlock { stmts } = tx.n(body) else {
        return false;
    };
    tx.ast.list(stmts).iter().any(|&s| stmt_instantiates(tx, s))
}

fn stmt_instantiates(tx: &Tx, s: NodeId) -> bool {
    let n = tx.n(s);
    if n.modifiers().contains(Modifiers::DECLARE) {
        return false;
    }
    match n {
        Node::Interface { .. } | Node::TypeAlias { .. } | Node::Empty {} => false,
        Node::FunctionDecl { body, .. } => body.is_some(),
        Node::Enum { modifiers, .. } => {
            !modifiers.contains(Modifiers::CONST) || tx.opts.preserve_const_enums
        }
        Node::Module { .. } => is_instantiated(tx, s),
        Node::ImportEquals {
            type_only,
            modifiers,
            ..
        } => !type_only && modifiers.contains(Modifiers::EXPORT),
        Node::ExportNamed { type_only, .. } => !type_only,
        _ => true,
    }
}

/// True if any declaration of namespace binding `b` is instantiated.
pub(crate) fn is_instantiated_binding(tx: &Tx, b: BindingId) -> bool {
    let Node::Program { stmts } = tx.n(tx.ast.root) else {
        return true;
    };
    let mut found = false;
    for &s in tx.ast.list(stmts) {
        if let Node::Module { name, .. } = tx.n(s) {
            let mut first = name;
            while let Node::QualifiedName { left, .. } = tx.n(first) {
                first = left;
            }
            if tx.sc.decl_of.get(&first) == Some(&b) {
                found = true;
                if is_instantiated(tx, s) {
                    return true;
                }
            }
        }
    }
    !found
}
