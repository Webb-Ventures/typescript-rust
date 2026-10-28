//! `unicorn/no-async-promise-finally`: the first argument of `x.finally(cb)` (the name
//! also through a computed key with a static value) is an async, non-generator function:
//! written inline (through TS wrappers), the initializer of a single plain `const`, or a
//! single async function declaration. With type information, a receiver whose type is
//! known not to be a promise (every non-nullable union member has no `then` method) is
//! skipped; `any`/`unknown` count as unknown, so they are reported.

use super::prelude::*;
use super::unicorn_static::{skip_ts_only, static_value};
use crate::type_util::is_thenable;
use crate::types::{TypeFlags, TypeInfo};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-async-promise-finally",
        "Disallow async functions as `Promise#finally()` callbacks.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoAsyncPromiseFinally>,
};

#[derive(Debug, Default)]
struct NoAsyncPromiseFinally;

fn is_async_function(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    match *cx.node(id) {
        Node::FunctionExpr { flags, .. }
        | Node::FunctionDecl { flags, .. }
        | Node::Arrow { flags, .. } => {
            flags.contains(FnFlags::ASYNC) && !flags.contains(FnFlags::GENERATOR)
        }
        _ => false,
    }
}

fn is_async_callback(cx: &Ctx<'_, '_>, arg: NodeId) -> bool {
    let ast = cx.ast();
    let arg = skip_ts_only(ast, arg);
    if is_async_function(cx, arg) {
        return true;
    }
    if !matches!(ast.node(arg), Node::Ident { .. }) {
        return false;
    }
    let sem = cx.semantic();
    let Some(Resolution::Symbol(s)) = sem.reference_at(arg).map(|r| sem.reference(r).resolution)
    else {
        return false;
    };
    let [decl] = sem.symbol(s).decls.as_slice() else {
        return false;
    };
    match decl.kind {
        DeclKind::Const => match *ast.node(decl.node) {
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } if name == decl.name => is_async_function(cx, skip_ts_only(ast, init)),
            _ => false,
        },
        DeclKind::Function => is_async_function(cx, decl.node),
        _ => false,
    }
}

/// `Some(false)` when the type is known not to be a promise, `Some(true)` when it is one,
/// `None` when unknown.
fn is_promise(ti: &dyn TypeInfo, t: Ty, depth: u32) -> Option<bool> {
    if depth > 8 || t == Ty::UNRESOLVED {
        return None;
    }
    if ti.flags(t).intersects(TypeFlags::ANY | TypeFlags::UNKNOWN) {
        return None;
    }
    let t = ti.non_nullable(t);
    let members = ti.union_members(t);
    if members.len() > 1 {
        let results: Vec<Option<bool>> = members
            .iter()
            .map(|&m| is_promise(ti, m, depth + 1))
            .collect();
        if results.iter().all(|r| *r == Some(true)) {
            return Some(true);
        }
        if results.iter().all(|r| *r == Some(false)) {
            return Some(false);
        }
        return None;
    }
    Some(is_thenable(ti, t))
}

impl Rule for NoAsyncPromiseFinally {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *cx.node(node) else {
            return;
        };
        let (obj, name) = match *ast.node(callee) {
            Node::Member { obj, prop, .. } => (obj, ident_name(ast, prop).map(str::to_owned)),
            Node::Index { obj, index, .. } => {
                (obj, static_value(cx, index).and_then(|v| v.to_js_string()))
            }
            _ => return,
        };
        if name.as_deref() != Some("finally") {
            return;
        }
        let Some(&callback) = ast.list(args).first() else {
            return;
        };
        if !is_async_callback(cx, callback) {
            return;
        }
        let ti = cx.types();
        if ti.is_available()
            && let Some(t) = ti.type_at(obj)
            && is_promise(ti, t, 0) == Some(false)
        {
            return;
        }
        cx.report(
            cx.span(callback),
            "Do not pass an async function to `Promise#finally()`.",
        )
        .id("no-async-promise-finally");
    }
}
