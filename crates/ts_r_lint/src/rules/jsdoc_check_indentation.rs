//! `jsdoc/check-indentation`: no padding of two or more spaces or tabs after a line's `*`.
//!
//! Fenced code blocks (from a "* ```" line to the line before one starting with "* ```", a
//! tag or the comment end) and the content of `excludeTags` tags (`@example`, up to the
//! line before the next tag or the end) are blanked first, keeping their line count. Then
//! the first line whose `*` (or the opening `/**`) is followed by two spaces or tabs is
//! reported. With `allowIndentedSections` only tag lines, the first content line and lines
//! indented less than the previous indented line of the same section are reported; that
//! mode reports one line lower than the offending line, except on the first line (the
//! plugin counts the line breaks before the line wrongly), and we keep that.

use super::prelude::*;
use crate::jsdoc_util::{each_block, is_js_ws};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/check-indentation",
        "Reports invalid padding inside JSDoc blocks.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<CheckIndentation>,
};

#[derive(Debug)]
struct CheckIndentation {
    allow_indented_sections: bool,
    exclude_tags: Vec<String>,
}

impl FromOptions for CheckIndentation {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = opts.first().and_then(Value::as_object);
        Ok(CheckIndentation {
            allow_indented_sections: o
                .and_then(|o| o.get("allowIndentedSections"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            exclude_tags: match o
                .and_then(|o| o.get("excludeTags"))
                .and_then(Value::as_array)
            {
                Some(a) => a
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect(),
                None => vec!["example".to_owned()],
            },
        })
    }
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `@\w+\s` at `i`.
fn tag_then_space(s: &str, i: usize) -> bool {
    let b = s.as_bytes();
    if b.get(i) != Some(&b'@') {
        return false;
    }
    let mut j = i + 1;
    while j < b.len() && is_word(b[j]) {
        j += 1;
    }
    j > i + 1 && s[j..].chars().next().is_some_and(is_js_ws)
}

fn skip_blank(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    i
}

/// The lookahead after a masked section's last line break: `[ \t]*\*`, then `/`, a tag, or
/// (for code blocks) a fence.
fn ends_section(s: &str, i: usize, fence: bool) -> bool {
    let b = s.as_bytes();
    let i = skip_blank(b, i);
    if b.get(i) != Some(&b'*') {
        return false;
    }
    if b.get(i + 1) == Some(&b'/') {
        return true;
    }
    let j = skip_blank(b, i + 1);
    (fence && s[j..].starts_with("```")) || tag_then_space(s, j)
}

/// Replaces each match of `([ \t]+\*)[ \t]<opener>...\n(?=<end>)` by its margin and a line
/// break per line break of the masked text. `opener(s, i)` gives the end of the opener
/// starting at `i` (after the space), if there is one there.
fn mask(s: &str, opener: impl Fn(&str, usize) -> Option<usize>, fence: bool) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    let mut i = 0;
    while i < b.len() {
        // A run of spaces and tabs followed by `*`, a space or tab, and the opener.
        if !(b[i] == b' ' || b[i] == b'\t') {
            i += 1;
            continue;
        }
        let run_end = skip_blank(b, i);
        let ok = b.get(run_end) == Some(&b'*')
            && matches!(b.get(run_end + 1), Some(b' ' | b'\t'))
            && opener(s, run_end + 2).is_some();
        if !ok {
            i = run_end.max(i + 1);
            continue;
        }
        let after = opener(s, run_end + 2).unwrap_or(run_end + 2);
        // The first line break after the opener that ends the section.
        let mut n = after;
        let mut end = None;
        while let Some(off) = s[n..].find('\n') {
            let at = n + off;
            if ends_section(s, at + 1, fence) {
                end = Some(at);
                break;
            }
            n = at + 1;
        }
        let Some(end) = end else {
            // No later opener can find an end either.
            break;
        };
        let breaks = s[after..=end].matches('\n').count();
        out.push_str(&s[pos..i]);
        let margin = &s[i..=run_end];
        for _ in 0..breaks {
            out.push_str(margin);
            out.push('\n');
        }
        pos = end + 1;
        i = pos;
    }
    out.push_str(&s[pos..]);
    out
}

/// `^(?:\/?\**|[\t ]*)\*([\t ]{n,})`: the length of the match and of its padding.
fn indent_match(line: &str, min: usize) -> Option<(usize, usize)> {
    let b = line.as_bytes();
    let pad = |at: usize| skip_blank(b, at) - at;
    // `\/?\**\*`: an optional slash and one or more stars.
    let mut i = usize::from(b.first() == Some(&b'/'));
    let stars = b[i..].iter().take_while(|&&c| c == b'*').count();
    if stars > 0 {
        i += stars;
        let p = pad(i);
        if p >= min {
            return Some((i + p, p));
        }
    }
    // `[\t ]*\*`.
    let i = skip_blank(b, 0);
    if b.get(i) == Some(&b'*') {
        let p = pad(i + 1);
        if p >= min {
            return Some((i + 1 + p, p));
        }
    }
    None
}

/// `^\s*\*\s+\S`.
fn star_then_content(line: &str) -> bool {
    let rest = line.trim_start_matches(is_js_ws);
    let Some(rest) = rest.strip_prefix('*') else {
        return false;
    };
    let after = rest.trim_start_matches(is_js_ws);
    after.len() < rest.len() && !after.is_empty()
}

/// `/@\w+/`.
fn has_tag_like(line: &str) -> bool {
    let b = line.as_bytes();
    (0..b.len()).any(|i| b[i] == b'@' && b.get(i + 1).is_some_and(|&c| is_word(c)))
}

impl CheckIndentation {
    fn masked(&self, text: &str) -> String {
        let text = mask(
            text,
            |s, i| s[i..].starts_with("```").then_some(i + 3),
            true,
        );
        if self.exclude_tags.is_empty() {
            return text;
        }
        let tags = &self.exclude_tags;
        mask(
            &text,
            |s, i| {
                let rest = s[i..].strip_prefix('@')?;
                tags.iter().find_map(|t| {
                    let after = rest.strip_prefix(t.as_str())?;
                    matches!(after.as_bytes().first(), Some(b' ' | b'\n'))
                        .then_some(i + 1 + t.len())
                })
            },
            false,
        )
    }

    /// The comment-relative line to report.
    fn find(&self, text: &str) -> Option<i64> {
        let lines: Vec<&str> = text.split('\n').collect();
        if !self.allow_indented_sections {
            return lines
                .iter()
                .position(|l| indent_match(l, 2).is_some())
                .map(|i| i as i64);
        }
        let line_number = |i: usize| i.saturating_sub(1) as i64 + 1;
        let mut seen_content = false;
        let mut section: Option<usize> = None;
        for (idx, line) in lines.iter().enumerate() {
            if let Some((len, amount)) = indent_match(line, 2) {
                let after = &line[len..];
                let b = after.as_bytes();
                if b.first() == Some(&b'@') && b.get(1).is_some_and(|&c| is_word(c)) {
                    return Some(line_number(idx));
                }
                let content = !after.trim_matches(is_js_ws).is_empty();
                if !seen_content && content {
                    return Some(line_number(idx));
                }
                if seen_content && content {
                    match section {
                        None => section = Some(amount),
                        Some(cur) if amount < cur => return Some(line_number(idx)),
                        Some(_) => {}
                    }
                }
            } else if star_then_content(line) {
                section = None;
            }
            if star_then_content(line) {
                seen_content = true;
            }
            if has_tag_like(line) {
                section = None;
            }
        }
        None
    }
}

impl Rule for CheckIndentation {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let src = cx.file.src;
        each_block(cx, |cx, f, doc| {
            let text = self.masked(doc.comment.text(src));
            if let Some(rel) = self.find(&text) {
                let (span, loc) = doc.report_at(f, rel, None);
                cx.report(span, "There must be no indentation.").loc(loc);
            }
        });
    }
}
