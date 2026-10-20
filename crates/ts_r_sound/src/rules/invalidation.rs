//! S-09: narrowings dropped at effect points.
//!
//! A property reference (`o.p`, `this.p`, `o.p.q`, `o["p"]`) whose flow type is narrower than
//! its declared type is reported (TSR0901) when an effect point (a call or `new` not on the
//! safe list, `await`, `yield`, an assignment to any property or element, `delete`) lies
//! between the narrowing and the use, and the use relies on the narrowing: with the declared
//! type it would fail (a member read on a nullable value, a missing member, a typed target
//! the declared type is not assignable to). A local `let`/`var`/parameter assigned inside
//! another function is reported the same way (TSR0902), with calls, `await` and `yield` as
//! effect points. A reference whose every property is `readonly` is exempt.
//!
//! The narrowing is the nearest enclosing `if`/`?:`/`&&`/`||` test that mentions the
//! reference, an earlier `if (...) return;`-style guard statement that mentions it, or an
//! earlier assignment to it. Effects are taken from the source range between that point and
//! the use (outside nested function bodies), so an effect on another branch that exits is
//! still counted (a conservative over-approximation).

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{AssignOp, BinaryOp, Node, NodeId, UnaryOp};
use ts_r_binder::meaning;
use ts_r_span::Span;
use ts_r_types::{SymId, TypeFlags, TypeId};

use crate::codes;
use crate::ctx::Ctx;

/// Callees that cannot run user code or mutate objects (`Object.prototype`-free statics).
const SAFE_CALLEES: &[&str] = &[
    "Object.is",
    "Object.keys",
    "Array.isArray",
    "Number.isFinite",
    "Number.isInteger",
    "Number.isNaN",
    "Number.isSafeInteger",
    "Number.parseFloat",
    "Number.parseInt",
    "isNaN",
    "isFinite",
    "parseInt",
    "parseFloat",
    "encodeURIComponent",
    "decodeURIComponent",
    "encodeURI",
    "decodeURI",
];

/// Namespaces whose every function is safe.
const SAFE_NAMESPACES: &[&str] = &["Math"];

struct Effect {
    span: Span,
    text: String,
}

pub fn run(cx: &mut Ctx<'_, '_, '_>) {
    if !cx.has_rule(9) {
        return;
    }
    let nodes = cx.nodes.clone();
    let mut captured: FxHashMap<SymId, bool> = FxHashMap::default();
    let mut reported: FxHashSet<NodeId> = FxHashSet::default();
    for &n in &nodes {
        match cx.node(n) {
            Node::Member { .. } | Node::Index { .. } => {
                property_ref(cx, n, &mut reported);
            }
            Node::Ident { .. } => local_ref(cx, n, &mut captured),
            _ => {}
        }
    }
}

/// `o.p.q` -> `Some(o)` when the chain's root is an identifier or `this`.
fn chain_root(cx: &Ctx<'_, '_, '_>, mut n: NodeId) -> Option<NodeId> {
    for _ in 0..64 {
        n = cx.skip_parens(n);
        match cx.node(n) {
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                if optional {
                    return None;
                }
                n = obj;
            }
            Node::Ident { .. } | Node::This {} => return Some(n),
            _ => return None,
        }
    }
    None
}

/// Whether `n` is in a position that only writes or inspects it (not a read relying on it).
fn is_write_or_test(cx: &Ctx<'_, '_, '_>, n: NodeId) -> bool {
    let Some((p, child)) = cx.parent_skip_parens(n) else {
        return true;
    };
    match cx.node(p) {
        Node::Assign { left, .. } => left == child,
        Node::Update { .. } => true,
        Node::Unary {
            op: UnaryOp::TypeOf | UnaryOp::Delete,
            ..
        } => true,
        _ => false,
    }
}

fn property_ref(cx: &mut Ctx<'_, '_, '_>, r: NodeId, reported: &mut FxHashSet<NodeId>) {
    if chain_root(cx, r).is_none() || is_write_or_test(cx, r) {
        return;
    }
    // Readonly chains are exempt.
    let mut cur = r;
    let mut all_readonly = true;
    let mut first_sym = None;
    for _ in 0..64 {
        cur = cx.skip_parens(cur);
        match cx.node(cur) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => {
                let l = cx.loc(cur);
                match cx.ck.quietly(|ck| ck.property_symbol_of_access(l)) {
                    Some(s) => {
                        if first_sym.is_none() {
                            first_sym = Some(s);
                        }
                        if !cx.ck.quietly(|ck| ck.is_readonly_symbol(s)) {
                            all_readonly = false;
                        }
                    }
                    None => return,
                }
                cur = obj;
            }
            _ => break,
        }
    }
    if all_readonly {
        return;
    }
    let Some(sym) = first_sym else {
        return;
    };
    let narrowed = cx.ty(r);
    let declared = cx.ck.quietly(|ck| ck.type_of_symbol(sym));
    if narrowed == declared || narrowed.is_any_like() || declared.is_any_like() {
        return;
    }
    if !relies(cx, r, declared, narrowed) {
        return;
    }
    let text = cx.src(r).to_owned();
    let Some(from) = narrowing_point(cx, r, &text) else {
        return;
    };
    let Some(effect) = first_effect(cx, from, cx.span(r).start, true) else {
        return;
    };
    if reported.insert(r) {
        let line = cx.line(effect.span.start).to_string();
        let short = cx.short(r);
        cx.report(
            codes::NARROWING_INVALIDATED,
            cx.span(r),
            &[&short, &effect.text, &line],
        );
    }
}

fn local_ref(cx: &mut Ctx<'_, '_, '_>, r: NodeId, captured: &mut FxHashMap<SymId, bool>) {
    // A plain identifier read (not a property name, declaration or write).
    let Some((p, child)) = cx.parent_skip_parens(r) else {
        return;
    };
    match cx.node(p) {
        Node::Member { prop, .. } if prop == child => return,
        Node::VarDeclarator { name, .. } if name == child => return,
        Node::Param { .. } | Node::Property { .. } | Node::MethodDef { .. } => return,
        _ => {}
    }
    if is_write_or_test(cx, r) {
        return;
    }
    let Some(sym) = cx.ck.c.resolve_ident(cx.loc(r), meaning::VALUE) else {
        return;
    };
    let decls = cx.ck.c.decls(sym);
    let Some(decl) = decls.first().copied() else {
        return;
    };
    if decl.file != cx.file {
        return;
    }
    let is_mutable_local = match cx.node(decl.node) {
        Node::VarDeclarator { .. } => !is_const_declarator(cx, decl.node),
        Node::Param { .. } => true,
        Node::Ident { .. } => cx.parent(decl.node).is_some_and(|p| match cx.node(p) {
            Node::Param { .. } => true,
            Node::VarDeclarator { .. } => !is_const_declarator(cx, p),
            _ => false,
        }),
        _ => false,
    };
    if !is_mutable_local {
        return;
    }
    let is_captured = match captured.get(&sym) {
        Some(&c) => c,
        None => {
            let c = assigned_in_closure(cx, sym, decl.node);
            captured.insert(sym, c);
            c
        }
    };
    if !is_captured {
        return;
    }
    let narrowed = cx.ty(r);
    let declared = cx.ck.quietly(|ck| ck.type_of_symbol(sym));
    if narrowed == declared || narrowed.is_any_like() || declared.is_any_like() {
        return;
    }
    if !relies(cx, r, declared, narrowed) {
        return;
    }
    let text = cx.src(r).to_owned();
    let Some(from) = narrowing_point(cx, r, &text) else {
        return;
    };
    let Some(effect) = first_effect(cx, from, cx.span(r).start, false) else {
        return;
    };
    let line = cx.line(effect.span.start).to_string();
    cx.report(
        codes::CAPTURED_INVALIDATED,
        cx.span(r),
        &[&text, &effect.text, &line],
    );
}

fn is_const_declarator(cx: &Ctx<'_, '_, '_>, d: NodeId) -> bool {
    let Some(p) = cx.parent(d) else {
        return false;
    };
    cx.src(p).trim_start().starts_with("const ")
        || cx.src(p).trim_start().starts_with("using ")
        || cx.src(p).trim_start().starts_with("await using ")
}

/// Whether local `sym` (declared at `decl`) is assigned inside a function other than the one
/// declaring it.
fn assigned_in_closure(cx: &mut Ctx<'_, '_, '_>, sym: SymId, decl: NodeId) -> bool {
    let home = cx.enclosing_fn(decl);
    let nodes = cx.nodes.clone();
    for &n in &nodes {
        let target = match cx.node(n) {
            Node::Assign { left, .. } => left,
            Node::Update { arg, .. } => arg,
            _ => continue,
        };
        let target = cx.skip_parens(target);
        if !matches!(cx.node(target), Node::Ident { .. }) {
            continue;
        }
        if cx.ck.c.resolve_ident(cx.loc(target), meaning::VALUE) != Some(sym) {
            continue;
        }
        if cx.enclosing_fn(n) != home {
            return true;
        }
    }
    false
}

/// Whether the use at `r` only type-checks with the narrowed type.
fn relies(cx: &mut Ctx<'_, '_, '_>, r: NodeId, declared: TypeId, narrowed: TypeId) -> bool {
    let nullable = |cx: &Ctx<'_, '_, '_>, t: TypeId| {
        let f = cx.flags(t);
        f.intersects(TypeFlags::NULLABLE | TypeFlags::VOID)
            || (f.contains(TypeFlags::UNION)
                && cx.ck.store().members_of_union(t).iter().any(|&m| {
                    cx.flags(m)
                        .intersects(TypeFlags::NULLABLE | TypeFlags::VOID)
                }))
    };
    let Some((p, child)) = cx.parent_skip_parens(r) else {
        return false;
    };
    match cx.node(p) {
        Node::Member {
            obj,
            prop,
            optional: false,
        } if obj == child => {
            if nullable(cx, declared) && !nullable(cx, narrowed) {
                return true;
            }
            let Some(name) = cx.key_text(prop) else {
                return false;
            };
            let has_declared = cx
                .ck
                .quietly(|ck| ck.property_of_type(declared, &name))
                .is_some();
            let has_narrowed = cx
                .ck
                .quietly(|ck| ck.property_of_type(narrowed, &name))
                .is_some();
            !has_declared && has_narrowed
        }
        Node::Index {
            obj,
            optional: false,
            ..
        }
        | Node::Call {
            callee: obj,
            optional: false,
            ..
        } if obj == child => nullable(cx, declared) && !nullable(cx, narrowed),
        _ => match cx.site_of(r) {
            Some(site) => {
                !site.target.is_any_like()
                    && cx.is_assignable(narrowed, site.target)
                    && !cx.is_assignable(declared, site.target)
            }
            None => false,
        },
    }
}

/// Whether the source of `n` mentions `text` as a whole reference.
fn mentions(cx: &Ctx<'_, '_, '_>, n: NodeId, text: &str) -> bool {
    let s = cx.src(n);
    let mut from = 0;
    while let Some(i) = s[from..].find(text) {
        let end = from + i + text.len();
        let next = s[end..].chars().next();
        if !next.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$') {
            return true;
        }
        from = end;
    }
    false
}

fn always_exits(cx: &Ctx<'_, '_, '_>, s: NodeId) -> bool {
    match cx.node(s) {
        Node::Return { .. } | Node::Throw { .. } | Node::Break { .. } | Node::Continue { .. } => {
            true
        }
        Node::Block { stmts } => cx
            .ast
            .list(stmts)
            .last()
            .is_some_and(|&l| always_exits(cx, l)),
        Node::If {
            cons, alt: Some(a), ..
        } => always_exits(cx, cons) && always_exits(cx, a),
        _ => false,
    }
}

/// The source position after which the narrowing of `r` (text `text`) holds.
fn narrowing_point(cx: &Ctx<'_, '_, '_>, r: NodeId, text: &str) -> Option<u32> {
    let mut child = r;
    let mut cur = cx.parent(r);
    for _ in 0..512 {
        let p = cur?;
        match cx.node(p) {
            Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. } => return None,
            Node::If { test, .. } | Node::Conditional { test, .. } | Node::While { test, .. }
                if test != child && mentions(cx, test, text) =>
            {
                return Some(cx.span(test).end);
            }
            Node::Binary {
                left,
                op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
                ..
            } if left != child && mentions(cx, left, text) => {
                return Some(cx.span(left).end);
            }
            Node::Block { stmts } | Node::Program { stmts } | Node::ModuleBlock { stmts } => {
                let list = cx.ast.list(stmts);
                if let Some(i) = list.iter().position(|&s| s == child) {
                    for &s in list[..i].iter().rev() {
                        match cx.node(s) {
                            Node::If { test, cons, alt } if mentions(cx, test, text) => {
                                let exits = always_exits(cx, cons)
                                    || alt.is_some_and(|a| always_exits(cx, a));
                                if exits {
                                    return Some(cx.span(s).end);
                                }
                            }
                            Node::ExprStmt { expr } => {
                                if let Node::Assign {
                                    left,
                                    op: AssignOp::Assign,
                                    ..
                                } = cx.node(cx.skip_parens(expr))
                                    && cx.src(left) == text
                                {
                                    return Some(cx.span(s).end);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
        child = p;
        cur = cx.parent(p);
    }
    None
}

fn is_safe_call(cx: &mut Ctx<'_, '_, '_>, call: NodeId, callee: NodeId) -> bool {
    let callee = cx.skip_parens(callee);
    let text = cx.src(callee);
    if SAFE_CALLEES.contains(&text) {
        return true;
    }
    if let Node::Member { obj, .. } = cx.node(callee) {
        let o = cx.src(cx.skip_parens(obj));
        if SAFE_NAMESPACES.contains(&o) {
            return true;
        }
        // Methods of primitives (`string`, `number`, `bigint`, `boolean`).
        let ot = cx.ty(obj);
        let f = cx.flags(ot);
        let prim = TypeFlags::STRING_LIKE | TypeFlags::NUMBER_LIKE | TypeFlags::BIGINT_LIKE;
        if f.intersects(prim | TypeFlags::BOOLEAN_LIKE) && !f.contains(TypeFlags::UNION) {
            return true;
        }
        if f.contains(TypeFlags::UNION)
            && cx
                .ck
                .store()
                .members_of_union(ot)
                .iter()
                .all(|&m| cx.flags(m).intersects(prim | TypeFlags::BOOLEAN_LIKE))
        {
            return true;
        }
        // Read-only methods of lib containers and dates that run no user code.
        if let Node::Member { prop, .. } = cx.node(callee) {
            let method = cx.src(prop);
            let head = cx.type_str(ot);
            let head = head
                .trim_start_matches("readonly ")
                .split('<')
                .next()
                .unwrap_or("")
                .to_owned();
            let is_array = head.ends_with("[]") || head == "Array" || head == "ReadonlyArray";
            let safe = match head.as_str() {
                "Map" | "ReadonlyMap" | "WeakMap" => matches!(method, "get" | "has"),
                "Set" | "ReadonlySet" | "WeakSet" => method == "has",
                "Date" => {
                    method.starts_with("get")
                        || matches!(method, "valueOf" | "toISOString" | "toJSON")
                }
                _ => {
                    is_array
                        && matches!(
                            method,
                            "includes" | "indexOf" | "lastIndexOf" | "at" | "slice"
                        )
                }
            };
            if safe && is_lib_method(cx, callee) {
                return true;
            }
        }
    }
    let _ = call;
    false
}

/// The called property is declared in a lib file (not a user subclass override).
fn is_lib_method(cx: &mut Ctx<'_, '_, '_>, callee: NodeId) -> bool {
    let l = cx.loc(callee);
    let Some(sym) = cx.ck.quietly(|ck| ck.property_symbol_of_access(l)) else {
        return false;
    };
    let decls = cx.ck.c.decls(sym);
    !decls.is_empty() && decls.iter().all(|d| cx.info.is_lib(d.file))
}

/// The first effect point that starts at or after `from` and ends before `to`, outside nested
/// function bodies. `property_writes`: assignments to properties and `delete` count too.
fn first_effect(
    cx: &mut Ctx<'_, '_, '_>,
    from: u32,
    to: u32,
    property_writes: bool,
) -> Option<Effect> {
    let nodes = cx.nodes.clone();
    let mut skip_until = 0u32;
    for &n in &nodes {
        let sp = cx.span(n);
        if sp.start < skip_until {
            continue;
        }
        if sp.start >= to {
            break;
        }
        if sp.end <= from || sp.start < from {
            continue;
        }
        let effect = match cx.node(n) {
            Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. } => {
                skip_until = sp.end;
                continue;
            }
            _ if sp.end > to => continue,
            Node::Call { callee, .. } => !is_safe_call(cx, n, callee),
            Node::New { .. }
            | Node::TaggedTemplate { .. }
            | Node::Await { .. }
            | Node::Yield { .. } => true,
            Node::Assign { left, .. } if property_writes => matches!(
                cx.node(cx.skip_parens(left)),
                Node::Member { .. } | Node::Index { .. }
            ),
            Node::Update { arg, .. } if property_writes => matches!(
                cx.node(cx.skip_parens(arg)),
                Node::Member { .. } | Node::Index { .. }
            ),
            Node::Unary {
                op: UnaryOp::Delete,
                ..
            } if property_writes => true,
            _ => false,
        };
        if effect {
            return Some(Effect {
                span: sp,
                text: cx.short(n),
            });
        }
    }
    None
}
