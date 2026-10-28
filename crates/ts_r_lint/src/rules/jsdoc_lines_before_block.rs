//! `jsdoc/lines-before-block`: at least `lines` (1) blank lines between a JSDoc block and the
//! token or comment before it.
//!
//! A block with an `excludedTags` tag (`@type`) is skipped, as is one at the start of the
//! file or (unless `checkBlockStarts`) right after a `(`, `=`, `[` or `{`; then, when the
//! previous token's end line plus `lines` reaches the block's first line, it is reported
//! unless it shares that token's line (`ignoreSameLine`) or fits on one line
//! (`ignoreSingleLines`). The fix inserts the line breaks after the previous token; for a
//! block on the same line it also breaks the line and indents the block like its second
//! line (`/**\n<indent> *`, less the gap the block had), else to the column before the
//! line's first token.

use super::prelude::*;
use crate::jsdoc_util::each_block;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/lines-before-block",
        "Enforces minimum number of newlines before JSDoc comment blocks",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<LinesBeforeBlock>,
};

#[derive(Debug)]
struct LinesBeforeBlock {
    check_block_starts: bool,
    excluded: Vec<String>,
    ignore_same_line: bool,
    ignore_single_lines: bool,
    lines: u32,
}

impl FromOptions for LinesBeforeBlock {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = opts.first().and_then(Value::as_object);
        let flag = |k: &str, d: bool| {
            o.and_then(|o| o.get(k))
                .and_then(Value::as_bool)
                .unwrap_or(d)
        };
        let excluded = match o
            .and_then(|o| o.get("excludedTags"))
            .and_then(Value::as_array)
        {
            Some(a) => a
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            None => vec!["type".to_owned()],
        };
        let lines = o
            .and_then(|o| o.get("lines"))
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .min(10_000) as u32;
        Ok(LinesBeforeBlock {
            check_block_starts: flag("checkBlockStarts", false),
            excluded,
            ignore_same_line: flag("ignoreSameLine", true),
            ignore_single_lines: flag("ignoreSingleLines", true),
            lines,
        })
    }
}

/// The previous token or comment before `pos`: its end, and whether it is one of the
/// punctuators that open a group (`(`, `=`, `[`, `{`).
fn token_before(ast: &ts_r_ast::Ast, src: &str, pos: usize) -> Option<(usize, bool)> {
    let before = &src[..pos];
    let end = before
        .trim_end_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
        .len();
    if end == 0 {
        return None;
    }
    let idx = ast
        .comments
        .partition_point(|c| (c.span.end as usize) < end);
    if ast
        .comments
        .get(idx)
        .is_some_and(|c| c.span.end as usize == end)
    {
        return Some((end, false));
    }
    let b = src.as_bytes();
    let last = b[end - 1];
    let opener = match last {
        b'(' | b'[' | b'{' => true,
        b'=' => !(end >= 2 && b"=!<>+-*/%&|^?".contains(&b[end - 2])),
        _ => false,
    };
    Some((end, opener))
}

impl Rule for LinesBeforeBlock {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.file.ast;
        let src = cx.file.src;
        let lines = cx.file.lines;
        each_block(cx, |cx, _f, doc| {
            let excluded: Vec<&str> = self.excluded.iter().map(String::as_str).collect();
            if doc.block.has_any_tag(&excluded) {
                return;
            }
            let start = doc.comment.start as usize;
            let Some((tok_end, opener)) = token_before(ast, src, start) else {
                return;
            };
            if opener && !self.check_block_starts {
                return;
            }
            let tok_line = lines.line_of(tok_end as u32 - 1);
            let start_line = doc.line;
            if tok_line + self.lines < start_line {
                return;
            }
            let same_line = tok_line == start_line;
            if same_line && self.ignore_same_line {
                return;
            }
            let end_line = lines.line_of(doc.comment.end.saturating_sub(1));
            if self.ignore_single_lines && start_line == end_line {
                return;
            }
            let mut text = "\n".repeat(self.lines as usize);
            if same_line {
                let line_start = lines.line_start(start_line).unwrap_or(0) as usize;
                let col = |at: usize| src[line_start..at].encode_utf16().count();
                let space_diff = col(start).saturating_sub(col(tok_end));
                // `^\*\n([\t ]*) \*` on the comment's value, then `.slice(spaceDiff)`.
                let value = &src[start + 2..doc.comment.end as usize];
                let indent = value
                    .strip_prefix("*\n")
                    .and_then(|rest| {
                        let ws = rest.len() - rest.trim_start_matches([' ', '\t']).len();
                        (ws > 0 && rest[..ws].ends_with(' ') && rest[ws..].starts_with('*'))
                            .then(|| &rest[..ws - 1])
                    })
                    .map(|g| g.get(space_diff.min(g.len())..).unwrap_or("").to_owned())
                    .unwrap_or_default();
                let indent = if indent.is_empty() {
                    let line_text = &src[line_start..start];
                    let first = line_text.len() - line_text.trim_start().len();
                    let start_col = src[line_start..line_start + first].encode_utf16().count();
                    " ".repeat(start_col.saturating_sub(1))
                } else {
                    indent
                };
                text.push('\n');
                text.push_str(&indent);
            }
            let message = format!("Required {} line(s) before JSDoc block", self.lines);
            cx.report(doc.comment, message)
                .fix(Fix::insert(tok_end as u32, text));
        });
    }
}
