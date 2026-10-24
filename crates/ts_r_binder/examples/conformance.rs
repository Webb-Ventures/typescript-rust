//! Compares binder diagnostics with TypeScript 7's conformance baselines.
//!
//! ```text
//! cargo xtask upstream sync   # once
//! scripts/limit.sh --mem-mb 4096 --time-s 600 -- \
//!   cargo run --release -p ts_r_binder --example conformance -- [--verbose] [filter]
//! ```
//!
//! For every test under `tests/cases/{compiler,conformance}` with a TS7 baseline, the test is
//! split into its files, each file is parsed and bound, the files are linked, and the binder's
//! codes are compared with the baseline lines that carry those codes. Mismatches are written to
//! `target/binder-conformance.txt`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use ts_r_ast::SourceKind;
use ts_r_binder::program::{Program, ProgramFile};
use ts_r_binder::{BindOptions, bind, messages};
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_resolver::{Resolver, ResolverOptions};
use ts_r_span::LineIndex;

ts_r_testing::cap_memory!();

/// Codes compared: everything the binder reports except the internal depth limit.
fn compared(code: u32) -> bool {
    code != 99001 && code != 6203 && messages::ALL_CODES.contains(&code)
}

type Diag = (String, u32, u32, u32, String);

/// Parse errors that do not make a file "broken" for the driver: tsc reports them from its
/// checker (`ts_r_checker::driver::CHECKER_GRAMMAR_CODES`).
const CHECKER_GRAMMAR_CODES: [u32; 17] = [
    5076, 1106, 1497, 1164, 8002, 8003, 8004, 8005, 8008, 8009, 8010, 8011, 8012, 8013, 8016, 8017,
    8037,
];

/// The binder's TS1xxx codes the driver keeps in a file with syntax errors (tsc's binder
/// reports them whatever the parser found; `ts_r_checker::driver::BINDER_CODES`).
const BINDER_CODES: &[u32] = &[1100, 1101, 1102, 1184, 1210, 1215, 1314, 1315, 1316, 1344];

struct Test {
    name: String,
    options: Vec<(String, String)>,
    files: Vec<(String, String)>,
    opts: BindOptions,
    expected: Option<BTreeSet<Diag>>,
}

fn upstream() -> PathBuf {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    here.join("../../target/upstream/typescript")
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
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
        {
            out.push(p);
        }
    }
}

fn parse_directive(line: &str) -> Option<(String, String)> {
    let t = line.trim_start();
    let rest = t.strip_prefix("//")?.trim_start().strip_prefix('@')?;
    let (k, v) = rest.split_once(':')?;
    if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some((k.trim().to_ascii_lowercase(), v.trim().to_owned()))
}

/// `(key, value)` pairs: directives or `(file name, contents)`.
type Pairs = Vec<(String, String)>;

fn split_test(text: &str, default_name: &str) -> (Pairs, Pairs) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut files: Vec<(String, String)> = Vec::new();
    let mut options = Vec::new();
    let mut cur_name: Option<String> = None;
    let mut cur = String::new();
    for line in text.split_inclusive('\n') {
        if let Some((k, v)) = parse_directive(line) {
            if k == "filename" {
                if let Some(n) = cur_name.take() {
                    let text = std::mem::take(&mut cur);
                    files.push((n, text.trim_start_matches(['\n', '\r']).to_owned()));
                } else {
                    cur.clear();
                }
                cur_name = Some(v);
            } else {
                options.push((k, v));
            }
            continue;
        }
        cur.push_str(line);
    }
    match cur_name {
        Some(n) => files.push((n, cur.trim_start_matches(['\n', '\r']).to_owned())),
        None => {
            let trimmed = cur.trim_start_matches(['\n', '\r']).to_owned();
            files.push((default_name.to_owned(), trimmed));
        }
    }
    (files, options)
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `file:line:col - error TS1234: message` (pretty output).
fn parse_pretty(line: &str) -> Option<Diag> {
    let (loc, rest) = line.split_once(" - error TS")?;
    let mut parts = loc.rsplitn(3, ':');
    let c = parts.next()?.parse().ok()?;
    let l = parts.next()?.parse().ok()?;
    let file = parts.next()?;
    let (code, msg) = rest.split_once(": ")?;
    let code: u32 = code.parse().ok()?;
    compared(code).then(|| {
        (
            file.trim_start_matches("./").to_owned(),
            l,
            c,
            code,
            msg.to_owned(),
        )
    })
}

fn parse_baseline(text: &str) -> BTreeSet<Diag> {
    let mut out = BTreeSet::new();
    if text.contains('\u{1b}') {
        for line in text.lines() {
            if line.starts_with("====") {
                break;
            }
            if let Some(d) = parse_pretty(&strip_ansi(line)) {
                out.insert(d);
            }
        }
        return out;
    }
    for line in text.lines() {
        if line.starts_with("====") {
            break;
        }
        // file(line,col): error TS1234: message
        let Some((loc, rest)) = line.split_once("): ") else {
            continue;
        };
        let Some((file, lc)) = loc.rsplit_once('(') else {
            continue;
        };
        let Some((l, c)) = lc.split_once(',') else {
            continue;
        };
        let (Ok(l), Ok(c)) = (l.parse::<u32>(), c.parse::<u32>()) else {
            continue;
        };
        let Some(code_part) = rest.split(": ").next() else {
            continue;
        };
        let Some(code) = code_part
            .rsplit("TS")
            .next()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let msg = rest.split_once(": ").map_or("", |x| x.1).to_owned();
        if compared(code) {
            out.insert((file.trim_start_matches("./").to_owned(), l, c, code, msg));
        }
    }
    out
}

/// Compiler options that affect module resolution, as tsconfig JSON members.
fn serde_like_options(opts: &[(String, String)]) -> String {
    let mut out = Vec::new();
    for (k, v) in opts {
        let first = v.split(',').next().unwrap_or("").trim();
        let key = match k.as_str() {
            "module" => "module",
            "moduleresolution" => "moduleResolution",
            "target" => "target",
            "baseurl" => "baseUrl",
            "allowjs" => "allowJs",
            "resolvejsonmodule" => "resolveJsonModule",
            "allowimportingtsextensions" => "allowImportingTsExtensions",
            "customconditions" => "customConditions",
            "paths" => {
                out.push(format!("\"paths\": {v}"));
                continue;
            }
            _ => continue,
        };
        let val = match first {
            "true" | "false" => first.to_owned(),
            _ if key == "customConditions" => format!(
                "[{}]",
                v.split(',')
                    .map(|c| format!("{:?}", c.trim()))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            _ => format!("{first:?}"),
        };
        out.push(format!("\"{key}\": {val}"));
    }
    out.join(", ")
}

fn option_bool(opts: &[(String, String)], key: &str) -> Option<bool> {
    opts.iter()
        .rev()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| {
            match v
                .split(',')
                .next()
                .map(|s| s.trim().to_ascii_lowercase())
                .as_deref()
            {
                Some("true") => Some(true),
                Some("false") => Some(false),
                _ => None,
            }
        })
}

/// One test per configuration variant that has baselines (`stem(opt=v,...).ext`), or one
/// for the plain configuration. A variant without `.errors.txt` expects no errors.
fn load(path: &Path, baselines: &Path, names: &BTreeSet<String>) -> Vec<Test> {
    let Ok(raw) = std::fs::read(path) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (Some(stem), Some(file_name)) = (
        path.file_stem().and_then(|s| s.to_str()),
        path.file_name().and_then(|s| s.to_str()),
    ) else {
        return Vec::new();
    };
    let (files, options) = split_test(&text, file_name);
    let mut variants: BTreeSet<Option<String>> = BTreeSet::new();
    for n in names.range(stem.to_owned()..) {
        if !n.starts_with(stem) {
            break;
        }
        let rest = &n[stem.len()..];
        if rest.starts_with('.') {
            variants.insert(None);
        } else if let Some(v) = rest.strip_prefix('(')
            && let Some((v, _)) = v.split_once(')')
        {
            variants.insert(Some(v.to_owned()));
        }
    }
    let mut out = Vec::new();
    for v in variants {
        let mut options = options.clone();
        let base = match &v {
            Some(v) => {
                for kv in v.split(',') {
                    if let Some((k, val)) = kv.split_once('=') {
                        options.push((k.trim().to_ascii_lowercase(), val.trim().to_owned()));
                    }
                }
                format!("{stem}({v}).errors.txt")
            }
            None => format!("{stem}.errors.txt"),
        };
        let expected = if names.contains(&base) {
            match std::fs::read_to_string(baselines.join(&base)) {
                Ok(t) => parse_baseline(&t),
                Err(_) => continue,
            }
        } else {
            BTreeSet::new()
        };
        let force = options
            .iter()
            .any(|(k, v)| k == "moduledetection" && v.eq_ignore_ascii_case("force"));
        let opts = BindOptions {
            force_module: force,
            always_strict: option_bool(&options, "alwaysstrict"),
            allow_unreachable_code: option_bool(&options, "allowunreachablecode"),
            allow_unused_labels: option_bool(&options, "allowunusedlabels"),
            preserve_const_enums: option_bool(&options, "preserveconstenums") == Some(true)
                || option_bool(&options, "isolatedmodules") == Some(true),
            esm_format: false,
            package_esm: false,
            package_scope: None,
        };
        let name = match &v {
            Some(v) => format!("{}({v})", path.display()),
            None => path.display().to_string(),
        };
        out.push(Test {
            name,
            options,
            files: files.clone(),
            opts,
            expected: Some(expected),
        });
    }
    out
}

fn run(test: &Test) -> BTreeSet<Diag> {
    let mut parsed = Vec::new();
    for (name, src) in &test.files {
        let kind = SourceKind::from_path(name);
        let force = test.opts.force_module || name.ends_with(".mts") || name.ends_with(".cts");
        let opts = BindOptions {
            force_module: force,
            ..test.opts
        };
        let p = ts_r_parser::parse(src, kind);
        let b = bind(&p.ast, src, &opts);
        let broken = p
            .syntax_errors()
            .iter()
            .any(|d| d.is_error() && !CHECKER_GRAMMAR_CODES.contains(&d.code));
        parsed.push((name.clone(), src.as_str(), p.ast, b, broken));
    }
    let mut out = BTreeSet::new();
    let fmt_name = |n: &str| n.trim_start_matches("./").to_owned();
    // Driver rule for JS files: `checkJs: false` drops binder diagnostics; plain JS (checkJs
    // unset) drops duplicate-identifier errors but keeps the rest.
    let check_js = test
        .options
        .iter()
        .find(|(k, _)| k == "checkjs")
        .map(|(_, v)| v.trim() == "true");
    let compared = |i: usize, code: u32| {
        let name = parsed[i].0.as_str();
        // Driver rule for files with syntax errors: the binder's TS1xxx errors other than
        // `BINDER_CODES` wait for a clean parse (reserved words, duplicate properties).
        if parsed[i].4 && (1000..2000).contains(&code) && !BINDER_CODES.contains(&code) {
            return false;
        }
        if !SourceKind::from_path(name).is_typescript() {
            match check_js {
                Some(false) => return false,
                None if code == 2300 => return false,
                _ => {}
            }
        }
        compared(code)
    };
    let lines: Vec<LineIndex> = parsed.iter().map(|(_, s, ..)| LineIndex::new(s)).collect();
    for (i, (name, src, _, b, _)) in parsed.iter().enumerate() {
        for d in &b.diagnostics {
            if compared(i, d.code) {
                let lc = lines[i].line_col_utf16(src, d.span.start);
                out.insert((
                    fmt_name(name),
                    lc.line + 1,
                    lc.col + 1,
                    d.code,
                    d.message.clone(),
                ));
            }
        }
    }
    // Resolve through ts_r_resolver over an in-memory copy of the test's files.
    let abs = |n: &str| {
        let n = n.trim_start_matches("./");
        if n.starts_with('/') {
            n.to_owned()
        } else {
            format!("/.src/{n}")
        }
    };
    let mut fs = MemoryFs::new();
    fs.set_cwd("/.src");
    for (name, src, ..) in &parsed {
        fs.add_file(&abs(name), src);
    }
    let mut co = serde_like_options(&test.options);
    if co.is_empty() {
        co.push_str("\"noEmit\": true");
    }
    fs.add_file(
        "/.src/__tsconfig.json",
        &format!("{{\"compilerOptions\": {{{co}}}, \"files\": []}}"),
    );
    let cfg = load_tsconfig(&fs, "/.src/__tsconfig.json");
    let resolver = Resolver::new(&fs, ResolverOptions::from_compiler_options(&cfg.options));
    let paths: Vec<String> = parsed.iter().map(|(n, ..)| abs(n)).collect();
    let files: Vec<ProgramFile> = parsed
        .iter()
        .zip(&paths)
        .map(|((_, src, ast, b, _), path)| ProgramFile {
            path: path.as_str(),
            ast,
            src,
            bound: b,
        })
        .collect();
    let program = Program::link_with(files, &resolver);
    for d in &program.diagnostics {
        if compared(d.file.0 as usize, d.code) {
            let i = d.file.0 as usize;
            let (name, src, ..) = &parsed[i];
            let lc = lines[i].line_col_utf16(src, d.span.start);
            out.insert((
                fmt_name(name),
                lc.line + 1,
                lc.col + 1,
                d.code,
                d.message.clone(),
            ));
        }
    }
    out
}

fn main() {
    let mut verbose = false;
    let mut filter = None;
    for a in std::env::args().skip(1) {
        if a == "--verbose" {
            verbose = true;
        } else {
            filter = Some(a);
        }
    }
    let root = upstream();
    let mut paths = Vec::new();
    let mut suites = Vec::new();
    for suite in ["compiler", "conformance"] {
        let mut v = Vec::new();
        collect(&root.join("tests/cases").join(suite), &mut v);
        let base = root
            .join("tsc/testdata/baselines/reference/submodule")
            .join(suite);
        let names: BTreeSet<String> = std::fs::read_dir(&base)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| e.file_name().to_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        suites.push((base, names));
        for p in v {
            if filter
                .as_ref()
                .is_none_or(|f| p.display().to_string().contains(f.as_str()))
            {
                paths.push((suites.len() - 1, p));
            }
        }
    }
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::new());
    let start = std::time::Instant::now();
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((suite, p)) = paths.get(i) else {
                        break;
                    };
                    let (base, names) = &suites[*suite];
                    for test in load(p, base, names) {
                        let actual = run(&test);
                        if let Ok(mut r) = results.lock() {
                            r.push((
                                test.name.clone(),
                                test.expected.clone().unwrap_or_default(),
                                actual,
                            ));
                        }
                    }
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap_or_default();
    results.sort_by(|a, b| a.0.cmp(&b.0));
    let (mut tests, mut with_expected, mut exact, mut fp_tests) = (0, 0, 0, 0);
    let (mut exp_n, mut matched_n, mut missing_n, mut extra_n) = (0, 0, 0, 0);
    let mut per_code: std::collections::BTreeMap<u32, (u32, u32, u32)> = Default::default();
    let mut report = String::new();
    let key = |d: &Diag| (d.0.clone(), d.1, d.2, d.3);
    for (name, expected, actual) in &results {
        tests += 1;
        let ek: BTreeSet<_> = expected.iter().map(key).collect();
        let ak: BTreeSet<_> = actual.iter().map(key).collect();
        let missing: Vec<&Diag> = expected.iter().filter(|d| !ak.contains(&key(d))).collect();
        let extra: Vec<&Diag> = actual.iter().filter(|d| !ek.contains(&key(d))).collect();
        exp_n += expected.len();
        missing_n += missing.len();
        extra_n += extra.len();
        matched_n += expected.len() - missing.len();
        for d in expected {
            per_code.entry(d.3).or_default().0 += 1;
        }
        for d in &missing {
            per_code.entry(d.3).or_default().1 += 1;
        }
        for d in &extra {
            per_code.entry(d.3).or_default().2 += 1;
        }
        if !expected.is_empty() {
            with_expected += 1;
            if missing.is_empty() && extra.is_empty() {
                exact += 1;
            }
        } else if !extra.is_empty() {
            fp_tests += 1;
        }
        if !missing.is_empty() || !extra.is_empty() {
            report.push_str(&format!("== {name}\n"));
            for d in missing {
                report.push_str(&format!(
                    "  - {}({},{}) TS{}: {}\n",
                    d.0, d.1, d.2, d.3, d.4
                ));
            }
            for d in extra {
                report.push_str(&format!(
                    "  + {}({},{}) TS{}: {}\n",
                    d.0, d.1, d.2, d.3, d.4
                ));
            }
        }
    }
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/binder-conformance.txt");
    let _ = std::fs::write(&out, &report);
    println!(
        "{tests} tests in {:.1}s; {with_expected} expect binder diagnostics, {exact} match exactly; {fp_tests} tests with only extra diagnostics",
        start.elapsed().as_secs_f64()
    );
    println!(
        "diagnostics: expected {exp_n}, matched {matched_n}, missing {missing_n}, extra {extra_n}"
    );
    println!("per code (expected / missing / extra):");
    for (c, (e, m, x)) in per_code {
        println!("  TS{c}: {e} / {m} / {x}");
    }
    if verbose {
        print!("{report}");
    }
    println!("mismatches: {}", out.display());
}
