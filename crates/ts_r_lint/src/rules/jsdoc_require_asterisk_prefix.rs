//! `jsdoc/require-asterisk-prefix`: every line of a JSDoc block starts with `*` (`always`,
//! the default) or doesn't (`never`), per tag with the `tags` option (`always`, `never`,
//! `any` lists of tag names or `*description` for the block description).
//!
//! Lines are checked in order and the first offending one is reported, at the comment's own
//! column on that line (the plugin's `column: 0`), which may lie past the line's end. The
//! `/**` line and a bare `*/` line are not checked; a line belongs to the last tag above it.

use super::prelude::*;
use crate::jsdoc_util::each_block;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-asterisk-prefix",
        "Requires that each JSDoc line starts with an `*`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<RequireAsteriskPrefix>,
};

#[derive(Debug, Default)]
struct RequireAsteriskPrefix {
    always: bool,
    never: bool,
    tags_always: Vec<String>,
    tags_never: Vec<String>,
    tags_any: Vec<String>,
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

impl FromOptions for RequireAsteriskPrefix {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mode = match opts.first() {
            None => "always",
            Some(Value::String(s)) if matches!(s.as_str(), "always" | "never" | "any") => s,
            Some(_) => {
                return Err(
                    "jsdoc/require-asterisk-prefix: expected \"always\", \"never\" or \"any\""
                        .into(),
                );
            }
        };
        let tags = opts.get(1).and_then(|o| o.get("tags"));
        Ok(RequireAsteriskPrefix {
            always: mode == "always",
            never: mode == "never",
            tags_always: strings(tags.and_then(|t| t.get("always"))),
            tags_never: strings(tags.and_then(|t| t.get("never"))),
            tags_any: strings(tags.and_then(|t| t.get("any"))),
        })
    }
}

fn has(list: &[String], name: &str) -> bool {
    list.iter().any(|s| s == name)
}

impl Rule for RequireAsteriskPrefix {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, finder, doc| {
            let mut current: Option<String> = None;
            for k in 0..doc.block.source.len() {
                let id = doc.block.source[k];
                let line = &doc.block.lines[id];
                let t = &line.tokens;
                if let Some(tag) = t.tag.strip_prefix('@') {
                    current = Some(tag.to_owned());
                } else if !t.tag.is_empty() {
                    current = Some(t.tag.clone());
                }
                if !t.end.is_empty() && t.tag.is_empty() {
                    continue;
                }
                let check = current.as_deref().unwrap_or("*description");
                if has(&self.tags_any, check) {
                    continue;
                }
                let never = !t.delimiter.is_empty()
                    && t.delimiter != "/**"
                    && (self.never && !has(&self.tags_always, check)
                        || has(&self.tags_never, check));
                let always = !never
                    && t.delimiter.is_empty()
                    && (self.always && !has(&self.tags_never, check)
                        || has(&self.tags_always, check));
                if !never && !always {
                    continue;
                }
                let number = line.number;
                let has_text = !t.tag.is_empty() || !t.description.is_empty();
                let tokens = &mut doc.block.lines[id].tokens;
                let msg = if never {
                    tokens.delimiter.clear();
                    tokens.post_delimiter.clear();
                    "Expected JSDoc line to have no prefix."
                } else {
                    if tokens.start.is_empty() {
                        tokens.start = format!("{} ", doc.indent);
                    }
                    tokens.delimiter = "*".to_owned();
                    tokens.post_delimiter = if has_text { " " } else { "" }.to_owned();
                    "Expected JSDoc line to have the prefix."
                };
                let (span, loc) = doc.report_at(finder, number, Some(0));
                let fix = doc.fix();
                cx.report(span, msg).loc(loc).fix(fix);
                return;
            }
        });
    }
}
