//! `ts-r-lint`: ESLint-compatible linter. See crates/ts_r_lint_cli/AGENTS.md.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use ts_r_lint::Severity;
use ts_r_lint::output::{Counts, Format, render};
use ts_r_lint::project::{LintOptions, lint_paths};

const HELP: &str = "\
ts-r-lint [options] [paths...]

Lints JavaScript and TypeScript files with ESLint-compatible rules and configs
(eslint.config.*, .oxlintrc.json, biome.json).

Options:
  --fix                         Apply fixes and write files
  -f, --format <name>           stylish (default), json, sarif, github
  -c, --config <path>           Use this config for every file
  --max-warnings <n>            Exit 1 when there are more than n warnings
  --quiet                       Report errors only (warn rules are not run)
  -o, --output-file <path>      Write the report to a file
  --threads <n>                 Worker threads (default: min(4, cores))
  --rule <name>                 Only run this rule (repeatable)
  --no-type-check               Do not run type-aware rules (no tsconfig program)
  --no-gitignore                Do not honour .gitignore files
  --no-warn-ignored             Do not warn about ignored files named explicitly
  --no-inline-config            Ignore eslint-disable and /* eslint */ comments
  --report-unused-disable-directives
                                Report unused eslint-disable comments as errors
  --report-unused-disable-directives-severity <sev>
                                off, warn or error
  --list-rules                  Print the built-in rules and exit
  -h, --help                    Show this help
  -v, --version                 Show the version
";

struct Args {
    opts: LintOptions,
    format: Format,
    max_warnings: Option<usize>,
    quiet: bool,
    output_file: Option<PathBuf>,
}

fn parse_args() -> Result<Option<Args>, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let mut opts = LintOptions::new(cwd);
    // Type-aware rules run on our checker, per tsconfig project (see ts_r_lint::typed).
    opts.resolve.types_available = true;
    let mut format = Format::Stylish;
    let mut max_warnings = None;
    let mut quiet = false;
    let mut output_file = None;
    let mut only: Vec<String> = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_owned(), Some(v.to_owned())),
            _ => (a.clone(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            match inline.clone() {
                Some(v) => Ok(v),
                None => it.next().ok_or_else(|| format!("{name} needs a value")),
            }
        };
        match flag.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(None);
            }
            "-v" | "--version" => {
                println!("ts-r-lint {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--list-rules" => {
                for d in ts_r_lint::Registry::global().all() {
                    println!("{}", d.meta.name);
                }
                return Ok(None);
            }
            "--fix" => opts.fix = true,
            "-f" | "--format" => {
                let v = value("--format")?;
                format = Format::parse(&v).ok_or_else(|| format!("unknown format '{v}'"))?;
            }
            "-c" | "--config" => opts.config = Some(PathBuf::from(value("--config")?)),
            "--max-warnings" => {
                let v = value("--max-warnings")?;
                let n: i64 = v.parse().map_err(|_| format!("bad --max-warnings '{v}'"))?;
                max_warnings = (n >= 0).then_some(n as usize);
            }
            "--quiet" => quiet = true,
            "-o" | "--output-file" => output_file = Some(PathBuf::from(value("--output-file")?)),
            "--threads" => {
                let v = value("--threads")?;
                opts.threads = v.parse().map_err(|_| format!("bad --threads '{v}'"))?;
            }
            "--rule" => only.push(value("--rule")?),
            "--no-gitignore" => opts.gitignore = false,
            "--no-type-check" => opts.resolve.types_available = false,
            "--no-warn-ignored" => opts.warn_ignored = false,
            "--no-inline-config" => opts.resolve.no_inline_config = true,
            "--report-unused-disable-directives" => {
                opts.resolve.report_unused_override = Some(Severity::Error)
            }
            "--report-unused-disable-directives-severity" => {
                let v = value("--report-unused-disable-directives-severity")?;
                opts.resolve.report_unused_override =
                    Some(Severity::parse(&v).ok_or_else(|| format!("bad severity '{v}'"))?);
            }
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option '{s}'")),
            _ => opts.paths.push(PathBuf::from(a)),
        }
    }
    opts.resolve.quiet = quiet;
    if !only.is_empty() {
        opts.resolve.only_rules = Some(only);
    }
    Ok(Some(Args {
        opts,
        format,
        max_warnings,
        quiet,
        output_file,
    }))
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Some(a)) => a,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ts-r-lint: {e}\n\n{HELP}");
            return ExitCode::from(2);
        }
    };
    let mut run = match lint_paths(&args.opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ts-r-lint: {e}");
            return ExitCode::from(2);
        }
    };
    if args.quiet {
        for r in &mut run.reports {
            r.result.diagnostics.retain(|d| d.is_error());
        }
    }
    let mut total = Counts::default();
    for r in &run.reports {
        total.add(Counts::of(&r.result.diagnostics));
    }
    let text = render(
        args.format,
        &run.reports,
        &args.opts.cwd,
        ts_r_lint::Registry::global(),
    );
    match &args.output_file {
        Some(p) => {
            if let Err(e) = std::fs::write(p, &text) {
                eprintln!("ts-r-lint: cannot write {}: {e}", p.display());
                return ExitCode::from(2);
            }
        }
        None => {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(text.as_bytes());
            if args.format != Format::Stylish && !text.ends_with('\n') {
                let _ = out.write_all(b"\n");
            }
        }
    }
    if !run.unsupported.is_empty() {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(
            err,
            "ts-r-lint: {} configured rule(s) are not run:",
            run.unsupported.len()
        );
        for u in &run.unsupported {
            let _ = writeln!(err, "  {}  {} ({})", u.config, u.rule, u.reason.describe());
        }
    }
    for n in &run.notes {
        eprintln!("ts-r-lint: note: {n}");
    }
    let too_many = args.max_warnings.is_some_and(|m| total.warnings > m);
    if too_many {
        eprintln!(
            "ts-r-lint found too many warnings (maximum: {}).",
            args.max_warnings.unwrap_or(0)
        );
    }
    if total.errors > 0 || too_many {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
