//! Text producers: import rewriting, HTML client injection, CSS-as-JS modules, content types,
//! code frames.

use crate::scan::ImportRecord;

/// URL the client runtime is served from, and the one pages and modules import it by: Vite's,
/// so a page or plugin that names `/@vite/client` shares the one instance (`/@ts-r/client`
/// re-exports it).
pub const CLIENT_URL: &str = "/@vite/client";

/// One replacement in a code string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub start: u32,
    pub end: u32,
    pub text: String,
}

/// Applies non-overlapping edits (any order) and prepends `prelude`.
pub fn apply_edits(code: &str, prelude: &str, edits: &mut [Edit]) -> String {
    edits.sort_by_key(|e| e.start);
    let extra: usize = edits.iter().map(|e| e.text.len()).sum();
    let mut out = String::with_capacity(prelude.len() + code.len() + extra);
    out.push_str(prelude);
    let mut at = 0usize;
    for e in edits.iter() {
        let (s, t) = (e.start as usize, e.end as usize);
        if s < at
            || t > code.len()
            || s > t
            || !code.is_char_boundary(s)
            || !code.is_char_boundary(t)
        {
            continue; // overlapping or out of range: skip rather than corrupt
        }
        out.push_str(&code[at..s]);
        out.push_str(&e.text);
        at = t;
    }
    out.push_str(&code[at..]);
    out
}

/// Rewrites every import specifier for which `url_for` returns a URL. `url_for` gets the
/// index into `imports` so callers can use precomputed resolutions.
pub fn rewrite_imports(
    code: &str,
    imports: &[ImportRecord],
    prelude: &str,
    mut url_for: impl FnMut(usize, &ImportRecord) -> Option<String>,
) -> String {
    let mut edits: Vec<Edit> = imports
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let url = url_for(i, r)?;
            (url != r.specifier).then_some(Edit {
                start: r.start,
                end: r.end,
                text: url,
            })
        })
        .collect();
    apply_edits(code, prelude, &mut edits)
}

/// The line injected at the top of a module that uses `import.meta.hot`. Kept on one line
/// so source maps shift by columns on line 1 only.
pub fn hot_prelude(url: &str) -> String {
    hot_prelude_at(CLIENT_URL, url)
}

/// [`hot_prelude`] with the client at `client` (under a base).
pub fn hot_prelude_at(client: &str, url: &str) -> String {
    format!(
        "import {{ createHotContext as __ts_r_hot }} from {};import.meta.hot = __ts_r_hot({});",
        js_string(client),
        js_string(url)
    )
}

/// Inserts the client `<script>` after `<head>` (or `<html>`, or at the very start).
pub fn inject_client(html: &str) -> String {
    inject_client_with(html, "")
}

/// [`inject_client`] plus `extra` HTML right after the client script (before any of the
/// page's own scripts).
pub fn inject_client_with(html: &str, extra: &str) -> String {
    inject_client_at(html, CLIENT_URL, extra)
}

/// [`inject_client_with`] with the client at `client` (under a base).
pub fn inject_client_at(html: &str, client: &str, extra: &str) -> String {
    let tag = format!("<script type=\"module\" src=\"{client}\"></script>{extra}");
    let lower = html.to_ascii_lowercase();
    let at = find_tag_end(&lower, "<head").or_else(|| find_tag_end(&lower, "<html"));
    match at {
        Some(i) => format!("{}\n    {tag}{}", &html[..i], &html[i..]),
        None => {
            // After a doctype if there is one.
            match lower.trim_start().starts_with("<!doctype") {
                true => {
                    let i = lower.find('>').map_or(0, |i| i + 1);
                    format!("{}\n{tag}{}", &html[..i], &html[i..])
                }
                false => format!("{tag}\n{html}"),
            }
        }
    }
}

/// `value` for a double-quoted HTML attribute.
pub fn attr_escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;")
}

/// The page with `nonce="<nonce>"` on every `<script>`, `<style>` and
/// `<link rel="stylesheet|modulepreload|preload">` that has no `nonce` yet (Vite's
/// `html.cspNonce`, as observed on its csp playground). Script and style contents and
/// comments are skipped.
pub fn with_nonce(html: &str, nonce: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let attr = format!(" nonce=\"{}\"", attr_escape(nonce));
    let mut out = String::with_capacity(html.len() + 64);
    let mut copied = 0;
    let mut from = 0;
    while let Some(i) = lower[from..].find('<') {
        let start = from + i;
        if lower[start..].starts_with("<!--") {
            from = lower[start..]
                .find("-->")
                .map_or(lower.len(), |j| start + j + 3);
            continue;
        }
        let name_end = lower[start + 1..]
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
            .map_or(lower.len(), |j| start + 1 + j);
        let name = &lower[start + 1..name_end];
        let Some(end) = crate::html_proxy::tag_end(&lower, start) else {
            break;
        };
        let tag = &lower[start..end];
        let wants = match name {
            "script" | "style" => true,
            "link" => link_rel(tag).is_some_and(|r| {
                r.split_ascii_whitespace()
                    .any(|r| matches!(r, "stylesheet" | "modulepreload" | "preload"))
            }),
            _ => false,
        };
        if wants && !crate::html_proxy::has_attr(tag, "nonce") {
            out.push_str(&html[copied..name_end]);
            out.push_str(&attr);
            copied = name_end;
        }
        from = end + 1;
        if matches!(name, "script" | "style") {
            let close = format!("</{name}");
            from = lower[from..].find(&close).map_or(lower.len(), |j| from + j);
        }
    }
    out.push_str(&html[copied..]);
    out
}

/// The `rel` value of a (lowercased) `<link ...>` tag.
fn link_rel(tag: &str) -> Option<&str> {
    let (i, _) = tag
        .match_indices("rel")
        .find(|&(i, _)| i > 0 && tag.as_bytes()[i - 1].is_ascii_whitespace())?;
    let rest = tag[i + 3..].trim_start().strip_prefix('=')?.trim_start();
    match rest.as_bytes().first()? {
        q @ (b'"' | b'\'') => {
            let rest = &rest[1..];
            rest.find(*q as char).map(|j| &rest[..j])
        }
        _ => rest
            .split(|c: char| c.is_ascii_whitespace() || c == '>')
            .next(),
    }
}

/// Byte index just after the `>` of the first `<name ...>` tag (not `<header>`).
fn find_tag_end(lower: &str, name: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let start = from + i;
        let after = start + name.len();
        match lower.as_bytes().get(after) {
            Some(b'>') | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') => {
                return lower[after..].find('>').map(|j| after + j + 1);
            }
            _ => from = after,
        }
    }
    None
}

/// A stylesheet as a self-accepting JS module that injects a `<style>` tag. A plain
/// stylesheet exports nothing, like Vite's (the text is `?inline`'s default export). A CSS
/// module (`x.module.css`) served this way (a dependency's) exports an empty class map as
/// its default, so a page importing one still links; app CSS modules go through
/// [`css_module_with_exports`].
pub fn css_module(url: &str, css: &str) -> String {
    css_module_at(CLIENT_URL, url, url, css)
}

/// [`css_module`] with the client at `client` (under a base). `style_id` names the `<style>`
/// tag (`data-vite-dev-id`): Vite uses the stylesheet's file path, so a page rendered with a
/// `<style>` or `<link>` carrying that id is reused instead of styled twice.
pub fn css_module_at(client: &str, url: &str, style_id: &str, css: &str) -> String {
    let exports = match is_css_module(url) {
        true => "export default {};\n".to_owned(),
        false => String::new(),
    };
    style_module(client, url, style_id, css, &exports)
}

/// A CSS module (scoped by [`crate::css_modules`]) as a JS module: the style, the class map
/// as the default export and each name that is a JS identifier as a named export, as Vite's
/// `dataToEsm` with named exports does.
pub fn css_module_with_exports(
    client: &str,
    url: &str,
    style_id: &str,
    css: &str,
    exports: &[(String, String)],
) -> String {
    let mut code = String::new();
    let mut map = String::from("export default {");
    for (i, (k, v)) in exports.iter().enumerate() {
        if i > 0 {
            map.push(',');
        }
        map.push_str(&format!("\n  {}: {}", js_string(k), js_string(v)));
        if is_export_name(k) {
            code.push_str(&format!("export const {k} = {};\n", js_string(v)));
        }
    }
    map.push_str(if exports.is_empty() { "};\n" } else { "\n};\n" });
    code.push_str(&map);
    style_module(client, url, style_id, css, &code)
}

/// Whether a CSS module name can be a named export (`const <name>`).
fn is_export_name(k: &str) -> bool {
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
        "instanceof",
        "interface",
        "let",
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
        "var",
        "void",
        "while",
        "with",
        "yield",
    ];
    let mut chars = k.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        && !RESERVED.contains(&k)
        && !k.starts_with("__ts_r_")
}

fn style_module(client: &str, url: &str, style_id: &str, css: &str, exports: &str) -> String {
    let hot = js_string(url);
    let id = js_string(style_id);
    let client = js_string(client);
    format!(
        "import {{ createHotContext as __ts_r_hot, updateStyle as __ts_r_update_style, removeStyle as __ts_r_remove_style }} from {client};\n\
         import.meta.hot = __ts_r_hot({hot});\n\
         const __ts_r_id = {id};\n\
         const __ts_r_css = {};\n\
         __ts_r_update_style(__ts_r_id, __ts_r_css);\n\
         import.meta.hot.accept();\n\
         import.meta.hot.prune(() => __ts_r_remove_style(__ts_r_id));\n{exports}",
        js_string(css),
    )
}

/// Whether `url` names a CSS module: `.module.` before a stylesheet extension
/// (`vite: packages/vite/src/node/plugins/css.ts`, the CSS modules pattern).
pub fn is_css_module(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let Some((stem, ext)) = path.rsplit_once('.') else {
        return false;
    };
    stem.ends_with(".module")
        && matches!(
            ext,
            "css" | "less" | "sass" | "scss" | "styl" | "stylus" | "pcss" | "postcss" | "sss"
        )
}

/// Joins a relative URL path onto a directory URL ending in `/`, resolving `.` and `..`.
pub fn join_url(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    let (path, suffix) = match rel.find(['?', '#']) {
        Some(i) => (&rel[..i], &rel[i..]),
        None => (rel, ""),
    };
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    format!("/{}{suffix}", parts.join("/"))
}

/// A JS string literal (JSON escaping, plus U+2028/2029 and `</script` safety).
pub fn js_string(s: &str) -> String {
    let json = serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_owned());
    json.replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
        .replace("</", "<\\/")
}

/// The import attributes clause after a static import's specifier: `with { ... }` (or the
/// older `assert { ... }`) starting after the closing quote at `quote`. Returns the span
/// from just after the quote to just after the `}`, which the server removes: the browser
/// would otherwise demand a JSON MIME type for a `.json` module served as JS (Vite strips
/// static import attributes too, `vite: packages/vite/src/node/plugins/importAnalysis.ts`).
pub fn import_attributes_span(code: &str, quote: u32) -> Option<(u32, u32)> {
    let b = code.as_bytes();
    let start = quote as usize + 1;
    let mut i = skip_trivia(b, start);
    let kw = if b[i.min(b.len())..].starts_with(b"with") {
        4
    } else if b[i.min(b.len())..].starts_with(b"assert") {
        6
    } else {
        return None;
    };
    i = skip_trivia(b, i + kw);
    if b.get(i) != Some(&b'{') {
        return None;
    }
    // To the matching `}`: attribute values are string literals, keys identifiers or strings;
    // comments are skipped.
    let mut depth = 0u32;
    while i < b.len() {
        let after = skip_trivia(b, i);
        if after != i {
            i = after;
            continue;
        }
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((start as u32, i as u32 + 1));
                }
            }
            q @ (b'"' | b'\'') => {
                i += 1;
                while i < b.len() && b[i] != q && b[i] != b'\n' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
            }
            b';' => return None,
            _ => {}
        }
        i += 1;
    }
    None
}

/// Past spaces and comments from `i` (newlines included).
fn skip_trivia(b: &[u8], mut i: usize) -> usize {
    while i < b.len() {
        match b[i] {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = b[i + 2..]
                    .windows(2)
                    .position(|w| w == b"*/")
                    .map_or(b.len(), |p| i + 2 + p + 2);
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                i = b[i..]
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(b.len(), |p| i + p);
            }
            _ => break,
        }
    }
    i
}

/// Content type by file extension.
pub fn content_type(ext: &str) -> &'static str {
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" | "mts" | "cts" => {
            "text/javascript; charset=utf-8"
        }
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "webmanifest" => "application/manifest+json",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "eot" => "application/vnd.ms-fontobject",
        "wasm" => "application/wasm",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "ogg" => "audio/ogg",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

pub const JS_CONTENT_TYPE: &str = "text/javascript; charset=utf-8";

/// 1-based line and column (in bytes) of `offset` in `src`.
pub fn line_col(src: &str, offset: usize) -> (u32, u32) {
    let offset = offset.min(src.len());
    let before = &src.as_bytes()[..offset];
    let line = before.iter().filter(|&&c| c == b'\n').count() + 1;
    let col = offset
        - before
            .iter()
            .rposition(|&c| c == b'\n')
            .map_or(0, |i| i + 1)
        + 1;
    (line as u32, col as u32)
}

/// A few lines of context with a caret under `column`.
pub fn code_frame(src: &str, line: u32, column: u32) -> String {
    let lines: Vec<&str> = src.lines().collect();
    if line == 0 || line as usize > lines.len().max(1) {
        return String::new();
    }
    let first = line.saturating_sub(2).max(1);
    let last = (line + 2).min(lines.len() as u32);
    let width = last.to_string().len();
    let mut out = String::new();
    for n in first..=last {
        let text = lines.get(n as usize - 1).copied().unwrap_or("");
        let marker = if n == line { ">" } else { " " };
        out.push_str(&format!("{marker} {n:>width$} | {text}\n"));
        if n == line {
            let pad: String = text
                .chars()
                .take(column.saturating_sub(1) as usize)
                .map(|c| if c == '\t' { '\t' } else { ' ' })
                .collect();
            out.push_str(&format!("  {} | {pad}^\n", " ".repeat(width)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::scan;

    #[test]
    fn csp_nonce_goes_on_scripts_styles_and_stylesheets_once() {
        let html = "<link rel=\"stylesheet\" href=\"a.css\"><link rel=icon href=i.png>\n<style>p{}</style><!-- <script> -->\n<script>let s = '<style>'</script><script nonce=\"x\">1</script><link rel='modulepreload' href=m.js>";
        assert_eq!(
            with_nonce(html, "N\""),
            "<link nonce=\"N&quot;\" rel=\"stylesheet\" href=\"a.css\"><link rel=icon href=i.png>\n<style nonce=\"N&quot;\">p{}</style><!-- <script> -->\n<script nonce=\"N&quot;\">let s = '<style>'</script><script nonce=\"x\">1</script><link nonce=\"N&quot;\" rel='modulepreload' href=m.js>"
        );
    }

    /// `nonce` inside another attribute's quoted value is not a nonce; a bare `nonce` is.
    #[test]
    fn csp_nonce_ignores_text_inside_attribute_values() {
        let html = "<script data-note=\" nonce=example\">window.loaded=true</script><style title='nonce=x'></style><script nonce>1</script><script src=x.js data-nonce=1></script>";
        assert_eq!(
            with_nonce(html, "N"),
            "<script nonce=\"N\" data-note=\" nonce=example\">window.loaded=true</script><style nonce=\"N\" title='nonce=x'></style><script nonce>1</script><script nonce=\"N\" src=x.js data-nonce=1></script>"
        );
    }

    #[test]
    fn import_attribute_spans() {
        fn span(code: &str) -> Option<&str> {
            let r = &scan(code).imports[0];
            import_attributes_span(code, r.end).map(|(s, e)| &code[s as usize..e as usize])
        }
        assert_eq!(
            span("import d from './d.json' with { type: 'json' };"),
            Some(" with { type: 'json' }")
        );
        assert_eq!(
            span("export * from \"./d.json\" /* c */ assert {\n  type: \"json\"\n}\nx()"),
            Some(" /* c */ assert {\n  type: \"json\"\n}")
        );
        assert_eq!(span("import d from './d.json';\nwith ({}) {}"), None);
        assert_eq!(span("import d from './d.json'\nwithout()"), None);
        assert_eq!(span("import d from './d.json' with { type: 'json'"), None);
        assert_eq!(span("import d from './d.json' with"), None);
        // A brace or quote inside a comment is not syntax.
        assert_eq!(
            span("import d from './d.json' with { /* } */ type: 'json' // '}\n};\nx()"),
            Some(" with { /* } */ type: 'json' // '}\n}")
        );
    }

    #[test]
    fn rewrites_bare_and_relative_imports() {
        let code = "import React from 'react';\nimport { a } from \"./a.js\";\nexport * from 'lodash/fp';\nconst m = import('./lazy.js');\n";
        let imports = scan(code).imports;
        let out = rewrite_imports(code, &imports, "/*pre*/", |_, r| {
            Some(match r.specifier.as_str() {
                "react" => "/@modules/react?v=abc".to_owned(),
                "lodash/fp" => "/@modules/lodash/fp?v=abc".to_owned(),
                "./a.js" => "/src/a.js?t=5".to_owned(),
                other => other.to_owned(),
            })
        });
        assert_eq!(
            out,
            "/*pre*/import React from '/@modules/react?v=abc';\nimport { a } from \"/src/a.js?t=5\";\nexport * from '/@modules/lodash/fp?v=abc';\nconst m = import('./lazy.js');\n"
        );
    }

    #[test]
    fn apply_edits_skips_bad_ranges() {
        let mut edits = vec![
            Edit {
                start: 4,
                end: 5,
                text: "X".into(),
            },
            Edit {
                start: 0,
                end: 1,
                text: "Y".into(),
            },
            Edit {
                start: 4,
                end: 6,
                text: "overlap".into(),
            },
            Edit {
                start: 9,
                end: 99,
                text: "oob".into(),
            },
        ];
        assert_eq!(apply_edits("abcdefg", "", &mut edits), "YbcdXfg");
    }

    #[test]
    fn injects_client_script() {
        let out = inject_client("<!doctype html><html><head><title>x</title></head></html>");
        assert!(out.contains(
            "<head>\n    <script type=\"module\" src=\"/@vite/client\"></script><title>"
        ));
        let out = inject_client("<HTML lang=en><header>no head</header>");
        assert!(out.starts_with("<HTML lang=en>\n    <script"));
        let out = inject_client("<!DOCTYPE html><p>bare");
        assert!(out.starts_with("<!DOCTYPE html>\n<script"));
        assert!(inject_client("<p>x</p>").starts_with("<script"));
    }

    #[test]
    fn css_helpers() {
        let m = css_module("/src/a.css", "body { content: \"</style>\" }");
        assert!(m.contains("import.meta.hot.accept();"));
        assert!(m.contains("<\\/style>"));
        assert!(!m.contains("export"));
        let m = css_module("/src/a.module.css?t=1", ".a { color: red }");
        assert!(m.ends_with("export default {};\n"), "{m}");
        assert!(is_css_module("/x.module.scss"));
        assert!(!is_css_module("/module.css"));
        assert!(!is_css_module("/x.module.css.js"));
        assert_eq!(join_url("/", "../../x"), "/x");
    }

    #[test]
    fn frames_and_positions() {
        let src = "line1\nline2 bad\nline3\n";
        assert_eq!(line_col(src, 12), (2, 7));
        assert_eq!(line_col(src, 0), (1, 1));
        let f = code_frame(src, 2, 7);
        assert!(f.contains("> 2 | line2 bad\n"), "{f}");
        assert!(f.contains("|       ^"), "{f}");
        assert_eq!(code_frame(src, 99, 1), "");
    }

    #[test]
    fn content_types() {
        assert_eq!(content_type("js"), JS_CONTENT_TYPE);
        assert_eq!(content_type("svg"), "image/svg+xml");
        assert_eq!(content_type("weird"), "application/octet-stream");
    }
}
