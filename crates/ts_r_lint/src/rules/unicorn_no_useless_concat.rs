//! `unicorn/no-useless-concat`: `+` between two literals on the same line: string literals
//! or untagged templates, the left one being the left operand or, in `x + 'a' + 'b'`, the
//! right operand of the left `+` (a chain). Reported at the `+`, except when the folded
//! string would contain a `${...}` placeholder, when a template's raw text would end in `$`
//! right before one starting with `{`, or when the folded string would be a `use ...`
//! statement in a directive prologue.
//!
//! The fix writes one single-quoted string, or a template when either side has
//! substitutions (string literal raw text gets its unescaped `` ` `` and `${` escaped). No
//! fix when a comment is in the replaced range, in a chain whose left side is parenthesized
//! or whose right template has substitutions (that would reorder side effects), or when a
//! template would receive a legacy octal / `\8` / `\9` escape. A template fix is spaced
//! from a glued keyword and gets a `;` when it would continue the previous line.

use super::prelude::*;
use super::unicorn_text::paren_span;
use super::unicorn_text::{is_parenthesized, needs_semicolon, would_remove_comments};
use super::unicorn_util::{space_around_keyword, with_edits};
use ts_r_ast::Ast;
use ts_r_span::Span;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-concat",
        "Disallow useless concatenation of literals.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoUselessConcat>,
};

#[derive(Debug, Default)]
struct NoUselessConcat;

fn is_stringish(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::StringLit { .. } | Node::TemplateLit { .. }
    )
}

/// The value of a string literal or of a template without substitutions.
fn string_value(ast: &Ast, id: NodeId) -> Option<&str> {
    match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value)),
        Node::TemplateLit { parts } => match *ast.list(parts) {
            [only] => match *ast.node(only) {
                Node::TemplateElement { cooked, .. } => Some(ast.atom(cooked)),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// `${` followed by at least one non-`}` and a `}`.
fn has_placeholder(s: &str) -> bool {
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        let after = &rest[i + 2..];
        if let Some(close) = after.find('}')
            && close > 0
        {
            return true;
        }
        rest = after;
    }
    false
}

/// Escapes the unescaped `` ` `` and the `$` of an unescaped `${`.
fn escape_for_template(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut backslashes = 0usize;
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if backslashes.is_multiple_of(2) && (c == '`' || (c == '$' && chars.peek() == Some(&'{'))) {
            out.push('\\');
        }
        out.push(c);
        backslashes = if c == '\\' { backslashes + 1 } else { 0 };
    }
    out
}

/// An unescaped `\1`-`\9` or `\0` followed by a digit.
fn has_template_incompatible_escape(raw: &str) -> bool {
    let b = raw.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            match b.get(i + 1) {
                Some(b'1'..=b'9') => return true,
                Some(b'0') if b.get(i + 2).is_some_and(u8::is_ascii_digit) => return true,
                _ => i += 2,
            }
        } else {
            i += 1;
        }
    }
    false
}

/// A single-quoted JavaScript string literal for `s`.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\u{b}' => out.push_str("\\v"),
            '\0' if !chars.peek().is_some_and(char::is_ascii_digit) => out.push_str("\\0"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// The literal's content as template raw text.
fn template_raw(cx: &Ctx<'_, '_>, id: NodeId) -> String {
    let text = cx.text(id);
    let inner = text.get(1..text.len().saturating_sub(1)).unwrap_or("");
    match cx.node(id) {
        Node::TemplateLit { .. } => inner.to_owned(),
        _ => escape_for_template(inner),
    }
}

/// `node` is a statement of a directive prologue (the program's or a function body's).
fn in_directive_prologue(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some(stmt) = cx.parent(node) else {
        return false;
    };
    if !matches!(ast.node(stmt), Node::ExprStmt { .. }) {
        return false;
    }
    let Some(owner) = cx.parent(stmt) else {
        return false;
    };
    let stmts = match *ast.node(owner) {
        Node::Program { stmts } => stmts,
        Node::Block { stmts }
            if cx.parent(owner).is_some_and(|f| {
                matches!(
                    ast.node(f),
                    Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
                )
            }) =>
        {
            stmts
        }
        _ => return false,
    };
    ast.list(stmts)
        .iter()
        .take_while(|&&s| s != stmt)
        .all(|&s| match *ast.node(s) {
            Node::Directive { .. } => true,
            Node::ExprStmt { expr } => matches!(ast.node(expr), Node::StringLit { .. }),
            _ => false,
        })
}

impl Rule for NoUselessConcat {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary {
            left: node_left,
            op: BinaryOp::Add,
            right,
        } = *cx.node(node)
        else {
            return;
        };
        let right = skip_parens(ast, right);
        if !is_stringish(ast, right) {
            return;
        }
        let outer_left = skip_parens(ast, node_left);
        let left = if is_stringish(ast, outer_left) {
            outer_left
        } else if let Node::Binary {
            op: BinaryOp::Add,
            right: inner,
            ..
        } = *ast.node(outer_left)
            && is_stringish(ast, skip_parens(ast, inner))
        {
            skip_parens(ast, inner)
        } else {
            return;
        };
        let src = cx.src();
        let (left_span, right_span) = (cx.span(left), cx.span(right));
        if left_span.end > right_span.start
            || src[left_span.end as usize..right_span.start as usize]
                .contains(['\n', '\r', '\u{2028}', '\u{2029}'])
        {
            return;
        }
        let chain = left != outer_left;
        let (lv, rv) = (string_value(ast, left), string_value(ast, right));
        let replacement = match (lv, rv) {
            (Some(l), Some(r)) => {
                let joined = format!("{l}{r}");
                if has_placeholder(&joined) {
                    return;
                }
                let lower = joined.to_ascii_lowercase();
                if lower.starts_with("use")
                    && joined[3..].starts_with(char::is_whitespace)
                    && in_directive_prologue(cx, parenthesized_span_node(cx, node))
                {
                    return;
                }
                quote(&joined)
            }
            _ => {
                let (l, r) = (template_raw(cx, left), template_raw(cx, right));
                if l.ends_with('$') && r.starts_with('{') {
                    return;
                }
                format!("`{l}{r}`")
            }
        };

        // The `+`: the last token before the right operand and its parentheses.
        let mut at = paren_span(cx, right).start as usize;
        loop {
            at = skip_trivia_backward(ast, src, at);
            if at > 0 && src.as_bytes()[at - 1] == b'(' {
                at -= 1;
            } else {
                break;
            }
        }
        if at == 0 || src.as_bytes()[at - 1] != b'+' {
            return;
        }
        let operator = Span::new(at as u32 - 1, at as u32);

        let range = Span::new(paren_span(cx, left).start, paren_span(cx, right).end);
        let fix = (|| {
            if would_remove_comments(cx, range, &[])
                || (chain && is_parenthesized(cx, node_left))
                || (chain && rv.is_none())
            {
                return None;
            }
            let mut text = replacement;
            let mut edits = Vec::new();
            if text.starts_with('`') {
                if has_template_incompatible_escape(&text) {
                    return None;
                }
                edits = space_around_keyword(cx, node);
                if needs_semicolon(cx, range.start, &text) {
                    text.insert(0, ';');
                }
            }
            Some(with_edits(Fix::replace(range, text), edits))
        })();
        cx.report(
            operator,
            "Do not concatenate two literals, combine them into one.",
        )
        .id("no-useless-concat")
        .fix_opt(fix);
    }
}

/// The outermost parenthesized form of `node` (its parent is then the statement).
fn parenthesized_span_node(cx: &Ctx<'_, '_>, node: NodeId) -> NodeId {
    let mut id = node;
    while let Some(p) = cx.parent(id) {
        if !matches!(cx.node(p), Node::Paren { .. }) {
            break;
        }
        id = p;
    }
    id
}
