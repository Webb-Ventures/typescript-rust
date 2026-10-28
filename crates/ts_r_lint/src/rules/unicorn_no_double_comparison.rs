//! `unicorn/no-double-comparison`: two comparisons of the same operands (in either order)
//! joined by `||` / `&&` that one operator expresses: `a === b || a < b` is `a <= b`,
//! `a < b || a > b` is `a !== b`, `a <= b && a >= b` is `a === b`, `a <= b && a !== b` is
//! `a < b`, and the mirrored forms. Skipped when a comparison holds an optional chain or an
//! operand of the first one is statically a regular expression. The suggestion reuses the
//! first comparison's operands (an unparenthesized `as` / `satisfies` right operand of a
//! relational result is wrapped) unless a comment outside them would be lost.

use super::prelude::*;
use super::unicorn_ast::contains_node;
use super::unicorn_static::{Static, static_value_control_flow};
use super::unicorn_text::{is_parenthesized, needs_semicolon, paren_span, paren_text};
use super::unicorn_util::is_same_reference;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-double-comparison",
        "Disallow two comparisons of the same operands that can be combined into one.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<NoDoubleComparison>,
};

#[derive(Debug, Default)]
struct NoDoubleComparison;

fn unwrap_ts(ast: &Ast, mut id: NodeId) -> NodeId {
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

fn op_str(op: BinaryOp) -> Option<&'static str> {
    Some(match op {
        BinaryOp::Lt => "<",
        BinaryOp::LtEq => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::GtEq => ">=",
        BinaryOp::StrictEq => "===",
        BinaryOp::StrictNotEq => "!==",
        _ => return None,
    })
}

fn flip(op: &str) -> &'static str {
    match op {
        "<" => ">",
        "<=" => ">=",
        ">" => "<",
        ">=" => "<=",
        "===" => "===",
        _ => "!==",
    }
}

fn reduce(logical: BinaryOp, a: &str, b: &str) -> Option<&'static str> {
    let pair = |x: &str, y: &str| (a == x && b == y) || (a == y && b == x);
    match logical {
        BinaryOp::LogicalOr if pair("===", "<") => Some("<="),
        BinaryOp::LogicalOr if pair("===", ">") => Some(">="),
        BinaryOp::LogicalOr if pair("<", ">") => Some("!=="),
        BinaryOp::LogicalAnd if pair("<=", ">=") => Some("==="),
        BinaryOp::LogicalAnd if pair("<=", "!==") => Some("<"),
        BinaryOp::LogicalAnd if pair(">=", "!==") => Some(">"),
        _ => None,
    }
}

/// A comparison without optional chaining: its operator and operands.
fn comparison(ast: &Ast, id: NodeId) -> Option<(&'static str, NodeId, NodeId)> {
    let Node::Binary { left, op, right } = *ast.node(id) else {
        return None;
    };
    let op = op_str(op)?;
    let optional = contains_node(ast, id, |n| {
        matches!(
            n,
            Node::Member { optional: true, .. }
                | Node::Index { optional: true, .. }
                | Node::Call { optional: true, .. }
        )
    });
    (!optional).then_some((op, left, right))
}

impl Rule for NoDoubleComparison {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { left, op, right } = *cx.node(node) else {
            return;
        };
        if !matches!(op, BinaryOp::LogicalOr | BinaryOp::LogicalAnd) {
            return;
        }
        let (Some((lop, ll, lr)), Some((rop, rl, rr))) = (
            comparison(ast, unwrap_ts(ast, left)),
            comparison(ast, unwrap_ts(ast, right)),
        ) else {
            return;
        };
        let right_op = if is_same_reference(cx, ll, rl) && is_same_reference(cx, lr, rr) {
            rop
        } else if is_same_reference(cx, ll, rr) && is_same_reference(cx, lr, rl) {
            flip(rop)
        } else {
            return;
        };
        let Some(result) = reduce(op, lop, right_op) else {
            return;
        };
        let is_regexp = |id: NodeId| {
            matches!(
                static_value_control_flow(cx, id),
                Some(Static::RegExp { .. })
            )
        };
        if is_regexp(ll) || is_regexp(lr) {
            return;
        }
        let left_text = paren_text(cx, ll);
        let lr_inner = skip_parens(ast, lr);
        let mut right_text = paren_text(cx, lr).to_owned();
        if !is_parenthesized(cx, lr_inner)
            && matches!(result, "<" | "<=" | ">" | ">=")
            && matches!(ast.node(lr_inner), Node::As { .. } | Node::Satisfies { .. })
        {
            right_text = format!("({right_text})");
        }
        let replacement = format!("{left_text} {result} {right_text}");
        let span = cx.span(node);
        let kept = [paren_span(cx, ll), paren_span(cx, lr)];
        let dropped = ast.comments_in(span).iter().any(|c| {
            c.span.start >= span.start
                && c.span.end <= span.end
                && kept
                    .iter()
                    .all(|k| !(c.span.start >= k.start && c.span.end <= k.end))
        });
        let fix = (!dropped).then(|| {
            let text = if needs_semicolon(cx, span.start, &replacement) {
                format!(";{replacement}")
            } else {
                replacement.clone()
            };
            Fix::replace(span, text)
        });
        let report = cx.report(
            span,
            format!("These comparisons can be simplified to `{replacement}`."),
        );
        if let Some(fix) = fix {
            report.suggest(
                "no-double-comparison/suggestion",
                format!("Replace with `{replacement}`."),
                fix,
            );
        }
    }
}
