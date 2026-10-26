//! `jsdoc/no-defaults`: no `[name=default]` on `@param`, and no value on `@default`.

use super::prelude::*;
use crate::jsdoc_util::ContextOpts;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/no-defaults",
        "This rule reports defaults being used on the relevant portion of `@param` or `@default`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NoDefaults>,
};

#[derive(Debug)]
struct NoDefaults {
    contexts: ContextOpts,
    no_optional: bool,
}

impl FromOptions for NoDefaults {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let contexts = ContextOpts::parse(opts)?;
        let no_optional = contexts
            .opts
            .get("noOptionalParamNames")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(NoDefaults {
            contexts,
            no_optional,
        })
    }
}

/// Whether a parsed name token was `[name]` / `[name=default]`, and whether it had a default.
fn optional_default(token: &str, name: &str) -> (bool, bool) {
    match token.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
        Some(inner) if !name.is_empty() => (true, inner.contains('=')),
        _ => (false, false),
    }
}

impl Rule for NoDefaults {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        self.contexts.each_doc(cx, |cx, finder, doc| {
            for i in 0..doc.block.tags.len() {
                let t = &doc.block.tags[i];
                let Some(&first) = t.source.first() else {
                    continue;
                };
                let tag = t.tag.clone();
                let span = doc.line_span(finder, doc.block.tag_line(i));
                if matches!(tag.as_str(), "param" | "arg" | "argument") {
                    let (optional, default) =
                        optional_default(&doc.block.lines[first].tokens.name, &t.name);
                    let name = t.name.clone();
                    let message = if self.no_optional && optional {
                        doc.block.lines[first].tokens.name = name;
                        format!("Optional param names are not permitted on @{tag}.")
                    } else if default {
                        doc.block.lines[first].tokens.name = format!("[{name}]");
                        format!("Defaults are not permitted on @{tag}.")
                    } else {
                        continue;
                    };
                    let fix = doc.fix();
                    cx.report(span, message).fix(fix);
                } else if matches!(tag.as_str(), "default" | "defaultvalue")
                    && !t.description.trim().is_empty()
                {
                    let tokens = &mut doc.block.lines[first].tokens;
                    tokens.description.clear();
                    tokens.post_tag.clear();
                    let fix = doc.fix();
                    cx.report(span, format!("Default values are not permitted on @{tag}."))
                        .fix(fix);
                }
            }
        });
    }
}
