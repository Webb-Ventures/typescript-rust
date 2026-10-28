//! `jsdoc/require-yields-type`: every `@yields` (or `@yield`) tag has a type.

use super::prelude::*;
use crate::jsdoc_util::each_block;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-yields-type",
        "Requires a type for `@yields` tags",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<RequireYieldsType>,
};

#[derive(Debug, Default)]
struct RequireYieldsType;

impl Rule for RequireYieldsType {
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
                .any(|t| matches!(t.tag.as_str(), "yields" | "yield") && t.typ.is_empty());
            if missing {
                cx.report(doc.comment, "@yields should have a type");
            }
        });
    }
}
