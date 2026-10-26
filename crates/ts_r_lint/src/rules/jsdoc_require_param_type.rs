//! `jsdoc/require-param-type`: every `@param` tag has a type.

use super::prelude::*;
use crate::jsdoc_util::{ContextOpts, preferred_tags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-param-type",
        "Requires that each `@param` tag has a type value (in curly brackets)",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<RequireParamType>,
};

#[derive(Debug)]
struct RequireParamType {
    base: ContextOpts,
    set_default_root: bool,
    default_root: String,
}

impl FromOptions for RequireParamType {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let base = ContextOpts::parse(opts)?;
        Ok(RequireParamType {
            set_default_root: bool_field(&base.opts, "setDefaultDestructuredRootType", false),
            default_root: str_field(&base.opts, "defaultDestructuredRootType")
                .unwrap_or("object")
                .to_owned(),
            base,
        })
    }
}

impl Rule for RequireParamType {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        self.base.each_doc(cx, |cx, finder, doc| {
            let Some((name, tags)) = preferred_tags(cx, finder, doc, "param") else {
                return;
            };
            let params = finder.params(doc.target);
            let mut root: i64 = -1;
            for i in tags {
                let t = &doc.block.tags[i];
                if !t.name.contains('.') {
                    root += 1;
                }
                if !t.typ.is_empty() {
                    continue;
                }
                let line = doc.block.tag_line(i);
                let destructured = usize::try_from(root)
                    .ok()
                    .and_then(|r| params.get(r))
                    .is_some_and(|&p| finder.is_destructured(p));
                if destructured {
                    if finder.settings.exempt_destructured_roots {
                        continue;
                    }
                    if self.set_default_root {
                        let first = doc.block.tags[i].source[0];
                        let tokens = &mut doc.block.lines[first].tokens;
                        tokens.typ = format!("{{{}}}", self.default_root);
                        tokens.post_type = " ".to_owned();
                        let fix = doc.fix();
                        cx.report(
                            doc.line_span(finder, line),
                            format!("Missing root type for @{name}."),
                        )
                        .fix(fix);
                        continue;
                    }
                }
                let msg = format!("Missing JSDoc @{name} \"{}\" type.", doc.block.tags[i].name);
                cx.report(doc.line_span(finder, line), msg);
            }
        });
    }
}
