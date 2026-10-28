//! `jsdoc/require-throws`: a function that throws documents it with `@throws`.

use super::prelude::*;
use crate::jsdoc_util::{
    ContextOpts, exempted_by, function_of, has_exempting_tag, has_throw_value, replaced_docs,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-throws",
        "Requires that throw statements are documented with `@throws` tags.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<RequireThrows>,
};

#[derive(Debug)]
struct RequireThrows(ContextOpts);

impl FromOptions for RequireThrows {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        ContextOpts::parse(opts).map(RequireThrows)
    }
}

const FUNCTION_TYPES: &[&str] = &[
    "ArrowFunctionExpression",
    "FunctionDeclaration",
    "FunctionExpression",
    "MethodDefinition",
];

impl Rule for RequireThrows {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let settings = crate::jsdoc::Settings::from_cx(cx);
        let all = self.0.contexts(&settings).is_ok_and(|c| c.all);
        let exempted = exempted_by(&self.0.opts, settings.mode == "closure");
        self.0.each_doc(cx, |cx, finder, doc| {
            if doc.block.has_any_tag(&["abstract", "virtual", "type"])
                || replaced_docs(finder, doc)
                || has_exempting_tag(&doc.block, &exempted)
            {
                return;
            }
            let Some(name) = doc.preferred(cx, finder, "throws") else {
                return;
            };
            let iterating = !all
                || doc
                    .target
                    .is_some_and(|t| FUNCTION_TYPES.contains(&finder.es_type(t)));
            let throws = || {
                iterating
                    && doc.target.is_some_and(|t| {
                        FUNCTION_TYPES[..3].contains(&finder.es_type(t))
                            && function_of(finder, Some(t))
                                .is_some_and(|f| has_throw_value(finder.ast, f))
                    })
            };
            match doc.block.tags.iter().find(|t| t.tag == name) {
                Some(tag) => {
                    if tag.typ.trim() == "never" && throws() {
                        cx.report(
                            doc.comment,
                            format!(
                                "JSDoc @{name} declaration set to \"never\" but throw value found."
                            ),
                        );
                    }
                }
                None => {
                    if throws() {
                        cx.report(doc.comment, format!("Missing JSDoc @{name} declaration."));
                    }
                }
            }
        });
    }
}
