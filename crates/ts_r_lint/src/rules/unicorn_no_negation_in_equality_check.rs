//! `unicorn/no-negation-in-equality-check`: `!a === b` (and `!==`, `==`, `!=`) reads as
//! `!(a === b)` but compares `!a`. The `!` is reported (not `!!a`); the suggestion drops it
//! and flips the operator, spacing a glued keyword (`return!a` -> `return a`),
//! parenthesizing a `return`/`throw` argument that then starts on a later line and an
//! operand that would open a statement or arrow body with `{`/`function`, and adding `;`
//! where the statement could join the previous one. Under an unparenthesized `yield` whose
//! operand starts on a later line there is no suggestion.

use super::prelude::*;
use super::unicorn_no_instanceof_builtins::starts_continued_statement;
use super::unicorn_no_typeof_undefined::{keyword_argument_parent, start_needs_parens};
use super::unicorn_util::{space_around_keyword, with_edits};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-negation-in-equality-check",
        "Disallow negated expression in equality check.",
        NO_PRESET,
        Category::Suspicious,
    )
    .suggestions(),
    create: create_default::<NoNegationInEqualityCheck>,
};

#[derive(Debug, Default)]
struct NoNegationInEqualityCheck;

fn is_not(ast: &ts_r_ast::Ast, id: NodeId) -> Option<NodeId> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Unary {
            op: UnaryOp::Not,
            arg,
        } => Some(arg),
        _ => None,
    }
}

impl Rule for NoNegationInEqualityCheck {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::Binary { left, op, .. } = *cx.node(node) else {
            return;
        };
        let negated = match op {
            BinaryOp::StrictEq => "!==",
            BinaryOp::StrictNotEq => "===",
            BinaryOp::Eq => "!=",
            BinaryOp::NotEq => "==",
            _ => return,
        };
        let Some(arg) = is_not(ast, left) else {
            return;
        };
        if is_not(ast, arg).is_some() {
            return;
        }
        let not = skip_parens(ast, left);
        let bang = cx.span(not).start;
        let after_bang = skip_trivia_forward(src, bang as usize + 1) as u32;
        let keyword = keyword_argument_parent(cx, not);
        let next_same_line = !has_newline(src, bang, after_bang);
        let msg = "Negated expression is not allowed in equality check.";
        let id = "no-negation-in-equality-check/error";
        if keyword.is_some_and(|k| matches!(cx.node(k), Node::Yield { .. })) && !next_same_line {
            cx.report(Span::new(bang, bang + 1), msg).id(id);
            return;
        }
        let parens =
            !matches!(ast.node(arg), Node::Paren { .. }) && start_needs_parens(cx, arg, not);
        let mut edits = space_around_keyword(cx, node);
        if let Some(k) = keyword {
            let kw_end = cx.span(k).start
                + if matches!(cx.node(k), Node::Return { .. }) {
                    6
                } else {
                    5
                };
            if has_newline(src, kw_end, after_bang) {
                let s = cx.span(k);
                edits.push((Span::at(kw_end), " (".to_owned()));
                let close = if s.text(src).ends_with(';') {
                    s.end - 1
                } else {
                    s.end
                };
                edits.push((Span::at(close), ")".to_owned()));
            }
        }
        if parens {
            let a = cx.span(arg);
            edits.push((Span::at(a.start), "(".to_owned()));
            edits.push((Span::at(a.end), ")".to_owned()));
        }
        let first = if parens {
            Some('(')
        } else {
            src[after_bang as usize..].chars().next()
        };
        if first.is_some_and(|c| "[(/`+-*,.<".contains(c)) && starts_continued_statement(cx, node) {
            edits.push((Span::at(bang), ";".to_owned()));
        }
        let op_at = skip_trivia_forward(src, cx.span(left).end as usize) as u32;
        let op_len = op.as_str().len() as u32;
        edits.push((Span::new(op_at, op_at + op_len), negated.to_owned()));
        let fix = with_edits(Fix::delete(Span::new(bang, bang + 1)), edits);
        cx.report(Span::new(bang, bang + 1), msg).id(id).suggest(
            "no-negation-in-equality-check/suggestion",
            format!("Switch to '{negated}' check."),
            fix,
        );
    }
}
