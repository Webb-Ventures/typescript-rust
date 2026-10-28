//! Asset and JSON modules, matching what Vite 8.3.4 serves in dev (observed, no Vite code):
//!
//! - `import x from './a.png'` / `?url`: the file's URL; small SVGs (under
//!   [`SVG_INLINE_LIMIT`] bytes) are inlined as `data:image/svg+xml,...` like Vite's dev
//!   server does.
//! - `?inline`: a data URL (SVG text-encoded, everything else base64); CSS with `?inline`
//!   or `?raw` is its text.
//! - JSON: named exports for top-level keys that are valid, non-reserved identifiers, and
//!   a default object referencing them, in source key order.

use crate::render::js_string;
use crate::ws::base64;

/// SVGs smaller than this are inlined as data URLs in dev (Vite's `assetsInlineLimit`).
pub const SVG_INLINE_LIMIT: usize = 4096;

/// The data URL for an asset's bytes (`?inline`, small SVGs).
pub fn data_url(ext: &str, mime: &str, bytes: &[u8]) -> String {
    if ext == "svg"
        && let Ok(text) = std::str::from_utf8(bytes)
        && let Some(u) = svg_data_url(text)
    {
        return u;
    }
    format!("data:{};base64,{}", mime_base(mime), base64(bytes))
}

fn mime_base(mime: &str) -> &str {
    mime.split(';').next().unwrap_or(mime).trim()
}

/// SVG as a compact, URL-encoded `data:` URL; `None` when the text needs base64 (it has
/// `'`, or `<text`/`<foreignObject>` whose whitespace matters).
pub fn svg_data_url(svg: &str) -> Option<String> {
    if svg.contains('\'') || svg.contains("<text") || svg.contains("<foreignObject") {
        return Some(format!(
            "data:image/svg+xml;base64,{}",
            base64(svg.as_bytes())
        ));
    }
    // Whitespace between tags goes; other runs of whitespace become one space.
    let trimmed = svg.trim();
    let mut compact = String::with_capacity(trimmed.len());
    let mut chars = trimmed.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            while chars.peek().is_some_and(|n| n.is_whitespace()) {
                chars.next();
            }
            if compact.ends_with('>') && chars.peek() == Some(&'<') {
                continue;
            }
            compact.push(' ');
        } else {
            compact.push(c);
        }
    }
    let mut out = String::with_capacity(compact.len() + 32);
    out.push_str("data:image/svg+xml,");
    for c in compact.chars() {
        match c {
            '"' => out.push('\''),
            '<' => out.push_str("%3c"),
            '>' => out.push_str("%3e"),
            ' ' => out.push_str("%20"),
            '#' => out.push_str("%23"),
            '%' => out.push_str("%25"),
            c => out.push(c),
        }
    }
    Some(out)
}

/// `build.assetsInlineLimit` as a function of the asset's path: `Some(inline)` decides,
/// `None` leaves it to the default limit.
pub type InlineFn = std::sync::Arc<dyn Fn(&std::path::Path) -> Option<bool> + Send + Sync>;

/// Whether an asset import of `ext` with `len` bytes is inlined by default in dev, with
/// the default limit.
pub fn inline_by_default(ext: &str, len: u64) -> bool {
    inline_under(ext, len, SVG_INLINE_LIMIT as u64)
}

/// [`inline_by_default`] with the project's limit (`build.assetsInlineLimit`; 0 = never).
pub fn inline_under(ext: &str, len: u64, limit: u64) -> bool {
    ext == "svg" && len < limit
}

/// `export default "<value>";` for asset URLs and texts.
pub fn default_export(value: &str) -> String {
    format!("export default {};\n", js_string(value))
}

/// Words that can't be `export const` names (reserved words, strict-mode reserved words
/// and the globals Vite also leaves out).
const RESERVED: &[&str] = &[
    "arguments",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "eval",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "Infinity",
    "instanceof",
    "interface",
    "let",
    "NaN",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

fn is_export_name(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_alphabetic() || first == '_' || first == '$')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        && !RESERVED.contains(&key)
}

/// A JSON file as an ES module. Errors are serde's (with line and column).
pub fn json_module(text: &str) -> Result<String, serde_json::Error> {
    // Validate (and get a precise error) first; the text is then walked in source order.
    serde_json::from_str::<serde_json::Value>(text)?;
    let src = text.trim_start_matches('\u{feff}').trim();
    if !src.starts_with('{') {
        return Ok(format!("export default {};\n", minify(src)));
    }
    // A repeated key keeps its first position and its last value, as `JSON.parse` does.
    let mut members: Vec<(String, &str)> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (raw_key, value) in top_level_members(src) {
        let key: String = serde_json::from_str(raw_key).unwrap_or_default();
        match index.get(&key) {
            Some(&i) => members[i].1 = value,
            None => {
                index.insert(key.clone(), members.len());
                members.push((key, value));
            }
        }
    }
    let mut out = String::with_capacity(src.len() + members.len() * 16);
    let mut body = Vec::with_capacity(members.len());
    for (key, value) in members {
        let value = minify(value);
        if key == "__proto__" {
            // A computed key keeps it an own property, as JSON.parse would.
            body.push(format!("  [{}]: {value}", js_string(&key)));
        } else if is_export_name(&key) {
            out.push_str(&format!("export const {key} = {value};\n"));
            body.push(format!("  {key}"));
        } else {
            body.push(format!("  {}: {value}", js_string(&key)));
        }
    }
    if body.is_empty() {
        out.push_str("export default {};\n");
    } else {
        out.push_str("export default {\n");
        out.push_str(&body.join(",\n"));
        out.push_str("\n};\n");
    }
    Ok(out)
}

/// `(raw key literal, raw value text)` of a valid JSON object's top level, in order.
fn top_level_members(src: &str) -> Vec<(&str, &str)> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 1; // after `{`
    loop {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b',') {
            i += 1;
        }
        if i >= b.len() || b[i] != b'"' {
            return out;
        }
        let key_start = i;
        i = string_end(b, i);
        let key = &src[key_start..i];
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b':') {
            i += 1;
        }
        let value_start = i;
        let mut depth = 0i32;
        while i < b.len() {
            match b[i] {
                b'"' => {
                    i = string_end(b, i);
                    continue;
                }
                b'{' | b'[' => depth += 1,
                b'}' | b']' if depth == 0 => break,
                b'}' | b']' => depth -= 1,
                b',' if depth == 0 => break,
                _ => {}
            }
            i += 1;
        }
        out.push((key, src[value_start..i].trim_end()));
    }
}

/// Index just past the string starting at `b[i] == '"'`.
fn string_end(b: &[u8], mut i: usize) -> usize {
    i += 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    b.len()
}

/// Valid JSON without insignificant whitespace (order and number spelling kept).
fn minify(json: &str) -> String {
    let b = json.as_bytes();
    let mut out = String::with_capacity(json.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let e = string_end(b, i);
                out.push_str(&json[i..e]);
                i = e;
            }
            c if c.is_ascii_whitespace() => i += 1,
            _ => {
                let start = i;
                while i < b.len() && b[i] != b'"' && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                out.push_str(&json[start..i]);
            }
        }
    }
    // JSON allows U+2028/2029 in strings; old JS engines didn't.
    out.replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_urls_like_vite() {
        assert_eq!(
            svg_data_url("<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>").unwrap(),
            "data:image/svg+xml,%3csvg%20xmlns='http://www.w3.org/2000/svg'%3e%3crect/%3e%3c/svg%3e"
        );
        assert_eq!(
            svg_data_url("<svg xmlns=\"http://www.w3.org/2000/svg\">\n\t<path fill=\"#fff\" d=\"M0 0  L1 1\"/>\n  <g a=\"50%\" b=\"&amp;\" c=\"é\"/>\n</svg>\n").unwrap(),
            "data:image/svg+xml,%3csvg%20xmlns='http://www.w3.org/2000/svg'%3e%3cpath%20fill='%23fff'%20d='M0%200%20L1%201'/%3e%3cg%20a='50%25'%20b='&amp;'%20c='é'/%3e%3c/svg%3e"
        );
        assert_eq!(
            svg_data_url("  <svg xmlns=\"http://www.w3.org/2000/svg\">a  b\n<g/>  </svg>  ")
                .unwrap(),
            "data:image/svg+xml,%3csvg%20xmlns='http://www.w3.org/2000/svg'%3ea%20b%20%3cg/%3e%3c/svg%3e"
        );
        assert_eq!(
            svg_data_url("<svg xmlns=\"http://www.w3.org/2000/svg\"><text>Hi</text></svg>")
                .unwrap(),
            "data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciPjx0ZXh0PkhpPC90ZXh0Pjwvc3ZnPg=="
        );
        assert_eq!(
            data_url("png", "image/png", b"\x89PNG\r\n\x1a\nfakepng"),
            "data:image/png;base64,iVBORw0KGgpmYWtlcG5n"
        );
        assert!(inline_by_default("svg", 100) && !inline_by_default("svg", 5000));
        assert!(!inline_by_default("png", 10));
    }

    #[test]
    fn json_modules_like_vite() {
        let m = json_module(r#"{"name":"x","version":"1.0","nested":{"b":1,"a":2},"kebab-key":2,"default":3,"let":4,"ünï":5,"__proto__":{"p":1}}"#).unwrap();
        assert_eq!(
            m,
            "export const name = \"x\";\nexport const version = \"1.0\";\nexport const nested = {\"b\":1,\"a\":2};\nexport const ünï = 5;\nexport default {\n  name,\n  version,\n  nested,\n  \"kebab-key\": 2,\n  \"default\": 3,\n  \"let\": 4,\n  ünï,\n  [\"__proto__\"]: {\"p\":1}\n};\n"
        );
        assert_eq!(
            json_module("[1, 2, {\"a\": 3}]\n").unwrap(),
            "export default [1,2,{\"a\":3}];\n"
        );
        assert_eq!(json_module(" {} ").unwrap(), "export default {};\n");
        assert_eq!(
            json_module("{ \"s\": \"a b\\\" c\" }").unwrap(),
            "export const s = \"a b\\\" c\";\nexport default {\n  s\n};\n"
        );
        // A repeated key (in any spelling) is one export with the last value.
        assert_eq!(
            json_module(r#"{"x":1,"y":2,"\u0078":3}"#).unwrap(),
            "export const x = 3;\nexport const y = 2;\nexport default {\n  x,\n  y\n};\n"
        );
        let e = json_module("{\n  \"a\": 1,\n}").unwrap_err();
        assert_eq!(e.line(), 3);
    }
}
