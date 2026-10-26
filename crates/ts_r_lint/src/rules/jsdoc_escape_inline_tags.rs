//! `jsdoc/escape-inline-tags`: a `@tag` in the middle of descriptive text (where TypeScript
//! would read it as a new tag) must be escaped or wrapped in an inline tag or code span.

use super::prelude::*;
use crate::jsdoc::{self, Block, Contexts, Doc, Settings, Tokens};

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
fn code_spans(desc: &str) -> Vec<(usize, usize)> {
    let b = desc.as_bytes();
    let run = |s: usize| b[s..].iter().take_while(|&&c| c == b'`').count();
    let mut out = Vec::new();
    let mut s = 0;
    while s < b.len() {
        if b[s] == b'`' && (s == 0 || b[s - 1] != b'\\') {
            let n = run(s);
            let close = (s + n..b.len()).find(|&e| b[e] == b'`' && b[e - 1] != b'`' && run(e) == n);
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

/// Whether `at` is inside the content of a markdown code span.
fn in_code_span(desc: &str, at: usize) -> bool {
    for (start, end) in code_spans(desc) {
        if at < start {
            return false;
        }
        if at < end {
            return true;
        }
    }
    false
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

fn ignored(desc: &str, m_start: usize, m_end: usize) -> bool {
    let at = m_start + desc[m_start..m_end].rfind('@').unwrap_or(0);
    in_code_span(desc, at)
        || (matches!(
            inline_tag_name(desc, at),
            "inheritDoc" | "link" | "linkcode" | "linkplain"
        ) && scoped_package(&desc[at..]))
}

impl EscapeInlineTags {
    fn unescaped<'d>(&self, desc: &'d str) -> Option<&'d str> {
        scan(desc).into_iter().find_map(|(s, e, n)| {
            let name = &desc[n..e];
            (!self.allowed.iter().any(|a| a == name) && !ignored(desc, s, e)).then_some(name)
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
        for (s, e) in Self::occurrences(desc, name) {
            if ignored(desc, s, e) {
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
fn rewire(b: &mut Block) {
    let mut by_number: Vec<(i64, usize)> = Vec::new();
    for t in &b.tags {
        for &id in &t.source {
            let n = b.lines[id].number;
            match by_number.iter_mut().find(|(k, _)| *k == n) {
                Some(e) => e.1 = id,
                None => by_number.push((n, id)),
            }
        }
    }
    for i in 0..b.source.len() {
        let n = b.lines[b.source[i]].number;
        if let Some(&(_, id)) = by_number.iter().find(|(k, _)| *k == n) {
            b.source[i] = id;
        }
    }
}

impl EscapeInlineTags {
    fn check(&self, cx: &mut Ctx<'_, '_>, f: &jsdoc::Finder<'_>, doc: &mut Doc) {
        let description = block_description(&doc.block);
        let normalized = description.strip_prefix('\n').unwrap_or(&description);
        let mut found: Vec<(String, usize)> = Vec::new();
        let mut line_start = 0;
        for (idx, line) in normalized.split('\n').enumerate() {
            for (s, e, n) in scan(line) {
                let name = &line[n..e];
                if self.allowed.iter().any(|a| a == name)
                    || ignored(normalized, line_start + s, line_start + e)
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
                for (k, &id) in lines.iter().enumerate() {
                    let d = &doc.block.lines[id].tokens.description;
                    if !d.is_empty() && !Self::occurrences(d, &name).is_empty() {
                        let new = self.escape(d, &name);
                        doc.block.lines[id].tokens.description = new;
                        rel = k as i64;
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

    #[test]
    fn code_spans() {
        assert!(in_code_span("x `a @b` y", 5));
        assert!(!in_code_span("x `a` @b", 6));
        assert!(in_code_span("``npm `init` @eslint/config``", 13));
        assert!(!in_code_span("\\`a @b`", 4));
    }
}
