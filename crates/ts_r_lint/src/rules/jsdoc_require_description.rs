//! `jsdoc/require-description`: a block has a description, in the block body (`body`, the
//! default), as a `@description` tag (`tag`), or either (`any`).

use super::prelude::*;
use crate::jsdoc::{self, Preferred};
use crate::jsdoc_util::{
    ContextOpts, SpecialMethods, avoid_docs, description, js_trim, tag_description,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-description",
        "Requires that all functions (and potentially other contexts) have a description.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<RequireDescription>,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    Body,
    Tag,
    Any,
}

#[derive(Debug)]
struct RequireDescription {
    ctx: ContextOpts,
    style: Style,
    special: SpecialMethods,
}

impl FromOptions for RequireDescription {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let ctx = ContextOpts::parse(opts)?;
        let style = match ctx.opts.get("descriptionStyle").and_then(Value::as_str) {
            None | Some("body") => Style::Body,
            Some("tag") => Style::Tag,
            Some("any") => Style::Any,
            Some(s) => return Err(format!("invalid descriptionStyle `{s}`")),
        };
        let special = SpecialMethods::from_options(
            &ctx.opts,
            SpecialMethods {
                constructors: true,
                getters: true,
                setters: true,
            },
        );
        Ok(RequireDescription {
            ctx,
            style,
            special,
        })
    }
}

impl Rule for RequireDescription {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        self.ctx.each_doc(cx, |cx, f, doc| {
            if avoid_docs(f, doc, &self.ctx.opts, Some(self.special)) {
                return;
            }
            let b = &doc.block;
            let (target, blocked) = match jsdoc::preferred_tag(f.settings, "description") {
                Preferred::Name(n) => (n, false),
                Preferred::Blocked(msg) => {
                    if !b.has_tag("description") {
                        return;
                    }
                    if self.style == Style::Tag {
                        let msg = msg.unwrap_or_else(|| "Unexpected tag `@description`".into());
                        let span = match b.tags.iter().position(|t| t.tag.eq_ignore_ascii_case("description")) {
                            Some(i) => doc.line_span(f, b.tag_line(i)),
                            None => doc.comment,
                        };
                        cx.report(span, msg);
                        return;
                    }
                    ("description".to_owned(), true)
                }
            };
            if self.style != Style::Tag {
                if !js_trim(&description(b).text()).is_empty() {
                    return;
                }
                if self.style == Style::Body {
                    let msg = match b.tags.iter().find(|t| t.tag == "desc" || t.tag == "description") {
                        Some(t) => format!(
                            "Remove the @{0} tag to leave a plain block description or add additional description text above the @{0} line.",
                            t.tag
                        ),
                        None => "Missing JSDoc block description.".to_owned(),
                    };
                    cx.report(doc.comment, msg);
                    return;
                }
            }
            let examples: Vec<usize> = if blocked {
                Vec::new()
            } else {
                (0..b.tags.len()).filter(|&i| b.tags[i].tag == target).collect()
            };
            if examples.is_empty() {
                let msg = if self.style == Style::Any {
                    format!("Missing JSDoc block description or @{target} declaration.")
                } else {
                    format!("Missing JSDoc @{target} declaration.")
                };
                cx.report(doc.comment, msg);
                return;
            }
            for i in examples {
                let text = format!("{} {}", b.tags[i].name, tag_description(b, i));
                if js_trim(&text).is_empty() {
                    let span = doc.line_span(f, b.tag_line(i));
                    cx.report(span, format!("Missing JSDoc @{target} description."));
                }
            }
        });
    }
}
