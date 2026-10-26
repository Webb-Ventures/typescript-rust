//! `unicorn/no-misrefactored-assignment`: `a += a + b` (any arithmetic or bitwise compound
//! assignment whose right side applies the same operator to the target; for the commutative
//! `+`, `*`, `&`, `|`, `^` also `a += b + a`) repeats the target. The suggestion keeps the
//! other operand, with its parentheses when it binds looser than `+`, unless comments
//! outside it would be lost.

use super::prelude::*;
use super::unicorn_text::{paren_span, would_remove_comments};
use super::unicorn_util::is_same_reference;
use crate::ast_util::precedence;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-misrefactored-assignment",
        "Disallow misrefactored compound assignments where the target is duplicated in the right-hand side.",
        NO_PRESET,
        Category::Correctness,
    )
    .suggestions(),
    create: create_default::<NoMisrefactoredAssignment>,
};

#[derive(Debug, Default)]
struct NoMisrefactoredAssignment;

impl Rule for NoMisrefactoredAssignment {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Assign]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Assign { left, op, right } = *cx.node(node) else {
            return;
        };
        let (bin, commutative) = match op {
            AssignOp::AddAssign => (BinaryOp::Add, true),
            AssignOp::SubAssign => (BinaryOp::Sub, false),
            AssignOp::MulAssign => (BinaryOp::Mul, true),
            AssignOp::DivAssign => (BinaryOp::Div, false),
            AssignOp::ModAssign => (BinaryOp::Mod, false),
            AssignOp::ExpAssign => (BinaryOp::Exp, false),
            AssignOp::BitAndAssign => (BinaryOp::BitAnd, true),
            AssignOp::BitOrAssign => (BinaryOp::BitOr, true),
            AssignOp::BitXorAssign => (BinaryOp::BitXor, true),
            AssignOp::ShlAssign => (BinaryOp::Shl, false),
            AssignOp::ShrAssign => (BinaryOp::Shr, false),
            AssignOp::UShrAssign => (BinaryOp::UShr, false),
            _ => return,
        };
        let right = skip_parens(ast, right);
        let Node::Binary {
            left: rl,
            op: rop,
            right: rr,
        } = *ast.node(right)
        else {
            return;
        };
        if rop != bin {
            return;
        }
        let kept = if is_same_reference(cx, left, rl) {
            rr
        } else if commutative && is_same_reference(cx, left, rr) {
            rl
        } else {
            return;
        };
        let kept_inner = skip_parens(ast, kept);
        let kept_span = if precedence(ast, kept_inner) < 12 {
            paren_span(cx, kept_inner)
        } else {
            cx.span(kept_inner)
        };
        let right_span = cx.span(right);
        let target = cx.text(skip_parens(ast, left)).to_owned();
        let fix = (!would_remove_comments(cx, right_span, &[kept_span]))
            .then(|| Fix::replace(right_span, kept_span.text(cx.src()).to_owned()));
        let span = cx.span(node);
        let report = cx
            .report(
                span,
                format!("`{target}` is duplicated in the right-hand side, which is likely a refactoring mistake."),
            )
            .id("no-misrefactored-assignment/error");
        if let Some(fix) = fix {
            report.suggest(
                "no-misrefactored-assignment/suggestion",
                format!("Remove the duplicated `{target}`."),
                fix,
            );
        }
    }
}
