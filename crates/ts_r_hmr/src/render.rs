//! Text producers: import rewriting, HTML client injection, CSS-as-JS modules, content types,
//! code frames.

use crate::scan::ImportRecord;

/// URL the client runtime is served from.
pub const CLIENT_URL: &str = "/@ts-r/client";

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

/// A stylesheet as a self-accepting JS module that injects a `<style>` tag.
pub fn css_module(url: &str, css: &str) -> String {
    css_module_at(CLIENT_URL, url, css)
}

/// [`css_module`] with the client at `client` (under a base).
pub fn css_module_at(client: &str, url: &str, css: &str) -> String {
    let id = js_string(url);
    let client = js_string(client);
    format!(
        "import {{ createHotContext as __ts_r_hot, updateStyle as __ts_r_update_style, removeStyle as __ts_r_remove_style }} from {client};\n\
         import.meta.hot = __ts_r_hot({id});\n\
         const __ts_r_id = {id};\n\
         const __ts_r_css = {};\n\
         __ts_r_update_style(__ts_r_id, __ts_r_css);\n\
         import.meta.hot.accept();\n\
         import.meta.hot.prune(() => __ts_r_remove_style(__ts_r_id));\n\
         export default __ts_r_css;\n",
        js_string(css)
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
            "<head>\n    <script type=\"module\" src=\"/@ts-r/client\"></script><title>"
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
