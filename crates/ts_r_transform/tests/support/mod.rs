//! Shared by `tests/fixtures.rs` and the `oracle` example: fixture options and output
//! comparison by AST shape.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use ts_r_ast::SourceKind;
use ts_r_codegen::{CodegenOptions, shape::shape};

const EXTENSIONS: &[&str] = &["ts", "tsx", "mts", "cts"];

pub fn is_fixture(p: &Path) -> bool {
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    !name.ends_with(".d.ts")
        && p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| EXTENSIONS.contains(&e))
}

/// `a.ts` → `a.js`, `a.mts` → `a.mjs`, `a.cts` → `a.cjs`, `a.tsx` → `a.js` (or `.jsx`
/// with `jsx: preserve`; the oracle does not use that).
pub fn js_name(name: &str) -> String {
    for (from, to) in [
        (".mts", ".mjs"),
        (".cts", ".cjs"),
        (".tsx", ".js"),
        (".ts", ".js"),
    ] {
        if let Some(stem) = name.strip_suffix(from) {
            return format!("{stem}{to}");
        }
    }
    name.to_owned()
}

/// Where `--bless` stores tsc's output for a fixture.
pub fn stored_path(f: &Path) -> PathBuf {
    let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
    f.with_file_name(format!("{name}.tsc.js"))
}

pub fn hash_str(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// The `compilerOptions` JSON of a fixture's `// @name: value` header lines.
pub fn options_json(src: &str) -> String {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for line in src.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let Some(rest) = t.strip_prefix("//") else {
            break;
        };
        let rest = rest.trim();
        let Some(rest) = rest.strip_prefix('@') else {
            continue;
        };
        let Some((name, value)) = rest.split_once(':') else {
            continue;
        };
        let Some(decl) = ts_r_config::options::find_option(name.trim())
            .or_else(|| ts_r_config::options::find_option_ignore_case(name.trim()))
        else {
            continue;
        };
        let name = decl.name.to_owned();
        let value = value.trim();
        use ts_r_config::options::OptionType as T;
        let v = match decl.ty {
            T::Bool => value.to_ascii_lowercase(),
            T::Number => value.to_owned(),
            T::StringList | T::PathList | T::Lib => {
                let items: Vec<String> = value
                    .split(',')
                    .map(|s| format!("\"{}\"", s.trim()))
                    .collect();
                format!("[{}]", items.join(", "))
            }
            T::PathMap | T::Plugins => continue,
            _ => format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"")),
        };
        pairs.retain(|(n, _)| *n != name);
        pairs.push((name, v));
    }
    let body: Vec<String> = pairs.iter().map(|(n, v)| format!("\"{n}\": {v}")).collect();
    format!("{{{}}}", body.join(", "))
}

/// Blanks the absolute path in `const _jsxFileName = "...";` (it names a temporary file).
fn blank_jsx_file_name(js: &str) -> String {
    const NEEDLE: &str = "const _jsxFileName = \"";
    match js.find(NEEDLE) {
        Some(i) => {
            let start = i + NEEDLE.len();
            let end = js[start..].find('"').map_or(start, |e| start + e);
            format!("{}{}", &js[..start], &js[end..])
        }
        None => js.to_owned(),
    }
}

/// Parses JavaScript and returns (shape, normalized text without comments).
pub fn normalize(js: &str) -> (String, String) {
    let js = &blank_jsx_file_name(js);
    let parsed = ts_r_parser::parse(js, SourceKind::Js);
    let opts = CodegenOptions {
        comments: false,
        ..CodegenOptions::default()
    };
    let text = ts_r_codegen::print(&parsed.ast, js, &opts).code;
    (shape(&parsed.ast), text)
}

/// `None` if `ours` and `theirs` have the same AST shape, else a short diff.
pub fn compare(ours: &str, theirs: &str) -> Option<String> {
    let (a, at) = normalize(ours);
    let (b, bt) = normalize(theirs);
    if a == b {
        return None;
    }
    let al: Vec<&str> = at.lines().collect();
    let bl: Vec<&str> = bt.lines().collect();
    let first = al
        .iter()
        .zip(bl.iter())
        .position(|(x, y)| x != y)
        .unwrap_or(al.len().min(bl.len()));
    let mut out = String::new();
    let from = first.saturating_sub(2);
    for (i, l) in bl.iter().enumerate().skip(from).take(8) {
        out.push_str(&format!("  tsc  {:>4}| {l}\n", i + 1));
    }
    for (i, l) in al.iter().enumerate().skip(from).take(8) {
        out.push_str(&format!("  ours {:>4}| {l}\n", i + 1));
    }
    if at == bt {
        out.push_str("  (same text, different shape)\n");
    }
    Some(out)
}
