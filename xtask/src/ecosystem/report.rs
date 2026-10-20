//! Results (`ecosystem/results/<date>.json`), failure grouping and `ecosystem/STATUS.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::util::root;

pub const STAGES: &[&str] = &["parse", "transform", "bundle", "check", "tests"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Fail,
    /// The worker crashed, ran out of time or memory, or could not be set up.
    Error,
    /// Nothing to check (no TS sources, no types, no JS entry) or the oracle failed too.
    #[default]
    Skip,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StageResult {
    pub status: Status,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// Files (parse, transform), entry builds (bundle) or projects (check) compared.
    pub checked: u32,
    pub failed: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<Failure>,
    /// Every failure's cause with its count (`failures` keeps only the first
    /// [`MAX_FAILURES`] examples).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub causes: BTreeMap<String, u32>,
    pub ms: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metrics: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Failure {
    /// Grouping key: a diagnostic code and message, or an error signature.
    pub cause: String,
    /// Package-relative file (or entry specifier).
    pub file: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// Keep at most this many failures per stage result (the count is kept in `failed`).
pub const MAX_FAILURES: usize = 40;

impl StageResult {
    pub fn skip(note: impl Into<String>) -> Self {
        StageResult {
            status: Status::Skip,
            note: note.into(),
            ..Default::default()
        }
    }

    pub fn error(cause: &str, file: &str, detail: &str) -> Self {
        StageResult {
            status: Status::Error,
            note: cause.to_string(),
            checked: 1,
            failed: 1,
            failures: vec![Failure {
                cause: cause.to_string(),
                file: file.to_string(),
                detail: truncate(detail, 400),
            }],
            causes: BTreeMap::from([(cause.to_string(), 1)]),
            ..Default::default()
        }
    }

    pub fn fail(
        &mut self,
        cause: impl Into<String>,
        file: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.failed += 1;
        let cause = cause.into();
        *self.causes.entry(cause.clone()).or_default() += 1;
        if self.failures.len() < MAX_FAILURES {
            self.failures.push(Failure {
                cause,
                file: file.into(),
                detail: truncate(&detail.into(), 600),
            });
        }
    }

    /// Sets `status` from the counts.
    pub fn finish(&mut self) {
        if self.status == Status::Error {
            return;
        }
        self.status = if self.failed > 0 {
            Status::Fail
        } else if self.checked > 0 {
            Status::Pass
        } else {
            Status::Skip
        };
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageResult {
    pub rank: u32,
    pub name: String,
    pub version: String,
    pub stages: BTreeMap<String, StageResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunFile {
    pub date: String,
    pub git: String,
    pub list_generated: String,
    pub top: usize,
    pub stages: Vec<String>,
    pub seconds: f64,
    pub budget_exhausted: bool,
    pub tools: BTreeMap<String, String>,
    pub packages: Vec<PackageResult>,
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Normalizes a message into a grouping key: numbers become `N`, long quoted names `'…'`.
pub fn signature(msg: &str) -> String {
    let msg = msg.lines().next().unwrap_or("").trim();
    let mut out = String::with_capacity(msg.len());
    let mut chars = msg.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            let mut digits = String::from(c);
            while let Some(d) = chars.peek().copied().filter(char::is_ascii_digit) {
                digits.push(d);
                chars.next();
            }
            // Diagnostic codes stay; other numbers (positions, counts) are blanked.
            if out.ends_with("TS") {
                out.push_str(&digits);
            } else {
                out.push('N');
            }
        } else if c == '\'' || c == '"' {
            let mut inner = String::new();
            let mut closed = false;
            for d in chars.by_ref() {
                if d == c {
                    closed = true;
                    break;
                }
                inner.push(d);
            }
            out.push(c);
            // Keep punctuation tokens (`';' expected`), blank names and types.
            if !inner.is_empty()
                && !inner
                    .chars()
                    .any(|c| c.is_alphanumeric() || c == '_' || c == '$')
            {
                out.push_str(&inner);
            } else {
                out.push('…');
            }
            if closed {
                out.push(c);
            }
        } else {
            out.push(c);
        }
    }
    truncate(&out, 120)
}

const CRATES: &[&str] = &[
    "ts_r_lexer",
    "ts_r_parser",
    "ts_r_ast",
    "ts_r_transform",
    "ts_r_codegen",
    "ts_r_minify",
    "ts_r_bundler",
    "ts_r_build",
    "ts_r_resolver",
    "ts_r_config",
    "ts_r_binder",
    "ts_r_checker",
    "ts_r_check",
    "ts_r_types",
    "ts_r_diagnostics",
];

/// The crate that owns a failure cause.
pub fn owner(stage: &str, cause: &str) -> &'static str {
    let code = cause.split_whitespace().find_map(|w| {
        w.trim_start_matches("TS")
            .trim_end_matches(':')
            .parse::<u32>()
            .ok()
            .filter(|_| w.starts_with("TS"))
    });
    // A panic names its crate.
    for name in CRATES {
        if cause.contains(&format!("crates/{name}/")) {
            return name;
        }
    }
    match stage {
        "parse" => match code {
            Some(1002 | 1010 | 1125 | 1126 | 1127 | 1160 | 1161 | 1351 | 1487 | 1489) => {
                "ts_r_lexer"
            }
            _ if cause.starts_with("limit") || cause.starts_with("crash") => "ts_r_parser",
            _ => "ts_r_parser",
        },
        "transform" => {
            if cause.starts_with("output does not reparse") {
                "ts_r_codegen"
            } else {
                "ts_r_transform"
            }
        }
        "bundle" => {
            if cause.contains("minified") {
                "ts_r_minify"
            } else {
                "ts_r_bundler"
            }
        }
        "check" => {
            if matches!(code, Some(2307 | 2792 | 2834 | 2835 | 7016)) {
                "ts_r_resolver"
            } else if matches!(code, Some(5000..=6999)) {
                "ts_r_config"
            } else if matches!(code, Some(1000..=1999)) {
                "ts_r_parser"
            } else if cause.starts_with("crash") || cause.starts_with("limit") {
                "ts_r_check"
            } else {
                "ts_r_checker"
            }
        }
        "tests" => {
            if cause.contains("ts-r-check") {
                "ts_r_checker"
            } else if cause.contains("ts-r-build") || cause.contains("esbuild") {
                "ts_r_bundler"
            } else if cause.contains("ts-r-run")
                || cause.contains("tsx")
                || cause.contains("ts-node")
            {
                "ts_r_run"
            } else {
                "ts_r_transform"
            }
        }
        _ => "?",
    }
}

/// A file-name slug for a cause.
pub fn slug(cause: &str) -> String {
    let mut s = String::new();
    for c in cause.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
        if s.len() >= 60 {
            break;
        }
    }
    s.trim_end_matches('-').to_string()
}

#[derive(Debug, Default)]
pub struct Cause {
    pub stage: String,
    pub cause: String,
    pub packages: BTreeSet<String>,
    pub files: usize,
    pub example: Option<(String, Failure)>,
}

/// Failure causes over all packages, most packages affected first.
pub fn group(run: &RunFile) -> Vec<Cause> {
    let mut map: BTreeMap<(String, String), Cause> = BTreeMap::new();
    for p in &run.packages {
        for (stage, r) in &p.stages {
            // Older results have no `causes`: count the kept examples instead.
            let mut counts = r.causes.clone();
            if counts.is_empty() {
                for f in &r.failures {
                    *counts.entry(f.cause.clone()).or_default() += 1;
                }
            }
            for (cause, n) in counts {
                let c = map
                    .entry((stage.clone(), cause.clone()))
                    .or_insert_with(|| Cause {
                        stage: stage.clone(),
                        cause: cause.clone(),
                        ..Default::default()
                    });
                c.packages.insert(p.name.clone());
                c.files += n as usize;
                if c.example.is_none() {
                    c.example = r
                        .failures
                        .iter()
                        .find(|f| f.cause == cause)
                        .map(|f| (p.name.clone(), f.clone()));
                }
            }
        }
    }
    let mut v: Vec<Cause> = map.into_values().collect();
    v.sort_by(|a, b| {
        b.packages
            .len()
            .cmp(&a.packages.len())
            .then(b.files.cmp(&a.files))
            .then_with(|| (&a.stage, &a.cause).cmp(&(&b.stage, &b.cause)))
    });
    v
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Tally {
    pub pass: usize,
    pub fail: usize,
    pub error: usize,
    pub skip: usize,
}

impl Tally {
    pub fn rate(&self) -> f64 {
        let n = self.pass + self.fail + self.error;
        if n == 0 {
            0.0
        } else {
            100.0 * self.pass as f64 / n as f64
        }
    }
}

pub fn tally(run: &RunFile, stage: &str) -> Tally {
    let mut t = Tally::default();
    for p in &run.packages {
        match p.stages.get(stage).map(|r| r.status) {
            Some(Status::Pass) => t.pass += 1,
            Some(Status::Fail) => t.fail += 1,
            Some(Status::Error) => t.error += 1,
            Some(Status::Skip) => t.skip += 1,
            None => {}
        }
    }
    t
}

/// One-line pass-rate summary per stage (for logs and commit messages).
pub fn summary_line(run: &RunFile) -> String {
    let mut parts = Vec::new();
    for s in &run.stages {
        let t = tally(run, s);
        parts.push(format!(
            "{s} {}/{} ({:.1}%)",
            t.pass,
            t.pass + t.fail + t.error,
            t.rate()
        ));
    }
    parts.join(", ")
}

/// The markdown for `ecosystem/STATUS.md`.
pub fn status_markdown(run: &RunFile) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# Ecosystem status\n");
    let _ = writeln!(
        s,
        "Generated by `cargo xtask ecosystem` on {} at `{}`: the top {} packages of \
         [packages.toml](packages.toml) (list of {}), {:.0} s{}. Raw results: \
         `ecosystem/results/{}.json` (not committed). Triage and repros: [README.md](README.md).\n",
        run.date,
        run.git,
        run.top,
        run.list_generated,
        run.seconds,
        if run.budget_exhausted {
            ", **time budget exhausted** (some packages not run)"
        } else {
            ""
        },
        run.date
    );
    let tools: Vec<String> = run.tools.iter().map(|(k, v)| format!("{k} {v}")).collect();
    let _ = writeln!(s, "Tools: {}.\n", tools.join(", "));
    let _ = writeln!(s, "| Stage | Pass | Fail | Error | Skip | Pass rate |");
    let _ = writeln!(s, "| --- | ---: | ---: | ---: | ---: | ---: |");
    for stage in &run.stages {
        let t = tally(run, stage);
        let _ = writeln!(
            s,
            "| {stage} | {} | {} | {} | {} | {:.1}% |",
            t.pass,
            t.fail,
            t.error,
            t.skip,
            t.rate()
        );
    }
    let _ = writeln!(
        s,
        "\nPass rate = pass / (pass + fail + error). Skips have nothing to compare: no TS \
         sources (transform), no JavaScript entry or esbuild's own bundle fails (bundle), no \
         `.d.ts` (check), not selected or the suite fails with its own tools (tests).\n"
    );

    // Stage totals in files / entries.
    let _ = writeln!(s, "| Stage | Items compared | Items failing |");
    let _ = writeln!(s, "| --- | ---: | ---: |");
    for stage in &run.stages {
        let (mut c, mut f) = (0u64, 0u64);
        for p in &run.packages {
            if let Some(r) = p.stages.get(stage) {
                c += u64::from(r.checked);
                f += u64::from(r.failed);
            }
        }
        let _ = writeln!(s, "| {stage} | {c} | {f} |");
    }

    // Bundle size versus esbuild.
    if run.stages.iter().any(|s| s == "bundle") {
        let mut ratios: Vec<f64> = Vec::new();
        let (mut ours, mut theirs) = (0.0, 0.0);
        for p in &run.packages {
            if let Some(r) = p.stages.get("bundle")
                && let (Some(a), Some(b)) = (
                    r.metrics.get("min_bytes_ts_r"),
                    r.metrics.get("min_bytes_esbuild"),
                )
                && *b > 0.0
            {
                ours += a;
                theirs += b;
                ratios.push(a / b);
            }
        }
        if !ratios.is_empty() {
            ratios.sort_by(|a, b| a.total_cmp(b));
            let median = ratios[ratios.len() / 2];
            let smaller = ratios.iter().filter(|r| **r <= 1.0).count();
            let _ = writeln!(
                s,
                "\n## Bundle size versus esbuild (minified)\n\n{} packages with both bundles: \
                 ts-r-build {:.1} KiB total, esbuild {:.1} KiB ({:+.1}%). Median ratio {:.3}; \
                 {} of {} packages are the same size or smaller. Worst ratios:\n",
                ratios.len(),
                ours / 1024.0,
                theirs / 1024.0,
                100.0 * (ours / theirs - 1.0),
                median,
                smaller,
                ratios.len()
            );
            let mut worst: Vec<(f64, &str)> = run
                .packages
                .iter()
                .filter_map(|p| {
                    let r = p.stages.get("bundle")?;
                    let a = r.metrics.get("min_bytes_ts_r")?;
                    let b = r.metrics.get("min_bytes_esbuild")?;
                    (*b > 0.0).then_some((a / b, p.name.as_str()))
                })
                .collect();
            worst.sort_by(|a, b| b.0.total_cmp(&a.0));
            for (r, n) in worst.iter().take(5) {
                let _ = writeln!(s, "- {n}: {r:.3}×");
            }
        }
    }

    // The tests stage, package by package (it runs on few packages).
    let tested: Vec<(&PackageResult, &StageResult)> = run
        .packages
        .iter()
        .filter_map(|p| Some((p, p.stages.get("tests")?)))
        .filter(|(_, r)| {
            !(r.status == Status::Skip
                && (r.note.starts_with("tests run on the top") || r.note.starts_with("not among")))
        })
        .collect();
    if !tested.is_empty() {
        let _ = writeln!(
            s,
            "\n## Tests stage\n\nEach package's own suite at its git tag: once with its own tools \
             (baseline), once with tsc / tsx / ts-node / esbuild swapped for ts-r. When ts-r-check \
             fails the build, the suite runs a third time with ts-r-check's errors ignored, to \
             show whether ts-r's emit and loader pass it.\n"
        );
        let _ = writeln!(
            s,
            "| Package | Result | With check errors ignored | ts-r tools that ran | Baseline s | ts-r s |"
        );
        let _ = writeln!(s, "| --- | --- | --- | --- | ---: | ---: |");
        for (p, r) in tested {
            let result = match r.status {
                Status::Skip => format!("skip: {}", truncate(&r.note, 80)),
                Status::Pass => "pass".into(),
                _ => r
                    .failures
                    .first()
                    .map_or("fail".into(), |f| format!("fail: {}", f.cause)),
            };
            let ignored = match r.metrics.get("passes_ignoring_check") {
                Some(v) if *v > 0.0 => "pass",
                Some(_) => "fail",
                None => "",
            };
            let ran: Vec<String> = r
                .metrics
                .iter()
                .filter_map(|(k, v)| Some(format!("{} ×{v}", k.strip_prefix("ran_")?)))
                .collect();
            let secs = |k: &str| {
                r.metrics
                    .get(k)
                    .map_or(String::new(), |v| format!("{:.1}", v / 1000.0))
            };
            let _ = writeln!(
                s,
                "| {}@{} | {} | {} | {} | {} | {} |",
                p.name,
                p.version,
                result.replace('|', "\\|"),
                ignored,
                ran.join(", "),
                secs("baseline_ms"),
                secs("ts_r_ms")
            );
        }
    }

    let causes = group(run);
    let _ = writeln!(s, "\n## Top failure causes\n");
    if causes.is_empty() {
        let _ = writeln!(s, "None.");
    } else {
        let _ = writeln!(
            s,
            "| # | Stage | Cause | Packages | Items | Owner | Example | Repro |"
        );
        let _ = writeln!(s, "| ---: | --- | --- | ---: | ---: | --- | --- | --- |");
        for (i, c) in causes.iter().take(25).enumerate() {
            let example = c
                .example
                .as_ref()
                .map(|(p, f)| format!("{p}: `{}`", truncate(&f.file, 60).replace('|', "\\|")))
                .unwrap_or_default();
            let found = find_repro(&c.stage, &c.cause);
            let owner = found
                .as_deref()
                .and_then(repro_owner)
                .unwrap_or_else(|| owner(&c.stage, &c.cause).to_string());
            let repro = found
                .map(|p| format!("[{}]({p})", p.rsplit('/').next().unwrap_or(&p)))
                .unwrap_or_default();
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | {} | `{}` | {} | {} |",
                i + 1,
                c.stage,
                md_cell(&c.cause),
                c.packages.len(),
                c.files,
                owner,
                example,
                repro
            );
        }
    }

    let _ = writeln!(s, "\n## Failing packages\n");
    for stage in &run.stages {
        let mut names: Vec<String> = run
            .packages
            .iter()
            .filter(|p| {
                matches!(
                    p.stages.get(stage).map(|r| r.status),
                    Some(Status::Fail | Status::Error)
                )
            })
            .map(|p| p.name.clone())
            .collect();
        names.sort();
        if names.is_empty() {
            let _ = writeln!(s, "- **{stage}**: none");
        } else {
            let _ = writeln!(s, "- **{stage}** ({}): {}", names.len(), names.join(", "));
        }
    }
    s
}

fn md_cell(s: &str) -> String {
    truncate(s, 100)
        .replace('|', "\\|")
        .replace('\n', " ")
        .replace('`', "'")
}

/// The `// Owner: <crate>` line of a repro (relative to `ecosystem/`): triage knows better
/// than [`owner`]'s heuristic.
pub fn repro_owner(rel: &str) -> Option<String> {
    let text = std::fs::read_to_string(root().join("ecosystem").join(rel)).ok()?;
    text.lines().take(20).find_map(|l| {
        let rest = l
            .trim()
            .trim_start_matches("//")
            .trim_start_matches('#')
            .trim();
        let owner = rest.strip_prefix("Owner:")?.trim();
        let owner = owner.split_whitespace().next()?.trim_matches('`');
        (!owner.is_empty()).then(|| owner.to_string())
    })
}

/// `repros/<stage>/<slug>.{ts,js,...}` relative to `ecosystem/`, if one was written.
pub fn find_repro(stage: &str, cause: &str) -> Option<String> {
    // A tests-stage failure in ts-r-check is a checker bug: link the check repro for the code.
    if stage == "tests"
        && cause.contains("ts-r-check")
        && let Some(code) = cause
            .split(['(', ')'])
            .find(|w| w.starts_with("TS") && w[2..].chars().all(|c| c.is_ascii_digit()))
    {
        let prefix = format!("extra-{}-", code.to_ascii_lowercase());
        let mut names: Vec<String> = std::fs::read_dir(root().join("ecosystem/repros/check"))
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&prefix))
            .collect();
        names.sort();
        return names.first().map(|n| format!("repros/check/{n}"));
    }
    let slug = slug(cause);
    let dir = root().join("ecosystem/repros").join(stage);
    for ext in ["ts", "js", "mjs", "cjs", "tsx", "d.ts", "md"] {
        let name = format!("{slug}.{ext}");
        if dir.join(&name).is_file() {
            return Some(format!("repros/{stage}/{name}"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_group_similar_messages() {
        assert_eq!(signature("TS1005: ';' expected."), "TS1005: ';' expected.");
        assert_eq!(
            signature("Could not resolve \"some-long-name\" at 12:4"),
            "Could not resolve \"…\" at N:N"
        );
        assert_eq!(slug("TS1005: ';' expected."), "ts1005-expected");
    }

    #[test]
    fn owners() {
        assert_eq!(owner("parse", "TS1127: Invalid character."), "ts_r_lexer");
        assert_eq!(owner("parse", "TS1005: ';' expected."), "ts_r_parser");
        assert_eq!(
            owner("check", "missing TS2307: Cannot find module"),
            "ts_r_resolver"
        );
        assert_eq!(owner("bundle", "shape differs (minified)"), "ts_r_minify");
    }

    #[test]
    fn tallies_and_markdown() {
        let mut stages = BTreeMap::new();
        let mut r = StageResult {
            checked: 3,
            ..Default::default()
        };
        r.fail("TS1005: ';' expected.", "a.js", "");
        r.finish();
        stages.insert("parse".to_string(), r);
        let run = RunFile {
            date: "2026-10-15".into(),
            git: "abc".into(),
            list_generated: "2026-10-15".into(),
            top: 1,
            stages: vec!["parse".into()],
            seconds: 1.0,
            budget_exhausted: false,
            tools: BTreeMap::new(),
            packages: vec![PackageResult {
                rank: 1,
                name: "x".into(),
                version: "1.0.0".into(),
                stages,
            }],
        };
        let t = tally(&run, "parse");
        assert_eq!((t.pass, t.fail), (0, 1));
        let md = status_markdown(&run);
        assert!(md.contains("| parse | 0 | 1 | 0 | 0 | 0.0% |"), "{md}");
        assert!(md.contains("TS1005"));
        assert_eq!(group(&run).len(), 1);
    }
}
