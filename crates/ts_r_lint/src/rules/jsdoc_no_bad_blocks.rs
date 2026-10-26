//! `jsdoc/no-bad-blocks`: a block comment that looks like JSDoc (it has tags) but opens with
//! one asterisk, or three or more, should open with `/**`.
//!
//! Every `/* */` comment except `eslint` directives is checked. A comment with one asterisk
//! is reported when, read as `/**` + the rest, it has a tag outside `ignore`
//! (`ts-check`, `ts-expect-error`, `ts-ignore`, `ts-nocheck`); one with three or more is
//! reported the same way, or always with `preventAllMultiAsteriskBlocks`. The plugin keeps
//! one "had extra asterisks" flag for the whole file, set by any comment that opens with
//! three or more asterisks (reported or not), and every fix reads it when it runs: with the
//! flag set the fix rewrites a leading `/***...` to `/**` (so a one-asterisk comment's fix
//! changes nothing), else the first `/*` becomes `/**`. We keep that.

use super::prelude::*;
use crate::jsdoc::{self, Block};
use ts_r_ast::CommentKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/no-bad-blocks",
        "This rule checks for multi-line-style comments which fail to meet the criteria of a JSDoc block.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NoBadBlocks>,
};

#[derive(Debug)]
struct NoBadBlocks {
    ignore: Vec<String>,
    prevent_all: bool,
}

impl FromOptions for NoBadBlocks {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = opts.first().and_then(Value::as_object);
        let ignore = match o.and_then(|o| o.get("ignore")).and_then(Value::as_array) {
            Some(a) => a
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            None => ["ts-check", "ts-expect-error", "ts-ignore", "ts-nocheck"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        };
        let prevent_all = o
            .and_then(|o| o.get("preventAllMultiAsteriskBlocks"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(NoBadBlocks {
            ignore,
            prevent_all,
        })
    }
}

/// The number of asterisks right after `/`.
fn leading_stars(text: &str) -> usize {
    text.strip_prefix('/')
        .map_or(0, |r| r.len() - r.trim_start_matches('*').len())
}

impl Rule for NoBadBlocks {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        if !jsdoc::settings_load(cx) {
            return;
        }
        let file = cx.file;
        let mut extra = false;
        let mut bad: Vec<Span> = Vec::new();
        for c in &file.ast.comments {
            if c.kind != CommentKind::Block {
                continue;
            }
            let text = c.span.text(file.src);
            let stars = leading_stars(text);
            let initial = if stars == 1 { &text[2..] } else { text };
            if initial.trim_start().starts_with("eslint") {
                continue;
            }
            if stars != 1 {
                if stars < 3 {
                    continue;
                }
                extra = true;
                if self.prevent_all {
                    bad.push(c.span);
                    continue;
                }
            }
            let rest = &text[1 + stars..];
            let block = Block::parse(&format!("/**{rest}"));
            if !block.tags.is_empty() && !block.tags.iter().any(|t| self.ignore.contains(&t.tag)) {
                bad.push(c.span);
            }
        }
        for span in bad {
            let text = span.text(file.src);
            let fixed = if extra {
                let stars = leading_stars(text);
                if stars >= 3 {
                    format!("/**{}", &text[1 + stars..])
                } else {
                    text.to_owned()
                }
            } else {
                text.replacen("/*", "/**", 1)
            };
            cx.report(
                span,
                "Expected JSDoc-like comment to begin with two asterisks.",
            )
            .fix(Fix::replace(span, fixed));
        }
    }
}
