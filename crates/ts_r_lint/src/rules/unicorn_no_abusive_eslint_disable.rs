//! `unicorn/no-abusive-eslint-disable`: an `eslint-disable`, `eslint-disable-line` or
//! `eslint-disable-next-line` comment that names no rule is reported. The report starts at
//! column 0 of the comment's first line (ESLint's column -1), which lies before every
//! directive on that line, so the blanket directive can't silence its own report.

use super::prelude::*;
use crate::diagnostic::Loc;
use crate::directives::{DirectiveKind, InlineConfig};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-abusive-eslint-disable",
        "Enforce specifying rules to disable in `eslint-disable` comments.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoAbusiveEslintDisable>,
};

#[derive(Debug, Default)]
struct NoAbusiveEslintDisable;

impl Rule for NoAbusiveEslintDisable {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let (src, lines) = (cx.src(), cx.file.lines);
        let inline = InlineConfig::parse(cx.ast(), src, lines);
        for d in &inline.directives {
            if d.kind == DirectiveKind::Enable || !d.rules.is_empty() {
                continue;
            }
            let start = lines.line_of(d.comment.start);
            let end = lines.line_col_utf16(src, d.comment.end);
            let line_start = lines.line_start(start).unwrap_or(d.comment.start);
            cx.report(
                Span::at(line_start),
                "Specify the rules you want to disable.",
            )
            .id("no-abusive-eslint-disable")
            .loc(Loc {
                line: start + 1,
                column: 0,
                end_line: end.line + 1,
                end_column: end.col + 1,
            });
        }
    }
}
