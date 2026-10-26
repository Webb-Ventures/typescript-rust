//! `jsdoc/require-throws-description`: every `@throws` (or `@exception`) tag has a description.

use super::prelude::*;
use crate::jsdoc_util::each_block;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-throws-description",
        "Requires a description for `@throws` tags",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<RequireThrowsDescription>,
};

#[derive(Debug, Default)]
struct RequireThrowsDescription;

impl Rule for RequireThrowsDescription {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, _, doc| {
            let missing = doc.block.tags.iter().any(|t| {
                matches!(t.tag.as_str(), "throws" | "exception") && t.description.is_empty()
            });
            if missing {
                cx.report(doc.comment, "@throws should have a description");
            }
        });
    }
}
