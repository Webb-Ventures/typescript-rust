//! `jsdoc/require-hyphen-before-param-description`: a `-` separates a `@param` name from
//! its description (or, with `"never"`, doesn't), optionally for other tags too.

use super::prelude::*;
use crate::jsdoc::{Doc, Finder};
use crate::jsdoc_util::{each_block, preferred_name, preferred_tags, tag_description};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-hyphen-before-param-description",
        "Requires a hyphen before the `@param` description (and optionally before `@property` descriptions).",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<RequireHyphen>,
};

#[derive(Debug, Default)]
struct RequireHyphen {
    always: bool,
    /// `tags`: tag name (or `*`) and whether it wants the hyphen, in option order.
    tags: Vec<(String, bool)>,
}

impl FromOptions for RequireHyphen {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let main = opts.first().and_then(Value::as_str);
        let always = main.is_none_or(|m| m == "always");
        let tags = opts
            .get(1)
            .and_then(|o| o.get("tags"))
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().is_none_or(|s| s == "always")))
                    .collect()
            })
            .unwrap_or_default();
        Ok(RequireHyphen { always, tags })
    }
}

/// `^\s*-`, and whether the hyphen ends the line (`^\s*-\n`).
fn leading_hyphen(desc: &str) -> (bool, bool) {
    match desc.trim_start().strip_prefix('-') {
        Some(rest) => (true, rest.starts_with('\n')),
        None => (false, false),
    }
}

fn check(
    cx: &mut Ctx<'_, '_>,
    finder: &Finder<'_>,
    doc: &mut Doc,
    i: usize,
    name: &str,
    always: bool,
) {
    let desc = tag_description(&doc.block, i);
    if desc.trim().is_empty() {
        return;
    }
    let (hyphen, hyphen_newline) = leading_hyphen(&desc);
    let src = doc.block.tags[i].source.clone();
    let lines = src
        .iter()
        .take_while(|&&id| doc.block.lines[id].tokens.description.is_empty())
        .count() as i64;
    let line = doc.block.tag_line(i) + lines;
    if always && !hyphen_newline {
        if hyphen {
            return;
        }
        for &id in &src {
            let t = &mut doc.block.lines[id].tokens;
            if !t.description.is_empty() {
                let rest = t.description.trim_start();
                let lead = &t.description[..t.description.len() - rest.len()];
                t.description = format!("{lead}- {rest}");
                break;
            }
            if !t.name.is_empty() {
                return;
            }
        }
        let fix = doc.fix();
        let span = doc.line_span(finder, line);
        cx.report(
            span,
            format!("There must be a hyphen before @{name} description."),
        )
        .fix(fix);
    } else if hyphen {
        let message = if always {
            format!("There must be no hyphen followed by newline after the @{name} name.")
        } else {
            format!("There must be no hyphen before @{name} description.")
        };
        for &id in &src {
            let t = &mut doc.block.lines[id].tokens;
            if !t.description.is_empty() {
                let d = t.description.trim_start();
                t.description = d.strip_prefix('-').map_or(d, str::trim_start).to_owned();
                if hyphen_newline {
                    t.post_name.clear();
                }
                break;
            }
        }
        let fix = doc.fix();
        let span = doc.line_span(finder, line);
        cx.report(span, message).fix(fix);
    }
}

fn check_all(cx: &mut Ctx<'_, '_>, finder: &Finder<'_>, doc: &mut Doc, tag: &str, always: bool) {
    let Some((name, ids)) = preferred_tags(cx, finder, doc, tag) else {
        return;
    };
    for i in ids {
        check(cx, finder, doc, i, &name, always);
    }
}

impl Rule for RequireHyphen {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, finder, doc| {
            check_all(cx, finder, doc, "param", self.always);
            for (tag, always) in &self.tags {
                if tag != "*" {
                    check_all(cx, finder, doc, tag, *always);
                    continue;
                }
                let param = preferred_name(finder.settings, "param");
                let names: Vec<String> = doc.block.tags.iter().map(|t| t.tag.clone()).collect();
                for t in names {
                    if param.as_deref() == Some(t.as_str())
                        || self.tags.iter().any(|(k, _)| k != "*" && *k == t)
                    {
                        continue;
                    }
                    check_all(cx, finder, doc, &t, *always);
                }
            }
        });
    }
}
