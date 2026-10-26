//! `no-new-wrappers`: disallow `new String`, `new Number` and `new Boolean`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-new-wrappers",
        "Disallow `new` operators with the `String`, `Number`, and `Boolean` objects",
        NO_PRESET,
        Category::Pedantic,
    ),
    create: create_default::<NoNewWrappers>,
};

#[derive(Debug, Default)]
struct NoNewWrappers;

impl Rule for NoNewWrappers {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::New { callee, .. } = *cx.node(node) else {
            return;
        };
        let callee = skip_parens(cx.ast(), callee);
        let Some(name) = ident_name(cx.ast(), callee) else {
            return;
        };
        if matches!(name, "String" | "Number" | "Boolean") && cx.is_global_ref(callee, name) {
            cx.report(
                cx.span(node),
                format!("Do not use {name} as a constructor."),
            )
            .id("noConstructor");
        }
    }
}
