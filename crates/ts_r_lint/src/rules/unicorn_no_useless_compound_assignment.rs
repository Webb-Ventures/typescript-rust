//! `unicorn/no-useless-compound-assignment`: `x += 0`, `x -= 0`, `x *= 1`, `x /= 1` and
//! `x **= 1` (a plain number literal; not `0n`, `-0` or names) change nothing. As a
//! statement in a program, block, static block or `case`, the suggestion removes it;
//! where its value is used, it becomes `x`. Nothing is offered for a callee, tag or `delete`
//! operand, or when a comment would be lost.

use super::prelude::*;
use super::unicorn_ast::remove_statements;
use super::unicorn_no_duplicate_logical_operands::is_reference_context;
use super::unicorn_text::{paren_span, paren_text, would_remove_comments};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-compound-assignment",
        "Disallow useless compound assignments such as `x += 0`.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<NoUselessCompoundAssignment>,
};

#[derive(Debug, Default)]
struct NoUselessCompoundAssignment;

impl Rule for NoUselessCompoundAssignment {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Assign]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Assign { left, op, right } = *cx.node(node) else {
            return;
        };
        let (operator, want) = match op {
            AssignOp::AddAssign => ("+=", 0.0),
            AssignOp::SubAssign => ("-=", 0.0),
            AssignOp::MulAssign => ("*=", 1.0),
            AssignOp::DivAssign => ("/=", 1.0),
            AssignOp::ExpAssign => ("**=", 1.0),
            _ => return,
        };
        let right = skip_parens(ast, right);
        let Node::NumberLit { value } = *ast.node(right) else {
            return;
        };
        if value.get() != want {
            return;
        }
        let raw = cx.text(right).to_owned();
        let message = format!(
            "This compound assignment is useless as `{operator} {raw}` does not change the value."
        );
        let mut outer = node;
        while let Some(p) = cx.parent(outer) {
            if !matches!(ast.node(p), Node::Paren { .. }) {
                break;
            }
            outer = p;
        }
        let stmt = cx
            .parent(outer)
            .filter(|&p| matches!(ast.node(p), Node::ExprStmt { .. }))
            .filter(|&p| {
                cx.parent(p).is_some_and(|g| {
                    matches!(
                        ast.node(g),
                        Node::Program { .. }
                            | Node::Block { .. }
                            | Node::StaticBlock { .. }
                            | Node::SwitchCase { .. }
                    )
                })
            });
        let span = cx.span(node);
        let blocked = is_reference_context(cx, node)
            || match stmt {
                Some(s) => would_remove_comments(cx, cx.span(s), &[]),
                None => would_remove_comments(cx, span, &[paren_span(cx, left)]),
            };
        let suggestion = (!blocked).then(|| match stmt {
            Some(s) => (
                "no-useless-compound-assignment/remove",
                "Remove the useless assignment.".to_owned(),
                remove_statements(cx, s, s, false),
            ),
            None => {
                let left_text = cx.text(skip_parens(ast, left));
                (
                    "no-useless-compound-assignment/replace",
                    format!("Replace the useless assignment with `{left_text}`."),
                    Fix::replace(span, paren_text(cx, left).to_owned()),
                )
            }
        });
        let report = cx
            .report(span, message)
            .id("no-useless-compound-assignment/error");
        if let Some((id, desc, fix)) = suggestion {
            report.suggest(id, desc, fix);
        }
    }
}
