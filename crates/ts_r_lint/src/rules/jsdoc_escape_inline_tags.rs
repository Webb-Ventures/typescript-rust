//! `jsdoc/escape-inline-tags`: a `@tag` in the middle of descriptive text (where TypeScript
//! would read it as a new tag) must be escaped or wrapped in an inline tag or code span.

use super::prelude::*;
use crate::jsdoc::{self, Block, Contexts, Doc, Settings, Tokens};
use rustc_hash::FxHashMap;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/escape-inline-tags",
        "Reports use of JSDoc tags in non-tag positions (in the default \"typescript\" mode)",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<EscapeInlineTags>,
};

#[derive(Debug)]
struct EscapeInlineTags {
    allowed: Vec<String>,
    enable_fixer: bool,
    backticks: bool,
}

impl FromOptions for EscapeInlineTags {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(EscapeInlineTags {
            allowed: str_list(o, "allowedInlineTags"),
            enable_fixer: bool_field(o, "enableFixer", false),
            backticks: str_field(o, "fixType") == Some("backticks"),
        })
    }
}

/// Upper bound on fix-and-rescan rounds per tag (the plugin loops until nothing is left).
const MAX_ROUNDS: usize = 100;

fn word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// The tag name start of a `(start or whitespace)@` at byte `i`, if any.
fn at_sign(desc: &str, i: usize) -> Option<usize> {
    let b = desc.as_bytes();
    if i == 0 && b.first() == Some(&b'@') {
        return Some(0);
    }
    let c = desc[i..].chars().next()?;
    let n = i + c.len_utf8();
    ((c.is_whitespace() || c == '\u{feff}') && b.get(n) == Some(&b'@')).then_some(n)
}

/// Matches of `(start or whitespace)@word`: (match start, match end, tag name start).
fn scan(desc: &str) -> Vec<(usize, usize, usize)> {
    let b = desc.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < desc.len() {
        if let Some(at) = at_sign(desc, i) {
            let name_start = at + 1;
            let mut e = name_start;
            while e < b.len() && word(b[e]) {
                e += 1;
            }
            if e > name_start {
                out.push((i, e, name_start));
                i = e;
                continue;
            }
        }
        i += desc[i..].chars().next().map_or(1, char::len_utf8);
    }
    out
}

/// The contents of the markdown code spans of `desc`, in order (`` `a` ``, ``` ``a`b`` ```;
/// a backslash before the opening run disables it).
///
/// An opener is the rest of a backtick run from `s` (`n` backticks); its closer is the next
/// whole run of exactly `n` backticks. Whole runs are indexed by length up front, so a long
/// run with no closer costs a lookup per backtick, not a rescan of the rest of the text.
fn code_spans(desc: &str) -> Vec<(usize, usize)> {
    let b = desc.as_bytes();
    let run = |s: usize| b[s..].iter().take_while(|&&c| c == b'`').count();
    let mut by_len: FxHashMap<usize, Vec<usize>> = FxHashMap::default();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'`' {
            let n = run(i);
            by_len.entry(n).or_default().push(i);
            i += n;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    let mut s = 0;
    // The end of the backtick run `s` is in (recomputed when `s` passes it).
    let mut run_end = 0;
    while s < b.len() {
        if b[s] == b'`' && (s == 0 || b[s - 1] != b'\\') {
            if run_end <= s {
                run_end = s + run(s);
            }
            let n = run_end - s;
            let close = by_len
                .get(&n)
                .and_then(|starts| starts.get(starts.partition_point(|&e| e < s + n)).copied());
            if let Some(e) = close {
                out.push((s + n, e));
                s = e + n;
                continue;
            }
        }
        s += 1;
    }
    out
}

/// Whether `at` is inside the content of one of `spans` (from [`code_spans`]).
fn in_code_span(spans: &[(usize, usize)], at: usize) -> bool {
    let k = spans.partition_point(|&(_, end)| end <= at);
    spans.get(k).is_some_and(|&(start, _)| start <= at)
}

fn inline_tag_name(desc: &str, at: usize) -> &str {
    let Some(start) = desc[..(at + 2).min(desc.len())].rfind("{@") else {
        return "";
    };
    match desc[start..].find('}') {
        Some(end) if start + end > at => {}
        _ => return "",
    }
    let rest = &desc[start + 2..];
    let n = rest.bytes().take_while(|&c| word(c)).count();
    &rest[..n]
}

fn scoped_package(s: &str) -> bool {
    let part = |c: u8| word(c) || c == b'.' || c == b'-';
    let b = s.as_bytes();
    if b.first() != Some(&b'@') {
        return false;
    }
    let n = b[1..].iter().take_while(|&&c| part(c)).count();
    if n == 0 || b.get(1 + n) != Some(&b'/') {
        return false;
    }
    b.get(2 + n).is_some_and(|&c| part(c))
}

/// Whether the match `m_start..m_end` of `desc` is exempt; `spans` are `desc`'s code spans.
fn ignored(desc: &str, spans: &[(usize, usize)], m_start: usize, m_end: usize) -> bool {
    let at = m_start + desc[m_start..m_end].rfind('@').unwrap_or(0);
    in_code_span(spans, at)
        || (matches!(
            inline_tag_name(desc, at),
            "inheritDoc" | "link" | "linkcode" | "linkplain"
        ) && scoped_package(&desc[at..]))
}

impl EscapeInlineTags {
    fn unescaped<'d>(&self, desc: &'d str) -> Option<&'d str> {
        let spans = code_spans(desc);
        scan(desc).into_iter().find_map(|(s, e, n)| {
            let name = &desc[n..e];
            (!self.allowed.iter().any(|a| a == name) && !ignored(desc, &spans, s, e))
                .then_some(name)
        })
    }

    /// Occurrences of `(start or whitespace)@name` (a prefix of a longer word counts too).
    fn occurrences(desc: &str, name: &str) -> Vec<(usize, usize)> {
        let pat = format!("@{name}");
        let mut out = Vec::new();
        let mut i = 0;
        while i < desc.len() {
            let c = desc[i..].chars().next().unwrap_or(' ');
            if let Some(at) = at_sign(desc, i).filter(|&at| desc[at..].starts_with(&pat)) {
                let end = at + pat.len();
                out.push((i, end));
                i = end;
                continue;
            }
            i += c.len_utf8();
        }
        out
    }

    fn escape(&self, desc: &str, name: &str) -> String {
        let mut out = String::with_capacity(desc.len() + 2);
        let mut last = 0;
        let spans = code_spans(desc);
        for (s, e) in Self::occurrences(desc, name) {
            if ignored(desc, &spans, s, e) {
                continue;
            }
            let at = s + desc[s..e].rfind('@').unwrap_or(0);
            out.push_str(&desc[last..at]);
            if self.backticks {
                out.push_str(&format!("`@{name}`"));
            } else {
                out.push_str(&format!("\\@{name}"));
            }
            last = e;
        }
        out.push_str(&desc[last..]);
        out
    }
}

fn message(name: &str) -> String {
    format!(
        "Unexpected inline JSDoc tag. Did you mean to use {{@{name}}}, \\@{name}, or `@{name}`?"
    )
}

/// The block description (`getDescription`).
fn block_description(b: &Block) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let mut tags_begun = false;
    for (idx, &id) in b.source.iter().enumerate() {
        let t = &b.lines[id].tokens;
        if !t.tag.is_empty() {
            tags_begun = true;
        }
        if idx > 0 && (!t.tag.is_empty() || !t.end.is_empty()) {
            if !tags_begun && !t.description.is_empty() {
                parts.push(&t.description);
            }
            break;
        }
        if !tags_begun && (idx > 0 || !t.description.is_empty()) {
            parts.push(if !t.description.is_empty() {
                &t.description
            } else if parts.is_empty() {
                "\n"
            } else {
                ""
            });
        }
    }
    parts.join("\n")
}

/// Rewrites the block description lines (`setBlockDescription`).
fn set_block_description(b: &mut Block, f: impl Fn(&str) -> String) {
    let mut start_idx = None;
    let mut end_idx = b.source.len();
    let mut info = Tokens::default();
    let mut descs = Vec::new();
    for (idx, &id) in b.source.iter().enumerate() {
        let t = &b.lines[id].tokens;
        if t.delimiter == "/**" {
            continue;
        }
        if start_idx.is_none() {
            start_idx = Some(idx);
            info.start = t.start.clone();
            info.delimiter = t.delimiter.clone();
        }
        if !t.tag.is_empty() || !t.end.is_empty() {
            end_idx = idx;
            break;
        }
        descs.push(t.description.clone());
    }
    let Some(start) = start_idx else {
        return;
    };
    if descs.is_empty() {
        return;
    }
    let mut new_ids = Vec::with_capacity(descs.len());
    for d in descs {
        let description = f(&d);
        let tokens = Tokens {
            start: info.start.clone(),
            delimiter: info.delimiter.clone(),
            post_delimiter: if description.trim().is_empty() {
                String::new()
            } else {
                " ".to_owned()
            },
            description,
            ..Tokens::default()
        };
        new_ids.push(b.new_line(0, tokens));
    }
    b.source.splice(start..end_idx.max(start), new_ids);
}

/// A tag's description lines (`getTagDescription`).
fn tag_descriptions(b: &Block, tag: usize) -> Vec<String> {
    let mut out = Vec::new();
    for &id in &b.tags[tag].source {
        let t = &b.lines[id].tokens;
        let lead = if !t.tag.is_empty() && !t.post_tag.is_empty() {
            t.post_tag.as_str()
        } else if t.tag.is_empty() && t.name.is_empty() && t.typ.is_empty() {
            t.post_delimiter.as_str()
        } else {
            ""
        };
        let mut it = lead.chars();
        it.next();
        let d = format!("{}{}{}", it.as_str(), t.description, t.line_end);
        if !t.end.is_empty() {
            if !d.is_empty() {
                out.push(d);
            }
            break;
        }
        out.push(d);
    }
    out
}

/// `rewireSpecs`: block lines are replaced by the tag line with the same number.
/// The last tag line with a number wins; one map lookup per line.
fn rewire(b: &mut Block) {
    let mut by_number: FxHashMap<i64, usize> = FxHashMap::default();
    for t in &b.tags {
        for &id in &t.source {
            by_number.insert(b.lines[id].number, id);
        }
    }
    for i in 0..b.source.len() {
        if let Some(&id) = by_number.get(&b.lines[b.source[i]].number) {
            b.source[i] = id;
        }
    }
}

impl EscapeInlineTags {
    fn check(&self, cx: &mut Ctx<'_, '_>, f: &jsdoc::Finder<'_>, doc: &mut Doc) {
        let description = block_description(&doc.block);
        let normalized = description.strip_prefix('\n').unwrap_or(&description);
        let mut found: Vec<(String, usize)> = Vec::new();
        let spans = code_spans(normalized);
        let mut line_start = 0;
        for (idx, line) in normalized.split('\n').enumerate() {
            for (s, e, n) in scan(line) {
                let name = &line[n..e];
                if self.allowed.iter().any(|a| a == name)
                    || ignored(normalized, &spans, line_start + s, line_start + e)
                {
                    continue;
                }
                found.push((name.to_owned(), idx));
            }
            line_start += line.len() + 1;
        }
        for (name, idx) in found {
            let span = doc.line_span(f, idx as i64 + 1);
            let fix = self.enable_fixer.then(|| {
                set_block_description(&mut doc.block, |d| self.escape(d, &name));
                doc.fix()
            });
            cx.report(span, message(&name)).fix_opt(fix);
        }
        for ti in 0..doc.block.tags.len() {
            if doc.block.tags[ti].tag == "example" {
                continue;
            }
            for _ in 0..MAX_ROUNDS {
                let name = tag_descriptions(&doc.block, ti)
                    .iter()
                    .find_map(|d| self.unescaped(d).map(str::to_owned));
                let Some(name) = name else {
                    break;
                };
                let mut rel = 0;
                let lines = doc.block.tags[ti].source.clone();
                // The first line with an occurrence the escape changes: one that is only in a
                // code span (or a `{@link @scope/pkg}`) is not the one `unescaped` found.
                let mut changed = false;
                for (k, &id) in lines.iter().enumerate() {
                    let d = &doc.block.lines[id].tokens.description;
                    if !d.is_empty() && !Self::occurrences(d, &name).is_empty() {
                        let new = self.escape(d, &name);
                        if new == *d {
                            continue;
                        }
                        doc.block.lines[id].tokens.description = new;
                        rel = k as i64;
                        changed = true;
                        break;
                    }
                }
                let line = rel + doc.block.tag_line(ti);
                let fix = self.enable_fixer.then(|| {
                    rewire(&mut doc.block);
                    doc.fix()
                });
                cx.report(doc.line_span(f, line), message(&name))
                    .fix_opt(fix);
                if !changed {
                    // Nothing on the tag's lines to escape: another round would find the same
                    // name again.
                    break;
                }
            }
        }
    }
}

impl Rule for EscapeInlineTags {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let settings = Settings::from_cx(cx);
        if settings.mode != "typescript" {
            return;
        }
        let contexts = Contexts {
            entries: Vec::new(),
            all: true,
            plain_any: false,
            additive: false,
            no_tracking: false,
        };
        jsdoc::for_each_doc(cx, &settings, &contexts, |cx, f, doc| {
            self.check(cx, f, doc)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_tags() {
        let m = scan("@a b @c d@e");
        let names: Vec<_> = m.iter().map(|&(_, e, n)| &"@a b @c d@e"[n..e]).collect();
        assert_eq!(names, ["a", "c"]);
    }

    fn in_span(desc: &str, at: usize) -> bool {
        in_code_span(&super::code_spans(desc), at)
    }

    #[test]
    fn code_spans() {
        assert!(in_span("x `a @b` y", 5));
        assert!(!in_span("x `a` @b", 6));
        assert!(in_span("``npm `init` @eslint/config``", 13));
        assert!(!in_span("\\`a @b`", 4));
        // The tail of an unmatched run opens a span when a run that long follows.
        assert_eq!(super::code_spans("```a``b"), [(3, 4)]);
        assert_eq!(super::code_spans("``a`b`c"), [(2, 3)]);
    }

    #[test]
    fn long_unmatched_backtick_run_is_fast() {
        let desc = format!("text @bad {}", "`".repeat(200_000));
        let start = std::time::Instant::now();
        assert!(super::code_spans(&desc).is_empty());
        assert!(start.elapsed().as_secs() < 5, "took {:?}", start.elapsed());
    }
}
