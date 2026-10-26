//! `unicorn/no-unnecessary-nested-ternary`: `a ? (b ? x : y) : y` is `a && b ? x : y`, and
//! `a ? x : (b ? x : y)` is `a || b ? x : y` (branches compared by source text). The fix
//! rewrites the expression, parenthesizing operands as logical and conditional children
//! need, adding a leading `;` when needed, and is withheld when a comment would be lost.

use super::prelude::*;
use super::unicorn_text::{
    is_parenthesized, needs_semicolon, paren_span, paren_text, would_remove_comments,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unnecessary-nested-ternary",
        "Disallow unnecessary nested ternary expressions.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoUnnecessaryNestedTernary>,
};

#[derive(Debug, Default)]
struct NoUnnecessaryNestedTernary;

fn logical_child_text(cx: &Ctx<'_, '_>, id: NodeId, op: BinaryOp) -> String {
    let text = paren_text(cx, id);
    let needs = !is_parenthesized(cx, id)
        && match *cx.node(id) {
            Node::Binary { op: o, .. } => o != op,
            Node::Await { .. }
            | Node::Conditional { .. }
            | Node::Assign { .. }
            | Node::Arrow { .. }
            | Node::Yield { .. }
            | Node::Seq { .. } => true,
            _ => false,
        };
    if needs {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

fn conditional_child_text(cx: &Ctx<'_, '_>, id: NodeId) -> String {
    let text = paren_text(cx, id);
    let needs = !is_parenthesized(cx, id)
        && matches!(
            cx.node(id),
            Node::Await { .. }
                | Node::Assign { .. }
                | Node::Yield { .. }
                | Node::Seq { .. }
                | Node::As { .. }
                | Node::TypeAssertion { .. }
        );
    if needs {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

impl Rule for NoUnnecessaryNestedTernary {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Conditional]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Conditional { test, cons, alt } = *cx.node(node) else {
            return;
        };
        let (cons, alt) = (skip_parens(ast, cons), skip_parens(ast, alt));
        let test = skip_parens(ast, test);
        let parts = match (*ast.node(cons), *ast.node(alt)) {
            (
                Node::Conditional {
                    test: t,
                    cons: c,
                    alt: a,
                },
                _,
            ) if cx.text(skip_parens(ast, a)) == cx.text(alt) => Some((
                BinaryOp::LogicalAnd,
                test,
                skip_parens(ast, t),
                skip_parens(ast, c),
                skip_parens(ast, a),
            )),
            (
                _,
                Node::Conditional {
                    test: t,
                    cons: c,
                    alt: a,
                },
            ) if cx.text(cons) == cx.text(skip_parens(ast, c)) => Some((
                BinaryOp::LogicalOr,
                test,
                skip_parens(ast, t),
                cons,
                skip_parens(ast, a),
            )),
            _ => None,
        };
        let Some((op, left, right, yes, no)) = parts else {
            return;
        };
        let span = cx.span(node);
        let kept: Vec<Span> = [left, right, yes, no]
            .iter()
            .map(|&n| paren_span(cx, n))
            .collect();
        let fix = (!would_remove_comments(cx, span, &kept)).then(|| {
            let operator = if op == BinaryOp::LogicalAnd {
                "&&"
            } else {
                "||"
            };
            let mut text = format!(
                "{} {operator} {} ? {} : {}",
                logical_child_text(cx, left, op),
                logical_child_text(cx, right, op),
                conditional_child_text(cx, yes),
                conditional_child_text(cx, no)
            );
            if needs_semicolon(cx, span.start, &text) {
                text.insert(0, ';');
            }
            Fix::replace(span, text)
        });
        cx.report(span, "Do not use unnecessary nested ternary expressions.")
            .id("no-unnecessary-nested-ternary")
            .fix_opt(fix);
    }
}
