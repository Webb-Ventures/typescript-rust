//! Dependency CSS for the browser: relative `@import`s are inlined (wrapped in `@media`,
//! `@supports` or `@layer` blocks when the import has conditions) and relative `url()`s
//! become absolute dev-server URLs, because the stylesheet is served from a different URL
//! than the file it came from (a `<style>` tag, or a pre-bundled `/@ts-r/deps/*.css`).
//!
//! Used for CSS inside `node_modules`: imported by app code (`/@modules/pkg/x.css`) and
//! imported by dependencies (the bundler's batch reads CSS through [`bundle_css`]). This is
//! a scanner, not a CSS parser: comments and strings are skipped, everything else is copied.

use std::path::{Component, Path};

use crate::http;
use crate::resolve::normalize;

/// Nesting limit for `@import` chains.
const MAX_DEPTH: usize = 32;
/// Limits for one stylesheet, whatever the nesting: `@import`s inlined and bytes read for
/// them (files that each import the next twice double the work per level).
const MAX_INLINED: usize = 4096;
const MAX_INLINED_BYTES: usize = 32 << 20;

/// What inlining one stylesheet has spent: `@import`s inlined, bytes read for them.
#[derive(Default)]
struct Spent {
    imports: usize,
    bytes: usize,
}

/// `file`'s CSS with relative `@import`s inlined and relative `url()`s replaced by
/// `url_of(<absolute file>)`. Imports that can't be read (remote, missing, cyclic) are kept
/// and hoisted to the top, where `@import` is valid; past the depth or size limits an
/// import is left out with a comment. `None` if `file` can't be read.
pub fn bundle_css(
    file: &Path,
    read: &dyn Fn(&Path) -> Option<String>,
    url_of: &dyn Fn(&Path) -> String,
) -> Option<String> {
    let css = read(file)?;
    let mut hoisted = Vec::new();
    let mut stack = vec![file.to_path_buf()];
    let mut spent = Spent::default();
    let body = inline(
        file,
        &css,
        read,
        url_of,
        &mut stack,
        &mut spent,
        &mut hoisted,
    );
    if hoisted.is_empty() {
        return Some(body);
    }
    let mut out = hoisted.join("\n");
    out.push('\n');
    out.push_str(&body);
    Some(out)
}

/// The URL a dependency file is served at, without absolute paths: `/<path>` under the
/// root, otherwise `/@ts-r/up/<n>/<path>` (relative to the root's `n`-th ancestor; the server
/// serves only files inside a `node_modules` directory that way).
pub fn asset_url(root: &Path, file: &Path) -> String {
    let r: Vec<Component<'_>> = root.components().collect();
    let f: Vec<Component<'_>> = file.components().collect();
    let common = r.iter().zip(&f).take_while(|(a, b)| a == b).count();
    let up = r.len() - common;
    let rest: Vec<String> = f[common..]
        .iter()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let rest = rest.join("/");
    if up == 0 {
        http::percent_encode_path(&format!("/{rest}"))
    } else {
        http::percent_encode_path(&format!("/@ts-r/up/{up}/{rest}"))
    }
}

fn is_relative_ref(v: &str) -> bool {
    !v.is_empty()
        && !v.starts_with('/')
        && !v.starts_with('#')
        && !v.starts_with('~')
        && !v.contains(':')
}

fn inline(
    file: &Path,
    css: &str,
    read: &dyn Fn(&Path) -> Option<String>,
    url_of: &dyn Fn(&Path) -> String,
    stack: &mut Vec<std::path::PathBuf>,
    spent: &mut Spent,
    hoisted: &mut Vec<String>,
) -> String {
    let dir = file.parent().unwrap_or(Path::new("/"));
    let b = css.as_bytes();
    let mut out = String::with_capacity(css.len());
    let mut i = 0;
    let mut copied = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = css[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
            }
            q @ (b'"' | b'\'') => i = skip_string(b, i, q),
            b'@' if starts_with_ci(&b[i + 1..], b"charset") => {
                // `@charset` is only valid first in a file; inlined files drop theirs.
                let end = statement_end(b, i);
                out.push_str(&css[copied..i]);
                i = end;
                copied = end;
            }
            b'@' if starts_with_ci(&b[i + 1..], b"import")
                && !b
                    .get(i + 7)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'-') =>
            {
                let end = statement_end(b, i);
                let stmt = css[i + 7..end].trim().trim_end_matches(';').trim();
                out.push_str(&css[copied..i]);
                match import_target(stmt) {
                    Some((target, conditions)) if is_relative_ref(target) => {
                        let path = normalize(&dir.join(strip_query(target)));
                        // A cycle is already included; too deep, too much or unreadable
                        // can't be.
                        let cyclic = stack.contains(&path);
                        let within = stack.len() < MAX_DEPTH
                            && spent.imports < MAX_INLINED
                            && spent.bytes < MAX_INLINED_BYTES;
                        if cyclic {
                            // nothing to add
                        } else if let Some(text) = within.then(|| read(&path)).flatten() {
                            spent.imports += 1;
                            spent.bytes += text.len();
                            stack.push(path.clone());
                            let body = inline(&path, &text, read, url_of, stack, spent, hoisted);
                            stack.pop();
                            out.push_str(&wrap(conditions, &body));
                        } else {
                            out.push_str(&format!(
                                "/* ts-r: @import \"{}\" not inlined (missing, too deep or too large) */",
                                target.replace("*/", "")
                            ));
                        }
                    }
                    _ => hoisted.push(format!("@import {stmt};")),
                }
                i = end;
                copied = end;
            }
            b'u' | b'U'
                if starts_with_ci(&b[i..], b"url(") && (i == 0 || !is_ident_byte(b[i - 1])) =>
            {
                let open = i + 4;
                let Some(close) = css[open..].find(')').map(|e| open + e) else {
                    break;
                };
                let inner = css[open..close].trim();
                let value = inner
                    .strip_prefix(['"', '\''])
                    .and_then(|s| s.strip_suffix(['"', '\'']))
                    .unwrap_or(inner);
                if is_relative_ref(value) {
                    let (path_part, suffix) = match value.find(['?', '#']) {
                        Some(k) => (&value[..k], &value[k..]),
                        None => (value, ""),
                    };
                    let abs = normalize(&dir.join(path_part));
                    out.push_str(&css[copied..i]);
                    out.push_str(&format!("url(\"{}{suffix}\")", url_of(&abs)));
                    copied = close + 1;
                }
                i = close + 1;
            }
            _ => i += 1,
        }
    }
    out.push_str(&css[copied.min(css.len())..]);
    out
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_'
}

fn starts_with_ci(hay: &[u8], needle: &[u8]) -> bool {
    hay.len() >= needle.len() && hay[..needle.len()].eq_ignore_ascii_case(needle)
}

fn strip_query(s: &str) -> &str {
    s.split(['?', '#']).next().unwrap_or(s)
}

/// Index just past a quoted string starting at `i`.
fn skip_string(b: &[u8], i: usize, q: u8) -> usize {
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
fn statement_end(b: &[u8], i: usize) -> usize {
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

/// `"x.css" screen` / `url(x.css) layer(base)` → (`x.css`, `screen` / `layer(base)`).
fn import_target(stmt: &str) -> Option<(&str, &str)> {
    let s = stmt.trim_start();
    let (target, rest) = if let Some(r) = s.strip_prefix(['"', '\'']) {
        let q = s.as_bytes()[0] as char;
        let end = r.find(q)?;
        (&r[..end], &r[end + 1..])
    } else if s.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("url(")) {
        let end = s.find(')')?;
        let inner = s[4..end].trim();
        let inner = inner
            .strip_prefix(['"', '\''])
            .and_then(|x| x.strip_suffix(['"', '\'']))
            .unwrap_or(inner);
        (inner, &s[end + 1..])
    } else {
        return None;
    };
    Some((target, rest.trim()))
}

/// Wraps inlined CSS in the import's conditions: `layer[(name)]`, `supports(...)`, media.
fn wrap(conditions: &str, body: &str) -> String {
    let mut c = conditions.trim();
    let mut open = Vec::new();
    if c.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("layer")) {
        let after = &c[5..];
        if let Some(r) = after.strip_prefix('(')
            && let Some(end) = r.find(')')
        {
            open.push(format!("@layer {}", r[..end].trim()));
            c = r[end + 1..].trim();
        } else if after.is_empty() || after.starts_with(char::is_whitespace) {
            open.push("@layer".to_owned());
            c = after.trim();
        }
    }
    if c.get(..9)
        .is_some_and(|p| p.eq_ignore_ascii_case("supports("))
    {
        let mut depth = 0;
        let mut end = None;
        for (k, ch) in c.char_indices().skip(8) {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(k);
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some(end) = end {
            let cond = &c[9..end];
            let cond = if cond.trim_start().starts_with('(') {
                cond.to_owned()
            } else {
                format!("({cond})")
            };
            open.push(format!("@supports {cond}"));
            c = c[end + 1..].trim();
        }
    }
    if !c.is_empty() {
        open.push(format!("@media {c}"));
    }
    let mut s = String::with_capacity(body.len() + 64);
    for o in &open {
        s.push_str(o);
        s.push_str(" {\n");
    }
    s.push_str(body);
    for _ in &open {
        s.push_str("\n}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn run(files: &[(&str, &str)], entry: &str) -> String {
        let map: HashMap<PathBuf, String> = files
            .iter()
            .map(|(p, c)| (PathBuf::from(p), (*c).to_owned()))
            .collect();
        let read = |p: &Path| map.get(p).cloned();
        let url_of = |p: &Path| asset_url(Path::new("/w/app"), p);
        bundle_css(Path::new(entry), &read, &url_of).unwrap()
    }

    #[test]
    fn inlines_imports_and_rewrites_urls() {
        let out = run(
            &[
                (
                    "/w/node_modules/ui/dist/style.css",
                    "@charset \"utf-8\";\n@import './base.css';\n@import url(\"theme.css\") screen and (min-width: 1px);\n@import 'https://fonts.example/x.css';\n.a { background: url(./img/dot.svg?x#y) } /* url(not-me.png) */ .b { content: \"url(nor-me)\" }\n",
                ),
                (
                    "/w/node_modules/ui/dist/base.css",
                    "@import './style.css';\n.base { src: url('../fonts/f.woff2'), url(data:font/woff2;base64,AA) }\n",
                ),
                ("/w/node_modules/ui/dist/theme.css", ".t { color: red }"),
            ],
            "/w/node_modules/ui/dist/style.css",
        );
        assert!(
            out.starts_with("@import 'https://fonts.example/x.css';\n"),
            "{out}"
        );
        assert!(!out.contains("@charset"), "{out}");
        assert!(
            out.contains(".base { src: url(\"/@ts-r/up/1/node_modules/ui/fonts/f.woff2\"), url(data:font/woff2;base64,AA) }"),
            "{out}"
        );
        // The cycle back to style.css is dropped (it is already included).
        assert!(!out.contains("style.css"), "{out}");
        assert!(
            out.contains("@media screen and (min-width: 1px) {\n.t { color: red }\n}"),
            "{out}"
        );
        assert!(
            out.contains("url(\"/@ts-r/up/1/node_modules/ui/dist/img/dot.svg?x#y\")"),
            "{out}"
        );
        assert!(out.contains("/* url(not-me.png) */"), "{out}");
        assert!(out.contains("\"url(nor-me)\""), "{out}");
    }

    #[test]
    fn layers_and_supports() {
        let out = run(
            &[
                (
                    "/w/app/node_modules/x/a.css",
                    "@import 'b.css' layer(base) supports(display: grid) print;\n@import \"b.css\" layer;",
                ),
                ("/w/app/node_modules/x/b.css", "i { j: k }"),
            ],
            "/w/app/node_modules/x/a.css",
        );
        assert!(
            out.contains(
                "@layer base {\n@supports (display: grid) {\n@media print {\ni { j: k }\n}\n}\n}"
            ),
            "{out}"
        );
        assert!(out.contains("@layer {\ni { j: k }\n}"), "{out}");
    }

    #[test]
    fn asset_urls_have_no_absolute_paths() {
        let root = Path::new("/w/apps/web");
        assert_eq!(
            asset_url(root, Path::new("/w/apps/web/node_modules/a b/x.woff")),
            "/node_modules/a%20b/x.woff"
        );
        assert_eq!(
            asset_url(
                root,
                Path::new("/w/node_modules/.pnpm/f@1/node_modules/f/x.woff")
            ),
            "/@ts-r/up/2/node_modules/.pnpm/f@1/node_modules/f/x.woff"
        );
    }

    #[test]
    fn pathological_input_terminates() {
        let deep: Vec<(String, String)> = (0..100)
            .map(|i| {
                (
                    format!("/w/app/node_modules/d/{i}.css"),
                    format!("@import './{}.css';\n.c{i} {{}}", i + 1),
                )
            })
            .collect();
        let files: Vec<(&str, &str)> = deep.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let out = run(&files, "/w/app/node_modules/d/0.css");
        assert!(out.contains(".c0 {}") && out.contains(".c30 {}"), "{out}");
        // Each file imports the next twice: 2^31 inlines without a total limit.
        let wide: Vec<(String, String)> = (0..=30)
            .map(|i| {
                let text = if i == 30 {
                    ".x{}".to_owned()
                } else {
                    format!("@import './{n}.css';\n@import './{n}.css';\n", n = i + 1)
                };
                (format!("/w/app/node_modules/w/{i}.css"), text)
            })
            .collect();
        let files: Vec<(&str, &str)> = wide.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let out = run(&files, "/w/app/node_modules/w/0.css");
        assert!(out.contains(".x{}") && out.contains("too large"));
        assert!(out.len() < 4 << 20, "{} bytes", out.len());
        for s in [
            "@import",
            "@import '",
            "url(",
            "url(\"x",
            "/* x",
            "\"x",
            "@charset",
        ] {
            let _ = run(
                &[("/w/app/node_modules/d/x.css", s)],
                "/w/app/node_modules/d/x.css",
            );
        }
    }
}
