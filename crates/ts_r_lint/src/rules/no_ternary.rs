//! `no-ternary`: disallow conditional expressions.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-ternary",
        "Disallow ternary operators",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoTernary>,
};

#[derive(Debug, Default)]
struct NoTernary;

impl Rule for NoTernary {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Conditional]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        cx.report_node(node, "Ternary operator used.")
            .id("noTernaryOperator");
    }
}
