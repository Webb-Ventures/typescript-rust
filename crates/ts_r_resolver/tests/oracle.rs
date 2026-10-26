//! Compares our resolver with `tsc --traceResolution`.
//!
//! Every directory under `tests/fixtures/` is a project; for each `tsconfig*.json` at its
//! root, `expected/<config>.trace.txt` holds one line per resolution tsc performed:
//!
//! ```text
//! module<TAB>file<TAB>import|require<TAB>specifier<TAB>resolved path or NOT RESOLVED<TAB>package id
//! types<TAB>file<TAB>import|require<TAB>name<TAB>resolved path or NOT RESOLVED<TAB>package id<TAB>primary
//! ```
//!
//! The fixture's absolute path is stored as `${FIXTURE}`. The mode comes from the conditions
//! tsc printed (for a type reference, which prints none, from the `import`/`require`
//! condition its `exports` lookup matched; `require` when neither matched). Regenerate with
//! `cargo test -p ts_r_resolver --test oracle -- --ignored regenerate` (uses `$TS_R_TSC` or
//! `npx -y -p typescript@7.0.2 tsc`); the version is in `tests/fixtures/TSC_VERSION.txt`.

use std::path::{Path, PathBuf};
use std::process::Command;

use ts_r_config::{RealFs, load_tsconfig, path};
use ts_r_resolver::{ResolutionMode, Resolver, ResolverOptions};

// External projects are real-world input.
ts_r_testing::cap_memory!();

const PLACEHOLDER: &str = "${FIXTURE}";
const INFERRED: &str = "__inferred type names__.ts";

#[derive(Debug, Clone, PartialEq, Eq)]
struct TraceLine {
    kind: String,
    file: String,
    mode: String,
    spec: String,
    result: String,
    package_id: String,
    primary: String,
}

impl TraceLine {
    fn render(&self) -> String {
        let mut s = format!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            self.kind, self.file, self.mode, self.spec, self.result, self.package_id
        );
        if self.kind == "types" {
            s.push('\t');
            s.push_str(&self.primary);
        }
        s
    }

    fn parse(line: &str) -> Option<Self> {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 6 {
            return None;
        }
        Some(Self {
            kind: parts[0].into(),
            file: parts[1].into(),
            mode: parts[2].into(),
            spec: parts[3].into(),
            result: parts[4].into(),
            package_id: parts[5].into(),
            primary: parts.get(6).copied().unwrap_or("").into(),
        })
    }
}

fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let rest = s.strip_prefix(start)?;
    rest.strip_suffix(end)
}

/// Parses `--traceResolution` output. Nested resolutions (e.g. `imports` targets that are
/// packages) have no end marker; they are dropped.
fn parse_trace(output: &str) -> Vec<TraceLine> {
    let mut stack: Vec<TraceLine> = Vec::new();
    let mut out: Vec<TraceLine> = Vec::new();
    for line in output.lines() {
        if let Some(rest) = between(line, "======== Resolving module '", "'. ========") {
            if let Some((spec, file)) = rest.split_once("' from '") {
                stack.push(TraceLine {
                    kind: "module".into(),
                    file: file.into(),
                    mode: "?".into(),
                    spec: spec.into(),
                    result: String::new(),
                    package_id: String::new(),
                    primary: String::new(),
                });
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("======== Resolving type reference directive '") {
            if let Some((name, rest)) = rest.split_once("', containing file '") {
                let file = rest.split("', root directory").next().unwrap_or(rest);
                let file = file.strip_suffix("'. ========").unwrap_or(file);
                stack.push(TraceLine {
                    kind: "types".into(),
                    file: file.into(),
                    mode: "?".into(),
                    spec: name.into(),
                    result: String::new(),
                    package_id: String::new(),
                    primary: String::new(),
                });
            }
            continue;
        }
        if let Some(conds) = line
            .strip_prefix("Resolving in ")
            .and_then(|r| r.split_once(" with conditions "))
        {
            if let Some(top) = stack.last_mut()
                && top.mode == "?"
            {
                top.mode = if conds.1.contains("'import'") {
                    "import"
                } else {
                    "require"
                }
                .into();
            }
            continue;
        }
        // A type reference prints no "Resolving in" line: the first `import`/`require`
        // condition tsc matched in a package's `exports` gives the mode away (only the
        // mode's own condition can match). With neither, the mode did not matter.
        if let Some(cond) = line.strip_prefix("Matched 'exports' condition '") {
            if let Some(top) = stack.last_mut()
                && top.mode == "?"
            {
                if cond.starts_with("import'") {
                    top.mode = "import".into();
                } else if cond.starts_with("require'") {
                    top.mode = "require".into();
                }
            }
            continue;
        }
        let (name, result) = if let Some(rest) = line
            .strip_prefix("======== Module name '")
            .or_else(|| line.strip_prefix("======== Type reference directive '"))
        {
            if let Some((name, r)) = rest.split_once("' was successfully resolved to '") {
                (name, Some(r.strip_suffix(". ========").unwrap_or(r)))
            } else if let Some(name) = rest.strip_suffix("' was not resolved. ========") {
                (name, None)
            } else {
                continue;
            }
        } else {
            continue;
        };
        while stack.len() > 1 && stack.last().is_some_and(|t| t.spec != name) {
            stack.pop();
        }
        let Some(mut cur) = stack.pop() else { continue };
        match result {
            None => cur.result = "NOT RESOLVED".into(),
            Some(r) => {
                // `path'[ with Package ID 'id'][, primary: bool]`
                let (r, primary) = match r.rsplit_once(", primary: ") {
                    Some((a, p)) => (a, p),
                    None => (r, ""),
                };
                let (file, pid) = match r.split_once("' with Package ID '") {
                    Some((f, p)) => (f, p.trim_end_matches('\'')),
                    None => (r.trim_end_matches('\''), ""),
                };
                cur.result = file.into();
                cur.package_id = pid.into();
                cur.primary = primary.into();
            }
        }
        if cur.mode == "?" {
            cur.mode = "require".into();
        }
        if !out.contains(&cur) {
            out.push(cur);
        }
    }
    out
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fixture_projects() -> Vec<(PathBuf, Vec<String>)> {
    let mut out = Vec::new();
    let mut dirs: Vec<_> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for d in dirs {
        let mut configs: Vec<String> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with("tsconfig") && n.ends_with(".json"))
            .collect();
        configs.sort();
        out.push((d, configs));
    }
    out
}

/// Runs our resolver for `want` and renders the result in the same shape.
fn ours(resolver: &Resolver<&RealFs>, want: &TraceLine) -> TraceLine {
    let mode = if want.mode == "import" {
        ResolutionMode::Import
    } else {
        ResolutionMode::Require
    };
    let mut got = TraceLine {
        result: "NOT RESOLVED".into(),
        package_id: String::new(),
        primary: String::new(),
        ..want.clone()
    };
    if want.kind == "types" {
        let containing = if want.file.ends_with(INFERRED) {
            None
        } else {
            Some(want.file.as_str())
        };
        if let Some(r) = resolver.resolve_type_reference(&want.spec, containing, mode) {
            got.result = r.resolved_file_name.clone();
            got.package_id = r
                .package_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default();
            got.primary = r.primary.to_string();
        }
    } else if let Some(r) = resolver.resolve_module(&want.spec, &want.file, mode) {
        got.result = r.resolved_file_name.clone();
        got.package_id = r
            .package_id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
    }
    got
}

/// Compares every line; returns (compared, mismatch descriptions).
fn compare(fs: &RealFs, config: &str, want: &[TraceLine]) -> (usize, Vec<String>) {
    let parsed = load_tsconfig(fs, config);
    let resolver = Resolver::new(fs, ResolverOptions::from_compiler_options(&parsed.options));
    let mut failures = Vec::new();
    for w in want {
        let got = ours(&resolver, w);
        if &got != w {
            failures.push(format!("  want {}\n  got  {}", w.render(), got.render()));
        }
    }
    (want.len(), failures)
}

fn with_placeholder(lines: &[TraceLine], fixture: &str, to_placeholder: bool) -> Vec<TraceLine> {
    let (from, to) = if to_placeholder {
        (fixture, PLACEHOLDER)
    } else {
        (PLACEHOLDER, fixture)
    };
    lines
        .iter()
        .map(|l| TraceLine {
            file: l.file.replace(from, to),
            result: l.result.replace(from, to),
            ..l.clone()
        })
        .collect()
}

/// A type reference's trace has no conditions line; the matched `exports` condition decides
/// (tRPC's `/// <reference types="vitest" />` under bundler matched `import`).
#[test]
fn type_reference_mode_from_matched_condition() {
    let trace = "======== Resolving type reference directive 'v', containing file '/p/a.ts', root directory '/p/node_modules/@types'. ========\n\
        Entering conditional exports.\n\
        Matched 'exports' condition 'import'.\n\
        Matched 'exports' condition 'types'.\n\
        ======== Type reference directive 'v' was successfully resolved to '/p/node_modules/v/i.d.ts', primary: false. ========\n\
        ======== Resolving type reference directive 'w', containing file '/p/a.ts', root directory '/p/node_modules/@types'. ========\n\
        ======== Type reference directive 'w' was successfully resolved to '/p/node_modules/@types/w/index.d.ts', primary: true. ========\n";
    let lines = parse_trace(trace);
    let modes: Vec<&str> = lines.iter().map(|l| l.mode.as_str()).collect();
    assert_eq!(modes, ["import", "require"]);
}

#[test]
fn fixtures_match_tsc_trace() {
    let fs = RealFs::new();
    let mut failures = Vec::new();
    let mut compared = 0;
    for (dir, configs) in fixture_projects() {
        let fixture = path::normalize(&dir.to_string_lossy());
        for cfg in configs {
            let expected = dir.join("expected").join(format!("{cfg}.trace.txt"));
            let text = std::fs::read_to_string(&expected)
                .unwrap_or_else(|_| panic!("missing {}", expected.display()));
            let want: Vec<TraceLine> = text.lines().filter_map(TraceLine::parse).collect();
            let want = with_placeholder(&want, &fixture, false);
            let (n, f) = compare(&fs, &path::combine(&fixture, &cfg), &want);
            compared += n;
            if !f.is_empty() {
                failures.push(format!(
                    "{}/{cfg}:\n{}",
                    dir.file_name().unwrap().to_string_lossy(),
                    f.join("\n").replace(&fixture, PLACEHOLDER)
                ));
            }
        }
    }
    assert!(compared > 100, "only {compared} resolutions found");
    assert!(
        failures.is_empty(),
        "mismatches against tsc ({compared} resolutions):\n{}",
        failures.join("\n")
    );
}

/// tsc under `scripts/limit.sh`, which the root AGENTS.md requires for oracle runs (limit.sh
/// is bash: Windows runs tsc directly).
fn tsc_command() -> Command {
    let (program, args): (String, &[&str]) = match std::env::var("TS_R_TSC") {
        Ok(p) => (p, &[]),
        Err(_) => ("npx".into(), &["-y", "-p", "typescript@7.0.2", "tsc"]),
    };
    let mut c = if cfg!(windows) {
        Command::new(program)
    } else {
        let limit = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/limit.sh");
        let mut c = Command::new(limit);
        c.arg("--").arg(program);
        c
    };
    c.args(args);
    c
}

fn run_trace(dir: &Path, cfg: &str) -> Vec<TraceLine> {
    let out = tsc_command()
        .current_dir(dir)
        .args([
            "-p",
            cfg,
            "--traceResolution",
            "--noEmit",
            "--pretty",
            "false",
        ])
        .output()
        .expect("run tsc");
    parse_trace(&String::from_utf8_lossy(&out.stdout))
}

#[test]
#[ignore = "runs the real tsc; use to regenerate expected traces"]
fn regenerate() {
    let version = tsc_command().arg("--version").output().expect("run tsc");
    let version = String::from_utf8_lossy(&version.stdout).trim().to_owned();
    std::fs::write(
        fixtures_dir().join("TSC_VERSION.txt"),
        format!("{version}\n"),
    )
    .unwrap();
    for (dir, configs) in fixture_projects() {
        let fixture = path::normalize(&dir.to_string_lossy());
        std::fs::create_dir_all(dir.join("expected")).unwrap();
        for cfg in configs {
            let lines = with_placeholder(&run_trace(&dir, &cfg), &fixture, true);
            let body: String = lines.iter().map(|l| format!("{}\n", l.render())).collect();
            std::fs::write(dir.join("expected").join(format!("{cfg}.trace.txt")), body).unwrap();
        }
    }
}

fn find_tsconfigs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if name != "node_modules" && !name.starts_with('.') {
                find_tsconfigs(&e.path(), out);
            }
        } else if name.starts_with("tsconfig") && name.ends_with(".json") {
            out.push(e.path());
        }
    }
}

/// Every import tsc resolves in the projects under `$TS_R_EXTERNAL_PROJECTS`
/// (`:`-separated roots; every `tsconfig*.json` below them, skipping `node_modules`):
/// `TS_R_EXTERNAL_PROJECTS=/path/to/repo cargo test -p ts_r_resolver --test oracle -- --ignored external --nocapture`
#[test]
#[ignore = "needs external projects and the real tsc"]
fn external_projects_match_tsc() {
    let roots = std::env::var("TS_R_EXTERNAL_PROJECTS").unwrap_or_default();
    let fs = RealFs::new();
    let mut failures = Vec::new();
    let mut compared = 0;
    let mut configs_seen = 0;
    for root in roots.split(':').filter(|r| !r.is_empty()) {
        let mut configs = Vec::new();
        find_tsconfigs(Path::new(root), &mut configs);
        for cfg in configs {
            let dir = cfg.parent().unwrap();
            let name = cfg.file_name().unwrap().to_string_lossy().into_owned();
            let want = run_trace(dir, &name);
            let (n, f) = compare(&fs, &path::normalize(&cfg.to_string_lossy()), &want);
            configs_seen += 1;
            compared += n;
            eprintln!("{}: {n} resolutions, {} mismatches", cfg.display(), f.len());
            if !f.is_empty() {
                failures.push(format!("{}:\n{}", cfg.display(), f.join("\n")));
            }
        }
    }
    eprintln!(
        "{configs_seen} configs, {compared} resolutions, {} configs with mismatches",
        failures.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
