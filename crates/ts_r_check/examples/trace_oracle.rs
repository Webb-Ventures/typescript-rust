//! Compares `--traceResolution` output with TypeScript 7's `.trace.json` baselines.
//!
//! ```text
//! cargo build -j 3 --release -p ts_r_check --example trace_oracle
//! scripts/limit.sh --mem-mb 4096 --time-s 600 -- \
//!   target/release/examples/trace_oracle [--verbose] [filter...]
//! ```
//!
//! Every test with a trace baseline (`tests/cases/{compiler,conformance}` against
//! `submodule/{compiler,conformance}`, and tsc 7's own `tsc/testdata/tests/cases/compiler`;
//! one run per baseline variant) is split into its files and loaded as a program over an
//! in-memory file system with the test's options, as the conformance harness does
//! (`ts_r_checker`'s `conformance` example). The program's trace (`src/trace.rs`, with the
//! harness's `FakeTSVersion`) is compared line by line with the baseline: a test matches
//! when every line does; lines count as matched along the longest common subsequence.
//! Mismatches go to `target/trace-oracle.txt`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ts_r_checker::{LibStore, load_program};
use ts_r_config::options::{OptionType, find_option_ignore_case};
use ts_r_config::{MemoryFs, load_tsconfig, path};

#[path = "../src/trace.rs"]
#[allow(dead_code)]
mod trace;

ts_r_testing::cap_memory!();

type Pairs = Vec<(String, String)>;

struct Test {
    name: String,
    options: Pairs,
    files: Pairs,
    expected: Vec<String>,
}

fn upstream() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/upstream/typescript")
}

fn collect(dir: &Path, out: &mut BTreeMap<String, PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect(&p, out);
        } else if let Some(ext) = p.extension().and_then(|e| e.to_str())
            && matches!(
                ext,
                "ts" | "tsx" | "js" | "jsx" | "mts" | "cts" | "mjs" | "cjs"
            )
            && let Some(stem) = p.file_stem().and_then(|s| s.to_str())
        {
            out.entry(stem.to_owned()).or_insert(p);
        }
    }
}

fn parse_directive(line: &str) -> Option<(String, String)> {
    let t = line.trim_start();
    let rest = t.strip_prefix("//")?.trim_start().strip_prefix('@')?;
    let (k, v) = rest.split_once(':')?;
    let k = k.trim_end();
    if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some((k.trim().to_ascii_lowercase(), v.trim().to_owned()))
}

fn split_test(text: &str, default_name: &str) -> (Pairs, Pairs) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut files: Pairs = Vec::new();
    let mut options = Vec::new();
    let mut cur_name: Option<String> = None;
    let mut cur = String::new();
    for line in text.split_inclusive('\n') {
        if let Some((k, v)) = parse_directive(line) {
            if k == "symlink" {
                // `@symlink: a, b` after `@filename: f`: `a` and `b` link to `f`.
                if let Some(n) = &cur_name {
                    for l in v.split(',') {
                        options.push(("link".to_owned(), format!("{n} -> {}", l.trim())));
                    }
                }
                continue;
            }
            if k == "filename" {
                if let Some(n) = cur_name.take() {
                    files.push((n, std::mem::take(&mut cur)));
                } else {
                    cur.clear();
                }
                cur_name = Some(v);
            } else {
                options.push((k, v));
            }
            continue;
        }
        if cur.is_empty() && line.trim().is_empty() {
            continue;
        }
        cur.push_str(line);
    }
    match cur_name {
        Some(n) => files.push((n, cur)),
        None => files.push((default_name.to_owned(), cur)),
    }
    (files, options)
}

/// The tsconfig `compilerOptions` members for the test's directives.
fn options_json(opts: &[(String, String)]) -> String {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for (k, v) in opts.iter().rev() {
        let Some(decl) = find_option_ignore_case(k) else {
            continue;
        };
        if !seen.insert(decl.name) {
            continue;
        }
        let first = v.split(',').next().unwrap_or("").trim();
        let val = match decl.ty {
            OptionType::Bool => match first.to_ascii_lowercase().as_str() {
                "true" => "true".to_owned(),
                "false" => "false".to_owned(),
                _ => continue,
            },
            OptionType::Number => match first.parse::<f64>() {
                Ok(_) => first.to_owned(),
                Err(_) => continue,
            },
            OptionType::StringList | OptionType::PathList | OptionType::Lib => {
                // tsc's harness keeps the spaces of `customConditions` entries.
                let keep = decl.name == "customConditions";
                let items: Vec<String> = v
                    .split(',')
                    .map(|s| format!("{:?}", if keep { s } else { s.trim() }))
                    .filter(|s| s != "\"\"")
                    .collect();
                format!("[{}]", items.join(","))
            }
            OptionType::PathMap | OptionType::Plugins => v.clone(),
            _ => format!("{first:?}"),
        };
        out.push(format!("{:?}: {val}", decl.name));
    }
    out.join(", ")
}

/// The tests of one baseline directory: `stem[(variant)].trace.json` with its source.
fn load_suite(baselines: &Path, sources: &BTreeMap<String, PathBuf>, out: &mut Vec<Test>) {
    let Ok(rd) = std::fs::read_dir(baselines) else {
        return;
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|n| n.ends_with(".trace.json"))
        .collect();
    names.sort();
    for n in names {
        let base = &n[..n.len() - ".trace.json".len()];
        let (stem, variant) = match base.split_once('(') {
            Some((s, v)) => (s, v.strip_suffix(')')),
            None => (base, None),
        };
        let Some(src) = sources.get(stem) else {
            continue;
        };
        let (Ok(text), Ok(expected)) = (
            std::fs::read_to_string(src),
            std::fs::read_to_string(baselines.join(&n)),
        ) else {
            continue;
        };
        let file_name = src.file_name().and_then(|s| s.to_str()).unwrap_or("a.ts");
        let (files, mut options) = split_test(&text, file_name);
        if let Some(v) = variant {
            for kv in v.split(',') {
                if let Some((k, val)) = kv.split_once('=') {
                    options.push((k.trim().to_ascii_lowercase(), val.trim().to_owned()));
                }
            }
        }
        out.push(Test {
            name: base.to_owned(),
            options,
            files,
            expected: expected.lines().map(str::to_owned).collect(),
        });
    }
}

fn run(test: &Test, libs: &LibStore) -> Vec<String> {
    let cwd = test
        .options
        .iter()
        .rev()
        .find(|(k, _)| k == "currentdirectory")
        .map_or("/.src".to_owned(), |(_, v)| {
            format!("/{}", v.trim_matches('/'))
        });
    let abs = |n: &str| {
        let n = n.trim_start_matches("./");
        let drive = n.as_bytes().get(1) == Some(&b':');
        if n.starts_with('/') || drive {
            n.to_owned()
        } else {
            path::combine(&cwd, n)
        }
    };
    let mut fs = MemoryFs::new();
    fs.set_cwd(&cwd);
    let mut roots = Vec::new();
    let has_config = test
        .files
        .iter()
        .any(|(n, _)| n.to_ascii_lowercase().ends_with("tsconfig.json"));
    let no_implicit = test
        .options
        .iter()
        .any(|(k, v)| k == "noimplicitreferences" && v.eq_ignore_ascii_case("true"));
    let last_refers = test.files.last().is_some_and(|(_, src)| {
        src.contains("require(")
            || src
                .split("reference")
                .skip(1)
                .any(|r| r.starts_with(char::is_whitespace) && r.trim_start().starts_with("path"))
    });
    let only_last = !has_config && (no_implicit || last_refers);
    let last = test.files.len().saturating_sub(1);
    for (i, (name, src)) in test.files.iter().enumerate() {
        let a = abs(name);
        fs.add_file(&a, src);
        if a.to_ascii_lowercase().ends_with(".json") || (only_last && i != last) {
            continue;
        }
        roots.push(a);
    }
    for (k, v) in &test.options {
        if k == "link"
            && let Some((target, link)) = v.split_once("->")
        {
            fs.add_symlink(&abs(link.trim()), &abs(target.trim()));
        }
    }
    let co = options_json(&test.options);
    let own_config = test
        .files
        .iter()
        .map(|(n, _)| abs(n))
        .find(|n| n.ends_with("/tsconfig.json"));
    let wrapper = match &own_config {
        Some(own) => {
            let dir = own.rsplit_once('/').map_or("", |(d, _)| d);
            let wrapper = format!("{dir}/__tsconfig.json");
            fs.add_file(
                &wrapper,
                &format!("{{\"extends\": {own:?}, \"compilerOptions\": {{{co}}}}}"),
            );
            wrapper
        }
        None => {
            let wrapper = path::combine(&cwd, "__tsconfig.json");
            fs.add_file(
                &wrapper,
                &format!("{{\"compilerOptions\": {{{co}}}, \"files\": []}}"),
            );
            wrapper
        }
    };
    let cfg = load_tsconfig(&fs, &wrapper);
    if let Some(own) = &own_config {
        roots = load_tsconfig(&fs, own).file_names;
    }
    let program = load_program(&fs, &roots, &cfg.options, Some(libs), 1);
    trace::trace(
        &fs,
        &program,
        None,
        ts_r_resolver::Trace::new().with_compiler_version("FakeTSVersion"),
    )
}

/// Length of the longest common subsequence of `a` and `b`.
fn lcs(a: &[String], b: &[String]) -> usize {
    if a.len().saturating_mul(b.len()) > 50_000_000 {
        // Too big for the table: count equal lines position by position.
        return a.iter().zip(b).filter(|(x, y)| x == y).count();
    }
    let mut prev = vec![0u32; b.len() + 1];
    let mut cur = vec![0u32; b.len() + 1];
    for x in a {
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = if x == y {
                prev[j] + 1
            } else {
                cur[j].max(prev[j + 1])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()] as usize
}

fn main() {
    let mut verbose = false;
    let mut filters: Vec<String> = Vec::new();
    for a in std::env::args().skip(1) {
        if a == "--verbose" {
            verbose = true;
        } else {
            filters.push(a);
        }
    }
    let root = upstream();
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files not found; run `cargo xtask upstream sync --minimal`");
        std::process::exit(2);
    };
    let reference = root.join("tsc/testdata/baselines/reference");
    let mut tests = Vec::new();
    for (cases, baselines) in [
        ("tests/cases/compiler", "submodule/compiler"),
        ("tests/cases/conformance", "submodule/conformance"),
        ("tsc/testdata/tests/cases/compiler", "compiler"),
    ] {
        let mut sources = BTreeMap::new();
        collect(&root.join(cases), &mut sources);
        load_suite(&reference.join(baselines), &sources, &mut tests);
    }
    tests.retain(|t| filters.is_empty() || filters.iter().any(|f| t.name.contains(f.as_str())));
    let (mut exact, mut matched, mut total) = (0usize, 0usize, 0usize);
    let mut report = String::new();
    let start = std::time::Instant::now();
    let results = std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(512 << 20)
            .spawn_scoped(s, || {
                tests
                    .iter()
                    .map(|t| run(t, &libs))
                    .collect::<Vec<Vec<String>>>()
            })
            .ok()
            .and_then(|h| h.join().ok())
            .unwrap_or_default()
    });
    for (test, actual) in tests.iter().zip(&results) {
        let m = lcs(&test.expected, actual);
        total += test.expected.len();
        matched += m;
        if *actual == test.expected {
            exact += 1;
            continue;
        }
        report.push_str(&format!(
            "== {} ({m}/{} lines, {} ours)\n",
            test.name,
            test.expected.len(),
            actual.len()
        ));
        // The first difference and some context after it.
        let first = test
            .expected
            .iter()
            .zip(actual)
            .position(|(a, b)| a != b)
            .unwrap_or(test.expected.len().min(actual.len()));
        let from = first.saturating_sub(2);
        for i in from..(first + 12) {
            match (test.expected.get(i), actual.get(i)) {
                (Some(e), Some(a)) if e == a => report.push_str(&format!("    {e}\n")),
                (e, a) => {
                    if let Some(e) = e {
                        report.push_str(&format!("  - {e}\n"));
                    }
                    if let Some(a) = a {
                        report.push_str(&format!("  + {a}\n"));
                    }
                }
            }
        }
        if verbose {
            println!("mismatch: {} ({m}/{})", test.name, test.expected.len());
        }
    }
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/trace-oracle.txt");
    let _ = std::fs::write(&out, &report);
    println!(
        "{} tests in {:.1}s: {exact} exact; lines {matched}/{total} ({:.1}%)",
        tests.len(),
        start.elapsed().as_secs_f64(),
        if total == 0 {
            0.0
        } else {
            100.0 * matched as f64 / total as f64
        }
    );
    println!("mismatches: {}", out.display());
}
