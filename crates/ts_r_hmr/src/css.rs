//! Dependency CSS for the browser: relative `@import`s are inlined (wrapped in `@media`,
//! `@supports` or `@layer` blocks when the import has conditions) and relative `url()`s
//! become absolute dev-server URLs, because the stylesheet is served from a different URL
//! than the file it came from (a `<style>` tag, or a pre-bundled `/@ts-r/deps/*.css`).
//!
//! Used for CSS inside `node_modules`: imported by app code (`/@modules/pkg/x.css`) and
//! imported by dependencies (the bundler's batch reads CSS through [`bundle_css`]). This is
//! a scanner, not a CSS parser: comments and strings are skipped, everything else is copied.

use std::path::{Component, Path, PathBuf};

use crate::css_refs::{rewrite_refs, skip_string, starts_with_ci, statement_end};
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

/// What a stylesheet's references mean: how its files are read, where an `@import` points
/// and what a `url()` becomes. The dependency rules are [`bundle_css`]'s; the dev server has
/// its own for app CSS (aliases, the public directory, the base).
pub trait CssHost {
    /// A stylesheet's text, `None` if it can't be read.
    fn read(&mut self, file: &Path) -> Option<String>;
    /// The file an `@import` target in `importer` names, `None` to keep the `@import` (it
    /// is hoisted to the top, where `@import` is valid).
    fn import(&mut self, target: &str, importer: &Path) -> Option<PathBuf>;
    /// The new value of a `url()` or `image-set()` reference in `importer` (unquoted,
    /// unescaped), `None` to keep it.
    fn url(&mut self, raw: &str, importer: &Path) -> Option<String>;
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
    Some(bundle_text(file, &css, &mut DepHost { read, url_of }))
}

/// [`bundle_css`] for CSS already read (`file` names it, for relative references), with
/// `host` deciding what imports and urls mean.
pub fn bundle_text(file: &Path, css: &str, host: &mut dyn CssHost) -> String {
    let mut hoisted = Vec::new();
    let mut stack = vec![file.to_path_buf()];
    let mut spent = Spent::default();
    let body = inline(file, css, host, &mut stack, &mut spent, &mut hoisted);
    if hoisted.is_empty() {
        return body;
    }
    let mut out = hoisted.join("\n");
    out.push('\n');
    out.push_str(&body);
    out
}

/// Dependency CSS: relative references only, files under their own directory.
struct DepHost<'a> {
    read: &'a dyn Fn(&Path) -> Option<String>,
    url_of: &'a dyn Fn(&Path) -> String,
}

impl CssHost for DepHost<'_> {
    fn read(&mut self, file: &Path) -> Option<String> {
        (self.read)(file)
    }

    fn import(&mut self, target: &str, importer: &Path) -> Option<PathBuf> {
        let dir = importer.parent().unwrap_or(Path::new("/"));
        is_relative_ref(target).then(|| normalize(&dir.join(strip_query(target))))
    }

    fn url(&mut self, raw: &str, importer: &Path) -> Option<String> {
        if !is_relative_ref(raw) {
            return None;
        }
        let dir = importer.parent().unwrap_or(Path::new("/"));
        let (path_part, suffix) = match raw.find(['?', '#']) {
            Some(k) => (&raw[..k], &raw[k..]),
            None => (raw, ""),
        };
        let abs = normalize(&dir.join(path_part));
        Some(format!("{}{suffix}", (self.url_of)(&abs)))
    }
}

/// The URL a dependency file is served at, without absolute paths: `/<path>` under the
/// root, otherwise `/@ts-r/up/<n>/<path>` (relative to the root's `n`-th ancestor; the server
/// serves only files inside a `node_modules` directory that way).
pub fn asset_url(root: &Path, file: &Path) -> String {
    http::percent_encode_path(&asset_path(root, file))
}

/// [`asset_url`] without the percent-encoding (CSS quotes what needs it).
pub fn asset_path(root: &Path, file: &Path) -> String {
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
        format!("/{rest}")
    } else {
        format!("/@ts-r/up/{up}/{rest}")
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
    host: &mut dyn CssHost,
    stack: &mut Vec<PathBuf>,
    spent: &mut Spent,
    hoisted: &mut Vec<String>,
) -> String {
    let b = css.as_bytes();
    let mut out = String::with_capacity(css.len());
    let mut i = 0;
    // Text from here up to the next statement handled below still needs its references
    // rewritten.
    let mut copied = 0;
    let refs = |out: &mut String, text: &str, host: &mut dyn CssHost| {
        out.push_str(&rewrite_refs(text, &mut |raw| host.url(raw, file)));
    };
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = css[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
            }
            q @ (b'"' | b'\'') => i = skip_string(b, i, q),
            b'@' if starts_with_ci(&b[i + 1..], b"charset") => {
                // `@charset` is only valid first in a file; inlined files drop theirs.
                let end = statement_end(b, i);
                refs(&mut out, &css[copied..i], host);
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
                refs(&mut out, &css[copied..i], host);
                let target = import_target(stmt);
                match target.and_then(|(t, c)| Some((t, c, host.import(t, file)?))) {
                    Some((target, conditions, path)) => {
                        // A cycle is already included; too deep, too much or unreadable
                        // can't be.
                        let cyclic = stack.contains(&path);
                        let within = stack.len() < MAX_DEPTH
                            && spent.imports < MAX_INLINED
                            && spent.bytes < MAX_INLINED_BYTES;
                        if cyclic {
                            // nothing to add
                        } else if let Some(text) = within.then(|| host.read(&path)).flatten() {
                            spent.imports += 1;
                            spent.bytes += text.len();
                            stack.push(path.clone());
                            let body = inline(&path, &text, host, stack, spent, hoisted);
                            stack.pop();
                            out.push_str(&wrap(conditions, &body));
                        } else {
                            out.push_str(&format!(
                                "/* ts-r: @import \"{}\" not inlined (missing, too deep or too large) */",
                                target.replace("*/", "")
                            ));
                        }
                    }
                    None => hoisted.push(format!("@import {stmt};")),
                }
                i = end;
                copied = end;
            }
            _ => i += 1,
        }
    }
    refs(&mut out, &css[copied.min(css.len())..], host);
    out
}

fn strip_query(s: &str) -> &str {
    s.split(['?', '#']).next().unwrap_or(s)
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
            out.contains(".base { src: url('/@ts-r/up/1/node_modules/ui/fonts/f.woff2'), url(data:font/woff2;base64,AA) }"),
            "{out}"
        );
        // The cycle back to style.css is dropped (it is already included).
        assert!(!out.contains("style.css"), "{out}");
        assert!(
            out.contains("@media screen and (min-width: 1px) {\n.t { color: red }\n}"),
            "{out}"
        );
        assert!(
            out.contains("url(/@ts-r/up/1/node_modules/ui/dist/img/dot.svg?x#y)"),
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
