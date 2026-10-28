//! `unicorn/no-unnecessary-array-flat-depth`: `foo.flat(1)` (a plain method call with one
//! argument, the number literal `1`) passes the default depth. Reported at the `1`; the fix
//! empties the argument list to `()`, or removes just the argument when the list has a
//! comment. Receivers known not to be arrays are skipped.

use super::prelude::*;
use super::unicorn_ast::{call_tokens, method_call, remove_argument};
use super::unicorn_types::should_skip_known_non_array_receiver;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unnecessary-array-flat-depth",
        "Disallow using `1` as the `depth` argument of `Array#flat()`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoUnnecessaryArrayFlatDepth>,
};

#[derive(Debug, Default)]
struct NoUnnecessaryArrayFlatDepth;

impl Rule for NoUnnecessaryArrayFlatDepth {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node) else {
            return;
        };
        if call.name != "flat" || call.optional_call {
            return;
        }
        let [arg] = *call.args else {
            return;
        };
        let one = skip_parens(ast, arg);
        if !matches!(*ast.node(one), Node::NumberLit { value } if value.get() == 1.0) {
            return;
        }
        if should_skip_known_non_array_receiver(cx, call.obj) {
            return;
        }
        let fix = call_tokens(cx, node).and_then(|(open, close, _)| {
            let list = Span::new(open, close + 1);
            if ast.comments_in(list).is_empty() {
                Some(Fix::replace(list, "()"))
            } else {
                remove_argument(cx, node, 0)
            }
        });
        let report = cx
            .report(
                cx.span(one),
                "Passing `1` as the `depth` argument is unnecessary.",
            )
            .id("no-unnecessary-array-flat-depth");
        if let Some(fix) = fix {
            report.fix(fix);
        }
    }
}
