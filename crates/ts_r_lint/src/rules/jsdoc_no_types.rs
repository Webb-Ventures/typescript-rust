//! `jsdoc/no-types`: no `{type}` on `@param`/`@returns` (TypeScript has the types), nor on a
//! class's `@property`.

use super::prelude::*;
use crate::jsdoc_util::{ContextOpts, is_virtual_function, iterating_function_or_variable};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/no-types",
        "This rule reports types being used on `@param` or `@returns` (redundant with TypeScript).",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NoTypes>,
};

const DEFAULTS: &[&str] = &[
    "ArrowFunctionExpression",
    "FunctionDeclaration",
    "FunctionExpression",
    "TSDeclareFunction",
    "TSMethodSignature",
    "ClassDeclaration",
];

#[derive(Debug)]
struct NoTypes(ContextOpts);

impl FromOptions for NoTypes {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        ContextOpts::parse_with(opts, DEFAULTS).map(NoTypes)
    }
}

impl Rule for NoTypes {
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
            if !iterating_function_or_variable(finder, all, doc.target)
                && !is_virtual_function(all, &doc.block)
            {
                return;
            }
            let is_class = doc
                .target
                .is_some_and(|t| finder.es_type(t) == "ClassDeclaration");
            for i in 0..doc.block.tags.len() {
                let t = &doc.block.tags[i];
                let message = match t.tag.as_str() {
                    _ if t.typ.is_empty() => continue,
                    "param" | "arg" | "argument" | "returns" | "return" => {
                        format!("Types are not permitted on @{}.", t.tag)
                    }
                    "prop" | "property" if is_class => format!(
                        "Types are not permitted on @{} in the supplied context.",
                        t.tag
                    ),
                    _ => continue,
                };
                let span = doc.line_span(finder, doc.block.tag_line(i));
                for id in doc.block.tags[i].source.clone() {
                    let tokens = &mut doc.block.lines[id].tokens;
                    tokens.post_tag.clear();
                    tokens.typ.clear();
                }
                let fix = doc.fix();
                cx.report(span, message).fix(fix);
            }
        });
    }
}
