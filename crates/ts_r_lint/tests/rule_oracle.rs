//! Rule oracle: replays the cases recorded from real ESLint / typescript-eslint
//! (`tests/oracle/rules/*.json`, produced by `tests/oracle/gen/gen-rule-oracle.mjs` from the
//! published rule *tests* only) and compares messages, positions, fixes and suggestions.
//!
//! Each rule's pass count must not drop below `tests/oracle/baseline.txt`. Regenerate the
//! baseline with `TS_R_LINT_ORACLE_BLESS=1 cargo test -p ts_r_lint --test rule_oracle`.
//! Filter with `TS_R_LINT_ORACLE_RULE=<substring>[,<substring>...]`; print mismatches with
//! `TS_R_LINT_ORACLE_VERBOSE=1` (or `=N` for at most N per rule).
//!
//! Type-aware cases (`typed` in the data) are linted as a file of `tests/oracle/typed` (a
//! copy of typescript-eslint's test fixtures, mounted in memory at `/fixtures`) through our
//! checker, like `ts-r-lint` does for a real project. They are skipped when the TypeScript
//! lib files are missing (`cargo xtask upstream sync --minimal`).
//!
//! A rule file's top-level `globals` (eslint-plugin-unicorn's tester defaults) are added to
//! every case's own.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use ts_r_lint::Registry;
use ts_r_lint::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};
use ts_r_lint::engine::lint_source;
use ts_r_lint::fix::apply_fixes;
use ts_r_lint::output::Positions;
use ts_r_lint::typed::{TypedFile, check_and_lint, load_project};
use ts_r_lint::types::{FileKey, NoTypeInfo};

ts_r_testing::cap_memory!();

fn oracle_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/oracle")
}

#[derive(Default, Debug, Clone, Copy)]
struct Tally {
    total: usize,
    passed: usize,
}

/// Normalized message for comparison.
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone)]
struct Msg {
    line: u64,
    column: u64,
    end: Option<(u64, u64)>,
    message: String,
    message_id: Option<String>,
    suggestions: Vec<(String, String)>,
}

fn expected_msgs(rule: &str, case: &Value) -> Vec<Msg> {
    let mut v: Vec<Msg> = case["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["ruleId"].as_str() == Some(rule))
        .map(|m| Msg {
            line: m["line"].as_u64().unwrap_or(0),
            column: m["column"].as_u64().unwrap_or(0),
            end: m["endLine"]
                .as_u64()
                .map(|l| (l, m["endColumn"].as_u64().unwrap_or(0))),
            message: m["message"].as_str().unwrap_or("").to_owned(),
            message_id: m["messageId"].as_str().map(str::to_owned),
            suggestions: m["suggestions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| {
                    (
                        s["desc"].as_str().unwrap_or("").to_owned(),
                        s["output"].as_str().unwrap_or("").to_owned(),
                    )
                })
                .collect(),
        })
        .collect();
    v.sort();
    v
}

fn config_for(rule: &str, case: &Value) -> Value {
    let lo = &case["languageOptions"];
    let mut language = serde_json::Map::new();
    for k in ["ecmaVersion", "sourceType", "globals"] {
        if !lo[k].is_null() {
            language.insert(k.to_owned(), lo[k].clone());
        }
    }
    let mut po = lo
        .get("parserOptions")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(ef) = lo.get("ecmaFeatures").filter(|v| !v.is_null()) {
        po.insert("ecmaFeatures".to_owned(), ef.clone());
    }
    if !po.is_empty() {
        language.insert("parserOptions".to_owned(), Value::Object(po));
    }
    if case["parser"] == "ts" {
        language.insert("parser".to_owned(), "typescript-eslint/parser".into());
    }
    let mut value = vec![Value::from("error")];
    if let Some(opts) = case["options"].as_array() {
        value.extend(opts.iter().cloned());
    }
    // ESLint reported (and fixed) an unused directive in this case: its fix shapes the
    // expected output, so report them too (the messages are not compared).
    let unused = case["messages"].as_array().is_some_and(|ms| {
        ms.iter().any(|m| {
            m["ruleId"].is_null()
                && m["message"]
                    .as_str()
                    .is_some_and(|t| t.starts_with("Unused eslint-disable directive"))
        })
    });
    let report_unused = if unused { "warn" } else { "off" };
    json!([{
        "files": ["**/*.{js,mjs,cjs,jsx,ts,tsx,mts,cts}"],
        "languageOptions": language,
        "linterOptions": { "reportUnusedDisableDirectives": report_unused },
        "settings": case.get("settings").cloned().unwrap_or(json!({})),
        "rules": { rule: value },
    }])
}

/// The typed fixtures (path relative to the fixtures root, text), read once.
fn typed_fixtures() -> &'static Vec<(String, String)> {
    static F: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    F.get_or_init(|| {
        let root = oracle_dir().join("typed");
        let mut out = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(t) = std::fs::read_to_string(&p) {
                    let rel = p
                        .strip_prefix(&root)
                        .unwrap_or(&p)
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.push((rel, t));
                }
            }
        }
        out.sort();
        out
    })
}

fn libs() -> Option<&'static ts_r_checker::LibStore> {
    static L: std::sync::OnceLock<Option<ts_r_checker::LibStore>> = std::sync::OnceLock::new();
    L.get_or_init(ts_r_checker::LibStore::locate).as_ref()
}

/// Shared levels kept alive across cases (the registry holds them weakly).
static KEEP: std::sync::Mutex<Vec<std::sync::Arc<ts_r_checker::Layer>>> =
    std::sync::Mutex::new(Vec::new());

/// Lints a typed case through the checker: `Ok(None)` when the lib files are missing.
fn lint_typed(
    case: &Value,
    code: &str,
    filename: &str,
    fc: &ts_r_lint::FileConfig,
) -> Result<Option<ts_r_lint::FileResult>, String> {
    let Some(libs) = libs() else { return Ok(None) };
    let typed = &case["typed"];
    let mut fs = ts_r_config::MemoryFs::new();
    for (rel, text) in typed_fixtures() {
        fs.add_file(&format!("/fixtures/{rel}"), text);
    }
    let path = format!("/fixtures/{filename}");
    fs.add_file(&path, code);
    let root = match typed["root"].as_str().unwrap_or("") {
        "" => "/fixtures".to_owned(),
        r => format!("/fixtures/{r}"),
    };
    let config = match typed["project"].as_str() {
        Some(p) => normalize_path(&format!("{root}/{p}")),
        None => {
            let dir = path.rsplit_once('/').map_or("/", |(d, _)| d).to_owned();
            ts_r_config::find_config_file(&fs, &dir, "tsconfig.json")
                .ok_or("no tsconfig for the file")?
        }
    };
    let project = load_project(&fs, &config, libs, 1);
    if let Some(p) = &project.parent
        && let Ok(mut k) = KEEP.lock()
        && !k.iter().any(|x| std::sync::Arc::ptr_eq(x, p))
    {
        k.push(p.clone());
    }
    let file = project
        .file_index(&path)
        .ok_or_else(|| format!("{path} is not in {config}"))?;
    let run = check_and_lint(
        &project,
        &[TypedFile {
            file,
            cfg: std::sync::Arc::new(fc.clone()),
        }],
        1,
    );
    Ok(run.results.into_iter().next())
}

/// `/a/./b/../c` -> `/a/c`.
fn normalize_path(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    format!("/{}", out.join("/"))
}

fn run_case(reg: &'static Registry, rule: &str, case: &Value) -> Result<(), String> {
    let raw = case["code"].as_str().unwrap_or("");
    // Like the CLI, lint the text without a leading BOM and keep the BOM in front of fixed
    // output (the generator applies fixes to ESLint's BOM-less text the same way).
    let (bom, code) = match raw.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", raw),
    };
    let filename = case["filename"].as_str().unwrap_or("file.js");
    let typed = case["typed"].is_object();
    let cfg_json = config_for(rule, case);
    let objs = objects_from_eslint_json(&cfg_json, reg).map_err(|e| format!("config: {e}"))?;
    let base = PathBuf::from(if typed { "/fixtures" } else { "/oracle" });
    let cfg = ConfigFile::new(ConfigKind::Eslint, None, base, objs);
    let mut opts = ResolveOptions::new();
    opts.types_available = typed;
    let fc = cfg
        .file_config(filename, &opts)
        .map_err(|e| format!("not linted: {e:?}"))?;
    if fc.rules.get(rule).is_none() {
        return Err(format!("rule not configured (options?) {:?}", opts.notes));
    }
    let r = if typed {
        match lint_typed(case, code, filename, &fc)? {
            Some(r) => r,
            None => return Ok(()),
        }
    } else {
        let path = format!("/oracle/{filename}");
        lint_source(&path, code, &fc, &NoTypeInfo, FileKey(0))
    };
    if let Some(f) = r.diagnostics.iter().find(|d| d.fatal) {
        return Err(format!("fatal: {}", f.message));
    }
    let pos = Positions::new(code);
    let mut actual: Vec<Msg> = r
        .diagnostics
        .iter()
        .filter(|d| d.rule == Some(rule_static(reg, rule)))
        .map(|d| {
            let (l, c) = pos.start_of(d);
            let (el, ec) = pos.end_of(d);
            Msg {
                line: l as u64,
                column: c as u64,
                end: Some((el as u64, ec as u64)),
                message: d.message.clone(),
                message_id: d.message_id.map(str::to_owned),
                suggestions: d
                    .suggestions
                    .iter()
                    .map(|s| {
                        (
                            s.desc.clone(),
                            format!("{bom}{}", apply_fixes(code, &[&s.fix]).0),
                        )
                    })
                    .collect(),
            }
        })
        .collect();
    actual.sort();
    let mut expected = expected_msgs(rule, case);
    // ESLint omits end positions for some reports; ignore them where absent, and ignore
    // message ids we do not set.
    for (e, a) in expected.iter_mut().zip(actual.iter_mut()) {
        if e.end.is_none() {
            a.end = None;
        }
        if a.message_id.is_none() {
            e.message_id = None;
        }
    }
    actual.sort();
    expected.sort();
    if actual != expected {
        return Err(format!(
            "messages differ\n    expected: {}\n    actual:   {}",
            fmt_msgs(&expected),
            fmt_msgs(&actual)
        ));
    }
    let fixes: Vec<&ts_r_lint::Fix> = r
        .diagnostics
        .iter()
        .filter_map(|d| d.fix.as_ref())
        .collect();
    let out = if fixes.is_empty() {
        None
    } else {
        Some(format!("{bom}{}", apply_fixes(code, &fixes).0))
    };
    let exp_out = case["output"].as_str().map(str::to_owned);
    // ESLint reports `output: null` when no fix applies; a fix that changes nothing counts
    // the same.
    let out = out.filter(|o| o != raw || exp_out.is_some());
    if out != exp_out {
        return Err(format!(
            "output differs\n    expected: {exp_out:?}\n    actual:   {out:?}"
        ));
    }
    Ok(())
}

fn rule_static(reg: &'static Registry, rule: &str) -> &'static str {
    reg.find(rule).map(|d| d.meta.name).unwrap_or("")
}

fn fmt_msgs(m: &[Msg]) -> String {
    m.iter()
        .map(|m| {
            let end = m.end.map(|(l, c)| format!("-{l}:{c}")).unwrap_or_default();
            let sugg = if m.suggestions.is_empty() {
                String::new()
            } else {
                format!(" sugg{:?}", m.suggestions)
            };
            format!(
                "\n      {}:{}{end} {:?} [{}]{sugg}",
                m.line,
                m.column,
                m.message,
                m.message_id.as_deref().unwrap_or("")
            )
        })
        .collect()
}

fn load_baseline(path: &Path) -> BTreeMap<String, usize> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let name = it.next()?;
            let passed: usize = it.next()?.split('/').next()?.parse().ok()?;
            Some((name.to_owned(), passed))
        })
        .collect()
}

enum FileResult {
    /// Left out by `TS_R_LINT_ORACLE_RULE`.
    Filtered,
    NotImplemented(String),
    /// `log` holds the mismatches `TS_R_LINT_ORACLE_VERBOSE` asked for.
    Ran {
        rule: String,
        tally: Tally,
        log: String,
    },
}

/// Replays one rule's recorded cases.
fn run_file(reg: &'static Registry, f: &Path, filter: Option<&str>, verbose: usize) -> FileResult {
    let data: Value = serde_json::from_str(&std::fs::read_to_string(f).unwrap()).unwrap();
    let rule = data["rule"].as_str().unwrap_or("").to_owned();
    if let Some(fl) = filter
        && !fl.split(',').any(|f| rule.contains(f))
    {
        return FileResult::Filtered;
    }
    if reg.find(&rule).is_none() {
        return FileResult::NotImplemented(rule);
    }
    let mut tally = Tally::default();
    let mut log = String::new();
    let mut shown = 0;
    // File-level `globals` (a tester's defaults, eslint-plugin-unicorn's) apply to every
    // case under its own.
    let file_globals = data["globals"].as_object();
    for case in data["cases"].as_array().into_iter().flatten() {
        tally.total += 1;
        let merged;
        let case = match file_globals {
            Some(g) => {
                let mut c = case.clone();
                let mut all = g.clone();
                if let Some(own) = case["languageOptions"]["globals"].as_object() {
                    all.extend(own.clone());
                }
                if !c["languageOptions"].is_object() {
                    c["languageOptions"] = json!({});
                }
                c["languageOptions"]["globals"] = Value::Object(all);
                merged = c;
                &merged
            }
            None => case,
        };
        let res = std::panic::catch_unwind(|| run_case(reg, &rule, case))
            .unwrap_or_else(|_| Err("panicked".to_owned()));
        match res {
            Ok(()) => tally.passed += 1,
            Err(e) => {
                if shown < verbose {
                    shown += 1;
                    log.push_str(&format!(
                        "--- {rule} [{}] options={} \n{}\n  {e}\n",
                        case["filename"].as_str().unwrap_or(""),
                        case.get("options")
                            .map(|o| o.to_string())
                            .unwrap_or_default(),
                        case["code"].as_str().unwrap_or("")
                    ));
                }
            }
        }
    }
    FileResult::Ran { rule, tally, log }
}

#[test]
fn rule_oracle() {
    let reg = Registry::global();
    let dir = oracle_dir().join("rules");
    let filter = std::env::var("TS_R_LINT_ORACLE_RULE").ok();
    let verbose: usize = match std::env::var("TS_R_LINT_ORACLE_VERBOSE") {
        Ok(v) => v.parse().unwrap_or(usize::MAX),
        Err(_) => 0,
    };
    let bless = std::env::var_os("TS_R_LINT_ORACLE_BLESS").is_some();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("oracle dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    // Rule files are independent: replay them on up to 4 threads (the CI and corpus limit),
    // then report in file order so the output stays deterministic.
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(4));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: std::sync::Mutex<Vec<(usize, FileResult)>> = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(f) = files.get(i) else { break };
                    let r = run_file(reg, f, filter.as_deref(), verbose);
                    results.lock().unwrap().push((i, r));
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|(i, _)| *i);
    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    let mut not_implemented = Vec::new();
    for (_, r) in results {
        match r {
            FileResult::Filtered => {}
            FileResult::NotImplemented(rule) => not_implemented.push(rule),
            FileResult::Ran { rule, tally, log } => {
                eprint!("{log}");
                tallies.insert(rule, tally);
            }
        }
    }
    let (mut total, mut passed) = (0, 0);
    let mut report = String::from("# rule passed/total (generated by rule_oracle; do not edit)\n");
    for (r, t) in &tallies {
        total += t.total;
        passed += t.passed;
        report.push_str(&format!("{r} {}/{}\n", t.passed, t.total));
    }
    eprintln!("{report}");
    eprintln!(
        "oracle: {passed}/{total} cases pass across {} rules; {} rules with data not implemented: {}",
        tallies.len(),
        not_implemented.len(),
        not_implemented.join(", ")
    );
    let baseline_path = oracle_dir().join("baseline.txt");
    if bless && filter.is_none() {
        std::fs::write(&baseline_path, report).unwrap();
        return;
    }
    let baseline = load_baseline(&baseline_path);
    let mut regressions = Vec::new();
    for (r, t) in &tallies {
        if let Some(&b) = baseline.get(r)
            && t.passed < b
        {
            regressions.push(format!("{r}: {} < baseline {b}", t.passed));
        }
    }
    assert!(
        regressions.is_empty(),
        "oracle regressions:\n{}",
        regressions.join("\n")
    );
}
