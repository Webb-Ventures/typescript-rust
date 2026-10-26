//! `jsdoc/require-returns-description`: every `@returns` tag has a description, unless its
//! type says nothing is returned.

use super::prelude::*;
use crate::jsdoc_util::{ContextOpts, preferred_tags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-returns-description",
        "Requires that the `@returns` tag has a `description` value (not including `void`/`undefined` type returns)",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<RequireReturnsDescription>,
};

/// Types that need no description.
const EMPTY_TYPES: &[&str] = &["Promise<undefined>", "Promise<void>", "undefined", "void"];

#[derive(Debug)]
struct RequireReturnsDescription(ContextOpts);

impl FromOptions for RequireReturnsDescription {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        ContextOpts::parse(opts).map(RequireReturnsDescription)
    }
}

impl Rule for RequireReturnsDescription {
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
                let t = &doc.block.tags[i];
                if EMPTY_TYPES.contains(&t.typ.trim()) || !t.description.trim().is_empty() {
                    continue;
                }
                let span = doc.line_span(finder, doc.block.tag_line(i));
                cx.report(span, format!("Missing JSDoc @{name} description."));
            }
        });
    }
}
