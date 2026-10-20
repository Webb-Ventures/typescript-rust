//! `ts-r-check --watch` (design record 0012, E4), for `-p` and `-b`.
//!
//! Every file-system question a build asks (does this file exist, what is in this
//! directory, what is this file's stamp) goes through a [`TrackingFs`], which remembers the
//! answer and who asked it (a project's config load, its root list or its program). A
//! change is a question whose answer differs now: a file edited, touched (its modification
//! time is part of the stamp), created or deleted, a directory listing that grew. The
//! files a build writes or touches are answered again right after it, so a build never
//! triggers the next one.
//!
//! - `-p`: each change rebuilds the program with the parsed-file cache of the session
//!   ([`ts_r_checker::program::SourceCache`]) and the previous build's program state kept in
//!   memory, so only what changed (and what depends on it) is emitted again, as tsc does.
//! - `-b`: each change rebuilds the projects whose own inputs changed and the projects that
//!   reference them; the others print their last diagnostics again
//!   ([`ts_r_projects::build::run_watch`]).
//!
//! Output follows tsc 7: the screen is cleared before each build (not with
//! `--preserveWatchOutput`, `--diagnostics`, `--extendedDiagnostics` or plain output), then
//! `Starting compilation in watch mode...` or `File change detected. Starting incremental
//! compilation...`, the diagnostics, and `Found N errors. Watching for file changes.`. The
//! first build's exit status is the session's: always 0 for `-p`, the build's for `-b`.
//!
//! Changes are noticed by native file events ([`ts_r_watch`]) and, for what they don't
//! cover, by polling every `--watchInterval` milliseconds (default 2000). The hidden driver
//! `TS_R_WATCH_DRIVER=stdin` replaces both for tests: each line on stdin is "look for
//! changes now", end of input ends the session, and after each look the line
//! `\x01ts-r-watch-cycle <status>` is printed so a harness knows the build is done.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Write};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use ts_r_checker::program::SourceCache;
use ts_r_config::{
    ConfigLoader, DirEntry, FileStamp, FileSystem, OptionId, ParsedConfig, RealFs, path,
};
use ts_r_diagnostics::Category;
use ts_r_projects::build::{self as pbuild, Compiled, Host, Job, Rendered, status_line};
use ts_r_projects::buildinfo::{self, BuildInfo};
use ts_r_projects::graph;

use crate::build::{self, BuildHost};
use crate::{Args, global, merge_command_line, print_global};

/// Clears the terminal (and its scroll-back) and moves the cursor home, as tsc does.
const CLEAR: &str = "\x1b[2J\x1b[3J\x1b[H";

/// The line the stdin driver prints after each look for changes.
const CYCLE_SENTINEL: &str = "\u{1}ts-r-watch-cycle";

/// Which part of a project asked a question.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Part {
    /// Loading its tsconfig (and the configs it extends; wildcard directories).
    Config,
    /// Its root files and persisted program inputs, recorded after the config loads
    /// (an up-to-date project loads no program but still rebuilds when an input changes).
    Roots,
    /// Loading its program (module resolution, imported files, references).
    Program,
}

/// Who asked: a project (its config path; empty for the build as a whole) and the part.
type Owner = (String, Part);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    File,
    Dir,
    Stamp,
    List,
    Real,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Seen {
    Bool(bool),
    Stamp(Option<FileStamp>),
    List(Vec<DirEntry>),
    Path(String),
}

#[derive(Debug)]
struct Probe {
    seen: Seen,
    owners: BTreeSet<Owner>,
}

#[derive(Debug)]
struct State {
    owner: Owner,
    probes: BTreeMap<(String, Kind), Probe>,
}

/// The real file system, remembering every question asked through it (module docs).
#[derive(Debug)]
pub struct TrackingFs {
    inner: RealFs,
    state: Mutex<State>,
}

impl TrackingFs {
    pub fn new(inner: RealFs) -> Self {
        TrackingFs {
            inner,
            state: Mutex::new(State {
                owner: (String::new(), Part::Program),
                probes: BTreeMap::new(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Who the questions from now on are for.
    pub fn set_owner(&self, project: &str, part: Part) {
        self.lock().owner = (project.to_owned(), part);
    }

    fn eval(&self, kind: Kind, path: &str) -> Seen {
        match kind {
            Kind::File => Seen::Bool(self.inner.file_exists(path)),
            Kind::Dir => Seen::Bool(self.inner.directory_exists(path)),
            Kind::Stamp => Seen::Stamp(self.inner.stamp(path)),
            Kind::List => Seen::List(self.inner.read_dir(path)),
            Kind::Real => Seen::Path(self.inner.realpath(path)),
        }
    }

    fn record(&self, kind: Kind, path: &str, seen: Seen) {
        let mut st = self.lock();
        let owner = st.owner.clone();
        match st.probes.get_mut(&(path.to_owned(), kind)) {
            Some(p) => {
                p.seen = seen;
                p.owners.insert(owner);
            }
            None => {
                st.probes.insert(
                    (path.to_owned(), kind),
                    Probe {
                        seen,
                        owners: BTreeSet::from([owner]),
                    },
                );
            }
        }
    }

    /// Forgets every question.
    pub fn clear(&self) {
        self.lock().probes.clear();
    }

    /// Forgets the questions `part` of `project` asked (every project's with `None`).
    pub fn forget(&self, project: Option<&str>, part: Part) {
        let mut st = self.lock();
        st.probes.retain(|_, p| {
            p.owners
                .retain(|(o, pt)| *pt != part || project.is_some_and(|x| x != o));
            !p.owners.is_empty()
        });
    }

    /// The projects whose questions have different answers now (the build as a whole is
    /// the empty name). The new answers are kept.
    pub fn take_changes(&self) -> BTreeSet<String> {
        let mut st = self.lock();
        let mut out = BTreeSet::new();
        let keys: Vec<(String, Kind)> = st.probes.keys().cloned().collect();
        for key in keys {
            let now = self.eval(key.1, &key.0);
            if let Some(p) = st.probes.get_mut(&key)
                && p.seen != now
            {
                p.seen = now;
                out.extend(p.owners.iter().map(|(o, _)| o.clone()));
            }
        }
        out
    }

    /// Answers again the questions about `paths` (files the build wrote or touched) and
    /// about their entries in the directories that contain them.
    pub fn refresh(&self, paths: &[String]) {
        let mut st = self.lock();
        let mut dirs: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for p in paths {
            for kind in [Kind::File, Kind::Dir, Kind::Stamp, Kind::Real] {
                if let Some(probe) = st.probes.get_mut(&(p.clone(), kind)) {
                    probe.seen = self.eval(kind, p);
                }
            }
            let mut at = p.as_str();
            while let Some((dir, name)) = at.rsplit_once('/') {
                if dir.is_empty() || !dirs.entry(dir).or_default().insert(name) {
                    break;
                }
                at = dir;
            }
        }
        for (d, names) in dirs {
            for kind in [Kind::Dir, Kind::File, Kind::Real] {
                if let Some(probe) = st.probes.get_mut(&(d.to_owned(), kind)) {
                    probe.seen = self.eval(kind, d);
                }
            }
            if let Some(probe) = st.probes.get_mut(&(d.to_owned(), Kind::List))
                && let Seen::List(entries) = &mut probe.seen
            {
                // Only emitted entries belong to this build; other changes need a rebuild.
                entries.retain(|e| !names.contains(e.name.as_str()));
                entries.extend(
                    self.inner
                        .read_dir(d)
                        .into_iter()
                        .filter(|e| names.contains(e.name.as_str())),
                );
                entries.sort_by(|a, b| a.name.cmp(&b.name));
            }
        }
    }
}

impl FileSystem for TrackingFs {
    fn read_file(&self, path: &str) -> Option<String> {
        self.record(Kind::Stamp, path, Seen::Stamp(self.inner.stamp(path)));
        self.inner.read_file(path)
    }

    fn file_exists(&self, path: &str) -> bool {
        let v = self.inner.file_exists(path);
        self.record(Kind::File, path, Seen::Bool(v));
        v
    }

    fn directory_exists(&self, path: &str) -> bool {
        let v = self.inner.directory_exists(path);
        self.record(Kind::Dir, path, Seen::Bool(v));
        v
    }

    fn read_dir(&self, path: &str) -> Vec<DirEntry> {
        let v = self.inner.read_dir(path);
        self.record(Kind::List, path, Seen::List(v.clone()));
        v
    }

    fn realpath(&self, path: &str) -> String {
        let v = self.inner.realpath(path);
        self.record(Kind::Real, path, Seen::Path(v.clone()));
        v
    }

    fn case_sensitive(&self) -> bool {
        self.inner.case_sensitive()
    }

    fn cwd(&self) -> String {
        self.inner.cwd()
    }

    // The stored case of a path's ancestors: not a dependency. Recording it as a listing
    // would rebuild on any change next to an ancestor (another project in the same parent).
    fn disk_name(&self, dir: &str, name: &str) -> Option<String> {
        self.inner.disk_name(dir, name)
    }

    fn stamp(&self, path: &str) -> Option<FileStamp> {
        let v = self.inner.stamp(path);
        self.record(Kind::Stamp, path, Seen::Stamp(v));
        v
    }
}

/// How the session learns that something may have changed.
enum Driver {
    /// `TS_R_WATCH_DRIVER=stdin`: a line on stdin.
    Stdin,
    /// Native events, and polling every `interval`.
    Native {
        wake: mpsc::Receiver<()>,
        send: mpsc::Sender<()>,
        watching: BTreeSet<String>,
        watchers: Vec<ts_r_watch::FileWatcher>,
        interval: Duration,
    },
}

impl Driver {
    fn new(cl: &ts_r_config::cli::CommandLine) -> Driver {
        if std::env::var("TS_R_WATCH_DRIVER").is_ok_and(|v| v == "stdin") {
            return Driver::Stdin;
        }
        let (send, wake) = mpsc::channel();
        Driver::Native {
            wake,
            send,
            watching: BTreeSet::new(),
            watchers: Vec::new(),
            interval: Duration::from_millis(cl.watch_interval.map_or(2000, |n| n.max(50)) as u64),
        }
    }

    /// Waits for a possible change. `false`: the session ends.
    fn wait(&mut self) -> bool {
        match self {
            Driver::Stdin => {
                let mut line = String::new();
                matches!(std::io::stdin().lock().read_line(&mut line), Ok(n) if n > 0)
            }
            Driver::Native { wake, interval, .. } => {
                let _ = wake.recv_timeout(*interval);
                // Coalesce a burst of events into one look.
                while wake.try_recv().is_ok() {}
                true
            }
        }
    }

    /// After a look: the stdin driver prints its sentinel; the native driver watches `roots`
    /// (the working directory and the project directories, recursively; what lies outside
    /// them is polled).
    fn looked(&mut self, out: &mut dyn Write, status: u8, roots: &[String]) {
        match self {
            Driver::Stdin => {
                let _ = writeln!(out, "{CYCLE_SENTINEL} {status}");
                let _ = out.flush();
            }
            Driver::Native {
                send,
                watching,
                watchers,
                ..
            } => {
                let _ = out.flush();
                for d in roots {
                    if watching.iter().any(|w| path::contains(w, d, true))
                        || !std::path::Path::new(d).is_dir()
                    {
                        continue;
                    }
                    let send = send.clone();
                    if let Ok(w) = ts_r_watch::watch_filtered(
                        std::path::Path::new(d),
                        ts_r_watch::DebounceOptions::default(),
                        ignore_vcs,
                        move |_| {
                            let _ = send.send(());
                        },
                    ) {
                        watchers.push(w);
                        watching.insert(d.clone());
                    }
                }
            }
        }
    }
}

/// Paths whose changes never matter to a build (version-control metadata).
fn ignore_vcs(p: &std::path::Path) -> bool {
    p.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some(".git" | ".hg" | ".svn" | ".DS_Store")
        )
    })
}

fn found_line(pretty: bool, errors: usize) -> String {
    let m = if errors == 1 {
        "Found 1 error. Watching for file changes.".to_owned()
    } else {
        format!("Found {errors} errors. Watching for file changes.")
    };
    status_line(pretty, &m)
}

/// The start of a build: the screen cleared (when allowed), then the status line.
fn start(out: &mut dyn Write, pretty: bool, clear: bool, first: bool) {
    if clear {
        let _ = write!(out, "{CLEAR}");
    }
    let m = if first {
        "Starting compilation in watch mode..."
    } else {
        "File change detected. Starting incremental compilation..."
    };
    let _ = write!(out, "{}", status_line(pretty, m));
}

/// Whether builds clear the screen: pretty output without `--preserveWatchOutput` and the
/// statistics flags.
fn clears(cl: &ts_r_config::cli::CommandLine, pretty: bool, preserve: bool) -> bool {
    pretty
        && !preserve
        && !cl.flag(OptionId::Diagnostics)
        && !cl.flag(OptionId::ExtendedDiagnostics)
}

/// `-w` with `-p` (or files on the command line). Returns the exit status.
pub fn run_project(args: &Args, cwd: &str, pretty: bool, out: &mut dyn Write) -> u8 {
    let cl = &args.cl;
    let real = RealFs::new();
    let config_path = match crate::project_config(cl, &real, cwd) {
        Ok(c) => c,
        Err((message, code)) => {
            match message {
                Some(d) => print_global(out, &[d], pretty),
                None => {
                    let _ = write!(out, "{}", crate::help_text(false));
                }
            }
            let _ = out.flush();
            return code;
        }
    };
    let fs = TrackingFs::new(real);
    let quiet = cl.flag(OptionId::Quiet);
    let mut host = BuildHost::new(&fs, cwd, pretty, quiet, args.threads);
    host.list_emitted = cl.flag(OptionId::ListEmittedFiles);
    host.report = crate::explain::Flags::from_command_line(cl);
    host.cache = Some(SourceCache::default());
    let mut memory: Option<BuildInfo> = None;
    let mut driver = Driver::new(cl);
    // The config files' stamps when they were last parsed, and whether that parse found
    // no inputs (TS18003).
    let mut parsed_at: Option<(Vec<Option<FileStamp>>, bool)> = None;
    let mut cycle = |first: bool, out: &mut dyn Write| {
        fs.clear();
        if let Some(p) = &config_path
            && !fs.file_exists(p)
        {
            let d = global(5083, Category::Error, format!("Cannot read file '{p}'."));
            print_global(out, &[d], pretty);
            let _ = write!(out, "{}", found_line(pretty, 1));
            return;
        }
        let mut parsed = match &config_path {
            Some(p) => ConfigLoader::new(&fs).load_with_overrides(p, |o| merge_command_line(cl, o)),
            None => {
                ConfigLoader::new(&fs).command_line(&cl.file_names, |o| merge_command_line(cl, o))
            }
        };
        // A rebuild for a directory change alone keeps what the config's last parse said
        // about inputs (observed: files removed from an `include` directory report no
        // TS18003 in watch mode, though a clean build does).
        let stamps: Vec<Option<FileStamp>> = std::iter::once(&parsed.config_path)
            .chain(&parsed.extended_config_paths)
            .map(|p| fs.stamp(p))
            .collect();
        let no_inputs = parsed.diagnostics.iter().any(|d| d.code == 18003);
        match &parsed_at {
            Some((before, false)) if *before == stamps => {
                parsed.diagnostics.retain(|d| d.code != 18003);
            }
            _ => parsed_at = Some((stamps, no_inputs)),
        }
        let preserve = cl.flag(OptionId::PreserveWatchOutput)
            || parsed.options.preserve_watch_output() == Some(true);
        start(out, pretty, clears(cl, pretty, preserve), first);
        let compiled = build::compile(&host, &parsed, None, false, Some(&mut memory));
        print_compiled(out, &compiled, pretty, quiet, true);
        let written = std::mem::take(&mut *host.written.lock().unwrap_or_else(|e| e.into_inner()));
        fs.refresh(&written);
    };
    let mut roots = vec![cwd.to_owned()];
    if let Some(p) = &config_path {
        roots.push(path::directory_of(p).to_owned());
    }
    cycle(true, out);
    driver.looked(out, 0, &roots);
    while driver.wait() {
        if !fs.take_changes().is_empty() {
            cycle(false, out);
        }
        driver.looked(out, 0, &roots);
    }
    let _ = out.flush();
    0
}

/// A build's diagnostics, the pretty summary (`summary`: `-p` prints one) and the closing
/// status line.
fn print_compiled(out: &mut dyn Write, c: &Compiled, pretty: bool, quiet: bool, summary: bool) {
    let Rendered {
        text,
        errors,
        files,
    } = &c.rendered;
    let _ = write!(out, "{text}");
    if summary && pretty && *errors > 0 && !quiet {
        let _ = write!(out, "\n{}", build::summary(files, *errors));
    }
    let _ = write!(out, "{}", found_line(pretty, *errors));
    let _ = out.flush();
}

/// The `--build` host of a watch session: compilations one at a time, each recording what
/// its program asked under its project.
struct WatchHost<'a> {
    inner: BuildHost<'a>,
    fs: &'a TrackingFs,
    one_at_a_time: Mutex<()>,
}

impl Host for WatchHost<'_> {
    fn compile(&self, job: &Job) -> Compiled {
        let _one = self.one_at_a_time.lock().unwrap_or_else(|e| e.into_inner());
        let p = &job.config.config_path;
        self.fs.forget(Some(p), Part::Program);
        self.fs.set_owner(p, Part::Program);
        let c = self.inner.compile(job);
        self.fs.set_owner("", Part::Config);
        c
    }

    fn render(&self, diags: &[ts_r_config::ConfigDiagnostic]) -> Rendered {
        self.inner.render(diags)
    }

    fn pretty(&self) -> bool {
        self.inner.pretty
    }

    fn show(&self, path: &str) -> String {
        self.inner.show(path)
    }

    fn touched(&self, paths: &[String]) {
        self.inner.touched(paths);
    }
}

/// `-b -w`. Returns the exit status of the first build.
pub fn run_build(args: &Args, cwd: &str, pretty: bool, out: &mut dyn Write) -> u8 {
    let cl = &args.cl;
    let fs = TrackingFs::new(RealFs::new());
    let quiet = cl.flag(OptionId::Quiet);
    let mut inner = BuildHost::new(&fs, cwd, pretty, quiet, args.threads);
    inner.list_emitted = cl.flag(OptionId::ListEmittedFiles);
    inner.report = crate::explain::Flags::from_command_line(cl);
    inner.cache = Some(SourceCache::default());
    let host = WatchHost {
        inner,
        fs: &fs,
        one_at_a_time: Mutex::new(()),
    };
    let flags = build::build_flags(cl);
    let clear = clears(cl, pretty, cl.flag(OptionId::PreserveWatchOutput));
    let mut session = pbuild::Session::default();
    let mut driver = Driver::new(cl);
    let dirs = std::cell::RefCell::new(vec![cwd.to_owned()]);
    let mut cycle = |changed: Option<&BTreeSet<String>>, out: &mut dyn Write| -> u8 {
        fs.forget(None, Part::Config);
        fs.forget(None, Part::Roots);
        fs.set_owner("", Part::Config);
        let roots = build::build_roots(cl, &fs, cwd);
        let g = graph::build_graph(&fs, &roots, &mut |p| {
            fs.set_owner(p, Part::Config);
            let c = ConfigLoader::new(&fs).load_with_overrides(p, |o| merge_command_line(cl, o));
            fs.set_owner("", Part::Config);
            c
        });
        for node in &g.order {
            if let Some(c) = &node.config {
                record_roots(&fs, c);
            }
            let dir = path::directory_of(&node.config_path).to_owned();
            let mut dirs = dirs.borrow_mut();
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        fs.set_owner("", Part::Config);
        start(out, pretty, clear, changed.is_none());
        let report = pbuild::run_watch(
            &host,
            &fs,
            &g,
            flags,
            pbuild::Cycle {
                session: &mut session,
                changed,
            },
            out,
        );
        let _ = write!(out, "{}", found_line(pretty, report.errors));
        let _ = out.flush();
        let written =
            std::mem::take(&mut *host.inner.written.lock().unwrap_or_else(|e| e.into_inner()));
        fs.refresh(&written);
        build::exit_code(&report)
    };
    let status = cycle(None, out);
    driver.looked(out, status, &dirs.borrow());
    while driver.wait() {
        let changed = fs.take_changes();
        let mut code = status;
        if !changed.is_empty() {
            code = cycle(Some(&changed), out);
        }
        driver.looked(out, code, &dirs.borrow());
    }
    let _ = out.flush();
    status
}

/// A project's roots and persisted inputs, so an up-to-date project (no program loaded)
/// still rebuilds when one changes.
fn record_roots(fs: &TrackingFs, config: &ParsedConfig) {
    fs.set_owner(&config.config_path, Part::Roots);
    for f in &config.file_names {
        fs.stamp(f);
    }
    let file = buildinfo::path_for(&config.options, &config.config_path);
    if let Some(text) = fs.read_file(&file)
        && let buildinfo::Read::Ours(info) = buildinfo::parse(&text)
        && let Some(state) = info.program
    {
        for p in state
            .files
            .iter()
            .map(|f| &f.path)
            .chain(&state.package_jsons)
            .chain(&state.missing_package_jsons)
        {
            fs.stamp(&buildinfo::absolute(&file, p));
        }
    }
    fs.set_owner("", Part::Config);
}

#[cfg(test)]
mod tests {
    use super::*;

    ts_r_testing::cap_memory!();

    #[test]
    fn sibling_of_an_ancestor_is_not_an_input() {
        let root = std::env::temp_dir().join(format!("ts-r-watch-ancestor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("tsconfig.json"), "{}").unwrap();
        std::fs::write(project.join("a.ts"), "export const a = 1;").unwrap();
        let config = path::normalize_slashes(&project.join("tsconfig.json").to_string_lossy());
        let fs = TrackingFs::new(RealFs::new());
        let parsed = ts_r_config::load_tsconfig(&fs, &config);
        assert_eq!(parsed.file_names.len(), 1, "{:?}", parsed.file_names);
        assert!(fs.take_changes().is_empty());
        // Another project appearing next to this one (in any ancestor) changes nothing here.
        std::fs::create_dir_all(root.join("other")).unwrap();
        assert!(fs.take_changes().is_empty());
        std::fs::write(project.join("b.ts"), "export const b = 1;").unwrap();
        assert!(!fs.take_changes().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refresh_preserves_unrelated_directory_changes() {
        let root = std::env::temp_dir().join(format!("ts-r-watch-refresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for nested in [false, true] {
            for change in ["none", "create", "delete"] {
                let dir = root.join(format!("{nested}-{change}"));
                std::fs::create_dir_all(&dir).unwrap();
                let source = dir.join("b.ts");
                if change == "delete" {
                    std::fs::write(&source, "export const b = 1;").unwrap();
                }
                let dir = path::normalize_slashes(&dir.to_string_lossy());
                let output_dir = if nested {
                    path::combine(&dir, "out/nested")
                } else {
                    dir.clone()
                };
                let output = path::combine(&output_dir, "a.js");
                let fs = TrackingFs::new(RealFs::new());
                fs.set_owner("project", Part::Config);
                fs.read_dir(&dir);
                fs.read_dir(&output_dir);
                fs.directory_exists(&output_dir);
                match change {
                    "create" => std::fs::write(&source, "export const b = 1;").unwrap(),
                    "delete" => std::fs::remove_file(&source).unwrap(),
                    _ => {}
                }
                std::fs::create_dir_all(&output_dir).unwrap();
                std::fs::write(&output, "export const a = 1;").unwrap();
                fs.refresh(&[output]);
                let expected = if change == "none" {
                    BTreeSet::new()
                } else {
                    BTreeSet::from(["project".to_owned()])
                };
                assert_eq!(fs.take_changes(), expected, "{nested}-{change}");
                assert!(fs.take_changes().is_empty());
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
