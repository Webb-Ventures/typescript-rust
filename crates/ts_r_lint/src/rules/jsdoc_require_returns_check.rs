//! `jsdoc/require-returns-check`: a documented `@returns` is matched by a return value in
//! the function.

use super::prelude::*;
use crate::jsdoc_util::{
    Checks, ContextOpts, defines_name, each_block_checking, fn_flags, function_of,
    may_be_undefined, method_kind, return_may_be_undefined, returns_on_all_branches,
};
use ts_r_ast::{FnFlags, MethodKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-returns-check",
        "Requires a return statement in function body if a `@returns` tag is specified in jsdoc comment.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<RequireReturnsCheck>,
};

#[derive(Debug)]
struct RequireReturnsCheck {
    ctx: ContextOpts,
    exempt_async: bool,
    exempt_generators: Option<bool>,
    no_native_types: bool,
    report_undefined: bool,
}

impl FromOptions for RequireReturnsCheck {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let ctx = ContextOpts::functions_only(opts);
        let flag = |k: &str| ctx.opts.get(k).and_then(Value::as_bool);
        Ok(RequireReturnsCheck {
            exempt_async: flag("exemptAsync").unwrap_or(true),
            exempt_generators: flag("exemptGenerators"),
            no_native_types: flag("noNativeTypes").unwrap_or(true),
            report_undefined: flag("reportMissingReturnForUndefinedTypes").unwrap_or(false),
            ctx,
        })
    }
}

/// Types that can't be what an async function (a Promise) returns.
const NATIVE_TYPES: &[&str] = &[
    "undefined",
    "null",
    "boolean",
    "number",
    "bigint",
    "string",
    "symbol",
    "object",
    "Array",
    "Function",
    "Date",
    "RegExp",
];

/// `asserts x` (an assertion signature) anywhere in the type.
fn has_asserts(ty: &str) -> bool {
    ty.match_indices("asserts")
        .any(|(i, m)| ty[i + m.len()..].starts_with(char::is_whitespace))
}

impl Rule for RequireReturnsCheck {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        // Every name-defining tag in the file, and whether its type allows `undefined`.
        let mut typedefs: Vec<(String, bool)> = Vec::new();
        let all = Checks {
            private: true,
            internal: true,
        };
        each_block_checking(cx, all, |_, finder, doc| {
            let mode = finder.settings.mode.as_str();
            for t in &doc.block.tags {
                if defines_name(&t.tag, mode) {
                    typedefs.push((t.name.clone(), may_be_undefined(Some(t))));
                }
            }
        });
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
            let Some(func) = function_of(finder, doc.target) else {
                return;
            };
            let flags = fn_flags(finder.ast, func);
            let is_async = flags.contains(FnFlags::ASYNC);
            if self.exempt_async && is_async {
                return;
            }
            let Some(name) = doc.preferred(cx, finder, "returns") else {
                return;
            };
            let mut tags = doc.block.tags.iter().filter(|t| t.tag == name);
            let Some(tag) = tags.next() else {
                return;
            };
            if tags.next().is_some() {
                cx.report(doc.comment, format!("Found more than one @{name} declaration."));
                return;
            }
            let ty = tag.typ.trim();
            if has_asserts(ty) {
                return;
            }
            let ast = finder.ast;
            let never = ty == "never";
            if never && crate::jsdoc::has_return_value(ast, func, false) {
                cx.report(
                    doc.comment,
                    format!(
                        "JSDoc @{name} declaration set with \"never\" but return expression is present in function."
                    ),
                );
                return;
            }
            if self.no_native_types && is_async && NATIVE_TYPES.contains(&ty) {
                cx.report(
                    doc.comment,
                    "Function is async or otherwise returns a Promise but the return type is a native type.",
                );
                return;
            }
            let may_undefined = || {
                may_be_undefined(Some(tag))
                    || return_may_be_undefined(ty, |n| {
                        typedefs.iter().find(|(t, _)| t == n).is_some_and(|&(_, u)| u)
                    })
            };
            let missing = if tag.typ.is_empty() {
                !crate::jsdoc::has_return_value(ast, func, self.exempt_async)
            } else {
                !returns_on_all_branches(ast, func, self.exempt_async)
            };
            let exempt_generators = self
                .exempt_generators
                .unwrap_or(finder.settings.mode == "typescript");
            if !never
                && (self.report_undefined || !may_undefined())
                && missing
                && !(exempt_generators && flags.contains(FnFlags::GENERATOR))
            {
                cx.report(
                    doc.comment,
                    format!("JSDoc @{name} declaration present but return expression not available in function."),
                );
            }
        });
    }
}
