//! `unicorn/prefer-unary-minus`: `x * -1`, `-1 * x` and `x / -1` (but not `-1 / x`, and not
//! when both sides are `-1`) are `-x`. The fix writes `-x`, `-(x)` when `x` binds looser than
//! a unary operator or is an update, `!`-assertion, `<T>` assertion or a negation; wraps the
//! result where a unary expression can't stand unparenthesized; adds `;` or a space before
//! it when it would join the previous statement or a `-` token; and spaces it from a glued
//! keyword. It is withheld when a comment outside `x` would be lost.

use super::prelude::*;
use super::unicorn_text::{needs_semicolon, unary_replacement_needs_parens, would_remove_comments};
use super::unicorn_util::{space_around_keyword, with_edits};
use crate::ast_util::precedence;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-unary-minus",
        "Prefer the unary minus operator over multiplying or dividing by `-1`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferUnaryMinus>,
};

#[derive(Debug, Default)]
struct PreferUnaryMinus;

fn is_negative_one(ast: &Ast, id: NodeId) -> bool {
    let id = skip_parens(ast, id);
    matches!(*ast.node(id), Node::Unary { op: UnaryOp::Minus, arg }
        if matches!(*ast.node(skip_parens(ast, arg)), Node::NumberLit { value } if value.get() == 1.0))
}

impl Rule for PreferUnaryMinus {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { left, op, right } = *cx.node(node) else {
            return;
        };
        let operand = match op {
            BinaryOp::Mul if is_negative_one(ast, right) && !is_negative_one(ast, left) => left,
            BinaryOp::Mul if is_negative_one(ast, left) && !is_negative_one(ast, right) => right,
            BinaryOp::Div if is_negative_one(ast, right) && !is_negative_one(ast, left) => left,
            _ => return,
        };
        let operand = skip_parens(ast, operand);
        let span = cx.span(node);
        let fix = (!would_remove_comments(cx, span, &[cx.span(operand)])).then(|| {
            let text = cx.text(operand);
            let negated = matches!(
                *ast.node(operand),
                Node::Unary {
                    op: UnaryOp::Minus,
                    ..
                }
            );
            let parens = precedence(ast, operand) < 16
                || negated
                || matches!(
                    ast.node(operand),
                    Node::Update { .. } | Node::NonNull { .. } | Node::TypeAssertion { .. }
                );
            let mut replacement = if parens {
                format!("-({text})")
            } else {
                format!("-{text}")
            };
            if unary_replacement_needs_parens(cx, node) {
                replacement = format!("({replacement})");
            }
            if needs_semicolon(cx, span.start, &replacement) {
                replacement = format!(";{replacement}");
            } else if span.start > 0 && cx.src().as_bytes()[span.start as usize - 1] == b'-' {
                replacement = format!(" {replacement}");
            }
            with_edits(
                Fix::replace(span, replacement),
                space_around_keyword(cx, node),
            )
        });
        let operation = if op == BinaryOp::Mul {
            "multiplying"
        } else {
            "dividing"
        };
        cx.report(
            span,
            format!("Prefer the unary minus operator over {operation} by `-1`."),
        )
        .fix_opt(fix);
    }
}
