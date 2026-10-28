//! `jsdoc/require-template-description`: every `@template` tag has a description.

use super::prelude::*;
use crate::jsdoc_util::each_block;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-template-description",
        "Requires a description for `@template` tags",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<RequireTemplateDescription>,
};

#[derive(Debug, Default)]
struct RequireTemplateDescription;

impl Rule for RequireTemplateDescription {
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
                .any(|t| t.tag == "template" && t.description.is_empty());
            if missing {
                cx.report(doc.comment, "@template should have a description");
            }
        });
    }
}
