//! `no-useless-concat`: disallow `+` between two string literals on the same line.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-useless-concat",
        "Disallow unnecessary concatenation of literals or template literals",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoUselessConcat>,
};

#[derive(Debug, Default)]
struct NoUselessConcat;

fn is_concat(ast: &Ast, id: NodeId) -> Option<(NodeId, NodeId)> {
    match *ast.node(id) {
        Node::Binary {
            left,
            op: BinaryOp::Add,
            right,
        } => Some((left, right)),
        _ => None,
    }
}

/// The operand next to the operator: the rightmost leaf of a left-hand concatenation, or
/// the leftmost leaf of a right-hand one.
fn edge(ast: &Ast, mut id: NodeId, rightmost: bool) -> NodeId {
    for _ in 0..10_000 {
        id = skip_parens(ast, id);
        match is_concat(ast, id) {
            Some((l, r)) => id = if rightmost { r } else { l },
            None => break,
        }
    }
    id
}

fn is_string(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::StringLit { .. } | Node::TemplateLit { .. }
    )
}

impl Rule for NoUselessConcat {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some((left, right)) = is_concat(ast, node) else {
            return;
        };
        let l = edge(ast, left, true);
        let r = edge(ast, right, false);
        if !is_string(ast, l) || !is_string(ast, r) {
            return;
        }
        let src = cx.src();
        if has_newline(src, cx.span(l).end, cx.span(r).start) {
            return;
        }
        let op = skip_trivia_forward(src, cx.span(left).end as usize) as u32;
        cx.report(
            Span::new(op, op + 1),
            "Unexpected string concatenation of literals.",
        )
        .id("unexpectedConcat");
    }
}
