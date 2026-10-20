//! `import.meta.env`, `.env` files and `define` (Vite's dev semantics, observed on Vite
//! 8.3.4; no Vite code).
//!
//! - Files, lowest to highest priority: `.env`, `.env.local`, `.env.[mode]`,
//!   `.env.[mode].local` in the env directory (the root unless `envDir`). Values may use
//!   `$VAR`, `${VAR}`, `${VAR:-default}` and `${VAR-default}` (process variables first, then
//!   the files' own), `\$` for a literal `$`. Only keys starting with an `envPrefix` (default
//!   `VITE_`) reach the browser; process environment variables with a prefix override the
//!   files.
//! - Modules that mention `import.meta.env` get `import.meta.env = {...};` prepended on
//!   line 1: the variables plus `BASE_URL`, `MODE`, `DEV`, `PROD`, `SSR` and every
//!   `define` key under `import.meta.env.`, sorted by key, values as JS expressions.
//! - Other `define` keys become globals set by the client before any module runs (served
//!   at `/@vite/env` too), as Vite does in dev; `process.env.NODE_ENV` is also replaced
//!   inline (with `define`'s value, else the `NODE_ENV` string).
//! - `%NAME%` in HTML is replaced with the value of `import.meta.env.NAME` when it exists.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::render::js_string;

/// What `.env` loading and `define` need.
#[derive(Clone, Debug)]
pub struct EnvOptions {
    /// Directory holding the `.env*` files; `None` = don't load files.
    pub dir: Option<PathBuf>,
    pub mode: String,
    pub prefixes: Vec<String>,
    /// The public base path (`BASE_URL`).
    pub base: String,
    /// `define` entries: key → JS expression (strings already JSON-quoted by the caller
    /// when they were non-string values).
    pub define: Vec<(String, String)>,
}

/// The computed environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    /// `import.meta.env` keys → JS expressions, sorted.
    pub meta: BTreeMap<String, String>,
    /// String values of `import.meta.env` for `%NAME%` in HTML.
    pub html_values: BTreeMap<String, String>,
    /// `import.meta.env = {...};`
    pub prelude: String,
    /// Code that sets the global `define`s (empty when there are none).
    pub globals: String,
    /// The replacement for `process.env.NODE_ENV` in source modules.
    pub node_env: String,
    /// The `.env*` files that were considered (existing or not), for watching.
    pub files: Vec<PathBuf>,
}

/// The env file names for `mode`, lowest priority first.
pub fn file_names(mode: &str) -> [String; 4] {
    [
        ".env".to_owned(),
        ".env.local".to_owned(),
        format!(".env.{mode}"),
        format!(".env.{mode}.local"),
    ]
}

/// Whether `path` is one of the env files (any mode) in `dir`.
pub fn is_env_file(dir: &Path, path: &Path) -> bool {
    path.parent() == Some(dir)
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == ".env" || n.starts_with(".env."))
}

/// Checks `envPrefix` the way Vite does: an empty prefix would expose every variable.
pub fn check_prefixes(prefixes: &[String]) -> Result<(), String> {
    if prefixes.iter().any(String::is_empty) {
        return Err("envPrefix contains an empty string, which would expose every environment variable to the browser".to_owned());
    }
    Ok(())
}

/// Loads the env files and computes everything the server injects.
pub fn load(opts: &EnvOptions) -> Env {
    load_with(
        opts,
        &|k| std::env::var(k).ok(),
        &std::env::vars().collect::<Vec<_>>(),
    )
}

/// [`load`] with an explicit process environment (tests).
pub fn load_with(
    opts: &EnvOptions,
    process_var: &dyn Fn(&str) -> Option<String>,
    process_vars: &[(String, String)],
) -> Env {
    let mut parsed: Vec<(String, String)> = Vec::new();
    let mut files = Vec::new();
    if let Some(dir) = &opts.dir {
        for name in file_names(&opts.mode) {
            let path = dir.join(&name);
            if let Ok(text) = std::fs::read_to_string(&path) {
                for (k, v) in parse(&text) {
                    match parsed.iter_mut().find(|(pk, _)| *pk == k) {
                        Some(slot) => slot.1 = v,
                        None => parsed.push((k, v)),
                    }
                }
            }
            files.push(path);
        }
    }
    let expanded = expand(&parsed, process_var);
    let has_prefix = |k: &str| opts.prefixes.iter().any(|p| k.starts_with(p.as_str()));
    let mut vars: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in &expanded {
        if has_prefix(k) {
            vars.insert(k.clone(), v.clone());
        }
    }
    for (k, v) in process_vars {
        if has_prefix(k) {
            vars.insert(k.clone(), v.clone());
        }
    }
    let node_env_value = process_var("NODE_ENV")
        .or_else(|| {
            expanded
                .iter()
                .find(|(k, _)| k == "NODE_ENV")
                .map(|(_, v)| v.clone())
        })
        .unwrap_or_else(|| "development".to_owned());
    let prod = node_env_value == "production";

    let mut meta: BTreeMap<String, String> = BTreeMap::new();
    let mut html_values: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in &vars {
        meta.insert(k.clone(), js_string(v));
        html_values.insert(k.clone(), v.clone());
    }
    for (k, v) in [
        ("BASE_URL", js_string(&opts.base)),
        ("MODE", js_string(&opts.mode)),
        ("DEV", (!prod).to_string()),
        ("PROD", prod.to_string()),
        ("SSR", "false".to_owned()),
    ] {
        html_values.insert(
            k.to_owned(),
            match k {
                "BASE_URL" => opts.base.clone(),
                "MODE" => opts.mode.clone(),
                _ => v.clone(),
            },
        );
        meta.insert(k.to_owned(), v);
    }
    let mut globals: BTreeMap<&str, &str> = BTreeMap::new();
    let mut node_env = js_string(&node_env_value);
    for (k, expr) in &opts.define {
        if let Some(name) = k.strip_prefix("import.meta.env.") {
            meta.insert(name.to_owned(), expr.clone());
            if let Ok(serde_json::Value::String(s)) = serde_json::from_str(expr) {
                html_values.insert(name.to_owned(), s);
            }
            continue;
        }
        if k == "process.env.NODE_ENV" {
            node_env = expr.clone();
        }
        globals.insert(k, expr);
    }
    let mut prelude = String::from("import.meta.env = {");
    for (i, (k, v)) in meta.iter().enumerate() {
        if i > 0 {
            prelude.push_str(", ");
        }
        prelude.push_str(&js_string(k));
        prelude.push_str(": ");
        prelude.push_str(v);
    }
    prelude.push_str("};");
    let globals = if globals.is_empty() {
        String::new()
    } else {
        let mut s = String::from("const __tsr_defines = {");
        for (i, (k, v)) in globals.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(&js_string(k));
            s.push_str(": ");
            s.push_str(v);
        }
        s.push_str(
            "};\nfor (const key of Object.keys(__tsr_defines)) {\n  const path = key.split(\".\");\n  let target = globalThis;\n  for (const part of path.slice(0, -1)) target = target[part] ??= {};\n  target[path[path.length - 1]] = __tsr_defines[key];\n}\n",
        );
        s
    };
    Env {
        meta,
        html_values,
        prelude,
        globals,
        node_env,
        files,
    }
}

/// Replaces `%NAME%` in HTML with `import.meta.env.NAME` where it exists.
pub fn replace_html(html: &str, env: &Env) -> String {
    if !html.contains('%') {
        return html.to_owned();
    }
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let end = after.find(|c: char| c == '%' || c.is_whitespace());
        match end {
            Some(j) if after.as_bytes()[j] == b'%' && j > 0 => {
                let name = &after[..j];
                match env.html_values.get(name) {
                    Some(v) => out.push_str(v),
                    None => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[j + 1..];
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Parses dotenv text into `(key, value)` pairs in order (later duplicates override).
pub fn parse(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let src = text.replace("\r\n", "\n");
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        // One logical line: skip leading whitespace.
        while i < b.len() && matches!(b[i], b' ' | b'\t') {
            i += 1;
        }
        let line_end = src[i..].find('\n').map_or(b.len(), |j| i + j);
        let mut p = i;
        if src[p..line_end].starts_with("export ") || src[p..line_end].starts_with("export\t") {
            p += 7;
            while p < line_end && matches!(b[p], b' ' | b'\t') {
                p += 1;
            }
        }
        let key_start = p;
        while p < line_end && (b[p].is_ascii_alphanumeric() || matches!(b[p], b'_' | b'.' | b'-')) {
            p += 1;
        }
        let key = &src[key_start..p];
        while p < line_end && matches!(b[p], b' ' | b'\t') {
            p += 1;
        }
        // `KEY=value`, or YAML-ish `KEY: value`.
        let equals = p < line_end && b[p] == b'=';
        let colon = p + 1 < line_end && b[p] == b':' && matches!(b[p + 1], b' ' | b'\t');
        let sep = (equals || colon).then_some(p + 1);
        let (Some(mut v), false) = (sep, key.is_empty()) else {
            i = line_end + 1;
            continue;
        };
        while v < line_end && matches!(b[v], b' ' | b'\t') {
            v += 1;
        }
        let quote = b
            .get(v)
            .copied()
            .filter(|q| matches!(q, b'"' | b'\'' | b'`'));
        let (value, next) = match quote {
            Some(q) => {
                // A quoted value may span lines; it ends at the next unescaped quote.
                let mut j = v + 1;
                let mut close = None;
                while j < b.len() {
                    if b[j] == b'\\' && j + 1 < b.len() && b[j + 1] == q {
                        j += 2;
                        continue;
                    }
                    if b[j] == q {
                        close = Some(j);
                        break;
                    }
                    j += 1;
                }
                match close {
                    Some(c) => {
                        let inner = &src[v + 1..c];
                        let value = if q == b'"' {
                            inner.replace("\\n", "\n").replace("\\r", "\r")
                        } else {
                            inner.to_owned()
                        };
                        let after_close = src[c..].find('\n').map_or(b.len(), |k| c + k);
                        (value, after_close + 1)
                    }
                    None => {
                        // Unterminated: the rest of the line, as an unquoted value.
                        let raw = &src[v..line_end];
                        let raw = raw.split('#').next().unwrap_or(raw);
                        (raw.trim().to_owned(), line_end + 1)
                    }
                }
            }
            None => {
                let raw = &src[v..line_end];
                let raw = match raw.find('#') {
                    Some(h) => &raw[..h],
                    None => raw,
                };
                (raw.trim().to_owned(), line_end + 1)
            }
        };
        match out.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => out.push((key.to_owned(), value)),
        }
        i = next;
    }
    out
}

/// Expands `$VAR`, `${VAR}`, `${VAR:-default}`, `${VAR-default}` and `\$` in parsed values.
/// A key the process environment also sets (with a different value) takes that value.
pub fn expand(
    parsed: &[(String, String)],
    process_var: &dyn Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    let mut running: Vec<(String, String)> = parsed.to_vec();
    for i in 0..running.len() {
        let key = running[i].0.clone();
        let raw = running[i].1.clone();
        let value = match process_var(&key) {
            Some(p) if p != raw => p,
            _ => expand_value(&raw, process_var, &running, 0),
        };
        running[i].1 = value;
    }
    running
}

fn lookup(
    name: &str,
    process_var: &dyn Fn(&str) -> Option<String>,
    running: &[(String, String)],
) -> Option<String> {
    process_var(name).or_else(|| {
        running
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    })
}

fn expand_value(
    value: &str,
    process_var: &dyn Fn(&str) -> Option<String>,
    running: &[(String, String)],
    depth: u32,
) -> String {
    let b = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && b.get(i + 1) == Some(&b'$') {
            out.push('$');
            i += 2;
            continue;
        }
        if b[i] != b'$' {
            let ch = value[i..].chars().next().unwrap_or('\u{fffd}');
            out.push(ch);
            i += ch.len_utf8().max(1);
            continue;
        }
        if b.get(i + 1) == Some(&b'{') {
            let Some(close) = value[i + 2..].find('}') else {
                out.push('$');
                i += 1;
                continue;
            };
            let inner = &value[i + 2..i + 2 + close];
            let (name, default) = match inner.find(":-").or_else(|| inner.find('-')) {
                Some(d) => {
                    let skip = if inner[d..].starts_with(":-") { 2 } else { 1 };
                    (&inner[..d], Some(&inner[d + skip..]))
                }
                None => (inner, None),
            };
            let found = lookup(name, process_var, running).filter(|v| !v.is_empty());
            let v = match (found, default) {
                (Some(v), _) => v,
                (None, Some(d)) if depth < 8 => expand_value(d, process_var, running, depth + 1),
                _ => String::new(),
            };
            out.push_str(&v);
            i += 2 + close + 1;
            continue;
        }
        let start = i + 1;
        let mut end = start;
        while end < b.len() && (b[end].is_ascii_alphanumeric() || b[end] == b'_') {
            end += 1;
        }
        if end == start {
            out.push('$');
            i += 1;
            continue;
        }
        out.push_str(&lookup(&value[start..end], process_var, running).unwrap_or_default());
        i = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn parses_dotenv() {
        let text = "A=1\n# comment\nexport B = two \nC=\"quoted # not comment\"\nD='single'\nE=x # trailing\nF=\"l1\nl2\"\nG=\"a\\nb\"\n  H: colon\nbad line\nA=override\nI=\n";
        let p = parse(text);
        let get = |k: &str| p.iter().find(|(x, _)| x == k).map(|(_, v)| v.as_str());
        assert_eq!(get("A"), Some("override"));
        assert_eq!(get("B"), Some("two"));
        assert_eq!(get("C"), Some("quoted # not comment"));
        assert_eq!(get("D"), Some("single"));
        assert_eq!(get("E"), Some("x"));
        assert_eq!(get("F"), Some("l1\nl2"));
        assert_eq!(get("G"), Some("a\nb"));
        assert_eq!(get("H"), Some("colon"));
        assert_eq!(get("I"), Some(""));
        assert_eq!(p[0].0, "A"); // first position kept
        // Never panics on junk.
        let _ = parse("=\n\"\nX=\"unterminated\n'\n`\u{0}");
    }

    #[test]
    fn expands_values() {
        let parsed = parse(
            "A=from-a\nB=${A}-b\nC=$A$A\nD=${MISSING:-fallback}\nE=\\$literal\nF=${HOME_X}\nG=${MISSING-x}\n",
        );
        let process = |k: &str| (k == "HOME_X").then(|| "/home/x".to_owned());
        let e = expand(&parsed, &process);
        let get = |k: &str| e.iter().find(|(x, _)| x == k).map(|(_, v)| v.as_str());
        assert_eq!(get("B"), Some("from-a-b"));
        assert_eq!(get("C"), Some("from-afrom-a"));
        assert_eq!(get("D"), Some("fallback"));
        assert_eq!(get("E"), Some("$literal"));
        assert_eq!(get("F"), Some("/home/x"));
        assert_eq!(get("G"), Some("x"));
    }

    /// The fixture Vite 8.3.4 was observed on (see AGENTS.md): same files, same output.
    #[test]
    fn matches_vite_precedence_and_prelude() {
        let dir = TempDir::new("env");
        dir.write(
            ".env",
            "VITE_A=from-env\nVITE_B=b-env\nSECRET=nope\nAPP_B=app-b\nVITE_EXP=${VITE_A}-expanded\n",
        );
        dir.write(".env.local", "VITE_A=from-local\n");
        dir.write(".env.development", "VITE_A=from-dev\nVITE_B=b-dev\n");
        dir.write(".env.development.local", "VITE_B=b-dev-local\n");
        dir.write(".env.production", "VITE_A=prod\n");
        let opts = EnvOptions {
            dir: Some(dir.path().to_path_buf()),
            mode: "development".into(),
            prefixes: vec!["VITE_".into(), "APP_".into()],
            base: "/".into(),
            define: vec![
                ("__APP_VERSION__".into(), "\"1.2.3\"".into()),
                ("import.meta.env.NUMBER".into(), "7".into()),
                ("process.env.OTHER".into(), "\"other\"".into()),
            ],
        };
        let none = |_: &str| None;
        let env = load_with(&opts, &none, &[("VITE_FROM_SHELL".into(), "sh".into())]);
        assert_eq!(
            env.prelude,
            r#"import.meta.env = {"APP_B": "app-b", "BASE_URL": "/", "DEV": true, "MODE": "development", "NUMBER": 7, "PROD": false, "SSR": false, "VITE_A": "from-dev", "VITE_B": "b-dev-local", "VITE_EXP": "from-dev-expanded", "VITE_FROM_SHELL": "sh"};"#
        );
        assert!(
            env.globals.starts_with(
                r#"const __tsr_defines = {"__APP_VERSION__": "1.2.3", "process.env.OTHER": "other"};"#
            ),
            "{}",
            env.globals
        );
        assert_eq!(env.node_env, "\"development\"");
        assert_eq!(
            replace_html(
                "<title>%VITE_A% %MODE% %SECRET% 100% %BASE_URL%</title>",
                &env
            ),
            "<title>from-dev development %SECRET% 100% /</title>"
        );
        assert!(is_env_file(dir.path(), &dir.path().join(".env.staging")));
        assert!(!is_env_file(dir.path(), &dir.path().join("x.env")));
        assert!(check_prefixes(&["".into()]).is_err());
    }
}
