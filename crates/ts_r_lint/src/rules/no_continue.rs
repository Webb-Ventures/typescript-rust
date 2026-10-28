//! `no-continue`: disallow `continue` statements.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-continue",
        "Disallow `continue` statements",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoContinue>,
};

#[derive(Debug, Default)]
struct NoContinue;

impl Rule for NoContinue {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Continue]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        cx.report(cx.span(node), "Unexpected use of continue statement.")
            .id("unexpected");
    }
}
