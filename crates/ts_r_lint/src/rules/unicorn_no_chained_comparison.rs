//! `unicorn/no-chained-comparison`: a comparison (`<`, `>`, `<=`, `>=`, `==`, `!=`, `===`,
//! `!==`) with another comparison as an operand, such as `a < b < c`. An equality whose
//! other operand is a boolean (`(a < b) === flag`, `=== true`, `!== Boolean(b)`, typed or
//! inferred) is intentional and skipped.
//!
//! Only `(a OP1 b) OP2 c` with both operators of the same kind (ordering or equality), a
//! right operand that is neither a boolean literal nor a comparison, a plain `a`, and a
//! side-effect-free middle gets the suggestion `a OP1 b && b OP2 c`; not where an
//! operator around it would capture part of the `&&` (an unparenthesized operand of a
//! binary, `??`, `as` or `satisfies`), nor when a comment outside `a` and `c` would be
//! lost.

use super::prelude::*;
use super::unicorn_static::has_side_effect;
use super::unicorn_text::{
    is_parenthesized, needs_parens_at_expression_start, paren_span, paren_text,
    would_remove_comments,
};
use super::unicorn_types::is_boolean;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-chained-comparison",
        "Disallow chained comparisons such as `a < b < c`.",
        NO_PRESET,
        Category::Correctness,
    )
    .suggestions(),
    create: create_default::<NoChainedComparison>,
};

#[derive(Debug, Default)]
struct NoChainedComparison;

fn ordering(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Lt | BinaryOp::LtEq | BinaryOp::Gt | BinaryOp::GtEq
    )
}

fn equality(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Eq | BinaryOp::NotEq | BinaryOp::StrictEq | BinaryOp::StrictNotEq
    )
}

/// The comparison operator of `id` (parentheses skipped), if it is one.
fn comparison_op(ast: &Ast, id: NodeId) -> Option<BinaryOp> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Binary { op, .. } if ordering(op) || equality(op) => Some(op),
        _ => None,
    }
}

fn op_text(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Lt => "<",
        BinaryOp::LtEq => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::GtEq => ">=",
        BinaryOp::Eq => "==",
        BinaryOp::NotEq => "!=",
        BinaryOp::StrictEq => "===",
        _ => "!==",
    }
}

/// The `&&` rewrite can stand where `node` is.
fn safe_for_logical(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    if is_parenthesized(cx, node) {
        return true;
    }
    cx.parent(node).is_none_or(|p| match cx.node(p) {
        Node::Binary { op, .. } => matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr),
        Node::As { .. } | Node::Satisfies { .. } => false,
        _ => true,
    })
}

impl Rule for NoChainedComparison {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { left, op, right } = *cx.node(node) else {
            return;
        };
        if !ordering(op) && !equality(op) {
            return;
        }
        let nested_on_left = comparison_op(ast, left).is_some();
        if !nested_on_left && comparison_op(ast, right).is_none() {
            return;
        }
        let sibling = if nested_on_left { right } else { left };
        if equality(op) && is_boolean(cx, sibling) {
            return;
        }
        let msg = "Comparison operators cannot be chained. The inner comparison evaluates to a boolean, which is then compared instead of the operands.";
        let id = "no-chained-comparison/error";
        let span = cx.span(node);
        let nested = skip_parens(ast, left);
        let suggestion = (|| {
            let Node::Binary {
                left: first,
                op: inner_op,
                right: middle,
            } = *ast.node(nested)
            else {
                return None;
            };
            let plausible = nested_on_left
                && ordering(inner_op) == ordering(op)
                && !matches!(ast.node(skip_parens(ast, right)), Node::BoolLit { .. })
                && comparison_op(ast, right).is_none()
                && comparison_op(ast, first).is_none()
                && safe_for_logical(cx, node)
                && !has_side_effect(ast, middle)
                && !would_remove_comments(
                    cx,
                    span,
                    &[paren_span(cx, first), paren_span(cx, right)],
                );
            if !plausible {
                return None;
            }
            let middle_text = paren_text(cx, middle);
            let mut replacement = format!(
                "{} && {middle_text} {} {}",
                cx.text(nested),
                op_text(op),
                paren_text(cx, right)
            );
            if needs_parens_at_expression_start(cx, nested, node) {
                replacement = format!("({replacement})");
            }
            Some(replacement)
        })();
        let report = cx.report(span, msg).id(id);
        if let Some(replacement) = suggestion {
            report.suggest(
                "no-chained-comparison/suggestion",
                format!("Replace with `{replacement}`."),
                Fix::replace(span, replacement),
            );
        }
    }
}
