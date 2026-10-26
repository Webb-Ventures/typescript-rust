//! `unicorn/no-subtraction-comparison`: `a - b > 0` (any ordering or equality operator, `0`
//! on either side) reads better as `a > b`.
//!
//! With the `0` on the left the ordering operator flips. The rewrite keeps both operands'
//! parentheses, wraps itself when `a` would start a statement, and adds a `;` when it would
//! continue the previous line. It is a fix when it can't change the result: for `<`/`>` both
//! operands are numbers; otherwise both are finite static numbers. Else it is a suggestion,
//! and nothing is offered when a comment sits outside the operands.

use super::prelude::*;
use super::unicorn_static::{Static, static_value_control_flow};
use super::unicorn_text::{
    is_parenthesized, needs_parens_at_expression_start, needs_semicolon, paren_span, paren_text,
    would_remove_comments,
};
use super::unicorn_types::is_number;
use super::unicorn_util::space_around_keyword;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-subtraction-comparison",
        "Prefer comparing values directly over subtracting and comparing to `0`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<NoSubtractionComparison>,
};

#[derive(Debug, Default)]
struct NoSubtractionComparison;

fn is_zero(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::NumberLit { value } if value.get() == 0.0)
}

fn subtraction(ast: &Ast, id: NodeId) -> Option<(NodeId, NodeId)> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Binary {
            left,
            op: BinaryOp::Sub,
            right,
        } => Some((left, right)),
        _ => None,
    }
}

/// The operator with its operands swapped.
fn mirrored(op: BinaryOp) -> BinaryOp {
    match op {
        BinaryOp::Gt => BinaryOp::Lt,
        BinaryOp::GtEq => BinaryOp::LtEq,
        BinaryOp::Lt => BinaryOp::Gt,
        BinaryOp::LtEq => BinaryOp::GtEq,
        other => other,
    }
}

fn is_finite_static_number(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    matches!(static_value_control_flow(cx, id), Some(Static::Num(n)) if n.is_finite())
}

impl Rule for NoSubtractionComparison {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary {
            left: l,
            op,
            right: r,
        } = *cx.node(node)
        else {
            return;
        };
        if !matches!(
            op,
            BinaryOp::Gt
                | BinaryOp::GtEq
                | BinaryOp::Lt
                | BinaryOp::LtEq
                | BinaryOp::StrictEq
                | BinaryOp::StrictNotEq
                | BinaryOp::Eq
                | BinaryOp::NotEq
        ) {
            return;
        }
        let ((left, right), op) = match (subtraction(ast, l), subtraction(ast, r)) {
            (Some(sub), _) if is_zero(ast, r) => (sub, op),
            (_, Some(sub)) if is_zero(ast, l) => (sub, mirrored(op)),
            _ => return,
        };
        let span = cx.span(node);
        let msg = "Prefer comparing the values directly over comparing the difference with `0`.";
        let id = "no-subtraction-comparison/error";
        if would_remove_comments(cx, span, &[paren_span(cx, left), paren_span(cx, right)]) {
            cx.report(span, msg).id(id);
            return;
        }
        let mut replacement = format!(
            "{} {} {}",
            paren_text(cx, left),
            op.as_str(),
            paren_text(cx, right)
        );
        if !is_parenthesized(cx, left) && needs_parens_at_expression_start(cx, left, node) {
            replacement = format!("({replacement})");
        }
        let text = if needs_semicolon(cx, span.start, &replacement) {
            format!(";{replacement}")
        } else {
            replacement.clone()
        };
        let mut fix = Fix::replace(span, text);
        for (s, t) in space_around_keyword(cx, node) {
            fix = fix.and(s, t);
        }
        let can_fix = if matches!(op, BinaryOp::Gt | BinaryOp::Lt) {
            is_number(cx, left) && is_number(cx, right)
        } else {
            is_finite_static_number(cx, left) && is_finite_static_number(cx, right)
        };
        let report = cx.report(span, msg).id(id);
        if can_fix {
            report.fix(fix);
        } else {
            report.suggest(
                "no-subtraction-comparison/suggestion",
                format!("Replace with `{replacement}`."),
                fix,
            );
        }
    }
}
