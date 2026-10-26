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

use rustc_hash::FxHashMap;
use ts_r_ast::{Atom, Modifiers, ModuleKind, Node, NodeId, NodeKind, VarKind};

use crate::builder::Build;
use crate::scope::{BindingId, BindingKind, ScopeKind};
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
    let param = param_name(tx, scope, name_atom, &name);
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
        } else if top_level && idx == 0 && tx.opts.is_cjs() {
            for alias in crate::cjs::list_aliases(tx, name_atom) {
                let e = tx.ident("exports", at);
                let em = tx.member_or_index(e, &alias, at);
                rhs = tx.assign(em, rhs, at);
            }
        }
        let pa = tx.paren(rhs, at);
        map_names(tx, &[l, n2], name_span);
        tx.binary(l, ts_r_ast::BinaryOp::LogicalOr, pa, at)
    };
    let iife = iife(tx, param, &stmts, arg, at);
    map_iife(tx, iife, decl_span, name_span, block);
    out.push(iife);
}

/// The IIFE parameter: the namespace's name, or a unique `N_1` when a value named `N` is
/// declared anywhere inside the body (any nested scope: `const N`, a parameter, a catch
/// variable, an inner namespace `N`, an `import N =` alias; not types, enum members or
/// names merged from another declaration). Observed, tsc 7.0.2
/// (`exportDeclarationForModuleOrEnumWithMemberOfSameName`); keeping `N` would make
/// `(function (N) { const N = 0; })` a SyntaxError.
fn param_name(tx: &mut Tx, scope: u32, name_atom: Atom, name: &str) -> Atom {
    if scope == 0 || name_atom == Atom::EMPTY {
        return name_atom;
    }
    if tx.value_bindings_by_name.is_none() {
        let mut by_name: FxHashMap<Atom, Vec<BindingId>> = FxHashMap::default();
        for (i, b) in tx.sc.bindings.iter().enumerate() {
            if !matches!(
                b.kind,
                BindingKind::TypeImport | BindingKind::EnumMember | BindingKind::MergedNsExport
            ) {
                by_name.entry(b.name).or_default().push(i as BindingId);
            }
        }
        tx.value_bindings_by_name = Some(by_name);
    }
    let candidates = tx
        .value_bindings_by_name
        .as_ref()
        .and_then(|m| m.get(&name_atom))
        .cloned()
        .unwrap_or_default();
    let declared = candidates.into_iter().any(|b| {
        let binding = &tx.sc.bindings[b as usize];
        if binding.kind == BindingKind::Namespace && !is_instantiated_ns(tx, b) {
            return false;
        }
        // Inside the body: the binding's scope is `scope` or nested in it.
        let mut s = Some(binding.scope);
        let mut steps = 0usize;
        while let Some(cur) = s {
            if cur == scope {
                return true;
            }
            steps += 1;
            if steps > tx.sc.scopes.len() {
                return false;
            }
            s = tx.sc.scopes.get(cur as usize).and_then(|sc| sc.parent);
        }
        false
    });
    if declared {
        tx.fresh(name, true)
    } else {
        name_atom
    }
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
    first_declaration(tx, b)
}

/// Whether a namespace or enum of binding `b` must declare its variable: no earlier class,
/// function or variable (not a `declare var`, which writes nothing) supplies the name, and
/// no namespace or enum declared it already (observed: `class C {} enum C {}` and `var x;
/// namespace x {}` write no `var`). The binding's kind is that of its first declaration.
pub(crate) fn first_declaration(tx: &mut Tx, b: BindingId) -> bool {
    let bnd = tx.sc.binding(b);
    match bnd.kind {
        BindingKind::Class | BindingKind::Function => false,
        BindingKind::Var | BindingKind::Let | BindingKind::Const
            if !tx.sc.ambient_vars.contains(&bnd.decl) =>
        {
            false
        }
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
    let declare = matches!(tx.n(id), Node::Module { modifiers, .. } if modifiers.contains(Modifiers::DECLARE));
    !declare && body_instantiates(tx, id)
}

/// True if the namespace's body has a value declaration, `declare` ones included: tsc
/// gives `namespace N { export declare const x: T }` its (empty) IIFE (tsc:
/// tsc/internal/ast/utilities.go, getModuleInstanceStateWorker).
fn body_instantiates(tx: &Tx, id: NodeId) -> bool {
    let Node::Module {
        modifiers,
        kind,
        name,
        body,
    } = tx.n(id)
    else {
        return false;
    };
    let _ = modifiers;
    if kind == ModuleKind::Global || tx.kind(name) == NodeKind::StringLit {
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
    let declare = n.modifiers().contains(Modifiers::DECLARE);
    match n {
        Node::Interface { .. } | Node::TypeAlias { .. } | Node::Empty {} => false,
        Node::FunctionDecl { body, .. } => declare || body.is_some(),
        Node::Enum { modifiers, .. } => {
            !modifiers.contains(Modifiers::CONST) || tx.opts.preserve_const_enums
        }
        Node::Module { .. } => body_instantiates(tx, s),
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
/// Whether namespace binding `b`, declared at any depth, has an instantiated declaration
/// (a namespace with only types or empty namespaces gets no IIFE and no runtime name).
fn is_instantiated_ns(tx: &Tx, b: BindingId) -> bool {
    let mut found = false;
    for (&segment, &scope) in &tx.sc.ns_scope_of {
        if tx.sc.decl_of.get(&segment) != Some(&b) {
            continue;
        }
        if let Some(ScopeKind::Namespace(m)) = tx.sc.scopes.get(scope as usize).map(|s| s.kind) {
            found = true;
            if is_instantiated(tx, m) {
                return true;
            }
        }
    }
    !found
}

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

/// Whether the entity name of `import x = A.B.C` (`entity`) names a value, as far as the
/// file tells: `Some(false)` when it resolves through this file's namespaces to a type
/// (an interface, a type alias, a namespace with no values, a const enum), `Some(true)`
/// for a value, `None` when the file does not declare it. tsc keeps an unused top-level
/// alias of a script whose target is a value or unresolved (observed with tsc 7.0.2:
/// `import v = foo.v` stays as `var v = foo.v;`, `import t = foo.onlyTypes` goes).
pub(crate) fn entity_is_value(tx: &Tx, entity: NodeId) -> Option<bool> {
    let mut path = Vec::new();
    let mut n = entity;
    while let Node::QualifiedName { left, right } = tx.n(n) {
        path.push(right);
        n = left;
    }
    let Node::Ident { name: root } = tx.n(n) else {
        return None;
    };
    path.reverse();
    let Node::Program { stmts } = tx.n(tx.ast.root) else {
        return None;
    };
    let top: Vec<NodeId> = tx.ast.list(stmts).to_vec();
    member_is_value(tx, &top, root, &path, 0, false)
}

/// [`entity_is_value`] with the root resolved by scope analysis: a name declared in an
/// enclosing namespace body is looked up among that body's statements
/// (`namespace M { namespace N {} export import X = N; }`).
pub(crate) fn alias_target_is_value(tx: &Tx, entity: NodeId) -> Option<bool> {
    let mut path = Vec::new();
    let mut n = entity;
    while let Node::QualifiedName { left, right } = tx.n(n) {
        path.push(right);
        n = left;
    }
    let Node::Ident { name: root } = tx.n(n) else {
        return None;
    };
    path.reverse();
    let Some(b) = tx.sc.binding_of_ref(n) else {
        return entity_is_value(tx, entity);
    };
    let scope = tx.sc.binding(b).scope;
    match tx.sc.scopes[scope as usize].kind {
        crate::scope::ScopeKind::Module => entity_is_value(tx, entity),
        crate::scope::ScopeKind::Namespace(m) => {
            let Node::Module {
                name,
                body: Some(body),
                ..
            } = tx.n(m)
            else {
                return None;
            };
            // Only the last segment's scope (`namespace A.B`) holds the block's statements.
            let last = match tx.n(name) {
                Node::QualifiedName { right, .. } => right,
                _ => name,
            };
            if tx.sc.ns_scope_of.get(&last) != Some(&scope) {
                return None;
            }
            let Node::ModuleBlock { stmts } = tx.n(body) else {
                return None;
            };
            let stmts: Vec<NodeId> = tx.ast.list(stmts).to_vec();
            member_is_value(tx, &stmts, root, &path, 0, false)
        }
        _ => None,
    }
}

/// `export = X;` where this file declares `X` only as a type (an interface, a type alias, a
/// namespace of types): tsc writes nothing for it, and the module gets the `__esModule`
/// marker as if it had no export assignment.
pub(crate) fn export_assignment_elided(tx: &Tx, stmt: NodeId) -> bool {
    let Node::ExportAssignment { expr } = tx.n(stmt) else {
        return false;
    };
    // verbatimModuleSyntax keeps it as written (observed).
    !tx.opts.verbatim_module_syntax
        && tx.ast.source_kind.is_typescript()
        && matches!(tx.n(expr), Node::Ident { .. })
        && entity_is_value(tx, expr) == Some(false)
}

/// Whether `name` (then `rest`) declared among `stmts` is a value. `ambient`: the
/// statements are a `declare namespace`'s body, where every member is exported.
fn member_is_value(
    tx: &Tx,
    stmts: &[NodeId],
    name: Atom,
    rest: &[NodeId],
    depth: u32,
    ambient: bool,
) -> Option<bool> {
    if depth > 64 {
        return None;
    }
    let mut namespaces = Vec::new();
    let mut found_type = false;
    for &s in stmts {
        let n = tx.n(s);
        let mods = n.modifiers();
        let visible = depth == 0 || ambient || mods.contains(Modifiers::EXPORT);
        if !visible {
            continue;
        }
        let named = |id: Option<NodeId>| {
            id.is_some_and(|id| matches!(tx.n(id), Node::Ident { name: a } if a == name))
        };
        match n {
            Node::VarDecl { decls, .. } => {
                let declares = tx.ast.list(decls).iter().any(
                    |&d| matches!(tx.n(d), Node::VarDeclarator { name: p, .. } if named(Some(p))),
                );
                if declares {
                    return rest.is_empty().then_some(true);
                }
            }
            Node::FunctionDecl { name: nm, .. } | Node::ClassDecl { name: nm, .. } if named(nm) => {
                return rest.is_empty().then_some(true);
            }
            Node::Enum {
                modifiers,
                name: nm,
                ..
            } if named(Some(nm)) => {
                if !rest.is_empty() {
                    return None;
                }
                if !modifiers.contains(Modifiers::CONST) || tx.opts.preserve_const_enums {
                    return Some(true);
                }
                found_type = true;
            }
            Node::ImportEquals { name: nm, .. } if named(Some(nm)) => return None,
            Node::ImportDecl { specifiers, .. }
                if tx.ast.list(specifiers).iter().any(|&sp| match tx.n(sp) {
                    Node::ImportSpecifier { local, .. }
                    | Node::ImportDefaultSpecifier { local }
                    | Node::ImportNamespaceSpecifier { local } => named(Some(local)),
                    _ => false,
                }) =>
            {
                return None;
            }
            Node::Interface { name: nm, .. } | Node::TypeAlias { name: nm, .. }
                if named(Some(nm)) =>
            {
                found_type = true;
            }
            Node::Module {
                modifiers,
                kind,
                name: nm,
                body,
            } if kind != ModuleKind::Global && tx.kind(nm) != NodeKind::StringLit => {
                let mut segs = Vec::new();
                let mut first = nm;
                while let Node::QualifiedName { left, right } = tx.n(first) {
                    segs.push(right);
                    first = left;
                }
                segs.push(first);
                segs.reverse();
                if !named(Some(segs[0])) {
                    continue;
                }
                namespaces.push((
                    s,
                    segs,
                    modifiers.contains(Modifiers::DECLARE) || ambient,
                    body,
                ));
            }
            _ => {}
        }
    }
    if namespaces.is_empty() {
        return if found_type { Some(false) } else { None };
    }
    // A namespace `name`: walk its segments (`namespace A.B {}` declares `A.B`) into the
    // bodies, or answer for the namespace itself.
    let mut any_unknown = false;
    for (_, segs, amb, body) in &namespaces {
        let mut i = 1;
        while i < segs.len()
            && rest.get(i - 1).is_some_and(|&r| {
                ident_atom(tx, r).is_some() && ident_atom(tx, r) == ident_atom(tx, segs[i])
            })
        {
            i += 1;
        }
        let Some(body) = body else { continue };
        let Node::ModuleBlock { stmts } = tx.n(*body) else {
            continue;
        };
        let inner: Vec<NodeId> = tx.ast.list(stmts).to_vec();
        if i < segs.len() {
            // `rest` ends inside the segments (`A.B` of `namespace A.B.C`): `A.B` holds
            // namespace `C`, so it is a value when this body is.
            if rest.len() < i {
                if body_has_value(tx, &inner, depth) {
                    return Some(true);
                }
                continue;
            }
            // The path leaves the segments: this declaration does not declare it.
            continue;
        }
        let rest_here = &rest[i - 1..];
        match rest_here.split_first() {
            None => {
                if body_has_value(tx, &inner, depth) {
                    return Some(true);
                }
            }
            Some((&next, more)) => {
                let Some(next) = ident_atom(tx, next) else {
                    any_unknown = true;
                    continue;
                };
                match member_is_value(tx, &inner, next, more, depth + 1, *amb) {
                    Some(true) => return Some(true),
                    Some(false) => {}
                    None => any_unknown = true,
                }
            }
        }
    }
    if any_unknown { None } else { Some(false) }
}

/// Whether a namespace body declares a value (tsc's instantiated namespace; `declare`
/// does not matter: an ambient namespace with a `const` is a value).
fn body_has_value(tx: &Tx, stmts: &[NodeId], depth: u32) -> bool {
    if depth > 64 {
        return true;
    }
    stmts.iter().any(|&s| match tx.n(s) {
        Node::Interface { .. } | Node::TypeAlias { .. } | Node::Empty {} => false,
        Node::Enum { modifiers, .. } => {
            !modifiers.contains(Modifiers::CONST) || tx.opts.preserve_const_enums
        }
        Node::Module {
            kind, name, body, ..
        } => {
            if kind == ModuleKind::Global || tx.kind(name) == NodeKind::StringLit {
                return false;
            }
            match body.map(|b| tx.n(b)) {
                Some(Node::ModuleBlock { stmts }) => {
                    let inner: Vec<NodeId> = tx.ast.list(stmts).to_vec();
                    body_has_value(tx, &inner, depth + 1)
                }
                _ => false,
            }
        }
        Node::ImportEquals { type_only, .. } => !type_only,
        Node::ExportNamed { type_only, .. } => !type_only,
        _ => true,
    })
}

fn ident_atom(tx: &Tx, id: NodeId) -> Option<Atom> {
    match tx.n(id) {
        Node::Ident { name } => Some(name),
        _ => None,
    }
}
