//! The file references inside a stylesheet: `url(...)` and the candidates of `image-set(...)`
//! (`-webkit-image-set(...)` too). [`rewrite_refs`] hands each one to a callback and puts
//! back what it returns, keeping everything else byte for byte.
//!
//! A scanner, not a parser. Comments, strings and `@import` statements are skipped (an
//! `@import url(...)` is the inliner's business, see [`crate::css`]). External URLs (`//x`,
//! `https://x`), `data:` URLs, `#fragment` references and function calls (`url(var(--x))`)
//! never reach the callback. In `image-set()` a candidate may be a bare string
//! (`'a.png' 1x`); a rewritten one becomes a `url()` call. The new value is quoted when the
//! old one was, and gains double quotes when it needs them (spaces, non-ASCII, `)`).
//!
//! Vite's CSS plugin behaves this way (`vite: packages/vite/src/node/plugins/css.ts`,
//! `rewriteCssUrls`, `rewriteCssImageSet` and `doUrlReplace`).

/// `css` with each reference the callback maps to `Some(new_url)` replaced. The callback
/// gets the reference unquoted, with CSS escapes (`\(`) removed.
pub fn rewrite_refs(css: &str, map: &mut dyn FnMut(&str) -> Option<String>) -> String {
    let b = css.as_bytes();
    let mut out = String::new();
    let mut copied = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = css[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
            }
            q @ (b'"' | b'\'') => i = skip_string(b, i, q),
            b'\\' => i += 2,
            b'@' if starts_with_ci(&b[i + 1..], b"import")
                && !b.get(i + 7).is_some_and(|&c| is_ident_byte(c)) =>
            {
                i = statement_end(b, i);
            }
            _ if at_function(b, i, b"url(") => {
                let Some((end, raw)) = url_token(css, i + 4) else {
                    i += 4;
                    continue;
                };
                if let Some(new) = replace(raw, map) {
                    out.push_str(&css[copied..i]);
                    out.push_str(&new);
                    copied = end;
                }
                i = end;
            }
            _ if image_set_args(b, i).is_some() => {
                let args = image_set_args(b, i).unwrap_or(i + 1);
                i = image_set(css, args, map, &mut out, &mut copied);
            }
            _ => i += 1,
        }
    }
    if copied == 0 {
        return css.to_owned();
    }
    out.push_str(&css[copied.min(css.len())..]);
    out
}

/// Walks the candidates of an `image-set(` whose arguments start at `at`; returns the index
/// past its `)`.
fn image_set(
    css: &str,
    at: usize,
    map: &mut dyn FnMut(&str) -> Option<String>,
    out: &mut String,
    copied: &mut usize,
) -> usize {
    let b = css.as_bytes();
    let mut i = at;
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let Some(&c) = b.get(i) else {
            return b.len();
        };
        // The candidate's image: a string or `url()` (rewritten); anything else is copied.
        let token = match c {
            b'"' | b'\'' => {
                let end = skip_string(b, i, c);
                Some((end, &css[i..end]))
            }
            _ if at_function(b, i, b"url(") => url_token(css, i + 4),
            _ => None,
        };
        if let Some((end, raw)) = token {
            if let Some(new) = replace(raw, map) {
                out.push_str(&css[*copied..i]);
                out.push_str(&new);
                *copied = end;
            }
            i = end;
        }
        // The rest of the candidate (resolution, `type(...)`) up to `,` or the closing `)`.
        let mut depth = 0usize;
        loop {
            let Some(&c) = b.get(i) else {
                return b.len();
            };
            match c {
                q @ (b'"' | b'\'') => {
                    i = skip_string(b, i, q);
                    continue;
                }
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    i = css[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
                    continue;
                }
                b'\\' => i += 1,
                b'(' => depth += 1,
                b')' if depth == 0 => return i + 1,
                b')' => depth -= 1,
                b',' if depth == 0 => {
                    i += 1;
                    break;
                }
                _ => {}
            }
            i += 1;
        }
    }
}

/// The reference with its new value (`url("...")`), or `None` to keep it as written.
fn replace(raw: &str, map: &mut dyn FnMut(&str) -> Option<String>) -> Option<String> {
    let (mut wrap, inner) = match raw.as_bytes() {
        [q @ (b'"' | b'\''), .., last] if last == q => (*q as char, &raw[1..raw.len() - 1]),
        [b'"' | b'\'', ..] => return None,
        _ => ('\0', raw),
    };
    if skipped(inner) {
        return None;
    }
    let mut new = map(&unescape(inner))?;
    if wrap == '\0' && (needs_encoding(&new) || new.contains(')')) {
        wrap = '"';
    }
    if wrap == '\'' && new.contains('\'') {
        wrap = '"';
    }
    if wrap == '"' && new.contains('"') {
        new = escape_double_quotes(&new);
    }
    Some(match wrap {
        '\0' => format!("url({new})"),
        q => format!("url({q}{new}{q})"),
    })
}

/// References left alone: external, `data:`, fragment-only and function calls.
fn skipped(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    let scheme_len = lower.bytes().take_while(u8::is_ascii_alphabetic).count();
    let external = lower[scheme_len..].starts_with("://") || lower.starts_with("//");
    let call = lower
        .as_bytes()
        .first()
        .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
        && lower
            .bytes()
            .position(|c| !(c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-')))
            .is_some_and(|p| lower.as_bytes()[p] == b'(');
    url.is_empty() || external || lower.starts_with("data:") || url.starts_with('#') || call
}

/// Removes CSS escapes of non-word characters (`\(` → `(`).
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(&n) = chars.peek()
            && !(n.is_ascii_alphanumeric() || n == '_')
        {
            out.push(n);
            chars.next();
            continue;
        }
        out.push(c);
    }
    out
}

fn escape_double_quotes(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    let mut prev = '\0';
    for c in s.chars() {
        if c == '"' && prev != '\\' {
            out.push('\\');
        }
        out.push(c);
        prev = c;
    }
    out
}

/// Whether percent-encoding the URL (as JS `encodeURI` does) would change it.
fn needs_encoding(s: &str) -> bool {
    s.bytes()
        .any(|c| !(c.is_ascii_alphanumeric() || b";,/?:@&=+$-_.!~*'()#".contains(&c)))
}

/// At `url(`, `image-set(` ... (ASCII case-insensitive), not inside a longer identifier.
fn at_function(b: &[u8], i: usize, name: &[u8]) -> bool {
    starts_with_ci(&b[i..], name) && (i == 0 || !is_ident_byte(b[i - 1]))
}

/// The index after the `(` of an `image-set(` or `-webkit-image-set(` at `i`.
fn image_set_args(b: &[u8], i: usize) -> Option<usize> {
    if at_function(b, i, b"image-set(") {
        return Some(i + 10);
    }
    at_function(b, i, b"-webkit-image-set(").then_some(i + 18)
}

/// After `url(` at `open`: the index past `)` and the value (quotes kept, whitespace
/// trimmed). `None` when malformed (unterminated, or text after a quoted value).
fn url_token(css: &str, open: usize) -> Option<(usize, &str)> {
    let b = css.as_bytes();
    let mut i = open;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    let start = i;
    match b.get(i)? {
        &q @ (b'"' | b'\'') => {
            let end = skip_string(b, i, q);
            let mut j = end;
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            (b.get(j) == Some(&b')')).then(|| (j + 1, &css[start..end]))
        }
        _ => {
            while i < b.len() {
                match b[i] {
                    b'\\' => i += 2,
                    b')' => return Some((i + 1, css[start..i].trim_end())),
                    b'"' | b'\'' | b'(' => return None,
                    _ => i += 1,
                }
            }
            None
        }
    }
}

pub(crate) fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c >= 0x80
}

pub(crate) fn starts_with_ci(hay: &[u8], needle: &[u8]) -> bool {
    hay.len() >= needle.len() && hay[..needle.len()].eq_ignore_ascii_case(needle)
}

/// Index just past a quoted string starting at `i` (or past the line end if unterminated).
pub(crate) fn skip_string(b: &[u8], i: usize, q: u8) -> usize {
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            c if c == q || c == b'\n' => return j + 1,
            _ => j += 1,
        }
    }
    b.len()
}

/// Index just past the `;` ending the at-rule starting at `i` (strings and parens skipped).
pub(crate) fn statement_end(b: &[u8], i: usize) -> usize {
    let mut j = i;
    let mut depth = 0usize;
    while j < b.len() {
        match b[j] {
            q @ (b'"' | b'\'') => {
                j = skip_string(b, j, q);
                continue;
            }
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b';' if depth == 0 => return j + 1,
            b'{' | b'}' if depth == 0 => return j,
            _ => {}
        }
        j += 1;
    }
    b.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upper(css: &str) -> String {
        rewrite_refs(css, &mut |u| Some(format!("/base/{u}")))
    }

    #[test]
    fn rewrites_url_calls_keeping_quotes() {
        assert_eq!(upper("a{b:url(x.png)}"), "a{b:url(/base/x.png)}");
        assert_eq!(upper("a{b:url( 'x.png' )}"), "a{b:url('/base/x.png')}");
        assert_eq!(upper(r#"a{b:URL("x.png")}"#), r#"a{b:url("/base/x.png")}"#);
    }

    #[test]
    fn skips_comments_strings_imports_and_external() {
        let css = "/* url(a.png) */ a{content:'url(b.png)'} @import url(c.css); \
                   d{e:url(http://x/y.png) url(//x/y) url(data:image/png,1) url(#f) url(var(--x))}";
        assert_eq!(upper(css), css);
        // `my-url(` is a different function.
        assert_eq!(upper("a{b:my-url(x)}"), "a{b:my-url(x)}");
    }

    #[test]
    fn image_set_strings_become_url_calls() {
        assert_eq!(
            upper(
                "a{b:-webkit-image-set('x.png' 1x, url(y.png) 2x, linear-gradient(red, blue) 3x)}"
            ),
            "a{b:-webkit-image-set(url('/base/x.png') 1x, url(/base/y.png) 2x, linear-gradient(red, blue) 3x)}"
        );
        assert_eq!(
            upper("a{b:image-set('x.png' type('image/png') 1x, var(--v) 2x)}"),
            "a{b:image-set(url('/base/x.png') type('image/png') 1x, var(--v) 2x)}"
        );
    }

    #[test]
    fn quotes_what_needs_quoting() {
        let mut map = |_: &str| Some("/a b(1).png".to_owned());
        assert_eq!(rewrite_refs("url(x)", &mut map), "url(\"/a b(1).png\")");
        let mut map = |_: &str| Some("data:image/svg+xml,<svg a='1' b=\"2\">".to_owned());
        assert_eq!(
            rewrite_refs("url('x')", &mut map),
            "url(\"data:image/svg+xml,<svg a='1' b=\\\"2\\\">\")"
        );
    }

    #[test]
    fn unescapes_and_keeps_unmapped() {
        let mut seen = Vec::new();
        let out = rewrite_refs(r"url(a\(1\).png) url(keep.png)", &mut |u| {
            seen.push(u.to_owned());
            (u != "keep.png").then(|| "/n.png".to_owned())
        });
        assert_eq!(seen, ["a(1).png", "keep.png"]);
        assert_eq!(out, "url(/n.png) url(keep.png)");
    }

    #[test]
    fn malformed_input_terminates() {
        for css in ["url(", "url('a", "url('a' b)", "image-set(", "/*", "'"] {
            assert_eq!(upper(css), css);
        }
        assert_eq!(upper("image-set('a' 1x,"), "image-set(url('/base/a') 1x,");
    }
}
