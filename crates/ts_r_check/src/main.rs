//! `ts-r-check [-p tsconfig.json] [tsc flags] [--threads N] [--no-history]
//!  [--sound=off|strict|sound|contracts] [--sound-config ts-r.toml|tsrconfig.json] [--sound-report[=json]]`
//!
//! Type-checks a project and prints diagnostics in tsc's format, then runs sound mode
//! (`ts_r_sound`, design record 0007) over the files it covers. The command line is tsc
//! 7.0.2's (`ts_r_config::cli`: every `tsc --all` flag, tsc's parse errors); the flags after
//! the program name that are ts-r's own (`--threads`, `--no-history`, `--sound*`) are taken
//! out first. Unless `noEmit` is set the program is emitted (`ts_r_emit`, design record 0012
//! E1): JavaScript and source maps are produced in memory, then written unless
//! `noEmitOnError` and errors say otherwise; `--listEmittedFiles` lists them. The exit codes
//! are tsc's: 0 without errors, 2 with errors when every output was written, 1 with errors
//! when some output was skipped (`noEmit`, `noEmitOnError`, TS5055, or a file ts-r cannot emit
//! yet: TSR2001/TSR2002/TSR2003/TSR2006). A flag whose behaviour is not implemented and would
//! change what tsc prints or writes would be a TSR2004 error, never silently ignored (none is
//! left: `--traceResolution` prints every resolution step first, `trace`);
//! `--watch` (`watch`, E4) rebuilds `-p` and `-b` projects as their files change;
//! profiling flags and a non-English `--locale` are a TSR2005 warning. The check history goes
//! to the ts-r cache (`ts_r_history::persist`): a run with no change prints the stored result
//! without parsing, a run after an edit re-checks only the affected files. `--no-history`
//! checks everything and leaves the cache alone. History is skipped when sound mode may apply
//! (it needs the loaded program and history does not save its results) and when the program
//! is emitted (emit needs the loaded program). With a `rootDir`, history is used when every
//! file it reports that could be emitted lies under it (no TS6059 is possible); otherwise the
//! program is loaded for the TS6059 check. `TS_R_CHECK_STATS=1` prints what the history did
//! to stderr.

mod build;
mod explain;
mod init;
mod trace;
mod watch;

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use ts_r_checker::{LibStore, ProgramFiles};
use ts_r_config::cli::{self, CommandLine, FLAGS, FlagTarget, TSC_VERSION};
use ts_r_config::options::flags as option_flags;
use ts_r_config::{
    ConfigLoader, FileSystem, OptionId, OptionValue, RealFs, find_config_file, resolve_project_arg,
    show_config,
};
use ts_r_diagnostics::{Category, Diagnostic, FileId};
use ts_r_diagnostics::{
    Files, Renderer, SOUND_CODE_BASE, code_label, format_plain, format_pretty, format_summary,
};
use ts_r_sound::config::{self, Level, SoundConfig};
use ts_r_sound::{SoundRun, check_sound, report};
use ts_r_span::SourceText;

struct Args {
    /// tsc's command line (everything but ts-r's own flags).
    cl: CommandLine,
    /// `--threads N`, else `--checkers N`, else 1 with `--singleThreaded`, else 4; at most 4.
    threads: usize,
    history: bool,
    /// `--sound=<level>`: every project file at this level (`off` turns sound mode off).
    sound: Option<Level>,
    sound_config: Option<String>,
    /// `--sound-report` (text) or `--sound-report=json`.
    sound_report: Option<bool>,
}

/// ts-r's own flags, listed by `--help` and `--all` after tsc's.
const TS_R_FLAGS: &[(&str, &str)] = &[
    ("--threads N", "Check with N threads (1 to 4, default 4)."),
    (
        "--no-history",
        "Check everything; neither read nor write the check history.",
    ),
    (
        "--sound=off|strict|sound|contracts",
        "Put every project file at this sound-mode level (off: no sound mode).",
    ),
    (
        "--sound-config FILE",
        "Sound-mode configuration (ts-r.toml, tsrconfig.json or a tsconfig with a \"ts-r\" key).",
    ),
    (
        "--sound-report[=json]",
        "Print the sound-mode coverage report.",
    ),
];

/// TSR codes (`SOUND_CODE_BASE + n`, printed `TSRnnnn`; design record 0012).
const TSR_NOT_IMPLEMENTED: u32 = SOUND_CODE_BASE + 2004;
const TSR_IGNORED: u32 = SOUND_CODE_BASE + 2005;

/// Flags ts-r-check accepts but cannot honour yet, with the phase of record 0012 that brings
/// them. They change what tsc prints or writes, so each is a TSR2004 error.
const NOT_IMPLEMENTED: &[(OptionId, &str)] = &[];

/// Profiling flags: accepted, ignored, one TSR2005 warning each.
const IGNORED: &[OptionId] = &[
    OptionId::GenerateTrace,
    OptionId::GenerateCpuProfile,
    OptionId::PprofDir,
];

fn parse_args(raw: Vec<String>, cwd: &str, fs: &RealFs) -> Result<Args, String> {
    let mut threads = None;
    let mut history = true;
    let mut sound = None;
    let mut sound_config = None;
    let mut sound_report = None;
    let mut rest = Vec::with_capacity(raw.len());
    let mut it = raw.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--threads" => {
                let n = it.next().ok_or("Option '--threads' expects an argument.")?;
                threads = Some(
                    n.parse::<usize>()
                        .map_err(|_| format!("Invalid thread count '{n}'."))?,
                );
            }
            "--no-history" => history = false,
            "--sound-config" => {
                sound_config = Some(
                    it.next()
                        .ok_or("Option '--sound-config' expects an argument.")?,
                )
            }
            "--sound-report" => sound_report = Some(false),
            "--sound-report=json" => sound_report = Some(true),
            s if s.starts_with("--sound=") => {
                let l = &s["--sound=".len()..];
                sound = Some(Level::parse(l).ok_or_else(|| {
                    format!("Invalid sound level '{l}': expected off, strict, sound or contracts.")
                })?);
            }
            _ => rest.push(arg),
        }
    }
    let cl = cli::parse_command_line(&rest, cwd, fs);
    let threads = threads
        .or_else(|| match cl.options.get(OptionId::Checkers) {
            Some(OptionValue::Number(n)) => Some(*n as usize),
            _ => None,
        })
        .unwrap_or(if cl.flag(OptionId::SingleThreaded) {
            1
        } else {
            4
        })
        .clamp(1, 4);
    Ok(Args {
        cl,
        threads,
        history,
        sound,
        sound_config,
        sound_report,
    })
}

/// `-v`: tsc's line (scripts match `Version x.y.z`), then the ts-r version.
fn version_line() -> String {
    format!("Version {TSC_VERSION} (ts-r {})", env!("CARGO_PKG_VERSION"))
}

/// `--help` (common flags) or `--all` (every flag, in `tsc --all` order and sections).
fn help_text(all: bool) -> String {
    let mut out = format!(
        "ts-r-check: The TypeScript checker - {}\n\n",
        version_line()
    );
    out.push_str("usage: ts-r-check [-p tsconfig.json] [options]\n");
    out.push_str(
        "Accepts tsc 7's command line. Writes JavaScript, source maps and declarations (syntax\nts-r cannot lower for the target yet: TSR2003); --noEmit only checks.\n",
    );
    if !all {
        out.push('\n');
    }
    const COMMON: &[&str] = &[
        "help",
        "watch",
        "all",
        "version",
        "init",
        "project",
        "showConfig",
        "build",
        "pretty",
        "noEmit",
        "target",
        "module",
        "lib",
        "strict",
        "quiet",
        "checkers",
    ];
    let mut section = None;
    for f in FLAGS {
        if !all && !COMMON.contains(&f.name) {
            continue;
        }
        if all && section != Some(f.section) {
            section = Some(f.section);
            out.push_str(&format!("\n{}:\n", f.section.title()));
        }
        let mut names = format!("--{}", f.name);
        for a in f.aliases {
            names.push_str(&format!(", -{a}"));
        }
        let note = match f.target {
            FlagTarget::Build(_) => " (with --build)",
            FlagTarget::Compiler(id) if NOT_IMPLEMENTED.iter().any(|(n, _)| *n == id) => {
                " (not implemented yet: TSR2004)"
            }
            FlagTarget::Compiler(id) if IGNORED.contains(&id) => " (ignored: TSR2005)",
            _ => "",
        };
        out.push_str(&format!("  {names}{note}\n"));
    }
    out.push_str("\nts-r:\n");
    for (name, what) in TS_R_FLAGS {
        out.push_str(&format!("  {name}  {what}\n"));
    }
    if !all {
        out.push_str("\nRun ts-r-check --all for every flag.\n");
    }
    out
}

/// A diagnostic without a file (command-line errors, TSR20xx).
fn global(code: u32, category: Category, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        code,
        category,
        message: message.into(),
        file: FileId(u32::MAX),
        span: ts_r_span::Span::EMPTY,
        related: Vec::new(),
    }
}

/// Prints diagnostics without a location, as tsc prints command-line errors (no summary).
fn print_global(out: &mut (impl Write + ?Sized), diags: &[Diagnostic], pretty: bool) {
    let none = Shown {
        sources: Vec::new(),
    };
    for d in diags {
        if pretty {
            let mut text = format_pretty(d, &none, Renderer { color: true });
            if text.ends_with("\n\n") {
                text.pop();
            }
            let _ = write!(out, "{text}");
        } else {
            let _ = writeln!(out, "{}", format_plain(d, &none));
        }
    }
}

/// TSR2004 errors and TSR2005 warnings for flags ts-r-check cannot honour.
fn unsupported_flags(cl: &CommandLine) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for &(id, phase) in NOT_IMPLEMENTED {
        if cl.flag(id) {
            out.push(global(
                TSR_NOT_IMPLEMENTED,
                Category::Error,
                format!(
                    "'--{}' is not implemented yet (design record 0012, {phase}).",
                    id.decl().name
                ),
            ));
        }
    }
    for &id in IGNORED {
        if cl.options.get(id).is_some() {
            out.push(global(
                TSR_IGNORED,
                Category::Warning,
                format!(
                    "'--{}' is accepted and ignored: ts-r does not write profiles.",
                    id.decl().name
                ),
            ));
        }
    }
    if let Some(OptionValue::String(locale)) = cl.options.get(OptionId::Locale) {
        let lang = locale.split(['-', '_']).next().unwrap_or("");
        if !lang.eq_ignore_ascii_case("en") {
            out.push(global(
                TSR_IGNORED,
                Category::Warning,
                format!(
                    "'--locale {locale}' is accepted and ignored: ts-r prints English messages."
                ),
            ));
        }
    }
    out
}

/// Merges the command-line options into the loaded config's, as tsc does, so option checks
/// (TS5096, TS5055, ...), `--showConfig` and the history key see them. Reporting-only flags
/// (`--pretty`, `--checkers`, ...: hidden, no `affects` class) stay out of the options.
fn merge_command_line(cl: &CommandLine, options: &mut ts_r_config::CompilerOptions) {
    let mut merged = cl.clone();
    merged.given.retain(|id| {
        let d = id.decl();
        d.affects != 0 || d.flags & (option_flags::HIDDEN | option_flags::CMDLINE_ONLY) == 0
    });
    merged.apply_to(options);
}

/// The tsconfig to load; `None`: the files named on the command line are the program.
/// `Err((diagnostic, exit code))` when there is none: no diagnostic means "print the help"
/// (tsc without a project and without arguments).
fn project_config(
    cl: &CommandLine,
    fs: &RealFs,
    cwd: &str,
) -> Result<Option<String>, (Option<Diagnostic>, u8)> {
    let err = |code: u32, message: String| Err((Some(global(code, Category::Error, message)), 1));
    let project = match cl.options.get(OptionId::Project) {
        Some(OptionValue::String(p)) => Some(p.clone()),
        _ => None,
    };
    if !cl.file_names.is_empty() {
        if project.is_some() {
            return err(
                5042,
                "Option 'project' cannot be mixed with source files on a command line.".into(),
            );
        }
        if !cl.flag(OptionId::IgnoreConfig) && find_config_file(fs, cwd, "tsconfig.json").is_some()
        {
            return err(
                5112,
                "tsconfig.json is present but will not be loaded if files are specified on commandline. Use '--ignoreConfig' to skip this error.".into(),
            );
        }
        return Ok(None);
    }
    let Some(p) = project else {
        // No project: `--showConfig` reports it (TS5081, observed), anything else prints the help.
        return match find_config_file(fs, cwd, "tsconfig.json") {
            Some(c) => Ok(Some(c)),
            None if cl.flag(OptionId::ShowConfig) => err(
                5081,
                format!("Cannot find a tsconfig.json file at the current directory: {cwd}."),
            ),
            None => Err((None, 1)),
        };
    };
    let abs = ts_r_config::path::resolve(cwd, &ts_r_config::path::normalize_slashes(&p));
    match resolve_project_arg(fs, &abs) {
        Some(c) => Ok(Some(c)),
        None if fs.directory_exists(&abs) => err(
            5081,
            format!(
                "Cannot find a tsconfig.json file at the current directory: {}.",
                ts_r_config::path::combine(&abs, "tsconfig.json")
            ),
        ),
        None => err(5058, format!("The specified path does not exist: '{abs}'.")),
    }
}

/// Files of the program, by id, shown relative to the working directory.
struct Shown {
    sources: Vec<Option<SourceText>>,
}

impl Files for Shown {
    fn source(&self, file: FileId) -> Option<&SourceText> {
        self.sources.get(file.0 as usize).and_then(|s| s.as_ref())
    }
}

fn relative(path: &str, cwd: &str) -> String {
    let cwd = cwd.trim_end_matches('/');
    if let Some(rest) = path.strip_prefix(cwd)
        && let Some(rest) = rest.strip_prefix('/')
    {
        return rest.to_owned();
    }
    // `../` segments for siblings.
    let p: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let c: Vec<&str> = cwd.split('/').filter(|s| !s.is_empty()).collect();
    let common = p.iter().zip(&c).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return path.to_owned();
    }
    let mut out: Vec<&str> = vec![".."; c.len() - common];
    out.extend(&p[common..]);
    out.join("/")
}

/// The sound configuration: `--sound-config`, else `ts-r.toml` / `tsrconfig.json` / the tsconfig `"ts-r"` key, else
/// none (pragmas alone). `--sound=<level>` then puts every project file at that level.
/// The flag is true when a configuration was given or found (or `--sound` set a level).
fn load_sound_config(
    fs: &RealFs,
    args: &Args,
    tsconfig: &str,
    cwd: &str,
) -> (SoundConfig, Vec<config::ConfigError>, bool) {
    let (cfg, errors) = match &args.sound_config {
        Some(p) => {
            let abs = if p.starts_with('/') {
                p.clone()
            } else {
                format!("{cwd}/{p}")
            };
            match fs.read_file(&abs) {
                Some(text) => config::parse_file(&text, &abs),
                None => (
                    None,
                    vec![config::ConfigError {
                        file: abs.clone(),
                        message: format!("cannot read '{p}'"),
                    }],
                ),
            }
        }
        None => config::discover(fs, tsconfig),
    };
    let configured = cfg.is_some() || !errors.is_empty() || args.sound.is_some();
    let base_dir = tsconfig.rsplit_once('/').map_or("", |(d, _)| d);
    let mut cfg = cfg.unwrap_or_else(|| SoundConfig {
        base_dir: base_dir.to_owned(),
        ..SoundConfig::default()
    });
    if let Some(level) = args.sound {
        cfg.level = level;
        cfg.include = vec!["**/*".to_owned()];
        cfg.exclude.clear();
    }
    (cfg, errors, configured)
}

/// Whether some file in `files` (non-lib files with their external-library flag) could be
/// emitted and lies outside `root`: the files TS6059 reports on. External libraries and
/// declaration files are never emitted; anything else outside `root` counts, so the check
/// errs towards loading the program.
fn outside_root_dir(root: &str, files: &[(String, bool)], case_sensitive: bool) -> bool {
    files.iter().any(|(path, external)| {
        !external
            && !is_declaration_name(path)
            && !ts_r_config::path::contains(root, path, case_sensitive)
    })
}

/// `x.d.ts`, `x.d.mts`, `x.d.cts` and arbitrary-extension declarations (`x.d.css.ts`).
fn is_declaration_name(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".d.ts")
        || lower.ends_with(".d.mts")
        || lower.ends_with(".d.cts")
        || (lower.ends_with(".ts") && lower.rsplit('/').next().is_some_and(|n| n.contains(".d.")))
}

/// Per file id: `(path, text)` for non-lib files.
fn sources_of(program: &ProgramFiles) -> Vec<Option<(String, String)>> {
    program
        .files
        .iter()
        .map(|f| (!f.is_lib).then(|| (f.path.clone(), f.text.clone())))
        .collect()
}

/// Adds the config diagnostics to the program's, in tsc's order: by file path, with
/// diagnostics without a file first. Program diagnostics keep their order; config files get
/// new ids at the end of `sources` (text read from `fs`), so they render with a location.
fn merge_config_diagnostics(
    diags: Vec<Diagnostic>,
    config: &[ts_r_config::ConfigDiagnostic],
    sources: &mut Vec<Option<(String, String)>>,
    fs: &dyn FileSystem,
) -> Vec<Diagnostic> {
    if config.is_empty() {
        return diags;
    }
    let path_of = |sources: &[Option<(String, String)>], d: &Diagnostic| -> String {
        sources
            .get(d.file.0 as usize)
            .and_then(|s| s.as_ref())
            .map_or(String::new(), |(p, _)| p.clone())
    };
    let mut keyed: Vec<(String, Diagnostic)> = diags
        .into_iter()
        .map(|d| (path_of(sources, &d), d))
        .collect();
    for c in config {
        let file = match &c.file {
            Some(f) => {
                match sources
                    .iter()
                    .position(|s| s.as_ref().is_some_and(|(p, _)| p == f))
                {
                    Some(i) => {
                        // History runs leave the text of files without diagnostics empty.
                        if let Some((_, text)) = sources[i].as_mut().filter(|(_, t)| t.is_empty()) {
                            *text = fs.read_file(f).unwrap_or_default();
                        }
                        FileId(i as u32)
                    }
                    None => {
                        sources.push(Some((f.clone(), fs.read_file(f).unwrap_or_default())));
                        FileId((sources.len() - 1) as u32)
                    }
                }
            }
            None => FileId(u32::MAX),
        };
        let mut message = c.message.clone();
        for r in &c.related {
            message.push_str("\n  ");
            message.push_str(r);
        }
        let d = Diagnostic {
            code: c.code,
            category: match c.severity {
                ts_r_config::Severity::Error => ts_r_diagnostics::Category::Error,
                ts_r_config::Severity::Warning => ts_r_diagnostics::Category::Warning,
            },
            message,
            file,
            span: c
                .span
                .map_or(ts_r_span::Span::EMPTY, |(s, e)| ts_r_span::Span::new(s, e)),
            related: Vec::new(),
        };
        let key = c.file.clone().unwrap_or_default();
        // After every diagnostic of an earlier or the same file.
        let at = keyed.partition_point(|(p, _)| *p <= key);
        keyed.insert(at, (key, d));
    }
    keyed.into_iter().map(|(_, d)| d).collect()
}

/// tsc's `--pretty` closing summary: `Found 1 error in a.ts:3`, `Found 2 errors in the same
/// file, starting at: a.ts:3`, or `Found 7 errors in 2 files.` followed by an `Errors  Files`
/// table (first error line per file). Diagnostics without a file count as errors only.
fn pretty_summary(
    diags: &[Diagnostic],
    shown: &Shown,
    error_count: usize,
    sound_count: usize,
) -> String {
    const GREY: &str = "\x1b[90m";
    const RESET: &str = "\x1b[0m";
    // (file, first error line, errors) in output order.
    let mut files: Vec<(FileId, u32, usize)> = Vec::new();
    for d in diags.iter().filter(|d| d.is_error()) {
        let Some(src) = shown.source(d.file) else {
            continue;
        };
        match files.iter_mut().find(|(f, _, _)| *f == d.file) {
            Some(entry) => entry.2 += 1,
            None => files.push((d.file, src.line_index().line_of(d.span.start) + 1, 1)),
        }
    }
    let name = |f: FileId| shown.source(f).map_or("", |s| s.name());
    let first = files
        .first()
        .map(|&(f, line, _)| format!("{}{GREY}:{line}{RESET}", name(f)));
    let head = match (error_count, files.len(), &first) {
        (1, _, Some(at)) => format!("Found 1 error in {at}"),
        (n, 1, Some(at)) => format!("Found {n} errors in the same file, starting at: {at}"),
        _ => format_summary(error_count, files.len(), None),
    };
    let mut out = format!("{}\n", with_sound_count(&head, sound_count));
    if files.len() > 1 {
        let digits = files
            .iter()
            .map(|(_, _, n)| n.to_string().len())
            .max()
            .unwrap_or(1);
        let width = digits.max("Errors".len());
        out.push('\n');
        out.push_str(&" ".repeat(width - "Errors".len()));
        out.push_str("Errors  Files\n");
        // The table is sorted by the names shown (observed in every tsc 7 baseline).
        let mut rows = files.clone();
        rows.sort_by(|a, b| name(a.0).cmp(name(b.0)));
        for &(f, line, n) in &rows {
            out.push_str(&format!("{n:>width$}  {}{GREY}:{line}{RESET}\n", name(f)));
        }
    }
    // tsc ends the summary with a blank line.
    out.push('\n');
    out
}

/// `Found 7 errors in 2 files.` becomes `Found 7 errors (3 sound-mode) in 2 files.`
fn with_sound_count(summary: &str, sound: usize) -> String {
    if sound == 0 {
        return summary.to_owned();
    }
    let Some(i) = summary.find(" error") else {
        return summary.to_owned();
    };
    let mut end = i + " error".len();
    if summary[end..].starts_with('s') {
        end += 1;
    }
    format!(
        "{} ({sound} sound-mode){}",
        &summary[..end],
        &summary[end..]
    )
}

/// The working directory as tsc 7 sees it: `$PWD` when it names the current directory (so a
/// path through a symbolic link, such as macOS's `/tmp`, is kept as typed), else the
/// resolved current directory.
fn working_directory() -> String {
    let current = std::env::current_dir().ok();
    if let (Some(pwd), Some(cur)) = (std::env::var_os("PWD"), &current) {
        let pwd = std::path::PathBuf::from(pwd);
        if pwd.is_absolute() && std::fs::canonicalize(&pwd).ok() == std::fs::canonicalize(cur).ok()
        {
            return pwd.to_string_lossy().replace('\\', "/");
        }
    }
    current
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
}

/// Pretty output without `--pretty`, as tsc 7 decides it: `NO_COLOR` (set, any value) turns it
/// off, `FORCE_COLOR` (set, any value, even `0`) turns it on, else a terminal on stdout.
fn default_pretty() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    std::env::var_os("FORCE_COLOR").is_some() || std::io::stdout().is_terminal()
}

/// `--init` (before `--help`, like tsc): writes the starter tsconfig.json, or TS5054 when
/// one exists (exit status 0 either way).
fn run_init(
    cl: &CommandLine,
    fs: &RealFs,
    cwd: &str,
    pretty: bool,
    out: &mut impl Write,
) -> ExitCode {
    let code = match init::run(&cl.options, &cl.given, fs, cwd) {
        init::Outcome::Created(text) => {
            let _ = write!(out, "{text}");
            0
        }
        init::Outcome::Exists(path) => {
            let d = global(
                5054,
                Category::Error,
                format!("A 'tsconfig.json' file is already defined at: '{path}'."),
            );
            print_global(out, &[d], pretty);
            0
        }
        init::Outcome::WriteFailed(e) => {
            let d = global(5033, Category::Error, format!("Could not write file {e}."));
            print_global(out, &[d], pretty);
            1
        }
    };
    let _ = out.flush();
    ExitCode::from(code)
}

fn main() -> ExitCode {
    let fs = RealFs::new();
    let cwd = working_directory();
    let args = match parse_args(std::env::args().skip(1).collect(), &cwd, &fs) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let cl = &args.cl;
    let mut out = std::io::stdout().lock();
    let pretty = match cl.options.get(OptionId::Pretty) {
        Some(OptionValue::Bool(b)) => *b,
        _ => default_pretty(),
    };
    // Like tsc: command-line errors are printed and nothing else happens.
    if !cl.diagnostics.is_empty() {
        let diags: Vec<Diagnostic> = cl
            .diagnostics
            .iter()
            .map(|d| global(d.code, Category::Error, d.message.clone()))
            .collect();
        print_global(&mut out, &diags, pretty);
        let _ = out.flush();
        return ExitCode::from(1);
    }
    if cl.flag(OptionId::Version) {
        let _ = writeln!(out, "{}", version_line());
        return ExitCode::SUCCESS;
    }
    if cl.flag(OptionId::Init) && !cl.build {
        return run_init(cl, &fs, &cwd, pretty, &mut out);
    }
    if cl.flag(OptionId::Help) || cl.flag(OptionId::All) {
        let _ = write!(out, "{}", help_text(cl.flag(OptionId::All)));
        return ExitCode::SUCCESS;
    }
    let cli_diags = unsupported_flags(cl);
    let cli_failed = cli_diags.iter().any(Diagnostic::is_error);
    if cli_failed || !cl.flag(OptionId::Quiet) {
        print_global(&mut out, &cli_diags, pretty);
    }
    if cli_failed {
        let _ = out.flush();
        return ExitCode::from(1);
    }
    if cl.flag(OptionId::Watch) {
        return ExitCode::from(if cl.build {
            watch::run_build(&args, &cwd, pretty, &mut out)
        } else {
            watch::run_project(&args, &cwd, pretty, &mut out)
        });
    }
    if cl.build {
        return ExitCode::from(build::run(cl, &fs, &cwd, pretty, args.threads, &mut out));
    }
    let config_path = match project_config(cl, &fs, &cwd) {
        Ok(c) => c,
        Err((message, code)) => {
            match message {
                Some(d) => print_global(&mut out, &[d], pretty),
                None => {
                    let _ = write!(out, "{}", help_text(false));
                }
            }
            let _ = out.flush();
            return ExitCode::from(code);
        }
    };
    let parsed = match &config_path {
        Some(p) => {
            ConfigLoader::new(&fs).load_with_overrides(p, |options| merge_command_line(cl, options))
        }
        None => ConfigLoader::new(&fs)
            .command_line(&cl.file_names, |options| merge_command_line(cl, options)),
    };
    let config_path = parsed.config_path.clone();
    if cl.flag(OptionId::ShowConfig) {
        let _ = write!(out, "{}", show_config(&parsed));
        return ExitCode::SUCCESS;
    }
    let quiet = cl.flag(OptionId::Quiet);
    let report = explain::Flags::from_command_line(cl).with(&parsed.options);
    if cl.flag(OptionId::ListFilesOnly) {
        let program = ts_r_checker::load_program(
            &fs,
            &parsed.file_names,
            &parsed.options,
            LibStore::locate().as_ref(),
            args.threads,
        );
        if report.trace {
            let _ = write!(out, "{}", trace::text(&fs, &program, None));
        }
        let _ = write!(out, "{}", explain::list(&program));
        let _ = out.flush();
        return ExitCode::SUCCESS;
    }
    let started = std::time::Instant::now();
    let mut error_count = 0usize;
    // Config diagnostics: parse errors (unknown options, bad values) and program-level option
    // checks (TS5052, TS5069, TS5102, TS5055, ...). Like tsc, an option error means no
    // syntactic or semantic diagnostics are reported; parse errors do not stop the check.
    let mut config_diags = parsed.all_diagnostics();
    // Project references (design record 0012, E3): their sources resolve to their outputs.
    let mut ref_configs = Vec::new();
    let (mut refs_failed, mut clash) = (false, false);
    let (refs, project_refs) = if parsed.references.is_empty() {
        (None, false)
    } else {
        let mut load = |p: &str| ts_r_projects::references::load_config(&fs, p);
        let ref_diags = ts_r_projects::references::reference_diagnostics(&parsed, &mut load, false);
        clash = ts_r_projects::references::buildinfo_clash(&ref_diags);
        refs_failed = ref_diags
            .iter()
            .any(|d| matches!(d.severity, ts_r_config::Severity::Error));
        config_diags.extend(ref_diags);
        ts_r_config::tsconfig::sort_and_dedupe(&mut config_diags);
        ref_configs = ts_r_projects::references::referenced_configs(&parsed, &mut load);
        let map = ts_r_projects::references::project_references(&ref_configs, fs.case_sensitive());
        ((!map.is_empty()).then(|| std::sync::Arc::new(map)), true)
    };
    let incremental = ts_r_projects::buildinfo::is_incremental(&parsed.options);
    // `noCheck`: syntax errors only; `semantic`: semantic errors among the diagnostics.
    let no_check = parsed.options.no_check() == Some(true);
    let mut semantic = 0usize;
    // A reference in error is an option error too (observed).
    let options_failed = refs_failed
        || parsed
            .option_diagnostics()
            .into_iter()
            .chain(parsed.emit_overwrite_diagnostics())
            .any(|d| matches!(d.severity, ts_r_config::Severity::Error));
    let libs = LibStore::locate();
    // Sound configuration first: it decides whether history can be used.
    let (sound_cfg, sound_configured) = match args.sound {
        Some(Level::Off) => (None, false),
        _ => {
            let (cfg, errors, configured) = load_sound_config(&fs, &args, &config_path, &cwd);
            for e in &errors {
                let _ = writeln!(
                    out,
                    "{}(1,1): error TSR0020: Invalid sound-mode configuration: {}.",
                    relative(&e.file, &cwd),
                    e.message
                );
            }
            error_count += errors.len();
            (Some(cfg), configured)
        }
    };
    // Emit (and the rootDir check) need the loaded program. tsc emits even when an option is
    // in error; it then reports no syntactic or semantic diagnostics.
    let emitting = !parsed.options.effective().no_emit();
    // Emit does not depend on the check's results, and without `noEmitOnError` (or an
    // incremental build, which decides what to write from them) nothing the check finds can
    // stop the writes: then emitting and writing run next to the check.
    let overlap = emitting
        && !incremental
        && args.threads > 1
        && parsed.options.no_emit_on_error() != Some(true);
    // The `package.json` paths resolution looked for go into the buildinfo, as with `-b`
    // (a later `-b` then finds it unchanged and does not rewrite it).
    let full_check = |check: bool, overlap: bool| {
        let (program, package_jsons) = ts_r_checker::program::load_program_tracking_package_jsons(
            &fs,
            &parsed.file_names,
            &parsed.options,
            libs.as_ref(),
            args.threads,
            refs.clone(),
        );
        let checked = |program: &ProgramFiles| {
            if !check {
                return (Vec::new(), 0);
            }
            let (mut d, semantic) = build::check(program, args.threads, no_check);
            let composite = ts_r_projects::references::composite_diagnostics(program, &parsed);
            if !composite.is_empty() {
                d.extend(composite);
                build::sort_by_file(program, &mut d);
            }
            (d, semantic)
        };
        let ((diagnostics, semantic), early) = if overlap {
            std::thread::scope(|s| {
                let program = &program;
                let early = std::thread::Builder::new()
                    .stack_size(64 << 20)
                    .spawn_scoped(s, || {
                        let host = ts_r_checker::typenode::specifier_host_with_references(
                            &fs,
                            &parsed.options,
                            refs.clone(),
                        );
                        let cfg = emit_config(&parsed, &fs, &host, args.threads);
                        early_emit(
                            program,
                            &cfg,
                            &ref_configs,
                            fs.case_sensitive(),
                            args.threads,
                        )
                    })
                    .ok();
                let checked = checked(program);
                (checked, early.and_then(|h| h.join().ok()))
            })
        } else {
            (checked(&program), None)
        };
        (program, diagnostics, semantic, early, package_jsons)
    };
    let needs_program = emitting || incremental || project_refs || no_check
            // Option errors: the program's syntax errors win over them (see below).
            || options_failed
            || report.needs_program();
    // Sound mode needs the loaded program, and history does not save its results: with a
    // sound configuration, skip history; without one, use it unless a source mentions `@ts-r`
    // (a pragma may turn a file on), then check fully. A warm run never misses sound results.
    let mut early = None;
    let mut package_jsons = None;
    let (program, sources, mut diags) = if needs_program {
        let (program, mut diags, s, e, p) = full_check(!options_failed, overlap);
        early = e;
        package_jsons = Some(p);
        semantic = s;
        if options_failed {
            build::syntax_before_options(&parsed, &program, &mut diags, &mut config_diags);
        }
        let sources = sources_of(&program);
        (Some(program), sources, diags)
    } else if options_failed {
        (None, Vec::new(), Vec::new())
    } else if sound_configured {
        let (program, diags, s, _, p) = full_check(true, false);
        semantic = s;
        package_jsons = Some(p);
        let sources = sources_of(&program);
        (Some(program), sources, diags)
    } else {
        let t = std::time::Instant::now();
        let cache = if args.history {
            ts_r_history::persist::open_cache()
        } else {
            None
        };
        let outcome =
            ts_r_history::persist::check(&fs, cache.as_ref(), &parsed, libs.as_ref(), args.threads);
        if std::env::var_os("TS_R_CHECK_STATS").is_some() {
            eprintln!(
                "ts-r-check: {:?}, {} changed, {} checked, {} reused, {:.1} ms ({}; shapes {:.1}, plan {:.1}, link+check {:.1})",
                outcome.mode,
                outcome.stats.changed,
                outcome.stats.checked,
                outcome.stats.reused,
                t.elapsed().as_secs_f64() * 1000.0,
                outcome
                    .phases
                    .iter()
                    .map(|(n, ms)| format!("{n} {ms:.1}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                outcome.stats.ms[0],
                outcome.stats.ms[1],
                outcome.stats.ms[2],
            );
        }
        // History runs may skip reading unchanged files: their saved flag answers.
        let pragmas = sound_cfg.is_some() && outcome.pragma;
        // TS6059 needs the program (it points at the importing node): load it only when a
        // file history reports could be outside `rootDir`.
        let outside_root = parsed
            .options
            .root_dir()
            .is_some_and(|root| outside_root_dir(root, &outcome.files, fs.case_sensitive()));
        if pragmas || outside_root {
            let (program, diags, _, _, _) = full_check(true, false);
            let sources = sources_of(&program);
            (Some(program), sources, diags)
        } else {
            (None, outcome.sources, outcome.diagnostics)
        }
    };
    let mut sound_count = 0usize;
    let mut numeric_codes = false;
    if let (Some(cfg), Some(program)) = (&sound_cfg, &program) {
        numeric_codes = cfg.policy.numeric_codes;
        let sound = check_sound(&SoundRun {
            program,
            config: cfg,
            base: &diags,
            threads: args.threads,
            case_sensitive: fs.case_sensitive(),
        });
        sound_count = sound
            .diagnostics
            .iter()
            .filter(|d| d.is_error() && d.code >= SOUND_CODE_BASE)
            .count();
        if let Some(json) = args.sound_report {
            let show = |p: &str| relative(p, &cwd);
            let text = if json {
                report::json(&sound, &show)
            } else {
                report::text(&sound, &show)
            };
            let _ = writeln!(out, "{text}");
        }
        diags.extend(sound.diagnostics);
        let path_of = |d: &Diagnostic| {
            program
                .files
                .get(d.file.0 as usize)
                .map_or("", |f| f.path.as_str())
        };
        diags.sort_by(|a, b| {
            path_of(a)
                .cmp(path_of(b))
                .then(a.span.start.cmp(&b.span.start))
        });
    }
    let mut emitted = None;
    let mut early_writes = None;
    if let Some(program) = &program {
        let host = ts_r_checker::typenode::specifier_host_with_references(
            &fs,
            &parsed.options,
            refs.clone(),
        );
        let cfg = emit_config(&parsed, &fs, &host, args.threads);
        let (located, global) = ts_r_emit::root_dir_diagnostics(program, &cfg);
        if !located.is_empty() || !global.is_empty() {
            // A root file outside `rootDir` is an option-level error: tsc then reports no
            // syntactic or semantic diagnostics. One only an import brings in keeps them.
            let g = !global.is_empty();
            diags = build::rooted_program_diagnostics(program, &parsed, diags, located, g);
            config_diags.extend(global);
        }
        if emitting {
            let e = if let Some(early) = early.take() {
                let EarlyEmit {
                    result,
                    inputs,
                    writes,
                } = early;
                merge_emit_diagnostics(program, &result, &mut diags, &mut config_diags);
                early_writes = writes;
                (result, inputs)
            } else {
                let mut e = emit_program(program, &cfg, &mut diags, &mut config_diags);
                ts_r_projects::references::drop_referenced_outputs(
                    &mut e.0,
                    program,
                    &ref_configs,
                    fs.case_sensitive(),
                );
                e
            };
            emitted = Some(e);
        }
    }
    let mut sources = sources;
    let diags = merge_config_diagnostics(diags, &config_diags, &mut sources, &fs);
    let diags = build::add_missing_roots(&parsed, &fs, diags, &mut sources);
    let shown = Shown {
        sources: sources
            .into_iter()
            .map(|s| s.map(|(path, text)| SourceText::new(relative(&path, &cwd), text)))
            .collect(),
    };
    let numeric = |line: String, d: &Diagnostic| {
        if numeric_codes && d.code >= SOUND_CODE_BASE {
            line.replacen(&code_label(d.code), &format!("TS{}", d.code), 1)
        } else {
            line
        }
    };
    if report.trace
        && let Some(program) = &program
    {
        // `--traceResolution`: before the diagnostics.
        let _ = write!(out, "{}", trace::text(&fs, program, refs.clone()));
    }
    if quiet {
        // `--quiet`: no diagnostics; the exit code still reports them.
    } else if pretty {
        let r = Renderer { color: true };
        for d in &diags {
            let mut text = numeric(format_pretty(d, &shown, r), d);
            // tsc follows a located diagnostic (it has a code frame) with a blank line, and a
            // global one with none.
            if shown.source(d.file).is_none() && text.ends_with("\n\n") {
                text.pop();
            }
            let _ = write!(out, "{text}");
        }
    } else {
        for d in &diags {
            let _ = writeln!(out, "{}", numeric(format_plain(d, &shown), d));
        }
    }
    error_count += diags.iter().filter(|d| d.is_error()).count();
    // Outputs are written only now, when every diagnostic is known (noEmitOnError), unless
    // they were written next to the check (`early_writes`, when nothing could stop them).
    let mut skipped = true;
    if incremental && let Some(program) = &program {
        // `incremental`/`composite`: write what changed, and the buildinfo (E3).
        let blocked = error_count > 0 && parsed.options.no_emit_on_error() == Some(true);
        skipped = emitted.as_ref().is_none_or(|(r, _)| r.skipped) || blocked;
        let (written, failed) = build::write_project(
            &parsed,
            program,
            emitted.as_ref(),
            blocked,
            build::Reported {
                errors: error_count > semantic,
                semantic_errors: semantic > 0,
                no_check,
            },
            false,
            false,
            package_jsons.as_deref(),
            clash,
            None,
        );
        if !quiet {
            for d in &failed {
                let _ = writeln!(out, "{}", format_plain(d, &shown));
            }
        }
        error_count += failed.len();
        skipped |= !failed.is_empty();
        let list = cl.flag(OptionId::ListEmittedFiles)
            || parsed.options.list_emitted_files() == Some(true);
        if list && !quiet {
            let _ = write!(out, "{}", ts_r_emit::listing(&written));
        }
    } else if let Some((result, inputs)) = emitted {
        skipped = result.skipped;
        let blocked = error_count > 0 && parsed.options.no_emit_on_error() == Some(true);
        if blocked {
            skipped = true;
        } else {
            let (written, failed) = match early_writes.take() {
                Some(w) => w,
                None => write_emitted(&result, &inputs, args.threads),
            };
            if !quiet {
                for d in &failed {
                    let _ = writeln!(out, "{}", format_plain(d, &shown));
                }
            }
            error_count += failed.len();
            skipped |= !failed.is_empty();
            // A reporting flag: kept out of the merged options (see `merge_command_line`).
            let list = cl.flag(OptionId::ListEmittedFiles)
                || parsed.options.list_emitted_files() == Some(true);
            if list && !quiet {
                let _ = write!(out, "{}", ts_r_emit::listing(&written));
            }
        }
    }
    if let Some(program) = &program {
        if !quiet {
            let _ = write!(
                out,
                "{}",
                explain::files(program, &parsed, report, &fs, &cwd)
            );
        }
        if report.stats {
            explain::stats(program, started.elapsed());
        }
    }
    if pretty && error_count > 0 && !quiet {
        let _ = write!(
            out,
            "\n{}",
            pretty_summary(&diags, &shown, error_count, sound_count)
        );
    }
    let _ = out.flush();
    ExitCode::from(match (error_count, skipped) {
        (0, _) => 0,
        (_, true) => 1,
        (_, false) => 2,
    })
}

/// The emit configuration of `parsed`.
fn emit_config<'a>(
    parsed: &'a ts_r_config::ParsedConfig,
    fs: &RealFs,
    host: &'a dyn ts_r_checker::typenode::ModuleSpecifierHost,
    threads: usize,
) -> ts_r_emit::EmitConfig<'a> {
    ts_r_emit::EmitConfig {
        options: &parsed.options,
        // A synthetic config built from the command line is no emit configuration.
        config: (!parsed.command_line).then_some(parsed),
        threads,
        case_sensitive: fs.case_sensitive(),
        specifiers: Some(host),
    }
}

/// An emit made next to the check: the result (its diagnostics not merged yet, outputs of
/// referenced projects dropped), the protected inputs, and the writes (written, TS5033).
struct EarlyEmit {
    result: ts_r_emit::EmitResult,
    inputs: Vec<String>,
    writes: Option<(Vec<String>, Vec<Diagnostic>)>,
}

/// Emits `program` and writes the outputs (see `overlap` in `main`).
fn early_emit(
    program: &ProgramFiles,
    cfg: &ts_r_emit::EmitConfig,
    ref_configs: &[ts_r_config::ParsedConfig],
    case_sensitive: bool,
    threads: usize,
) -> EarlyEmit {
    let (mut result, inputs) = emit_raw(program, cfg);
    ts_r_projects::references::drop_referenced_outputs(
        &mut result,
        program,
        ref_configs,
        case_sensitive,
    );
    let writes = Some(write_emitted(&result, &inputs, threads));
    EarlyEmit {
        result,
        inputs,
        writes,
    }
}

/// Writes `result`'s outputs, never over one of `inputs`.
fn write_emitted(
    result: &ts_r_emit::EmitResult,
    inputs: &[String],
    threads: usize,
) -> (Vec<String>, Vec<Diagnostic>) {
    let mut writer =
        ts_r_emit::FsWriter::protecting(inputs.iter().map(String::as_str)).with_threads(threads);
    ts_r_emit::write_outputs(result, &mut writer)
}

/// Emits `program` into memory, with its own checker. Returns the result and the input paths
/// the writer must never overwrite.
fn emit_raw(
    program: &ProgramFiles,
    cfg: &ts_r_emit::EmitConfig,
) -> (ts_r_emit::EmitResult, Vec<String>) {
    let checker = ts_r_checker::Checker::new(program);
    let resolver = ts_r_emit::CheckerResolver::new(&checker, cfg.options);
    let result = ts_r_emit::emit(program, &resolver, cfg);
    let inputs = program
        .files
        .iter()
        .filter(|f| !f.is_lib)
        .map(|f| f.path.clone())
        .collect();
    (result, inputs)
}

/// Emits `program` into memory ([`emit_raw`]) and merges its diagnostics
/// ([`merge_emit_diagnostics`]).
fn emit_program(
    program: &ProgramFiles,
    cfg: &ts_r_emit::EmitConfig,
    diags: &mut Vec<Diagnostic>,
    config_diags: &mut Vec<ts_r_config::ConfigDiagnostic>,
) -> (ts_r_emit::EmitResult, Vec<String>) {
    let (result, inputs) = emit_raw(program, cfg);
    merge_emit_diagnostics(program, &result, diags, config_diags);
    (result, inputs)
}

/// Emit diagnostics join the others: located ones (TSR2002, TSR2003 at the construct) by
/// file, global ones with the option diagnostics. A TS5055 the config check already reported
/// is not repeated.
fn merge_emit_diagnostics(
    program: &ProgramFiles,
    result: &ts_r_emit::EmitResult,
    diags: &mut Vec<Diagnostic>,
    config_diags: &mut Vec<ts_r_config::ConfigDiagnostic>,
) {
    let mut located = false;
    // Copied, not taken: the build state reads which files they explain (`ts_r_projects`).
    for d in result.diagnostics.iter().cloned() {
        if d.file.0 == u32::MAX {
            if !config_diags
                .iter()
                .any(|c| c.code == d.code && c.message == d.message)
            {
                config_diags.push(ts_r_config::ConfigDiagnostic::global(d.code, d.message));
            }
        } else {
            located = true;
            diags.push(d);
        }
    }
    if located {
        let path_of = |d: &Diagnostic| {
            program
                .files
                .get(d.file.0 as usize)
                .map_or("", |f| f.path.as_str())
        };
        diags.sort_by(|a, b| {
            path_of(a)
                .cmp(path_of(b))
                .then(a.span.start.cmp(&b.span.start))
        });
    }
}
