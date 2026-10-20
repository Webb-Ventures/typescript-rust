//! `cargo xtask oracle <project-dir>...`: run tsc at the pinned version and normalize its
//! diagnostics to JSON, so ts-r's checker can be compared against it line by line.
//!
//! `--cmd "<program> <args>"` runs any tsc-compatible command instead (for example our own
//! `ts-r-check`), so both sides of a comparison go through the same normalization.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::{Deserialize, Serialize};

use crate::args::Args;
use crate::upstream::{load_pins, ts_pin};
use crate::util::{Result, limited, npm_tool, read_to_string, write_output};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Project directory as given on the command line, `/`-separated.
    pub project: String,
    /// File relative to the project directory (`""` for global diagnostics).
    pub file: String,
    /// 1-based line and column (`0` for global diagnostics).
    pub line: u32,
    pub col: u32,
    pub code: u32,
    pub category: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub tool: String,
    pub version: String,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn run(mut args: Args) -> Result<ExitCode> {
    let out = args.opt("out")?;
    let custom = args.opt("cmd")?;
    let from_output = args.opt("from-output")?;
    let projects = args.rest();
    args.finish()?;

    let pins = load_pins()?;
    let version = ts_pin(&pins).version.clone().unwrap_or_default();

    let mut report = Report {
        tool: if custom.is_some() {
            "custom".into()
        } else {
            "tsc".into()
        },
        version: if custom.is_some() {
            "unknown".into()
        } else {
            version.clone()
        },
        diagnostics: Vec::new(),
    };

    if let Some(path) = from_output {
        let project = projects.first().cloned().unwrap_or_else(|| ".".into());
        let text = read_to_string(Path::new(&path))?;
        report.diagnostics = parse_tsc_output(&text, &normalize_path(&project));
    } else {
        if projects.is_empty() {
            return Err(
                "usage: cargo xtask oracle <project-dir>... [--out file.json] [--cmd \"...\"]"
                    .into(),
            );
        }
        for project in &projects {
            let dir = Path::new(project);
            if !dir.is_dir() {
                return Err(format!("{project} is not a directory").into());
            }
            let mut cmd = match &custom {
                Some(c) => {
                    let mut parts = c.split_whitespace();
                    let program = parts.next().ok_or("--cmd is empty")?;
                    let mut cmd = limited(program_path(program, &std::env::current_dir()?));
                    cmd.args(parts);
                    cmd
                }
                None => {
                    let mut cmd = limited(npm_tool("npx"));
                    cmd.args(["-y", "-p", &format!("typescript@{version}"), "tsc"]);
                    cmd
                }
            };
            cmd.args(["--noEmit", "--pretty", "false", "-p", "."])
                .current_dir(dir);
            eprintln!("oracle: running {} in {project}", report.tool);
            let output = cmd
                .output()
                .map_err(|e| format!("failed to start {:?}: {e}", cmd.get_program()))?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let diags = parse_tsc_output(&stdout, &normalize_path(project));
            // tsc exits non-zero when it reports errors; only fail if it printed nothing usable.
            if !output.status.success() && diags.is_empty() {
                return Err(format!(
                    "{} failed in {project} ({}): {}{}",
                    report.tool,
                    output.status,
                    stdout.trim(),
                    String::from_utf8_lossy(&output.stderr).trim()
                )
                .into());
            }
            report.diagnostics.extend(diags);
        }
    }
    report.diagnostics.sort();
    eprintln!("oracle: {} diagnostics", report.diagnostics.len());
    write_output(
        out.as_deref(),
        &(serde_json::to_string_pretty(&report)? + "\n"),
    )?;
    Ok(ExitCode::SUCCESS)
}

/// `--cmd`'s program as the command line meant it: a path with a directory part
/// (`target/release/ts-r-check`) is relative to `cwd`, not to the project dir the command
/// runs in; a bare name is still looked up on PATH.
fn program_path(program: &str, cwd: &Path) -> PathBuf {
    let path = Path::new(program);
    if path.is_relative() && path.components().count() > 1 {
        cwd.join(path)
    } else {
        path.to_path_buf()
    }
}

fn normalize_path(p: &str) -> String {
    let p = p.replace('\\', "/");
    let p = p.strip_prefix("./").unwrap_or(&p);
    let p = p.trim_end_matches('/');
    if p.is_empty() {
        ".".into()
    } else {
        p.to_string()
    }
}

const CATEGORIES: &[&str] = &["error", "warning", "message", "suggestion"];

/// Splits `<cat> TS<code>: <message>` into its parts.
fn parse_head(s: &str) -> Option<(String, u32, String)> {
    let (cat, rest) = s.split_once(' ')?;
    if !CATEGORIES.contains(&cat) {
        return None;
    }
    let rest = rest.strip_prefix("TS")?;
    let (code, message) = rest.split_once(": ").or_else(|| rest.split_once(':'))?;
    Some((
        cat.to_string(),
        code.trim().parse().ok()?,
        message.to_string(),
    ))
}

/// Parses `tsc --pretty false` output into normalized diagnostics.
///
/// Lines look like `src/a.ts(3,7): error TS2322: Type ...` or `error TS5083: ...`; indented
/// lines that follow continue the previous message chain and are appended with `\n`.
pub fn parse_tsc_output(text: &str, project: &str) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(last) = out.last_mut() {
                last.message.push('\n');
                last.message.push_str(line.trim());
            }
            continue;
        }
        if let Some((category, code, message)) = parse_head(line) {
            out.push(Diagnostic {
                project: project.to_string(),
                file: String::new(),
                line: 0,
                col: 0,
                code,
                category,
                message,
            });
            continue;
        }
        // `<file>(<line>,<col>): <cat> TS<code>: <message>`
        let Some(idx) = CATEGORIES
            .iter()
            .filter_map(|c| line.find(&format!("): {c} TS")))
            .min()
        else {
            continue;
        };
        let (loc, rest) = (&line[..idx], &line[idx + 3..]);
        let Some(open) = loc.rfind('(') else { continue };
        let (file, pos) = (&loc[..open], &loc[open + 1..]);
        let mut nums = pos.split(',').map(|n| n.trim().parse::<u32>().ok());
        let (Some(Some(l)), Some(Some(c))) = (nums.next(), nums.next()) else {
            continue;
        };
        let Some((category, code, message)) = parse_head(rest) else {
            continue;
        };
        out.push(Diagnostic {
            project: project.to_string(),
            file: normalize_path(file),
            line: l,
            col: c,
            code,
            category,
            message,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tsc_pretty_false() {
        let text = "src/a.ts(3,7): error TS2322: Type 'string' is not assignable to type 'number'.\r\n\
                    src\\dir (1)\\b.ts(10,1): error TS2345: Argument of type '{ a: 1; }' is not assignable.\n  \
                    Object literal may only specify known properties.\n\
                    error TS5083: Cannot read file 'tsconfig.base.json'.\n\
                    Found 3 errors.\n";
        let d = parse_tsc_output(text, "fixtures/p");
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].file, "src/a.ts");
        assert_eq!((d[0].line, d[0].col, d[0].code), (3, 7, 2322));
        assert_eq!(d[1].file, "src/dir (1)/b.ts");
        assert_eq!(
            d[1].message,
            "Argument of type '{ a: 1; }' is not assignable.\nObject literal may only specify known properties."
        );
        assert_eq!(d[2].file, "");
        assert_eq!(d[2].code, 5083);
        assert_eq!(d[2].project, "fixtures/p");
    }

    #[test]
    fn custom_program_paths() {
        let cwd = Path::new("/work");
        assert_eq!(
            program_path("target/release/ts-r-check", cwd),
            Path::new("/work/target/release/ts-r-check")
        );
        assert_eq!(
            program_path("./ts-r-check", cwd),
            Path::new("/work/./ts-r-check")
        );
        assert_eq!(program_path("ts-r-check", cwd), Path::new("ts-r-check"));
        assert_eq!(program_path("/bin/tsc", cwd), Path::new("/bin/tsc"));
    }

    #[test]
    fn paths() {
        assert_eq!(normalize_path("./a\\b/"), "a/b");
        assert_eq!(normalize_path("."), ".");
        assert_eq!(normalize_path("./"), ".");
    }
}
