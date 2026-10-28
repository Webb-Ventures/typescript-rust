//! `unicorn/no-duplicate-logical-operands`: in `a && a` / `a || a` (also the last operand of
//! a same-operator chain, `x && a && a`) the right operand repeats its neighbour. Both must be
//! simple references: names, `this`, `super` and non-optional member chains whose computed
//! keys are names, `this`, literals or substitution-free templates. Skipped inside `with`.
//! The fix keeps the left side; it is only a suggestion unless both operands are bare names
//! or `this`, and nothing is offered where the expression is a callee, a tag or a `delete`
//! operand (it would change `this` or the reference) or when a comment would be lost.

use super::prelude::*;
use super::unicorn_text::{paren_span, paren_text, static_string_value};
use super::unicorn_util::is_same_reference;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-duplicate-logical-operands",
        "Disallow adjacent duplicate operands in logical expressions.",
        NO_PRESET,
        Category::Correctness,
    )
    .fixable()
    .suggestions(),
    create: create_default::<NoDuplicateLogicalOperands>,
};

#[derive(Debug, Default)]
struct NoDuplicateLogicalOperands;

fn unwrap(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => id = expr,
            _ => return id,
        }
    }
    id
}

fn is_simple_reference(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > 200 {
        return false;
    }
    let id = unwrap(ast, id);
    match *ast.node(id) {
        Node::Ident { .. } | Node::Super {} | Node::This {} => true,
        Node::Member { obj, optional, .. } => !optional && is_simple_reference(ast, obj, depth + 1),
        Node::Index {
            obj,
            optional,
            index,
        } => {
            let key = unwrap(ast, index);
            !optional
                && is_simple_reference(ast, obj, depth + 1)
                && (matches!(
                    ast.node(key),
                    Node::Ident { .. }
                        | Node::This {}
                        | Node::StringLit { .. }
                        | Node::NumberLit { .. }
                        | Node::BigIntLit { .. }
                        | Node::BoolLit { .. }
                        | Node::NullLit {}
                        | Node::RegexLit { .. }
                ) || static_string_value(ast, key).is_some())
        }
        _ => false,
    }
}

/// The expression (through parentheses and TS wrappers) is a callee, a tag or a `delete`
/// operand.
pub(crate) fn is_reference_context(cx: &Ctx<'_, '_>, mut id: NodeId) -> bool {
    let ast = cx.ast();
    for _ in 0..10_000 {
        let Some(p) = cx.parent(id) else {
            return false;
        };
        match *ast.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. } => id = p,
            Node::Call { callee, .. } => return callee == id,
            Node::TaggedTemplate { tag, .. } => return tag == id,
            Node::Unary {
                op: UnaryOp::Delete,
                ..
            } => return true,
            _ => return false,
        }
    }
    false
}

impl Rule for NoDuplicateLogicalOperands {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { left, op, right } = *cx.node(node) else {
            return;
        };
        if !matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr) {
            return;
        }
        let left_inner = skip_parens(ast, left);
        let adjacent = match *ast.node(left_inner) {
            Node::Binary {
                op: inner_op,
                right: r,
                ..
            } if inner_op == op => r,
            _ => left_inner,
        };
        if !is_simple_reference(ast, adjacent, 0)
            || !is_simple_reference(ast, right, 0)
            || !is_same_reference(cx, adjacent, right)
        {
            return;
        }
        let mut p = cx.parent(node);
        for _ in 0..100_000 {
            let Some(id) = p else { break };
            if matches!(ast.node(id), Node::With { .. }) {
                return;
            }
            p = cx.parent(id);
        }
        let right_span = cx.span(skip_parens(ast, right));
        let span = cx.span(node);
        let keep = paren_span(cx, left);
        let comments_lost = ast.comments_in(span).iter().any(|c| {
            c.span.start >= span.start
                && c.span.end <= span.end
                && !(c.span.start >= keep.start && c.span.end <= keep.end)
        });
        let message = "This operand duplicates the left operand.";
        if is_reference_context(cx, node) || comments_lost {
            cx.report(right_span, message);
            return;
        }
        let fix = Fix::replace(span, paren_text(cx, left));
        let bare = |id: NodeId| {
            matches!(
                ast.node(skip_parens(ast, id)),
                Node::Ident { .. } | Node::This {}
            )
        };
        let report = cx.report(right_span, message);
        if bare(adjacent) && bare(right) {
            report.fix(fix);
        } else {
            report.suggest(
                "no-duplicate-logical-operands/suggestion",
                "Remove the duplicate operand.",
                fix,
            );
        }
    }
}
