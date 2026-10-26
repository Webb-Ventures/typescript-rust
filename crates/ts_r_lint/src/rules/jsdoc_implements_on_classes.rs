//! `jsdoc/implements-on-classes`: `@implements` only documents constructors (classes,
//! `@class`/`@constructor` functions).

use super::prelude::*;
use crate::jsdoc_util::{
    ContextOpts, is_virtual_function, iterating_function_or_variable, method_kind, preferred_tags,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/implements-on-classes",
        "Prohibits use of `@implements` on non-constructor functions (to enforce the tag only being used on classes/constructors)",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<ImplementsOnClasses>,
};

#[derive(Debug)]
struct ImplementsOnClasses(ContextOpts);

impl FromOptions for ImplementsOnClasses {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        ContextOpts::parse(opts).map(ImplementsOnClasses)
    }
}

impl Rule for ImplementsOnClasses {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let settings = crate::jsdoc::Settings::from_cx(cx);
        let all = self.0.contexts(&settings).is_ok_and(|c| c.all);
        self.0.each_doc(cx, |cx, finder, doc| {
            if iterating_function_or_variable(finder, all, doc.target) {
                if doc.block.has_any_tag(&["class", "constructor"])
                    || method_kind(finder, doc.target) == Some(MethodKind::Constructor)
                {
                    return;
                }
            } else if !is_virtual_function(all, &doc.block) {
                return;
            }
            let Some((_, tags)) = preferred_tags(cx, finder, doc, "implements") else {
                return;
            };
            for i in tags {
                let span = doc.line_span(finder, doc.block.tag_line(i));
                cx.report(span, "@implements used on a non-constructor function");
            }
        });
    }
}
