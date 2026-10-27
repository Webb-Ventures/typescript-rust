//! `prefer-template`: template literals instead of string concatenation.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "prefer-template",
        "Require template literals instead of string concatenation",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferTemplate>,
};

#[derive(Debug, Default)]
struct PreferTemplate;

const MAX_DEPTH: u32 = 500;

/// `a + b` (looking through parentheses): its operands.
fn concat(ast: &Ast, id: NodeId) -> Option<(NodeId, NodeId)> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Binary {
            op: BinaryOp::Add,
            left,
            right,
        } => Some((skip_parens(ast, left), skip_parens(ast, right))),
        _ => None,
    }
}

fn is_string(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(skip_parens(ast, id)),
        Node::StringLit { .. } | Node::TemplateLit { .. }
    )
}

/// Some leaf of the concatenation is (`want`) / is not (`!want`) a string or template.
fn has_leaf(ast: &Ast, id: NodeId, want: bool, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return true;
    }
    match concat(ast, id) {
        Some((l, r)) => has_leaf(ast, r, want, depth + 1) || has_leaf(ast, l, want, depth + 1),
        None => is_string(ast, id) == want,
    }
}

/// A string literal leaf with a legacy octal or `\8` / `\9` escape.
fn has_octal_escape(ast: &Ast, src: &str, id: NodeId, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return true;
    }
    if let Some((l, r)) = concat(ast, id) {
        return has_octal_escape(ast, src, l, depth + 1)
            || has_octal_escape(ast, src, r, depth + 1);
    }
    let id = skip_parens(ast, id);
    if !matches!(ast.node(id), Node::StringLit { .. }) {
        return false;
    }
    let b = ast.span(id).text(src).as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            match (b.get(i + 1), b.get(i + 2)) {
                (Some(b'1'..=b'9'), _) | (Some(b'0'), Some(b'0'..=b'9')) => return true,
                _ => i += 2,
            }
        } else {
            i += 1;
        }
    }
    false
}

/// `startsWithTemplateCurly` / `endsWithTemplateCurly` (`first`): the operand's text would
/// begin / end with a `${..}` substitution. For a binary operand both look at its left side
/// and right side's start respectively, as ESLint does.
fn template_curly(ast: &Ast, id: NodeId, first: bool, depth: u32) -> bool {
    let id = skip_parens(ast, id);
    if depth > MAX_DEPTH {
        return false;
    }
    match *ast.node(id) {
        Node::Binary { left, right, op }
            if !matches!(
                op,
                BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce
            ) =>
        {
            if first {
                template_curly(ast, left, true, depth + 1)
            } else {
                template_curly(ast, right, true, depth + 1)
            }
        }
        Node::TemplateLit { parts } => {
            let parts = ast.list(parts);
            let quasis: Vec<NodeId> = parts
                .iter()
                .copied()
                .filter(|&p| matches!(ast.node(p), Node::TemplateElement { .. }))
                .collect();
            let has_expr = quasis.len() < parts.len();
            let q = if first { quasis.first() } else { quasis.last() };
            has_expr && q.is_some_and(|&q| ast.span(q).is_empty())
        }
        Node::StringLit { .. } => false,
        _ => true,
    }
}

/// Whitespace and comments in `src[a..b]`, without the parentheses between them.
fn trivia_between(ast: &Ast, src: &str, a: u32, b: u32) -> String {
    let mut out = String::new();
    let mut pos = a as usize;
    let end = b as usize;
    let comments = &ast.comments;
    let mut ci = comments.partition_point(|c| (c.span.start as usize) < pos);
    while pos < end {
        if let Some(c) = comments.get(ci)
            && c.span.start as usize == pos
        {
            let ce = (c.span.end as usize).min(end);
            out.push_str(&src[pos..ce]);
            pos = ce;
            ci += 1;
            continue;
        }
        let Some(ch) = src[pos..].chars().next() else {
            break;
        };
        if ch != '(' && ch != ')' {
            out.push(ch);
        }
        pos += ch.len_utf8();
    }
    out
}

/// A string literal's body as template text.
fn string_to_template(raw: &str) -> String {
    let quote = raw.chars().next().unwrap_or('"');
    let body = raw.get(1..raw.len().saturating_sub(1)).unwrap_or("");
    // `${` and `` ` `` get a backslash unless an odd run of backslashes already escapes them.
    let b = body.as_bytes();
    let mut out = String::with_capacity(body.len() + 2);
    let mut i = 0;
    let mut start = 0;
    while i < b.len() {
        let run_start = i;
        while i < b.len() && b[i] == b'\\' {
            i += 1;
        }
        let run = i - run_start;
        let tok = if body[i..].starts_with("${") {
            2
        } else if body[i..].starts_with('`') {
            1
        } else {
            0
        };
        if tok > 0 {
            out.push_str(&body[start..i]);
            if run % 2 == 0 {
                out.push('\\');
            }
            out.push_str(&body[i..i + tok]);
            i += tok;
            start = i;
        } else if run == 0 {
            i += body[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out.push_str(&body[start..]);
    let escaped_quote = format!("\\{quote}");
    format!("`{}`", out.replace(&escaped_quote, &quote.to_string()))
}

fn template_of(
    cx: &Ctx<'_, '_>,
    id: NodeId,
    before: Option<&str>,
    after: Option<&str>,
    depth: u32,
) -> String {
    let ast = cx.ast();
    let src = cx.src();
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::StringLit { .. } => return string_to_template(cx.text(id)),
        Node::TemplateLit { .. } => return cx.text(id).to_string(),
        _ => {}
    }
    if depth < MAX_DEPTH
        && let Some((l, r)) = concat(ast, id)
        && has_leaf(ast, id, true, 0)
    {
        let lend = cx.span(l).end;
        let rstart = cx.span(r).start;
        // The `+`: the first character after `l` that isn't trivia or `)`.
        let mut plus = lend as usize;
        loop {
            plus = skip_trivia_forward(src, plus);
            if src[plus..].starts_with(')') {
                plus += 1;
            } else {
                break;
            }
        }
        let before_plus = trivia_between(ast, src, lend, plus as u32);
        let after_plus = trivia_between(ast, src, plus as u32 + 1, rstart);
        let mut left;
        let right;
        if template_curly(ast, l, false, 0) {
            let around = format!("{before_plus}{after_plus}");
            left = template_of(cx, l, before, Some(&around), depth + 1);
            right = template_of(cx, r, None, after, depth + 1);
        } else if template_curly(ast, r, true, 0) {
            let around = format!("{before_plus}{after_plus}");
            left = template_of(cx, l, before, None, depth + 1);
            right = template_of(cx, r, Some(&around), after, depth + 1);
        } else {
            return format!(
                "{}{before_plus}+{after_plus}{}",
                template_of(cx, l, before, None, depth + 1),
                template_of(cx, r, after, None, depth + 1)
            );
        }
        left.pop();
        return format!("{left}{}", right.get(1..).unwrap_or(""));
    }
    format!(
        "`${{{}{}{}}}`",
        before.unwrap_or(""),
        cx.text(id),
        after.unwrap_or("")
    )
}

impl Rule for PreferTemplate {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if concat(ast, node).is_none() {
            return;
        }
        if let Some(p) = parent_skip_parens(ast, cx.file.parents, node)
            && concat(ast, p).is_some()
        {
            return;
        }
        if !has_leaf(ast, node, true, 0) || !has_leaf(ast, node, false, 0) {
            return;
        }
        let src = cx.src();
        let fix = (!has_octal_escape(ast, src, node, 0)).then(|| {
            let prefix = if needs_leading_semicolon(ast, src, cx.file.parents, node) {
                ";"
            } else {
                ""
            };
            Fix::replace(
                cx.span(node),
                format!("{prefix}{}", template_of(cx, node, None, None, 0)),
            )
        });
        cx.report_node(node, "Unexpected string concatenation.")
            .id("unexpectedStringConcatenation")
            .fix_opt(fix);
    }
}
