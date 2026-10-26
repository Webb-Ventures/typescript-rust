//! `unicorn/no-accidental-bitwise-operator`: a bitwise operator that was almost surely
//! meant as its logical one: `x & x.y` / `x & x[y]` (a guard; the member not optional), or
//! `|` / `|=` whose right side (through parentheses) can't be a number: an object, array,
//! class, template, function or arrow, string or boolean. The operator is reported with a
//! suggestion to write `&&` / `||` / `||=`; an unparenthesized operand of another binary
//! operator, of `??`, or (for `||`) of `&&` is then wrapped, with a `;` before when the
//! previous statement would continue into the `(`.

use super::prelude::*;
use super::unicorn_text::{is_parenthesized, needs_semicolon};
use ts_r_ast::Ast;
use ts_r_span::Span;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-accidental-bitwise-operator",
        "Disallow bitwise operators where a logical operator was likely intended.",
        NO_PRESET,
        Category::Suspicious,
    )
    .suggestions(),
    create: create_default::<NoAccidentalBitwiseOperator>,
};

#[derive(Debug, Default)]
struct NoAccidentalBitwiseOperator;

fn is_definitely_non_numeric(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(skip_parens(ast, id)),
        Node::ObjectLit { .. }
            | Node::ArrayLit { .. }
            | Node::ClassExpr { .. }
            | Node::TemplateLit { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::StringLit { .. }
            | Node::BoolLit { .. }
    )
}

fn is_guard(ast: &Ast, left: NodeId, right: NodeId) -> bool {
    let Some(name) = ident_name(ast, skip_parens(ast, left)) else {
        return false;
    };
    match *ast.node(skip_parens(ast, right)) {
        Node::Member {
            obj,
            optional: false,
            ..
        }
        | Node::Index {
            obj,
            optional: false,
            ..
        } => ident_name(ast, skip_parens(ast, obj)) == Some(name),
        _ => false,
    }
}

impl Rule for NoAccidentalBitwiseOperator {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary, NodeKind::Assign]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (left, right, op, logical) = match *cx.node(node) {
            Node::Binary {
                left,
                op: BinaryOp::BitAnd,
                right,
            } if is_guard(ast, left, right) => (left, right, "&", "&&"),
            Node::Binary {
                left,
                op: BinaryOp::BitOr,
                right,
            } => (left, right, "|", "||"),
            Node::Assign {
                left,
                op: AssignOp::BitOrAssign,
                right,
            } => (left, right, "|=", "||="),
            _ => return,
        };
        if op != "&" && !is_definitely_non_numeric(ast, right) {
            return;
        }
        // The operator: the first token after the left operand and its parentheses.
        let src = cx.src();
        let mut at = cx.span(left).end as usize;
        loop {
            at = skip_trivia_forward(src, at);
            if src[at..].starts_with(')') {
                at += 1;
            } else {
                break;
            }
        }
        if !src[at..].starts_with(op) {
            return;
        }
        let token = Span::new(at as u32, (at + op.len()) as u32);
        let mut fix = Fix::replace(token, logical);
        if !is_parenthesized(cx, node)
            && let Some(parent) = cx.parent(node)
            && let Node::Binary { op: parent_op, .. } = *cx.node(parent)
        {
            let wrap = match parent_op {
                BinaryOp::LogicalAnd => logical == "||",
                BinaryOp::LogicalOr => false,
                _ => true,
            };
            if wrap {
                let span = cx.span(node);
                let prefix = if needs_semicolon(cx, span.start, "(") {
                    ";("
                } else {
                    "("
                };
                fix = fix
                    .and(Span::at(span.start), prefix)
                    .and(Span::at(span.end), ")");
            }
        }
        cx.report(
            token,
            format!(
                "Unexpected bitwise operator `{op}`. Did you mean the logical operator `{logical}`?"
            ),
        )
        .id("no-accidental-bitwise-operator/error")
        .suggest(
            "no-accidental-bitwise-operator/suggestion",
            format!("Replace `{op}` with `{logical}`."),
            fix,
        );
    }
}
