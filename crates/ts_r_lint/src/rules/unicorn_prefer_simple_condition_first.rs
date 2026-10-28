//! `unicorn/prefer-simple-condition-first`: in an outermost `&&`/`||` chain used as a
//! condition, a simple operand (a name, `!` of one, `x == null`, `typeof x == "s"`, or a
//! strict comparison of names, `typeof` names and literals with a name on one side) after a
//! complex one should come first.
//!
//! A simple name after a `typeof name` guard for it (`!== "undefined"` in `&&`, `===` in
//! `||`) stays. When every complex operand before the last simple one is itself simple or a
//! `?:` of simple parts, the fix moves all simple operands to the front; otherwise the
//! report is the "unsafe" one without a fix. No fix when an operand chain sits in a TS
//! wrapper, or a comment lies between operands or right outside them.

use super::prelude::*;
use super::unicorn_conditions::{
    comment_next_to, is_ts_wrapper, logical_child_text, outermost_ts_wrapper, unwrap_ts_and_parens,
};
use super::unicorn_text::{paren_span, static_string_value, would_remove_comments};
use super::unicorn_util::is_boolean_context;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-simple-condition-first",
        "Prefer simple conditions first in logical expressions.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferSimpleConditionFirst>,
};

#[derive(Debug, Default)]
struct PreferSimpleConditionFirst;

const MAX_DEPTH: u32 = 200;

fn u(ast: &Ast, id: NodeId) -> NodeId {
    unwrap_ts_and_parens(ast, id)
}

fn is_ident(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(u(ast, id)), Node::Ident { .. })
}

/// `typeof name`, and the name.
fn typeof_ident(ast: &Ast, id: NodeId) -> Option<NodeId> {
    match *ast.node(u(ast, id)) {
        Node::Unary {
            op: UnaryOp::TypeOf,
            arg,
        } if is_ident(ast, arg) => Some(u(ast, arg)),
        _ => None,
    }
}

fn is_ident_or_typeof(ast: &Ast, id: NodeId) -> bool {
    is_ident(ast, id) || typeof_ident(ast, id).is_some()
}

fn is_literal(node: &Node) -> bool {
    matches!(
        node,
        Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
    )
}

fn is_static_string(ast: &Ast, id: NodeId) -> bool {
    static_string_value(ast, u(ast, id)).is_some()
}

fn is_simple_operand(ast: &Ast, id: NodeId) -> bool {
    let id = u(ast, id);
    if is_ident_or_typeof(ast, id) || is_literal(ast.node(id)) || is_static_string(ast, id) {
        return true;
    }
    let Node::Unary { op, arg } = *ast.node(id) else {
        return false;
    };
    match *ast.node(u(ast, arg)) {
        Node::NumberLit { .. } => matches!(op, UnaryOp::Minus | UnaryOp::Plus),
        Node::BigIntLit { .. } => op == UnaryOp::Minus,
        _ => false,
    }
}

fn is_null(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(u(ast, id)), Node::NullLit {})
}

fn is_simple(ast: &Ast, id: NodeId, depth: u32) -> bool {
    let id = u(ast, id);
    match *ast.node(id) {
        Node::Ident { .. } => true,
        Node::Unary {
            op: UnaryOp::Not,
            arg,
        } => depth < MAX_DEPTH && is_simple(ast, arg, depth + 1),
        Node::Binary { left, op, right } => match op {
            BinaryOp::Eq | BinaryOp::NotEq => {
                (is_ident(ast, left) && is_null(ast, right))
                    || (is_null(ast, left) && is_ident(ast, right))
                    || (typeof_ident(ast, left).is_some()
                        && (is_static_string(ast, right) || typeof_ident(ast, right).is_some()))
                    || (is_static_string(ast, left) && typeof_ident(ast, right).is_some())
            }
            BinaryOp::StrictEq | BinaryOp::StrictNotEq => {
                is_simple_operand(ast, left)
                    && is_simple_operand(ast, right)
                    && (is_ident_or_typeof(ast, left) || is_ident_or_typeof(ast, right))
            }
            _ => false,
        },
        _ => false,
    }
}

/// The operands of the `op` chain at `id`, looking through TS wrappers.
fn logical_operands(ast: &Ast, id: NodeId, op: BinaryOp, out: &mut Vec<NodeId>) {
    let mut stack = vec![skip_parens(ast, id)];
    while let Some(n) = stack.pop() {
        match *ast.node(u(ast, n)) {
            Node::Binary { left, op: o, right } if o == op && out.len() + stack.len() < 100_000 => {
                stack.push(skip_parens(ast, right));
                stack.push(skip_parens(ast, left));
            }
            _ => out.push(n),
        }
    }
}

fn is_typeof_guard_for(ast: &Ast, id: NodeId, name: &str, op: BinaryOp) -> bool {
    let Node::Binary {
        left,
        op: cmp,
        right,
    } = *ast.node(u(ast, id))
    else {
        return false;
    };
    let guard = if op == BinaryOp::LogicalAnd {
        matches!(cmp, BinaryOp::StrictNotEq | BinaryOp::NotEq)
    } else {
        matches!(cmp, BinaryOp::StrictEq | BinaryOp::Eq)
    };
    if !guard {
        return false;
    }
    let (typeof_side, other) = if typeof_ident(ast, left).is_some() {
        (left, right)
    } else {
        (right, left)
    };
    typeof_ident(ast, typeof_side).is_some_and(|arg| ident_name(ast, arg) == Some(name))
        && static_string_value(ast, u(ast, other)).as_deref() == Some("undefined")
}

fn has_earlier_typeof_guard(ast: &Ast, operands: &[NodeId], index: usize, op: BinaryOp) -> bool {
    let id = u(ast, operands[index]);
    let Some(name) = matches!(ast.node(id), Node::Ident { .. })
        .then(|| ident_name(ast, id))
        .flatten()
    else {
        return false;
    };
    operands[..index]
        .iter()
        .any(|&o| is_typeof_guard_for(ast, o, name, op))
}

fn is_safe_conditional(ast: &Ast, id: NodeId, depth: u32) -> bool {
    let Node::Conditional { test, cons, alt } = *ast.node(u(ast, id)) else {
        return false;
    };
    depth < MAX_DEPTH
        && [test, cons, alt].iter().all(|&c| {
            is_simple(ast, c, 0)
                || is_simple_operand(ast, c)
                || is_safe_conditional(ast, c, depth + 1)
        })
}

/// A TS wrapper around an `op` chain somewhere in the chain at `id`.
fn has_wrapped_chain(ast: &Ast, id: NodeId, op: BinaryOp, depth: u32) -> bool {
    let id = skip_parens(ast, id);
    if is_ts_wrapper(ast.node(id)) {
        return matches!(*ast.node(u(ast, id)), Node::Binary { op: o, .. } if o == op);
    }
    match *ast.node(id) {
        Node::Binary { left, op: o, right } if o == op && depth < MAX_DEPTH => {
            has_wrapped_chain(ast, left, op, depth + 1)
                || has_wrapped_chain(ast, right, op, depth + 1)
        }
        _ => false,
    }
}

impl Rule for PreferSimpleConditionFirst {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { op, .. } = *cx.node(node) else {
            return;
        };
        if !matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr) {
            return;
        }
        let outer = outermost_ts_wrapper(cx, node);
        if parent_skip_parens(ast, cx.file.parents, outer)
            .is_some_and(|p| matches!(*cx.node(p), Node::Binary { op: o, .. } if o == op))
            || !is_boolean_context(cx, outer)
        {
            return;
        }
        let mut operands = Vec::new();
        logical_operands(ast, node, op, &mut operands);
        let simple: Vec<bool> = operands.iter().map(|&o| is_simple(ast, o, 0)).collect();
        let Some(first_complex) = simple.iter().position(|s| !s) else {
            return;
        };
        let Some(misplaced) = (first_complex + 1..operands.len())
            .find(|&i| simple[i] && !has_earlier_typeof_guard(ast, &operands, i, op))
        else {
            return;
        };
        let last_simple = simple.iter().rposition(|&s| s).unwrap_or(0);
        let safe = operands.iter().enumerate().all(|(i, &o)| {
            simple[i] || i > last_simple || is_simple(ast, o, 0) || is_safe_conditional(ast, o, 0)
        });
        let fix = (safe
            && !has_wrapped_chain(ast, node, op, 0)
            && !would_remove_comments(
                cx,
                cx.span(node),
                &operands
                    .iter()
                    .map(|&o| paren_span(cx, o))
                    .collect::<Vec<_>>(),
            )
            && !comment_next_to(
                cx,
                paren_span(cx, operands[0]).start,
                paren_span(cx, operands[operands.len() - 1]).end,
            ))
        .then(|| {
            let reordered = operands
                .iter()
                .zip(&simple)
                .filter(|(_, s)| **s)
                .chain(operands.iter().zip(&simple).filter(|(_, s)| !**s))
                .map(|(&o, _)| logical_child_text(cx, o, op))
                .collect::<Vec<_>>();
            Fix::replace(cx.span(node), reordered.join(&format!(" {} ", op.as_str())))
        });
        let span = cx.span(operands[misplaced]);
        let report = if safe {
            cx.report(
                span,
                format!(
                    "Prefer this simple condition first in the `{}` expression.",
                    op.as_str()
                ),
            )
            .id("prefer-simple-condition-first")
        } else {
            cx.report(
                span,
                "Consider moving this simple condition first after verifying short-circuit behavior.",
            )
            .id("prefer-simple-condition-first/unsafe")
        };
        report.fix_opt(fix);
    }
}
