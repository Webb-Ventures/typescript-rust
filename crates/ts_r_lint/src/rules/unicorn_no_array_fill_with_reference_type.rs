//! `unicorn/no-array-fill-with-reference-type`: `x.fill(value)` (at least one argument, no
//! optional chaining) where the first argument is a fresh reference value: an object,
//! array or class literal, or a `new` expression other than the global `RegExp`, directly
//! (through TS wrappers) or as the initializer of the `const` it names. Receivers known not
//! to be arrays are skipped (typed arrays are not: `isKnownNonArray`). Reported at the
//! value.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_no_array_reduce::const_initializer as const_init;
use super::unicorn_static::is_global_ident;
use super::unicorn_types::is_known_non_array;
use super::unicorn_util::skip_wrappers;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-array-fill-with-reference-type",
        "Disallow using reference values as `Array#fill()` values.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoArrayFillWithReferenceType>,
};

#[derive(Debug, Default)]
struct NoArrayFillWithReferenceType;

fn is_reference_expression(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_wrappers(ast, id);
    match *ast.node(id) {
        Node::ObjectLit { .. } | Node::ArrayLit { .. } | Node::ClassExpr { .. } => true,
        Node::New { callee, .. } => {
            !(ident_name(ast, callee) == Some("RegExp") && is_global_ident(cx, callee, "RegExp"))
        }
        _ => false,
    }
}

impl Rule for NoArrayFillWithReferenceType {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node) else {
            return;
        };
        if !call.plain() || call.name != "fill" || call.args.is_empty() {
            return;
        }
        let value = call.args[0];
        let reference = is_reference_expression(cx, value)
            || const_init(cx, skip_wrappers(ast, value))
                .is_some_and(|i| is_reference_expression(cx, i));
        if !reference || is_known_non_array(cx, call.obj) {
            return;
        }
        let span = cx.span(skip_parens(ast, value));
        cx.report(span, "Do not use a reference value as the fill value.")
            .id("no-array-fill-with-reference-type");
    }
}
