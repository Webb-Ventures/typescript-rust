//! `jsdoc/require-property`: a `@typedef` or `@namespace` whose type is a plain object
//! (`object`, `Object`, `PlainObject`) needs a `@property` in its block.

use super::prelude::*;
use crate::diagnostic::Loc;
use crate::jsdoc_util::{add_tag, each_block};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-property",
        "Requires that all `@typedef` and `@namespace` tags have `@property` when their type is a plain `object`, `Object`, or `PlainObject`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<RequireProperty>,
};

#[derive(Debug, Default)]
struct RequireProperty;

impl Rule for RequireProperty {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, finder, doc| {
            let owners: Vec<usize> = (0..doc.block.tags.len())
                .filter(|&i| matches!(doc.block.tags[i].tag.as_str(), "namespace" | "typedef"))
                .collect();
            if owners.is_empty() {
                return;
            }
            let Some(name) = doc.preferred(cx, finder, "property") else {
                cx.report(
                    Span::at(0),
                    "Cannot prohibit `@property` in the `tagNamePreference` setting while using the `require-property` rule.",
                )
                .loc(Loc::at(1, 2));
                return;
            };
            if doc.block.has_tag(&name) {
                return;
            }
            for i in owners {
                if !matches!(
                    doc.block.tags[i].typ.as_str(),
                    "object" | "Object" | "PlainObject"
                ) {
                    continue;
                }
                // Every report adds the tag to the same block (only the first fix applies
                // in a pass, as the fixes overlap).
                add_tag(doc, &name);
                let fix = doc.fix();
                cx.report(doc.comment, format!("Missing JSDoc @{name}."))
                    .fix(fix);
            }
        });
    }
}
