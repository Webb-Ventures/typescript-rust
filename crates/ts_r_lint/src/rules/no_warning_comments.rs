//! `no-warning-comments`: comments with `todo`, `fixme`, `xxx` (or the configured terms),
//! at their start (default) or anywhere.

use super::prelude::*;
use ts_r_ast::CommentKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-warning-comments",
        "Disallow specified warning terms in comments",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoWarningComments>,
};

#[derive(Debug, Default)]
struct NoWarningComments {
    /// `(term, pattern)`.
    terms: Vec<(String, String)>,
}

/// The `escape-string-regexp` package's escaping.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '|' | '\\' | '{' | '}' | '(' | ')' | '[' | ']' | '^' | '$' | '+' | '*' | '?' | '.' => {
                out.push('\\');
                out.push(c);
            }
            '-' => out.push_str("\\x2d"),
            _ => out.push(c),
        }
    }
    out
}

fn is_word(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl FromOptions for NoWarningComments {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let terms: Vec<String> = match o.get("terms").and_then(Value::as_array) {
            Some(a) => a
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            None => vec!["todo".into(), "fixme".into(), "xxx".into()],
        };
        let start = str_field(o, "location").unwrap_or("start") == "start";
        let decoration: String = o
            .get("decoration")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let decoration = escape(&decoration);
        let terms = terms
            .into_iter()
            .map(|t| {
                let prefix = if start {
                    format!("^[\\s{decoration}]*")
                } else if is_word(t.chars().next()) {
                    "\\b".to_owned()
                } else {
                    String::new()
                };
                let suffix = if is_word(t.chars().next_back()) {
                    "\\b"
                } else {
                    ""
                };
                let p = format!("{prefix}{}{suffix}", escape(&t));
                (t, p)
            })
            .collect();
        Ok(NoWarningComments { terms })
    }
}

/// ESLint's `isDirectiveComment` on the comment's value.
pub(crate) fn is_directive(kind: CommentKind, value: &str) -> bool {
    let v = value.trim();
    match kind {
        CommentKind::Line => v.starts_with("eslint-"),
        CommentKind::Block => {
            v.starts_with("eslint-")
                || v.starts_with("eslint ")
                || v.starts_with("global ")
                || v.starts_with("globals ")
                || v.starts_with("exported ")
        }
        CommentKind::Hashbang => false,
    }
}

/// A comment's text without its delimiters.
pub(crate) fn comment_value(kind: CommentKind, text: &str) -> &str {
    match kind {
        CommentKind::Block => text.get(2..text.len().saturating_sub(2)).unwrap_or(""),
        _ => text.get(2..).unwrap_or(""),
    }
}

impl Rule for NoWarningComments {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        for c in &ast.comments {
            if c.kind == CommentKind::Hashbang {
                continue;
            }
            let value = comment_value(c.kind, c.span.text(src));
            if is_directive(c.kind, value)
                && crate::regex_exec::test("\\bno-warning-comments\\b", "u", value)
            {
                continue;
            }
            for (term, pattern) in &self.terms {
                if !crate::regex_exec::test(pattern, "iu", value) {
                    continue;
                }
                let mut shown = String::new();
                let mut truncated = false;
                for w in value.split_whitespace() {
                    let len = shown.encode_utf16().count()
                        + w.encode_utf16().count()
                        + usize::from(!shown.is_empty());
                    if len <= 40 {
                        if !shown.is_empty() {
                            shown.push(' ');
                        }
                        shown.push_str(w);
                    } else {
                        truncated = true;
                        break;
                    }
                }
                let dots = if truncated { "..." } else { "" };
                cx.report(
                    c.span,
                    format!("Unexpected '{term}' comment: '{shown}{dots}'."),
                )
                .id("unexpectedComment");
            }
        }
    }
}
