//! `unicorn/no-confusing-array-with`: `array.with(-1, v)` replaces from the end, unlike most
//! index writes, and `array.with(array.length, v)` throws. Reported on `.with` of a plain
//! call whose first argument is a static number that truncates below zero, or the receiver's
//! own `.length`, unless the receiver is known not to be an array or typed array.

use super::prelude::*;
use super::unicorn_ast::{method_call, no_spread_in};
use super::unicorn_text::is_same_reference;
use super::unicorn_types::should_skip_known_non_array_receiver;
use super::unicorn_util::skip_wrappers;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-confusing-array-with",
        "Disallow confusing uses of `Array#with()`.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoConfusingArrayWith>,
};

#[derive(Debug, Default)]
struct NoConfusingArrayWith;

/// A number literal, possibly under `+`/`-` signs and TS wrappers.
fn static_number(ast: &Ast, id: NodeId, depth: u32) -> Option<f64> {
    if depth > 64 {
        return None;
    }
    match *ast.node(skip_wrappers(ast, id)) {
        Node::NumberLit { value } => Some(value.get()),
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } => static_number(ast, arg, depth + 1).map(|v| -v),
        Node::Unary {
            op: UnaryOp::Plus,
            arg,
        } => static_number(ast, arg, depth + 1),
        _ => None,
    }
}

/// `object.length` (not optional, not computed), through TS wrappers.
fn is_length_of(cx: &Ctx<'_, '_>, id: NodeId, object: NodeId) -> bool {
    let ast = cx.ast();
    matches!(
        *ast.node(skip_wrappers(ast, id)),
        Node::Member { obj, optional: false, prop }
            if ident_name(ast, prop) == Some("length") && is_same_reference(cx, obj, object)
    )
}

impl Rule for NoConfusingArrayWith {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node).filter(|m| m.plain() && m.name == "with") else {
            return;
        };
        if call.args.is_empty() || !no_spread_in(ast, call.args, 1) {
            return;
        }
        let index = call.args[0];
        let (id, message) = if static_number(ast, index, 0).is_some_and(|v| v.trunc() < 0.0) {
            (
                "negative-index",
                "Avoid using a negative index with `Array#with()`.",
            )
        } else if is_length_of(cx, index, call.obj) {
            (
                "length-index",
                "Avoid using `.length` as the index in `Array#with()`.",
            )
        } else {
            return;
        };
        if should_skip_known_non_array_receiver(cx, call.obj) {
            return;
        }
        cx.report(cx.span(call.prop), message).id(id);
    }
}
