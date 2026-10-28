//! `capitalized-comments`: comments should (or, with `"never"`, should not) start with a
//! capital letter.

use super::prelude::*;
use crate::regex;
use ts_r_ast::CommentKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "capitalized-comments",
        "Enforce or disallow capitalization of the first letter of a comment",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<CapitalizedComments>,
};

#[derive(Debug, Default)]
struct KindOptions {
    ignore_pattern: Option<String>,
    ignore_inline: bool,
    ignore_consecutive: bool,
}

#[derive(Debug)]
struct CapitalizedComments {
    never: bool,
    line: KindOptions,
    block: KindOptions,
}

impl FromOptions for CapitalizedComments {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let raw = obj_opt(opts, 1);
        let kind = |which: &str| {
            let o = raw.get(which).and_then(Value::as_object).unwrap_or(raw);
            KindOptions {
                ignore_pattern: o
                    .get("ignorePattern")
                    .and_then(Value::as_str)
                    .filter(|p| !p.is_empty())
                    .map(|p| format!("^\\s*(?:{p})")),
                ignore_inline: bool_field(o, "ignoreInlineComments", false),
                ignore_consecutive: bool_field(o, "ignoreConsecutiveComments", false),
            }
        };
        Ok(CapitalizedComments {
            never: opts.first().and_then(Value::as_str) == Some("never"),
            line: kind("line"),
            block: kind("block"),
        })
    }
}

fn is_js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

/// `^\s*[^:/?#\s]+:\/\/[^?#]`
fn maybe_url(s: &str) -> bool {
    let s = s.trim_start_matches(is_js_space);
    let scheme = s
        .find(|c: char| matches!(c, ':' | '/' | '?' | '#') || is_js_space(c))
        .unwrap_or(s.len());
    scheme > 0
        && s[scheme..]
            .strip_prefix("://")
            .and_then(|r| r.chars().next())
            .is_some_and(|c| c != '?' && c != '#')
}

/// ESLint's `COMMENTS_IGNORE_PATTERN`: directive-like comments.
fn is_directive(value: &str) -> bool {
    let v = value.trim_start_matches(is_js_space);
    if v.starts_with("eslint") || v.starts_with("jscs") {
        return true;
    }
    [
        "jshint", "jslint", "istanbul", "globals", "global", "exported",
    ]
    .iter()
    .any(|w| {
        v.strip_prefix(w)
            .and_then(|r| r.chars().next())
            .is_some_and(is_js_space)
    })
}

impl Rule for CapitalizedComments {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Some(letter) = regex::compile("\\p{L}") else {
            return;
        };
        let is_letter = |c: char| letter.is_match_flags(c.encode_utf8(&mut [0; 4]), "u");
        let line_of = |pos: usize| cx.file.lines.line_col_utf16(src, pos as u32).line;
        for c in &ast.comments {
            let (opts, value) = match c.kind {
                CommentKind::Line => (&self.line, c.span.text(src).get(2..).unwrap_or("")),
                CommentKind::Block => {
                    let t = c.span.text(src);
                    (
                        &self.block,
                        t.get(2..t.len().saturating_sub(2)).unwrap_or(""),
                    )
                }
                _ => continue,
            };
            if is_directive(value) {
                continue;
            }
            let bare = value.replace('*', "");
            if opts
                .ignore_pattern
                .as_deref()
                .and_then(regex::compile)
                .is_some_and(|re| re.is_match_flags(&bare, "u"))
            {
                continue;
            }
            let (start, end) = (c.span.start as usize, c.span.end as usize);
            let before = src[..start].trim_end_matches(is_js_space).len();
            let after = end + (src.len() - end - src[end..].trim_start_matches(is_js_space).len());
            if opts.ignore_inline
                && before > 0
                && after < src.len()
                && line_of(before) == line_of(start)
                && line_of(end) == line_of(after)
            {
                continue;
            }
            if opts.ignore_consecutive
                && ast.comments.iter().any(|p| {
                    p.span.end as usize == before
                        && matches!(p.kind, CommentKind::Line | CommentKind::Block)
                })
            {
                continue;
            }
            if maybe_url(&bare) {
                continue;
            }
            let Some(first) = bare.chars().find(|&ch| !is_js_space(ch)) else {
                continue;
            };
            if !is_letter(first) {
                continue;
            }
            let lower: String = first.to_lowercase().collect();
            let upper: String = first.to_uppercase().collect();
            let is_upper = first.to_string() != lower;
            let is_lower = first.to_string() != upper;
            if !(if self.never { is_upper } else { is_lower }) {
                continue;
            }
            let (id, msg) = if self.never {
                (
                    "unexpectedUppercaseComment",
                    "Comments should not begin with an uppercase character.",
                )
            } else {
                (
                    "unexpectedLowercaseComment",
                    "Comments should not begin with a lowercase character.",
                )
            };
            let fix = value
                .char_indices()
                .find(|&(_, ch)| is_letter(ch))
                .map(|(i, ch)| {
                    let at = c.span.start + 2 + i as u32;
                    let text: String = if self.never {
                        ch.to_lowercase().collect()
                    } else {
                        ch.to_uppercase().collect()
                    };
                    Fix::replace(Span::new(at, at + ch.len_utf8() as u32), text)
                });
            cx.report(c.span, msg).id(id).fix_opt(fix);
        }
    }
}
