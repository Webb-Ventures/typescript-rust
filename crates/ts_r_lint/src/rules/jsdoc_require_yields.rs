//! `jsdoc/require-yields`: a generator that yields documents it with `@yields` (and,
//! optionally, what `next()` receives with `@next`).

use super::prelude::*;
use crate::jsdoc::{Doc, Finder};
use crate::jsdoc_util::{
    ContextOpts, exempted_by, has_exempting_tag, has_yield_value, is_generator, replaced_docs,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-yields",
        "Requires yields are documented with `@yields` tags.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<RequireYields>,
};

#[derive(Debug)]
struct RequireYields {
    ctx: ContextOpts,
    force_next: bool,
    force_yields: bool,
    next: bool,
    next_with_generator_tag: bool,
    with_generator_tag: bool,
}

impl FromOptions for RequireYields {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let ctx = ContextOpts::parse(opts)?;
        let flag = |k: &str, d: bool| ctx.opts.get(k).and_then(Value::as_bool).unwrap_or(d);
        Ok(RequireYields {
            force_next: flag("forceRequireNext", false),
            force_yields: flag("forceRequireYields", false),
            next: flag("next", false),
            next_with_generator_tag: flag("nextWithGeneratorTag", false),
            with_generator_tag: flag("withGeneratorTag", true),
            ctx,
        })
    }
}

const FUNCTION_TYPES: &[&str] = &[
    "ArrowFunctionExpression",
    "FunctionDeclaration",
    "FunctionExpression",
    "MethodDefinition",
];

/// The preferred name of `name` and whether the block lacks it, reporting a repeated tag.
/// `None` when the tag is blocked.
fn check_tag(
    cx: &mut Ctx<'_, '_>,
    f: &Finder<'_>,
    doc: &Doc,
    name: &str,
) -> Option<(String, bool)> {
    let name = doc.preferred(cx, f, name)?;
    let count = doc.block.tags.iter().filter(|t| t.tag == name).count();
    if count > 1 {
        cx.report(
            doc.comment,
            format!("Found more than one @{name} declaration."),
        );
    }
    Some((name, count == 0))
}

impl Rule for RequireYields {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let settings = crate::jsdoc::Settings::from_cx(cx);
        let all = self.ctx.contexts(&settings).is_ok_and(|c| c.all);
        let exempted = exempted_by(&self.ctx.opts, settings.mode == "closure");
        self.ctx.each_doc(cx, |cx, finder, doc| {
            let b = &doc.block;
            if b.has_any_tag(&[
                "abstract",
                "virtual",
                "class",
                "constructor",
                "type",
                "interface",
            ]) || replaced_docs(finder, doc)
                || has_exempting_tag(b, &exempted)
            {
                return;
            }
            let iterating = !all
                || doc
                    .target
                    .is_some_and(|t| FUNCTION_TYPES.contains(&finder.es_type(t)));
            let generator = is_generator(finder, doc.target);
            // `hasYieldValue` looks through an export to its declaration; the `next` check
            // doesn't.
            let yields = |next: bool| {
                doc.target.is_some_and(|t| match t {
                    crate::jsdoc::Target::Export(_) if next => false,
                    t => has_yield_value(finder.ast, t.id(), next),
                })
            };
            if let Some((name, missing)) = check_tag(cx, finder, doc, "yields") {
                let report = missing
                    && ((self.with_generator_tag && b.has_tag("generator"))
                        || (iterating && generator && (self.force_yields || yields(false))));
                if report {
                    cx.report(doc.comment, format!("Missing JSDoc @{name} declaration."));
                }
            }
            if !(self.next || self.next_with_generator_tag || self.force_next) {
                return;
            }
            let Some((name, missing)) = check_tag(cx, finder, doc, "next") else {
                return;
            };
            let report = missing
                && ((self.next_with_generator_tag && b.has_tag("generator"))
                    || ((self.next || self.force_next)
                        && iterating
                        && generator
                        && (self.force_next || yields(true))));
            if report {
                cx.report(doc.comment, format!("Missing JSDoc @{name} declaration."));
            }
        });
    }
}
