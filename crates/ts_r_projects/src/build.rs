//! The `--build` driver (design record 0012, E3): walks the project graph in build order,
//! prints tsc's status messages, builds what is out of date through the [`Host`] (the
//! compile pipeline lives in ts-r-check), touches timestamps, and implements `--dry`,
//! `--force`, `--clean`, `--stopBuildOnErrors` and `--builders` (projects whose references
//! are done build in parallel; output is printed in build order, as with one builder).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::sync::{Arc, Condvar, Mutex};
use std::time::SystemTime;

use ts_r_config::{ConfigDiagnostic, FileSystem, ParsedConfig};
use ts_r_resolver::ProjectReferences;

use crate::buildinfo;
use crate::clock;
use crate::graph::{Graph, Node};
use crate::references;
use crate::status::{self, Status, Upstream};

/// `--build` flags.
#[derive(Debug, Clone, Copy, Default)]
pub struct Flags {
    pub verbose: bool,
    pub dry: bool,
    pub force: bool,
    pub clean: bool,
    pub stop_on_errors: bool,
    /// Projects built at once (1 to 4).
    pub builders: usize,
    /// A `--watch` build (set by [`run_watch`]; some statuses read differently).
    pub watch: bool,
}

/// A file with errors, for the closing summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorFile {
    pub name: String,
    pub line: u32,
    pub count: usize,
}

/// Diagnostics rendered for the terminal.
#[derive(Debug, Clone, Default)]
pub struct Rendered {
    pub text: String,
    pub errors: usize,
    /// Per file with errors, in output order.
    pub files: Vec<ErrorFile>,
}

/// What building one project did.
#[derive(Debug, Clone, Default)]
pub struct Compiled {
    pub rendered: Rendered,
    /// Some output was not written (`noEmit`, `noEmitOnError`, an emit ts-r cannot do yet).
    pub skipped: bool,
    /// Some file (an output or the buildinfo) was written.
    pub wrote: bool,
    /// The buildinfo was not written because a referenced project writes the same file
    /// (TS6377): the outputs are then touched, as tsc does.
    pub buildinfo_clash: bool,
    /// The only errors came from declaration emit (TS4094, ...) of a project without
    /// program state that wrote its outputs: tsc then updates the output timestamps
    /// (observed: `tsbuild/noCheck/dts-errors`, `tsbuild/declarationEmit/reports-dts-
    /// generation-errors`).
    pub emit_errors_only: bool,
}

/// One project to build.
#[derive(Debug)]
pub struct Job<'a> {
    pub config: &'a ParsedConfig,
    /// Sources of the referenced projects (transitively) and their outputs.
    pub references: Arc<ProjectReferences>,
    /// The referenced projects' configs (transitively, depth first).
    pub reference_configs: &'a [ParsedConfig],
    /// `--force`: write every output and the buildinfo.
    pub force: bool,
}

/// The compile pipeline and the terminal, provided by ts-r-check.
pub trait Host: Sync {
    /// Loads, checks and emits one project, writes its outputs and buildinfo, and renders
    /// its diagnostics.
    fn compile(&self, job: &Job) -> Compiled;
    /// Renders diagnostics without a program (TS6202, TS6053).
    fn render(&self, diags: &[ConfigDiagnostic]) -> Rendered;
    /// Pretty (coloured) output.
    fn pretty(&self) -> bool;
    /// A path as printed (relative to the working directory).
    fn show(&self, path: &str) -> String;
    /// Outputs whose timestamps the build updated (`--watch` stops tracking its own writes).
    fn touched(&self, _paths: &[String]) {}
}

/// The whole build's result, for the summary and the exit code.
#[derive(Debug, Default)]
pub struct Report {
    pub errors: usize,
    pub files: Vec<ErrorFile>,
    /// Some project skipped outputs (exit code 1 with errors).
    pub skipped: bool,
    /// A reference cycle stopped the build (exit code 4).
    pub cycle: bool,
}

/// One status line: `[HH:MM:SS AM] message` (pretty) or `HH:MM:SS AM - message`, then a
/// blank line.
pub fn status_line(pretty: bool, message: &str) -> String {
    let t = clock::time_of_day(SystemTime::now());
    if pretty {
        format!("[\x1b[90m{t}\x1b[0m] {message}\n\n")
    } else {
        format!("{t} - {message}\n\n")
    }
}

#[derive(Debug, Clone, Default)]
struct Outcome {
    errors: bool,
    /// Not built in this run because of its references.
    not_built: bool,
    /// Compiled in this run.
    compiled: bool,
}

/// What a `--watch` session keeps of each project between its builds.
#[derive(Debug, Default)]
pub struct Session {
    last: BTreeMap<String, Kept>,
}

/// A project's last result in a session.
#[derive(Debug, Clone, Default)]
struct Kept {
    /// The config existed (a config created since is built, not replayed).
    had_config: bool,
    outcome: Outcome,
    rendered: Rendered,
    skipped: bool,
}

/// One build of a `--watch` session (design record 0012, E4).
#[derive(Debug)]
pub struct Cycle<'a> {
    pub session: &'a mut Session,
    /// Projects (by config path) whose own inputs changed since the session's last build;
    /// `None` on the first build. Only they and the projects referencing them (transitively)
    /// are considered again: the others print their last diagnostics and nothing else.
    pub changed: Option<&'a BTreeSet<String>>,
}

/// How a project takes part in a build.
#[derive(Debug, Clone, Default)]
enum Mode {
    /// Status, then build when out of date.
    #[default]
    Build,
    /// `--watch`, unchanged: its last result again, without status messages.
    Replay(Kept),
    /// `--watch`: a reference changed and the last build had errors: built again
    /// ([`Status::HasErrors`]).
    HasErrors,
}

/// Runs the build. `fs` reads configs; outputs go to the real file system.
pub fn run(
    host: &dyn Host,
    fs: &dyn FileSystem,
    graph: &Graph,
    flags: Flags,
    out: &mut dyn Write,
) -> Report {
    run_cycle(host, fs, graph, flags, None, out)
}

/// One build of a `--watch` session: [`run`], considering only what [`Cycle::changed`]
/// affects, and remembering the results for the next build.
pub fn run_watch(
    host: &dyn Host,
    fs: &dyn FileSystem,
    graph: &Graph,
    flags: Flags,
    cycle: Cycle,
    out: &mut dyn Write,
) -> Report {
    let flags = Flags {
        watch: true,
        ..flags
    };
    run_cycle(host, fs, graph, flags, Some(cycle), out)
}

/// Each project's [`Mode`] in a watch build.
fn modes(graph: &Graph, cycle: &Cycle) -> Vec<Mode> {
    let n = graph.order.len();
    let Some(changed) = cycle.changed else {
        return vec![Mode::Build; n];
    };
    let direct: Vec<bool> = graph
        .order
        .iter()
        .map(|node| changed.contains(&node.config_path))
        .collect();
    let mut considered = direct.clone();
    // References usually come first in build order; iterate until nothing changes.
    loop {
        let mut grew = false;
        for i in 0..n {
            if !considered[i]
                && graph.order[i]
                    .references
                    .iter()
                    .any(|(j, _)| considered[*j])
            {
                considered[i] = true;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    (0..n)
        .map(|i| {
            let last = cycle.session.last.get(&graph.order[i].config_path);
            match last {
                Some(k) if k.had_config != graph.order[i].config.is_some() => Mode::Build,
                Some(k) if !considered[i] => Mode::Replay(k.clone()),
                Some(k) if !direct[i] && k.outcome.errors => Mode::HasErrors,
                _ => Mode::Build,
            }
        })
        .collect()
}

fn run_cycle(
    host: &dyn Host,
    fs: &dyn FileSystem,
    graph: &Graph,
    flags: Flags,
    cycle: Option<Cycle>,
    out: &mut dyn Write,
) -> Report {
    let pretty = host.pretty();
    let mut report = Report::default();
    let add = |report: &mut Report, r: &Rendered| {
        report.errors += r.errors;
        report.files.extend(r.files.iter().cloned());
    };
    if flags.verbose {
        let mut m = String::from("Projects in this build: ");
        for n in &graph.order {
            m.push_str("\r\n    * ");
            m.push_str(&host.show(&n.config_path));
        }
        let _ = write!(out, "{}", status_line(pretty, &m));
    }
    if !graph.cycles.is_empty() {
        let r = host.render(&graph.cycles);
        let _ = write!(out, "{}", r.text);
        add(&mut report, &r);
        report.cycle = true;
        return report;
    }
    if flags.clean {
        clean(host, graph, flags, out, &mut report);
        return report;
    }
    let modes = match &cycle {
        Some(c) => modes(graph, c),
        None => vec![Mode::Build; graph.order.len()],
    };

    // Referenced configs per project (transitively), for the resolver map.
    let mut load = |p: &str| references::load_config(fs, p);
    let ref_configs: Vec<Vec<ParsedConfig>> = graph
        .order
        .iter()
        .map(|n| match &n.config {
            Some(c) => references::referenced_configs(c, &mut load),
            None => Vec::new(),
        })
        .collect();

    let n = graph.order.len();
    let builders = flags.builders.clamp(1, 4);
    let results: Vec<(String, Outcome, Rendered, bool)> = if builders == 1 || n < 2 {
        let mut results = Vec::with_capacity(n);
        for (i, mode) in modes.iter().enumerate() {
            let r = build_one(
                host,
                graph,
                &ref_configs,
                i,
                &results_outcomes(&results),
                flags,
                mode,
            );
            let _ = write!(out, "{}", r.0);
            let _ = out.flush();
            results.push(r);
        }
        results
    } else {
        parallel(host, graph, &ref_configs, flags, builders, &modes, out)
    };
    for (_, _, rendered, skipped) in &results {
        add(&mut report, rendered);
        report.skipped |= *skipped;
    }
    if let Some(c) = cycle {
        c.session.last.clear();
        for (node, (_, outcome, rendered, skipped)) in graph.order.iter().zip(results) {
            c.session.last.insert(
                node.config_path.clone(),
                Kept {
                    had_config: node.config.is_some(),
                    outcome,
                    rendered,
                    skipped,
                },
            );
        }
    }
    report
}

fn results_outcomes(results: &[(String, Outcome, Rendered, bool)]) -> Vec<Option<Outcome>> {
    results.iter().map(|r| Some(r.1.clone())).collect()
}

/// Builds with several builders: a project starts when its references are done; output is
/// printed in build order.
fn parallel(
    host: &dyn Host,
    graph: &Graph,
    ref_configs: &[Vec<ParsedConfig>],
    flags: Flags,
    builders: usize,
    modes: &[Mode],
    out: &mut dyn Write,
) -> Vec<(String, Outcome, Rendered, bool)> {
    let n = graph.order.len();
    struct State {
        started: Vec<bool>,
        done: Vec<Option<(String, Outcome, Rendered, bool)>>,
    }
    let state = Mutex::new(State {
        started: vec![false; n],
        done: vec![None; n],
    });
    let cv = Condvar::new();
    let mut printed = 0;
    std::thread::scope(|s| {
        for _ in 0..builders {
            s.spawn(|| {
                loop {
                    // Pick the first project whose references are done.
                    let (i, outcomes) = {
                        let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            if st.started.iter().all(|s| *s) {
                                return;
                            }
                            let ready = (0..n).find(|&i| {
                                !st.started[i]
                                    && graph.order[i]
                                        .references
                                        .iter()
                                        .all(|(j, _)| st.done[*j].is_some())
                            });
                            match ready {
                                Some(i) => {
                                    st.started[i] = true;
                                    let outcomes: Vec<Option<Outcome>> = st
                                        .done
                                        .iter()
                                        .map(|d| d.as_ref().map(|d| d.1.clone()))
                                        .collect();
                                    break (i, outcomes);
                                }
                                None => {
                                    st = cv.wait(st).unwrap_or_else(|e| e.into_inner());
                                }
                            }
                        }
                    };
                    let r = build_one(host, graph, ref_configs, i, &outcomes, flags, &modes[i]);
                    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                    st.done[i] = Some(r);
                    cv.notify_all();
                }
            });
        }
        // Print in order as results arrive.
        while printed < n {
            let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
            while st.done[printed].is_none() {
                st = cv.wait(st).unwrap_or_else(|e| e.into_inner());
            }
            while printed < n
                && let Some(r) = &st.done[printed]
            {
                let _ = write!(out, "{}", r.0);
                printed += 1;
            }
            let _ = out.flush();
        }
    });
    let st = state.into_inner().unwrap_or_else(|e| e.into_inner());
    st.done.into_iter().map(|d| d.unwrap_or_default()).collect()
}

/// Status, messages and build of project `i`: (text, outcome, diagnostics, skipped).
fn build_one(
    host: &dyn Host,
    graph: &Graph,
    ref_configs: &[Vec<ParsedConfig>],
    i: usize,
    outcomes: &[Option<Outcome>],
    flags: Flags,
    mode: &Mode,
) -> (String, Outcome, Rendered, bool) {
    if let Mode::Replay(k) = mode {
        return (
            k.rendered.text.clone(),
            k.outcome.clone(),
            k.rendered.clone(),
            k.skipped,
        );
    }
    let pretty = host.pretty();
    let node: &Node = &graph.order[i];
    let mut text = String::new();
    let mut outcome = Outcome::default();
    let show = |p: &str| host.show(p);
    let line = |m: &str| status_line(pretty, m);
    let Some(config) = &node.config else {
        if flags.verbose {
            text.push_str(&line(
                &Status::ConfigMissing
                    .message(&node.config_path, &show)
                    .unwrap_or_default(),
            ));
        }
        let r = host.render(&[ConfigDiagnostic::global(
            6053,
            format!("File '{}' not found.", node.config_path),
        )]);
        text.push_str(&r.text);
        outcome.errors = true;
        outcome.not_built = true;
        // A missing project named on the command line skips outputs; one only referenced
        // does not (observed: `tsbuild/sample/building-project-in-not-build-order-doesnt-
        // throw-error`, `tsbuildWatch/sample/watches-config-files-that-are-not-present`).
        return (text, outcome, r, node.root);
    };
    let upstream: Vec<Upstream> = node
        .references
        .iter()
        .map(|(j, shown)| {
            let o = outcomes.get(*j).cloned().flatten().unwrap_or_default();
            let bi = graph.order[*j]
                .config
                .as_ref()
                .map(|c| buildinfo::path_for(&c.options, &c.config_path))
                .unwrap_or_default();
            Upstream {
                shown: shown.clone(),
                config: graph.order[*j].config_path.clone(),
                buildinfo: bi,
                errors: o.errors,
                skipped: o.not_built,
                built: o.compiled,
            }
        })
        .collect();
    let refs = Arc::new(references::project_references(
        &ref_configs[i],
        config.case_sensitive(),
    ));
    let st = status::check(&status::Inputs {
        config,
        upstream: &upstream,
        references: &refs,
        force: flags.force,
        stop_on_errors: flags.stop_on_errors,
        watch: flags.watch,
    });
    let st = match (mode, st) {
        (
            Mode::HasErrors,
            st @ (Status::UpstreamErrors { .. }
            | Status::UpstreamNotBuilt { .. }
            | Status::Container),
        ) => st,
        (Mode::HasErrors, _) => Status::HasErrors,
        (_, st) => st,
    };
    if flags.verbose
        && let Some(m) = st.message(&node.config_path, &show)
    {
        text.push_str(&line(&m));
    }
    let abs = |p: &str| p.to_owned();
    match &st {
        Status::Container => {
            // Only the config file's own errors (TS18002 and the like): the program-level
            // option checks (TS5108) come with a program, which a container never has
            // (observed: `tsbuild/resolveJsonModule/importing-json-module-from-project-
            // reference`).
            let r = host.render(&config.diagnostics);
            if r.errors > 0 {
                text.push_str(&r.text);
                outcome.errors = true;
                outcome.not_built = true;
                return (text, outcome, r, true);
            }
        }
        Status::UpstreamErrors { dep } | Status::UpstreamNotBuilt { dep } => {
            let why = if matches!(st, Status::UpstreamErrors { .. }) {
                "has errors"
            } else {
                "was not built"
            };
            if flags.verbose {
                text.push_str(&line(&format!(
                    "Skipping build of project '{}' because its dependency '{}' {why}",
                    show(&node.config_path),
                    show(dep)
                )));
            }
            outcome.not_built = true;
        }
        Status::UpToDate { .. } => {
            if flags.dry {
                text.push_str(&line(&format!(
                    "Project '{}' is up to date",
                    abs(&node.config_path)
                )));
            }
        }
        Status::TouchForInputs | Status::TouchForUpstream => {
            if flags.dry {
                text.push_str(&line(&format!(
                    "A non-dry build would update timestamps for output of project '{}'",
                    abs(&node.config_path)
                )));
            } else {
                if flags.verbose {
                    text.push_str(&line(&format!(
                        "Updating output timestamps of project '{}'...",
                        show(&node.config_path)
                    )));
                }
                touch_outputs(host, config);
            }
        }
        _ => {
            if flags.dry {
                text.push_str(&line(&format!(
                    "A non-dry build would build project '{}'",
                    abs(&node.config_path)
                )));
                return (text, outcome, Rendered::default(), false);
            }
            if flags.verbose {
                text.push_str(&line(&format!(
                    "Building project '{}'...",
                    show(&node.config_path)
                )));
            }
            let compiled = host.compile(&Job {
                config,
                references: refs.clone(),
                reference_configs: &ref_configs[i],
                force: flags.force,
            });
            text.push_str(&compiled.rendered.text);
            outcome.errors = compiled.rendered.errors > 0;
            outcome.compiled = true;
            let from_upstream = matches!(&st, Status::OlderThanInput { input, .. }
                if upstream.iter().any(|u| u.shown == *input || u.config == *input));
            // A newer config file that changed nothing (observed:
            // `tsbuildWatch/noEmit/semantic-errors-with-incremental`, `noEmit` turned on).
            let config_newer = matches!(&st, Status::OlderThanInput { input, .. }
                if *input == config.config_path || config.extended_config_paths.contains(input));
            let rebuilt = from_upstream || config_newer || matches!(st, Status::HasErrors);
            // A config edited after the buildinfo: its timestamp advances even when the
            // rebuild rewrote nothing, or the next build would rebuild again.
            let mtime = |p: &str| std::fs::metadata(p).and_then(|m| m.modified()).ok();
            let from_config = config_newer
                && matches!(&st, Status::OlderThanInput { output, input }
                    if mtime(output).zip(mtime(input)).is_some_and(|(o, i)| o < i));
            if compiled.buildinfo_clash
                || (rebuilt && !compiled.wrote)
                || (from_config && !outcome.errors)
                || (compiled.emit_errors_only && !buildinfo::is_incremental(&config.options))
            {
                if flags.verbose {
                    text.push_str(&line(&format!(
                        "Updating unchanged output timestamps of project '{}'...",
                        show(&node.config_path)
                    )));
                }
                touch_outputs(host, config);
            }
            return (text, outcome, compiled.rendered, compiled.skipped);
        }
    }
    (text, outcome, Rendered::default(), false)
}

/// Sets the outputs' modification time to now: the buildinfo, and every output of a
/// project without program state.
fn touch_outputs(host: &dyn Host, config: &ParsedConfig) {
    let bi = buildinfo::path_for(&config.options, &config.config_path);
    let mut files = vec![bi];
    if !buildinfo::is_incremental(&config.options) {
        files.extend(status::expected_outputs(config));
    }
    let now = SystemTime::now();
    files.retain(|f| {
        std::fs::OpenOptions::new()
            .write(true)
            .open(f)
            .is_ok_and(|file| file.set_modified(now).is_ok())
    });
    host.touched(&files);
}

/// `--clean`: deletes every project's outputs and buildinfo (`--dry`: lists them).
fn clean(host: &dyn Host, graph: &Graph, flags: Flags, out: &mut dyn Write, report: &mut Report) {
    let pretty = host.pretty();
    let mut files: Vec<String> = Vec::new();
    for node in &graph.order {
        let Some(c) = &node.config else {
            let r = host.render(&[ConfigDiagnostic::global(
                6053,
                format!("File '{}' not found.", node.config_path),
            )]);
            let _ = write!(out, "{}", r.text);
            report.errors += r.errors;
            report.files.extend(r.files);
            report.skipped = true;
            continue;
        };
        if status::is_container(c) {
            continue;
        }
        let mut outputs = status::expected_outputs(c);
        outputs.push(buildinfo::path_for(&c.options, &c.config_path));
        let cs = c.case_sensitive();
        for o in outputs {
            // An output that is also an input (a `.js` root without `outDir`) stays.
            let input = c
                .file_names
                .iter()
                .any(|f| ts_r_config::path::eq(f, &o, cs));
            if !input && std::path::Path::new(&o).is_file() && !files.contains(&o) {
                files.push(o);
            }
        }
    }
    if flags.dry {
        if !files.is_empty() {
            let mut m = String::from("A non-dry build would delete the following files: ");
            for f in &files {
                m.push_str("\r\n * ");
                m.push_str(f);
            }
            let _ = write!(out, "{}", status_line(pretty, &m));
        }
        return;
    }
    for f in &files {
        let _ = std::fs::remove_file(f);
    }
}
