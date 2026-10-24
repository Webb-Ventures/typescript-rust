//! Inline module scripts of an HTML page (`<script type="module">…</script>`) are served as
//! modules of their own, as Vite does: the page gets
//! `<script type="module" src="/page.html?html-proxy&index=N.js">` instead, and that URL
//! answers with the N-th inline script, transformed like any module (imports rewritten,
//! `import.meta.env`, `import.meta.hot`).

use std::ops::Range;
use std::path::{Path, PathBuf};

/// The query an inline script's URL carries after the page's URL.
const MARK: &str = "?html-proxy&index=";

/// Byte ranges of each inline module script: the opening tag's `>` and the script's text.
fn inline_scripts(html: &str) -> Vec<(usize, Range<usize>)> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while from < lower.len() {
        let Some(i) = lower[from..].find(['<']) else {
            break;
        };
        let start = from + i;
        if lower[start..].starts_with("<!--") {
            from = lower[start..]
                .find("-->")
                .map_or(lower.len(), |j| start + j + 3);
            continue;
        }
        from = start + 1;
        if !lower[start..].starts_with("<script")
            || !lower[start + 7..].starts_with(|c: char| c.is_ascii_whitespace() || c == '>')
        {
            continue;
        }
        let Some(gt) = tag_end(&lower, start) else {
            break;
        };
        let tag = &lower[start..gt];
        let Some(close) = lower[gt + 1..].find("</script").map(|j| gt + 1 + j) else {
            break;
        };
        from = close;
        if is_module(tag) && !has_attr(tag, "src") && !html[gt + 1..close].trim().is_empty() {
            out.push((gt, gt + 1..close));
        }
    }
    out
}

/// Index of the `>` closing the tag that starts at `start`, honouring quoted values.
fn tag_end(lower: &str, start: usize) -> Option<usize> {
    let mut quote = None;
    for (i, c) in lower[start..].char_indices() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => return Some(start + i),
            _ => {}
        }
    }
    None
}

fn is_module(tag: &str) -> bool {
    ["type=\"module\"", "type='module'", "type=module"]
        .iter()
        .any(|t| tag.contains(t))
}

fn has_attr(tag: &str, name: &str) -> bool {
    tag.match_indices(name).any(|(i, _)| {
        i > 0
            && tag.as_bytes()[i - 1].is_ascii_whitespace()
            && tag[i + name.len()..].trim_start().starts_with('=')
    })
}

/// The page with each inline module script replaced by a `src` pointing at its proxy URL;
/// `page_url` is the HTML file's URL (`/index.html`).
pub fn with_proxies(html: &str, page_url: &str) -> String {
    let scripts = inline_scripts(html);
    if scripts.is_empty() {
        return html.to_owned();
    }
    let mut out = String::with_capacity(html.len());
    let mut at = 0;
    for (n, (gt, body)) in scripts.into_iter().enumerate() {
        out.push_str(&html[at..gt]);
        out.push_str(&format!(" src=\"{page_url}{MARK}{n}.js\">"));
        at = body.end;
    }
    out.push_str(&html[at..]);
    out
}

/// The text of the `n`-th inline module script.
pub fn script(html: &str, n: usize) -> Option<&str> {
    inline_scripts(html).get(n).map(|(_, r)| &html[r.clone()])
}

/// The `index` query value of a proxy request (`0.js` → 0).
pub fn index_of(value: &str) -> Option<usize> {
    value.strip_suffix(".js")?.parse().ok()
}

/// The graph's file for the `n`-th inline script of `page`: the page's path with the proxy
/// query, so relative imports resolve from the page's directory.
pub fn module_file(page: &Path, n: usize) -> PathBuf {
    PathBuf::from(format!("{}{MARK}{n}.js", page.display()))
}

/// The page and script index behind a [`module_file`] path.
pub fn split_module_file(file: &Path) -> Option<(PathBuf, usize)> {
    let s = file.to_str()?;
    let (page, n) = s.split_once(MARK)?;
    Some((PathBuf::from(page), index_of(n)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<!doctype html><head><script>var classic = 1</script>
<!-- <script type="module">commented()</script> -->
<script type="module" src="/src/main.js"></script>
<script type="module" data-x="a>b">
  text('.mode', import.meta.env.MODE)
</script>
<SCRIPT TYPE='module'>import './b.js'</SCRIPT>
<script type="module">   </script>
</head>"#;

    #[test]
    fn inline_module_scripts_become_proxy_urls() {
        let out = with_proxies(PAGE, "/index.html");
        assert!(
            out.contains(
                r#"<script type="module" data-x="a>b" src="/index.html?html-proxy&index=0.js"></script>"#
            ),
            "{out}"
        );
        assert!(
            out.contains(
                r#"<SCRIPT TYPE='module' src="/index.html?html-proxy&index=1.js"></SCRIPT>"#
            ),
            "{out}"
        );
        // Classic, external, empty and commented-out scripts stay as they are.
        assert!(out.contains("<script>var classic = 1</script>"));
        assert!(out.contains(r#"<script type="module" src="/src/main.js"></script>"#));
        assert!(out.contains("commented()"));
        assert!(out.contains(r#"<script type="module">   </script>"#));
        assert_eq!(
            script(PAGE, 0).map(str::trim),
            Some("text('.mode', import.meta.env.MODE)")
        );
        assert_eq!(script(PAGE, 1), Some("import './b.js'"));
        assert_eq!(script(PAGE, 2), None);
    }

    #[test]
    fn module_file_round_trips() {
        let f = module_file(Path::new("/app/index.html"), 3);
        assert!(f.starts_with("/app"));
        assert_eq!(f.parent(), Some(Path::new("/app")));
        assert_eq!(
            split_module_file(&f),
            Some((PathBuf::from("/app/index.html"), 3))
        );
        assert_eq!(index_of("x.js"), None);
    }

    #[test]
    fn unterminated_markup_does_not_loop() {
        for html in [
            "<script type=module>",
            "<script type=\"module",
            "<!--",
            "<",
            "<script",
        ] {
            assert!(inline_scripts(html).is_empty(), "{html}");
        }
    }
}
