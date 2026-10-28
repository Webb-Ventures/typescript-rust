//! `jsdoc/check-access`: `@access` has a valid level, and a block has at most one
//! access-control tag.

use super::prelude::*;
use crate::jsdoc_util::{Checks, each_block_checking, preferred_tags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/check-access",
        "Checks that `@access` tags have a valid value",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<CheckAccess>,
};

const LEVELS: &[&str] = &["package", "private", "protected", "public"];

#[derive(Debug, Default)]
struct CheckAccess;

impl Rule for CheckAccess {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let checks = Checks {
            private: true,
            internal: false,
        };
        each_block_checking(cx, checks, |cx, finder, doc| {
            if let Some((name, tags)) = preferred_tags(cx, finder, doc, "access") {
                for i in tags {
                    let t = &doc.block.tags[i];
                    let level = format!("{} {}", t.name, t.description);
                    if !LEVELS.contains(&level.trim()) {
                        let span = doc.line_span(finder, doc.block.tag_line(i));
                        cx.report(span, format!("Missing valid JSDoc @{name} level."));
                    }
                }
            }
            let access = doc.block.tags.iter().filter(|t| t.tag == "access").count();
            let levels = doc
                .block
                .tags
                .iter()
                .filter(|t| LEVELS.contains(&t.tag.as_str()))
                .count();
            if access > 0 && levels > 0 {
                cx.report(
                    doc.comment,
                    "The @access tag may not be used with specific access-control tags (@package, @private, @protected, or @public).",
                );
            }
            if access > 1 || levels > 1 {
                cx.report(
                    doc.comment,
                    "At most one access-control tag may be present on a JSDoc block.",
                );
            }
        });
    }
}
