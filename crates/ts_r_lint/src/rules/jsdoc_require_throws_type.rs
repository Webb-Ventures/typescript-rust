//! `jsdoc/require-throws-type`: every `@throws` (or `@exception`) tag has a type.

use super::prelude::*;
use crate::jsdoc_util::each_block;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-throws-type",
        "Requires a type for `@throws` tags",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<RequireThrowsType>,
};

#[derive(Debug, Default)]
struct RequireThrowsType;

impl Rule for RequireThrowsType {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, _, doc| {
            let missing = doc
                .block
                .tags
                .iter()
                .any(|t| matches!(t.tag.as_str(), "throws" | "exception") && t.typ.is_empty());
            if missing {
                cx.report(doc.comment, "@throws should have a type");
            }
        });
    }
}
