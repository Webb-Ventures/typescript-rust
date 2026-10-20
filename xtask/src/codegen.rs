//! `cargo xtask codegen`: generate Rust data from synced upstream inputs.
//!
//! Generated files are never edited by hand. `--check` fails if the file on disk differs from
//! what would be generated (used in CI and bump PRs).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::args::Args;
use crate::upstream::{DiagEntry, TS_DIR, merged_catalog};
use crate::util::{Result, rel, root, run_capture};

const DEFAULT_OUT: &str = "target/codegen/diagnostics_generated.rs";

pub fn run(mut args: Args) -> Result<ExitCode> {
    let check = args.flag("check");
    let out = args.opt("out")?;
    let input = args.opt("input")?;
    let target = args.positional();
    args.finish()?;
    match target.as_deref() {
        Some("diagnostics") => diagnostics(check, out, input),
        Some("lint-globals") => lint_globals(check, out, input),
        Some(other) => Err(format!(
            "unknown codegen target `{other}` (known: diagnostics, lint-globals)"
        )
        .into()),
        None => {
            if out.is_some() {
                return Err("--out needs a target (diagnostics or lint-globals)".into());
            }
            // Every target. With `--check`, only the generated files that are checked in:
            // the diagnostics catalog's default output lives in `target/`.
            let mut code = ExitCode::SUCCESS;
            if check {
                println!(
                    "codegen: diagnostics has no checked-in output; check it with \
                     `cargo xtask codegen diagnostics --check --out <file>`"
                );
            } else {
                code = diagnostics(false, None, input.clone())?;
            }
            let lg = lint_globals(check, None, input)?;
            if lg != ExitCode::SUCCESS {
                code = lg;
            }
            Ok(code)
        }
    }
}

fn ts_commit(ts_dir: &Path) -> String {
    run_capture(
        Command::new("git")
            .arg("-C")
            .arg(ts_dir)
            .args(["rev-parse", "HEAD"]),
    )
    .map(|s| s.trim().to_string())
    .unwrap_or_else(|_| "unknown".into())
}

/// Writes `source` to `out_path`, or with `check` compares it with the file on disk.
fn emit(check: bool, out_path: &Path, source: &str, target: &str, what: &str) -> Result<ExitCode> {
    if check {
        let current = std::fs::read_to_string(out_path).unwrap_or_default();
        if current != source {
            println!(
                "codegen: {} is out of date; run `cargo xtask codegen {target}`",
                rel(out_path)
            );
            return Ok(ExitCode::from(1));
        }
        println!("codegen: {} is up to date", rel(out_path));
        return Ok(ExitCode::SUCCESS);
    }
    write_file(out_path, source)?;
    println!("codegen: wrote {what} to {}", rel(out_path));
    Ok(ExitCode::SUCCESS)
}

/// The TypeScript lib globals table for ts-r-lint (checked in at
/// `crates/ts_r_lint/src/lib_globals_generated.rs`).
fn lint_globals(check: bool, out: Option<String>, input: Option<String>) -> Result<ExitCode> {
    let ts_dir = input
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join(TS_DIR));
    let libs_dir = ts_dir.join(crate::lint_globals::LIBS_DIR);
    if !libs_dir.is_dir() {
        return Err(format!(
            "no lib files under {} - run `cargo xtask upstream sync --minimal` first",
            libs_dir.display()
        )
        .into());
    }
    let libs = crate::lint_globals::read_libs(&libs_dir)?;
    let source = crate::lint_globals::render(&libs, &ts_commit(&ts_dir));
    let out_path = match out {
        Some(p) => PathBuf::from(p),
        None => root().join(crate::lint_globals::DEFAULT_OUT),
    };
    let what = format!("{} lib files", libs.len());
    emit(check, &out_path, &source, "lint-globals", &what)
}

fn diagnostics(check: bool, out: Option<String>, input: Option<String>) -> Result<ExitCode> {
    let ts_dir = input
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join(TS_DIR));
    // Later catalogs win: tsc 7's extra catalog rewords some codes for TS 7.
    let (entries, sources) = merged_catalog(|p| std::fs::read_to_string(ts_dir.join(p)).ok())?;
    if sources.is_empty() {
        return Err(format!(
            "no diagnosticMessages.json under {} - run `cargo xtask upstream sync --minimal` first",
            ts_dir.display()
        )
        .into());
    }
    let commit = ts_commit(&ts_dir);
    let entries: Vec<DiagEntry> = entries.into_values().collect();
    let source = render_diagnostics(&entries, &commit, &sources);

    let out_path = match out {
        Some(p) => PathBuf::from(p),
        None => root().join(DEFAULT_OUT),
    };
    if check {
        let current = std::fs::read_to_string(&out_path).unwrap_or_default();
        if current != source {
            println!(
                "codegen: {} is out of date; run `cargo xtask codegen diagnostics --out {}`",
                rel(&out_path),
                rel(&out_path)
            );
            return Ok(ExitCode::from(1));
        }
        println!("codegen: {} is up to date", rel(&out_path));
        return Ok(ExitCode::SUCCESS);
    }
    write_file(&out_path, &source)?;
    println!(
        "codegen: wrote {} diagnostics to {}",
        entries.len(),
        rel(&out_path)
    );
    Ok(ExitCode::SUCCESS)
}

fn write_file(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    Ok(())
}

/// `SCREAMING_SNAKE_CASE` identifier from a message template plus its code.
pub fn const_name(message: &str, code: u32) -> String {
    let mut name = String::new();
    let mut pending_sep = false;
    for ch in message.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_sep && !name.is_empty() {
                name.push('_');
            }
            pending_sep = false;
            name.push(ch.to_ascii_uppercase());
        } else {
            pending_sep = true;
        }
        if name.len() >= 80 {
            break;
        }
    }
    let name = name.trim_end_matches('_');
    let mut out = String::new();
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        out.push('_');
    }
    out.push_str(name);
    if !out.is_empty() && out != "_" {
        out.push('_');
    }
    let _ = write!(out, "{code}");
    out
}

fn category_variant(category: &str) -> &'static str {
    match category {
        "Warning" => "Warning",
        "Suggestion" => "Suggestion",
        "Message" => "Message",
        _ => "Error",
    }
}

/// Renders the generated Rust module. Output depends only on the inputs (deterministic).
pub fn render_diagnostics(entries: &[DiagEntry], commit: &str, sources: &[&str]) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "// @generated by `cargo xtask codegen diagnostics` from microsoft/TypeScript@{commit}\n\
         // ({}).\n\
         // Do not edit by hand. Message text: Copyright (c) Microsoft Corporation, Apache-2.0.\n",
        sources.join(" + ")
    );
    s.push_str(
        "/// How a diagnostic is reported.\n\
         #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n\
         pub enum Category {\n    Error,\n    Warning,\n    Suggestion,\n    Message,\n}\n\n\
         /// A diagnostic code and its message template (`{0}`-style placeholders).\n\
         #[derive(Clone, Copy, Debug, PartialEq, Eq)]\n\
         pub struct DiagnosticInfo {\n    pub code: u32,\n    pub category: Category,\n    pub message: &'static str,\n    /// Editors fade the span (unused code).\n    pub unnecessary: bool,\n    /// Editors strike the span through (deprecated API).\n    pub deprecated: bool,\n}\n\n",
    );
    let mut names = std::collections::BTreeSet::new();
    let mut idents = Vec::with_capacity(entries.len());
    for e in entries {
        let mut name = const_name(&e.message, e.code);
        while !names.insert(name.clone()) {
            name.push_str("_DUP");
        }
        let _ = writeln!(
            s,
            "pub const {name}: DiagnosticInfo = DiagnosticInfo {{ code: {}, category: Category::{}, message: {:?}, unnecessary: {}, deprecated: {} }};",
            e.code,
            category_variant(&e.category),
            e.message,
            e.unnecessary,
            e.deprecated
        );
        idents.push(name);
    }
    let _ = writeln!(
        s,
        "\n/// Every diagnostic, sorted by code.\npub static ALL: &[DiagnosticInfo] = &["
    );
    for name in &idents {
        let _ = writeln!(s, "    {name},");
    }
    s.push_str(
        "];\n\n\
         /// Looks up a diagnostic by its numeric code.\n\
         pub fn lookup(code: u32) -> Option<&'static DiagnosticInfo> {\n    \
         ALL.binary_search_by_key(&code, |d| d.code).ok().map(|i| &ALL[i])\n}\n",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(
            const_name("Unterminated string literal.", 1002),
            "UNTERMINATED_STRING_LITERAL_1002"
        );
        assert_eq!(const_name("'{0}' expected.", 1005), "_0_EXPECTED_1005");
        assert_eq!(const_name("...", 7), "_7");
        assert!(const_name(&"word ".repeat(100), 1).len() < 100);
    }

    #[test]
    fn render_is_sorted_and_escaped() {
        let entries = vec![
            DiagEntry {
                code: 1002,
                category: "Error".into(),
                message: "Unterminated string literal.".into(),
                unnecessary: false,
                deprecated: false,
            },
            DiagEntry {
                code: 2322,
                category: "Error".into(),
                message: "Type '{0}' is not assignable to type \"{1}\".".into(),
                unnecessary: false,
                deprecated: false,
            },
            DiagEntry {
                code: 6133,
                category: "Suggestion".into(),
                message: "'{0}' is declared but its value is never read.".into(),
                unnecessary: false,
                deprecated: false,
            },
        ];
        let src = render_diagnostics(&entries, "abc", &["a.json"]);
        assert!(src.contains("pub const UNTERMINATED_STRING_LITERAL_1002: DiagnosticInfo"));
        assert!(src.contains(r#"message: "Type '{0}' is not assignable to type \"{1}\".""#));
        assert!(src.contains("category: Category::Suggestion"));
        let a = src.find("    UNTERMINATED_STRING_LITERAL_1002,").unwrap();
        let b = src
            .find("    TYPE_0_IS_NOT_ASSIGNABLE_TO_TYPE_1_2322,")
            .unwrap();
        assert!(a < b);
        assert_eq!(src, render_diagnostics(&entries, "abc", &["a.json"]));
    }
}
