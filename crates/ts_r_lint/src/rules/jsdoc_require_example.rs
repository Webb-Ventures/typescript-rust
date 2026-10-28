//! `jsdoc/require-example`: function blocks (or the `contexts` given) have an `@example`.
//!
//! A block without `@example` reports `Missing JSDoc @example declaration.` (the fix makes a
//! one-line block multiline and adds an empty `@example`), unless `exemptNoArguments` is on
//! and a function (or variable holding one) takes no parameters. Each `@example` whose name
//! and description are blank reports `Missing JSDoc @example description.` on its line.
//! Skipped: replaced docs, `exemptedBy` tags (default `@inheritdoc`), constructors and
//! `@class`/`@constructor` blocks unless `checkConstructors` (default on), getters unless
//! `checkGetters`, setters unless `checkSetters`.

use super::prelude::*;
use crate::jsdoc::Target;
use crate::jsdoc_util::{
    ContextOpts, add_tag, exempted_by, has_exempting_tag, iterating_function_or_variable,
    make_multiline, replaced_docs, tag_description,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-example",
        "Requires that all functions (and potentially other contexts) have examples",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<RequireExample>,
};

#[derive(Debug)]
struct RequireExample {
    ctx: ContextOpts,
    enable_fixer: bool,
    exempt_no_arguments: bool,
    check_constructors: bool,
    check_getters: bool,
    check_setters: bool,
}

impl FromOptions for RequireExample {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(RequireExample {
            enable_fixer: bool_field(o, "enableFixer", true),
            exempt_no_arguments: bool_field(o, "exemptNoArguments", false),
            check_constructors: bool_field(o, "checkConstructors", true),
            check_getters: bool_field(o, "checkGetters", false),
            check_setters: bool_field(o, "checkSetters", false),
            ctx: ContextOpts::parse(opts)?,
        })
    }
}

impl Rule for RequireExample {
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
            let node = match doc.target {
                Some(Target::Node(id)) => Some(id),
                _ => None,
            };
            let own = node.and_then(|id| match *finder.ast.node(id) {
                Node::MethodDef { kind, .. } => Some(kind),
                _ => None,
            });
            let parent = node
                .and_then(|id| finder.parents.parent(id))
                .and_then(|p| match *finder.ast.node(p) {
                    Node::MethodDef { kind, .. } => Some(kind),
                    _ => None,
                });
            let ctor = own == Some(MethodKind::Constructor)
                || parent == Some(MethodKind::Constructor)
                || doc.block.has_any_tag(&["class", "constructor"]);
            if replaced_docs(finder, doc)
                || (!self.check_constructors && ctor)
                || (parent == Some(MethodKind::Get) && !self.check_getters)
                || (parent == Some(MethodKind::Set) && !self.check_setters)
                || has_exempting_tag(&doc.block, &exempted)
            {
                return;
            }
            let examples: Vec<usize> = doc
                .block
                .tags
                .iter()
                .enumerate()
                .filter(|(_, t)| t.tag == "example")
                .map(|(i, _)| i)
                .collect();
            if examples.is_empty() {
                if self.exempt_no_arguments
                    && iterating_function_or_variable(finder, all, doc.target)
                    && finder.params(doc.target).is_empty()
                {
                    return;
                }
                let fix = self.enable_fixer.then(|| {
                    if doc.block.source.len() == 1 {
                        make_multiline(&mut doc.block, &doc.indent);
                    }
                    add_tag(doc, "example");
                    doc.fix()
                });
                let r = cx.report(doc.comment, "Missing JSDoc @example declaration.");
                match fix {
                    Some(f) => r.fix(f),
                    // The plugin still offers a fix that rewrites the block unchanged.
                    None => r.fix(doc.fix()),
                };
                return;
            }
            for ti in examples {
                let content = format!(
                    "{} {}",
                    doc.block.tags[ti].name,
                    tag_description(&doc.block, ti)
                );
                if content.trim().split('\n').all(str::is_empty) {
                    let span = doc.line_span(finder, doc.block.tag_line(ti));
                    cx.report(span, "Missing JSDoc @example description.");
                }
            }
        });
    }
}
