//! `jsdoc/tag-lines`: empty lines between tags (`always` / `never` / `any`, with per-tag
//! overrides), after the last tag (`endLines`), after the block description
//! (`startLines`, `startLinesWithNoTags`) and inside it (`maxBlockLines`).

use super::prelude::*;
use crate::jsdoc::{Block, Doc, Finder, Tokens};
use crate::jsdoc_util::{add_lines, description, each_block, remove_tag, set_block_description};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/tag-lines",
        "Enforces lines (or no lines) before, after, or between tags.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<TagLines>,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lines {
    Always,
    Any,
    Never,
}

impl Lines {
    fn parse(v: Option<&Value>) -> Option<Lines> {
        match v?.as_str()? {
            "always" => Some(Lines::Always),
            "any" => Some(Lines::Any),
            "never" => Some(Lines::Never),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
struct TagOpts {
    lines: Option<Lines>,
    count: Option<i64>,
}

#[derive(Debug)]
struct TagLines {
    mode: Lines,
    apply_to_end_tag: bool,
    count: i64,
    end_lines: Option<i64>,
    max_block_lines: Option<i64>,
    start_lines: Option<i64>,
    start_lines_no_tags: Option<i64>,
    /// Sorted by tag name.
    tags: Vec<(String, TagOpts)>,
}

impl FromOptions for TagLines {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mode = Lines::parse(opts.first()).unwrap_or(Lines::Never);
        let o = opts.get(1).and_then(Value::as_object);
        let get = |k: &str| o.and_then(|o| o.get(k));
        // A key that is present but `null` turns the check off; a missing one takes the default.
        let num = |k: &str, default: Option<i64>| match get(k) {
            None => default,
            Some(v) => v.as_i64(),
        };
        let mut tags: Vec<(String, TagOpts)> = get("tags")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| {
                        let t = TagOpts {
                            lines: Lines::parse(v.get("lines")),
                            count: v.get("count").and_then(Value::as_i64),
                        };
                        (k.clone(), t)
                    })
                    .collect()
            })
            .unwrap_or_default();
        tags.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(TagLines {
            mode,
            apply_to_end_tag: get("applyToEndTag")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            count: get("count").and_then(Value::as_i64).unwrap_or(1),
            end_lines: num("endLines", Some(0)),
            max_block_lines: num("maxBlockLines", None),
            start_lines: num("startLines", Some(0)),
            start_lines_no_tags: get("startLinesWithNoTags").and_then(Value::as_i64),
            tags,
        })
    }
}

fn plural(n: i64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn is_empty_line(t: &Tokens) -> bool {
    t.tag.is_empty() && t.name.is_empty() && t.typ.is_empty() && t.description.is_empty()
}

/// The description lines `set_block_description` replaces, with their post-delimiters.
fn description_lines(b: &Block) -> (Vec<String>, Vec<String>) {
    let mut descs = Vec::new();
    let mut posts = Vec::new();
    for &id in &b.source {
        let t = &b.lines[id].tokens;
        if t.delimiter == "/**" {
            continue;
        }
        if !t.tag.is_empty() || !t.end.is_empty() {
            break;
        }
        descs.push(t.description.clone());
        posts.push(t.post_delimiter.clone());
    }
    (descs, posts)
}

fn rebuild(b: &mut Block, lines: Vec<(String, String)>) {
    set_block_description(b, |info| {
        lines
            .into_iter()
            .map(|(description, post_delimiter)| Tokens {
                start: info.start.clone(),
                delimiter: info.delimiter.clone(),
                post_delimiter,
                description,
                ..Tokens::default()
            })
            .collect()
    });
}

impl TagLines {
    fn tag_opts(&self, token: &str) -> Option<&TagOpts> {
        let name = token.strip_prefix('@').unwrap_or(token);
        self.tags
            .binary_search_by(|(k, _)| k.as_str().cmp(name))
            .ok()
            .map(|i| &self.tags[i].1)
    }

    fn tag_lines(&self, token: &str) -> Option<Lines> {
        self.tag_opts(token).and_then(|t| t.lines)
    }

    /// Lines after a tag: `never` lines between tags and the `endLines` count.
    fn check_after_tags(&self, cx: &mut Ctx<'_, '_>, finder: &Finder<'_>, doc: &mut Doc) {
        let total = doc.block.tags.len();
        for tag_idx in 0..total {
            let mut last_tag = String::new();
            let mut last_empty: Option<usize> = None;
            let mut report_index: Option<usize> = None;
            let mut empty_count: i64 = 0;
            let source = doc.block.tags[tag_idx].source.clone();
            for (idx, &id) in source.iter().enumerate() {
                let t = &doc.block.lines[id].tokens;
                if !t.description.is_empty() {
                    report_index = None;
                }
                if !last_tag.is_empty()
                    && matches!(self.tag_lines(&last_tag), Some(Lines::Always | Lines::Any))
                {
                    continue;
                }
                let empty = is_empty_line(t);
                if empty
                    && t.end.is_empty()
                    && (self.mode == Lines::Never
                        || (!last_tag.is_empty()
                            && self.tag_lines(&last_tag) == Some(Lines::Never)))
                {
                    report_index = Some(idx);
                    continue;
                }
                if t.end.is_empty() {
                    if empty {
                        empty_count += 1;
                    } else {
                        empty_count = 0;
                    }
                    last_empty = empty.then_some(idx);
                }
                last_tag = t.tag.clone();
            }
            if let (Some(end_lines), Some(last)) = (self.end_lines, last_empty)
                && tag_idx == total - 1
            {
                let diff = end_lines - empty_count;
                let number = doc.block.lines[source[last]].number;
                let msg = format!("Expected {end_lines} trailing lines");
                if diff < 0 {
                    let offset = (last as i64 + diff + 1).max(0) as usize;
                    remove_tag(&mut doc.block, tag_idx, offset);
                    let fix = doc.fix();
                    let span = doc.line_span(finder, number + diff + 1);
                    cx.report(span, msg).fix(fix);
                } else if diff > 0 {
                    let indent = doc.indent.clone();
                    add_lines(&mut doc.block, &indent, tag_idx, last, diff as usize);
                    let fix = doc.fix();
                    let span = doc.line_span(finder, number);
                    cx.report(span, msg).fix(fix);
                }
                return;
            }
            if let Some(at) = report_index {
                let number = doc.block.lines[source[0]].number;
                remove_tag(&mut doc.block, tag_idx, at);
                let fix = doc.fix();
                let span = doc.line_span(finder, number + 1);
                cx.report(span, "Expected no lines between tags").fix(fix);
                return;
            }
        }
    }

    /// Too few lines after a tag under `always` (or a tag's own `always`).
    fn check_between_tags(&self, cx: &mut Ctx<'_, '_>, finder: &Finder<'_>, doc: &mut Doc) {
        let mut total = doc.block.tags.len();
        if !self.apply_to_end_tag {
            total = total.saturating_sub(1);
        }
        for tag_idx in 0..total {
            let source = doc.block.tags[tag_idx].source.clone();
            let mut empties: Vec<(usize, i64)> = Vec::new();
            let mut current = String::new();
            let mut desc_idx = 0;
            for (idx, &id) in source.iter().enumerate() {
                let line = &doc.block.lines[id];
                let t = &line.tokens;
                if !t.description.is_empty() {
                    empties.clear();
                    desc_idx = idx;
                }
                if !t.tag.is_empty() {
                    current = t.tag.clone();
                }
                if is_empty_line(t) && t.end.is_empty() {
                    empties.push((idx, line.number));
                }
            }
            let opts = if current.is_empty() {
                None
            } else {
                self.tag_opts(&current)
            };
            let lines = opts.and_then(|o| o.lines);
            let found = empties.len() as i64;
            let default_always = self.mode == Lines::Always
                && !matches!(lines, Some(Lines::Never | Lines::Any))
                && found < self.count;
            let mut fix_count = self.count;
            let mut override_always = false;
            if !default_always {
                fix_count = opts.and_then(|o| o.count).unwrap_or(self.count);
                override_always = lines == Some(Lines::Always) && found < fix_count;
            }
            if !(default_always || override_always) {
                continue;
            }
            let at = match empties.last() {
                Some(&(idx, _)) if idx != 0 => idx,
                _ => desc_idx + 1,
            };
            let number = match empties.last() {
                Some(&(_, n)) if n != 0 => n,
                _ => source
                    .get(desc_idx)
                    .map_or(0, |&id| doc.block.lines[id].number),
            };
            let indent = doc.indent.clone();
            add_lines(
                &mut doc.block,
                &indent,
                tag_idx,
                at,
                (fix_count - found).max(0) as usize,
            );
            let fix = doc.fix();
            let span = doc.line_span(finder, number);
            let msg = format!(
                "Expected {fix_count} line{} between tags but found {found}",
                plural(fix_count)
            );
            cx.report(span, msg).fix(fix);
            return;
        }
    }

    /// `maxBlockLines`; true when it reported (which ends the check).
    fn check_max_block_lines(
        &self,
        cx: &mut Ctx<'_, '_>,
        finder: &Finder<'_>,
        doc: &mut Doc,
    ) -> bool {
        let Some(max) = self.max_block_lines else {
            return false;
        };
        if let Some(start) = self.start_lines
            && max < start
        {
            cx.report(
                doc.comment,
                "If set to a number, `maxBlockLines` must be greater than or equal to `startLines`.",
            );
            return true;
        }
        let text = description(&doc.block).text();
        let need = (max + 2).max(0) as usize;
        let Some((start, len)) = newline_run(&text, need) else {
            return false;
        };
        let line = text[..start].matches('\n').count();
        let (descs, posts) = description_lines(&doc.block);
        let skip = (line as i64 + len as i64 - 1 - max).max(0) as usize;
        let keep = |v: Vec<String>| -> Vec<String> {
            let head = v.iter().take(line).cloned();
            let tail = v.iter().skip(skip).cloned();
            head.chain(tail).collect()
        };
        let lines = keep(descs).into_iter().zip(keep(posts)).collect();
        rebuild(&mut doc.block, lines);
        let fix = doc.fix();
        let span = doc.line_span(finder, line as i64);
        let msg = format!(
            "Expected a maximum of {max} line{} within block description",
            plural(max)
        );
        cx.report(span, msg).fix(fix);
        true
    }

    fn check_start_lines(&self, cx: &mut Ctx<'_, '_>, finder: &Finder<'_>, doc: &mut Doc) {
        if self.start_lines.is_none() && self.start_lines_no_tags.is_none() {
            return;
        }
        let no_tags = doc.block.tags.is_empty();
        if no_tags && self.start_lines_no_tags.is_none() {
            return;
        }
        let d = description(&doc.block);
        let text = d.text();
        if !text.chars().any(|c| !c.is_whitespace()) {
            return;
        }
        let starting = if no_tags {
            self.start_lines_no_tags
        } else {
            self.start_lines
        };
        let shown = starting.map_or_else(|| "null".to_owned(), |n| n.to_string());
        let starting = starting.unwrap_or(0);
        let trailing = (text.len() - text.trim_end_matches('\n').len()) as i64;
        let diff = trailing - starting;
        let last = d.last_line as i64;
        let (descs, posts) = description_lines(&doc.block);
        if diff > 0 {
            let keep = descs.len().saturating_sub(diff as usize);
            let lines = descs.into_iter().zip(posts).take(keep).collect();
            rebuild(&mut doc.block, lines);
            let fix = doc.fix();
            let span = doc.line_span(finder, last - diff);
            let msg = format!(
                "Expected only {shown} line{} after block description",
                plural(starting)
            );
            cx.report(span, msg).fix(fix);
        } else if diff < 0 {
            let mut lines: Vec<(String, String)> = Vec::new();
            for k in 0..descs.len() + (-diff) as usize {
                let desc = descs.get(k).cloned().unwrap_or_default();
                let post = if desc.trim().is_empty() {
                    String::new()
                } else {
                    posts.get(k).cloned().unwrap_or_default()
                };
                lines.push((desc, post));
            }
            rebuild(&mut doc.block, lines);
            let fix = doc.fix();
            let span = doc.line_span(finder, last);
            cx.report(
                span,
                format!("Expected {shown} lines after block description"),
            )
            .fix(fix);
        }
    }
}

/// The first run of at least `need` line breaks in `s`: its byte offset and length.
fn newline_run(s: &str, need: usize) -> Option<(usize, usize)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\n' {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i] == b'\n' {
            i += 1;
        }
        if i - start >= need {
            return Some((start, i - start));
        }
    }
    None
}

impl Rule for TagLines {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, finder, doc| {
            self.check_after_tags(cx, finder, doc);
            self.check_between_tags(cx, finder, doc);
            if self.check_max_block_lines(cx, finder, doc) {
                return;
            }
            self.check_start_lines(cx, finder, doc);
        });
    }
}
