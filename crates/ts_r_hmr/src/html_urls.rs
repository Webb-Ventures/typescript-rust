//! A page's references as the dev server serves it: file URLs in asset attributes get the
//! base, and its CSS (`<style>` blocks, `style` attributes) is transformed like app CSS.
//!
//! The rules (Vite's dev HTML hook, `vite: packages/vite/src/node/server/middlewares/indexHtml.ts`,
//! `processNodeUrl` and `devHtmlHook`, and the attribute table in
//! `packages/vite/src/node/assetSource.ts`):
//!
//! - The attributes that name files: `src` of `audio`, `embed`, `img`, `input`, `script`,
//!   `source`, `track` and `video`; `srcset` of `img` and `source`; `href` of `link`, `image`
//!   and `use` (and `xlink:href` of the last two); `imagesrcset` of `link`; `data` of `object`;
//!   `poster` of `video`; and `content` of a `meta` whose `name` or `property` names an image
//!   (`og:image`, `twitter:image`, the `msapplication-*` tiles, ...). A `srcset` is a list of
//!   candidates, each rewritten on its own.
//! - A root-absolute URL (`/x`, not `//x`) gets the base. A relative one (`./x`, `x`, not a
//!   `scheme:` URL) does too, when `relative` is set: the server sets it for the root
//!   `index.html` served at a URL other than `/` (under a base, or as the history fallback),
//!   where the browser would resolve it against the wrong directory.
//! - An element with a `vite-ignore` attribute is left alone, and the attribute is removed.
//! - In a classic inline `<script>` (no `src`, not `type="module"`), the string literal of
//!   each `import('...')` call is a URL like the attributes' (`/x` gets the base); the rest of
//!   a script is copied as it is, and so are comments.

/// The `meta` names and properties whose `content` is a file URL.
const META_NAMES: &[&str] = &[
    "msapplication-tileimage",
    "msapplication-square70x70logo",
    "msapplication-square150x150logo",
    "msapplication-square310x310logo",
    "msapplication-wide310x150logo",
    "msapplication-config",
    "twitter:image",
];
const META_PROPERTIES: &[&str] = &[
    "og:image",
    "og:image:url",
    "og:image:secure_url",
    "og:audio",
    "og:audio:secure_url",
    "og:video",
    "og:video:secure_url",
];

/// `html` with its references rewritten. `css(text)` transforms a `<style>` block's or a
/// `style` attribute's CSS.
pub fn rewrite(
    html: &str,
    base: &str,
    relative: bool,
    css: &mut dyn FnMut(&str) -> String,
) -> String {
    let mut out = String::with_capacity(html.len() + 64);
    let mut at = 0usize;
    let mut i = 0usize;
    while let Some(off) = html[i..].find('<') {
        let start = i + off;
        if html[start..].starts_with("<!--") {
            i = html[start..]
                .find("-->")
                .map_or(html.len(), |j| start + j + 3);
            continue;
        }
        let name_end = html[start + 1..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == ':'))
            .map_or(html.len(), |j| start + 1 + j);
        if name_end == start + 1 {
            i = start + 1;
            continue;
        }
        let name = html[start + 1..name_end].to_ascii_lowercase();
        let (attrs, end) = attributes(html, name_end);
        let value_of = |n: &str| {
            attrs
                .iter()
                .find(|a| a.name == n)
                .map(|a| &html[a.value.clone()])
        };
        let ignored = attrs.iter().find(|a| a.name == "vite-ignore");
        if let Some(a) = ignored {
            // Drop the attribute (with the space before it); leave the rest as written.
            let from = html[..a.start].trim_end().len();
            out.push_str(&html[at..from]);
            at = a.end;
        } else {
            for a in &attrs {
                if a.value.is_empty() && !a.quoted {
                    continue;
                }
                let value = &html[a.value.clone()];
                let new = if a.name == "style"
                    && (value.contains("url(") || value.contains("image-set("))
                {
                    let quote = if a.quote == b'\'' { "&#39;" } else { "&quot;" };
                    let q = a.quote as char;
                    let t = css(value);
                    Some(if a.quoted { t.replace(q, quote) } else { t })
                } else if is_srcset(&name, &a.name) {
                    Some(srcset(value, &mut |u| url(u, base, relative)))
                } else if is_url_attr(&name, &a.name, &value_of) {
                    url(value, base, relative)
                } else {
                    None
                };
                if let Some(new) = new.filter(|n| n != value) {
                    out.push_str(&html[at..a.value.start]);
                    out.push_str(&new);
                    at = a.value.end;
                }
            }
        }
        i = end.min(html.len());
        // A `<style>` block's text is CSS; a `<script>`'s is copied untouched.
        if (name == "style" || name == "script") && !html[..i].ends_with("/>") {
            let close = format!("</{name}");
            let body_end = find_ci(html, i, &close).unwrap_or(html.len());
            let body = &html[i..body_end];
            if name == "style" && ignored.is_none() {
                let new = css(body);
                if new != body {
                    out.push_str(&html[at..i]);
                    out.push_str(&new);
                    at = body_end;
                }
            } else if name == "script"
                && ignored.is_none()
                && value_of("src").is_none()
                && !value_of("type").is_some_and(|t| t.eq_ignore_ascii_case("module"))
            {
                for (s, e) in import_expressions(body) {
                    if let Some(new) = url(&body[s..e], base, relative) {
                        out.push_str(&html[at..i + s]);
                        out.push_str(&new);
                        at = i + e;
                    }
                }
            }
            i = body_end;
        }
        if i <= start {
            break;
        }
    }
    out.push_str(&html[at..]);
    out
}

/// The byte ranges (inside the quotes) of the string arguments of `import("...")` calls in
/// classic script text. Use the JS scanner so comments, strings, template text and regex
/// literals are skipped, and `x.import(...)` is left alone.
fn import_expressions(js: &str) -> Vec<(usize, usize)> {
    crate::scan::scan(js)
        .imports
        .into_iter()
        .filter(|r| r.kind == crate::scan::ImportKind::Dynamic)
        .map(|r| (r.start as usize, r.end as usize))
        .collect()
}

struct Attr {
    name: String,
    /// The whole attribute, name to closing quote.
    start: usize,
    end: usize,
    value: std::ops::Range<usize>,
    quoted: bool,
    quote: u8,
}

/// The attributes of the tag whose name ends at `from`, and the index past its `>`.
fn attributes(html: &str, from: usize) -> (Vec<Attr>, usize) {
    let b = html.as_bytes();
    let mut attrs = Vec::new();
    let mut j = from;
    while j < b.len() && b[j] != b'>' {
        if b[j].is_ascii_whitespace() || b[j] == b'/' {
            j += 1;
            continue;
        }
        let start = j;
        while j < b.len() && !b"=> \t\r\n/".contains(&b[j]) {
            j += 1;
        }
        let name = html[start..j].to_ascii_lowercase();
        let mut k = j;
        while k < b.len() && b[k].is_ascii_whitespace() {
            k += 1;
        }
        if b.get(k) != Some(&b'=') {
            attrs.push(Attr {
                name,
                start,
                end: j,
                value: j..j,
                quoted: false,
                quote: 0,
            });
            continue;
        }
        j = k + 1;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        let (value, quoted, quote, end) = match b.get(j) {
            Some(&q @ (b'"' | b'\'')) => {
                let e = html[j + 1..]
                    .find(q as char)
                    .map_or(html.len(), |k| j + 1 + k);
                (j + 1..e, true, q, (e + 1).min(html.len()))
            }
            _ => {
                let e = html[j..]
                    .find(|c: char| c.is_ascii_whitespace() || c == '>')
                    .map_or(html.len(), |k| j + k);
                (j..e, false, 0, e)
            }
        };
        attrs.push(Attr {
            name,
            start,
            end,
            value,
            quoted,
            quote,
        });
        j = end;
    }
    (attrs, (j + 1).min(html.len()))
}

fn is_srcset(tag: &str, attr: &str) -> bool {
    matches!(
        (tag, attr),
        ("img" | "source", "srcset") | ("link", "imagesrcset")
    )
}

fn is_url_attr<'a>(tag: &str, attr: &str, value_of: &dyn Fn(&str) -> Option<&'a str>) -> bool {
    match (tag, attr) {
        ("audio" | "embed" | "img" | "input" | "script" | "source" | "track" | "video", "src")
        | ("link", "href")
        | ("image" | "use", "href" | "xlink:href")
        | ("object", "data")
        | ("video", "poster") => true,
        ("meta", "content") => {
            let is = |key: &str, list: &[&str]| {
                value_of(key).is_some_and(|v| list.iter().any(|n| v.eq_ignore_ascii_case(n)))
            };
            is("name", META_NAMES) || is("property", META_PROPERTIES)
        }
        _ => false,
    }
}

/// One URL with the base: root-absolute ones always, relative ones when `relative`.
fn url(value: &str, base: &str, relative: bool) -> Option<String> {
    let v = value.trim();
    if v.starts_with('/') && !v.starts_with("//") {
        if base == "/" || v.starts_with(base) {
            return None;
        }
        return Some(format!("{base}{}", &v[1..]));
    }
    let bare = v
        .as_bytes()
        .first()
        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'@')
        && !v.contains(':');
    if relative && (v.starts_with('.') || bare) {
        return Some(join(base, v));
    }
    None
}

/// `base` (ending in `/`) joined with the relative `path`, `.` and `..` resolved.
fn join(base: &str, path: &str) -> String {
    let mut parts: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    let (path, suffix) = match path.find(['?', '#']) {
        Some(k) => (&path[..k], &path[k..]),
        None => (path, ""),
    };
    let trailing = path.ends_with('/');
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    let mut out = format!("/{}", parts.join("/"));
    if trailing && out.len() > 1 {
        out.push('/');
    }
    out.push_str(suffix);
    out
}

/// A `srcset` with each candidate's URL mapped (`a.png 1x, b.png 2x`).
fn srcset(value: &str, map: &mut dyn FnMut(&str) -> Option<String>) -> String {
    let mut out = Vec::new();
    for candidate in value.split(',') {
        let c = candidate.trim();
        if c.is_empty() {
            continue;
        }
        let (u, descriptor) = match c.find(char::is_whitespace) {
            Some(k) => (&c[..k], c[k..].trim()),
            None => (c, ""),
        };
        let u = map(u).unwrap_or_else(|| u.to_owned());
        out.push(if descriptor.is_empty() {
            u
        } else {
            format!("{u} {descriptor}")
        });
    }
    out.join(", ")
}

/// The first index of `needle` (ASCII lowercase) at or after `from`, ignoring case.
fn find_ci(hay: &str, from: usize, needle: &str) -> Option<usize> {
    let h = hay.as_bytes();
    let n = needle.as_bytes();
    (from..h.len().saturating_sub(n.len() - 1)).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(html: &str, relative: bool) -> String {
        rewrite(html, "/app/", relative, &mut |css| {
            css.replace("url(/i.png)", "url(/app/i.png)")
        })
    }

    #[test]
    fn root_absolute_urls_get_the_base() {
        let html = "<!-- <script src=\"/c.js\"> --><link rel=icon href=/f.ico><script type=\"module\" src='/src/m.js'></script><a href=\"/about\">x</a><img src=\"//cdn/x.png\" alt=\"a > b\"><img src=\"/i.png\">";
        assert_eq!(
            run(html, false),
            "<!-- <script src=\"/c.js\"> --><link rel=icon href=/app/f.ico><script type=\"module\" src='/app/src/m.js'></script><a href=\"/about\">x</a><img src=\"//cdn/x.png\" alt=\"a > b\"><img src=\"/app/i.png\">"
        );
    }

    #[test]
    fn relative_urls_srcset_and_meta() {
        let html = "<img src=\"./a.png\" srcset=\"./a.png 1x, /b.png 2x\"><meta property=\"og:image\" content=\"./og.png\"><meta name=\"description\" content=\"x.png\"><img src=\"data:image/png;base64,AA\"><video poster=\"p/../q.png\">";
        assert_eq!(
            run(html, true),
            "<img src=\"/app/a.png\" srcset=\"/app/a.png 1x, /app/b.png 2x\"><meta property=\"og:image\" content=\"/app/og.png\"><meta name=\"description\" content=\"x.png\"><img src=\"data:image/png;base64,AA\"><video poster=\"/app/q.png\">"
        );
        assert_eq!(run("<img src=\"./a.png\">", false), "<img src=\"./a.png\">");
    }

    #[test]
    fn styles_and_ignored_elements() {
        let html = "<style>a{b:url(/i.png)}</style><div style=\"c:url(/i.png)\"></div><script>const s = '<img src=\"/x.png\">'</script><img vite-ignore src=\"/i.png\">";
        assert_eq!(
            run(html, false),
            "<style>a{b:url(/app/i.png)}</style><div style=\"c:url(/app/i.png)\"></div><script>const s = '<img src=\"/x.png\">'</script><img src=\"/i.png\">"
        );
    }

    #[test]
    fn classic_script_import_expressions() {
        let html = "<script>import('/a.js'); x.import('/b.js'); // import('/c.js')\n\"import('/d.js')\"; import (\"./e.js\")</script><script type=module>import('/f.js')</script>";
        assert_eq!(
            run(html, true),
            "<script>import('/app/a.js'); x.import('/b.js'); // import('/c.js')\n\"import('/d.js')\"; import (\"/app/e.js\")</script><script type=module>import('/f.js')</script>"
        );
    }

    #[test]
    fn classic_script_regex_literals_are_unchanged() {
        let html = r#"<script>const re = /import("foo")/; const escaped = /\/["/]import("fake")/g; const n = 8 / 2; const x = n / import("real.js") / 2;</script>"#;
        assert_eq!(
            run(html, true),
            r#"<script>const re = /import("foo")/; const escaped = /\/["/]import("fake")/g; const n = 8 / 2; const x = n / import("/app/real.js") / 2;</script>"#
        );
    }

    #[test]
    fn malformed_input_terminates() {
        for html in [
            "<",
            "<img src=",
            "<img src=\"",
            "<style>",
            "<script",
            "<!--",
            "<a b c",
            "<script>import(",
            "<script>import('/a",
            "<script>'\\",
            "<script>/*",
        ] {
            let _ = run(html, true);
        }
    }
}
