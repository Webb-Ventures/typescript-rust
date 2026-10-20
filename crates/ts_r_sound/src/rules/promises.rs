//! S-10: no floating promises. An expression statement whose value is promise-like (has a
//! callable `then`), or an array of promise-likes, must be handled: `.catch(h)` or
//! `.then(f, h)` (TSR1001). `void p` needs a handled `p` (TSR1002). A function returning a
//! promise may not be passed where the parameter's function type returns `void` (TSR1003).

use ts_r_ast::{FnFlags, Node, NodeId, UnaryOp};
use ts_r_checker::members::SigKind;
use ts_r_types::{TypeFlags, TypeId};

use crate::codes;
use crate::ctx::{Ctx, SiteKind};

pub fn run(cx: &mut Ctx<'_, '_, '_>) {
    if !cx.has_rule(10) {
        return;
    }
    let nodes = cx.nodes.clone();
    for &n in &nodes {
        let Node::ExprStmt { expr } = cx.node(n) else {
            continue;
        };
        let e = cx.skip_parens(expr);
        match cx.node(e) {
            Node::Unary {
                op: UnaryOp::Void,
                arg,
            } => {
                let arg = cx.skip_parens(arg);
                let t = cx.ty(arg);
                if is_promise_like(cx, t, 0) && !handled(cx, arg, 0) {
                    let text = cx.short(arg);
                    cx.report(codes::VOID_PROMISE, cx.span(e), &[&text]);
                }
            }
            Node::Assign { .. } | Node::Await { .. } | Node::Yield { .. } => {}
            _ => {
                let t = cx.ty(e);
                let floating = is_promise_like(cx, t, 0)
                    || cx
                        .ck
                        .quietly(|ck| ck.element_type_of_array_like(t))
                        .is_some_and(|el| is_promise_like(cx, el, 0));
                if floating && !handled(cx, e, 0) {
                    let text = source_text(cx, e);
                    cx.report(codes::FLOATING_PROMISE, cx.span(n), &[&text]);
                }
            }
        }
    }
    for (arg, site) in cx.sites() {
        let SiteKind::Argument { call, .. } = site.kind else {
            continue;
        };
        if !returns_promise(cx, arg) {
            continue;
        }
        let target_sigs = cx
            .ck
            .quietly(|ck| ck.signatures_of(site.target, SigKind::Call));
        if target_sigs.is_empty() {
            continue;
        }
        let all_void = target_sigs.iter().all(|&s| {
            let r = cx.ck.quietly(|ck| ck.return_type_of(s));
            r == TypeId::VOID
        });
        if all_void {
            let callee = match cx.node(call) {
                Node::Call { callee, .. } | Node::New { callee, .. } => cx.short(callee),
                _ => cx.short(call),
            };
            cx.report(codes::ASYNC_VOID_CALLBACK, cx.span(arg), &[&callee]);
        }
    }
}

/// `save` for `save("a")`, `fetch("/ping").then` for chains: the callee, else the text.
fn source_text(cx: &Ctx<'_, '_, '_>, e: NodeId) -> String {
    match cx.node(e) {
        Node::Call { callee, .. } | Node::New { callee, .. } => cx.short(callee),
        _ => cx.short(e),
    }
}

/// An async function or arrow, or a function whose call signatures return promise-likes.
fn returns_promise(cx: &mut Ctx<'_, '_, '_>, arg: NodeId) -> bool {
    let a = cx.skip_parens(arg);
    match cx.node(a) {
        Node::Arrow { flags, .. } | Node::FunctionExpr { flags, .. }
            if flags.contains(FnFlags::ASYNC) =>
        {
            true
        }
        _ => {
            let t = cx.ty(a);
            if t.is_any_like() {
                return false;
            }
            let sigs = cx.ck.quietly(|ck| ck.signatures_of(t, SigKind::Call));
            !sigs.is_empty()
                && sigs.iter().all(|&s| {
                    let r = cx.ck.quietly(|ck| ck.return_type_of(s));
                    is_promise_like(cx, r, 0)
                })
        }
    }
}

/// Has a callable `then` (every union member, ignoring `undefined`/`null`).
pub fn is_promise_like(cx: &mut Ctx<'_, '_, '_>, t: TypeId, depth: u32) -> bool {
    if depth > 4 || t.is_any_like() {
        return false;
    }
    let f = cx.flags(t);
    if f.intersects(TypeFlags::ANY_OR_UNKNOWN | TypeFlags::NEVER) {
        return false;
    }
    if f.contains(TypeFlags::UNION) {
        let members = cx.ck.store().members_of_union(t).to_vec();
        let mut any = false;
        for m in members {
            if cx
                .flags(m)
                .intersects(TypeFlags::NULLABLE | TypeFlags::VOID)
            {
                continue;
            }
            if !is_promise_like(cx, m, depth + 1) {
                return false;
            }
            any = true;
        }
        return any;
    }
    if !f.intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION | TypeFlags::TYPE_PARAMETER) {
        return false;
    }
    let Some(then) = cx.ck.quietly(|ck| ck.property_of_type(t, "then")) else {
        return false;
    };
    let tt = cx.ck.quietly(|ck| ck.type_of_symbol(then));
    !cx.ck
        .quietly(|ck| ck.signatures_of(tt, SigKind::Call))
        .is_empty()
}

/// `p.catch(h)`, `p.then(f, h)`, or `.finally` on a handled chain.
fn handled(cx: &Ctx<'_, '_, '_>, e: NodeId, depth: u32) -> bool {
    if depth > 32 {
        return false;
    }
    let e = cx.skip_parens(e);
    let Node::Call { callee, args, .. } = cx.node(e) else {
        return false;
    };
    let Node::Member { obj, prop, .. } = cx.node(cx.skip_parens(callee)) else {
        return false;
    };
    let n = cx.ast.list(args).len();
    match cx.src(prop) {
        "catch" => n >= 1,
        "then" => n >= 2,
        "finally" => handled(cx, obj, depth + 1),
        _ => false,
    }
}
