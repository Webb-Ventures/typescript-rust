//! `jsdoc/require-returns-type`: every `@returns` tag has a type.

use super::prelude::*;
use crate::jsdoc_util::{ContextOpts, preferred_tags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-returns-type",
        "Requires that `@returns` tag has type value (in curly brackets)",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<RequireReturnsType>,
};

#[derive(Debug)]
struct RequireReturnsType(ContextOpts);

impl FromOptions for RequireReturnsType {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        ContextOpts::parse(opts).map(RequireReturnsType)
    }
}

impl Rule for RequireReturnsType {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        self.0.each_doc(cx, |cx, finder, doc| {
            let Some((name, tags)) = preferred_tags(cx, finder, doc, "returns") else {
                return;
            };
            for i in tags {
                if doc.block.tags[i].typ.is_empty() {
                    let span = doc.line_span(finder, doc.block.tag_line(i));
                    cx.report(span, format!("Missing JSDoc @{name} type."));
                }
            }
        });
    }
}
