//! `jsdoc/normalize-see-links`: labelled links in `@see` tags use one `{@link}` form,
//! `{@link url|label}` (`canonicalForm: "pipe"`, the default) or `[label]{@link url}`
//! (`"prefix"`).
//!
//! Each `@see` tag's text (its name, the spacing after it and its description) is
//! scanned for markdown links `[label](url)`, the non-canonical `{@link}` form and inline
//! `{@link}` tags. A tag is reported as unsafe ("cannot be safely normalized") when a link
//! sits in a markdown code span, points at a scoped package (`@scope/pkg`), has a blank
//! label or uses the space form (`{@link url label}`), or when, with the recognised links
//! removed, something that looks like a broken link is left. Otherwise a tag with a
//! non-canonical link (or, with `wrapBareUrls`, whose whole text is one lowercase `http:`
//! or `https:` URL that parses) is reported; the first report's fix rewrites every such
//! tag: links in the canonical form (the label unescaped from its old form and escaped
//! for the new one, `%`, `|`, `{`, `}` percent-encoded in the target) and bare URLs as
//! `{@link url}`. Links are rewritten line by line, as the plugin does.

use super::prelude::*;
use crate::jsdoc::{Block, Doc};
use crate::jsdoc_tree::inline_tags;
use crate::jsdoc_util::{each_block, is_js_ws, js_trim, tag_description};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/normalize-see-links",
        "Normalizes labeled links in `@see` tags to a canonical `{@link}` form.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NormalizeSeeLinks>,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    Pipe,
    Prefix,
    Markdown,
}

#[derive(Debug)]
struct NormalizeSeeLinks {
    canonical: Form,
    enable_fixer: bool,
    wrap_bare_urls: bool,
}

impl FromOptions for NormalizeSeeLinks {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = opts.first().and_then(Value::as_object);
        let get = |k: &str| o.and_then(|o| o.get(k));
        Ok(NormalizeSeeLinks {
            canonical: match get("canonicalForm").and_then(Value::as_str) {
                Some("prefix") => Form::Prefix,
                _ => Form::Pipe,
            },
            enable_fixer: get("enableFixer").and_then(Value::as_bool).unwrap_or(true),
            wrap_bare_urls: get("wrapBareUrls")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

fn form_name(f: Form) -> &'static str {
    match f {
        Form::Pipe => "pipe",
        Form::Prefix => "prefix",
        Form::Markdown => "markdown",
    }
}

fn is_nl(c: char) -> bool {
    c == '\r' || c == '\n'
}

fn char_at(s: &str, i: usize) -> Option<char> {
    s.get(i..).and_then(|r| r.chars().next())
}

/// A label from `i`: characters other than the `stops` and line breaks, or a backslash
/// and any character but a line break. Returns where it stops.
fn scan_label(s: &str, mut i: usize, stops: &[char]) -> usize {
    while let Some(c) = char_at(s, i) {
        if c == '\\' {
            match char_at(s, i + 1) {
                Some(n) if !is_nl(n) => i += 1 + n.len_utf8(),
                _ => break,
            }
        } else if stops.contains(&c) || is_nl(c) {
            break;
        } else {
            i += c.len_utf8();
        }
    }
    i
}

fn skip_ws(s: &str, mut i: usize) -> usize {
    while let Some(c) = char_at(s, i)
        && is_js_ws(c)
    {
        i += c.len_utf8();
    }
    i
}

/// A run of characters for which `ok` holds, from `i`.
fn scan_while(s: &str, mut i: usize, ok: impl Fn(char) -> bool) -> usize {
    while let Some(c) = char_at(s, i)
        && ok(c)
    {
        i += c.len_utf8();
    }
    i
}

#[derive(Debug, Clone, Copy)]
struct Link {
    start: usize,
    end: usize,
    label: (usize, usize),
    target: (usize, usize),
}

/// What a failed attempt tells the search: where it may resume.
type Attempt = Result<Link, usize>;

fn not_escaped(s: &str, i: usize) -> bool {
    !s[..i].ends_with('\\')
}

/// `[label](target)` at `i`.
fn markdown_at(s: &str, i: usize) -> Attempt {
    let ls = i + 1;
    let le = scan_label(s, ls, &['`', ']']);
    if le == ls {
        return Err(le.max(i + 1));
    }
    let Some(rest) = s.get(le..).and_then(|r| r.strip_prefix("](")) else {
        return Err(le);
    };
    let ts = s.len() - rest.len();
    let te = scan_while(s, ts, |c| !matches!(c, '`' | '(' | ')') && !is_js_ws(c));
    if te == ts || char_at(s, te) != Some(')') {
        return Err(le);
    }
    Ok(Link {
        start: i,
        end: te + 1,
        label: (ls, le),
        target: (ts, te),
    })
}

/// `[label]{@link target}` at `i`.
fn prefix_at(s: &str, i: usize) -> Attempt {
    let ls = i + 1;
    let le = scan_label(s, ls, &[']']);
    if le == ls {
        return Err(le.max(i + 1));
    }
    let Some(rest) = s.get(le..).and_then(|r| r.strip_prefix("]{@link")) else {
        return Err(le);
    };
    let w = s.len() - rest.len();
    let ts = skip_ws(s, w);
    if ts == w {
        return Err(le);
    }
    let te = scan_while(s, ts, |c| !matches!(c, '}' | '|') && !is_js_ws(c));
    if te == ts || char_at(s, te) != Some('}') {
        return Err(le);
    }
    Ok(Link {
        start: i,
        end: te + 1,
        label: (ls, le),
        target: (ts, te),
    })
}

/// `{@link target|label}` at `i` (spaces allowed around the `|`).
fn pipe_at(s: &str, i: usize) -> Attempt {
    let next = i + 1;
    let Some(rest) = s.get(i..).and_then(|r| r.strip_prefix("{@link")) else {
        return Err(next);
    };
    let w = s.len() - rest.len();
    let ts = skip_ws(s, w);
    if ts == w {
        return Err(next);
    }
    let te = scan_while(s, ts, |c| !matches!(c, '}' | '|') && !is_js_ws(c));
    if te == ts {
        return Err(next);
    }
    let bar = skip_ws(s, te);
    if char_at(s, bar) != Some('|') {
        return Err(next);
    }
    let ws_start = bar + 1;
    let ls = skip_ws(s, ws_start);
    let le = scan_label(s, ls, &['}']);
    if char_at(s, le) != Some('}') {
        return Err(le.max(next));
    }
    let label = if le > ls {
        (ls, le)
    } else {
        // The spaces before the `}` give their last one back to the label.
        match s[ws_start..ls].chars().next_back() {
            Some(c) if !is_nl(c) => (ls - c.len_utf8(), le),
            _ => return Err(le.max(next)),
        }
    };
    Ok(Link {
        start: i,
        end: le + 1,
        label,
        target: (ts, te),
    })
}

/// Every non-overlapping link of one form, left to right.
fn find_all(s: &str, form: Form) -> Vec<Link> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let open = if form == Form::Pipe { "{@link" } else { "[" };
        let Some(off) = s[i..].find(open) else { break };
        let at = i + off;
        let r = match form {
            Form::Markdown if not_escaped(s, at) => markdown_at(s, at),
            Form::Prefix if not_escaped(s, at) => prefix_at(s, at),
            Form::Pipe => pipe_at(s, at),
            _ => Err(at + 1),
        };
        match r {
            Ok(l) => {
                i = l.end;
                out.push(l);
            }
            Err(resume) => i = resume.max(at + 1),
        }
        while !s.is_char_boundary(i) {
            i += 1;
        }
    }
    out
}

fn remove_all(s: &str, form: Form) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    for l in find_all(s, form) {
        out.push_str(&s[pos..l.start]);
        pos = l.end;
    }
    out.push_str(&s[pos..]);
    out
}

/// The first of `stops` or a line break at or after `from`. `memo` keeps the last answer,
/// which still holds for any later `from` up to it, so rising `from`s scan the text once.
fn next_stop(
    s: &str,
    from: usize,
    stops: &[char],
    memo: &mut Option<(usize, Option<usize>)>,
) -> Option<usize> {
    if let Some((f, r)) = *memo
        && f <= from
        && r.is_none_or(|r| r >= from)
    {
        return r;
    }
    let r = s[from..]
        .find(|c: char| stops.contains(&c) || is_nl(c))
        .map(|o| from + o);
    *memo = Some((from, r));
    r
}

/// The `…AttemptRegex` tests: something left that still looks like a link.
fn looks_like_link(s: &str) -> bool {
    let (mut bracket, mut paren, mut brace, mut pipe) = (None, None, None, None);
    // From `from`, the text up to the first of `stops` or a line break reaches `close`,
    // or runs to the end of the text.
    let closes = |from: usize, close: char, memo: &mut _| match next_stop(s, from, &[close], memo) {
        None => true,
        Some(e) => s[e..].starts_with(close),
    };
    for (at, _) in s.match_indices('[') {
        if !not_escaped(s, at) {
            continue;
        }
        let close = next_stop(s, at + 1, &[']'], &mut bracket);
        let Some(close) = close.filter(|&c| s[c..].starts_with(']')) else {
            continue;
        };
        if s[close..].starts_with("](") && closes(close + 2, ')', &mut paren) {
            return true;
        }
        if s[close..].starts_with("]{@link") && closes(close + "]{@link".len(), '}', &mut brace) {
            return true;
        }
    }
    s.match_indices("{@link").any(|(at, _)| {
        let from = at + "{@link".len();
        // A `|` before the first `}` or line break.
        let bar = next_stop(s, from, &['|', '}'], &mut pipe);
        bar.is_some_and(|b| s[b..].starts_with('|')) && closes(from, '}', &mut brace)
    })
}

/// `^@[\w.\-]+\/[\w.\-]+`.
fn is_scoped_package(t: &str) -> bool {
    let ok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-');
    let Some(r) = t.strip_prefix('@') else {
        return false;
    };
    let a = r.len() - r.trim_start_matches(ok).len();
    if a == 0 {
        return false;
    }
    let Some(r) = r[a..].strip_prefix('/') else {
        return false;
    };
    r.starts_with(ok)
}

/// The content ranges of the markdown code spans of `s` (a backtick run, not after a
/// backslash, closed by the next run of the same length).
fn code_spans(s: &str) -> Vec<(usize, usize)> {
    let b = s.as_bytes();
    // Maximal backtick runs: (start, len).
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'`' {
            let st = i;
            while i < b.len() && b[i] == b'`' {
                i += 1;
            }
            runs.push((st, i - st));
        } else {
            i += 1;
        }
    }
    // Run indexes by length, for finding the next run of a given length.
    let mut by_len: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for (k, &(_, len)) in runs.iter().enumerate() {
        by_len.entry(len).or_default().push(k);
    }
    let next_of_len = |after: usize, n: usize| {
        let v = by_len.get(&n)?;
        let at = v.partition_point(|&k| k <= after);
        v.get(at).copied()
    };
    let mut out = Vec::new();
    let mut pos = 0;
    let mut r = 0;
    while r < runs.len() {
        let (st, len) = runs[r];
        if st + len <= pos {
            r += 1;
            continue;
        }
        let mut matched = None;
        for open in st.max(pos)..st + len {
            if open == st && !not_escaped(s, st) {
                continue;
            }
            let n = st + len - open;
            if let Some(k) = next_of_len(r, n) {
                matched = Some((open, n, runs[k].0, k));
                break;
            }
        }
        match matched {
            Some((open, n, cst, k)) => {
                out.push((open + n, cst));
                pos = cst + n;
                r = k + 1;
            }
            None => r += 1,
        }
    }
    out
}

fn inside_code_span(spans: &[(usize, usize)], at: usize) -> bool {
    spans.iter().any(|&(a, b)| at >= a && at < b)
}

/// `URL.canParse` for an `http:`/`https:` URL, approximately: a host is required, without
/// the forbidden host characters, with a numeric port up to 65535.
fn url_parses(url: &str) -> bool {
    let rest = url
        .strip_prefix("https:")
        .or_else(|| url.strip_prefix("http:"))
        .unwrap_or("");
    let rest = rest.trim_start_matches(['/', '\\']);
    let auth_end = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
    let auth = &rest[..auth_end];
    let host_port = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
    let (host, port) = if let Some(r) = host_port.strip_prefix('[') {
        let Some((h, after)) = r.split_once(']') else {
            return false;
        };
        if h.is_empty()
            || !h
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
        {
            return false;
        }
        match after {
            "" => ("ipv6", None),
            p => match p.strip_prefix(':') {
                Some(p) => ("ipv6", Some(p)),
                None => return false,
            },
        }
    } else {
        match host_port.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (host_port, None),
        }
    };
    if host.is_empty()
        || host.chars().any(|c| {
            c.is_control()
                || matches!(
                    c,
                    ' ' | '#'
                        | '%'
                        | '/'
                        | ':'
                        | '<'
                        | '>'
                        | '?'
                        | '@'
                        | '['
                        | '\\'
                        | ']'
                        | '^'
                        | '|'
                        | '\u{7f}'
                )
        })
    {
        return false;
    }
    match port {
        None | Some("") => true,
        Some(p) => {
            p.chars().all(|c| c.is_ascii_digit()) && p.parse::<u32>().is_ok_and(|n| n <= 65535)
        }
    }
}

/// The whole text as one `http:`/`https:` URL (`^https?:\S+$` and `URL.canParse`).
fn bare_url(text: &str) -> Option<&str> {
    let t = js_trim(text);
    let ok = (t.starts_with("http:") || t.starts_with("https:"))
        && t.len() > t.find(':').unwrap_or(0) + 1
        && !t.chars().any(is_js_ws);
    (ok && url_parses(t)).then_some(t)
}

fn encode_target(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let b = t.as_bytes();
    for (i, c) in t.char_indices() {
        match c {
            '%' => {
                let hex = b
                    .get(i + 1..i + 3)
                    .is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit));
                out.push_str(if hex { "%" } else { "%25" });
            }
            '|' => out.push_str("%7C"),
            '{' => out.push_str("%7B"),
            '}' => out.push_str("%7D"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_label(canonical: Form, label: &str, source: Form) -> String {
    let unescape: &[char] = match source {
        Form::Pipe => &['\\', '}'],
        Form::Prefix => &['\\', ']'],
        Form::Markdown => &['\\', '}', ']'],
    };
    let mut decoded = String::with_capacity(label.len());
    let mut it = js_trim(label).chars().peekable();
    while let Some(c) = it.next() {
        if c == '\\'
            && let Some(&n) = it.peek()
            && unescape.contains(&n)
        {
            decoded.push(n);
            it.next();
        } else {
            decoded.push(c);
        }
    }
    let close = if canonical == Form::Pipe { '}' } else { ']' };
    let chars: Vec<char> = decoded.chars().collect();
    let mut out = String::with_capacity(decoded.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c == '\\' {
            let next = chars.get(i + 1).copied();
            if next.is_none_or(|n| n == '\\' || n == close) {
                out.push('\\');
            }
        } else if c == close {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn format_link(canonical: Form, label: &str, source: Form, target: &str) -> String {
    let t = encode_target(target);
    let l = escape_label(canonical, label, source);
    match canonical {
        Form::Pipe => format!("{{@link {t}|{l}}}"),
        _ => format!("[{l}]{{@link {t}}}"),
    }
}

fn replace_links(s: &str, form: Form, canonical: Form) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    for l in find_all(s, form) {
        out.push_str(&s[pos..l.start]);
        let label = &s[l.label.0..l.label.1];
        let target = &s[l.target.0..l.target.1];
        out.push_str(&format_link(canonical, label, form, target));
        pos = l.end;
    }
    out.push_str(&s[pos..]);
    out
}

fn normalize(desc: &str, canonical: Form) -> String {
    let md = replace_links(desc, Form::Markdown, canonical);
    match canonical {
        Form::Pipe => replace_links(&md, Form::Prefix, canonical),
        _ => replace_links(&md, Form::Pipe, canonical),
    }
}

/// The non-canonical labelled form `descriptionMatcher` looks for, besides markdown.
fn other_form(canonical: Form) -> Form {
    if canonical == Form::Pipe {
        Form::Prefix
    } else {
        Form::Pipe
    }
}

impl NormalizeSeeLinks {
    fn has_noncanonical_form(&self, s: &str) -> bool {
        !find_all(s, Form::Markdown).is_empty()
            || !find_all(s, other_form(self.canonical)).is_empty()
    }

    fn fix_tag(&self, b: &mut Block, tag: usize, bare: Option<&str>) {
        let first = b.tags[tag].source[0];
        let t = &mut b.lines[first].tokens;
        if !t.name.is_empty() {
            t.description = format!("{}{}{}", t.name, t.post_name, t.description);
            t.name.clear();
            t.post_name.clear();
        }
        let lines = b.tags[tag].source.clone();
        if let Some(url) = bare {
            for l in lines {
                let d = &b.lines[l].tokens.description;
                if let Some(at) = d.find(url) {
                    let new = format!(
                        "{}{{@link {}}}{}",
                        &d[..at],
                        encode_target(url),
                        &d[at + url.len()..]
                    );
                    b.lines[l].tokens.description = new;
                    break;
                }
            }
            return;
        }
        for l in lines {
            let d = &b.lines[l].tokens.description;
            if self.has_noncanonical_form(d) {
                let new = normalize(d, self.canonical);
                b.lines[l].tokens.description = new;
            }
        }
    }

    fn check(&self, cx: &mut Ctx<'_, '_>, f: &crate::jsdoc::Finder<'_>, doc: &mut Doc) {
        let mut fixable: Vec<(usize, Option<String>)> = Vec::new();
        for i in 0..doc.block.tags.len() {
            if doc.block.tags[i].tag != "see" {
                continue;
            }
            let b = &doc.block;
            let first = &b.lines[b.tags[i].source[0]].tokens;
            let td = tag_description(b, i);
            let raw = if first.name.is_empty() {
                td
            } else {
                format!("{}{}{td}", first.name, first.post_name)
            };
            let mut ambiguous = false;
            let mut noncanonical = false;
            let bare = if self.wrap_bare_urls {
                bare_url(&raw).map(str::to_owned)
            } else {
                None
            };
            if bare.is_some() {
                noncanonical = true;
            }
            let spans = code_spans(&raw);
            for l in find_all(&raw, Form::Markdown) {
                let label = &raw[l.label.0..l.label.1];
                let target = &raw[l.target.0..l.target.1];
                if inside_code_span(&spans, l.start)
                    || is_scoped_package(target)
                    || js_trim(label).is_empty()
                {
                    ambiguous = true;
                } else {
                    noncanonical = true;
                }
            }
            for it in inline_tags(&raw) {
                if it.tag != "link" || it.text.is_empty() {
                    continue;
                }
                if it.format == "space" || js_trim(it.text).is_empty() {
                    ambiguous = true;
                    continue;
                }
                if it.format == form_name(self.canonical) {
                    continue;
                }
                if inside_code_span(&spans, it.start) || is_scoped_package(it.namepath) {
                    ambiguous = true;
                } else {
                    noncanonical = true;
                }
            }
            let left = remove_all(&raw, Form::Markdown);
            let left = remove_all(&left, Form::Prefix);
            let left = remove_all(&left, Form::Pipe);
            ambiguous |= looks_like_link(&left);
            let rel = doc.block.tag_line(i);
            if ambiguous {
                let (span, loc) = doc.report_at(f, rel, None);
                cx.report(span, "@see link cannot be safely normalized.")
                    .loc(loc);
                continue;
            }
            if noncanonical {
                fixable.push((i, bare));
            }
        }
        let Some(&(first_tag, _)) = fixable.first() else {
            return;
        };
        let mut fix = None;
        if self.enable_fixer {
            let mut b = doc.block.clone();
            for (i, bare) in &fixable {
                self.fix_tag(&mut b, *i, bare.as_deref());
            }
            std::mem::swap(&mut doc.block, &mut b);
            fix = Some(doc.fix());
            std::mem::swap(&mut doc.block, &mut b);
        }
        for (i, bare) in &fixable {
            let msg = match bare {
                Some(_) => "Expected bare @see URL to use a {@link} tag.".to_owned(),
                None => format!(
                    "Expected @see link to use the {} form.",
                    form_name(self.canonical)
                ),
            };
            let (span, loc) = doc.report_at(f, doc.block.tag_line(*i), None);
            let r = cx.report(span, msg).loc(loc);
            if *i == first_tag
                && let Some(fx) = fix.take()
            {
                r.fix(fx);
            }
        }
    }
}

impl Rule for NormalizeSeeLinks {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, f, doc| self.check(cx, f, doc));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_spans_pair_runs_of_equal_length() {
        assert_eq!(code_spans("a `b` c"), vec![(3, 4)]);
        assert_eq!(code_spans("``a`b``"), vec![(2, 5)]);
        // The escaped run is skipped; the next one opens a span over the space.
        assert_eq!(code_spans("\\`a` `b`"), vec![(4, 5)]);
        assert!(code_spans("`unclosed").is_empty());
    }

    #[test]
    fn link_scans_stay_linear_on_unclosed_brackets() {
        let s = "[".repeat(20_000);
        assert!(find_all(&s, Form::Markdown).is_empty());
        assert!(find_all(&s, Form::Prefix).is_empty());
        let p = "{@link x|".repeat(5_000);
        assert!(find_all(&p, Form::Pipe).is_empty());
        assert!(code_spans(&"`".repeat(10_000)).is_empty());
        let t = std::time::Instant::now();
        assert!(!looks_like_link(&format!("{}\n", "[".repeat(200_000))));
        assert!(!looks_like_link(&format!("{}\n", "[](".repeat(100_000))));
        assert!(!looks_like_link(&format!("{}\n", "{@link".repeat(100_000))));
        assert!(!looks_like_link(&format!(
            "{}\n",
            "{@link|".repeat(100_000)
        )));
        assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
    }

    #[test]
    fn labels_are_re_escaped() {
        assert_eq!(
            format_link(Form::Pipe, "a}b", Form::Markdown, "https://x/a|b"),
            "{@link https://x/a%7Cb|a\\}b}"
        );
        assert_eq!(
            format_link(Form::Prefix, "A]B\\}C", Form::Pipe, "https://x"),
            "[A\\]B}C]{@link https://x}"
        );
    }
}
