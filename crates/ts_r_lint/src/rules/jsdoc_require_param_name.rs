//! `jsdoc/require-param-name`: every `@param` tag has a name.

use super::prelude::*;
use crate::jsdoc_util::{ContextOpts, preferred_tags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-param-name",
        "Requires that all `@param` tags have names",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<RequireParamName>,
};

#[derive(Debug)]
struct RequireParamName(ContextOpts);

impl FromOptions for RequireParamName {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        ContextOpts::parse(opts).map(RequireParamName)
    }
}

impl Rule for RequireParamName {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        self.0.each_doc(cx, |cx, finder, doc| {
            let Some((name, tags)) = preferred_tags(cx, finder, doc, "param") else {
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
