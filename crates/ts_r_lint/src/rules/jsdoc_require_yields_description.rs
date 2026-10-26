//! `jsdoc/require-yields-description`: every `@yields` (or `@yield`) tag has a name or description.

use super::prelude::*;
use crate::jsdoc_util::each_block;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-yields-description",
        "Requires a description for `@yields` tags",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<RequireYieldsDescription>,
};

#[derive(Debug, Default)]
struct RequireYieldsDescription;

impl Rule for RequireYieldsDescription {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, _, doc| {
            let missing = doc.block.tags.iter().any(|t| {
                matches!(t.tag.as_str(), "yields" | "yield")
                    && t.name.is_empty()
                    && t.description.is_empty()
            });
            if missing {
                cx.report(doc.comment, "@yields should have a description");
            }
        });
    }
}
