//! `unicorn/no-lonely-if`: an `if` without `else` that is the only statement of another `if`
//! without `else` (directly or as the sole statement of its block) is reported, and the two
//! are merged into `if (a && b)`.
//!
//! The fix rebuilds the outer statement as one replacement, keeping the source text of both
//! conditions and their comments:
//! - a test is wrapped in parentheses when it binds looser than `&&` (`||`, `??`, `?:`,
//!   arrows, assignments, `yield`, sequences) and isn't parenthesized already;
//! - comments between the outer `)` and `{` and between `{` and the inner `if` move before
//!   the merged statement, except a `//` comment right after the outer `)`, which stays after
//!   the merged condition (so `eslint-disable-line` keeps its line);
//! - the inner block keeps its body, and whatever followed the inner `if` inside the outer
//!   block is appended to it;
//! - when the inner body is a statement without `;` and the code after the outer `if` starts
//!   with a token that would continue it, the range grows to that token and a `;` is added.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-lonely-if",
        "Disallow `if` statements as the only statement in `if` blocks without `else`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoLonelyIf>,
};

#[derive(Debug, Default)]
struct NoLonelyIf;

impl Rule for NoLonelyIf {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::If]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if !matches!(*ast.node(node), Node::If { alt: None, .. }) {
            return;
        }
        let Some(parent) = cx.parent(node) else {
            return;
        };
        let outer = match *ast.node(parent) {
            Node::Block { stmts } if ast.list(stmts) == [node] => {
                match cx.parent(parent).map(|p| (p, ast.node(p))) {
                    Some((
                        p,
                        &Node::If {
                            cons, alt: None, ..
                        },
                    )) if cons == parent => p,
                    _ => return,
                }
            }
            Node::If {
                cons, alt: None, ..
            } if cons == node => parent,
            _ => return,
        };
        let fix = merge(cx, outer, node);
        cx.report_node(
            node,
            "Unexpected `if` as the only statement in a `if` block without `else`.",
        )
        .id("no-lonely-if")
        .fix_opt(fix);
    }
}

/// The token positions of an `if` statement.
struct IfTokens {
    span: Span,
    /// End of `if`.
    if_end: u32,
    /// Start of `(`.
    open_paren: u32,
    /// Start of `)`.
    close_paren: u32,
    test: NodeId,
    cons: NodeId,
    /// `{` and `}` starts when the body is a block.
    braces: Option<(u32, u32)>,
}

fn if_tokens(cx: &Ctx<'_, '_>, id: NodeId) -> Option<IfTokens> {
    let ast = cx.ast();
    let src = cx.src();
    let Node::If { test, cons, .. } = *ast.node(id) else {
        return None;
    };
    let span = cx.span(id);
    let if_end = span.start + 2;
    let open_paren = find_char(src, if_end, b'(')?;
    let close_paren =
        (skip_trivia_backward(ast, src, cx.span(cons).start as usize) as u32).checked_sub(1)?;
    if src.as_bytes().get(close_paren as usize) != Some(&b')') {
        return None;
    }
    let braces = match ast.node(cons) {
        Node::Block { .. } => {
            let b = cx.span(cons);
            Some((b.start, b.end - 1))
        }
        _ => None,
    };
    Some(IfTokens {
        span,
        if_end,
        open_paren,
        close_paren,
        test,
        cons,
        braces,
    })
}

fn merge(cx: &Ctx<'_, '_>, outer_id: NodeId, inner_id: NodeId) -> Option<Fix> {
    let src = cx.src();
    let text = |a: u32, b: u32| src.get(a as usize..b as usize).unwrap_or("");
    let outer = if_tokens(cx, outer_id)?;
    let inner = if_tokens(cx, inner_id)?;

    let outer_prefix = text(outer.if_end, outer.open_paren);
    let outer_test = test_text(cx, &outer, text(outer.open_paren + 1, outer.close_paren));
    let inner_test = test_text(cx, &inner, text(inner.open_paren + 1, inner.close_paren));
    let inner_condition = join(gap(text(inner.if_end, inner.open_paren)), &inner_test);

    let (condition_gap, leading, trailing) = match outer.braces {
        Some((open, close)) => (
            gap(text(outer.close_paren + 1, open)),
            gap(text(open + 1, inner.span.start)),
            gap(text(inner.span.end, close)),
        ),
        None => ("", gap(text(outer.close_paren + 1, inner.span.start)), ""),
    };
    let (leading, suffix) = if condition_gap.trim_start().starts_with("//") {
        (gap(leading).trim_start().to_owned(), condition_gap)
    } else {
        (
            gap(&join(condition_gap, leading)).trim_start().to_owned(),
            "",
        )
    };

    let mut range = outer.span;
    let body = match inner.braces {
        Some((open, close)) => format!(
            "{suffix}{}{{{}{trailing}}}",
            text(inner.close_paren + 1, open),
            text(open + 1, close)
        ),
        None => {
            let cons = cx.span(inner.cons);
            if needs_semicolon_after(cx, inner.cons) {
                let next = skip_trivia_forward(src, outer.span.end as usize);
                if src[next.min(src.len())..]
                    .starts_with(['[', '(', '/', '`', '+', '-', '*', ',', '.', '<'])
                {
                    range = Span::new(range.start, next as u32);
                }
            }
            let asi = if range.end == outer.span.end {
                String::new()
            } else {
                let g = text(outer.span.end, range.end);
                if g.trim().is_empty() && !g.contains('\n') {
                    ";".to_owned()
                } else {
                    format!(";{g}")
                }
            };
            format!(
                "{suffix}{}{}{trailing}{asi}",
                text(inner.close_paren + 1, cons.start),
                cons.text(src)
            )
        }
    };
    Some(Fix::replace(
        range,
        format!("{leading}if{outer_prefix}({outer_test} && {inner_condition}){body}"),
    ))
}

/// The test's text, parenthesized when it binds looser than `&&`.
fn test_text(cx: &Ctx<'_, '_>, tokens: &IfTokens, text: &str) -> String {
    let loose = match *cx.node(tokens.test) {
        Node::Binary { op, .. } => matches!(op, BinaryOp::LogicalOr | BinaryOp::Coalesce),
        Node::Conditional { .. }
        | Node::Arrow { .. }
        | Node::Assign { .. }
        | Node::Yield { .. }
        | Node::Seq { .. } => true,
        _ => false,
    };
    if loose {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

/// Text that is only whitespace counts as nothing.
fn gap(text: &str) -> &str {
    if text.trim().is_empty() { "" } else { text }
}

/// Joins two pieces, collapsing whitespace where both sides have some.
fn join(left: &str, right: &str) -> String {
    if left.ends_with(char::is_whitespace) && right.starts_with(char::is_whitespace) {
        format!("{left}{}", right.trim_start())
    } else {
        format!("{left}{right}")
    }
}

/// The statement ends, without `;`, in an expression a following `(`, `[`, ... would
/// continue.
fn needs_semicolon_after(cx: &Ctx<'_, '_>, mut stmt: NodeId) -> bool {
    let ast = cx.ast();
    for _ in 0..64 {
        stmt = match *ast.node(stmt) {
            Node::If { alt: Some(b), .. }
            | Node::If { cons: b, .. }
            | Node::While { body: b, .. }
            | Node::For { body: b, .. }
            | Node::ForIn { body: b, .. }
            | Node::ForOf { body: b, .. }
            | Node::Labeled { body: b, .. }
            | Node::With { body: b, .. } => b,
            _ => return stmt_continuable(ast, cx.src(), stmt),
        };
    }
    false
}
