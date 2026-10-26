//! `no-nested-ternary`: disallow a conditional expression in a branch of another.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-nested-ternary",
        "Disallow nested ternary expressions",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoNestedTernary>,
};

#[derive(Debug, Default)]
struct NoNestedTernary;

impl Rule for NoNestedTernary {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Conditional]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Conditional { cons, alt, .. } = *cx.node(node) else {
            return;
        };
        let ast = cx.ast();
        let nested = |id| matches!(ast.node(skip_parens(ast, id)), Node::Conditional { .. });
        if nested(cons) || nested(alt) {
            cx.report(cx.span(node), "Do not nest ternary expressions.")
                .id("noNestedTernary");
        }
    }
}
