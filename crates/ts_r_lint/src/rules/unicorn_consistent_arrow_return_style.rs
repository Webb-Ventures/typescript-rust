//! `unicorn/consistent-arrow-return-style`: an arrow whose concise body spans several lines
//! gets a block with an explicit `return`; a block holding only `return <one-line
//! expression>;` becomes a concise body.
//!
//! Fixes are withheld when the arrow has comments. The explicit fix re-indents a body that
//! started on the arrow's line, which it can't do when the body has a multi-line string,
//! template or JSX text. The implicit fix is withheld when the token after the block could
//! continue the expression (`(`, `[`, `` ` ``, `/`, `+`, `-`, `*`, `.`, `<`, or the rest of
//! a template).

use super::prelude::*;
use super::unicorn_ast::linebreak;
use super::unicorn_no_useless_boolean_cast::concise_arrow_body_text;
use super::unicorn_util::parenthesized_span;
use super::unicorn_util_h2::{indent_unit, line_indent};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/consistent-arrow-return-style",
        "Enforce a consistent return style for multiline arrow function bodies.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<ConsistentArrowReturnStyle>,
};

#[derive(Debug, Default)]
struct ConsistentArrowReturnStyle;

fn is_multiline(text: &str) -> bool {
    text.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
}

fn span_text<'s>(cx: &Ctx<'s, '_>, span: Span) -> &'s str {
    cx.src()
        .get(span.start as usize..span.end as usize)
        .unwrap_or_default()
}

fn has_multiline_literal(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    if depth > 500 {
        return true;
    }
    let ast = cx.ast();
    let literal = matches!(
        ast.node(id),
        Node::StringLit { .. } | Node::TemplateElement { .. } | Node::JsxText {}
    );
    (literal && is_multiline(cx.text(id)))
        || ast
            .children(id)
            .into_iter()
            .any(|c| has_multiline_literal(cx, c, depth + 1))
}

/// The arrow (or an expression ending with it) is a template substitution, so the token
/// after the body is the rest of the template.
fn ends_template_substitution(cx: &Ctx<'_, '_>, node: NodeId, end: u32) -> bool {
    let ast = cx.ast();
    let mut cur = node;
    for _ in 0..1000 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        if matches!(ast.node(p), Node::TemplateLit { .. }) {
            return true;
        }
        if cx.span(p).end != end {
            return false;
        }
        cur = p;
    }
    false
}

fn unsafe_next_token(cx: &Ctx<'_, '_>, node: NodeId, body_end: u32) -> bool {
    let src = cx.src();
    let next = skip_trivia_forward(src, body_end as usize);
    match src[next..].chars().next() {
        Some('[' | '(' | '/' | '`' | '+' | '-' | '*' | '.' | '<') => true,
        Some('}') => ends_template_substitution(cx, node, body_end),
        _ => false,
    }
}

fn only_return(ast: &Ast, body: NodeId) -> Option<NodeId> {
    let Node::Block { stmts } = *ast.node(body) else {
        return None;
    };
    match *ast.list(stmts) {
        [only] => match *ast.node(only) {
            Node::Return { arg } => arg,
            _ => None,
        },
        _ => None,
    }
}

fn explicit_fix(cx: &Ctx<'_, '_>, body: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let src = cx.src();
    let body_span = parenthesized_span(cx, body);
    let arrow_end = skip_trivia_backward(ast, src, body_span.start as usize);
    let arrow_start = arrow_end.checked_sub(2)?;
    if src.get(arrow_start..arrow_end) != Some("=>") {
        return None;
    }
    let first = skip_trivia_forward(src, arrow_end);
    let same_line = !is_multiline(&src[arrow_start..first]);
    if same_line && has_multiline_literal(cx, body, 0) {
        return None;
    }
    let lb = linebreak(src);
    let unit = indent_unit(cx);
    let indent = line_indent(src, arrow_start);
    let mut text = if matches!(ast.node(skip_parens(ast, body)), Node::ObjectLit { .. }) {
        cx.text(skip_parens(ast, body)).to_owned()
    } else {
        src[body_span.start as usize..body_span.end as usize].to_owned()
    };
    if same_line {
        text = indent_later_lines(&text, &unit);
    }
    Some(Fix::replace(
        Span::new(arrow_end as u32, body_span.end),
        format!(" {{{lb}{indent}{unit}return {text};{lb}{indent}}}"),
    ))
}

/// Every line after the first gets `unit` in front; whitespace-only lines become empty.
fn indent_later_lines(text: &str, unit: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut first = true;
    while !rest.is_empty() {
        let (line, brk, next) = match rest.find(['\n', '\r', '\u{2028}', '\u{2029}']) {
            Some(i) => {
                let len = if rest[i..].starts_with("\r\n") {
                    2
                } else {
                    rest[i..].chars().next().map_or(1, char::len_utf8)
                };
                (&rest[..i], &rest[i..i + len], &rest[i + len..])
            }
            None => (rest, "", ""),
        };
        if first {
            out.push_str(line);
        } else if !line.trim().is_empty() {
            out.push_str(unit);
            out.push_str(line);
        }
        out.push_str(brk);
        first = false;
        rest = next;
    }
    out
}

impl Rule for ConsistentArrowReturnStyle {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Arrow]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Arrow { body, .. } = *ast.node(node) else {
            return;
        };
        let has_comments = !ast.comments_in(cx.span(node)).is_empty();
        if matches!(ast.node(body), Node::Block { .. }) {
            let Some(ret) = only_return(ast, body) else {
                return;
            };
            if is_multiline(span_text(cx, parenthesized_span(cx, ret))) {
                return;
            }
            let body_span = cx.span(body);
            let fix = (!has_comments && !unsafe_next_token(cx, node, body_span.end))
                .then(|| Fix::replace(body_span, concise_arrow_body_text(cx, ret)));
            cx.report_node(
                node,
                "Use an implicit return for a single-line return expression.",
            )
            .id("useImplicitReturn")
            .fix_opt(fix);
            return;
        }
        if !is_multiline(span_text(cx, parenthesized_span(cx, body))) {
            return;
        }
        let fix = if has_comments {
            None
        } else {
            explicit_fix(cx, body)
        };
        cx.report_node(
            node,
            "Use an explicit return for a multiline arrow function body.",
        )
        .id("useExplicitReturn")
        .fix_opt(fix);
    }
}
