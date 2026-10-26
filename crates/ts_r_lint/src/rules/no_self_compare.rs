//! `no-self-compare`: disallow comparing an expression with itself.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-self-compare",
        "Disallow comparisons where both sides are exactly the same",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoSelfCompare>,
};

#[derive(Debug, Default)]
struct NoSelfCompare;

impl Rule for NoSelfCompare {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Binary { left, op, right } = *cx.node(node) else {
            return;
        };
        use BinaryOp::*;
        if !matches!(
            op,
            Eq | NotEq | StrictEq | StrictNotEq | Lt | LtEq | Gt | GtEq
        ) {
            return;
        }
        if same_tokens(cx.ast(), cx.src(), left, right) {
            cx.report(
                cx.span(node),
                "Comparing to itself is potentially pointless.",
            )
            .id("comparingToSelf");
        }
    }
}
