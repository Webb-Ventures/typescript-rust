//! `jsdoc/require-description-complete-sentence`: descriptions are made of sentences that
//! start with a capital letter and end with a period.
//!
//! Checked, in order: the block description (with fenced code blocks cut out), then every
//! tag under the preferred `description` name (`name` and description joined), then the
//! first failing tag among `@param`-like tags (`arg`, `argument`, `param`, `prop`,
//! `property`, `return`, `returns`, `template`; their description without a leading `- `),
//! then the first failing tag among `@classdesc`, `@deprecated`, `@exception`, `@file`,
//! `@fileoverview`, `@overview`, `@summary`, `@throws`, `@todo`, `@yield`, `@yields` and
//! the `tags` option. Blocks using `{@include}`, `{@includeCode}`, `{@inheritDoc}` or
//! `{@label}` are skipped.
//!
//! A text is split into paragraphs at blank lines (not after `;` or `:`; Markdown headings
//! dropped first), and a paragraph into sentences at `.`, `?` or `!` before whitespace or
//! the end (`{...}` spans after the start and the `abbreviations` removed first). Per
//! paragraph, until one fails: a sentence that is just punctuation; a sentence starting
//! lowercase (not a `|` table row); a paragraph not ending in `.`, `?`, `!`, `|` or a code
//! fence; with `newlineBeforeCapsAssumesBadSentenceEnd`, a capitalized line after one not
//! ending a sentence. Reports go on the tag's line (the block's second line for the
//! description), moved down two lines per paragraph index each time the same tag reports, at
//! the comment's column. The fix rewrites the original comment text: it appends `.` to the
//! paragraph's last line and capitalizes each lowercase sentence's first line where it
//! follows the tag (or `.`, `?`, `!`, `*` or `}`).

use super::prelude::*;
use crate::jsdoc::{Block, Doc, Finder, Preferred, preferred_tag};
use crate::jsdoc_tree::{block_inline_tags, tag_inline_tags};
use crate::jsdoc_util::{description, each_block, tag_description};
use crate::regex_exec;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-description-complete-sentence",
        "Requires that block description, explicit `@description`, and \
         `@param`/`@returns` tag descriptions are written in complete sentences",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<CompleteSentence>,
};

const OTHER_DESCRIPTIVE: &[&str] = &[
    "classdesc",
    "deprecated",
    "exception",
    "file",
    "fileoverview",
    "overview",
    "summary",
    "throws",
    "todo",
    "yield",
    "yields",
];

const WITH_NAMES: &[&str] = &[
    "arg", "argument", "param", "prop", "property", "return", "returns", "template",
];

const SKIPPING_INLINE: &[&str] = &[
    "include",
    "includeCode",
    "inheritDoc",
    "inheritdoc",
    "label",
];

#[derive(Debug)]
struct CompleteSentence {
    /// The `abbreviations` pattern, when there are any.
    abbreviations: Option<String>,
    newline_before_caps: bool,
    tags: Vec<String>,
}

impl FromOptions for CompleteSentence {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let abbrs = str_list(o, "abbreviations");
        let abbreviations = (!abbrs.is_empty()).then(|| {
            let alts: Vec<String> = abbrs
                .iter()
                .map(|a| escape_regex(&format!("{}.", a.strip_suffix('.').unwrap_or(a))))
                .collect();
            format!("\\b{}(?:$|\\s)", alts.join("|"))
        });
        Ok(CompleteSentence {
            abbreviations,
            newline_before_caps: bool_field(o, "newlineBeforeCapsAssumesBadSentenceEnd", false),
            tags: str_list(o, "tags"),
        })
    }
}

/// `escape-string-regexp`.
fn escape_regex(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '|' | '\\' | '{' | '}' | '(' | ')' | '[' | ']' | '^' | '$' | '+' | '*' | '?' | '.' => {
                out.push('\\');
                out.push(c);
            }
            '-' => out.push_str("\\x2d"),
            _ => out.push(c),
        }
    }
    out
}

fn test(pattern: &str, text: &str) -> bool {
    regex_exec::test(pattern, "u", text)
}

fn js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

/// `str[0] === str[0].toUpperCase()` (a character outside the BMP is half a pair there, which
/// has no case).
fn is_capitalized(s: &str) -> bool {
    match s.chars().next() {
        Some(c) if c.len_utf16() == 1 => c.to_uppercase().eq(std::iter::once(c)),
        _ => true,
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.len_utf16() == 1 => c.to_uppercase().chain(chars).collect(),
        _ => s.to_owned(),
    }
}

/// Splits at every match of `pattern` (which can't match empty text).
fn split(pattern: &str, text: &str) -> Vec<String> {
    let spans = regex_exec::match_spans(pattern, "gu", text).unwrap_or_default();
    let mut out = Vec::with_capacity(spans.len() + 1);
    let mut last = 0;
    for (a, b) in spans {
        if a < last || b == a {
            continue;
        }
        out.push(text[last..a].to_owned());
        last = b;
    }
    out.push(text[last..].to_owned());
    out
}

/// What a report is about: the block description, or a tag.
#[derive(Clone, Copy)]
enum Subject {
    Description,
    Tag(usize),
}

/// One block's state across its checks.
struct Check<'c, 'f> {
    finder: &'c Finder<'f>,
    doc: &'c Doc,
    abbreviations: Option<&'c str>,
    newline_before_caps: bool,
    /// The line each subject reports on next (description first, then each tag); the plugin
    /// moves it on every report.
    lines: Vec<Option<i64>>,
}

impl Check<'_, '_> {
    fn drop_abbreviations(&self, s: &str) -> String {
        match self.abbreviations {
            Some(p) => regex_exec::replace(p, "gu", s, "").unwrap_or_else(|| s.to_owned()),
            None => s.to_owned(),
        }
    }

    fn sentences(&self, paragraph: &str) -> Vec<String> {
        let txt = regex_exec::replace("(?<!^)\\{[\\s\\S]*?\\}\\s*", "gu", paragraph, "")
            .unwrap_or_else(|| paragraph.to_owned());
        let txt = self.drop_abbreviations(&txt);
        let puncts: Vec<&str> = regex_exec::match_spans("([.?!])(?:\\s+|$)", "gu", &txt)
            .unwrap_or_default()
            .into_iter()
            .map(|(a, b)| &txt[a..b])
            .collect();
        split("[.?!](?:\\s+|$)", &txt)
            .into_iter()
            .enumerate()
            .map(|(i, s)| match puncts.get(i) {
                None if test("^\\s*$", &s) => s,
                p => format!("{s}{}", p.copied().unwrap_or("")),
            })
            .collect()
    }

    fn report(
        &mut self,
        cx: &mut Ctx<'_, '_>,
        subject: Subject,
        par: usize,
        msg: &str,
        fix: Option<String>,
    ) {
        let slot = match subject {
            Subject::Description => 0,
            Subject::Tag(ti) => ti + 1,
        };
        let base = match subject {
            Subject::Description => {
                self.doc
                    .block
                    .source
                    .first()
                    .map_or(0, |&id| self.doc.block.lines[id].number)
                    + 1
            }
            Subject::Tag(ti) => self.doc.block.tag_line(ti),
        };
        let line = self.lines[slot].unwrap_or(base) + 2 * par as i64;
        self.lines[slot] = Some(line);
        let (span, loc) = self.doc.report_at(self.finder, line, Some(0));
        let r = cx.report(span, msg).loc(loc);
        if let Some(text) = fix {
            r.fix(Fix::replace(self.doc.comment, text));
        }
    }

    /// The comment text with the paragraph's sentences fixed.
    fn fixed(&self, subject: Subject, paragraph: &str, sentences: &[String]) -> String {
        let mut text = self.doc.comment.text(self.finder.src).to_owned();
        if !test("[.:?!]$", paragraph)
            && let Some(line) = paragraph.split('\n').rfind(|l| !l.is_empty())
        {
            let pattern = format!("{}$", escape_regex(line));
            if let Some(t) = regex_exec::replace(&pattern, "mu", &text, &format!("{line}.")) {
                text = t;
            }
        }
        for s in sentences {
            if test("^\\s*$", s) || is_capitalized(s) || s.starts_with('|') {
                continue;
            }
            let beginning = s.split('\n').next().unwrap_or("");
            let replaced = match subject {
                Subject::Tag(ti) => {
                    let tag = &self.doc.block.tags[ti].tag;
                    let pattern = format!("(@{}.*){}", escape_regex(tag), escape_regex(beginning));
                    regex_exec::replace_with(&pattern, "u", &text, |g| {
                        format!("{}{}", g[1].unwrap_or(""), capitalize(beginning))
                    })
                }
                Subject::Description => {
                    let pattern = format!("((?:[.?!]|\\*|\\}})\\s*){}", escape_regex(beginning));
                    regex_exec::replace(
                        &pattern,
                        "u",
                        &text,
                        &format!("$1{}", capitalize(beginning)),
                    )
                }
            };
            if let Some(t) = replaced {
                text = t;
            }
        }
        text
    }

    /// `validateDescription`: whether `text` reported.
    fn validate(&mut self, cx: &mut Ctx<'_, '_>, text: &str, subject: Subject) -> bool {
        if text.is_empty() || test("^\\n+$", text) {
            return false;
        }
        let no_headings =
            regex_exec::replace("^\\s*#[^\\n]*(\\n|$)", "gmu", text, "").unwrap_or_default();
        let paragraphs: Vec<String> = split("(?<![;:])\\n\\n+", &no_headings)
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect();
        for (par, paragraph) in paragraphs.iter().enumerate() {
            let sentences = self.sentences(paragraph);
            if sentences.iter().any(|s| test("^[.?!]$", s)) {
                self.report(
                    cx,
                    subject,
                    par,
                    "Sentences must be more than punctuation.",
                    None,
                );
            }
            let lowercase = sentences
                .iter()
                .any(|s| !test("^\\s*$", s) && !is_capitalized(s) && !s.starts_with('|'));
            if lowercase {
                let fix = self.fixed(subject, paragraph, &sentences);
                self.report(
                    cx,
                    subject,
                    par,
                    "Sentences should start with an uppercase character.",
                    Some(fix),
                );
            }
            let plain = self.drop_abbreviations(paragraph);
            if !test("(?:[.?!|]|```)\\s*$", &plain) {
                let fix = self.fixed(subject, paragraph, &sentences);
                self.report(
                    cx,
                    subject,
                    par,
                    "Sentences must end with a period.",
                    Some(fix),
                );
                return true;
            }
            if self.newline_before_caps && !newline_after_sentence_end(&plain) {
                self.report(
                    cx,
                    subject,
                    par,
                    "A line of text is started with an uppercase character, but the \
                     preceding line does not end the sentence.",
                    None,
                );
                return true;
            }
        }
        false
    }
}

/// `isNewLinePrecededByAPeriod`: no line starting with a capital and a lowercase letter
/// follows a line not ending in `.`, `:`, `?`, `!` or `|`.
fn newline_after_sentence_end(text: &str) -> bool {
    let mut last_ends: Option<bool> = None;
    for line in text.split('\n') {
        if last_ends == Some(false) && test("^[A-Z][a-z]", line) {
            return false;
        }
        last_ends = Some(test("[.:?!|]$", line));
    }
    true
}

fn uses_skipping_inline(b: &Block) -> bool {
    let skip = |t: &str| SKIPPING_INLINE.contains(&t);
    block_inline_tags(b).iter().any(|t| skip(&t.tag))
        || (0..b.tags.len()).any(|ti| tag_inline_tags(b, ti).iter().any(|t| skip(&t.tag)))
}

/// `${name} ${description}`, trimmed.
fn named_description(b: &Block, ti: usize) -> String {
    format!("{} {}", b.tags[ti].name, tag_description(b, ti))
        .trim_matches(js_space)
        .to_owned()
}

impl CompleteSentence {
    fn check_block(&self, cx: &mut Ctx<'_, '_>, finder: &Finder<'_>, doc: &Doc) {
        let b = &doc.block;
        if uses_skipping_inline(b) {
            return;
        }
        let mut desc = description(b).text();
        if let Some(spans) = regex_exec::match_spans("```[\\s\\S]*```", "gu", &desc) {
            for (a, e) in spans.into_iter().rev() {
                desc.replace_range(a..e, "");
            }
        }
        let mut check = Check {
            finder,
            doc,
            abbreviations: self.abbreviations.as_deref(),
            newline_before_caps: self.newline_before_caps,
            lines: vec![None; b.tags.len() + 1],
        };
        if check.validate(cx, &desc, Subject::Description) {
            return;
        }
        if let Preferred::Name(name) = preferred_tag(finder.settings, "description") {
            for ti in (0..b.tags.len()).filter(|&ti| b.tags[ti].tag == name) {
                let text = named_description(b, ti);
                check.validate(cx, &text, Subject::Tag(ti));
            }
        }
        let named: Vec<usize> = (0..b.tags.len())
            .filter(|&ti| WITH_NAMES.contains(&b.tags[ti].tag.as_str()))
            .collect();
        let unnamed: Vec<usize> = (0..b.tags.len())
            .filter(|&ti| {
                let t = b.tags[ti].tag.as_str();
                OTHER_DESCRIPTIVE.contains(&t)
                    || (self.tags.iter().any(|x| x == t)
                        && !named.iter().any(|&n| b.tags[n].tag == t))
            })
            .collect();
        for &ti in &named {
            let d = tag_description(b, ti);
            let d = d
                .strip_prefix("- ")
                .unwrap_or(&d)
                .trim_end_matches(js_space);
            if check.validate(cx, d, Subject::Tag(ti)) {
                break;
            }
        }
        for &ti in &unnamed {
            let text = named_description(b, ti);
            if check.validate(cx, &text, Subject::Tag(ti)) {
                break;
            }
        }
    }
}

impl Rule for CompleteSentence {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, finder, doc| self.check_block(cx, finder, doc));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(escape_regex("a.b-c"), "a\\.b\\x2dc");
        assert!(is_capitalized("Foo") && !is_capitalized("foo") && is_capitalized("."));
        assert_eq!(capitalize("тест"), "Тест");
        assert_eq!(split("\\n\\n+", "a\n\n\nb"), vec!["a", "b"]);
    }
}
