//! The parity check (plan: D6, R13, P8, K-17).
//!
//! The first time a project opens, a low-priority background job checks it twice, from disk:
//! with our checker (in this process, one thread) and with the user's own TypeScript
//! (`tsc -p <config> --noEmit --pretty false`, `nice`d, memory- and time-capped). Matching
//! means the same multiset of `(file, line, column, code)`. The result is cached by the
//! config's inputs (the tsconfig and everything it extends), the nearest lockfile, the
//! TypeScript version and the ts-r version, so it runs once per project until one of those
//! changes. A mismatch switches the project's type diagnostics to the user's TypeScript
//! (`fallback.rs`) and writes a report (`<cache>/lsp/parity/<id>.md`) that can be attached
//! to an issue as is.
//!
//! Jobs run one at a time on one worker thread.

use super::{CMsg, ConfigInfo};
use crate::backend::typescript::{self, TsEntry};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use ts_r_checker::{CheckRun, check_program, load_program};
use ts_r_config::{FileSystem, RealFs};
use ts_r_diagnostics::{Files, format_plain};
use ts_r_span::SourceText;

/// Bumped when the comparison changes (old cached results are ignored).
const PARITY_VERSION: u32 = 1;
/// Memory cap of the user's tsc (whole process tree).
const TSC_MEM_MB: u64 = 4096;
/// Time cap of the user's tsc.
const TSC_TIME: Duration = Duration::from_secs(600);
/// Differences listed in the status (the report has all of them).
const LISTED: usize = 20;

/// Outcome of a comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Result {
    /// Same diagnostics.
    Match,
    /// Different diagnostics: the project falls back.
    Mismatch,
    /// Could not compare (no TypeScript, tsc crashed or hit a limit, ...): our checker stays.
    Error,
}

impl Result {
    /// Wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Result::Match => "match",
            Result::Mismatch => "mismatch",
            Result::Error => "error",
        }
    }

    fn parse(s: &str) -> Option<Result> {
        match s {
            "match" => Some(Result::Match),
            "mismatch" => Some(Result::Mismatch),
            "error" => Some(Result::Error),
            _ => None,
        }
    }
}

/// What the status shows about a project's parity check.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    /// Project name (config path relative to the root).
    pub project: String,
    /// Result.
    pub result: Result,
    /// One line for people.
    pub detail: String,
    /// The user's TypeScript, e.g. `6.0.3`.
    pub typescript: String,
    /// Diagnostics we report / they report.
    pub ours: usize,
    pub theirs: usize,
    /// Differences (first [`LISTED`]), as `file(line,col): TScode`.
    pub only_ours: Vec<String>,
    pub only_theirs: Vec<String>,
    /// Full report (markdown) for an issue.
    pub report: Option<String>,
    /// Came from the cache.
    pub cached: bool,
    /// Time of our check and of theirs.
    pub ours_ms: f64,
    pub theirs_ms: f64,
}

impl Summary {
    /// JSON for `ts-r/status`.
    pub fn to_json(&self) -> Value {
        json!({
            "project": self.project,
            "result": self.result.as_str(),
            "detail": self.detail,
            "typescript": self.typescript,
            "ours": self.ours,
            "theirs": self.theirs,
            "onlyOurs": self.only_ours,
            "onlyTheirs": self.only_theirs,
            "report": self.report,
            "cached": self.cached,
            "oursMs": self.ours_ms,
            "theirsMs": self.theirs_ms,
        })
    }

    fn from_json(v: &Value) -> Option<Summary> {
        let strings = |k: &str| -> Vec<String> {
            v[k].as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        };
        Some(Summary {
            project: v["project"].as_str()?.to_string(),
            result: Result::parse(v["result"].as_str()?)?,
            detail: v["detail"].as_str().unwrap_or("").to_string(),
            typescript: v["typescript"].as_str().unwrap_or("").to_string(),
            ours: v["ours"].as_u64().unwrap_or(0) as usize,
            theirs: v["theirs"].as_u64().unwrap_or(0) as usize,
            only_ours: strings("onlyOurs"),
            only_theirs: strings("onlyTheirs"),
            report: v["report"].as_str().map(str::to_string),
            cached: true,
            ours_ms: v["oursMs"].as_f64().unwrap_or(0.0),
            theirs_ms: v["theirsMs"].as_f64().unwrap_or(0.0),
        })
    }
}

/// A finished job.
#[derive(Debug)]
pub struct Outcome {
    /// Config path (the project key).
    pub config: String,
    /// Result.
    pub summary: Summary,
}

struct Job {
    config: Arc<ConfigInfo>,
    tx: Sender<CMsg>,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    stop: bool,
}

/// Runs parity jobs one at a time.
pub struct Runner {
    root: PathBuf,
    queue: Arc<(Mutex<Queue>, Condvar)>,
    started: bool,
    cancel: Arc<AtomicBool>,
}

impl std::fmt::Debug for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runner").field("root", &self.root).finish()
    }
}

impl Runner {
    /// A runner for a workspace.
    pub fn new(root: PathBuf) -> Runner {
        Runner {
            root,
            queue: Arc::new((Mutex::new(Queue::default()), Condvar::new())),
            started: false,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Checks `config` (answering from the cache at once when possible).
    pub fn request(&mut self, config: Arc<ConfigInfo>, tx: Sender<CMsg>) {
        if disabled() {
            return;
        }
        let key = cache_key(&self.root, &config);
        if let Some(s) = key.as_ref().and_then(|k| read_cached(k)) {
            let _ = tx.send(CMsg::Parity(Box::new(Outcome {
                config: config.path.clone(),
                summary: s,
            })));
            return;
        }
        {
            let (m, cv) = &*self.queue;
            let mut q = m.lock().unwrap_or_else(|e| e.into_inner());
            q.jobs.push_back(Job { config, tx });
            cv.notify_one();
        }
        if !self.started {
            self.started = true;
            let queue = self.queue.clone();
            let root = self.root.clone();
            let cancel = self.cancel.clone();
            let _ = thread::Builder::new()
                .name("tsr-parity".into())
                .stack_size(256 << 20)
                .spawn(move || worker(root, queue, cancel));
        }
    }

    /// Stops the worker (a running tsc is killed).
    pub fn stop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        let (m, cv) = &*self.queue;
        let mut q = m.lock().unwrap_or_else(|e| e.into_inner());
        q.stop = true;
        q.jobs.clear();
        cv.notify_all();
    }
}

/// `TS_R_LSP_PARITY=off` disables the check (tests, or users who do not want tsc run).
fn disabled() -> bool {
    std::env::var("TS_R_LSP_PARITY").is_ok_and(|v| v == "off" || v == "0")
}

fn worker(root: PathBuf, queue: Arc<(Mutex<Queue>, Condvar)>, cancel: Arc<AtomicBool>) {
    loop {
        let job = {
            let (m, cv) = &*queue;
            let mut q = m.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if q.stop {
                    return;
                }
                if let Some(j) = q.jobs.pop_front() {
                    break j;
                }
                q = cv.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        let summary = run(&root, &job.config, &cancel);
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        if summary.result != Result::Error
            && let Some(k) = cache_key(&root, &job.config)
        {
            write_cached(&k, &summary);
        }
        let _ = job.tx.send(CMsg::Parity(Box::new(Outcome {
            config: job.config.path.clone(),
            summary,
        })));
    }
}

/// Relative display path.
fn rel(root: &Path, p: &str) -> String {
    let r = root.to_string_lossy().replace('\\', "/");
    p.strip_prefix(&format!("{}/", r.trim_end_matches('/')))
        .unwrap_or(p)
        .to_string()
}

/// One comparison.
pub fn run(root: &Path, config: &ConfigInfo, cancel: &AtomicBool) -> Summary {
    let project = rel(root, &config.path);
    let mut s = Summary {
        project: project.clone(),
        result: Result::Error,
        detail: String::new(),
        typescript: String::new(),
        ours: 0,
        theirs: 0,
        only_ours: Vec::new(),
        only_theirs: Vec::new(),
        report: None,
        cached: false,
        ours_ms: 0.0,
        theirs_ms: 0.0,
    };
    let dir = Path::new(&config.path).parent().unwrap_or(root);
    let Some(install) =
        typescript::find_typescript(dir).or_else(|| typescript::find_typescript(root))
    else {
        s.detail = "no TypeScript in node_modules to compare with".into();
        return s;
    };
    s.typescript = install.version.clone();
    // Theirs first: if it cannot run there is nothing to compare.
    let t = Instant::now();
    let theirs = match run_tsc(root, &install.entry, config, cancel) {
        Ok(out) => out,
        Err(e) => {
            s.detail = format!(
                "TypeScript {} could not check the project: {e}",
                install.version
            );
            return s;
        }
    };
    s.theirs_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let ours = our_lines(root, config);
    s.ours_ms = t.elapsed().as_secs_f64() * 1000.0;
    let a = keys(&ours);
    let b = keys(&theirs);
    s.ours = a.values().sum();
    s.theirs = b.values().sum();
    let mut only_ours = Vec::new();
    let mut only_theirs = Vec::new();
    for (k, &n) in &a {
        let m = b.get(k).copied().unwrap_or(0);
        for _ in m..n {
            only_ours.push(k.clone());
        }
    }
    for (k, &n) in &b {
        let m = a.get(k).copied().unwrap_or(0);
        for _ in m..n {
            only_theirs.push(k.clone());
        }
    }
    if only_ours.is_empty() && only_theirs.is_empty() {
        s.result = Result::Match;
        s.detail = format!(
            "same {} diagnostic(s) as TypeScript {}",
            s.ours, install.version
        );
        return s;
    }
    s.result = Result::Mismatch;
    s.detail = format!(
        "{} diagnostic(s) only ts-r reports, {} only TypeScript {} reports",
        only_ours.len(),
        only_theirs.len(),
        install.version
    );
    s.report = write_report(
        root,
        config,
        &s,
        &install,
        &only_ours,
        &only_theirs,
        &ours,
        &theirs,
    );
    s.only_ours = only_ours.into_iter().take(LISTED).collect();
    s.only_theirs = only_theirs.into_iter().take(LISTED).collect();
    s
}

/// Our diagnostics for the project from disk, as tsc prints them (`--pretty false`,
/// paths relative to the root).
pub fn our_lines(root: &Path, config: &ConfigInfo) -> Vec<String> {
    let fs = RealFs::new();
    let root_s = root.to_string_lossy().replace('\\', "/");
    let mut out = Vec::new();
    let parsed = ts_r_config::load_tsconfig(&fs, &config.path);
    for d in &parsed.diagnostics {
        out.push(d.render(|p| rel(root, p), |p| fs.read_file(p)));
    }
    let Some(libs) = super::libs() else {
        return out;
    };
    let program = load_program(&fs, &parsed.file_names, &parsed.options, Some(&libs), 1);
    let result = check_program(&program, CheckRun { threads: 1 });
    struct Shown(Vec<Option<SourceText>>);
    impl Files for Shown {
        fn source(&self, file: ts_r_diagnostics::FileId) -> Option<&SourceText> {
            self.0.get(file.0 as usize).and_then(|s| s.as_ref())
        }
    }
    let shown = Shown(
        program
            .files
            .iter()
            .map(|f| {
                (!f.is_lib)
                    .then(|| SourceText::new(rel(Path::new(&root_s), &f.path), f.text.clone()))
            })
            .collect(),
    );
    for d in &result.diagnostics {
        out.push(format_plain(d, &shown));
    }
    out
}

/// Runs the user's tsc on the project, capped; returns its output lines.
fn run_tsc(
    root: &Path,
    entry: &TsEntry,
    config: &ConfigInfo,
    cancel: &AtomicBool,
) -> std::result::Result<Vec<String>, String> {
    let mut argv: Vec<String> = match override_cmd() {
        Some(v) => v,
        None => match entry {
            TsEntry::TsServer(js) => {
                let tsc = js.with_file_name("tsc.js");
                let node = typescript::find_node().ok_or("node not found")?;
                vec![
                    node.to_string_lossy().into_owned(),
                    tsc.to_string_lossy().into_owned(),
                ]
            }
            TsEntry::Native(exe) => vec![exe.to_string_lossy().into_owned()],
        },
    };
    argv.extend([
        "-p".into(),
        config.path.clone(),
        "--noEmit".into(),
        "--pretty".into(),
        "false".into(),
    ]);
    if config.options.incremental() == Some(true) || config.options.composite() == Some(true) {
        // Never write the project's build info: the editor must not touch the repo.
        let tmp =
            std::env::temp_dir().join(format!("ts-r-parity-{}.tsbuildinfo", std::process::id()));
        argv.extend([
            "--tsBuildInfoFile".into(),
            tmp.to_string_lossy().into_owned(),
        ]);
    }
    let nice = Path::new("/usr/bin/nice");
    let mut cmd = if cfg!(unix) && nice.is_file() {
        let mut c = Command::new(nice);
        c.arg("-n").arg("10").args(&argv);
        c
    } else {
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]);
        c
    };
    cmd.current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", argv[0]))?;
    let pid = child.id();
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let reader = thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut s);
        s
    });
    let start = Instant::now();
    let mut last_sample = Instant::now() - Duration::from_secs(1);
    let status = loop {
        if let Ok(Some(st)) = child.try_wait() {
            break st;
        }
        let killed = |why: String, child: &mut std::process::Child| {
            let snap = crate::process::Snapshot::take();
            for p in snap.tree(pid).into_iter().rev() {
                let _ = Command::new("kill").arg("-9").arg(p.to_string()).status();
            }
            let _ = child.kill();
            let _ = child.wait();
            Err(why)
        };
        if cancel.load(Ordering::Relaxed) {
            return killed("cancelled".into(), &mut child);
        }
        if start.elapsed() > TSC_TIME {
            return killed(
                format!("ran longer than {} s", TSC_TIME.as_secs()),
                &mut child,
            );
        }
        if last_sample.elapsed() >= Duration::from_millis(250) {
            last_sample = Instant::now();
            if let Some(rss) = crate::process::tree_rss_bytes(pid)
                && rss > TSC_MEM_MB * 1024 * 1024
            {
                return killed(format!("used more than {TSC_MEM_MB} MB"), &mut child);
            }
        }
        thread::sleep(Duration::from_millis(20));
    };
    let text = reader.join().unwrap_or_default();
    // tsc exits 0 (no errors), 1 or 2 (errors); anything else is a crash.
    match status.code() {
        Some(0..=2) => Ok(text.lines().map(str::to_string).collect()),
        other => Err(format!("exited with {other:?}")),
    }
}

/// `TS_R_LSP_PARITY_TSC`: JSON argv replacing the user's tsc (tests).
fn override_cmd() -> Option<Vec<String>> {
    let raw = std::env::var("TS_R_LSP_PARITY_TSC").ok()?;
    let v: Vec<String> = serde_json::from_str(&raw).ok()?;
    (!v.is_empty()).then_some(v)
}

/// Comparison keys of diagnostic lines: `file(line,col): TScode`, or `TScode` for global
/// ones. Continuation lines (message chains) are skipped.
pub fn keys(lines: &[String]) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for l in lines {
        if l.starts_with(' ') || l.is_empty() {
            continue;
        }
        let Some(k) = key_of(l) else { continue };
        *out.entry(k).or_insert(0) += 1;
    }
    out
}

fn key_of(line: &str) -> Option<String> {
    let code_at = line.find(" TS")?;
    let code: String = line[code_at + 3..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if code.is_empty() {
        return None;
    }
    let head = &line[..code_at];
    // `path(line,col): error` or `error`.
    let loc = ["error", "warning", "message"]
        .iter()
        .find_map(|w| head.strip_suffix(w))
        .unwrap_or(head)
        .trim_end()
        .trim_end_matches(':');
    if loc.is_empty() {
        return Some(format!("TS{code}"));
    }
    Some(format!("{loc}: TS{code}"))
}

/// The cache key of a project's parity result (`None` when the config cannot be read).
fn cache_key(root: &Path, config: &ConfigInfo) -> Option<String> {
    let mut h = blake3::Hasher::new();
    h.update(
        format!(
            "ts-r-parity {PARITY_VERSION} {}\0",
            env!("CARGO_PKG_VERSION")
        )
        .as_bytes(),
    );
    h.update(rel(root, &config.path).as_bytes());
    for input in &config.inputs {
        h.update(b"\0");
        h.update(rel(root, input).as_bytes());
        h.update(&std::fs::read(input).ok()?);
    }
    let dir = Path::new(&config.path).parent()?;
    match lockfile(dir) {
        Some((p, bytes)) => {
            h.update(rel(root, &p.to_string_lossy()).as_bytes());
            h.update(&bytes);
        }
        None => {
            h.update(b"no lockfile");
        }
    }
    if let Some(ts) = typescript::find_typescript(dir) {
        h.update(ts.version.as_bytes());
    }
    Some(h.finalize().to_hex()[..32].to_string())
}

/// The nearest lockfile upwards from `dir`.
fn lockfile(dir: &Path) -> Option<(PathBuf, Vec<u8>)> {
    for d in dir.ancestors() {
        for name in [
            "pnpm-lock.yaml",
            "package-lock.json",
            "yarn.lock",
            "bun.lock",
            "bun.lockb",
            "npm-shrinkwrap.json",
        ] {
            let p = d.join(name);
            if let Ok(b) = std::fs::read(&p) {
                return Some((p, b));
            }
        }
    }
    None
}

fn parity_dir() -> PathBuf {
    ts_r_cache::cache_root().join("lsp").join("parity")
}

fn read_cached(key: &str) -> Option<Summary> {
    let text = std::fs::read_to_string(parity_dir().join(format!("{key}.json"))).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    Summary::from_json(&v)
}

fn write_cached(key: &str, s: &Summary) {
    let dir = parity_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let tmp = dir.join(format!("{key}.json.tmp{}", std::process::id()));
    if std::fs::write(&tmp, s.to_json().to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join(format!("{key}.json")));
    }
}

#[allow(clippy::too_many_arguments)]
fn write_report(
    root: &Path,
    config: &ConfigInfo,
    s: &Summary,
    install: &typescript::TsInstall,
    only_ours: &[String],
    only_theirs: &[String],
    ours: &[String],
    theirs: &[String],
) -> Option<String> {
    let dir = parity_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let id = blake3::hash(format!("{}{}", root.display(), config.path).as_bytes()).to_hex()[..16]
        .to_string();
    let path = dir.join(format!("{id}.md"));
    let mut r = String::new();
    let line = |r: &mut String, s: &str| {
        r.push_str(s);
        r.push('\n');
    };
    line(&mut r, &format!("# ts-r parity difference: {}", s.project));
    line(&mut r, "");
    line(
        &mut r,
        &format!(
            "- ts-r: {} ({}, {})",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    );
    line(
        &mut r,
        &format!(
            "- TypeScript: {} ({})",
            install.version,
            install.package_dir.display()
        ),
    );
    line(&mut r, &format!("- Project: `{}`", s.project));
    line(
        &mut r,
        &format!("- Diagnostics: ts-r {} / TypeScript {}", s.ours, s.theirs),
    );
    line(
        &mut r,
        &format!(
            "- Only ts-r: {} / only TypeScript: {}",
            only_ours.len(),
            only_theirs.len()
        ),
    );
    line(&mut r, "");
    line(&mut r, "Reproduce from the workspace root:");
    line(&mut r, "");
    line(&mut r, "```sh");
    line(
        &mut r,
        &format!("ts-r-check -p {} --noEmit --pretty false", s.project),
    );
    line(
        &mut r,
        &format!("npx tsc -p {} --noEmit --pretty false", s.project),
    );
    line(&mut r, "```");
    line(&mut r, "");
    line(
        &mut r,
        "Diagnostics are compared as `file(line,col): code`. Message texts are not compared.",
    );
    line(&mut r, "");
    for (title, list, full) in [
        ("Only ts-r reports", only_ours, ours),
        ("Only TypeScript reports", only_theirs, theirs),
    ] {
        line(&mut r, &format!("## {title} ({})", list.len()));
        line(&mut r, "");
        line(&mut r, "```text");
        for k in list {
            // The full line (with message) of the first diagnostic with this key.
            let full_line = full
                .iter()
                .find(|l| key_of(l).as_deref() == Some(k.as_str()));
            line(&mut r, full_line.map_or(k.as_str(), String::as_str));
        }
        line(&mut r, "```");
        line(&mut r, "");
    }
    line(
        &mut r,
        "Source files are not included. Add a minimal reproduction if you can.",
    );
    std::fs::write(&path, r).ok()?;
    Some(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_from_tsc_lines() {
        let lines: Vec<String> = [
            "src/a.ts(3,7): error TS2322: Type 'string' is not assignable to type 'number'.",
            "  Type 'x' is not assignable.",
            "error TS5102: Option 'baseUrl' has been removed.",
            "tsconfig.json(2,5): error TS5023: Unknown compiler option 'x'.",
            "src/a.ts(3,7): error TS2322: again",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let k = keys(&lines);
        assert_eq!(k.get("src/a.ts(3,7): TS2322"), Some(&2));
        assert_eq!(k.get("TS5102"), Some(&1));
        assert_eq!(k.get("tsconfig.json(2,5): TS5023"), Some(&1));
        assert_eq!(k.len(), 3);
    }
}
