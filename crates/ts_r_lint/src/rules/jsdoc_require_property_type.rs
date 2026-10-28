//! `jsdoc/require-property-type`: every `@property` tag has a type.

use super::prelude::*;
use crate::jsdoc_util::{each_block, preferred_tags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-property-type",
        "Requires that each `@property` tag has a type value (in curly brackets)",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<RequirePropertyType>,
};

#[derive(Debug, Default)]
struct RequirePropertyType;

impl Rule for RequirePropertyType {
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
                if t.typ.is_empty() {
                    let msg = format!("Missing JSDoc @{name} \"{}\" type.", t.name);
                    cx.report(doc.line_span(finder, doc.block.tag_line(i)), msg);
                }
            }
        });
    }
}
