//! `symbol-description`: require a description when calling `Symbol()`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "symbol-description",
        "Require symbol descriptions",
        NO_PRESET,
        Category::Pedantic,
    ),
    create: create_default::<SymbolDescription>,
};

#[derive(Debug, Default)]
struct SymbolDescription;

impl Rule for SymbolDescription {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Call { callee, args, .. } = *cx.node(node) else {
            return;
        };
        if cx.ast().list(args).is_empty() && cx.is_global_ref(callee, "Symbol") {
            cx.report(cx.span(node), "Expected Symbol to have a description.")
                .id("expected");
        }
    }
}
