//! `no-delete-var`: disallow deleting variables.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-delete-var",
        "Disallow deleting variables",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoDeleteVar>,
};

#[derive(Debug, Default)]
struct NoDeleteVar;

impl Rule for NoDeleteVar {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Unary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Unary {
            op: UnaryOp::Delete,
            arg,
        } = *cx.node(node)
        else {
            return;
        };
        if matches!(cx.node(skip_parens(cx.ast(), arg)), Node::Ident { .. }) {
            cx.report_node(node, "Variables should not be deleted.")
                .id("unexpected");
        }
    }
}
