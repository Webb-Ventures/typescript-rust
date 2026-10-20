//! Native watching of the ts-r backend's project inputs. The daemon owns it, so a change on
//! disk reaches the checker whatever the editor watches (VS Code's bridge registers nothing;
//! other clients may also send `workspace/didChangeWatchedFiles`, which is handled the same
//! way).
//!
//! The coordinator hands over directories (those of each project's tsconfig chain, root files
//! and resolved inputs, wherever they are); each is watched non-recursively, so the cost is one
//! watch per source directory and `node_modules` is never walked. Native events (`notify`:
//! FSEvents, inotify, ReadDirectoryChangesW) go through a [`Debouncer`] on the watch thread,
//! and each settled batch is *sent* to the coordinator's loop ([`super::CMsg::Watched`]): the
//! watch thread never touches backend state.
//!
//! Patterns from `ts_r_hmr`'s watcher (`crates/ts_r_hmr/AGENTS.md`): a burst settles after a
//! quiet window; a file created and gone within one burst (an editor's temp file) is dropped;
//! a file missing when its burst settles is re-checked after a grace period (backup-rename
//! saves), and FSEvents' replay of writes from before a directory's watch began is dropped
//! during [`STARTUP_WINDOW`] (macOS only; removals and coarse mtimes always count).

use notify::{EventKind, RecursiveMode, Watcher as _};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Debounce timing: flush once no event arrived for `QUIET`, at the latest `MAX_WAIT` after a
/// burst's first event; a missing file gets `GRACE` to come back.
pub const QUIET: Duration = Duration::from_millis(30);
pub const MAX_WAIT: Duration = Duration::from_millis(300);
pub const GRACE: Duration = Duration::from_millis(50);
/// How long after a directory's watch starts FSEvents replays are filtered.
pub const STARTUP_WINDOW: Duration = Duration::from_secs(10);
/// At most this many watched directories per backend (inotify's per-user limit is often 8192).
pub const MAX_DIRS: usize = 4096;

/// A settled change, as `workspace/didChangeWatchedFiles` types it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Created,
    Changed,
    Deleted,
}

impl ChangeKind {
    /// The LSP `FileChangeType`.
    pub fn lsp(self) -> u64 {
        match self {
            ChangeKind::Created => 1,
            ChangeKind::Changed => 2,
            ChangeKind::Deleted => 3,
        }
    }
}

/// Paths never watched or reported: dependency, VCS and build directories.
pub fn ignored(path: &str) -> bool {
    path.split(['/', '\\']).any(|c| {
        matches!(
            c,
            "node_modules" | ".git" | ".hg" | ".svn" | "target" | ".DS_Store"
        )
    })
}

/// Whether a changed file can matter to a project: sources, declarations, JSON (configs,
/// `package.json`, resolved JSON modules) and `ts-r.toml`.
pub fn relevant(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    name == "ts-r.toml"
        || [
            ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs", ".json", ".jsonc",
        ]
        .iter()
        .any(|e| lower.ends_with(e))
}

/// Native event classification fed to the [`Debouncer`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawKind {
    Create,
    Modify,
    Remove,
}

/// What the file system says about a path when its burst settles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileState {
    Missing,
    File,
    Dir,
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    saw_create: bool,
    grace_until: Option<Instant>,
}

/// The pure part of the watcher: collects raw events and decides when and what to flush.
#[derive(Debug, Default)]
pub struct Debouncer {
    pending: BTreeMap<PathBuf, Pending>,
    burst_start: Option<Instant>,
    last_event: Option<Instant>,
}

impl Debouncer {
    pub fn event(&mut self, path: PathBuf, kind: RawKind, now: Instant) {
        self.burst_start.get_or_insert(now);
        self.last_event = Some(now);
        let p = self.pending.entry(path).or_insert(Pending {
            saw_create: false,
            grace_until: None,
        });
        p.grace_until = None;
        if kind == RawKind::Create {
            p.saw_create = true;
        }
    }

    /// When [`Debouncer::poll`] should next run (`None`: nothing pending).
    pub fn deadline(&self) -> Option<Instant> {
        let burst = match (self.burst_start, self.last_event) {
            (Some(start), Some(last)) => Some((last + QUIET).min(start + MAX_WAIT)),
            _ => None,
        };
        let grace = self.pending.values().filter_map(|p| p.grace_until).min();
        match (burst, grace) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The changes ready at `now` (path, kind, whether it is a directory).
    pub fn poll(
        &mut self,
        now: Instant,
        probe: impl Fn(&Path) -> FileState,
    ) -> Vec<(PathBuf, ChangeKind, bool)> {
        let settled = match (self.burst_start, self.last_event) {
            (Some(start), Some(last)) => now >= last + QUIET || now >= start + MAX_WAIT,
            _ => false,
        };
        if settled {
            self.burst_start = None;
            self.last_event = None;
        }
        let mut out = Vec::new();
        let mut done = Vec::new();
        for (path, p) in self.pending.iter_mut() {
            let ready = match p.grace_until {
                Some(g) => now >= g,
                None => settled,
            };
            if !ready {
                continue;
            }
            let state = probe(path);
            let change = match state {
                FileState::File | FileState::Dir if p.saw_create => Some(ChangeKind::Created),
                FileState::File | FileState::Dir => Some(ChangeKind::Changed),
                // Created and gone within one burst: an editor's temp file.
                FileState::Missing if p.saw_create => None,
                FileState::Missing if p.grace_until.is_none() => {
                    p.grace_until = Some(now + GRACE);
                    continue;
                }
                FileState::Missing => Some(ChangeKind::Deleted),
            };
            done.push(path.clone());
            if let Some(kind) = change {
                out.push((path.clone(), kind, state == FileState::Dir));
            }
        }
        for d in done {
            self.pending.remove(&d);
        }
        out
    }
}

/// Whether a file with this mtime was last written before a watch that started at `started`
/// (as in `ts_r_hmr`: an mtime without a sub-second part comes from a coarse file system and
/// never predates).
pub fn predates_watch(mtime: SystemTime, started: SystemTime) -> bool {
    mtime < started
        && mtime
            .duration_since(UNIX_EPOCH)
            .is_ok_and(|d| d.subsec_nanos() != 0)
}

fn probe(path: &Path) -> FileState {
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => FileState::Dir,
        Ok(_) => FileState::File,
        Err(_) => FileState::Missing,
    }
}

/// What the watch thread receives.
enum Msg {
    Dirs(Vec<String>),
    /// A directory created in a watched one: watch it and its subdirectories, then report it
    /// as created once more, so files made there before the watch began are found by the
    /// config reload that follows.
    NewDir(String),
    Event(PathBuf, RawKind),
}

/// Subdirectories of a new directory watched with it (depth and count bounded).
const NEW_DIR_DEPTH: usize = 8;
const NEW_DIR_MAX: usize = 256;

/// A watched directory: the path as the coordinator named it (events carry the real path,
/// e.g. `/private/tmp/...` for `/tmp/...` on macOS) and when its watch began.
struct Dir {
    named: String,
    since: SystemTime,
    since_at: Instant,
}

/// A running watcher; dropping it stops the thread (which owns the native watcher).
pub struct InputWatch {
    tx: Sender<Msg>,
    stop: Arc<AtomicBool>,
}

impl std::fmt::Debug for InputWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputWatch").finish_non_exhaustive()
    }
}

impl Drop for InputWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl InputWatch {
    /// Starts the watch thread; `sink` gets each settled batch as (path, kind).
    pub fn start(sink: impl Fn(Vec<(String, ChangeKind)>) + Send + 'static) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel::<Msg>();
        let events = tx.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            let kind = match event.kind {
                EventKind::Create(_) => RawKind::Create,
                EventKind::Remove(_) => RawKind::Remove,
                EventKind::Modify(notify::event::ModifyKind::Metadata(_))
                | EventKind::Access(_)
                | EventKind::Other => return,
                EventKind::Modify(_) | EventKind::Any => RawKind::Modify,
            };
            for path in event.paths {
                let _ = events.send(Msg::Event(path, kind));
            }
        })
        .map_err(io::Error::other)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        thread::Builder::new()
            .name("tsr-watch".into())
            .spawn(move || run(watcher, rx, &stopped, sink))?;
        Ok(InputWatch { tx, stop })
    }

    /// Watches these directories too (non-recursively); already watched ones are skipped.
    pub fn add_dirs(&self, dirs: Vec<String>) {
        if !dirs.is_empty() {
            let _ = self.tx.send(Msg::Dirs(dirs));
        }
    }

    /// A directory was created in a watched one (see [`Msg::NewDir`]).
    pub fn add_new_dir(&self, dir: String) {
        let _ = self.tx.send(Msg::NewDir(dir));
    }
}

/// `dir` and its subdirectories (not ignored ones), breadth first, bounded.
fn subdirs(dir: &str) -> Vec<String> {
    let mut out = vec![dir.trim_end_matches('/').to_string()];
    let mut level = out.clone();
    for _ in 0..NEW_DIR_DEPTH {
        let mut next = Vec::new();
        for d in &level {
            let Ok(entries) = std::fs::read_dir(d) else {
                continue;
            };
            for e in entries.flatten() {
                if !e.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let Some(name) = e.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                let p = format!("{d}/{name}");
                if !ignored(&name) && out.len() < NEW_DIR_MAX {
                    out.push(p.clone());
                    next.push(p);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        level = next;
    }
    out
}

/// The watch thread's state.
struct Watching {
    watcher: notify::RecommendedWatcher,
    /// Real path of each watched directory -> how the coordinator named it.
    dirs: HashMap<PathBuf, Dir>,
    named: HashSet<String>,
    warned: bool,
}

impl Watching {
    /// Watches `dir`; whether it is newly watched.
    fn add(&mut self, dir: String) -> bool {
        if self.named.contains(&dir) || ignored(&dir) {
            return false;
        }
        if self.named.len() >= MAX_DIRS {
            if !self.warned {
                self.warned = true;
                crate::daemon::log(&format!(
                    "ts-r: watching the first {MAX_DIRS} input directories only"
                ));
            }
            return false;
        }
        let p = Path::new(&dir);
        if !p.is_dir() {
            return false;
        }
        // Taken before the watch starts: a later write is never taken for a replay.
        let since = SystemTime::now();
        let since_at = Instant::now();
        if let Err(e) = self.watcher.watch(p, RecursiveMode::NonRecursive) {
            crate::daemon::log(&format!("ts-r: cannot watch {dir}: {e}"));
            return false;
        }
        let real = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        self.named.insert(dir.clone());
        self.dirs.insert(
            real,
            Dir {
                named: dir,
                since,
                since_at,
            },
        );
        true
    }
}

fn run(
    watcher: notify::RecommendedWatcher,
    rx: mpsc::Receiver<Msg>,
    stopped: &AtomicBool,
    sink: impl Fn(Vec<(String, ChangeKind)>),
) {
    let mut w = Watching {
        watcher,
        dirs: HashMap::new(),
        named: HashSet::new(),
        warned: false,
    };
    let mut d = Debouncer::default();
    loop {
        if stopped.load(Ordering::SeqCst) {
            return;
        }
        let msg = match d.deadline() {
            // Idle: wake up now and then to notice the watcher being dropped.
            None => rx.recv_timeout(Duration::from_millis(250)),
            Some(at) => rx.recv_timeout(at.saturating_duration_since(Instant::now())),
        };
        let mut rescan: Vec<(String, ChangeKind)> = Vec::new();
        let mut on = |m: Msg, d: &mut Debouncer| match m {
            Msg::Event(path, kind) => {
                if !ignored(&path.to_string_lossy()) {
                    d.event(path, kind, Instant::now());
                }
            }
            Msg::Dirs(list) => {
                for dir in list {
                    w.add(dir);
                }
            }
            Msg::NewDir(dir) => {
                let mut added = false;
                for sub in subdirs(&dir) {
                    added |= w.add(sub);
                }
                if added {
                    rescan.push((dir, ChangeKind::Created));
                }
            }
        };
        match msg {
            Ok(m) => {
                on(m, &mut d);
                while let Ok(m) = rx.try_recv() {
                    on(m, &mut d);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if !rescan.is_empty() {
            sink(rescan);
        }
        let now = Instant::now();
        if d.deadline().is_none_or(|at| at > now) {
            continue;
        }
        let mut out = Vec::new();
        for (path, kind, is_dir) in d.poll(now, probe) {
            let parent = path.parent().and_then(|p| w.dirs.get(p));
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // A new or removed directory can change a project's globbed files; any other
            // file only if a project could read it.
            let wanted = relevant(name)
                || (is_dir && kind == ChangeKind::Created)
                || (kind == ChangeKind::Deleted && !name.contains('.'));
            if !wanted {
                continue;
            }
            // Only FSEvents replays; inotify and ReadDirectoryChangesW start empty.
            if cfg!(target_os = "macos")
                && kind != ChangeKind::Deleted
                && let Some(dir) = parent
                && now < dir.since_at + STARTUP_WINDOW
                && std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .is_ok_and(|m| predates_watch(m, dir.since))
            {
                continue;
            }
            let reported = match parent {
                Some(dir) => format!("{}/{name}", dir.named.trim_end_matches('/')),
                None => ts_r_config::path::normalize_slashes(&path.to_string_lossy()),
            };
            out.push((reported, kind));
        }
        if !out.is_empty() {
            sink(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fs(RefCell<HashMap<PathBuf, FileState>>);

    impl Fs {
        fn new() -> Fs {
            Fs(RefCell::new(HashMap::new()))
        }
        fn set(&self, p: &str, s: FileState) {
            self.0.borrow_mut().insert(PathBuf::from(p), s);
        }
        fn probe(&self) -> impl Fn(&Path) -> FileState + '_ {
            |p| {
                self.0
                    .borrow()
                    .get(p)
                    .copied()
                    .unwrap_or(FileState::Missing)
            }
        }
    }

    fn names(v: &[(PathBuf, ChangeKind, bool)]) -> Vec<(String, ChangeKind)> {
        v.iter()
            .map(|(p, k, _)| (p.to_string_lossy().into_owned(), *k))
            .collect()
    }

    #[test]
    fn a_burst_settles_once_quiet() {
        let fs = Fs::new();
        fs.set("/p/b.ts", FileState::File);
        let mut d = Debouncer::default();
        let t0 = Instant::now();
        d.event("/p/b.ts".into(), RawKind::Modify, t0);
        d.event(
            "/p/b.ts".into(),
            RawKind::Modify,
            t0 + Duration::from_millis(10),
        );
        assert!(
            d.poll(t0 + Duration::from_millis(20), fs.probe())
                .is_empty()
        );
        let out = d.poll(t0 + Duration::from_millis(41), fs.probe());
        assert_eq!(names(&out), vec![("/p/b.ts".into(), ChangeKind::Changed)]);
        assert_eq!(d.deadline(), None);
    }

    #[test]
    fn atomic_save_reports_the_target_only() {
        let fs = Fs::new();
        fs.set("/p/b.ts", FileState::File);
        let mut d = Debouncer::default();
        let t0 = Instant::now();
        d.event("/p/b.ts.tmp".into(), RawKind::Create, t0);
        d.event("/p/b.ts.tmp".into(), RawKind::Remove, t0);
        d.event("/p/b.ts".into(), RawKind::Modify, t0);
        let out = d.poll(t0 + QUIET, fs.probe());
        assert_eq!(names(&out), vec![("/p/b.ts".into(), ChangeKind::Changed)]);
    }

    #[test]
    fn a_missing_file_gets_a_grace_period() {
        let fs = Fs::new();
        let mut d = Debouncer::default();
        let t0 = Instant::now();
        d.event("/p/b.ts".into(), RawKind::Remove, t0);
        assert!(d.poll(t0 + QUIET, fs.probe()).is_empty());
        // Recreated in time: a change, not a removal.
        fs.set("/p/b.ts", FileState::File);
        let out = d.poll(t0 + QUIET + GRACE, fs.probe());
        assert_eq!(names(&out), vec![("/p/b.ts".into(), ChangeKind::Changed)]);
        // Gone for good: a removal.
        let fs = Fs::new();
        d.event("/p/c.ts".into(), RawKind::Remove, t0);
        assert!(d.poll(t0 + QUIET, fs.probe()).is_empty());
        let out = d.poll(t0 + QUIET + GRACE, fs.probe());
        assert_eq!(names(&out), vec![("/p/c.ts".into(), ChangeKind::Deleted)]);
    }

    #[test]
    fn a_long_burst_flushes_at_max_wait() {
        let fs = Fs::new();
        fs.set("/p/b.ts", FileState::File);
        let mut d = Debouncer::default();
        let t0 = Instant::now();
        let mut t = t0;
        while t < t0 + MAX_WAIT {
            d.event("/p/b.ts".into(), RawKind::Modify, t);
            t += Duration::from_millis(10);
        }
        assert_eq!(d.poll(t0 + MAX_WAIT, fs.probe()).len(), 1);
    }

    #[test]
    fn filters() {
        assert!(ignored("/w/node_modules/x/index.d.ts"));
        assert!(ignored("/w/.git/HEAD"));
        assert!(ignored("/w/target/debug/x.js"));
        assert!(!ignored("/w/src/targets.ts"));
        for n in [
            "a.ts",
            "a.d.ts",
            "a.MTS",
            "x.json",
            "tsconfig.json",
            "ts-r.toml",
            "a.jsx",
        ] {
            assert!(relevant(n), "{n}");
        }
        for n in ["a.ts~", ".a.ts.swp", "4913", "README.md"] {
            assert!(!relevant(n), "{n}");
        }
        // Sub-second parts are multiples of 100 ns: Windows' SystemTime has that resolution.
        let started = UNIX_EPOCH + Duration::new(1_000, 500);
        assert!(predates_watch(
            UNIX_EPOCH + Duration::new(999, 100),
            started
        ));
        assert!(!predates_watch(UNIX_EPOCH + Duration::new(999, 0), started));
        assert!(!predates_watch(
            UNIX_EPOCH + Duration::new(1_001, 100),
            started
        ));
    }
}
