//! `no-eq-null`: disallow `== null` and `!= null`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-eq-null",
        "Disallow `null` comparisons without type-checking operators",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create_default::<NoEqNull>,
};

#[derive(Debug, Default)]
struct NoEqNull;

impl Rule for NoEqNull {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Binary { left, op, right } = *cx.node(node) else {
            return;
        };
        if !matches!(op, BinaryOp::Eq | BinaryOp::NotEq) {
            return;
        }
        let ast = cx.ast();
        let is_null = |id| matches!(ast.node(skip_parens(ast, id)), Node::NullLit {});
        if is_null(left) || is_null(right) {
            cx.report(cx.span(node), "Use '===' to compare with null.")
                .id("unexpected");
        }
    }
}
