//! `@typescript-eslint/no-dynamic-delete`: disallow `delete` with computed key
//! expressions.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-dynamic-delete",
        "Disallow using the `delete` operator on computed key expressions",
        TS_STRICT,
        Category::Restriction,
    ),
    create: create_default::<NoDynamicDelete>,
};

#[derive(Debug, Default)]
struct NoDynamicDelete;

impl Rule for NoDynamicDelete {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Unary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Unary {
            op: UnaryOp::Delete,
            arg,
        } = *cx.node(node)
        else {
            return;
        };
        let Node::Index { index, .. } = *cx.node(skip_parens(ast, arg)) else {
            return;
        };
        let ok = match *cx.node(index) {
            Node::StringLit { .. } | Node::NumberLit { .. } => true,
            Node::Unary {
                op: UnaryOp::Minus,
                arg,
            } => matches!(cx.node(arg), Node::NumberLit { .. }),
            _ => false,
        };
        if !ok {
            cx.report_node(index, "Do not delete dynamically computed property keys.")
                .id("dynamicDelete");
        }
    }
}
