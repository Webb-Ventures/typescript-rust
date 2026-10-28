//! `jsdoc/require-property-name`: every `@property` tag has a name.

use super::prelude::*;
use crate::jsdoc_util::{each_block, preferred_tags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-property-name",
        "Requires that all `@property` tags have names",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<RequirePropertyName>,
};

#[derive(Debug, Default)]
struct RequirePropertyName;

impl Rule for RequirePropertyName {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, finder, doc| {
            let Some((name, tags)) = preferred_tags(cx, finder, doc, "property") else {
                return;
            };
            for i in tags {
                let t = &doc.block.tags[i];
                if !t.name.is_empty() {
                    continue;
                }
                let what = if t.typ.is_empty() { "type" } else { "tag" };
                let span = doc.line_span(finder, doc.block.tag_line(i));
                cx.report(
                    span,
                    format!("There must be an identifier after @{name} {what}."),
                );
            }
        });
    }
}
