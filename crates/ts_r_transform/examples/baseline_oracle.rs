//! Compares ts-r's transpile output with the emit baselines of the TypeScript test suite
//! (`tests/baselines/reference/*.js`, produced by tsc 7).
//!
//! ```text
//! scripts/limit.sh --mem-mb 4096 --time-s 600 -- cargo run --release -p ts_r_transform \
//!   --example baseline_oracle -- [--upstream DIR] [--show N] [--filter S] [--list FILE]
//! ```
//!
//! Each baseline holds the test's input files followed by tsc's outputs. Options come from
//! the test case's `// @option: value` lines, with the variant in the baseline's file name
//! (`name(target=esnext).js`) taking precedence. Tests ts-r does not cover are skipped:
//! targets below ES2022, `outFile`, AMD/System/UMD modules, `noEmit`, files with syntax
//! errors, and JavaScript inputs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ts_r_codegen::CodegenOptions;
use ts_r_config::ScriptTarget;
use ts_r_transform::{TransformOptions, transpile};

ts_r_testing::cap_memory!();

#[path = "../tests/support/mod.rs"]
mod support;

/// `// @name: value` lines of a test case (lower-cased names, last one wins).
fn test_options(src: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in src.lines() {
        let t = line.trim_start_matches('\u{feff}').trim_start();
        let Some(rest) = t.strip_prefix("//") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('@') else {
            continue;
        };
        let Some((name, value)) = rest.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
            continue;
        }
        out.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
    out
}

/// `name(target=esnext,module=commonjs).js` → `[("target", "esnext"), ("module", "commonjs")]`.
fn variant(file_name: &str) -> Vec<(String, String)> {
    let Some(open) = file_name.find('(') else {
        return Vec::new();
    };
    let Some(close) = file_name.rfind(')') else {
        return Vec::new();
    };
    file_name[open + 1..close]
        .split(',')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect()
}

struct Section {
    name: String,
    text: String,
}

fn sections(baseline: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    for line in baseline.split_inclusive('\n') {
        let t = line.trim_end_matches(['\r', '\n']);
        if let Some(rest) = t.strip_prefix("//// [") {
            if rest.ends_with("] ////") {
                continue;
            }
            if let Some(name) = rest.strip_suffix(']') {
                out.push(Section {
                    name: name.to_owned(),
                    text: String::new(),
                });
                continue;
            }
        }
        if let Some(s) = out.last_mut() {
            s.text.push_str(line);
        }
    }
    out
}

fn stem(name: &str) -> &str {
    for ext in [".tsx", ".ts", ".mts", ".cts", ".jsx", ".js", ".mjs", ".cjs"] {
        if let Some(s) = name.strip_suffix(ext) {
            return s;
        }
    }
    name
}

fn base(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

/// `.mjs` comes from `.mts`, `.cjs` from `.cts`, `.js`/`.jsx` from `.ts`/`.tsx`.
fn ext_matches(input: &str, output: &str) -> bool {
    if output.ends_with(".mjs") {
        input.ends_with(".mts")
    } else if output.ends_with(".cjs") {
        input.ends_with(".cts")
    } else {
        input.ends_with(".ts") || input.ends_with(".tsx")
    }
}

/// The files of a test case: `// @filename: path` sections, or the whole case.
fn split_case(src: &str, case_path: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;
    let mut preamble = String::new();
    for line in src.split_inclusive('\n') {
        let t = line.trim_start_matches('\u{feff}').trim();
        let directive = t
            .strip_prefix("//")
            .map(str::trim_start)
            .and_then(|r| r.strip_prefix('@'))
            .and_then(|r| r.split_once(':'))
            .filter(|(k, _)| k.trim().eq_ignore_ascii_case("filename"));
        if let Some((_, v)) = directive {
            if let Some(c) = current.take() {
                out.push(c);
            }
            let name = v.trim().to_owned();
            current = Some((name, String::new()));
            continue;
        }
        match current.as_mut() {
            Some((_, text)) => text.push_str(line),
            None => preamble.push_str(line),
        }
    }
    if let Some(c) = current.take() {
        out.push(c);
    }
    if out.is_empty() {
        out.push((base(case_path).to_owned(), preamble));
    }
    out
}

/// CommonJS-ness of a file under `node16`/`nodenext`: by extension, else the nearest
/// `package.json` in the test's files.
fn is_cjs_file(name: &str, files: &[(String, String)]) -> bool {
    if name.ends_with(".cts") {
        return true;
    }
    if name.ends_with(".mts") {
        return false;
    }
    let abs = |p: &str| {
        if p.starts_with('/') {
            p.to_owned()
        } else {
            format!("/{p}")
        }
    };
    let mut dir = abs(name);
    while let Some(i) = dir.rfind('/') {
        dir.truncate(i);
        let pkg = format!("{dir}/package.json");
        if let Some((_, text)) = files.iter().find(|(n, _)| abs(n) == pkg) {
            return !text
                .replace([' ', '\n', '\t', '\r'], "")
                .contains("\"type\":\"module\"");
        }
    }
    true
}

fn is_ts_input(name: &str) -> bool {
    support::is_fixture(Path::new(name))
}

fn is_js_output(name: &str) -> bool {
    [".js", ".jsx", ".mjs", ".cjs"]
        .iter()
        .any(|e| name.ends_with(e))
}

fn main() {
    let mut upstream = PathBuf::from("target/upstream/typescript");
    let mut show = 20usize;
    let mut filter: Option<String> = None;
    let mut list: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--upstream" => upstream = args.next().map(PathBuf::from).unwrap_or(upstream),
            "--show" => show = args.next().and_then(|s| s.parse().ok()).unwrap_or(20),
            "--filter" => filter = args.next(),
            "--list" => list = args.next().map(PathBuf::from),
            _ => {}
        }
    }
    // tsc 7 (typescript-go) baselines: the TypeScript suite ("submodule") and its own tests.
    let testdata = upstream.join("tsc/testdata");
    let mut names: Vec<PathBuf> = Vec::new();
    for dir in [
        "baselines/reference/submodule/compiler",
        "baselines/reference/submodule/conformance",
        "baselines/reference/compiler",
        "baselines/reference/conformance",
    ] {
        if let Ok(r) = std::fs::read_dir(testdata.join(dir)) {
            names.extend(
                r.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|e| e == "js")),
            );
        }
    }
    names.sort();
    let (mut ok, mut bad, mut skipped_tests, mut skipped_files) = (0usize, 0usize, 0usize, 0usize);
    let mut shown = 0usize;
    let mut mismatches = String::new();
    for path in &names {
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if let Some(f) = &filter
            && !file_name.contains(f.as_str())
        {
            continue;
        }
        let Ok(baseline) = std::fs::read_to_string(path) else {
            continue;
        };
        let Some(case_path) = baseline
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("//// ["))
            .and_then(|l| l.strip_suffix("] ////"))
        else {
            continue;
        };
        let Ok(case_src) = std::fs::read_to_string(upstream.join(case_path))
            .or_else(|_| std::fs::read_to_string(testdata.join(case_path)))
        else {
            // Missing or not UTF-8 (a few cases are UTF-16).
            skipped_tests += 1;
            continue;
        };
        let mut opts = test_options(&case_src);
        for (k, v) in variant(file_name) {
            opts.insert(k, v);
        }
        let get = |k: &str| opts.get(k).map(|s| s.to_ascii_lowercase());
        let module = get("module").unwrap_or_default();
        if opts.contains_key("outfile")
            || opts.contains_key("out")
            || get("noemit").as_deref() == Some("true")
            || get("emitdeclarationonly").as_deref() == Some("true")
            || ["amd", "system", "umd", "none"].contains(&module.as_str())
            || get("target").is_some_and(|t| t.contains(','))
        {
            skipped_tests += 1;
            continue;
        }
        let mut header = String::new();
        for (k, v) in &opts {
            if k == "filename" {
                continue;
            }
            header.push_str(&format!("// @{k}: {v}\n"));
        }
        let json = support::options_json(&header);
        let probe = TransformOptions::from_json(&json, "a.ts");
        if probe.target < ScriptTarget::ES2022 {
            skipped_tests += 1;
            continue;
        }
        let files = split_case(&case_src, case_path);
        let secs = sections(&baseline);
        let mut used = vec![false; files.len()];
        for out in secs.iter().filter(|s| is_js_output(&s.name)) {
            let out_stem = base(stem(&out.name));
            let candidates: Vec<usize> = (0..files.len())
                .filter(|&i| !used[i] && is_ts_input(&files[i].0))
                .filter(|&i| {
                    base(stem(&files[i].0)) == out_stem && ext_matches(&files[i].0, &out.name)
                })
                .collect();
            if candidates.is_empty() {
                skipped_files += 1;
                continue;
            }
            let mut first_diff: Option<(usize, String)> = None;
            let mut matched = false;
            let mut errors = false;
            for &i in &candidates {
                let (name, text) = &files[i];
                let cjs = is_cjs_file(name, &files);
                let opts = TransformOptions::from_json_format(&json, name, cjs);
                let result = transpile(name, text, &opts, &CodegenOptions::default());
                if result.has_errors() {
                    errors = true;
                    continue;
                }
                match support::compare(&result.code, &out.text) {
                    None => {
                        used[i] = true;
                        matched = true;
                        break;
                    }
                    Some(d) => {
                        if first_diff.is_none() {
                            first_diff = Some((i, d));
                        }
                    }
                }
            }
            if matched {
                ok += 1;
            } else if let Some((i, diff)) = first_diff {
                used[i] = true;
                bad += 1;
                mismatches.push_str(&format!("{file_name}\t{}\n", files[i].0));
                if shown < show {
                    shown += 1;
                    println!("MISMATCH {file_name} [{}] {json}\n{diff}", files[i].0);
                }
            } else if errors {
                skipped_files += 1;
            }
        }
    }
    if let Some(l) = list {
        let _ = std::fs::write(l, mismatches);
    }
    println!(
        "baseline oracle: {ok} match, {bad} mismatch; skipped {skipped_tests} tests (unsupported options/targets), {skipped_files} files (syntax errors / no input)"
    );
}
