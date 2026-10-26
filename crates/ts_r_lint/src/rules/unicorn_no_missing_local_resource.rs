//! `unicorn/no-missing-local-resource`: `new URL("<relative path>", import.meta.url)` must
//! name a file (or, with a trailing `/`, a directory) that exists next to the linted file.
//!
//! The first argument is a static string (or a template without expressions) without tabs or
//! line breaks; TS wrappers are looked through on both arguments. The value is trimmed of
//! control characters and spaces and cut at the first `?` or `#`; empty, root-relative
//! (`/x`) and scheme (`x:`) paths are skipped, as are paths whose percent-decoded parts hold
//! a slash. The decoded path is normalised like `path.posix.normalize` (keeping a leading
//! `./`) and looked up one directory entry at a time from the file's directory: an entry
//! that only matches ignoring case is reported with the correctly cased path (fixed in place
//! when the literal is written as its value, the path needed no normalising and no part
//! changes length), a missing one as missing. When a directory cannot be read the plain
//! existence of the whole path decides. Files without a real path (`<input>`, `<text>`) are
//! skipped. HTML, CSS and Markdown files are not linted here.

use super::prelude::*;
use super::unicorn_text::static_string_value;
use super::unicorn_util::skip_wrappers;
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use ts_r_ast::MetaPropKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-missing-local-resource",
        "Disallow references to missing local resources.",
        NO_PRESET,
        Category::Correctness,
    )
    .fixable(),
    create: create_default::<NoMissingLocalResource>,
};

#[derive(Debug, Default)]
struct NoMissingLocalResource;

/// The `start..end` byte range of `value` without leading and trailing characters up to
/// U+0020.
fn trimmed_range(value: &str) -> (usize, usize) {
    let start = value.len() - value.trim_start_matches(|c| c <= ' ').len();
    let end = value.trim_end_matches(|c| c <= ' ').len().max(start);
    (start, end)
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// `decodeURIComponent`: `None` when an escape is malformed or not UTF-8.
fn decode_uri_component(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let escape = |i: usize| -> Option<u8> {
        if bytes.get(i) != Some(&b'%') {
            return None;
        }
        Some(hex(*bytes.get(i + 1)?)? * 16 + hex(*bytes.get(i + 2)?)?)
    };
    while i < bytes.len() {
        if bytes[i] != b'%' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let lead = escape(i)?;
        let n = match lead {
            0x00..=0x7F => 1,
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => return None,
        };
        let mut seq = vec![lead];
        for k in 1..n {
            seq.push(escape(i + 3 * k)?);
        }
        std::str::from_utf8(&seq).ok()?;
        out.extend_from_slice(&seq);
        i += 3 * n;
    }
    String::from_utf8(out).ok()
}

/// Decodes `value`, or failing that each run of escapes on its own (a run that does not
/// decode stays as written).
fn decode_percent(value: &str) -> String {
    if let Some(d) = decode_uri_component(value) {
        return d;
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let run = percent_run_len(&rest[start..]);
        if run == 0 {
            out.push('%');
            rest = &rest[start + 1..];
            continue;
        }
        let encoded = &rest[start..start + run];
        match decode_uri_component(encoded) {
            Some(d) => out.push_str(&d),
            None => out.push_str(encoded),
        }
        rest = &rest[start + run..];
    }
    out.push_str(rest);
    out
}

/// The length of the run of `%XX` escapes `s` starts with.
fn percent_run_len(s: &str) -> usize {
    let b = s.as_bytes();
    let mut n = 0;
    while b.get(n) == Some(&b'%')
        && b.get(n + 1).is_some_and(u8::is_ascii_hexdigit)
        && b.get(n + 2).is_some_and(u8::is_ascii_hexdigit)
    {
        n += 3;
    }
    n
}

fn encode_bytes(s: &str) -> String {
    s.bytes().map(|b| format!("%{b:02X}")).collect()
}

/// `encodeURIComponent`.
fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || "-_.!~*'()".contains(c) {
            out.push(c);
        } else {
            let mut buf = [0; 4];
            out.push_str(&encode_bytes(c.encode_utf8(&mut buf)));
        }
    }
    out
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `path.posix.normalize` for a relative path.
fn posix_normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }
    let trailing = path.ends_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|&p| p != "..") {
                    parts.pop();
                } else {
                    parts.push("..");
                }
            }
            p => parts.push(p),
        }
    }
    let mut out = parts.join("/");
    if out.is_empty() {
        return if trailing { "./" } else { "." }.to_owned();
    }
    if trailing {
        out.push('/');
    }
    out
}

struct LocalResource {
    /// The decoded, normalised path to look up.
    path: String,
    /// The path as written (before `?` / `#`).
    raw_path: String,
    /// The `?...` / `#...` rest.
    suffix: String,
    normalized: bool,
}

fn local_resource(value: &str) -> Option<LocalResource> {
    let (s, e) = trimmed_range(value);
    let trimmed = &value[s..e];
    let path_end = trimmed.find(['?', '#']).unwrap_or(trimmed.len());
    let raw_path = &trimmed[..path_end];
    let slashed = raw_path.replace('\\', "/");
    if slashed.is_empty() || slashed.starts_with('/') || has_scheme(&slashed) {
        return None;
    }
    let parts: Vec<String> = slashed.split('/').map(decode_percent).collect();
    if parts.iter().any(|p| p.contains(['/', '\\'])) {
        return None;
    }
    let decoded = parts.join("/");
    let normal = posix_normalize(&decoded);
    let path = if raw_path.starts_with("./") && !normal.starts_with('.') {
        format!("./{normal}")
    } else {
        normal
    };
    let normalized = slashed != raw_path || path != decoded;
    Some(LocalResource {
        path,
        raw_path: raw_path.to_owned(),
        suffix: trimmed[path_end..].to_owned(),
        normalized,
    })
}

/// `^[a-z][\d+\-.a-z]*:`, any case.
fn has_scheme(s: &str) -> bool {
    let mut chars = s.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    for c in chars {
        if c == ':' {
            return true;
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
            return false;
        }
    }
    false
}

/// `raw_part` written with the casing of `corrected`, keeping its escapes where possible.
fn corrected_part(raw_part: &str, corrected: &str) -> String {
    let decoded = decode_percent(raw_part);
    if decoded == corrected {
        return raw_part.to_owned();
    }
    if utf16_len(&decoded) != utf16_len(corrected) {
        return encode_uri_component(corrected);
    }
    // Each escape run covers the same UTF-16 range of the decoded and the corrected text;
    // runs are replaced from the end so earlier offsets stay valid.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while let Some(off) = raw_part[i..].find('%') {
        let start = i + off;
        let len = percent_run_len(&raw_part[start..]);
        if len == 0 {
            i = start + 1;
            continue;
        }
        runs.push((start, len));
        i = start + len;
    }
    let mut result: Vec<u16> = corrected.encode_utf16().collect();
    for &(at, len) in runs.iter().rev() {
        let encoded = &raw_part[at..at + len];
        let start = utf16_len(&decode_percent(&raw_part[..at]));
        let value: Vec<u16> = decode_percent(encoded).encode_utf16().collect();
        let end = (start + value.len()).min(result.len());
        let start = start.min(end);
        let fixed = &result[start..end];
        let replacement: Vec<u16> = if fixed == value.as_slice() {
            encoded.encode_utf16().collect()
        } else {
            encode_bytes(&String::from_utf16_lossy(fixed))
                .encode_utf16()
                .collect()
        };
        result.splice(start..end, replacement);
    }
    String::from_utf16_lossy(&result)
}

fn corrected_path(raw_path: &str, corrected: &str) -> String {
    let fixed: Vec<&str> = corrected.split('/').collect();
    raw_path
        .split('/')
        .enumerate()
        .map(|(i, part)| corrected_part(part, fixed.get(i).copied().unwrap_or("")))
        .collect::<Vec<_>>()
        .join("/")
}

fn changes_length(raw_path: &str, corrected: &str) -> bool {
    let fixed: Vec<&str> = corrected.split('/').collect();
    raw_path.split('/').enumerate().any(|(i, part)| {
        utf16_len(&decode_percent(part)) != fixed.get(i).map_or(0, |p| utf16_len(p))
    })
}

/// A directory's entries: the exact names, and by lowercase name the one entry (or `None`
/// when several differ only in case).
struct Entries {
    exact: Vec<String>,
    insensitive: FxHashMap<String, Option<String>>,
}

enum Lookup {
    Missing,
    Exists { corrected: Option<String> },
}

#[derive(Default)]
struct Finder {
    dirs: FxHashMap<PathBuf, Option<Entries>>,
}

impl Finder {
    fn entries(&mut self, dir: &Path) -> Option<&Entries> {
        self.dirs
            .entry(dir.to_path_buf())
            .or_insert_with(|| {
                let mut exact = Vec::new();
                let mut insensitive: FxHashMap<String, Option<String>> = FxHashMap::default();
                for entry in std::fs::read_dir(dir).ok()? {
                    let name = entry.ok()?.file_name().to_string_lossy().into_owned();
                    let lower = name.to_lowercase();
                    match insensitive.get_mut(&lower) {
                        Some(slot) => *slot = None,
                        None => {
                            insensitive.insert(lower, Some(name.clone()));
                        }
                    }
                    exact.push(name);
                }
                Some(Entries { exact, insensitive })
            })
            .as_ref()
    }

    fn lookup(&mut self, base: &Path, path: &str) -> Lookup {
        let mut dir = base.to_path_buf();
        let mut corrected: Vec<String> = Vec::new();
        let mut wrong_case = false;
        for part in path.split('/') {
            if matches!(part, "" | "." | "..") {
                if !part.is_empty() {
                    dir.push(part);
                }
                corrected.push(part.to_owned());
                continue;
            }
            let Some(entries) = self.entries(&dir) else {
                return if base.join(path).exists() {
                    Lookup::Exists { corrected: None }
                } else {
                    Lookup::Missing
                };
            };
            let found = if entries.exact.iter().any(|e| e == part) {
                Some(part.to_owned())
            } else {
                entries
                    .insensitive
                    .get(&part.to_lowercase())
                    .cloned()
                    .flatten()
            };
            let Some(found) = found else {
                return Lookup::Missing;
            };
            wrong_case |= found != part;
            dir.push(&found);
            corrected.push(found);
        }
        let exists = if path.ends_with('/') {
            dir.is_dir()
        } else {
            dir.exists()
        };
        if !exists {
            return Lookup::Missing;
        }
        Lookup::Exists {
            corrected: wrong_case.then(|| corrected.join("/")),
        }
    }
}

fn is_import_meta_url(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(id)
    else {
        return false;
    };
    matches!(ast.node(prop), Node::Ident { .. })
        && ident_name(ast, prop) == Some("url")
        && matches!(
            ast.node(skip_parens(ast, obj)),
            Node::MetaProperty {
                kind: MetaPropKind::ImportMeta
            }
        )
}

impl Rule for NoMissingLocalResource {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::New { callee, args, .. } = *ast.node(node) else {
            return;
        };
        let &[resource, base] = ast.list(args) else {
            return;
        };
        if !matches!(ast.node(callee), Node::Ident { .. })
            || ident_name(ast, callee) != Some("URL")
            || matches!(ast.node(resource), Node::Spread { .. })
            || matches!(ast.node(base), Node::Spread { .. })
        {
            return;
        }
        let resource = skip_wrappers(ast, resource);
        if !is_import_meta_url(ast, skip_wrappers(ast, base)) {
            return;
        }
        let path = cx.file.path;
        if matches!(path, "<input>" | "<text>") {
            return;
        }
        let Some(value) = static_string_value(ast, resource) else {
            return;
        };
        if value.contains(['\t', '\n', '\r']) {
            return;
        }
        let span = cx.span(resource);
        let raw = span.text(cx.src());
        let value_start = (raw.len() >= 2 && raw.get(1..raw.len() - 1) == Some(value.as_str()))
            .then_some(span.start + 1);
        let (lead, end) = trimmed_range(&value);
        let text = &value[lead..end];
        let Some(local) = local_resource(text) else {
            return;
        };
        let range =
            value_start.map(|s| Span::new(s + lead as u32, s + lead as u32 + text.len() as u32));
        let report_span = range.unwrap_or(span);
        let base_dir = Path::new(path).parent().unwrap_or(Path::new(""));
        match Finder::default().lookup(base_dir, &local.path) {
            Lookup::Missing => {
                cx.report(report_span, format!("Resource `{text}` does not exist."))
                    .id("missing");
            }
            Lookup::Exists { corrected: None } => {}
            Lookup::Exists {
                corrected: Some(corrected),
            } => {
                let fixed_path = if local.normalized {
                    corrected.clone()
                } else {
                    corrected_path(&local.raw_path, &corrected)
                };
                let fixed = format!("{fixed_path}{}", local.suffix);
                let fix = (!local.normalized && !changes_length(&local.raw_path, &corrected))
                    .then_some(range)
                    .flatten()
                    .map(|r| Fix::replace(r, fixed.clone()));
                cx.report(
                    report_span,
                    format!("Resource path has incorrect casing. Use `{fixed}`."),
                )
                .id("incorrect-case")
                .fix_opt(fix);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_like_posix() {
        assert_eq!(posix_normalize("./a/b"), "a/b");
        assert_eq!(posix_normalize("a/../../b/"), "../b/");
        assert_eq!(posix_normalize("./"), "./");
        assert_eq!(posix_normalize("a/.."), ".");
    }

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(decode_percent("a%20b"), "a b");
        assert_eq!(decode_percent("%E2%82%AC"), "€");
        assert_eq!(decode_percent("%zz%41"), "%zzA");
        assert_eq!(decode_percent("%FF%41"), "%FF%41");
    }

    fn lint(path: &str, src: &str) -> Vec<String> {
        use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};
        let reg = crate::Registry::global();
        let json = serde_json::json!([{ "files": ["**/*.js"], "rules": {
            "unicorn/no-missing-local-resource": "error" } }]);
        let objs = objects_from_eslint_json(&json, reg).unwrap();
        let file = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let fc = file.file_config("a.js", &ResolveOptions::new()).unwrap();
        crate::lint_source(path, src, &fc, &crate::NoTypeInfo, crate::types::FileKey(0))
            .diagnostics
            .into_iter()
            .map(|d| d.message)
            .collect()
    }

    #[test]
    fn looks_resources_up_on_disk() {
        let dir = std::env::temp_dir().join(format!("ts-r-nmlr-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("assets/sub")).unwrap();
        std::fs::write(dir.join("assets/Logo.svg"), "").unwrap();
        let file = dir.join("a.js");
        let file = file.to_str().unwrap();
        let run = |s: &str| lint(file, &format!("new URL({s}, import.meta.url);"));
        assert!(run("\"./assets/Logo.svg#x\"").is_empty());
        assert!(run("\"./assets/sub/\"").is_empty());
        assert_eq!(
            run("\"./assets/logo.svg?raw\""),
            ["Resource path has incorrect casing. Use `./assets/Logo.svg?raw`."]
        );
        assert_eq!(
            run("\"./assets/Logo.svg/\""),
            ["Resource `./assets/Logo.svg/` does not exist."]
        );
        assert_eq!(
            run("\"./nope.svg\""),
            ["Resource `./nope.svg` does not exist."]
        );
        assert!(run("\"https://x/y\"").is_empty());
        assert!(lint("<input>", "new URL('./nope', import.meta.url);").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keeps_escapes_when_fixing_case() {
        assert_eq!(
            corrected_part("Encoded%20Name.md", "encoded name.md"),
            "encoded%20name.md"
        );
        assert_eq!(corrected_part("LOGO.svg", "logo.svg"), "logo.svg");
    }
}
