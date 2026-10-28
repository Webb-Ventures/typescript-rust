//! `unicorn/no-return-array-push`: `x.push(…)` / `x.unshift(…)` (non-computed, at least one
//! argument, no spread) whose result is used. The result counts as unused when the call,
//! through parentheses and TS wrappers, is an expression statement or a `void` operand; a
//! member read on the result (`router.push(to).catch(…)`) is left alone too, as is a
//! receiver known not to be an indexed collection. `push` on a well-known stream or router
//! path (`stream`, `router`, `this`, `this.router`, `this.$router`, `this.stream`,
//! `process.stdin/stdout/stderr`) is skipped unless the receiver is known to be an array.
//! Reported at the method name.
//!
//! When the call is directly the argument of a `return` (parentheses aside, not an open
//! optional chain) inside a block, and the `return` holds no comments outside the call, a
//! suggestion turns `return x.push(v);` into `x.push(v); return;` (the call wrapped when it
//! would start with a declaration-like token, a `;` added when it would continue the
//! previous line).

use super::prelude::*;
use super::unicorn_ast::{method_call, no_spread_in};
use super::unicorn_text::{
    has_unparenthesized_optional_chain, needs_semicolon, starts_with_declaration_like_token,
};
use super::unicorn_types::{is_array, is_known_non_indexed_collection};
use ts_r_ast::{Ast, UnaryOp};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-return-array-push",
        "Disallow using the return value of `Array#push()` and `Array#unshift()`.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<NoReturnArrayPush>,
};

#[derive(Debug, Default)]
struct NoReturnArrayPush;

const IGNORED_PUSH_RECEIVERS: &[&[&str]] = &[
    &["stream"],
    &["router"],
    &["this"],
    &["this", "router"],
    &["this", "$router"],
    &["this", "stream"],
    &["process", "stdin"],
    &["process", "stdout"],
    &["process", "stderr"],
];

/// `id` is exactly the static path (`this` matches a `this` expression).
fn is_static_path(cx: &Ctx<'_, '_>, mut id: NodeId, path: &[&str]) -> bool {
    let ast = cx.ast();
    let Some((first, rest)) = path.split_first() else {
        return false;
    };
    for name in rest.iter().rev() {
        let Node::Member { obj, prop, .. } = *ast.node(id) else {
            return false;
        };
        if !matches!(ast.node(prop), Node::Ident { .. }) || cx.text(prop) != *name {
            return false;
        }
        id = obj;
    }
    match ast.node(id) {
        Node::Ident { .. } => cx.text(id) == *first,
        Node::This { .. } => *first == "this",
        _ => false,
    }
}

fn is_ts_wrapper(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. }
    )
}

/// The outermost parenthesis or TS wrapper around `id`, and its parent.
fn outermost(cx: &Ctx<'_, '_>, mut id: NodeId) -> (NodeId, Option<NodeId>) {
    let ast = cx.ast();
    for _ in 0..10_000 {
        match cx.parent(id) {
            Some(p) if is_ts_wrapper(ast, p) => id = p,
            parent => return (id, parent),
        }
    }
    (id, None)
}

/// The `return` whose argument is the call (parentheses aside).
fn direct_return(cx: &Ctx<'_, '_>, call: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let mut id = call;
    for _ in 0..10_000 {
        let p = cx.parent(id)?;
        match *ast.node(p) {
            Node::Paren { .. } => id = p,
            Node::Return { arg: Some(arg) } if arg == id => return Some(p),
            _ => return None,
        }
    }
    None
}

fn suggestion(cx: &Ctx<'_, '_>, call: NodeId, ret: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    if has_unparenthesized_optional_chain(cx, call)
        || !matches!(
            cx.parent(ret).map(|p| ast.node(p)),
            Some(Node::Block { .. })
        )
        || ast.comments_in(cx.span(ret)).len() != ast.comments_in(cx.span(call)).len()
    {
        return None;
    }
    let mut text = cx.text(call).to_string();
    if starts_with_declaration_like_token(cx, call) {
        text = format!("({text})");
    }
    let span = cx.span(ret);
    let semi = if needs_semicolon(cx, span.start, &text) {
        ";"
    } else {
        ""
    };
    Some(Fix::replace(span, format!("{semi}{text}; return;")))
}

impl Rule for NoReturnArrayPush {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, id) else {
            return;
        };
        let method = call.name;
        if !matches!(method, "push" | "unshift")
            || call.args.is_empty()
            || !no_spread_in(ast, call.args, call.args.len())
        {
            return;
        }
        let obj = call.obj;
        if method == "push"
            && IGNORED_PUSH_RECEIVERS
                .iter()
                .any(|path| is_static_path(cx, obj, path))
            && !is_array(cx, obj)
        {
            return;
        }
        let (top, parent) = outermost(cx, id);
        match parent.map(|p| ast.node(p)) {
            Some(Node::ExprStmt { .. }) => return,
            Some(Node::Unary {
                op: UnaryOp::Void, ..
            }) => return,
            Some(&Node::Member { obj, .. }) | Some(&Node::Index { obj, .. }) if obj == top => {
                return;
            }
            _ => {}
        }
        if is_known_non_indexed_collection(cx, obj) {
            return;
        }
        let fix = direct_return(cx, id).and_then(|ret| suggestion(cx, id, ret));
        let method = method.to_string();
        let r = cx
            .report(
                cx.span(call.prop),
                format!("Do not use the return value of `.{method}(…)`."),
            )
            .id("no-return-array-push/error");
        if let Some(fix) = fix {
            r.suggest(
                "no-return-array-push/suggestion",
                format!("Separate the `{method}()` call from `return`."),
                fix,
            );
        }
    }
}
