//! `jsdoc/multiline-blocks`: when a block may (or must) span several lines, and no text on
//! the `/**` or `*/` lines of a multi-line block.

use super::prelude::*;
use crate::jsdoc::{Block, Doc, Tokens};
use crate::jsdoc_util::each_block;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/multiline-blocks",
        "Controls how and whether JSDoc blocks can be expressed as single or multiple line blocks.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<MultilineBlocks>,
};

#[derive(Debug)]
struct MultilineBlocks {
    allow_multiple_tags: bool,
    minimum_length: f64,
    multiline_tags: Vec<String>,
    no_final_line_text: bool,
    no_multiline_blocks: bool,
    no_single_line_blocks: bool,
    no_zero_line_text: bool,
    single_line_under: Option<f64>,
    single_line_tags: Vec<String>,
}

fn strings(o: Option<&Value>, key: &str, default: &[&str]) -> Vec<String> {
    match o.and_then(|o| o.get(key)).and_then(Value::as_array) {
        Some(a) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        None => default.iter().map(|s| (*s).to_owned()).collect(),
    }
}

impl FromOptions for MultilineBlocks {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = opts.first();
        let flag = |k: &str, d: bool| {
            o.and_then(|o| o.get(k))
                .and_then(Value::as_bool)
                .unwrap_or(d)
        };
        let num = |k: &str| o.and_then(|o| o.get(k)).and_then(Value::as_f64);
        Ok(MultilineBlocks {
            allow_multiple_tags: flag("allowMultipleTags", true),
            minimum_length: num("minimumLengthForMultiline").unwrap_or(f64::INFINITY),
            multiline_tags: strings(o, "multilineTags", &["*"]),
            no_final_line_text: flag("noFinalLineText", true),
            no_multiline_blocks: flag("noMultilineBlocks", false),
            no_single_line_blocks: flag("noSingleLineBlocks", false),
            no_zero_line_text: flag("noZeroLineText", true),
            single_line_under: num("requireSingleLineUnderCount").filter(|n| *n != 0.0),
            single_line_tags: strings(o, "singleLineTags", &["lends", "type"]),
        })
    }
}

/// Length as JavaScript counts it.
fn len(s: &str) -> f64 {
    s.encode_utf16().count() as f64
}

/// The block description as `comment-parser` joins it: the trimmed, non-empty description
/// lines before the first tag, joined by spaces.
fn compact_description(b: &Block) -> String {
    let first_tag = b.tags.first().and_then(|t| t.source.first().copied());
    b.source
        .iter()
        .take_while(|&&id| Some(id) != first_tag)
        .map(|&id| b.lines[id].tokens.description.trim())
        .filter(|d| !d.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Inserts a line at `at` in the block's source.
fn add_line(b: &mut Block, at: usize, tokens: Tokens) {
    let id = b.new_line(0, tokens);
    let at = at.min(b.source.len());
    b.source.insert(at, id);
}

/// Replaces the block with a single line.
fn single_line(b: &mut Block, tokens: Tokens) {
    let id = b.new_line(0, tokens);
    b.source = vec![id];
}

impl MultilineBlocks {
    fn invalid_single_line(&self, tag: &str) -> bool {
        self.no_single_line_blocks
            && (tag.is_empty()
                || (!self.single_line_tags.iter().any(|t| t == tag)
                    && !self.single_line_tags.iter().any(|t| t == "*")))
    }

    /// `checkForShortDescriptions`: a tagless block with one short description line.
    fn short_description(&self, b: &mut Block) -> bool {
        let Some(limit) = self.single_line_under else {
            return false;
        };
        if !b.tags.is_empty() {
            return false;
        }
        let mut last = 0;
        let mut under = false;
        let mut count = 0;
        for (idx, &id) in b.source.iter().enumerate() {
            let t = &b.lines[id].tokens;
            if !t.description.is_empty() {
                last = idx;
                if len(&t.start) + len(&t.delimiter) + len(&t.post_delimiter) + len(&t.description)
                    < limit
                {
                    under = true;
                }
                count += 1;
            }
        }
        if !under || count != 1 {
            return false;
        }
        let desc = b.lines[b.source[last]]
            .tokens
            .description
            .trim_end()
            .to_owned();
        let start = b.lines[b.source[0]].tokens.start.clone();
        single_line(
            b,
            Tokens {
                start,
                delimiter: "/**".to_owned(),
                post_delimiter: " ".to_owned(),
                description: format!("{desc} "),
                end: "*/".to_owned(),
                ..Tokens::default()
            },
        );
        true
    }

    /// `checkForShortTags`: a block with one short tag line and nothing else.
    fn short_tags(&self, b: &mut Block) -> bool {
        let Some(limit) = self.single_line_under else {
            return false;
        };
        if b.tags.is_empty() {
            return false;
        }
        let mut last = 0;
        let mut under = false;
        let mut other = false;
        let mut count = 0;
        for (idx, &id) in b.source.iter().enumerate() {
            let t = &b.lines[id].tokens;
            if !t.tag.is_empty() {
                last = idx;
                let total = [
                    &t.start,
                    &t.delimiter,
                    &t.post_delimiter,
                    &t.typ,
                    &t.post_type,
                    &t.name,
                    &t.post_name,
                    &t.tag,
                    &t.post_tag,
                    &t.description,
                ]
                .iter()
                .map(|s| len(s))
                .sum::<f64>();
                if total < limit {
                    under = true;
                }
                count += 1;
            } else if !t.description.is_empty() || !t.typ.is_empty() {
                other = true;
            }
        }
        if other || !under || count != 1 {
            return false;
        }
        let t = b.lines[b.source[last]].tokens.clone();
        let start = b.lines[b.source[0]].tokens.start.clone();
        single_line(
            b,
            Tokens {
                start,
                delimiter: "/**".to_owned(),
                post_delimiter: " ".to_owned(),
                tag: t.tag,
                post_tag: t.post_tag,
                typ: t.typ,
                post_type: t.post_type,
                name: t.name,
                post_name: t.post_name,
                description: format!("{} ", t.description.trim_end()),
                end: "*/".to_owned(),
                ..Tokens::default()
            },
        );
        true
    }

    /// The `/**` and `*/` line checks.
    fn line_checks(&self, cx: &mut Ctx<'_, '_>, doc: &mut Doc) {
        let b = &mut doc.block;
        let first = b.source[0];
        let t0 = b.lines[first].tokens.clone();
        if self.no_zero_line_text && (!t0.tag.is_empty() || !t0.description.is_empty()) {
            let (delimiter, start) = match b.source.get(1) {
                Some(&id) => {
                    let t = &b.lines[id].tokens;
                    (t.delimiter.clone(), t.start.clone())
                }
                None => (String::new(), String::new()),
            };
            b.lines[first].tokens = Tokens {
                delimiter: t0.delimiter.clone(),
                ..Tokens::default()
            };
            add_line(
                b,
                1,
                Tokens {
                    delimiter,
                    start,
                    ..t0
                },
            );
            let fix = doc.fix();
            cx.report(
                doc.comment,
                "Should have no text on the \"0th\" line (after the `/**`).",
            )
            .fix(fix);
            return;
        }
        let Some(&last) = b.source.last() else {
            return;
        };
        let tl = b.lines[last].tokens.clone();
        if self.no_final_line_text && !tl.description.trim().is_empty() {
            b.lines[last].tokens = Tokens {
                start: tl.start.clone(),
                end: tl.end.clone(),
                ..Tokens::default()
            };
            let at = b.source.len() - 1;
            add_line(
                b,
                at,
                Tokens {
                    description: tl.description.trim_end().to_owned(),
                    end: String::new(),
                    ..tl
                },
            );
            let fix = doc.fix();
            cx.report(
                doc.comment,
                "Should have no text on the final line (before the `*/`).",
            )
            .fix(fix);
        }
    }

    /// `makeMultiline`: a single line block as three lines.
    fn make_multiline(doc: &mut Doc) {
        let b = &mut doc.block;
        let first = b.source[0];
        let t = b.lines[first].tokens.clone();
        let (mut post_name, mut post_type, mut post_tag) =
            (t.post_name.clone(), t.post_type.clone(), t.post_tag.clone());
        if t.description.is_empty() {
            if !post_name.is_empty() {
                post_name.clear();
            } else if !post_type.is_empty() {
                post_type.clear();
            } else {
                post_tag.clear();
            }
        }
        b.lines[first].tokens = Tokens {
            delimiter: t.delimiter.clone(),
            ..Tokens::default()
        };
        let start = format!("{} ", doc.indent);
        add_line(
            b,
            1,
            Tokens {
                start: start.clone(),
                delimiter: "*".to_owned(),
                post_delimiter: t.post_delimiter,
                tag: t.tag,
                post_tag,
                typ: t.typ,
                post_type,
                name: t.name,
                post_name,
                description: t.description.trim_end().to_owned(),
                ..Tokens::default()
            },
        );
        add_line(
            b,
            2,
            Tokens {
                start,
                end: "*/".to_owned(),
                line_end: t.line_end,
                ..Tokens::default()
            },
        );
    }

    /// The single line every line of the block folds into.
    fn collapse(b: &mut Block) {
        let mut o = Tokens {
            delimiter: "/**".to_owned(),
            post_delimiter: " ".to_owned(),
            end: "*/".to_owned(),
            ..Tokens::default()
        };
        for &id in &b.source {
            let t = &b.lines[id].tokens;
            if !t.typ.is_empty() {
                o.typ = t.typ.clone();
            }
            if !t.tag.is_empty() && !t.typ.is_empty() && !t.name.is_empty() {
                o.post_type = t.post_type.clone();
            }
            o.name.push_str(&t.name);
            if !t.name.is_empty() && !t.description.is_empty() {
                o.post_name = t.post_name.clone();
            }
            o.description.push_str(&t.description);
            let shown = if o.description.is_empty() {
                &o.name
            } else {
                &o.description
            };
            if !shown.is_empty() && !shown.ends_with(' ') {
                o.description.push(' ');
            }
            o.line_end = t.line_end.clone();
            o.tag.push_str(&t.tag);
            if !t.tag.is_empty() {
                o.post_tag = if t.post_tag.is_empty() {
                    " ".to_owned()
                } else {
                    t.post_tag.clone()
                };
            }
        }
        single_line(b, o);
    }

    fn check(&self, cx: &mut Ctx<'_, '_>, doc: &mut Doc) {
        if doc.block.source.is_empty() {
            return;
        }
        if doc.block.source.len() == 1 {
            let t = &doc.block.lines[doc.block.source[0]].tokens;
            if !self.invalid_single_line(t.tag.strip_prefix('@').unwrap_or(&t.tag)) {
                return;
            }
            Self::make_multiline(doc);
            let fix = doc.fix();
            cx.report(
                doc.comment,
                "Single line blocks are not permitted by your configuration.",
            )
            .fix(fix);
            return;
        }
        if self.short_description(&mut doc.block) || self.short_tags(&mut doc.block) {
            let fix = doc.fix();
            cx.report(doc.comment, "Description is too short to be multi-line.")
                .fix(fix);
            return;
        }
        if self.no_multiline_blocks {
            let b = &doc.block;
            let tags = b.tags.len();
            if tags > 0 && self.multiline_tags.iter().any(|t| t == "*" || b.has_tag(t)) {
                return self.line_checks(cx, doc);
            }
            let description = compact_description(b);
            if len(&description) >= self.minimum_length {
                return self.line_checks(cx, doc);
            }
            if self.no_single_line_blocks
                && (tags == 0 || b.tags.iter().all(|t| self.invalid_single_line(&t.tag)))
            {
                cx.report(
                    doc.comment,
                    "Multiline JSDoc blocks are prohibited by your configuration but fixing would result in a single line block which you have prohibited with `noSingleLineBlocks`.",
                );
                return;
            }
            if tags > 1 {
                if !self.allow_multiple_tags {
                    cx.report(
                        doc.comment,
                        "Multiline JSDoc blocks are prohibited by your configuration but the block has multiple tags.",
                    );
                    return;
                }
            } else if tags == 1 && !description.trim().is_empty() {
                if !self.allow_multiple_tags {
                    cx.report(
                        doc.comment,
                        "Multiline JSDoc blocks are prohibited by your configuration but the block has a description with a tag.",
                    );
                    return;
                }
            } else {
                Self::collapse(&mut doc.block);
                let fix = doc.fix();
                cx.report(
                    doc.comment,
                    "Multiline JSDoc blocks are prohibited by your configuration.",
                )
                .fix(fix);
                return;
            }
        }
        self.line_checks(cx, doc);
    }
}

impl Rule for MultilineBlocks {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, _, doc| self.check(cx, doc));
    }
}
