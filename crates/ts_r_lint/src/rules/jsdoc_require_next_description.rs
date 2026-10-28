//! `jsdoc/require-next-description`: every `@next` tag has a name or description.

use super::prelude::*;
use crate::jsdoc_util::each_block;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-next-description",
        "Requires a description for `@next` tags",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<RequireNextDescription>,
};

#[derive(Debug, Default)]
struct RequireNextDescription;

impl Rule for RequireNextDescription {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, _, doc| {
            let missing = doc.block.tags.iter().any(|t| {
                matches!(t.tag.as_str(), "next") && t.name.is_empty() && t.description.is_empty()
            });
            if missing {
                cx.report(doc.comment, "@next should have a description");
            }
        });
    }
}
