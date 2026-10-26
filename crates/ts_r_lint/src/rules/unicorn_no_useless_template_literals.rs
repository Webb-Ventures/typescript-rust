//! `unicorn/no-useless-template-literals`: interpolations of static values (string, number,
//! boolean, `null` and bigint literals, `+`/`-` of a number, `-` of a bigint, and templates
//! without expressions) are reported on their (untagged) template. When every part is
//! static and the text around them is empty the fix writes a string literal; otherwise the
//! static parts are folded into the template's raw text, which is given up when a string's
//! escape is not valid in a template (`\1`, `\08`) or a digit would follow a `\0` escape. A
//! comment inside a static `${…}` blocks the fix, and a directive-prologue statement only
//! accepts a template. A template that is only `${expr}` with a dynamic `expr` gets a
//! `String(expr)` suggestion instead, except under `as const`/`<const>` and in TypeScript
//! files (whose contextual type needs the checker).

use super::prelude::*;
use super::unicorn_prefer_bigint_literals::string_to_bigint;
use super::unicorn_text::{call_argument_text, has_comment_in, is_parenthesized};
use super::unicorn_util::{in_unparenthesized_new_callee, space_around_keyword, with_edits};
use ts_r_ast::KeywordTypeKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-template-literals",
        "Disallow useless template literal expressions.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<NoUselessTemplateLiterals>,
};

const MESSAGE: &str = "Do not use unnecessary template literal expressions.";

#[derive(Debug, Default)]
struct NoUselessTemplateLiterals;

/// A static interpolation: its value, the text it takes inside a template, whether that text
/// came from a string with an escape templates reject, and whether the fix may drop it.
struct Problem {
    cooked: String,
    raw: String,
    incompatible: bool,
    fixable: bool,
}

/// `text` quoted with `'`, escaping what quote-js-string escapes.
fn quote_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\u{b}' => out.push_str("\\v"),
            '\0' => out.push_str("\\u{0}"),
            c if (c as u32) <= 0x1f || matches!(c as u32, 0x7f | 0x2028 | 0x2029) => {
                out.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// A string literal's inner raw text made safe for a template: unescaped `` ` `` and `${`
/// get a backslash.
fn escape_for_template(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                out.push(c);
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '`' => out.push_str("\\`"),
            '$' if chars.peek() == Some(&'{') => out.push_str("\\$"),
            c => out.push(c),
        }
    }
    out
}

/// Legacy octal (`\1`, `\012`) and `\8`/`\9` escapes, fine in strings, errors in templates.
fn has_incompatible_escape(raw: &str) -> bool {
    let b = raw.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            match b.get(i + 1) {
                Some(b'1'..=b'9') => return true,
                Some(b'0') if b.get(i + 2).is_some_and(u8::is_ascii_digit) => return true,
                _ => {}
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    false
}

/// The last character of `s` follows an odd run of backslashes.
fn last_escaped(s: &str) -> bool {
    let b = s.as_bytes();
    let n = b.len().saturating_sub(1);
    b[..n].iter().rev().take_while(|&&c| c == b'\\').count() % 2 == 1
}

/// Appends `add` to template raw text, escaping a `$` that would start `${`; `None` when a
/// digit would extend a `\0` escape.
fn append_raw(mut raw: String, add: &str) -> Option<String> {
    if add.starts_with(|c: char| c.is_ascii_digit()) && raw.ends_with('0') && last_escaped(&raw) {
        return None;
    }
    if add.starts_with('{') && raw.ends_with('$') && !last_escaped(&raw) {
        raw.pop();
        raw.push_str("\\$");
    }
    raw.push_str(add);
    Some(raw)
}

fn plain(cooked: String) -> Option<(String, String, bool)> {
    Some((cooked.clone(), cooked, false))
}

/// (cooked, raw, incompatible escape) of a static interpolated expression.
fn static_value(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(String, String, bool)> {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::Unary { op, arg } if matches!(op, UnaryOp::Plus | UnaryOp::Minus) => {
            let arg = skip_parens(ast, arg);
            match *ast.node(arg) {
                Node::NumberLit { value } => {
                    let v = value.get();
                    plain(number_to_string(if op == UnaryOp::Minus { -v } else { v }))
                }
                Node::BigIntLit { .. } if op == UnaryOp::Minus => {
                    let v = string_to_bigint(&bigint_text(cx, arg))?;
                    let digits = v.mag.to_string_radix(10);
                    plain(if digits == "0" {
                        digits
                    } else {
                        format!("-{digits}")
                    })
                }
                _ => None,
            }
        }
        Node::StringLit { value } => {
            let text = cx.text(id);
            let raw = text.get(1..text.len().saturating_sub(1)).unwrap_or("");
            Some((
                ast.atom(value).to_owned(),
                escape_for_template(raw),
                has_incompatible_escape(raw),
            ))
        }
        Node::NumberLit { value } => plain(number_to_string(value.get())),
        Node::BoolLit { value } => plain(value.to_string()),
        Node::NullLit {} => plain("null".to_owned()),
        Node::BigIntLit { .. } => plain(bigint_text(cx, id)),
        Node::TemplateLit { parts } => {
            let parts = ast.list(parts);
            let [elem] = parts else {
                return None;
            };
            let Node::TemplateElement { cooked, .. } = *ast.node(*elem) else {
                return None;
            };
            let text = cx.text(id);
            let raw = text.get(1..text.len().saturating_sub(1)).unwrap_or("");
            Some((ast.atom(cooked).to_owned(), raw.to_owned(), false))
        }
        _ => None,
    }
}

/// ESTree's `bigint`: the literal's text without `n` or separators.
fn bigint_text(cx: &Ctx<'_, '_>, id: NodeId) -> String {
    let t = cx.text(id);
    t.strip_suffix('n').unwrap_or(t).replace('_', "")
}

/// `node` is an expression statement of a program's or function body's directive prologue.
fn in_directive_prologue(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some(stmt) = parent_skip_parens(ast, cx.file.parents, node) else {
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
        Node::Block { stmts } if cx.parent(owner).is_some_and(|f| is_function(ast.kind(f))) => {
            stmts
        }
        _ => return false,
    };
    ast.list(stmts)
        .iter()
        .take_while(|&&s| s != stmt)
        .all(|&s| matches!(ast.node(s), Node::Directive { .. }))
}

fn is_const_assertion(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some(p) = parent_skip_parens(ast, cx.file.parents, node) else {
        return false;
    };
    let ty = match *ast.node(p) {
        Node::As { expr, ty } | Node::TypeAssertion { ty, expr }
            if skip_parens(ast, expr) == node =>
        {
            ty
        }
        _ => return false,
    };
    matches!(
        ast.node(ty),
        Node::KeywordType {
            kind: KeywordTypeKind::Const
        }
    )
}

fn is_typescript_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".ts", ".mts", ".cts", ".tsx"]
        .iter()
        .any(|e| lower.ends_with(e))
}

impl Rule for NoUselessTemplateLiterals {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::TemplateLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::TemplateLit { parts } = *cx.node(node) else {
            return;
        };
        if cx.parent(node).is_some_and(
            |p| matches!(*ast.node(p), Node::TaggedTemplate { quasi, .. } if quasi == node),
        ) {
            return;
        }
        let parts = ast.list(parts);
        let quasis: Vec<NodeId> = parts.iter().step_by(2).copied().collect();
        let exprs: Vec<NodeId> = parts.iter().skip(1).step_by(2).copied().collect();
        if exprs.is_empty() || quasis.len() != exprs.len() + 1 {
            return;
        }
        let src = cx.src();
        let raw_of = |q: NodeId| ast.span(q).text(src);
        // `${…}` of expression `i`, delimiters included.
        let interp = |i: usize| Span::new(ast.span(quasis[i]).end, ast.span(quasis[i + 1]).start);
        let problem = |i: usize| {
            static_value(cx, exprs[i]).map(|(cooked, raw, incompatible)| Problem {
                cooked,
                raw,
                incompatible,
                fixable: !has_comment_in(cx, interp(i)),
            })
        };
        let span = cx.span(node);

        if exprs.len() == 1 && raw_of(quasis[0]).is_empty() && raw_of(quasis[1]).is_empty() {
            if let Some(p) = problem(0) {
                let fix = (p.fixable && !in_directive_prologue(cx, node))
                    .then(|| Fix::replace(span, quote_string(&p.cooked)));
                cx.report(span, MESSAGE)
                    .id("no-useless-template-literals")
                    .fix_opt(fix);
                return;
            }
            if is_const_assertion(cx, node) || is_typescript_file(cx.file.path) {
                return;
            }
            let has_comments = has_comment_in(cx, interp(0));
            let mut replacement = format!("String({})", call_argument_text(cx, exprs[0]));
            if !is_parenthesized(cx, node) && in_unparenthesized_new_callee(cx, node) {
                replacement = format!("({replacement})");
            }
            let edits = space_around_keyword(cx, node);
            let r = cx.report(span, MESSAGE).id("no-useless-template-literals");
            if !has_comments {
                r.suggest(
                    "no-useless-template-literals/suggestion",
                    format!("Use `{replacement}` instead."),
                    with_edits(Fix::replace(span, replacement), edits),
                );
            }
            return;
        }

        let problems: Vec<Option<Problem>> = (0..exprs.len()).map(problem).collect();
        if problems.iter().all(Option::is_none) {
            return;
        }
        let fix = (|| {
            if problems.iter().flatten().any(|p| !p.fixable) {
                return None;
            }
            let all_static = problems.iter().all(Option::is_some);
            let replacement = if all_static && quasis.iter().all(|&q| raw_of(q).is_empty()) {
                let mut cooked = String::new();
                for (i, &q) in quasis.iter().enumerate() {
                    if let Node::TemplateElement { cooked: c, .. } = *ast.node(q) {
                        cooked.push_str(ast.atom(c));
                    }
                    if let Some(Some(p)) = problems.get(i) {
                        cooked.push_str(&p.cooked);
                    }
                }
                if in_directive_prologue(cx, node) {
                    return None;
                }
                quote_string(&cooked)
            } else {
                if problems.iter().flatten().any(|p| p.incompatible) {
                    return None;
                }
                let mut raw = String::new();
                for (i, &q) in quasis.iter().enumerate() {
                    raw = append_raw(raw, raw_of(q))?;
                    if i == exprs.len() {
                        break;
                    }
                    raw = match &problems[i] {
                        Some(p) => append_raw(raw, &p.raw)?,
                        None => raw + interp(i).text(src),
                    };
                }
                format!("`{raw}`")
            };
            Some(Fix::replace(span, replacement))
        })();
        cx.report(span, MESSAGE)
            .id("no-useless-template-literals")
            .fix_opt(fix);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_helpers() {
        assert_eq!(escape_for_template("a`b${c}\\`"), "a\\`b\\${c}\\`");
        assert!(has_incompatible_escape("\\1"));
        assert!(has_incompatible_escape("\\08"));
        assert!(!has_incompatible_escape("\\\\1"));
        assert!(!has_incompatible_escape("\\0"));
        assert_eq!(append_raw("\\0".to_owned(), "1"), None);
        assert_eq!(append_raw("a$".to_owned(), "{b").as_deref(), Some("a\\${b"));
        assert_eq!(quote_string("it's\n"), "'it\\'s\\n'");
    }
}
