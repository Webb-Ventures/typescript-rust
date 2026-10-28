//! `no-inline-comments`: comments on the same line as code.

use super::no_warning_comments::{comment_value, is_directive};
use super::prelude::*;
use ts_r_ast::CommentKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-inline-comments",
        "Disallow inline comments after code",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoInlineComments>,
};

#[derive(Debug, Default)]
struct NoInlineComments {
    ignore: Option<String>,
}

impl FromOptions for NoInlineComments {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoInlineComments {
            ignore: str_field(obj_opt(opts, 0), "ignorePattern").map(str::to_owned),
        })
    }
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

impl Rule for NoInlineComments {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        // Line start offsets, to find each comment's line without rescanning it.
        let mut starts = vec![0usize];
        let mut it = src.char_indices().peekable();
        while let Some((i, ch)) = it.next() {
            if is_line_break(ch) {
                if ch == '\r' && it.peek().is_some_and(|&(_, n)| n == '\n') {
                    continue;
                }
                starts.push(i + ch.len_utf8());
            }
        }
        for c in &ast.comments {
            if c.kind == CommentKind::Hashbang {
                continue;
            }
            let (s, e) = (c.span.start as usize, c.span.end as usize);
            let line_start = starts[starts.partition_point(|&l| l <= s) - 1];
            let next = starts.partition_point(|&l| l <= e);
            let line_end = match starts.get(next) {
                Some(&n) => src[..n].trim_end_matches(is_line_break).len().max(e),
                None => src.len(),
            };
            let pre = js_trim(&src[line_start..s]);
            let post = js_trim(&src[e..line_end]);
            if pre.is_empty() && post.is_empty() {
                continue;
            }
            let value = comment_value(c.kind, c.span.text(src));
            if let Some(p) = &self.ignore
                && crate::regex_exec::test(p, "u", value)
            {
                continue;
            }
            if (pre.is_empty() || pre == "{")
                && (post.is_empty() || post == "}")
                && in_jsx_empty(cx, c.span)
            {
                continue;
            }
            if is_directive(c.kind, value) {
                continue;
            }
            cx.report(c.span, "Unexpected comment inline with code.")
                .id("unexpectedInlineComment");
        }
    }
}

/// The comment is the only content of a JSX `{}` container.
fn in_jsx_empty(cx: &Ctx<'_, '_>, span: Span) -> bool {
    let ast = cx.ast();
    let mut stack = vec![ast.root];
    while let Some(n) = stack.pop() {
        let ns = cx.span(n);
        if ns.start > span.start || ns.end < span.end {
            continue;
        }
        if matches!(ast.node(n), Node::JsxExprContainer { expr: None }) {
            return true;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}
