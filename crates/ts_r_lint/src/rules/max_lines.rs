//! `max-lines`: at most `max` (300) lines in a file, optionally not counting blank lines
//! and comment-only lines.

use super::prelude::*;
use crate::diagnostic::Loc;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "max-lines",
        "Enforce a maximum number of lines per file",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<MaxLines>,
};

#[derive(Debug, Default)]
struct MaxLines {
    max: usize,
    skip_comments: bool,
    skip_blank: bool,
}

impl FromOptions for MaxLines {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let max = match opts.first() {
            Some(Value::Number(n)) => n.as_u64(),
            _ => o.get("max").and_then(Value::as_u64),
        }
        .unwrap_or(300) as usize;
        Ok(MaxLines {
            max,
            skip_comments: bool_field(o, "skipComments", false),
            skip_blank: bool_field(o, "skipBlankLines", false),
        })
    }
}

/// The start offset of every line (ESLint's line breaks: `\r\n`, `\r`, `\n`, U+2028/9).
pub(crate) fn line_starts(src: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    let mut it = src.char_indices().peekable();
    while let Some((i, ch)) = it.next() {
        if matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
            if ch == '\r' && it.peek().is_some_and(|&(_, n)| n == '\n') {
                continue;
            }
            starts.push(i + ch.len_utf8());
        }
    }
    starts
}

/// The 0-based lines that hold only comments (and whitespace): each comment's lines, less
/// its first and last when code shares them.
pub(crate) fn comment_only_lines(cx: &Ctx<'_, '_>, starts: &[usize]) -> Vec<bool> {
    let src = cx.src();
    let ast = cx.ast();
    let line_of = |pos: usize| starts.partition_point(|&s| s <= pos) - 1;
    let mut out = vec![false; starts.len()];
    for c in &ast.comments {
        let (s, e) = (c.span.start as usize, c.span.end as usize);
        let first = line_of(s);
        let last = line_of(e.saturating_sub(1).max(s));
        let before = skip_trivia_backward(ast, src, s);
        let first = if before > 0 && line_of(before - 1) == first {
            first + 1
        } else {
            first
        };
        let after = skip_trivia_forward(src, e);
        let end = if after < src.len() && line_of(after) == last {
            last
        } else {
            last + 1
        };
        for l in out.iter_mut().take(end).skip(first) {
            *l = true;
        }
    }
    out
}

impl Rule for MaxLines {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let src = cx.src();
        let starts = line_starts(src);
        let text = |i: usize| {
            let end = starts.get(i + 1).copied().unwrap_or(src.len());
            src[starts[i]..end].trim_end_matches(['\n', '\r', '\u{2028}', '\u{2029}'])
        };
        let total = starts.len();
        let mut lines: Vec<usize> = (0..total).collect();
        if total > 1 && text(total - 1).is_empty() {
            lines.pop();
        }
        if self.skip_blank {
            lines.retain(|&l| {
                !text(l)
                    .trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
                    .is_empty()
            });
        }
        if self.skip_comments {
            let comment_lines = comment_only_lines(cx, &starts);
            lines.retain(|&l| !comment_lines[l]);
        }
        if lines.len() <= self.max {
            return;
        }
        let start_line = lines[self.max];
        let last = total - 1;
        let loc = Loc {
            line: start_line as u32 + 1,
            column: 1,
            end_line: total as u32,
            end_column: text(last).encode_utf16().count() as u32 + 1,
        };
        let span = Span::new(starts[start_line] as u32, src.len() as u32);
        cx.report(
            span,
            format!(
                "File has too many lines ({}). Maximum allowed is {}.",
                lines.len(),
                self.max
            ),
        )
        .id("exceed")
        .loc(loc);
    }
}
