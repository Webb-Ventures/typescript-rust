//! S-08 (b) and (c): `this` must not escape a constructor or field initialiser before every
//! field is assigned (TSR0802), nor afterwards to code a subclass can override (TSR0803).
//!
//! Order: parameter properties, then field initialisers in declaration order, then the
//! constructor's top-level statements. A field counts as assigned from the start when it is
//! optional, its type includes `undefined`, or it has `!` (TSR0801 covers that); otherwise
//! when its initialiser has run or a top-level `this.f = ...` statement has. Escapes are calls
//! of methods or accessors on `this`, `this` used as a value (argument, return, stored), and
//! arrow functions that mention `this`.

use ts_r_ast::{MethodKind, Modifiers, Node, NodeId};
use ts_r_types::TypeFlags;

use crate::codes;
use crate::ctx::Ctx;

struct Class {
    /// Own methods and accessors: name, overridable.
    methods: Vec<(String, bool)>,
    /// Fields not yet assigned, in declaration order.
    unassigned: Vec<String>,
}

pub fn run(cx: &mut Ctx<'_, '_, '_>) {
    if !cx.has_rule(8) {
        return;
    }
    let nodes = cx.nodes.clone();
    for &n in &nodes {
        if let Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } = cx.node(n) {
            let members = cx.ast.list(members).to_vec();
            class(cx, &members);
        }
    }
}

fn member_name(cx: &Ctx<'_, '_, '_>, key: NodeId) -> Option<String> {
    cx.key_text(key)
}

fn class(cx: &mut Ctx<'_, '_, '_>, members: &[NodeId]) {
    let mut c = Class {
        methods: Vec::new(),
        unassigned: Vec::new(),
    };
    let mut ctor = None;
    // (field name, initialiser) in declaration order.
    let mut fields: Vec<(String, Option<NodeId>)> = Vec::new();
    for &m in members {
        match cx.node(m) {
            Node::MethodDef {
                kind,
                key,
                modifiers,
                value,
                ..
            } => {
                if modifiers.contains(Modifiers::STATIC) {
                    continue;
                }
                if kind == MethodKind::Constructor {
                    ctor = Some(value);
                    continue;
                }
                let Some(name) = member_name(cx, key) else {
                    continue;
                };
                let overridable =
                    !(modifiers.contains(Modifiers::PRIVATE) || name.starts_with('#'));
                c.methods.push((name, overridable));
            }
            Node::PropertyDef {
                key,
                modifiers,
                optional,
                definite,
                type_ann,
                value,
                ..
            } => {
                if modifiers.intersects(Modifiers::STATIC | Modifiers::DECLARE) {
                    continue;
                }
                let Some(name) = member_name(cx, key) else {
                    continue;
                };
                let mut assigned_from_start = optional || definite;
                if let Some(t) = type_ann {
                    let ty = cx.type_node(t);
                    if allows_undefined(cx, ty) {
                        assigned_from_start = true;
                    }
                } else if value.is_none() {
                    assigned_from_start = true; // implicit any: an error already
                }
                if !assigned_from_start {
                    c.unassigned.push(name.clone());
                }
                fields.push((name, value));
            }
            _ => {}
        }
    }
    let mut assigned_by_init: Vec<String> = Vec::new();
    // Parameter properties run first.
    if let Some(ctor) = ctor
        && let Some(parts) = ts_r_checker::nodes::fn_parts(cx.ast, ctor)
    {
        for &p in cx.ast.list(parts.params).to_vec().iter() {
            if let Node::Param {
                modifiers, pattern, ..
            } = cx.node(p)
                && modifiers.intersects(
                    Modifiers::PUBLIC
                        | Modifiers::PRIVATE
                        | Modifiers::PROTECTED
                        | Modifiers::READONLY,
                )
            {
                let name = cx.src(pattern).to_owned();
                c.unassigned.retain(|f| *f != name);
            }
        }
    }
    for (name, init) in &fields {
        if let Some(init) = init {
            escapes(cx, &c, *init);
            assigned_by_init.push(name.clone());
            c.unassigned.retain(|f| f != name);
        }
    }
    let Some(ctor) = ctor else {
        return;
    };
    let Some(parts) = ts_r_checker::nodes::fn_parts(cx.ast, ctor) else {
        return;
    };
    let Some(body) = parts.body else {
        return;
    };
    let Node::Block { stmts } = cx.node(body) else {
        return;
    };
    for &s in cx.ast.list(stmts).to_vec().iter() {
        escapes(cx, &c, s);
        // Top-level `this.f = ...;` assigns `f` from here on.
        if let Node::ExprStmt { expr } = cx.node(s)
            && let Node::Assign { left, .. } = cx.node(cx.skip_parens(expr))
            && let Node::Member { obj, prop, .. } = cx.node(cx.skip_parens(left))
            && matches!(cx.node(cx.skip_parens(obj)), Node::This {})
        {
            let name = cx.src(prop).to_owned();
            c.unassigned.retain(|f| *f != name);
        }
    }
}

fn allows_undefined(cx: &Ctx<'_, '_, '_>, t: ts_r_types::TypeId) -> bool {
    if t.is_any_like() {
        return true;
    }
    let f = cx.flags(t);
    if f.intersects(TypeFlags::ANY_OR_UNKNOWN | TypeFlags::UNDEFINED | TypeFlags::VOID) {
        return true;
    }
    f.contains(TypeFlags::UNION)
        && cx.ck.store().members_of_union(t).iter().any(|&m| {
            cx.flags(m)
                .intersects(TypeFlags::UNDEFINED | TypeFlags::VOID)
        })
}

/// Reports the first escape of `this` in `n` (a statement or initialiser).
fn escapes(cx: &mut Ctx<'_, '_, '_>, c: &Class, n: NodeId) {
    let mut stack = vec![n];
    let mut steps = 0u32;
    while let Some(x) = stack.pop() {
        steps += 1;
        if steps > 200_000 {
            return;
        }
        match cx.node(x) {
            // Functions and classes have their own `this`; arrows share it.
            Node::FunctionExpr { .. } | Node::FunctionDecl { .. } | Node::ClassExpr { .. } => {
                continue;
            }
            Node::Arrow { .. } => {
                if mentions_this(cx, x) {
                    let through = arrow_target(cx, x);
                    if report(cx, c, x, &through) {
                        return;
                    }
                }
                continue;
            }
            Node::This {} => {
                let Some((p, child)) = cx.parent_skip_parens(x) else {
                    continue;
                };
                match cx.node(p) {
                    Node::Member { obj, prop, .. } if obj == child => {
                        let name = cx.src(prop).to_owned();
                        if !c.methods.iter().any(|(m, _)| *m == name) {
                            continue; // a field read or write
                        }
                        let at = match cx.parent_skip_parens(p) {
                            Some((call, callee)) if matches!(cx.node(call), Node::Call { callee: c2, .. } if c2 == callee) => {
                                call
                            }
                            _ => p,
                        };
                        let text = cx.short(p);
                        if report_method(cx, c, at, &name, &text) {
                            return;
                        }
                    }
                    Node::Index { obj, .. } if obj == child => {}
                    _ => {
                        // `this` as a value: passed, returned or stored.
                        let through = value_target(cx, p, child);
                        if report(cx, c, x, &through) {
                            return;
                        }
                    }
                }
                continue;
            }
            _ => {}
        }
        let mut kids = Vec::new();
        cx.ast.for_each_child(x, |k| kids.push(k));
        stack.extend(kids.into_iter().rev());
    }
}

/// An escape of `this` itself (passed, stored, captured): TSR0802 before every field is
/// assigned, TSR0803 after.
fn report(cx: &mut Ctx<'_, '_, '_>, c: &Class, at: NodeId, through: &str) -> bool {
    if let Some(f) = c.unassigned.first() {
        let f = f.clone();
        cx.report(codes::THIS_BEFORE_INIT, cx.span(at), &[&f]);
        return true;
    }
    if cx.info.config.policy.this_escape_after_init {
        cx.report(codes::THIS_TO_OVERRIDABLE, cx.span(at), &[through]);
        return true;
    }
    false
}

/// A method or accessor used on `this`.
fn report_method(cx: &mut Ctx<'_, '_, '_>, c: &Class, at: NodeId, name: &str, text: &str) -> bool {
    if let Some(f) = c.unassigned.first() {
        let f = f.clone();
        cx.report(codes::THIS_BEFORE_INIT, cx.span(at), &[&f]);
        return true;
    }
    let overridable = c
        .methods
        .iter()
        .find(|(m, _)| m == name)
        .is_some_and(|(_, o)| *o);
    if overridable && cx.info.config.policy.this_escape_after_init {
        cx.report(codes::THIS_TO_OVERRIDABLE, cx.span(at), &[text]);
        return true;
    }
    false
}

fn mentions_this(cx: &Ctx<'_, '_, '_>, n: NodeId) -> bool {
    let mut stack = vec![n];
    while let Some(x) = stack.pop() {
        match cx.node(x) {
            Node::This {} => return true,
            Node::FunctionExpr { .. } | Node::FunctionDecl { .. } | Node::ClassExpr { .. }
                if x != n =>
            {
                continue;
            }
            _ => {}
        }
        cx.ast.for_each_child(x, |k| stack.push(k));
    }
    false
}

/// What an arrow function escapes through: the callee it is passed to.
fn arrow_target(cx: &Ctx<'_, '_, '_>, arrow: NodeId) -> String {
    match cx.parent_skip_parens(arrow) {
        Some((p, _)) => match cx.node(p) {
            Node::Call { callee, .. } | Node::New { callee, .. } => cx.short(callee),
            _ => "an arrow function".to_owned(),
        },
        None => "an arrow function".to_owned(),
    }
}

fn value_target(cx: &Ctx<'_, '_, '_>, p: NodeId, _child: NodeId) -> String {
    match cx.node(p) {
        Node::Call { callee, .. } | Node::New { callee, .. } => cx.short(callee),
        _ => cx.short(p),
    }
}
