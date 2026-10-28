//! `jsdoc/empty-tags`: tags that take no content (`@abstract`, `@async`, `@private`, ...)
//! have none.

use super::prelude::*;
use crate::jsdoc::Tokens;
use crate::jsdoc_util::{Checks, each_block_checking};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/empty-tags",
        "Checks tags that are expected to be empty (e.g., `@abstract` or `@async`), reporting if they have content",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<EmptyTags>,
};

const ALWAYS: &[&str] = &[
    "abstract",
    "async",
    "generator",
    "global",
    "hideconstructor",
    "ignore",
    "inheritDoc",
    "inner",
    "instance",
    "internal",
    "overload",
    "override",
    "readonly",
];
const NOT_CLOSURE: &[&str] = &[
    "inheritdoc",
    "package",
    "private",
    "protected",
    "public",
    "static",
];

#[derive(Debug, Default)]
struct EmptyTags {
    tags: Vec<String>,
}

impl FromOptions for EmptyTags {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let tags = opts
            .first()
            .and_then(|o| o.get("tags"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        Ok(EmptyTags { tags })
    }
}

fn is_stars(s: &str) -> bool {
    let t = s.trim_start();
    !t.is_empty() && t.bytes().all(|b| b == b'*')
}

impl Rule for EmptyTags {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let checks = Checks {
            private: true,
            internal: true,
        };
        each_block_checking(cx, checks, |cx, finder, doc| {
            let closure = finder.settings.mode == "closure";
            let empty = |t: &str| {
                ALWAYS.contains(&t)
                    || self.tags.iter().any(|x| x == t)
                    || if closure {
                        t == "interface"
                    } else {
                        NOT_CLOSURE.contains(&t)
                    }
            };
            let found: Vec<usize> = (0..doc.block.tags.len())
                .filter(|&i| empty(&doc.block.tags[i].tag))
                .collect();
            let last = found.last().copied();
            for i in found {
                let t = &doc.block.tags[i];
                let content = [&t.name, &t.description, &t.typ]
                    .into_iter()
                    .find(|s| !s.is_empty())
                    .map_or("", |s| s.as_str());
                if content.trim().is_empty() || (Some(i) == last && is_stars(content)) {
                    continue;
                }
                let tag = t.tag.clone();
                let span = doc.line_span(finder, doc.block.tag_line(i));
                if let Some(&id) = t.source.first() {
                    let old = &doc.block.lines[id].tokens;
                    let post_name = if old.end.is_empty() {
                        String::new()
                    } else {
                        old.post_name.clone()
                    };
                    doc.block.lines[id].tokens = Tokens {
                        start: old.start.clone(),
                        delimiter: old.delimiter.clone(),
                        post_delimiter: " ".to_owned(),
                        tag: format!("@{tag}"),
                        post_name,
                        end: old.end.clone(),
                        ..Tokens::default()
                    };
                }
                let fix = doc.fix();
                cx.report(span, format!("@{tag} should be empty.")).fix(fix);
            }
        });
    }
}
