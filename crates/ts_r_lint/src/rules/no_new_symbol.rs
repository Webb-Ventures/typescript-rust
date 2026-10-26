//! `no-new-symbol` (deprecated): disallow `new Symbol()`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-new-symbol",
        "Disallow `new` operators with the `Symbol` object",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoNewSymbol>,
};

#[derive(Debug, Default)]
struct NoNewSymbol;

impl Rule for NoNewSymbol {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::New { callee, .. } = *cx.node(node) else {
            return;
        };
        if cx.is_global_ref(callee, "Symbol") {
            let span = cx.span(skip_parens(cx.ast(), callee));
            cx.report(span, "`Symbol` cannot be called as a constructor.")
                .id("noNewSymbol");
        }
    }
}
