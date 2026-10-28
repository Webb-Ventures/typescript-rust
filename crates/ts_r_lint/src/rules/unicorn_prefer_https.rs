//! `unicorn/prefer-https`: `http://` URLs anywhere in the file text (code, strings,
//! comments, JSX).
//!
//! The scanner finds `http://` or `https://` (any case) not preceded by a word character,
//! `+`, `-` or `.`, an authority (no space or `!"#'(),/;<>?\]`{}`), and the rest of the URL
//! (as the authority, but `#`, `/` and `?` allowed); the next search starts after it. Only
//! lowercase `http` is reported (authority span), and not when the whole URL is a known
//! `http:` identifier (XML namespaces, vocabularies, schema ids), matches the `ignore`
//! option (exact strings or regular expressions), follows `xmlns=` / `xmlns:p=` (an opening
//! quote allowed) within 30 characters, or its host is not public: the WHATWG host of the
//! authority (after the last `@`, before a numeric port) must parse, not be an IP address,
//! and end in a label with a letter that is not a reserved suffix (`localhost`, `test`,
//! `example`, ...). The fix writes `https`.

use super::prelude::*;
use super::unicorn_ast::is_js_space;
use crate::regex_exec::match_all;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-https",
        "Prefer HTTPS over HTTP.",
        NO_PRESET,
        Category::Suspicious,
    )
    .fixable(),
    create: create::<PreferHttps>,
};

/// URIs defined to use `http:` as opaque identifiers.
const IDENTIFIER_URIS: &[&str] = &[
    "http://www.w3.org/2000/svg",
    "http://www.w3.org/1999/xhtml",
    "http://www.w3.org/1999/xlink",
    "http://www.w3.org/2000/xmlns/",
    "http://www.w3.org/XML/1998/namespace",
    "http://www.w3.org/1998/Math/MathML",
    "http://www.w3.org/2001/XInclude",
    "http://www.w3.org/2005/07/scxml",
    "http://www.w3.org/1999/XSL/Transform",
    "http://www.w3.org/1999/XSL/Format",
    "http://www.w3.org/2001/XMLSchema",
    "http://www.w3.org/2001/XMLSchema-instance",
    "http://www.w3.org/2005/xpath-functions",
    "http://www.w3.org/2005/xqt-errors",
    "http://www.w3.org/TR/2001/REC-xml-c14n-20010315",
    "http://www.w3.org/TR/2001/REC-xml-c14n-20010315#WithComments",
    "http://www.w3.org/TR/1999/REC-xpath-19991116",
    "http://www.w3.org/TR/1999/REC-xslt-19991116",
    "http://schemas.xmlsoap.org/soap/envelope/",
    "http://schemas.xmlsoap.org/soap/encoding/",
    "http://schemas.xmlsoap.org/wsdl/",
    "http://www.w3.org/2003/05/soap-envelope",
    "http://www.w3.org/2003/05/soap-encoding",
    "http://www.w3.org/2005/Atom",
    "http://purl.org/rss/1.0/",
    "http://purl.org/dc/elements/1.1/",
    "http://purl.org/dc/terms/",
    "http://schemas.microsoft.com/developer/msbuild/2003",
    "http://www.sitemaps.org/schemas/sitemap/0.9",
];

/// Families of identifier URIs (XML security algorithms, vocabulary terms, Android
/// resources, build-tool schemas), as `v`-flag patterns.
const IDENTIFIER_PATTERNS: &[&str] = &[
    r"^http:\/\/www\.w3\.org\/\d{4}\/(?:\d{2}\/)?(?:xmldsig(?:-more|\d+)?|xmlenc\d*|xml-exc-c14n|decrypt)#",
    r"^http:\/\/www\.w3\.org\/\d{4}\/(?:\d{2}\/)?(?:xmldsig-filter\d+|xml-c14n\d+)(?:#.*)?$",
    r"^http:\/\/www\.w3\.org\/(?:1999\/02\/22-rdf-syntax-ns|2000\/01\/rdf-schema|2001\/XMLSchema|2002\/07\/owl|2004\/02\/skos\/core|2005\/xqt-errors|ns\/(?:dcat|prov|shacl))#.*$",
    r"^http:\/\/www\.w3\.org\/2005\/xpath-functions(?:#.*|\/(?:array|map|math)(?:#.*)?)$",
    r"^http:\/\/purl\.org\/(?:rss\/1\.0|dc\/(?:elements\/1\.1|terms))\/[\w\-]+$",
    r"^http:\/\/schemas\.android\.com\/(?:apk\/(?:res\/(?:android|[A-Za-z]\w*(?:\.[A-Za-z]\w*)+)|res-auto)|aapt|tools)$",
    r"^http:\/\/maven\.apache\.org\/(?:EXTENSIONS|METADATA|POM|SETTINGS|TOOLCHAINS)\/\d+(?:\.\d+)+$",
    r"^http:\/\/www\.springframework\.org\/schema\/[\w\-]+(?:\/[\w\-]+)*$",
];

/// Top-level names that never resolve on the public internet.
const NON_PUBLIC_SUFFIXES: &[&str] = &[
    "example",
    "internal",
    "invalid",
    "local",
    "localdomain",
    "localhost",
    "onion",
    "test",
];

const XML_NAMESPACE_BEFORE: &str = r#"(?<![\w#$\-.:])xmlns(?::[\w\-.]+)?\s*=\s*["']?$"#;

#[derive(Debug, Default)]
struct PreferHttps {
    ignore_urls: Vec<String>,
    /// (source, flags)
    ignore_patterns: Vec<(String, String)>,
}

impl FromOptions for PreferHttps {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut out = PreferHttps::default();
        let Some(list) = obj_opt(opts, 0).get("ignore").and_then(Value::as_array) else {
            return Ok(out);
        };
        for item in list {
            match item {
                Value::String(s) => {
                    if !s.starts_with("http://") {
                        return Err(
                            "String values in the `ignore` option must start with `http://`."
                                .into(),
                        );
                    }
                    out.ignore_urls.push(s.clone());
                }
                // A RegExp, as the oracle data and config loader write it.
                Value::Object(m) => {
                    if let Some(src) = m.get("regex").and_then(Value::as_str) {
                        let flags = m.get("flags").and_then(Value::as_str).unwrap_or("");
                        out.ignore_patterns
                            .push((src.to_owned(), flags.replace(['g', 'y'], "")));
                    }
                }
                _ => return Err("`ignore` items must be strings or regular expressions".into()),
            }
        }
        Ok(out)
    }
}

fn matches(pattern: &str, flags: &str, text: &str) -> bool {
    match_all(pattern, flags, text).is_some_and(|m| !m.is_empty())
}

fn is_authority_char(c: char) -> bool {
    !is_js_space(c) && !"!\"#'(),/;<>?\\]`{}".contains(c)
}

fn is_rest_char(c: char) -> bool {
    !is_js_space(c) && !"!\"'(),;<>\\]`{}".contains(c)
}

/// A URL match at `i`: (is lowercase `http`, authority end, URL end).
fn url_at(src: &str, i: usize) -> Option<(bool, usize, usize)> {
    let b = src.as_bytes();
    let head = b.get(i..i + 4)?;
    if !head.eq_ignore_ascii_case(b"http") {
        return None;
    }
    let mut j = i + 4;
    let secure = b.get(j).is_some_and(|c| c.eq_ignore_ascii_case(&b's'));
    if secure {
        j += 1;
    }
    if b.get(j..j + 3) != Some(b"://") {
        return None;
    }
    j += 3;
    let rest = &src[j..];
    let authority = rest
        .char_indices()
        .find(|&(_, c)| !is_authority_char(c))
        .map_or(rest.len(), |(k, _)| k);
    if authority == 0 {
        return None;
    }
    let after = &rest[authority..];
    let tail = after
        .char_indices()
        .find(|&(_, c)| !is_rest_char(c))
        .map_or(after.len(), |(k, _)| k);
    let lower = !secure && head == b"http";
    Some((lower, j + authority, j + authority + tail))
}

/// The last 30 UTF-16 units before `pos`.
fn preceding(src: &str, pos: usize) -> &str {
    let mut units = 0;
    let mut start = pos;
    for (k, c) in src[..pos].char_indices().rev() {
        units += c.len_utf16();
        if units > 30 {
            break;
        }
        start = k;
    }
    &src[start..pos]
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(h) = b.get(i + 1..i + 3)
            && let Ok(h) = std::str::from_utf8(h)
            && let Ok(v) = u8::from_str_radix(h, 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

fn is_forbidden_domain_char(c: char) -> bool {
    c <= '\u{1f}' || c == '\u{7f}' || " #%/:<>?@[\\]^|".contains(c)
}

/// The hostname `new URL("http://" + authority)` would have, as far as its last label and
/// whether it is an IP address: `None` when the URL would not parse, `Some(None)` for an
/// IP address.
fn hostname(authority: &str) -> Option<Option<String>> {
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = match host_port.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (host_port, None),
    };
    if let Some(port) = port
        && (!port.bytes().all(|c| c.is_ascii_digit())
            || port.trim_start_matches('0').len() > 5
            || port.parse::<u32>().is_ok_and(|p| p > 65535))
    {
        return None;
    }
    if host.is_empty() || host.starts_with('[') {
        return None;
    }
    let decoded = String::from_utf8_lossy(&percent_decode(host)).into_owned();
    let mut name = String::with_capacity(decoded.len());
    for c in decoded.chars() {
        let c = match c {
            '\u{3002}' | '\u{ff0e}' | '\u{ff61}' => '.',
            '\u{ff01}'..='\u{ff5e}' => char::from_u32(c as u32 - 0xfee0).unwrap_or(c),
            '\u{fffd}' => return None,
            _ => c,
        };
        if is_forbidden_domain_char(c) {
            return None;
        }
        name.extend(c.to_lowercase());
    }
    if name.is_empty() {
        return None;
    }
    let mut labels: Vec<&str> = name.split('.').collect();
    if labels.len() > 1 && labels.last() == Some(&"") {
        labels.pop();
    }
    let last = labels.last().copied().unwrap_or("");
    let numeric = (!last.is_empty() && last.bytes().all(|c| c.is_ascii_digit()))
        || last
            .strip_prefix("0x")
            .is_some_and(|h| h.bytes().all(|c| c.is_ascii_hexdigit()));
    Some((!numeric).then_some(name))
}

fn has_public_tld(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    let Some(dot) = host.rfind('.') else {
        return false;
    };
    let tld = &host[dot + 1..];
    // A non-ASCII label becomes `xn--...`, which has letters.
    (tld.bytes().any(|c| c.is_ascii_alphabetic()) || !tld.is_ascii())
        && !NON_PUBLIC_SUFFIXES.contains(&tld)
}

impl PreferHttps {
    fn ignored(&self, url: &str) -> bool {
        IDENTIFIER_URIS.contains(&url)
            || IDENTIFIER_PATTERNS.iter().any(|p| matches(p, "v", url))
            || self.ignore_urls.iter().any(|u| u == url)
            || self.ignore_patterns.iter().any(|(p, f)| matches(p, f, url))
    }
}

impl Rule for PreferHttps {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let src = cx.src();
        let b = src.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if !b[i].eq_ignore_ascii_case(&b'h') {
                i += 1;
                continue;
            }
            let after_boundary = src[..i]
                .chars()
                .next_back()
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || "_+-.".contains(c)));
            let Some((lower, authority_end, end)) =
                after_boundary.then(|| url_at(src, i)).flatten()
            else {
                i += 1;
                continue;
            };
            let start = i;
            i = end;
            if !lower {
                continue;
            }
            let url = &src[start..end];
            let authority = &src[start + "http://".len()..authority_end];
            if self.ignored(url)
                || matches(XML_NAMESPACE_BEFORE, "i", preceding(src, start))
                || !hostname(authority)
                    .flatten()
                    .is_some_and(|h| has_public_tld(&h))
            {
                continue;
            }
            let (start, authority_end) = (start as u32, authority_end as u32);
            cx.report(Span::new(start, authority_end), "Prefer HTTPS over HTTP.")
                .id("prefer-https")
                .fix(Fix::replace(Span::new(start, start + 4), "https"));
        }
    }
}
