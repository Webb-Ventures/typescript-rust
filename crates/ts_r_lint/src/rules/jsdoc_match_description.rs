//! `jsdoc/match-description`: descriptions must match a regular expression.
//!
//! Checked, in the contexts in force (functions by default): the block description when it
//! is non-empty (reported on the comment's second line); each `@description`, `@summary`,
//! `@file` and `@classdesc` (name and description) not named in `tags`; and each tag named
//! in `tags` with a truthy value, using its description alone (leading hyphens and spaces
//! dropped) for tags that carry a name (`@param`, `@returns`, ...) and its name and
//! description otherwise. A tag's pattern is its `tags` string (or `match`), falling back
//! to `matchDescription` and then to "starts with a capital, digit, backtick or underscore
//! and ends with `.?!`, a backtick or an emoji". `mainDescription: false` turns off every
//! check except those of tags named in `tags`. With `nonemptyTags` (the default) an empty
//! `@copyright`, `@example`, `@see` or `@todo` not named in `tags` is reported.

use super::prelude::*;
use crate::jsdoc::{Doc, Finder};
use crate::jsdoc_util::{
    ContextOpts, description, js_trim, preferred_name, regex_string_matches, tag_description,
};
use serde_json::Map;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/match-description",
        "Enforces a regular expression pattern on descriptions.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<MatchDescription>,
};

const DEFAULT_PATTERN: &str = "^\n?([A-Z`\\d_][\\s\\S]*[.?!`\\p{RGI_Emoji}]\\s*)?$";

/// Tags whose first word after the type is a name.
const WITH_NAMES: &[&str] = &[
    "arg", "argument", "param", "prop", "property", "return", "returns", "template",
];

#[derive(Debug)]
struct MatchDescription {
    ctx: ContextOpts,
}

impl FromOptions for MatchDescription {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(MatchDescription {
            ctx: ContextOpts::parse(opts)?,
        })
    }
}

/// A truthy JavaScript value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

impl MatchDescription {
    fn opt(&self, key: &str) -> Option<&Value> {
        self.ctx.opts.get(key)
    }

    fn tags(&self) -> Option<&Map<String, Value>> {
        self.opt("tags").and_then(Value::as_object)
    }

    fn tag_opt(&self, tag: &str) -> Option<&Value> {
        self.tags().and_then(|t| t.get(tag))
    }

    /// `validateDescription`: reports `desc` when it fails its pattern; `tag` is the index
    /// of the tag it comes from.
    fn validate(
        &self,
        cx: &mut Ctx<'_, '_>,
        f: &Finder<'_>,
        doc: &Doc,
        desc: &str,
        tag: Option<usize>,
    ) {
        let main = self.opt("mainDescription");
        let mut message = self.opt("message").and_then(Value::as_str);
        let mut value = main;
        if let Some(Value::Object(o)) = main {
            value = o.get("match");
            message = o.get("message").and_then(Value::as_str);
        }
        let tag_name = tag.map(|i| doc.block.tags[i].tag.as_str());
        let listed = tag_name.is_some_and(|t| self.tags().is_some_and(|m| m.contains_key(t)));
        if matches!(value, Some(Value::Bool(false))) && !listed {
            return;
        }
        if let Some(t) = tag_name {
            match self.tag_opt(t) {
                Some(Value::Object(o)) => {
                    value = o.get("match");
                    message = o.get("message").and_then(Value::as_str);
                }
                v => value = v,
            }
        }
        let pattern = match value {
            Some(Value::String(s)) => s.as_str(),
            _ => match self.opt("matchDescription").and_then(Value::as_str) {
                Some(s) if !s.is_empty() => s,
                _ => DEFAULT_PATTERN,
            },
        };
        if regex_string_matches(pattern, desc) {
            return;
        }
        let rel = match tag {
            Some(i) => doc.block.tag_line(i),
            None => 1,
        };
        let (span, loc) = doc.report_at(f, rel, None);
        let msg = message.unwrap_or("JSDoc description does not satisfy the regex pattern.");
        cx.report(span, msg.to_owned()).loc(loc);
    }

    fn check(&self, cx: &mut Ctx<'_, '_>, f: &Finder<'_>, doc: &Doc) {
        let b = &doc.block;
        let main = description(b).text();
        if !main.is_empty() {
            self.validate(cx, f, doc, &main, None);
        }
        let name_and_description =
            |i: usize| js_trim(&format!("{} {}", b.tags[i].name, tag_description(b, i))).to_owned();
        for tag in ["description", "summary", "file", "classdesc"] {
            let Some(target) = preferred_name(f.settings, tag) else {
                continue;
            };
            if truthy(self.tag_opt(&target)) {
                continue;
            }
            for i in (0..b.tags.len()).filter(|&i| b.tags[i].tag == target) {
                self.validate(cx, f, doc, &name_and_description(i), Some(i));
            }
        }
        if !matches!(self.opt("nonemptyTags"), Some(Value::Bool(false))) {
            for tag in ["copyright", "example", "see", "todo"] {
                let Some(target) = doc.preferred(cx, f, tag) else {
                    continue;
                };
                if truthy(self.tag_opt(&target)) {
                    continue;
                }
                for i in (0..b.tags.len()).filter(|&i| b.tags[i].tag == target) {
                    if name_and_description(i).is_empty() {
                        let (span, loc) = doc.report_at(f, b.tag_line(i), None);
                        cx.report(span, "JSDoc description must not be empty.")
                            .loc(loc);
                    }
                }
            }
        }
        if self.tags().is_none_or(Map::is_empty) {
            return;
        }
        let listed: Vec<usize> = (0..b.tags.len())
            .filter(|&i| truthy(self.tag_opt(&b.tags[i].tag)))
            .collect();
        let (named, unnamed): (Vec<usize>, Vec<usize>) = listed
            .into_iter()
            .partition(|&i| WITH_NAMES.contains(&b.tags[i].tag.as_str()));
        for i in named {
            let d = tag_description(b, i);
            let d = d.trim_start_matches(['-', ' ']);
            self.validate(cx, f, doc, js_trim(d), Some(i));
        }
        for i in unnamed {
            self.validate(cx, f, doc, &name_and_description(i), Some(i));
        }
    }
}

impl Rule for MatchDescription {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        self.ctx.each_doc(cx, |cx, f, doc| self.check(cx, f, doc));
    }
}
