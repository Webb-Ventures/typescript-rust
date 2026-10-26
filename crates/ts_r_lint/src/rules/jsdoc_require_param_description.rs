//! `jsdoc/require-param-description`: every `@param` tag has a description.

use super::prelude::*;
use crate::jsdoc::{self, Contexts, Settings};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-param-description",
        "Requires that each `@param` tag has a `description` value",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<RequireParamDescription>,
};

#[derive(Debug)]
struct RequireParamDescription {
    opts: serde_json::Map<String, Value>,
    set_default_root: bool,
    default_root: String,
}

impl FromOptions for RequireParamDescription {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0).clone();
        if o.contains_key("contexts") {
            Contexts::from_options(&o, &Settings::default_for_validation())?;
        }
        Ok(RequireParamDescription {
            set_default_root: bool_field(&o, "setDefaultDestructuredRootDescription", false),
            default_root: str_field(&o, "defaultDestructuredRootDescription")
                .unwrap_or("The root object")
                .to_owned(),
            opts: o,
        })
    }
}

impl Rule for RequireParamDescription {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let settings = Settings::from_cx(cx);
        let Ok(contexts) = Contexts::from_options(&self.opts, &settings) else {
            return;
        };
        jsdoc::for_each_doc(cx, &settings, &contexts, |cx, finder, doc| {
            let Some(tag) = doc.preferred(cx, finder, "param") else {
                return;
            };
            let params = finder.params(doc.target);
            let mut root: i64 = -1;
            for i in 0..doc.block.tags.len() {
                if doc.block.tags[i].tag != tag {
                    continue;
                }
                let t = &doc.block.tags[i];
                if !t.name.contains('.') {
                    root += 1;
                }
                if !t.description.trim().is_empty() {
                    continue;
                }
                let line = doc.block.tag_line(i);
                let destructured = usize::try_from(root)
                    .ok()
                    .and_then(|r| params.get(r))
                    .is_some_and(|&p| finder.is_destructured(p));
                if destructured {
                    if settings.exempt_destructured_roots {
                        continue;
                    }
                    if self.set_default_root {
                        let first = doc.block.tags[i].source[0];
                        let tokens = &mut doc.block.lines[first].tokens;
                        tokens.description = self.default_root.clone();
                        tokens.post_name = " ".to_owned();
                        let span = doc.line_span(finder, line);
                        let fix = doc.fix();
                        cx.report(span, format!("Missing root description for @{tag}."))
                            .fix(fix);
                        continue;
                    }
                }
                let msg = format!(
                    "Missing JSDoc @{tag} \"{}\" description.",
                    doc.block.tags[i].name
                );
                cx.report(doc.line_span(finder, line), msg);
            }
        });
    }
}
