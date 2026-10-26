//! `jsdoc/require-yields-check`: a documented `@yields` (or `@next`) is matched by a
//! `yield` in the generator.

use super::prelude::*;
use crate::jsdoc::{Doc, Finder, Tag, Target};
use crate::jsdoc_util::{
    ContextOpts, has_yield_value, is_generator, may_be_undefined, method_kind,
};
use ts_r_ast::MethodKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-yields-check",
        "Ensures that if a `@yields` is present that a `yield` (or `yield` with a value) is present in the function body (or that if a `@next` is present that there is a yield with a return value present).",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<RequireYieldsCheck>,
};

#[derive(Debug)]
struct RequireYieldsCheck {
    ctx: ContextOpts,
    generators_only: bool,
    next: bool,
}

impl FromOptions for RequireYieldsCheck {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let ctx = ContextOpts::functions_only(opts);
        let flag = |k: &str| ctx.opts.get(k).and_then(Value::as_bool).unwrap_or(false);
        Ok(RequireYieldsCheck {
            generators_only: flag("checkGeneratorsOnly"),
            next: flag("next"),
            ctx,
        })
    }
}

/// The preferred name of `name` and its only tag; reports a repeated tag (and then gives
/// nothing, as for a blocked or missing tag).
fn the_tag<'d>(
    cx: &mut Ctx<'_, '_>,
    f: &Finder<'_>,
    doc: &'d Doc,
    name: &str,
) -> Option<(String, &'d Tag)> {
    let name = doc.preferred(cx, f, name)?;
    let mut tags = doc.block.tags.iter().filter(|t| t.tag == name);
    let first = tags.next()?;
    if tags.next().is_some() {
        cx.report(
            doc.comment,
            format!("Found more than one @{name} declaration."),
        );
        return None;
    }
    Some((name, first))
}

impl Rule for RequireYieldsCheck {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        self.ctx.each_doc(cx, |cx, finder, doc| {
            let closure = finder.settings.mode == "closure";
            let b = &doc.block;
            let class = finder.class_block(doc.target);
            let class_has = |t: &str| class.as_ref().is_some_and(|c| c.has_tag(t));
            if b.has_any_tag(&["abstract", "virtual", "class", "constructor", "interface"])
                || (closure && b.has_tag("record"))
                || method_kind(finder, doc.target) == Some(MethodKind::Constructor)
                || class_has("interface")
                || (closure && class_has("record"))
            {
                return;
            }
            let generator = is_generator(finder, doc.target);
            let yields = |next: bool| {
                doc.target.is_some_and(|t| match t {
                    Target::Export(_) if next => false,
                    t => has_yield_value(finder.ast, t.id(), next),
                })
            };
            if let Some((name, tag)) = the_tag(cx, finder, doc, "yields") {
                if tag.typ.trim() == "never" {
                    if yields(false) {
                        cx.report(doc.comment, format!("JSDoc @{name} declaration set with \"never\" but yield expression is present in function."));
                    }
                } else if (self.generators_only && !generator)
                    || (!may_be_undefined(Some(tag)) && !yields(false))
                {
                    cx.report(doc.comment, format!("JSDoc @{name} declaration present but yield expression not available in function."));
                }
            }
            if !self.next {
                return;
            }
            if let Some((name, tag)) = the_tag(cx, finder, doc, "next") {
                if tag.typ.trim() == "never" {
                    if yields(true) {
                        cx.report(doc.comment, format!("JSDoc @{name} declaration set with \"never\" but yield expression with return value is present in function."));
                    }
                } else if (self.generators_only && !generator)
                    || (!may_be_undefined(Some(tag)) && !yields(true))
                {
                    cx.report(doc.comment, format!("JSDoc @{name} declaration present but yield expression with return value not available in function."));
                }
            }
        });
    }
}
