//! `unicorn/no-duplicate-loops`: a `for…of` / `for await…of` whose iterable (through
//! parentheses and TS wrappers) is `x.map(..)` or `x.filter(..)` (at least one argument, no
//! optional chaining, plain name) on a receiver not known to be an iterator (iterator
//! helpers are lazy, so they don't build an intermediate array). Reported at the method.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_iter::{is_iterator_expression, unwrap_expression};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-duplicate-loops",
        "Disallow `.map()` and `.filter()` in `for…of` and `for await…of` loop headers.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoDuplicateLoops>,
};

#[derive(Debug, Default)]
struct NoDuplicateLoops;

impl Rule for NoDuplicateLoops {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ForOf]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::ForOf {
            is_await, right, ..
        } = *ast.node(id)
        else {
            return;
        };
        let Some(call) = method_call(ast, unwrap_expression(ast, right)) else {
            return;
        };
        if !matches!(call.name, "filter" | "map")
            || call.args.is_empty()
            || !call.plain()
            || is_iterator_expression(cx, call.obj)
        {
            return;
        }
        let message = format!(
            "Do not use `.{}()` directly in a `{}` loop header. It creates an intermediate array before the loop.",
            call.name,
            if is_await {
                "for await…of"
            } else {
                "for…of"
            }
        );
        cx.report(cx.span(call.prop), message)
            .id("no-duplicate-loops");
    }
}
