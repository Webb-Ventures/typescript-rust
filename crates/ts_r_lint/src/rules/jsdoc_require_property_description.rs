//! `jsdoc/require-property-description`: every `@property` tag has a description.

use super::prelude::*;
use crate::jsdoc_util::{each_block, preferred_tags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-property-description",
        "Requires that each `@property` tag has a description value",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<RequirePropertyDescription>,
};

#[derive(Debug, Default)]
struct RequirePropertyDescription;

impl Rule for RequirePropertyDescription {
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
                if t.description.trim().is_empty() {
                    let msg = format!("Missing JSDoc @{name} \"{}\" description.", t.name);
                    cx.report(doc.line_span(finder, doc.block.tag_line(i)), msg);
                }
            }
        });
    }
}
