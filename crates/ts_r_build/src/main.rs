//! `ts-r-build`: esbuild-compatible bundler CLI. Flags follow esbuild's spelling
//! (`--bundle --outdir=out --format=esm --define:K=V ...`); see `ts-r-build --help`.

mod args;
mod watch;

use std::io::Write as _;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use ts_r_bundler::{BuildContext, BuildResult, Message, Severity};
use ts_r_guard::vfs::GuardFs;

use crate::args::{Cli, LogLevel};

fn main() -> ExitCode {
    let started = Instant::now();
    let code = run();
    if std::env::var_os("TS_R_BUILD_TIMING").is_some() {
        eprintln!(
            "[timing] main {:.1}ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }
    code
}

fn run() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cli = match args::parse(&argv) {
        Ok(args::Parsed::Run(c)) => *c,
        Ok(args::Parsed::Help) => {
            print!("{}", args::HELP);
            return ExitCode::SUCCESS;
        }
        Ok(args::Parsed::Version) => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("ts-r-build: error: {e}");
            return ExitCode::from(1);
        }
    };
    if cli.watch {
        return ExitCode::from(watch::run(&cli) as u8);
    }
    let guard = guard_fs(&cli);
    let ctx = match BuildContext::new(cli.options.clone(), guard.clone()) {
        Ok(c) => c,
        Err(m) => {
            report(
                &cli,
                &BuildResult {
                    messages: m,
                    ..BuildResult::default()
                },
            );
            return ExitCode::from(1);
        }
    };
    let start = Instant::now();
    let mut r = ctx.build();
    ctx.finish_and_leak();
    add_guard_errors(&mut r, &guard, &cli.options.working_dir);
    let code = finish(&cli, &r, start);
    // The process exits next: freeing the outputs one by one would only cost time.
    std::mem::forget(r);
    code
}

/// The file system builds read through: `parse<T>` call sites are expanded as files are
/// read; bundles get one shared validator module per declaration (record 0007), unbundled
/// output inlines them per file. Types resolve with the bundler's tsconfig: `--tsconfig`, else
/// each file's nearest one.
pub(crate) fn guard_fs(cli: &Cli) -> Arc<GuardFs> {
    let wd = &cli.options.working_dir;
    let g = GuardFs::new(
        Arc::new(ts_r_config::RealFs::new()),
        wd,
        guard_options(cli),
        cli.options.bundle,
    );
    Arc::new(match &cli.options.tsconfig {
        Some(t) => g.with_tsconfig(&ts_r_config::path::resolve(
            wd,
            &ts_r_config::path::normalize_slashes(t),
        )),
        None => g,
    })
}

/// `[sound.guard]` from `ts-r.toml` or the `"ts-r"` key of the tsconfig (`--tsconfig`, else
/// the nearest one above the working directory). Problems are printed (TSR0020) and the
/// defaults used for the keys they concern.
fn guard_options(cli: &Cli) -> ts_r_guard::GuardOptions {
    use ts_r_config::path;
    let fs = ts_r_config::RealFs::new();
    let wd = &cli.options.working_dir;
    let tsconfig = match &cli.options.tsconfig {
        Some(t) => path::resolve(wd, &path::normalize_slashes(t)),
        None => ts_r_config::find_config_file(&fs, wd, "tsconfig.json")
            .unwrap_or_else(|| path::combine(wd, "tsconfig.json")),
    };
    let settings = ts_r_sound::config::guard_options(&fs, &tsconfig);
    for e in &settings.errors {
        eprintln!("{}", e.render());
    }
    settings.options
}

/// `parse<T>` errors (TSR1501, ...) of the build's files, as build errors.
pub(crate) fn add_guard_errors(r: &mut BuildResult, guard: &GuardFs, wd: &str) {
    let errors = guard.errors_in(&r.inputs);
    if !errors.is_empty() {
        // Like any build error: no output is left behind.
        for o in &r.outputs {
            let _ = std::fs::remove_file(&o.path);
        }
    }
    for f in errors {
        let text = std::fs::read_to_string(&f.path).unwrap_or_default();
        for e in f.errors {
            let mut m = Message::new(
                Severity::Error,
                Some(ts_r_config::path::relative(wd, &f.path, true)),
                format!("{}: {}", e.code_label(), e.message),
            );
            m.line = e.line;
            m.column = e.column.saturating_sub(1);
            m.line_text = text
                .lines()
                .nth(e.line.saturating_sub(1) as usize)
                .map(str::to_owned);
            r.messages.push(m);
        }
    }
}

/// Prints messages and the output summary; writes stdout output and the metafile.
pub(crate) fn finish(cli: &Cli, r: &BuildResult, start: Instant) -> ExitCode {
    report(cli, r);
    if r.has_errors() {
        return ExitCode::from(1);
    }
    if cli.stdout {
        let mut out = std::io::stdout().lock();
        for o in &r.outputs {
            if !o.path.ends_with(".map") {
                let _ = out.write_all(&o.contents);
            }
        }
        return ExitCode::SUCCESS;
    }
    if let (Some(path), Some(meta)) = (&cli.metafile, &r.metafile)
        && let Err(e) = std::fs::write(path, meta)
    {
        eprintln!("ts-r-build: error: cannot write metafile {path}: {e}");
        return ExitCode::from(1);
    }
    if cli.log_level >= LogLevel::Info {
        let wd = &cli.options.working_dir;
        let mut rows: Vec<(String, usize)> = r
            .outputs
            .iter()
            .map(|o| {
                (
                    ts_r_config::path::relative(wd, &o.path, true),
                    o.contents.len(),
                )
            })
            .collect();
        rows.sort();
        let w = rows.iter().map(|(p, _)| p.len()).max().unwrap_or(0);
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err);
        for (p, n) in rows {
            let _ = writeln!(err, "  {p:<w$}  {}", size(n));
        }
        let _ = writeln!(err, "\nDone in {}ms", start.elapsed().as_millis());
    }
    ExitCode::SUCCESS
}

fn report(cli: &Cli, r: &BuildResult) {
    let mut err = std::io::stderr().lock();
    for m in &r.messages {
        let show = match m.severity {
            Severity::Error => cli.log_level >= LogLevel::Error,
            Severity::Warning => cli.log_level >= LogLevel::Warning,
        };
        if show {
            let _ = writeln!(err, "{m}\n");
        }
    }
    let errors = r.errors().count();
    if errors > 0 && cli.log_level >= LogLevel::Error {
        let _ = writeln!(err, "{errors} error{}", if errors == 1 { "" } else { "s" });
    }
}

fn size(n: usize) -> String {
    if n < 1024 {
        format!("{n}b")
    } else if n < 1024 * 1024 {
        format!("{:.1}kb", n as f64 / 1024.0)
    } else {
        format!("{:.1}mb", n as f64 / (1024.0 * 1024.0))
    }
}
