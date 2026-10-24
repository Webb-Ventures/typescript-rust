//! `ts-r-check --build` (design record 0012, E3): the compile pipeline behind
//! [`ts_r_projects::build::Host`], and the incremental writes shared with `-p`.
//!
//! Build mode type-checks and emits each out-of-date project like `-p` does (no check
//! history: every build loads the program; no sound mode), with the project's references
//! redirected to their outputs, writes only what [`ts_r_projects::incremental::plan`] says,
//! and leaves a buildinfo. Diagnostics print per project as it builds; one pretty summary
//! closes the whole build.

use std::io::Write;
use std::sync::{Arc, Mutex};

use ts_r_checker::program::{SourceCache, load_program_cached_tracking};
use ts_r_checker::{CheckRun, LibStore, ProgramFiles, check_program};
use ts_r_config::cli::CommandLine;
use ts_r_config::{
    ConfigDiagnostic, ConfigLoader, FileSystem, OptionId, ParsedConfig, RealFs, Severity,
};
use ts_r_diagnostics::{Diagnostic, Files, Renderer, format_plain, format_pretty, format_summary};
use ts_r_emit::Writer;
use ts_r_projects::build::{self, Compiled, ErrorFile, Flags, Host, Job, Rendered};
use ts_r_projects::buildinfo::{self, BuildInfo, Read};
use ts_r_projects::{graph, incremental, references};
use ts_r_span::SourceText;

use crate::{Shown, emit_program, merge_command_line, merge_config_diagnostics, relative};

/// The pipeline and terminal for `--build` (and for `--watch`, which keeps one host for the
/// whole session).
pub struct BuildHost<'a> {
    /// Configs and program files are read through it (`--watch` records what was read).
    pub fs: &'a dyn FileSystem,
    pub cwd: String,
    pub pretty: bool,
    pub quiet: bool,
    pub threads: usize,
    pub libs: Option<LibStore>,
    /// `--listEmittedFiles` on the command line.
    pub list_emitted: bool,
    /// `--listFiles`, `--explainFiles`, `--diagnostics` on the command line (E4).
    pub report: crate::explain::Flags,
    /// Parsed files kept between compilations (`--watch`).
    pub cache: Option<SourceCache>,
    /// Paths written or touched (`--watch` takes them out after each compilation).
    pub written: Mutex<Vec<String>>,
}

impl<'a> BuildHost<'a> {
    pub fn new(
        fs: &'a dyn FileSystem,
        cwd: &str,
        pretty: bool,
        quiet: bool,
        threads: usize,
    ) -> Self {
        BuildHost {
            fs,
            cwd: cwd.to_owned(),
            pretty,
            quiet,
            threads,
            libs: LibStore::locate(),
            list_emitted: false,
            report: crate::explain::Flags::default(),
            cache: None,
            written: Mutex::new(Vec::new()),
        }
    }

    fn record_written(&self, paths: &[String]) {
        self.written
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(paths.iter().cloned());
    }
}

impl BuildHost<'_> {
    /// Renders `diags` (program files by id, config files appended to `sources`).
    fn render_diags(
        &self,
        diags: &[Diagnostic],
        sources: Vec<Option<(String, String)>>,
    ) -> Rendered {
        let shown = Shown {
            sources: sources
                .into_iter()
                .map(|s| s.map(|(path, text)| SourceText::new(relative(&path, &self.cwd), text)))
                .collect(),
        };
        let mut r = Rendered::default();
        for d in diags {
            if !self.quiet {
                if self.pretty {
                    let mut text = format_pretty(d, &shown, Renderer { color: true });
                    if shown.source(d.file).is_none() && text.ends_with("\n\n") {
                        text.pop();
                    }
                    r.text.push_str(&text);
                } else {
                    r.text.push_str(&format_plain(d, &shown));
                    r.text.push('\n');
                }
            }
            if !d.is_error() {
                continue;
            }
            r.errors += 1;
            if let Some(src) = shown.source(d.file) {
                let name = src.name().to_owned();
                match r.files.iter_mut().find(|f| f.name == name) {
                    Some(f) => f.count += 1,
                    None => r.files.push(ErrorFile {
                        name,
                        line: src.line_index().line_of(d.span.start) + 1,
                        count: 1,
                    }),
                }
            }
        }
        r
    }
}

impl Host for BuildHost<'_> {
    fn compile(&self, job: &Job) -> Compiled {
        compile(self, job.config, Some(job), true, None)
    }

    fn touched(&self, paths: &[String]) {
        self.record_written(paths);
    }

    fn render(&self, diags: &[ConfigDiagnostic]) -> Rendered {
        let mut sources = Vec::new();
        let merged = merge_config_diagnostics(Vec::new(), diags, &mut sources, self.fs);
        self.render_diags(&merged, sources)
    }

    fn pretty(&self) -> bool {
        self.pretty
    }

    fn show(&self, path: &str) -> String {
        relative(path, &self.cwd)
    }
}

/// Loads, checks and emits one project, writes what the incremental plan says, and renders
/// the diagnostics. `job` carries the references (`--build`); `build_mode` writes a
/// buildinfo for every project. `memory` (`--watch`): the program state of the previous
/// compilation in this session, used instead of the buildinfo on disk and updated.
pub fn compile(
    host: &BuildHost,
    parsed: &ParsedConfig,
    job: Option<&Job>,
    build_mode: bool,
    memory: Option<&mut Option<BuildInfo>>,
) -> Compiled {
    let started = std::time::Instant::now();
    let fs = host.fs;
    let mut config_diags = parsed.all_diagnostics();
    let mut load = |p: &str| references::load_config(fs, p);
    let ref_diags = references::reference_diagnostics(parsed, &mut load, build_mode);
    let clash = references::buildinfo_clash(&ref_diags);
    // A reference in error (TS6053, TS6306, TS6310) is an option error: no program
    // diagnostics are reported (observed).
    let refs_failed = ref_diags
        .iter()
        .any(|d| matches!(d.severity, Severity::Error))
        || parsed
            .references
            .iter()
            .any(|r| !fs.file_exists(&r.config_path));
    config_diags.extend(ref_diags);
    ts_r_config::tsconfig::sort_and_dedupe(&mut config_diags);
    let options_failed = refs_failed
        || parsed
            .option_diagnostics()
            .into_iter()
            .chain(parsed.emit_overwrite_diagnostics())
            .any(|d| matches!(d.severity, Severity::Error));
    let own_configs;
    let (refs, ref_configs) = match job {
        Some(j) => (j.references.clone(), j.reference_configs),
        None => {
            own_configs = references::referenced_configs(parsed, &mut load);
            let map = references::project_references(&own_configs, fs.case_sensitive());
            (Arc::new(map), own_configs.as_slice())
        }
    };
    let (program, package_jsons) = load_program_cached_tracking(
        &fs,
        &parsed.file_names,
        &parsed.options,
        host.libs.as_ref(),
        host.threads,
        host.cache.as_ref(),
        (!refs.is_empty()).then(|| refs.clone()),
    );
    let no_check = parsed.options.no_check() == Some(true);
    let (mut diags, semantic) = if options_failed {
        let mut d = Vec::new();
        syntax_before_options(parsed, &program, &mut d, &mut config_diags);
        (d, 0)
    } else {
        let (mut d, semantic) = check(&program, host.threads, no_check);
        let composite = references::composite_diagnostics(&program, parsed);
        if !composite.is_empty() {
            d.extend(composite);
            sort_by_file(&program, &mut d);
        }
        (d, semantic)
    };
    let emitting = !parsed.options.effective().no_emit();
    let specifiers = ts_r_checker::typenode::specifier_host_with_references(
        fs,
        &parsed.options,
        (!refs.is_empty()).then(|| refs.clone()),
    );
    let cfg = ts_r_emit::EmitConfig {
        options: &parsed.options,
        config: Some(parsed),
        threads: host.threads,
        case_sensitive: fs.case_sensitive(),
        specifiers: Some(&specifiers),
    };
    let (located, global) = ts_r_emit::root_dir_diagnostics(&program, &cfg);
    if !located.is_empty() || !global.is_empty() {
        // Option errors keep program diagnostics out, these too (observed with a missing
        // reference: `tsbuildWatch/sample/watches-config-files-that-are-not-present`).
        if !options_failed {
            diags = rooted_program_diagnostics(&program, parsed, located);
        }
        config_diags.extend(global);
    }
    let error_count = |d: &[Diagnostic], c: &[ts_r_config::ConfigDiagnostic]| {
        d.iter()
            .filter(|x| x.category == ts_r_diagnostics::Category::Error)
            .count()
            + c.iter()
                .filter(|x| matches!(x.severity, Severity::Error))
                .count()
    };
    let errors_before_emit = error_count(&diags, &config_diags);
    let emitted = emitting.then(|| {
        let mut e = emit_program(&program, &cfg, &mut diags, &mut config_diags);
        references::drop_referenced_outputs(&mut e.0, &program, ref_configs, fs.case_sensitive());
        e
    });
    let emit_errors = errors_before_emit == 0 && error_count(&diags, &config_diags) > 0;
    let mut sources = crate::sources_of(&program);
    let diags = merge_config_diagnostics(diags, &config_diags, &mut sources, fs);
    let diags = add_missing_roots(parsed, fs, diags, &mut sources);
    let mut rendered = host.render_diags(&diags, sources);
    let report = host.report.with(&parsed.options);
    if report.trace {
        // `--traceResolution`: before the diagnostics.
        let trace = crate::trace::text(&fs, &program, (!refs.is_empty()).then(|| refs.clone()));
        rendered.text.insert_str(0, &trace);
    }
    let errors = rendered.errors > 0;
    let reported = Reported {
        errors: rendered.errors > semantic,
        semantic_errors: semantic > 0,
        no_check,
    };
    let blocked = errors && parsed.options.no_emit_on_error() == Some(true);
    let mut skipped = emitted.as_ref().is_none_or(|(r, _)| r.skipped) || blocked;
    let (written, failed) = write_project(
        parsed,
        &program,
        emitted.as_ref(),
        blocked,
        reported,
        build_mode,
        job.is_some_and(|j| j.force),
        Some(&package_jsons),
        clash,
        memory,
    );
    host.record_written(&written);
    for d in &failed {
        if !host.quiet {
            rendered.text.push_str(&format_plain(
                d,
                &Shown {
                    sources: Vec::new(),
                },
            ));
            rendered.text.push('\n');
        }
        rendered.errors += 1;
        skipped = true;
    }
    if (host.list_emitted || parsed.options.list_emitted_files() == Some(true)) && !host.quiet {
        rendered.text.push_str(&ts_r_emit::listing(&written));
    }
    if !host.quiet {
        rendered.text.push_str(&crate::explain::files(
            &program, parsed, report, fs, &host.cwd,
        ));
    }
    if report.stats {
        crate::explain::stats(&program, started.elapsed());
    }
    Compiled {
        rendered,
        skipped,
        wrote: !written.is_empty(),
        buildinfo_clash: clash,
        emit_errors_only: emit_errors && !blocked && emitted.is_some() && failed.is_empty(),
    }
}

/// What a build reported, for the buildinfo ([`incremental::Built`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct Reported {
    /// Errors other than semantic ones (syntax, options, emit).
    pub errors: bool,
    /// Semantic (checker) errors.
    pub semantic_errors: bool,
    /// The build ran with `noCheck`.
    pub no_check: bool,
}

/// Checks `program`: under `noCheck` only its syntax ([`syntactic_diagnostics`]). Returns
/// the diagnostics and how many of them are semantic errors.
pub fn check(program: &ProgramFiles, threads: usize, no_check: bool) -> (Vec<Diagnostic>, usize) {
    let syntax = syntactic_diagnostics(program);
    if no_check {
        return (syntax, 0);
    }
    let d = check_program(program, CheckRun { threads }).diagnostics;
    let syntax_errors = syntax.iter().filter(|d| d.is_error()).count();
    let semantic = d
        .iter()
        .filter(|d| d.is_error())
        .count()
        .saturating_sub(syntax_errors);
    (d, semantic)
}

/// The program diagnostics tsc keeps when `rootDir` errors (TS6059) replace the checker's:
/// those, then TS6307 (a `composite` file not listed), by file.
pub fn rooted_program_diagnostics(
    program: &ProgramFiles,
    parsed: &ParsedConfig,
    located: Vec<Diagnostic>,
) -> Vec<Diagnostic> {
    let mut diags = located;
    let composite = references::composite_diagnostics(program, parsed);
    if !composite.is_empty() {
        diags.extend(composite);
        sort_by_file(program, &mut diags);
    }
    diags
}

/// tsc reports option diagnostics only for a program without syntax errors (observed:
/// `option-diagnostics-are-suppressed-when-there-are-syntactic-errors`). With option errors
/// (`diags` then empty) and syntax errors, the syntax errors are reported and only the
/// config file's own diagnostics stay.
pub fn syntax_before_options(
    parsed: &ParsedConfig,
    program: &ProgramFiles,
    diags: &mut Vec<Diagnostic>,
    config_diags: &mut Vec<ts_r_config::ConfigDiagnostic>,
) {
    let syntax = syntactic_diagnostics(program);
    if syntax.iter().any(Diagnostic::is_error) {
        *diags = syntax;
        *config_diags = parsed.diagnostics.clone();
    } else if program.files.iter().all(|f| !f.is_lib) {
        // Global types missing (`noLib`) are reported with the option errors.
        let checker = ts_r_checker::Checker::new(program);
        diags.extend(ts_r_checker::driver::missing_global_type_diagnostics(
            &checker,
        ));
    }
}

/// What a `noCheck` build reports of the program: syntax errors, without the grammar errors
/// tsc's checker reports (`CHECKER_GRAMMAR_CODES`), sorted like the checker's.
pub fn syntactic_diagnostics(program: &ProgramFiles) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (i, f) in program.files.iter().enumerate() {
        if f.is_lib {
            continue;
        }
        for d in ts_r_parser::tsc_syntax_errors(&f.syntax, &f.grammar, &f.lenient) {
            if !ts_r_checker::driver::CHECKER_GRAMMAR_CODES.contains(&d.code) {
                out.push(d.with_file(ts_r_diagnostics::FileId(i as u32)));
            }
        }
    }
    ts_r_checker::driver::sort_and_dedup(program, out)
}

/// Adds TS6053/TS6231 for `files` entries that name no file
/// (`ts_r_projects::roots`) after the leading global diagnostics; the config file joins
/// `sources` when related information points into it.
pub fn add_missing_roots(
    parsed: &ParsedConfig,
    fs: &dyn ts_r_config::FileSystem,
    mut diags: Vec<Diagnostic>,
    sources: &mut Vec<Option<(String, String)>>,
) -> Vec<Diagnostic> {
    let missing = ts_r_projects::roots::missing_root_diagnostics(parsed, fs, &mut |p| {
        let i = sources
            .iter()
            .position(|s| s.as_ref().is_some_and(|(q, _)| q == p))
            .unwrap_or_else(|| {
                sources.push(Some((p.to_owned(), fs.read_file(p).unwrap_or_default())));
                sources.len() - 1
            });
        ts_r_diagnostics::FileId(i as u32)
    });
    if !missing.is_empty() {
        let at = diags.iter().take_while(|d| d.file.0 == u32::MAX).count();
        diags.splice(at..at, missing);
    }
    diags
}

/// Writes the outputs the incremental plan picks, then the buildinfo (for `--build`, or an
/// `incremental`/`composite` project). Returns the files written, buildinfo last (tsc's
/// `--listEmittedFiles` lists it too), and TS5033 for failed writes. `package_jsons`:
/// see [`incremental::Built::package_jsons`]. `clash` (TS6377): every output is written and
/// the buildinfo is not. `memory` (`--watch`): the previous compilation's program state in
/// this session stands in for the buildinfo on disk (so a project without `incremental`
/// re-emits only what changed, as tsc's watch mode does) and receives the new state.
#[allow(clippy::too_many_arguments)]
pub fn write_project(
    parsed: &ParsedConfig,
    program: &ProgramFiles,
    emitted: Option<&(ts_r_emit::EmitResult, Vec<String>)>,
    blocked: bool,
    reported: Reported,
    build_mode: bool,
    force: bool,
    package_jsons: Option<&[String]>,
    clash: bool,
    memory: Option<&mut Option<BuildInfo>>,
) -> (Vec<String>, Vec<Diagnostic>) {
    let mut prev = incremental::Previous::load(parsed);
    let incremental_project = prev.incremental;
    let watching = memory.is_some();
    match memory.as_deref() {
        Some(Some(info)) => prev.read = Read::Ours(Box::new(info.clone())),
        // The first compilation of a session: a project without `incremental` starts clean
        // whatever an earlier buildinfo says.
        Some(None) if !incremental_project => prev.read = Read::Missing,
        _ => {}
    }
    if watching {
        // In memory every project keeps program state.
        prev.incremental = true;
    }
    let with_buildinfo = (build_mode || incremental_project) && !clash;
    let mut plan = incremental::plan(
        &prev,
        &incremental::Built {
            config: parsed,
            program: Some(program),
            emitted: emitted.map(|(r, _)| r),
            blocked,
            errors: reported.errors,
            semantic_errors: reported.semantic_errors,
            no_check: reported.no_check,
            write_all: force || clash || (build_mode && !prev.incremental),
            with_buildinfo,
            package_jsons,
        },
        &|p| std::fs::read(p).ok(),
    );
    if let Some(m) = memory {
        *m = Some(plan.buildinfo.clone());
    }
    let mut written = Vec::new();
    let mut failed = Vec::new();
    let mut writer = ts_r_emit::FsWriter::protecting(program.files.iter().map(|f| f.path.as_str()));
    if let Some((result, _)) = emitted
        && !plan.write.is_empty()
    {
        let subset = ts_r_emit::EmitResult {
            outputs: plan
                .write
                .iter()
                .map(|&i| result.outputs[i].clone())
                .collect(),
            diagnostics: Vec::new(),
            skipped: result.skipped,
        };
        let (w, f) = ts_r_emit::write_outputs(&subset, &mut writer);
        written = w;
        failed = f;
    }
    if with_buildinfo && !failed.is_empty() {
        plan.buildinfo.errors = true;
        plan.buildinfo_text = Some(buildinfo::to_text(&plan.buildinfo));
    }
    if let Some(text) = &plan.buildinfo_text {
        let path = &prev.buildinfo_path;
        match writer.write(path, text.as_bytes()) {
            Ok(()) => written.push(path.clone()),
            Err(e) => failed.push(ts_r_emit_global(
                5033,
                format!("Could not write file '{path}': {e}."),
            )),
        }
    }
    (written, failed)
}

pub fn sort_by_file(program: &ProgramFiles, diags: &mut [Diagnostic]) {
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

/// The closing `--pretty` summary over a whole build.
pub(crate) fn summary(files: &[ErrorFile], error_count: usize) -> String {
    const GREY: &str = "\x1b[90m";
    const RESET: &str = "\x1b[0m";
    let first = files
        .first()
        .map(|f| format!("{}{GREY}:{}{RESET}", f.name, f.line));
    let head = match (error_count, files.len(), &first) {
        (1, _, Some(at)) => format!("Found 1 error in {at}"),
        (n, 1, Some(at)) => format!("Found {n} errors in the same file, starting at: {at}"),
        _ => format_summary(error_count, files.len(), None),
    };
    let mut out = format!("{head}\n");
    if files.len() > 1 {
        let digits = files
            .iter()
            .map(|f| f.count.to_string().len())
            .max()
            .unwrap_or(1);
        let width = digits.max("Errors".len());
        out.push('\n');
        out.push_str(&" ".repeat(width - "Errors".len()));
        out.push_str("Errors  Files\n");
        // The table is sorted by the names shown (observed in every tsc 7 baseline).
        let mut rows: Vec<&ErrorFile> = files.iter().collect();
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        for f in rows {
            out.push_str(&format!(
                "{:>width$}  {}{GREY}:{}{RESET}\n",
                f.count, f.name, f.line
            ));
        }
    }
    out.push('\n');
    out
}

/// The projects named on a `--build` command line (the cwd's tsconfig.json without any).
pub fn build_roots(cl: &CommandLine, fs: &dyn FileSystem, cwd: &str) -> Vec<String> {
    if cl.file_names.is_empty() {
        vec![graph::resolve_project(fs, cwd, ".")]
    } else {
        cl.file_names
            .iter()
            .map(|f| graph::resolve_project(fs, cwd, f))
            .collect()
    }
}

/// The `--build` flags of a command line.
pub fn build_flags(cl: &CommandLine) -> Flags {
    let b = &cl.build_options;
    Flags {
        verbose: b.verbose,
        dry: b.dry,
        force: b.force,
        clean: b.clean,
        stop_on_errors: b.stop_build_on_errors,
        builders: b.builders.map_or(1, |n| n as usize),
        watch: false,
    }
}

/// The project graph, each config loaded with the command line merged in.
pub fn load_graph(cl: &CommandLine, fs: &dyn FileSystem, roots: &[String]) -> graph::Graph {
    graph::build_graph(fs, roots, &mut |p| {
        ConfigLoader::new(fs).load_with_overrides(p, |o| merge_command_line(cl, o))
    })
}

/// tsc's exit code for a build: 4 on a reference cycle, 0 without errors, 1 when outputs
/// were skipped, else 2.
pub fn exit_code(report: &build::Report) -> u8 {
    if report.cycle {
        4
    } else if report.errors == 0 {
        0
    } else if report.skipped {
        1
    } else {
        2
    }
}

/// `--build`: returns the exit code.
pub fn run(
    cl: &CommandLine,
    fs: &RealFs,
    cwd: &str,
    pretty: bool,
    threads: usize,
    out: &mut impl Write,
) -> u8 {
    let quiet = cl.flag(OptionId::Quiet);
    let roots = build_roots(cl, fs, cwd);
    let g = load_graph(cl, fs, &roots);
    let flags = build_flags(cl);
    let mut host = BuildHost::new(fs, cwd, pretty, quiet, threads);
    host.list_emitted = cl.flag(OptionId::ListEmittedFiles);
    host.report = crate::explain::Flags::from_command_line(cl);
    let report = build::run(&host, fs, &g, flags, out);
    if pretty && report.errors > 0 && !quiet {
        let _ = write!(out, "\n{}", summary(&report.files, report.errors));
    }
    let _ = out.flush();
    exit_code(&report)
}

fn ts_r_emit_global(code: u32, message: String) -> Diagnostic {
    crate::global(code, ts_r_diagnostics::Category::Error, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries() {
        let f = |name: &str, line, count| ErrorFile {
            name: name.into(),
            line,
            count,
        };
        assert_eq!(summary(&[], 1), "Found 1 error.\n\n");
        assert_eq!(
            summary(&[f("a.ts", 3, 1)], 1),
            "Found 1 error in a.ts\x1b[90m:3\x1b[0m\n\n"
        );
    }
}
