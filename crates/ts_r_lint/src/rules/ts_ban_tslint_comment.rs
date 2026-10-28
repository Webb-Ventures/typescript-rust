//! `@typescript-eslint/ban-tslint-comment`: report `tslint:enable` / `tslint:disable`
//! directives (with `-line` / `-next-line` and an optional `:rules` list).
//!
//! - The value (comment text without its delimiters) matches when, after leading
//!   whitespace, it reads `tslint:enable` or `tslint:disable`, optionally `-line` or
//!   `-next-line`, then `:`, whitespace or the end.
//! - The message quotes the comment re-delimited around its trimmed value (`// x`, `/* x */`).
//! - The fix removes the comment plus one character on each side: the one before only when the
//!   comment does not start a line, the one after always (usually the line break).

use super::prelude::*;
use ts_r_ast::CommentKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/ban-tslint-comment",
        "Disallow `// tslint:<rule-flag>` comments",
        TS_STYLISTIC,
        Category::Style,
    )
    .fixable(),
    create: create_default::<BanTslintComment>,
};

#[derive(Debug, Default)]
struct BanTslintComment;

/// JS `\s`.
fn is_js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

fn is_directive(value: &str) -> bool {
    let Some(rest) = value
        .trim_start_matches(is_js_space)
        .strip_prefix("tslint:")
    else {
        return false;
    };
    let Some(rest) = rest
        .strip_prefix("enable")
        .or_else(|| rest.strip_prefix("disable"))
    else {
        return false;
    };
    let rest = rest
        .strip_prefix("-line")
        .or_else(|| rest.strip_prefix("-next-line"))
        .unwrap_or(rest);
    rest.chars()
        .next()
        .is_none_or(|c| c == ':' || is_js_space(c))
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

impl Rule for BanTslintComment {
    fn has_run_file(&self) -> bool {
        true
    }

    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        for c in &ast.comments {
            let text = c.span.text(src);
            let (value, line) = match c.kind {
                CommentKind::Line => (text.get(2..).unwrap_or(""), true),
                CommentKind::Block => (
                    text.get(2..text.len().saturating_sub(2)).unwrap_or(""),
                    false,
                ),
                CommentKind::Hashbang => continue,
            };
            if !is_directive(value) {
                continue;
            }
            let trimmed = value.trim_matches(is_js_space);
            let shown = if line {
                format!("// {trimmed}")
            } else {
                format!("/* {trimmed} */")
            };
            let mut start = c.span.start as usize;
            if let Some(prev) = src[..start].chars().next_back()
                && !is_line_break(prev)
            {
                start -= prev.len_utf8();
            }
            let mut end = c.span.end as usize;
            if let Some(next) = src[end..].chars().next() {
                end += next.len_utf8();
            }
            cx.report(c.span, format!("tslint comment detected: \"{shown}\""))
                .id("commentDetected")
                .fix(Fix::delete(Span::new(start as u32, end as u32)));
        }
    }
}
