//! `jsdoc/no-blank-block-descriptions`: no blank description lines before tags, and at most
//! one in a block without tags.

use super::prelude::*;
use crate::jsdoc::Tokens;
use crate::jsdoc_util::{description, each_block, set_block_description};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/no-blank-block-descriptions",
        "If tags are present, this rule will prevent empty lines in the block description. If no tags are present, this rule will prevent extra empty lines in the block description.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoBlankBlockDescriptions>,
};

#[derive(Debug, Default)]
struct NoBlankBlockDescriptions;

/// Spaces and tabs, a newline, spaces and tabs, a newline, then any whitespace.
fn two_blank_lines(s: &str) -> bool {
    let mut rest = s;
    for _ in 0..2 {
        rest = rest.trim_start_matches([' ', '\t']);
        match rest.strip_prefix('\n') {
            Some(r) => rest = r,
            None => return false,
        }
    }
    rest.trim().is_empty()
}

impl Rule for NoBlankBlockDescriptions {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, finder, doc| {
            let d = description(&doc.block);
            let text = d.text();
            let has_tags = !doc.block.tags.is_empty();
            let blank = if has_tags {
                text.trim().is_empty()
            } else {
                two_blank_lines(&text)
            };
            if d.lines.is_empty() || !blank {
                return;
            }
            let message = if has_tags {
                set_block_description(&mut doc.block, |_| Vec::new());
                "There should be no blank lines in block descriptions followed by tags."
            } else {
                set_block_description(&mut doc.block, |info| {
                    vec![Tokens {
                        start: info.start.clone(),
                        delimiter: info.delimiter.clone(),
                        post_delimiter: info.post_delimiter.clone(),
                        ..Tokens::default()
                    }]
                });
                "There should be no extra blank lines in block descriptions not followed by tags."
            };
            let span = doc.line_span(finder, d.last_line as i64);
            let fix = doc.fix();
            cx.report(span, message).fix(fix);
        });
    }
}
