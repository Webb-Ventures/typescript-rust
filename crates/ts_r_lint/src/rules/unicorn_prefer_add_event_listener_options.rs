//! `unicorn/prefer-add-event-listener-options`: a boolean third argument of
//! `x.addEventListener(type, listener, true)` (three non-spread arguments, no optional
//! chaining) is fixed to the options object `{capture: true}`.

use super::prelude::*;
use super::unicorn_ast::{method_call, no_spread_in};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-add-event-listener-options",
        "Prefer an options object over a boolean in `addEventListener()`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferAddEventListenerOptions>,
};

#[derive(Debug, Default)]
struct PreferAddEventListenerOptions;

impl Rule for PreferAddEventListenerOptions {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node) else {
            return;
        };
        if call.name != "addEventListener"
            || !call.plain()
            || call.args.len() != 3
            || !no_spread_in(ast, call.args, 3)
        {
            return;
        }
        let opt = skip_parens(ast, call.args[2]);
        let Node::BoolLit { value } = *ast.node(opt) else {
            return;
        };
        let replacement = format!("{{capture: {value}}}");
        let msg = format!("Prefer `{replacement}` over `{value}`.");
        let span = cx.span(opt);
        cx.report(span, msg)
            .id("prefer-add-event-listener-options")
            .fix(Fix::replace(span, replacement));
    }
}
