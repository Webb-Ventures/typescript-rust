//! `no-throw-literal`: disallow throwing values that cannot be `Error` objects.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-throw-literal",
        "Disallow throwing literals as exceptions",
        NO_PRESET,
        Category::Pedantic,
    ),
    create: create_default::<NoThrowLiteral>,
};

#[derive(Debug, Default)]
struct NoThrowLiteral;

/// True when evaluating `id` might produce an `Error` object.
pub(crate) fn could_be_error(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > 500 {
        return true;
    }
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::Ident { .. }
        | Node::Call { .. }
        | Node::New { .. }
        | Node::Member { .. }
        | Node::Index { .. }
        | Node::TaggedTemplate { .. }
        | Node::Yield { .. }
        | Node::Await { .. } => true,
        Node::Assign { left, op, right } => match op {
            AssignOp::Assign | AssignOp::AndAssign => could_be_error(ast, right, depth + 1),
            AssignOp::OrAssign | AssignOp::CoalesceAssign => {
                could_be_error(ast, left, depth + 1) || could_be_error(ast, right, depth + 1)
            }
            _ => false,
        },
        Node::Seq { exprs } => ast
            .list(exprs)
            .last()
            .is_some_and(|&e| could_be_error(ast, e, depth + 1)),
        Node::Binary {
            left,
            op: op @ (BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce),
            right,
        } => {
            (op != BinaryOp::LogicalAnd && could_be_error(ast, left, depth + 1))
                || could_be_error(ast, right, depth + 1)
        }
        Node::Conditional { cons, alt, .. } => {
            could_be_error(ast, cons, depth + 1) || could_be_error(ast, alt, depth + 1)
        }
        _ => false,
    }
}

impl Rule for NoThrowLiteral {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Throw]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Throw { arg } = *cx.node(node) else {
            return;
        };
        if !could_be_error(cx.ast(), arg, 0) {
            cx.report(cx.span(node), "Expected an error object to be thrown.")
                .id("object");
        } else if cx.is_global_ref(arg, "undefined") {
            cx.report(cx.span(node), "Do not throw undefined.")
                .id("undef");
        }
    }
}
