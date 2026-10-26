//! `unicorn/no-negated-comparison`: `!(a === b)` becomes `a !== b` (also `!==`, `==`,
//! `!=`; never the relational operators, whose negation also rejects `NaN`). A `!` that is
//! itself negated is left alone. With `checkLogicalExpressions`, `!(a === b && c === d)`
//! made only of equalities, `&&` and `||` becomes `(a !== b || c !== d)` by De Morgan,
//! parenthesizing a sub-expression whose operator now binds looser than its parent's (or
//! that was parenthesized and changes operator); a comment inside withholds that fix.
//!
//! The fix removes the `!` and, for one comparison, its parentheses, unless the parent
//! needs them (`await`, `yield`, unary, binary, spread, `as`, `satisfies`, `!`, `<T>`) or
//! the comparison would start a statement with `{`, `function`, ... A `return`/`throw`
//! whose argument starts on the next line after the `!` gets its argument parenthesized; a
//! `yield` like that is reported without a fix.

use super::prelude::*;
use super::unicorn_text::{is_parenthesized, needs_parens_at_expression_start, needs_semicolon};
use super::unicorn_util::space_around_keyword;
use super::unicorn_util_h4::{keyword_argument_parent, parenthesize_keyword_argument};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-negated-comparison",
        "Disallow negated comparisons.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NoNegatedComparison>,
};

#[derive(Debug, Default)]
struct NoNegatedComparison {
    check_logical_expressions: bool,
}

impl FromOptions for NoNegatedComparison {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(Self {
            check_logical_expressions: bool_field(
                obj_opt(opts, 0),
                "checkLogicalExpressions",
                false,
            ),
        })
    }
}

fn negated_equality(op: BinaryOp) -> Option<&'static str> {
    Some(match op {
        BinaryOp::StrictEq => "!==",
        BinaryOp::StrictNotEq => "===",
        BinaryOp::Eq => "!=",
        BinaryOp::NotEq => "==",
        _ => return None,
    })
}

fn is_equality(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(id), Node::Binary { op, .. } if negated_equality(op).is_some())
}

fn only_equalities(ast: &Ast, id: NodeId, depth: u32) -> bool {
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::Binary {
            left,
            op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr,
            right,
        } => {
            depth < 200
                && only_equalities(ast, left, depth + 1)
                && only_equalities(ast, right, depth + 1)
        }
        _ => is_equality(ast, id),
    }
}

/// The span of a binary expression's operator token.
fn operator_span(cx: &Ctx<'_, '_>, left: NodeId, len: usize) -> Span {
    let left_end = super::unicorn_text::paren_span(cx, left).end;
    let start = skip_trivia_forward(cx.src(), left_end as usize) as u32;
    Span::new(start, start + len as u32)
}

fn operator_len(op: BinaryOp) -> usize {
    match op {
        BinaryOp::Eq | BinaryOp::NotEq => 2,
        _ => 3,
    }
}

/// The comparison's text with its operator negated.
fn negated_comparison_text(cx: &Ctx<'_, '_>, id: NodeId) -> String {
    let Node::Binary { left, op, .. } = *cx.node(id) else {
        return cx.text(id).to_owned();
    };
    let span = cx.span(id);
    let op_span = operator_span(cx, left, operator_len(op));
    let src = cx.src();
    format!(
        "{}{}{}",
        &src[span.start as usize..op_span.start as usize],
        negated_equality(op).unwrap_or_default(),
        &src[op_span.end as usize..span.end as usize]
    )
}

fn logical_text(cx: &Ctx<'_, '_>, id: NodeId, parent_op: Option<BinaryOp>, depth: u32) -> String {
    let ast = cx.ast();
    let inner = skip_parens(ast, id);
    let Node::Binary { left, op, right } = *ast.node(inner) else {
        return cx.text(inner).to_owned();
    };
    let op = match op {
        BinaryOp::LogicalAnd => BinaryOp::LogicalOr,
        BinaryOp::LogicalOr => BinaryOp::LogicalAnd,
        _ => return negated_comparison_text(cx, inner),
    };
    if depth > 200 {
        return cx.text(inner).to_owned();
    }
    let text = format!(
        "{} {} {}",
        logical_text(cx, left, Some(op), depth + 1),
        if op == BinaryOp::LogicalAnd {
            "&&"
        } else {
            "||"
        },
        logical_text(cx, right, Some(op), depth + 1)
    );
    let needs_parens = parent_op.is_some_and(|p| {
        (op != p && is_parenthesized(cx, inner))
            || (op == BinaryOp::LogicalOr && p == BinaryOp::LogicalAnd)
    });
    if needs_parens {
        format!("({text})")
    } else {
        text
    }
}

fn parent_needs_grouped_comparison(node: &Node) -> bool {
    // A logical expression is no `BinaryExpression` in ESTree.
    matches!(node, Node::Binary { op, .. } if !matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce))
        || matches!(
            node,
            Node::Await { .. }
                | Node::Spread { .. }
                | Node::As { .. }
                | Node::NonNull { .. }
                | Node::Satisfies { .. }
                | Node::TypeAssertion { .. }
                | Node::Unary { .. }
                | Node::Yield { .. }
        )
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

impl Rule for NoNegatedComparison {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Unary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Unary {
            op: UnaryOp::Not,
            arg,
        } = *cx.node(node)
        else {
            return;
        };
        let parent = parent_skip_parens(ast, cx.file.parents, node);
        if parent.is_some_and(|p| {
            matches!(*cx.node(p), Node::Unary { op: UnaryOp::Not, arg } if skip_parens(ast, arg) == node)
        }) {
            return;
        }
        let inner = skip_parens(ast, arg);
        let logical = !is_equality(ast, inner);
        if logical
            && (!self.check_logical_expressions
                || !matches!(
                    ast.node(inner),
                    Node::Binary {
                        op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr,
                        ..
                    }
                )
                || !only_equalities(ast, inner, 0))
        {
            return;
        }
        let src = cx.src();
        let span = cx.span(node);
        let bang = Span::new(span.start, span.start + 1);
        let next = skip_trivia_forward(src, bang.end as usize);
        let same_line = !src[bang.end as usize..next].contains(is_line_break);
        let keyword_parent = keyword_argument_parent(cx, node);
        let can_remove =
            same_line || !keyword_parent.is_some_and(|p| matches!(cx.node(p), Node::Yield { .. }));
        let parenthesize = keyword_parent.filter(|&p| {
            !same_line && matches!(cx.node(p), Node::Return { .. } | Node::Throw { .. })
        });
        let (msg, id) = if logical {
            (
                "Prefer the opposite comparisons instead of negating the whole logical expression.",
                "no-negated-comparison/logical-error",
            )
        } else {
            (
                "Prefer the opposite comparison instead of negating the whole comparison.",
                "no-negated-comparison/error",
            )
        };
        if !can_remove || (logical && !ast.comments_in(cx.span(inner)).is_empty()) {
            cx.report(span, msg).id(id);
            return;
        }
        let mut edits: Vec<(Span, String)> = Vec::new();
        let first_text = if logical {
            edits.push((cx.span(inner), logical_text(cx, inner, None, 0)));
            src[next..]
                .chars()
                .next()
                .map(String::from)
                .unwrap_or_default()
        } else {
            // A block comment right before `(` keeps a space from the comparison.
            let after_bang = skip_ws(src, bang.end as usize);
            if src[after_bang..].starts_with("/*")
                && let Some(end) = src[after_bang..].find("*/")
                && src[after_bang + end + 2..].starts_with('(')
            {
                edits.push((Span::at((after_bang + end + 2) as u32), " ".to_owned()));
            }
            let keep_parens = parent.is_some_and(|p| parent_needs_grouped_comparison(cx.node(p)))
                && !is_parenthesized(cx, node)
                || needs_parens_at_expression_start(cx, inner, node);
            if !keep_parens {
                let mut p = arg;
                for _ in 0..10_000 {
                    if !matches!(cx.node(p), Node::Paren { .. }) {
                        break;
                    }
                    let s = cx.span(p);
                    edits.push((Span::new(s.start, s.start + 1), String::new()));
                    edits.push((Span::new(s.end - 1, s.end), String::new()));
                    let Node::Paren { expr } = *cx.node(p) else {
                        break;
                    };
                    p = expr;
                }
            }
            let Node::Binary { left, op, .. } = *cx.node(inner) else {
                return;
            };
            edits.push((
                operator_span(cx, left, operator_len(op)),
                negated_equality(op).unwrap_or_default().to_owned(),
            ));
            if keep_parens {
                "(".to_owned()
            } else {
                cx.text(inner)
                    .chars()
                    .next()
                    .map(String::from)
                    .unwrap_or_default()
            }
        };
        if let Some(stmt) = parenthesize {
            edits.extend(parenthesize_keyword_argument(cx, stmt));
        } else {
            if needs_semicolon(cx, span.start, &first_text) {
                edits.push((Span::at(span.start), ";".to_owned()));
            }
            edits.extend(space_around_keyword(cx, node));
        }
        let mut fix = Fix::delete(bang);
        for (s, t) in edits {
            fix = fix.and(s, t);
        }
        cx.report(span, msg).id(id).fix(fix);
    }
}

fn skip_ws(src: &str, mut pos: usize) -> usize {
    while let Some(c) = src[pos..].chars().next() {
        if !c.is_whitespace() {
            break;
        }
        pos += c.len_utf8();
    }
    pos
}
