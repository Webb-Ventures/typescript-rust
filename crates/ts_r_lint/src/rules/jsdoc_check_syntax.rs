//! `jsdoc/check-syntax`: no Closure-style optional types (`{string=}`) outside the `closure`
//! and `permissive` modes.

use super::prelude::*;
use crate::jsdoc_util::each_block;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/check-syntax",
        "Reports against syntax not valid for the mode (e.g., Google Closure Compiler in non-Closure mode).",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<CheckSyntax>,
};

#[derive(Debug, Default)]
struct CheckSyntax;

impl Rule for CheckSyntax {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, finder, doc| {
            if !matches!(finder.settings.mode.as_str(), "jsdoc" | "typescript") {
                return;
            }
            // The first tag whose type ends in `=`.
            if let Some(i) = doc.block.tags.iter().position(|t| t.typ.ends_with('=')) {
                let span = doc.line_span(finder, doc.block.tag_line(i));
                cx.report(span, "Syntax should not be Google Closure Compiler style.");
            }
        });
    }
}
