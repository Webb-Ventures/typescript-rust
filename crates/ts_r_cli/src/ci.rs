//! `ts-r ci`: check, lint and build in parallel with prefixed output and a timing summary.

use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::resolve::{self, Env, Lookup};
use crate::tools::{self, CI_TOOLS};
use crate::version::Selected;

const USAGE: &str = "\
Usage: ts-r ci [--skip-missing]

Runs `ts-r check`, `ts-r lint` and `ts-r build` in parallel, prefixes every output line with
the tool's name, prints a timing summary, and fails if any of them fails.

Options:
  --skip-missing   Run the tools that are installed instead of failing when one is missing
  -h, --help       Print this help
";

/// ANSI colours for the prefixes, one per tool.
const COLORS: [&str; 3] = ["36", "35", "33"];

struct Job {
    name: &'static str,
    path: PathBuf,
}

struct Outcome {
    name: &'static str,
    result: io::Result<ExitStatus>,
    elapsed: Duration,
}

/// Entry point. Returns the process exit code.
pub fn run(args: &[OsString], selected: &Selected, env: &Env) -> i32 {
    let mut skip_missing = false;
    for a in args {
        match a.to_str() {
            Some("--skip-missing") => skip_missing = true,
            Some("-h" | "--help") => {
                print!("{USAGE}");
                return 0;
            }
            _ => {
                eprintln!("ts-r ci: unexpected argument {:?}\n\n{USAGE}", a);
                return 2;
            }
        }
    }

    let mut jobs = Vec::new();
    let mut missing = Vec::new();
    for name in CI_TOOLS {
        let tool = tools::find(name).expect("CI tools are in the tool table");
        match resolve::resolve(tool, selected, env) {
            Lookup::Found { path, .. } => jobs.push(Job {
                name: tool.name,
                path,
            }),
            Lookup::Missing { .. } => missing.push(tool),
        }
    }
    for tool in &missing {
        let what = if skip_missing {
            "skipping"
        } else {
            "can't run"
        };
        eprintln!(
            "ts-r ci: {what} {}: {} isn't installed; it ships in milestone {}",
            tool.name,
            tool.display_name(),
            tool.milestone
        );
    }
    if !missing.is_empty() && !skip_missing {
        eprintln!(
            "ts-r ci: run `ts-r doctor` to see where tools are looked for, or pass --skip-missing"
        );
        return 1;
    }
    if jobs.is_empty() {
        eprintln!(
            "ts-r ci: none of {} is installed; nothing ran",
            CI_TOOLS.join(", ")
        );
        return 1;
    }

    let color = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let width = CI_TOOLS.iter().map(|n| n.len()).max().unwrap_or(0);
    // Split the cores between the tools so they don't oversubscribe the machine.
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let threads = (cores / jobs.len()).max(1);

    let started = Instant::now();
    let outcomes: Vec<Outcome> = std::thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .iter()
            .enumerate()
            .map(|(i, job)| {
                let prefix = if color {
                    format!(
                        "\x1b[{}m{:<width$} |\x1b[0m ",
                        COLORS[i % COLORS.len()],
                        job.name
                    )
                } else {
                    format!("{:<width$} | ", job.name)
                };
                scope.spawn(move || run_job(job, &prefix, threads, color))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("ci job thread panicked"))
            .collect()
    });
    let wall = started.elapsed();
    summarize(
        &outcomes,
        &missing.iter().map(|t| t.name).collect::<Vec<_>>(),
        wall,
        width,
    )
}

fn run_job(job: &Job, prefix: &str, threads: usize, color: bool) -> Outcome {
    let start = Instant::now();
    let mut cmd = Command::new(&job.path);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.env("TS_R_THREADS", threads.to_string());
    if color {
        // Output goes through a pipe, so tools can't detect the terminal themselves.
        for var in ["FORCE_COLOR", "CLICOLOR_FORCE"] {
            if std::env::var_os(var).is_none() {
                cmd.env(var, "1");
            }
        }
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Outcome {
                name: job.name,
                result: Err(e),
                elapsed: start.elapsed(),
            };
        }
    };
    let out = child.stdout.take();
    let err = child.stderr.take();
    std::thread::scope(|s| {
        if let Some(out) = out {
            s.spawn(move || pump(out, prefix, false));
        }
        if let Some(err) = err {
            s.spawn(move || pump(err, prefix, true));
        }
        let result = child.wait();
        Outcome {
            name: job.name,
            result,
            elapsed: start.elapsed(),
        }
    })
}

/// Copies lines from a child pipe to our stdout or stderr, each with the prefix. Lines are
/// written whole under the stream lock so tools never interleave mid-line. Bytes are passed
/// through unchanged (no UTF-8 requirement).
fn pump(source: impl Read, prefix: &str, to_stderr: bool) {
    let mut reader = BufReader::new(source);
    let mut line = Vec::with_capacity(256);
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line.last() != Some(&b'\n') {
            line.push(b'\n');
        }
        let written = if to_stderr {
            let mut e = io::stderr().lock();
            e.write_all(prefix.as_bytes())
                .and_then(|()| e.write_all(&line))
        } else {
            let mut o = io::stdout().lock();
            o.write_all(prefix.as_bytes())
                .and_then(|()| o.write_all(&line))
                .and_then(|()| o.flush())
        };
        if written.is_err() {
            // Our output is closed (e.g. piped into `head`); keep draining so the tool
            // doesn't block on a full pipe.
            let _ = io::copy(&mut reader, &mut io::sink());
            return;
        }
    }
}

fn summarize(outcomes: &[Outcome], skipped: &[&str], wall: Duration, width: usize) -> i32 {
    let failed = outcomes
        .iter()
        .filter(|o| !matches!(&o.result, Ok(s) if s.success()))
        .count();
    let mut out = io::stdout().lock();
    let _ = writeln!(out);
    let verdict = if failed == 0 { "passed" } else { "FAILED" };
    let _ = writeln!(out, "ts-r ci {verdict} in {:.2}s", wall.as_secs_f64());
    for o in outcomes {
        let (status, detail) = match &o.result {
            Ok(s) if s.success() => ("ok", String::new()),
            Ok(s) => ("FAILED", describe_status(s)),
            Err(e) => ("FAILED", format!("could not start: {e}")),
        };
        let _ = writeln!(
            out,
            "  {:<width$}  {status:<6}  {:>7.2}s  {detail}",
            o.name,
            o.elapsed.as_secs_f64()
        );
    }
    for name in skipped {
        let _ = writeln!(out, "  {name:<width$}  skipped (not installed)");
    }
    i32::from(failed > 0)
}

fn describe_status(s: &ExitStatus) -> String {
    if let Some(code) = s.code() {
        return format!("(exit {code})");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = s.signal() {
            return format!("(killed by signal {sig})");
        }
    }
    "(terminated)".to_owned()
}
