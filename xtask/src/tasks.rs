//! `cargo xtask tasks`: turn oracle differences (ours vs tsc) into a grouped markdown task list.
//!
//! Input is either one JSON file `{"ours": <oracle report>, "tsc": <oracle report>}` or two
//! oracle reports via `--ours a.json --tsc b.json`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;

use serde::Deserialize;

use crate::args::Args;
use crate::oracle::{Diagnostic, Report};
use crate::util::{Result, read_to_string, write_output};

#[derive(Debug, Deserialize)]
struct DiffInput {
    ours: Report,
    #[serde(alias = "expected")]
    tsc: Report,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// tsc reports it, we don't.
    Missing,
    /// We report it, tsc doesn't.
    Extra,
    /// Same file and code, different position or message.
    Mismatch,
}

#[derive(Debug, Clone)]
pub struct Difference {
    pub kind: Kind,
    pub expected: Option<Diagnostic>,
    pub actual: Option<Diagnostic>,
}

impl Difference {
    fn diag(&self) -> &Diagnostic {
        self.expected
            .as_ref()
            .or(self.actual.as_ref())
            .expect("one side is set")
    }
}

pub fn run(mut args: Args) -> Result<ExitCode> {
    let out = args.opt("out")?;
    let ours = args.opt("ours")?;
    let tsc = args.opt("tsc")?;
    let limit: usize = args
        .opt("limit")?
        .map(|s| s.parse())
        .transpose()
        .map_err(|_| "--limit must be a number")?
        .unwrap_or(20);
    let input = args.positional();
    args.finish()?;
    let (ours, tsc) = match (input, ours, tsc) {
        (Some(path), None, None) => {
            let d: DiffInput = serde_json::from_str(&read_to_string(Path::new(&path))?)
                .map_err(|e| format!("{path}: expected {{\"ours\": ..., \"tsc\": ...}}: {e}"))?;
            (d.ours, d.tsc)
        }
        (None, Some(a), Some(b)) => (load_report(&a)?, load_report(&b)?),
        _ => {
            return Err(
                "usage: cargo xtask tasks <diff.json> | --ours <a.json> --tsc <b.json> [--out file.md]"
                    .into(),
            );
        }
    };
    let diffs = compute_differences(&ours.diagnostics, &tsc.diagnostics);
    let md = render(&diffs, &ours, &tsc, limit);
    write_output(out.as_deref(), &md)?;
    Ok(ExitCode::SUCCESS)
}

fn load_report(path: &str) -> Result<Report> {
    serde_json::from_str(&read_to_string(Path::new(path))?)
        .map_err(|e| format!("{path}: not an oracle report: {e}").into())
}

/// Multiset difference, then pairs leftovers with the same project/file/code as mismatches.
pub fn compute_differences(ours: &[Diagnostic], tsc: &[Diagnostic]) -> Vec<Difference> {
    let mut counts: BTreeMap<&Diagnostic, i64> = BTreeMap::new();
    for d in tsc {
        *counts.entry(d).or_default() += 1;
    }
    for d in ours {
        *counts.entry(d).or_default() -= 1;
    }
    let mut missing = Vec::new();
    let mut extra = Vec::new();
    for (d, n) in counts {
        for _ in 0..n.max(0) {
            missing.push(d.clone());
        }
        for _ in 0..(-n).max(0) {
            extra.push(d.clone());
        }
    }
    let mut used = vec![false; extra.len()];
    let mut out = Vec::new();
    for m in missing {
        let pair = extra.iter().enumerate().position(|(i, e)| {
            !used[i] && e.project == m.project && e.file == m.file && e.code == m.code
        });
        match pair {
            Some(i) => {
                used[i] = true;
                out.push(Difference {
                    kind: Kind::Mismatch,
                    expected: Some(m),
                    actual: Some(extra[i].clone()),
                });
            }
            None => out.push(Difference {
                kind: Kind::Missing,
                expected: Some(m),
                actual: None,
            }),
        }
    }
    for (i, e) in extra.into_iter().enumerate() {
        if !used[i] {
            out.push(Difference {
                kind: Kind::Extra,
                expected: None,
                actual: Some(e),
            });
        }
    }
    out
}

/// Rough area of the compiler a diagnostic code belongs to (by tsc's code ranges).
pub fn area_for_code(code: u32) -> &'static str {
    match code {
        1000..=1999 => "syntax (parser)",
        2000..=2999 => "semantic (checker)",
        4000..=4999 => "declaration emit",
        5000..=5999 => "compiler options / tsconfig",
        6000..=6999 => "CLI / informational",
        7000..=7999 => "noImplicitAny & suggestions",
        8000..=8999 => "JavaScript / JSDoc",
        9000..=9999 => "isolatedDeclarations",
        17000..=17999 => "JSX",
        18000..=18999 => "checker (misc)",
        _ => "other",
    }
}

/// Feature folder for upstream test paths (`.../conformance/<feature>/...`).
fn feature_of(d: &Diagnostic) -> Option<String> {
    let path = format!("{}/{}", d.project, d.file);
    let mut parts = path.split('/');
    while let Some(p) = parts.next() {
        if p == "conformance" || p == "compiler" {
            let next = parts.next()?;
            return Some(if p == "compiler" {
                "compiler".to_string()
            } else {
                next.to_string()
            });
        }
    }
    None
}

fn loc(d: &Diagnostic) -> String {
    let file = if d.project == "." || d.project.is_empty() {
        d.file.clone()
    } else if d.file.is_empty() {
        d.project.clone()
    } else {
        format!("{}/{}", d.project, d.file)
    };
    if d.line == 0 {
        file
    } else {
        format!("{file}:{}:{}", d.line, d.col)
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

pub fn render(diffs: &[Difference], ours: &Report, tsc: &Report, limit: usize) -> String {
    let mut groups: BTreeMap<u32, Vec<&Difference>> = BTreeMap::new();
    for d in diffs {
        groups.entry(d.diag().code).or_default().push(d);
    }
    let mut ordered: Vec<(u32, Vec<&Difference>)> = groups.into_iter().collect();
    ordered.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));

    let count = |k: Kind| diffs.iter().filter(|d| d.kind == k).count();
    let mut md = String::new();
    let _ = writeln!(
        md,
        "# Conformance tasks: {} vs {} {}\n",
        ours.tool, tsc.tool, tsc.version
    );
    let _ = writeln!(
        md,
        "{} differences in {} groups: {} missing (tsc reports, we don't), {} extra (we report, tsc doesn't), {} mismatched (same file and code, different position or message).\n",
        diffs.len(),
        ordered.len(),
        count(Kind::Missing),
        count(Kind::Extra),
        count(Kind::Mismatch)
    );
    if diffs.is_empty() {
        md.push_str("No differences. Nothing to do.\n");
        return md;
    }
    let _ = writeln!(
        md,
        "Work one group at a time with the `fix-conformance-diff` skill; shrink inputs with `minimize-repro`.\n"
    );
    let _ = writeln!(md, "| Code | Area | Missing | Extra | Mismatch |");
    let _ = writeln!(md, "|---|---|---:|---:|---:|");
    for (code, items) in &ordered {
        let c = |k: Kind| items.iter().filter(|d| d.kind == k).count();
        let _ = writeln!(
            md,
            "| TS{code} | {} | {} | {} | {} |",
            area_for_code(*code),
            c(Kind::Missing),
            c(Kind::Extra),
            c(Kind::Mismatch)
        );
    }
    md.push('\n');
    for (code, items) in &ordered {
        let title = first_line(&items[0].diag().message);
        let features: BTreeSet<String> =
            items.iter().filter_map(|d| feature_of(d.diag())).collect();
        let _ = writeln!(md, "## TS{code}: {title}\n");
        let _ = write!(
            md,
            "Area: {} · {} differences",
            area_for_code(*code),
            items.len()
        );
        if !features.is_empty() {
            let f: Vec<&str> = features.iter().map(String::as_str).collect();
            let _ = write!(md, " · features: {}", f.join(", "));
        }
        md.push_str("\n\n");
        for d in items.iter().take(limit) {
            let line = match d.kind {
                Kind::Missing => {
                    let e = d.expected.as_ref().expect("missing has expected");
                    format!("missing `{}`: {}", loc(e), first_line(&e.message))
                }
                Kind::Extra => {
                    let a = d.actual.as_ref().expect("extra has actual");
                    format!("extra `{}`: {}", loc(a), first_line(&a.message))
                }
                Kind::Mismatch => {
                    let e = d.expected.as_ref().expect("mismatch has expected");
                    let a = d.actual.as_ref().expect("mismatch has actual");
                    let mut s = format!("mismatch `{}`", loc(e));
                    if (e.line, e.col) != (a.line, a.col) {
                        let _ = write!(s, " (we report {}:{})", a.line, a.col);
                    }
                    if e.message != a.message {
                        let _ = write!(
                            s,
                            ": expected \"{}\", got \"{}\"",
                            first_line(&e.message),
                            first_line(&a.message)
                        );
                    }
                    s
                }
            };
            let _ = writeln!(md, "- [ ] {line}");
        }
        if items.len() > limit {
            let _ = writeln!(md, "- ... and {} more", items.len() - limit);
        }
        md.push('\n');
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(file: &str, line: u32, code: u32, msg: &str) -> Diagnostic {
        Diagnostic {
            project: "tests/cases/conformance/types/union".into(),
            file: file.into(),
            line,
            col: 1,
            code,
            category: "error".into(),
            message: msg.into(),
        }
    }

    #[test]
    fn differences_are_classified() {
        let tsc = vec![
            d("a.ts", 1, 2322, "x"),
            d("a.ts", 5, 2322, "y"),
            d("b.ts", 2, 1005, "';' expected."),
            d("c.ts", 3, 2304, "Cannot find name 'q'."),
        ];
        let ours = vec![
            d("a.ts", 1, 2322, "x"),
            d("a.ts", 6, 2322, "y"),
            d("c.ts", 3, 2304, "Cannot find name 'q'."),
            d("d.ts", 9, 2339, "nope"),
        ];
        let diffs = compute_differences(&ours, &tsc);
        let kinds: Vec<Kind> = diffs.iter().map(|d| d.kind).collect();
        assert_eq!(kinds, vec![Kind::Mismatch, Kind::Missing, Kind::Extra]);
        let report = |tool: &str| Report {
            tool: tool.into(),
            version: "7.0.2".into(),
            diagnostics: vec![],
        };
        let md = render(&diffs, &report("custom"), &report("tsc"), 20);
        assert!(md.contains("3 differences in 3 groups"));
        assert!(md.contains("## TS2322: y"));
        assert!(md.contains("(we report 6:1)"));
        assert!(md.contains("features: types"));
        assert!(md.contains("| TS1005 | syntax (parser) | 1 | 0 | 0 |"));
    }

    #[test]
    fn identical_reports_have_no_tasks() {
        let a = vec![d("a.ts", 1, 2322, "x")];
        assert!(compute_differences(&a, &a).is_empty());
    }
}
