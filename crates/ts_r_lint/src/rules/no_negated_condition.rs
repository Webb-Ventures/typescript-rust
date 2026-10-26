//! `no-negated-condition`: disallow negated conditions that have an `else` branch.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-negated-condition",
        "Disallow negated conditions",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoNegatedCondition>,
};

#[derive(Debug, Default)]
struct NoNegatedCondition;

fn is_negated(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(skip_parens(ast, id)),
        Node::Unary {
            op: UnaryOp::Not,
            ..
        } | Node::Binary {
            op: BinaryOp::NotEq | BinaryOp::StrictNotEq,
            ..
        }
    )
}

impl Rule for NoNegatedCondition {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::If, NodeKind::Conditional]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let test = match *ast.node(node) {
            Node::If {
                test,
                alt: Some(alt),
                ..
            } if !matches!(ast.node(alt), Node::If { .. }) => test,
            Node::Conditional { test, .. } => test,
            _ => return,
        };
        if is_negated(ast, test) {
            cx.report(cx.span(node), "Unexpected negated condition.")
                .id("unexpectedNegated");
        }
    }
}
