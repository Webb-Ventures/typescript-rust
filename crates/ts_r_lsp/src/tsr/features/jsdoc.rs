//! JSDoc comments: a small parser for `/** ... */` text and tsc 7's markdown rendering of
//! documentation in hover.
//!
//! Rendering rules from `tsc: tsc/internal/ls/hover.go` (`getDocumentationFromDeclaration`,
//! `writeComments`, `writeJSDocLink`), v7.0.2, in our words: the comment text, then for
//! every tag except `@type`, `@typedef` and `@callback` a paragraph `*@tag*`, the tag's
//! name (`` `x` `` of `@param x`, the class of `@augments`, the comma-separated names of
//! `@template`) and its comment after ` — ` (no dash when the comment starts with `-`).
//! Type expressions (`{number}`) are not shown, except `@throws`'s. `@example` is a `tsx`
//! code block (a caption after ` — `). `@see name` links the name. `{@link x text}`
//! becomes a markdown link to `x`'s declaration (`file#line,col-line,col`, 1-based), a URL
//! link, or plain `x text` when `x` does not resolve; `{@linkcode}` quotes in backticks.

/// One piece of comment text.
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    Text(String),
    /// `{@link name text}`; `code` for `@linkcode`.
    Link {
        name: Option<String>,
        text: String,
        code: bool,
    },
}

/// One `@tag`.
#[derive(Clone, Debug, Default)]
pub struct Tag {
    pub name: String,
    /// The name a tag documents: a parameter, property, template parameters joined by
    /// `,`, the class of `@augments`, the name of `@see`, the type text of `@throws`.
    pub target: Option<String>,
    /// For `@template`, each name.
    pub names: Vec<String>,
    pub comment: Vec<Part>,
    /// The comment as written (for `@example`).
    pub raw: String,
}

/// A parsed `/** ... */` comment.
#[derive(Clone, Debug, Default)]
pub struct JsDoc {
    pub comment: Vec<Part>,
    pub tags: Vec<Tag>,
}

impl JsDoc {
    /// Whether it declares a type (`@typedef`, `@callback`): its text documents that type.
    pub fn declares_type(&self) -> bool {
        self.tags
            .iter()
            .any(|t| t.name == "typedef" || t.name == "callback")
    }

    /// The `@param` tag for parameter `name`.
    pub fn param(&self, name: &str) -> Option<&Tag> {
        self.tags.iter().find(|t| {
            matches!(t.name.as_str(), "param" | "arg" | "argument")
                && t.target.as_deref() == Some(name)
        })
    }

    /// The `index`-th `@param` tag.
    pub fn param_at(&self, index: usize) -> Option<&Tag> {
        self.tags
            .iter()
            .filter(|t| matches!(t.name.as_str(), "param" | "arg" | "argument"))
            .nth(index)
    }

    /// The `@template` tag naming `name`.
    pub fn template(&self, name: &str) -> Option<&Tag> {
        self.tags
            .iter()
            .find(|t| t.name == "template" && t.names.iter().any(|n| n == name))
    }
}

/// The lines of a comment without `/**`, `*/` and the leading `*` margin.
fn lines(raw: &str) -> Vec<String> {
    let body = raw.strip_prefix("/**").unwrap_or(raw);
    let body = body.strip_suffix("*/").unwrap_or(body);
    let mut out = Vec::new();
    let mut margin: Option<usize> = None;
    for (i, line) in body.split('\n').enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if i == 0 {
            out.push(line.trim().to_string());
            continue;
        }
        let t = line.trim_start();
        let (t, starred) = match t.strip_prefix('*') {
            Some(r) => (r, true),
            None => (t, false),
        };
        if !starred {
            out.push(t.trim_end().to_string());
            continue;
        }
        let spaces = t.len() - t.trim_start_matches([' ', '\t']).len();
        if margin.is_none() && !t.trim().is_empty() {
            margin = Some(spaces.min(1));
        }
        let cut = spaces.min(margin.unwrap_or(1));
        out.push(t[cut..].trim_end().to_string());
    }
    out
}

/// The tag name after an `@` at byte `i` of `text`, when the `@` starts a tag (is
/// followed by an identifier start).
fn tag_name_at(text: &str, i: usize) -> Option<&str> {
    let rest = &text[i + 1..];
    match rest.chars().next() {
        Some(ch) if ch.is_alphabetic() || ch == '_' || ch == '$' => {}
        _ => return None,
    }
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$' || c == '-'))
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Parses a `/** ... */` comment. A tag starts at every `@` followed by a name, except
/// in fenced code blocks, inside `{@link ...}` and inside `@author`'s `<email>`.
pub fn parse(raw: &str) -> JsDoc {
    let text = lines(raw).join("\n");
    let b = text.as_bytes();
    let mut doc = JsDoc::default();
    // (tag name, start of its body, end).
    let mut pieces: Vec<(Option<String>, usize, usize)> = Vec::new();
    let mut cur: (Option<String>, usize) = (None, 0);
    let mut i = 0;
    let mut in_fence = false;
    let mut in_code = false;
    let mut line_start = true;
    while i < b.len() {
        let ch = b[i];
        if line_start && text[i..].trim_start_matches([' ', '\t']).starts_with("```") {
            in_fence = !in_fence;
        }
        line_start = ch == b'\n';
        if ch == b'\n' {
            in_code = false;
        } else if ch == b'`' && !in_fence {
            in_code = !in_code;
        }
        if in_fence || in_code {
            i += 1;
            continue;
        }
        if ch == b'{' && text[i + 1..].starts_with("@link") {
            i = text[i..].find('}').map_or(b.len(), |e| i + e + 1);
            continue;
        }
        if ch == b'<'
            && cur.0.as_deref() == Some("author")
            && let Some(e) = text[i..].find(['>', '\n'])
        {
            i += e + 1;
            continue;
        }
        if ch == b'@'
            && let Some(name) = tag_name_at(&text, i)
        {
            pieces.push((cur.0.take(), cur.1, i));
            cur = (Some(name.to_string()), i + 1 + name.len());
            i = cur.1;
            continue;
        }
        i += 1;
    }
    pieces.push((cur.0, cur.1, b.len()));
    for (name, start, end) in pieces {
        let body = text[start..end].trim();
        match name {
            None => doc.comment = parts(body),
            Some(n) => doc.tags.push(tag(&n, body)),
        }
    }
    doc
}

/// A balanced `{...}` at the start of `s` (after spaces): its text and the rest.
fn braced(s: &str) -> Option<(&str, &str)> {
    let t = s.trim_start();
    if !t.starts_with('{') {
        return None;
    }
    let mut depth = 0usize;
    // Where an inner `{...}` closed, for a type whose own `}` is missing.
    let mut inner_end = None;
    for (i, ch) in t.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&t[..=i], &t[i + 1..]));
                }
                if depth == 1 && inner_end.is_none() {
                    inner_end = Some(i + 1);
                }
            }
            _ => {}
        }
    }
    match inner_end {
        Some(e) => Some((&t[..e], &t[e..])),
        None => Some((t, "")),
    }
}

/// An entity name (`a`, `a.b`, `A#m`) at the start of `s`, and the rest.
fn entity(s: &str) -> (&str, &str) {
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '$' | '.' | '#')))
        .unwrap_or(s.len());
    let mut e = end;
    while e > 0 && s[..e].ends_with(['.', '#']) {
        e -= 1;
    }
    (&s[..e], &s[e..])
}

fn tag(name: &str, body: &str) -> Tag {
    let mut t = Tag {
        name: name.to_string(),
        raw: body.to_string(),
        ..Tag::default()
    };
    let mut rest = body;
    match name {
        "param" | "arg" | "argument" | "property" | "prop" => {
            if let Some((_, r)) = braced(rest) {
                rest = r;
            }
            let r = rest.trim_start();
            let (n, r) = if let Some(inner) = r.strip_prefix('[') {
                let close = inner.find(']').unwrap_or(inner.len());
                let n = inner[..close].split('=').next().unwrap_or("").trim();
                (n, inner.get(close + 1..).unwrap_or(""))
            } else {
                let end = r
                    .find(|c: char| c.is_whitespace() || c == '{')
                    .unwrap_or(r.len());
                (&r[..end], &r[end..])
            };
            // A missing name prints as an empty one.
            t.target = Some(n.to_string());
            rest = r;
            if let Some((_, r)) = braced(rest) {
                rest = r;
            }
        }
        "returns" | "return" | "type" | "this" | "satisfies" | "typedef" | "callback" => {
            if let Some((_, r)) = braced(rest) {
                rest = r;
            }
        }
        "throws" | "exception" => {
            if let Some((ty, r)) = braced(rest) {
                t.target = Some(ty.to_string());
                rest = r;
            }
        }
        "template" => {
            if let Some((_, r)) = braced(rest) {
                rest = r;
            }
            let mut r = rest.trim_start();
            loop {
                let (n, after) = entity(r);
                if n.is_empty() {
                    break;
                }
                t.names.push(n.to_string());
                let a = after.trim_start();
                match a.strip_prefix(',') {
                    Some(x) => r = x.trim_start(),
                    None => {
                        r = after;
                        break;
                    }
                }
            }
            rest = r;
        }
        "augments" | "extends" | "implements" => {
            let (inner, r) = match braced(rest) {
                Some((b, r)) => (b.trim_start_matches('{').trim_end_matches('}'), r),
                None => (rest.trim_start(), ""),
            };
            let (n, after) = entity(inner.trim_start());
            if !n.is_empty() {
                t.target = Some(n.replace('#', "."));
            }
            rest = if r.is_empty() && braced(rest).is_none() {
                after.trim_start_matches(|c: char| c != ' ' && c != '\n')
            } else {
                r
            };
        }
        "see" => {
            let r = rest.trim_start();
            let url = r.starts_with("http://") || r.starts_with("https://");
            if !r.starts_with('{') && !url {
                let (n, after) = entity(r);
                if !n.is_empty() {
                    t.target = Some(n.to_string());
                    rest = after;
                }
            }
        }
        _ => {}
    }
    let comment = rest.trim();
    t.comment = parts(comment);
    t
}

/// Comment text split into text and `{@link}` parts.
pub fn parts(s: &str) -> Vec<Part> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("{@link") {
        let after = &rest[i + 6..];
        let (code, after) = if let Some(a) = after.strip_prefix("code") {
            (true, a)
        } else if let Some(a) = after.strip_prefix("plain") {
            (false, a)
        } else {
            (false, after)
        };
        if !(after.starts_with(char::is_whitespace) || after.starts_with('}')) {
            text.push_str(&rest[..i + 6]);
            rest = &rest[i + 6..];
            continue;
        }
        let Some(close) = after.find('}') else {
            break;
        };
        text.push_str(&rest[..i]);
        if !text.is_empty() {
            out.push(Part::Text(std::mem::take(&mut text)));
        }
        let inner = after[..close].trim_start();
        let (n, t) = entity(inner);
        out.push(Part::Link {
            name: (!n.is_empty()).then(|| n.to_string()),
            text: t.trim_matches(' ').to_string(),
            code,
        });
        rest = &after[close + 1..];
    }
    text.push_str(rest);
    if !text.is_empty() {
        out.push(Part::Text(text));
    }
    out
}

/// Where a link name leads: a markdown URI, or `None` when it does not resolve.
pub type Resolve<'r> = dyn FnMut(&str) -> Option<String> + 'r;

fn quoted(out: &mut String, s: &str, quote: bool) {
    if quote && !s.contains('`') {
        out.push('`');
        out.push_str(s);
        out.push('`');
    } else {
        out.push_str(s);
    }
}

fn trim_comment_prefix(s: &str) -> &str {
    let s = s.trim_start_matches(' ');
    s.strip_prefix('|').unwrap_or(s).trim_start_matches(' ')
}

fn markdown_link(out: &mut String, text: &str, uri: &str, quote: bool) {
    out.push('[');
    quoted(out, text, quote);
    out.push_str("](");
    out.push_str(uri);
    out.push(')');
}

/// A link to `name` with `text` (`@see name`, `{@link name text}`).
fn name_link(out: &mut String, name: &str, text: &str, quote: bool, resolve: &mut Resolve<'_>) {
    let shown = name.replace('#', ".");
    if let Some(uri) = resolve(name) {
        let prefix = if text.starts_with("()") { 2 } else { 0 };
        let mut link = trim_comment_prefix(&text[prefix..]).to_string();
        if link.is_empty() {
            link = format!("{shown}{}", &text[..prefix]);
        }
        markdown_link(out, &link, &uri, quote);
        return;
    }
    let s = if text.is_empty() {
        shown
    } else {
        format!("{shown} {text}")
    };
    quoted(out, &s, quote);
}

/// Comment parts as markdown.
pub fn write_parts(out: &mut String, parts: &[Part], resolve: &mut Resolve<'_>) {
    for p in parts {
        match p {
            Part::Text(t) => out.push_str(t),
            Part::Link {
                name: None,
                text,
                code,
            } => quoted(out, text, *code),
            Part::Link {
                name: Some(n),
                text,
                code,
            } => {
                if (n == "http" || n == "https") && text.starts_with("://") {
                    let full = format!("{n}{text}");
                    let (uri, label) = match full.find([' ', '|']) {
                        Some(i) => {
                            let label = trim_comment_prefix(&full[i..]);
                            (
                                &full[..i],
                                if label.is_empty() { &full[..i] } else { label },
                            )
                        }
                        None => (full.as_str(), full.as_str()),
                    };
                    markdown_link(out, label, uri, *code);
                } else {
                    name_link(out, n, text, *code, resolve);
                }
            }
        }
    }
}

/// A code block for markdown, with enough backticks to hold `code`.
fn code_block(out: &mut String, lang: &str, code: &str) {
    if code.is_empty() {
        return;
    }
    let mut ticks = 3;
    while code.contains(&"`".repeat(ticks)) {
        ticks += 1;
    }
    let fence = "`".repeat(ticks);
    out.push_str(&fence);
    out.push_str(lang);
    out.push('\n');
    out.push_str(code);
    out.push('\n');
    out.push_str(&fence);
    out.push('\n');
}

/// Documentation and tags as markdown (tsc's hover documentation).
pub fn render(doc: &JsDoc, resolve: &mut Resolve<'_>) -> String {
    let mut out = String::new();
    write_parts(&mut out, &doc.comment, resolve);
    // Nested `@param` names (`options.a` after `options`) belong to their parent.
    let mut seen: Vec<String> = Vec::new();
    for t in &doc.tags {
        if matches!(t.name.as_str(), "type" | "typedef" | "callback") {
            continue;
        }
        let is_param = matches!(
            t.name.as_str(),
            "param" | "arg" | "argument" | "property" | "prop"
        );
        if is_param && let Some(n) = &t.target {
            if let Some((root, _)) = n.split_once('.')
                && seen.iter().any(|s| s == root)
            {
                continue;
            }
            seen.push(n.clone());
        }
        out.push_str("\n\n*@");
        out.push_str(&t.name);
        out.push('*');
        match t.name.as_str() {
            "param" | "arg" | "argument" | "property" | "prop" | "augments" | "extends"
            | "implements" => {
                if let Some(n) = &t.target {
                    out.push(' ');
                    quoted(&mut out, n, true);
                }
            }
            "template" => {
                for (i, n) in t.names.iter().enumerate() {
                    if i != 0 {
                        out.push(',');
                    }
                    out.push(' ');
                    quoted(&mut out, n, true);
                }
            }
            _ => {}
        }
        if t.name == "example" {
            let mut text = t.raw.as_str();
            if let Some(r) = text.strip_prefix("<caption>")
                && let Some(end) = r.find("</caption>")
            {
                out.push_str(" — ");
                out.push_str(&r[..end]);
                text = r[end + "</caption>".len()..].trim_start();
            }
            out.push('\n');
            if text.len() > 6
                && text.starts_with("```")
                && text.ends_with("```")
                && text.contains('\n')
            {
                out.push_str(text);
                out.push('\n');
            } else {
                code_block(&mut out, "tsx", text);
            }
        } else if t.name == "see"
            && let Some(n) = &t.target
        {
            out.push_str(" — ");
            name_link(&mut out, n, "", false, resolve);
            if !t.comment.is_empty() {
                out.push(' ');
                write_parts(&mut out, &t.comment, resolve);
            }
        } else if matches!(t.name.as_str(), "throws" | "exception")
            && let Some(ty) = &t.target
        {
            out.push_str(" — ");
            out.push_str(ty);
            if !t.comment.is_empty() {
                out.push(' ');
                write_parts(&mut out, &t.comment, resolve);
            }
        } else if !t.comment.is_empty() {
            out.push(' ');
            if !matches!(t.comment.first(), Some(Part::Text(s)) if s.starts_with('-')) {
                out.push_str("— ");
            }
            write_parts(&mut out, &t.comment, resolve);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(raw: &str) -> String {
        render(&parse(raw), &mut |_| None)
    }

    #[test]
    fn tags_render_like_tsc() {
        assert_eq!(
            md(
                "/**\n * Adds.\n *\n * @param a - first\n * @param {number} b second\n * @returns {number} sum\n */"
            ),
            "Adds.\n\n*@param* `a` - first\n\n*@param* `b` — second\n\n*@returns* — sum"
        );
        assert_eq!(md("/** One line. */"), "One line.");
        // Types are not shown; `@type` is dropped; nested params belong to their parent.
        assert_eq!(md("/** @type {number} */"), "");
        assert_eq!(
            md(
                "/**\n * @param {Object} o the args\n * @param {number} o.a first\n * @template T1,T2 Text\n * @this {number}\n */"
            ),
            "\n\n*@param* `o` — the args\n\n*@template* `T1`, `T2` — Text\n\n*@this*"
        );
        assert_eq!(
            md("/** @param {{a: {b: 1}}} [x=1] Desc */"),
            "\n\n*@param* `x` — Desc"
        );
    }

    #[test]
    fn links_and_examples() {
        assert_eq!(
            md("/** {@link https://vscode.dev|text} {@link unknown more} {@linkcode A#b} */"),
            "[text](https://vscode.dev) unknown more `A.b`"
        );
        let r = render(&parse("/** See {@link A()} */"), &mut |n| {
            (n == "A").then(|| "file:///a.ts#1,1-1,2".to_string())
        });
        assert_eq!(r, "See [A()](file:///a.ts#1,1-1,2)");
        assert_eq!(
            md("/**\n * @example\n * f(1)\n */"),
            "\n\n*@example*\n```tsx\nf(1)\n```\n"
        );
        assert!(parse("/** @typedef {number} N */").declares_type());
    }
}
