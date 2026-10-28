//! `jsdoc/no-multi-asterisks`: no `**` starting a middle line or before the closing `*/`.

use super::prelude::*;
use crate::jsdoc_util::each_block;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/no-multi-asterisks",
        "Prevents use of multiple asterisks at the beginning of lines.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NoMultiAsterisks>,
};

#[derive(Debug)]
struct NoMultiAsterisks {
    allow_whitespace: bool,
    at_end: bool,
    at_middle: bool,
}

impl FromOptions for NoMultiAsterisks {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = opts.first();
        let flag = |k: &str, d: bool| {
            o.and_then(|o| o.get(k))
                .and_then(Value::as_bool)
                .unwrap_or(d)
        };
        Ok(NoMultiAsterisks {
            allow_whitespace: flag("allowWhitespace", false),
            at_end: flag("preventAtEnd", true),
            at_middle: flag("preventAtMiddleLines", true),
        })
    }
}

/// Length of the leading run of spaces, tabs and lone `*`s (a `*` followed by another `*`
/// ends it).
fn lone_star_prefix(s: &str) -> usize {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b' ' | b'\t' => i += 1,
            b'*' if b.get(i + 1) != Some(&b'*') => i += 1,
            _ => break,
        }
    }
    i
}

/// Where the closing run of asterisks of `s` starts and the part of it before the final `*`:
/// the trailing run of `*` (or, with `spaces`, of `*`, space and tab) ending in `*`. A single
/// line block needs the run to start with a `*` and hold at least two.
fn end_run(s: &str, spaces: bool, single: bool) -> Option<(usize, &str)> {
    let b = s.as_bytes();
    if b.last() != Some(&b'*') {
        return None;
    }
    let in_run = |c: u8| c == b'*' || (spaces && (c == b' ' || c == b'\t'));
    let mut p = b.len() - 1;
    while p > 0 && in_run(b[p - 1]) {
        p -= 1;
    }
    if !single {
        return Some((p, &s[p..b.len() - 1]));
    }
    while p < b.len() && b[p] != b'*' {
        p += 1;
    }
    (p + 1 < b.len()).then(|| (p, &s[p + 1..b.len() - 1]))
}

impl Rule for NoMultiAsterisks {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, finder, doc| {
            for k in 0..doc.block.source.len() {
                let id = doc.block.source[k];
                let number = doc.block.lines[id].number;
                let t = &doc.block.lines[id].tokens;
                let middle = if self.allow_whitespace {
                    format!("{}{}", t.post_delimiter, t.description).starts_with('*')
                } else {
                    lone_star_prefix(&t.description) > 0
                };
                if self.at_middle
                    && t.end.is_empty()
                    && t.tag.is_empty()
                    && t.typ.is_empty()
                    && t.name.is_empty()
                    && middle
                {
                    let d = &t.description;
                    let cut = if self.allow_whitespace {
                        d.len() - d.trim_start_matches('*').len()
                    } else {
                        lone_star_prefix(d)
                    };
                    let rest = d[cut..].to_owned();
                    doc.block.lines[id].tokens.description = rest;
                    let fix = doc.fix();
                    let span = doc.line_span(finder, number);
                    cx.report(span, "Should be no multiple asterisks on middle lines.")
                        .fix(fix);
                    return;
                }
                if !self.at_end || t.end.is_empty() {
                    continue;
                }
                let single = t.delimiter == "/**";
                let delim = if single { "*" } else { t.delimiter.as_str() };
                let s = if self.allow_whitespace {
                    format!("{}{}{delim}", t.post_delimiter, t.description)
                } else {
                    format!("{}{delim}", t.description)
                };
                let Some((_, group)) = end_run(&s, !self.allow_whitespace, single) else {
                    continue;
                };
                if !single && !group.is_empty() && group.trim().is_empty() {
                    continue;
                }
                // The fix strips the run from the description (plus the delimiter), never
                // from the post-delimiter.
                let base = format!("{}{delim}", t.description);
                let cut =
                    end_run(&base, !self.allow_whitespace, single).map_or(base.len(), |(q, _)| q);
                let description = base[..cut].to_owned();
                let tokens = &mut doc.block.lines[id].tokens;
                if !single {
                    tokens.delimiter.clear();
                }
                tokens.description = description;
                let fix = doc.fix();
                let span = doc.line_span(finder, number);
                cx.report(span, "Should be no multiple asterisks on end lines.")
                    .fix(fix);
                return;
            }
        });
    }
}
