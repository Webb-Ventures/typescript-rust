//! File watching with debouncing tuned for editor save patterns, shared by the dev server and
//! `ts-r-check --watch` (design record 0012, E4).
//!
//! Native events come from `notify` (FSEvents with zero latency and `NoDefer` on macOS,
//! inotify on Linux, ReadDirectoryChangesW on Windows). The [`Debouncer`] coalesces bursts:
//!
//! - **Atomic save** (write `x.tmp`, rename over `x`): every path in the burst is collected;
//!   the temp path was created and is gone when the burst settles, so it is dropped, and the
//!   target is reported once as modified.
//! - **Double writes** (truncate, then write; or two writes): one burst within the quiet
//!   window becomes one change. A file that is *empty* when the burst settles gets a short
//!   grace period to be filled; the server also drops changes whose content hash didn't
//!   change, so a late second identical write costs nothing.
//! - **Delete then recreate** (backup-rename editors, a deleted file restored): a missing file
//!   is re-checked after a grace period before being reported as removed; if it is back, it is
//!   reported once, as created (chokidar without `atomic` reports a removal and an addition;
//!   consumers act on the addition, like the dev server retrying failed imports).
//! - **Removing a file reported earlier**: FSEvents keeps a path's flags for a while, so the
//!   removal of a file created a moment ago arrives flagged as created *and* removed, like a
//!   temp file. A path this debouncer has already reported as present is reported as removed,
//!   and so is a path the consumer said it read ([`WatchAdder::known`]): a file that existed
//!   before the watch started and was never edited during it is otherwise unknown here (by the
//!   time the event arrives the file is gone, so probing then can't tell it from a temp file).
//! - **Editing a file created a moment ago**: the same sticky flags mark the edit as a
//!   creation. A known path (reported as present, or read by the consumer) that is still there
//!   is reported as modified; after its removal is reported, re-creating it is a creation again.
//!
//! The quiet window is the latency floor of this step (budget: 1–5 ms), so it is short.
//!
//! **macOS fast path.** FSEvents delivers 5–15 ms after a write (measured p50 ≈ 11 ms with
//! zero latency and `NoDefer`), which alone would blow the 1–5 ms budget. kqueue on a single
//! file delivers in ≈ 0.1 ms but costs one file descriptor per file, so files that changed
//! recently (an LRU of [`FAST_PATH_FILES`]) are additionally watched with kqueue and
//! re-armed after every change (atomic saves replace the inode). The first save of a file
//! pays FSEvents latency; the next ones don't. Both sources feed the same debouncer, so the
//! late FSEvents duplicate is just a second burst whose content hash didn't change.
//!
//! **Replayed startup events (macOS).** FSEvents also reports writes made just *before* the
//! stream started (they reach `fseventsd` after the stream subscribes), so files written right
//! before the server started showed up as changes nobody made, ran `handleHotUpdate` and
//! pushed updates for modules the page never saw change. During [`STARTUP_WINDOW`] after the
//! watch starts, a settled change whose file's mtime predates the start is dropped (one `stat`
//! per settled change, only in that window; Vite's chokidar likewise ignores initial files).
//! Removals are always reported, and an mtime with no sub-second part (a coarse file system,
//! where the comparison can't be trusted) is always kept.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use notify::{EventKind, RecursiveMode, Watcher as _};

/// Debounce timing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebounceOptions {
    /// Flush once no event arrived for this long.
    pub quiet: Duration,
    /// Flush at the latest this long after the first event of a burst.
    pub max_wait: Duration,
    /// Extra wait for a file that is empty (truncate-then-write) or missing (delete then
    /// recreate) when its burst settles.
    pub grace: Duration,
}

impl Default for DebounceOptions {
    fn default() -> DebounceOptions {
        DebounceOptions {
            quiet: Duration::from_millis(3),
            max_wait: Duration::from_millis(30),
            grace: Duration::from_millis(40),
        }
    }
}

/// What happened to a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Created,
    Modified,
    Removed,
}

/// One settled change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    pub path: PathBuf,
    pub kind: ChangeKind,
    /// When the first native event of the burst arrived.
    pub first_event: Instant,
    /// Wall-clock time of the first event (to compare with the file's mtime).
    pub first_event_wall: SystemTime,
}

/// A set of changes that settled together.
#[derive(Clone, Debug)]
pub struct ChangeBatch {
    pub changes: Vec<FileChange>,
    /// When the batch was handed to the sink.
    pub flushed: Instant,
}

/// Native event classification fed to the [`Debouncer`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawKind {
    Create,
    Modify,
    Remove,
    Rename,
    Other,
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    first: Instant,
    first_wall: SystemTime,
    last: Instant,
    saw_create: bool,
    /// A probe found the file missing (it may have come back since).
    saw_missing: bool,
    /// Grace re-check deadline once the file was found empty or missing.
    grace_until: Option<Instant>,
}

/// What the filesystem says about a path when a burst settles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileState {
    Missing,
    Empty,
    Present,
}

/// The pure part of the watcher: collects raw events and decides when and what to flush.
/// The filesystem is consulted through the `probe` callback so tests can fake it.
#[derive(Debug)]
pub struct Debouncer {
    opts: DebounceOptions,
    pending: BTreeMap<PathBuf, Pending>,
    burst_start: Option<Instant>,
    last_event: Option<Instant>,
    /// Paths known to exist: last reported as created or modified, or named by the consumer
    /// ([`Debouncer::known`]). Removing one is never a temp file.
    reported: BTreeSet<PathBuf>,
}

impl Debouncer {
    pub fn new(opts: DebounceOptions) -> Debouncer {
        Debouncer {
            opts,
            pending: BTreeMap::new(),
            burst_start: None,
            last_event: None,
            reported: BTreeSet::new(),
        }
    }

    /// The consumer read `path`, so it exists: its removal is reported even when the native
    /// events also flag it as created (see the module docs).
    pub fn known(&mut self, path: PathBuf) {
        self.reported.insert(path);
    }

    pub fn event(&mut self, path: PathBuf, kind: RawKind, now: Instant, wall: SystemTime) {
        if kind == RawKind::Other {
            return;
        }
        self.burst_start.get_or_insert(now);
        self.last_event = Some(now);
        let p = self.pending.entry(path).or_insert(Pending {
            first: now,
            first_wall: wall,
            last: now,
            saw_create: false,
            saw_missing: false,
            grace_until: None,
        });
        p.last = now;
        p.grace_until = None;
        if kind == RawKind::Create {
            p.saw_create = true;
        }
    }

    /// When [`Debouncer::poll`] should next be called (`None` = nothing pending).
    pub fn deadline(&self) -> Option<Instant> {
        let burst = match (self.burst_start, self.last_event) {
            (Some(start), Some(last)) => {
                Some((last + self.opts.quiet).min(start + self.opts.max_wait))
            }
            _ => None,
        };
        let grace = self.pending.values().filter_map(|p| p.grace_until).min();
        match (burst, grace) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Returns the changes that are ready at `now`.
    pub fn poll(&mut self, now: Instant, probe: impl Fn(&Path) -> FileState) -> Vec<FileChange> {
        let burst_settled = match (self.burst_start, self.last_event) {
            (Some(start), Some(last)) => {
                now >= last + self.opts.quiet || now >= start + self.opts.max_wait
            }
            _ => false,
        };
        if burst_settled {
            self.burst_start = None;
            self.last_event = None;
        }
        let mut out = Vec::new();
        let mut done = Vec::new();
        for (path, p) in self.pending.iter_mut() {
            let ready = match p.grace_until {
                Some(g) => now >= g,
                None => burst_settled,
            };
            if !ready {
                continue;
            }
            let in_grace = p.grace_until.is_some();
            // FSEvents' sticky flags mark edits of a recently created file as creations too;
            // a file known to exist that is still there was modified. One that was seen gone
            // and is back was deleted and restored: a creation (chokidar reports the removal
            // and the addition; the addition is what consumers act on).
            let created = (p.saw_create && !self.reported.contains(path)) || p.saw_missing;
            let kind = match probe(path) {
                FileState::Present => Some(if created {
                    ChangeKind::Created
                } else {
                    ChangeKind::Modified
                }),
                FileState::Empty if !in_grace => {
                    p.grace_until = Some(now + self.opts.grace);
                    continue;
                }
                // Still empty: a new empty file is still a new file.
                FileState::Empty => Some(if created {
                    ChangeKind::Created
                } else {
                    ChangeKind::Modified
                }),
                // Created and gone within one burst: an editor's temp file.
                FileState::Missing if p.saw_create && !self.reported.contains(path) => None,
                FileState::Missing if !in_grace => {
                    p.grace_until = Some(now + self.opts.grace);
                    p.saw_missing = true;
                    continue;
                }
                FileState::Missing => Some(ChangeKind::Removed),
            };
            done.push(path.clone());
            if kind.is_some_and(|k| k != ChangeKind::Removed) {
                self.reported.insert(path.clone());
            } else {
                self.reported.remove(path);
            }
            if let Some(kind) = kind {
                out.push(FileChange {
                    path: path.clone(),
                    kind,
                    first_event: p.first,
                    first_event_wall: p.first_wall,
                });
            }
        }
        for d in done {
            self.pending.remove(&d);
        }
        out
    }
}

/// Paths the watcher ignores (dependency and VCS directories, OS litter).
pub fn ignored(path: &Path) -> bool {
    path.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some("node_modules" | ".git" | ".hg" | ".svn" | ".DS_Store")
        )
    })
}

fn probe(path: &Path) -> FileState {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() && m.len() == 0 => FileState::Empty,
        Ok(m) if m.is_file() => FileState::Present,
        // Directories are not modules; report them as present so they flush and get ignored.
        Ok(_) => FileState::Present,
        Err(_) => FileState::Missing,
    }
}

/// How long after the watch starts settled changes are checked for FSEvents replaying writes
/// from before it started (see the module docs).
pub const STARTUP_WINDOW: Duration = Duration::from_secs(10);

/// Whether a file with this mtime was last written before a watch that started at `started`.
/// An mtime without a sub-second part comes from a coarse file system (HFS+, FAT, some network
/// mounts) whose rounding can put a later write before `started`; it never predates.
pub fn predates_watch(mtime: SystemTime, started: SystemTime) -> bool {
    mtime < started
        && mtime
            .duration_since(UNIX_EPOCH)
            .is_ok_and(|d| d.subsec_nanos() != 0)
}

/// A settled change FSEvents replayed from before the watch started: not a removal, and the
/// file wasn't written since the start.
fn replayed_from_before(change: &FileChange, started: SystemTime) -> bool {
    change.kind != ChangeKind::Removed
        && std::fs::metadata(&change.path)
            .and_then(|m| m.modified())
            .is_ok_and(|m| predates_watch(m, started))
}

/// How many recently changed files get a kqueue fast-path watch on macOS (one fd each).
pub const FAST_PATH_FILES: usize = 64;

type RawEvent = (PathBuf, RawKind, Instant, SystemTime);

/// What the watch thread receives: native events, and requests to watch more directories.
enum Msg {
    Event(RawEvent),
    Watch(PathBuf, RecursiveMode),
    Known(PathBuf),
}

impl std::fmt::Debug for Msg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Msg::Event(e) => write!(f, "Event({:?})", e.0),
            Msg::Watch(p, _) => write!(f, "Watch({p:?})"),
            Msg::Known(p) => write!(f, "Known({p:?})"),
        }
    }
}

/// A running watcher; dropping it stops watching (the debounce thread exits and drops the
/// native watcher it owns).
#[derive(Debug)]
pub struct FileWatcher {
    adder: WatchAdder,
    stop: Arc<AtomicBool>,
}

impl FileWatcher {
    /// A handle that adds directories to this watcher (for files outside the root, such as
    /// linked workspace packages). Holding it doesn't keep the watcher alive.
    pub fn adder(&self) -> WatchAdder {
        self.adder.clone()
    }
}

/// Adds directories to a running [`FileWatcher`]; a no-op once the watcher stopped.
#[derive(Clone, Debug)]
pub struct WatchAdder {
    tx: mpsc::Sender<Msg>,
}

/// Sends raw events (from the kqueue fast path) into the watch thread's channel.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct Forward(mpsc::Sender<Msg>);

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl Forward {
    fn send(&self, e: RawEvent) -> Result<(), ()> {
        self.0.send(Msg::Event(e)).map_err(|_| ())
    }
}

impl WatchAdder {
    /// Watches `dir` too (recursively or not); its events go through the same debouncer.
    pub fn add(&self, dir: &Path, recursive: bool) {
        let mode = if recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        let _ = self.tx.send(Msg::Watch(dir.to_path_buf(), mode));
    }

    /// Tells the watcher the consumer read `file` (it exists): deleting it is reported as a
    /// removal even when FSEvents also flags it as created, like a temp file. Call it once
    /// per file, when first read.
    pub fn known(&self, file: &Path) {
        let _ = self.tx.send(Msg::Known(file.to_path_buf()));
    }
}

impl Drop for FileWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

#[cfg(target_os = "macos")]
mod fast {
    use super::{FAST_PATH_FILES, Forward, RawKind};
    use kqueue::{EventData, EventFilter, FilterFlag, Ident};
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::mpsc::{Receiver, TryRecvError};
    use std::time::{Duration, Instant, SystemTime};

    /// Runs the kqueue loop until either channel disconnects.
    pub fn run(events: Forward, arm: Receiver<PathBuf>) {
        let Ok(mut kq) = kqueue::Watcher::new() else {
            return;
        };
        let flags = FilterFlag::NOTE_WRITE
            | FilterFlag::NOTE_EXTEND
            | FilterFlag::NOTE_DELETE
            | FilterFlag::NOTE_RENAME;
        let mut lru: VecDeque<PathBuf> = VecDeque::new();
        loop {
            let mut changed = false;
            loop {
                match arm.try_recv() {
                    Ok(path) => {
                        if let Some(i) = lru.iter().position(|p| *p == path) {
                            lru.remove(i);
                        } else if lru.len() == FAST_PATH_FILES
                            && let Some(old) = lru.pop_front()
                        {
                            let _ = kq.remove_filename(&old, EventFilter::EVFILT_VNODE);
                        }
                        // Re-adding replaces the old fd: re-arms on the file's current inode.
                        if kq
                            .add_filename(&path, EventFilter::EVFILT_VNODE, flags)
                            .is_ok()
                        {
                            lru.push_back(path);
                            changed = true;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            if changed && kq.watch().is_err() {
                return;
            }
            if lru.is_empty() {
                // Nothing to poll yet; block until something is armed.
                match arm.recv() {
                    Ok(path) => {
                        if kq
                            .add_filename(&path, EventFilter::EVFILT_VNODE, flags)
                            .is_ok()
                        {
                            lru.push_back(path);
                            if kq.watch().is_err() {
                                return;
                            }
                        }
                    }
                    Err(_) => return,
                }
                continue;
            }
            if let Some(ev) = kq.poll(Some(Duration::from_millis(20)))
                && let (Ident::Filename(_, name), EventData::Vnode(_)) = (&ev.ident, &ev.data)
            {
                let msg = (
                    PathBuf::from(name),
                    RawKind::Modify,
                    Instant::now(),
                    SystemTime::now(),
                );
                if events.send(msg).is_err() {
                    return;
                }
            }
        }
    }
}

/// Watches `root` recursively and calls `sink` with each settled batch, on a dedicated
/// thread. Paths [`ignored`] by default (dependency and VCS directories) are not reported.
pub fn watch(
    root: &Path,
    opts: DebounceOptions,
    sink: impl Fn(ChangeBatch) + Send + 'static,
) -> notify::Result<FileWatcher> {
    watch_filtered(root, opts, ignored, sink)
}

/// [`watch`] with the caller's rule for paths to ignore (ts-r-check watches `node_modules`:
/// installing a package changes module resolution).
pub fn watch_filtered(
    root: &Path,
    opts: DebounceOptions,
    ignore: fn(&Path) -> bool,
    sink: impl Fn(ChangeBatch) + Send + 'static,
) -> notify::Result<FileWatcher> {
    let (tx, rx) = mpsc::channel::<Msg>();
    #[cfg(target_os = "macos")]
    let arm = {
        let (arm_tx, arm_rx) = mpsc::channel::<PathBuf>();
        let events = Forward(tx.clone());
        thread::Builder::new()
            .name("ts-r-watch-kqueue".into())
            .spawn(move || fast::run(events, arm_rx))
            .map_err(notify::Error::io)?;
        arm_tx
    };
    let adder = WatchAdder { tx: tx.clone() };
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        let now = Instant::now();
        let wall = SystemTime::now();
        let kind = match event.kind {
            EventKind::Create(_) => RawKind::Create,
            EventKind::Modify(notify::event::ModifyKind::Name(_)) => RawKind::Rename,
            EventKind::Modify(notify::event::ModifyKind::Metadata(_)) => RawKind::Other,
            EventKind::Modify(_) => RawKind::Modify,
            EventKind::Remove(_) => RawKind::Remove,
            EventKind::Any => RawKind::Modify,
            EventKind::Access(_) | EventKind::Other => RawKind::Other,
        };
        for path in event.paths {
            if !ignore(&path) {
                let _ = tx.send(Msg::Event((path, kind, now, wall)));
            }
        }
    })?;
    // Taken before the stream starts: a write after this point is never mistaken for a replay.
    let started = SystemTime::now();
    let started_at = Instant::now();
    watcher.watch(root, RecursiveMode::Recursive)?;
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    thread::Builder::new()
        .name("ts-r-watch".into())
        .spawn(move || {
            // The thread owns the native watcher so it can add directories to it.
            let mut watcher = watcher;
            let mut d = Debouncer::new(opts);
            loop {
                if stopped.load(Ordering::SeqCst) {
                    return;
                }
                let msg = match d.deadline() {
                    // Idle: wake up now and then to notice the watcher being dropped.
                    None => rx.recv_timeout(Duration::from_millis(250)),
                    Some(deadline) => {
                        rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    }
                };
                let mut on = |m: Msg, d: &mut Debouncer| match m {
                    Msg::Event((path, kind, now, wall)) => d.event(path, kind, now, wall),
                    Msg::Watch(dir, mode) => {
                        let _ = watcher.watch(&dir, mode);
                    }
                    Msg::Known(path) => d.known(path),
                };
                match msg {
                    Ok(m) => {
                        on(m, &mut d);
                        // Drain whatever else is already queued before deciding.
                        while let Ok(m) = rx.try_recv() {
                            on(m, &mut d);
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
                let now = Instant::now();
                if d.deadline().is_some_and(|dl| dl <= now) {
                    let mut changes = d.poll(now, probe);
                    // Only FSEvents replays; inotify and ReadDirectoryChangesW start empty.
                    if cfg!(target_os = "macos") && now < started_at + STARTUP_WINDOW {
                        changes.retain(|c| !replayed_from_before(c, started));
                    }
                    #[cfg(target_os = "macos")]
                    for c in &changes {
                        if c.kind != ChangeKind::Removed {
                            let _ = arm.send(c.path.clone());
                        }
                    }
                    if !changes.is_empty() {
                        sink(ChangeBatch {
                            changes,
                            flushed: Instant::now(),
                        });
                    }
                }
            }
        })
        .map_err(notify::Error::io)?;
    Ok(FileWatcher { adder, stop })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

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

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn opts() -> DebounceOptions {
        DebounceOptions {
            quiet: ms(3),
            max_wait: ms(30),
            grace: ms(40),
        }
    }

    fn kinds(v: &[FileChange]) -> Vec<(String, ChangeKind)> {
        v.iter()
            .map(|c| (c.path.to_string_lossy().into_owned(), c.kind))
            .collect()
    }

    #[test]
    fn atomic_rename_save_is_one_modify_of_the_target() {
        let fs = Fs::new();
        let mut d = Debouncer::new(opts());
        let t0 = Instant::now();
        let w = SystemTime::now();
        // write a.js.tmp, then rename it over a.js
        d.event("/p/a.js.tmp".into(), RawKind::Create, t0, w);
        d.event("/p/a.js.tmp".into(), RawKind::Modify, t0 + ms(1), w);
        d.event("/p/a.js.tmp".into(), RawKind::Rename, t0 + ms(1), w);
        d.event("/p/a.js".into(), RawKind::Rename, t0 + ms(1), w);
        fs.set("/p/a.js", FileState::Present);
        assert!(
            d.poll(t0 + ms(2), fs.probe()).is_empty(),
            "still within quiet window"
        );
        assert_eq!(d.deadline(), Some(t0 + ms(4)));
        let out = d.poll(t0 + ms(4), fs.probe());
        assert_eq!(kinds(&out), [("/p/a.js".to_owned(), ChangeKind::Modified)]);
        assert_eq!(out[0].first_event, t0 + ms(1));
        assert_eq!(d.deadline(), None);
    }

    #[test]
    fn double_write_within_window_is_one_change() {
        let fs = Fs::new();
        fs.set("/p/a.js", FileState::Present);
        let mut d = Debouncer::new(opts());
        let t0 = Instant::now();
        let w = SystemTime::now();
        d.event("/p/a.js".into(), RawKind::Modify, t0, w);
        d.event("/p/a.js".into(), RawKind::Modify, t0 + ms(2), w);
        assert!(d.poll(t0 + ms(4), fs.probe()).is_empty());
        let out = d.poll(t0 + ms(5), fs.probe());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].first_event, t0);
    }

    #[test]
    fn truncate_then_write_waits_for_content() {
        let fs = Fs::new();
        fs.set("/p/a.js", FileState::Empty);
        let mut d = Debouncer::new(opts());
        let t0 = Instant::now();
        let w = SystemTime::now();
        d.event("/p/a.js".into(), RawKind::Modify, t0, w);
        assert!(
            d.poll(t0 + ms(3), fs.probe()).is_empty(),
            "empty file deferred"
        );
        // The second write lands during the grace period.
        fs.set("/p/a.js", FileState::Present);
        d.event("/p/a.js".into(), RawKind::Modify, t0 + ms(10), w);
        let out = d.poll(t0 + ms(13), fs.probe());
        assert_eq!(kinds(&out), [("/p/a.js".to_owned(), ChangeKind::Modified)]);
        // A file that stays empty is reported after the grace period.
        fs.set("/p/b.js", FileState::Empty);
        d.event("/p/b.js".into(), RawKind::Modify, t0 + ms(20), w);
        assert!(d.poll(t0 + ms(23), fs.probe()).is_empty());
        assert_eq!(d.deadline(), Some(t0 + ms(63)));
        assert_eq!(d.poll(t0 + ms(63), fs.probe()).len(), 1);
    }

    #[test]
    fn delete_then_recreate_is_one_creation_and_real_delete_is_removed() {
        let fs = Fs::new();
        let mut d = Debouncer::new(opts());
        let t0 = Instant::now();
        let w = SystemTime::now();
        d.event("/p/a.js".into(), RawKind::Remove, t0, w);
        assert!(d.poll(t0 + ms(3), fs.probe()).is_empty(), "missing → grace");
        fs.set("/p/a.js", FileState::Present);
        let out = d.poll(t0 + ms(43), fs.probe());
        // Seen gone, then back: restored (one change, a creation), even for a known file.
        assert_eq!(kinds(&out), [("/p/a.js".to_owned(), ChangeKind::Created)]);

        d.event("/p/b.js".into(), RawKind::Remove, t0 + ms(50), w);
        assert!(d.poll(t0 + ms(53), fs.probe()).is_empty());
        let out = d.poll(t0 + ms(93), fs.probe());
        assert_eq!(kinds(&out), [("/p/b.js".to_owned(), ChangeKind::Removed)]);
    }

    #[test]
    fn continuous_events_flush_at_max_wait() {
        let fs = Fs::new();
        fs.set("/p/a.js", FileState::Present);
        let mut d = Debouncer::new(opts());
        let t0 = Instant::now();
        let w = SystemTime::now();
        for i in 0..20 {
            d.event("/p/a.js".into(), RawKind::Modify, t0 + ms(i * 2), w);
        }
        assert_eq!(d.deadline(), Some(t0 + ms(30)));
        assert_eq!(d.poll(t0 + ms(30), fs.probe()).len(), 1);
    }

    #[test]
    fn new_file_is_created_and_ignored_dirs_are_skipped() {
        let fs = Fs::new();
        fs.set("/p/new.js", FileState::Present);
        let mut d = Debouncer::new(opts());
        let t0 = Instant::now();
        d.event("/p/new.js".into(), RawKind::Create, t0, SystemTime::now());
        let out = d.poll(t0 + ms(3), fs.probe());
        assert_eq!(kinds(&out), [("/p/new.js".to_owned(), ChangeKind::Created)]);
        assert!(ignored(Path::new("/p/node_modules/x/index.js")));
        assert!(ignored(Path::new("/p/.git/index")));
        assert!(!ignored(Path::new("/p/src/a.js")));
    }

    #[test]
    fn removing_a_reported_file_flagged_as_created_is_a_removal() {
        // FSEvents' sticky flags: the removal of a file created a moment ago comes as
        // create + remove, like a temp file, but this one was reported as created.
        let fs = Fs::new();
        fs.set("/p/new.js", FileState::Present);
        let mut d = Debouncer::new(opts());
        let t0 = Instant::now();
        let w = SystemTime::now();
        d.event("/p/new.js".into(), RawKind::Create, t0, w);
        assert_eq!(d.poll(t0 + ms(3), fs.probe()).len(), 1);
        fs.set("/p/new.js", FileState::Missing);
        let t1 = t0 + ms(2000);
        d.event("/p/new.js".into(), RawKind::Create, t1, w);
        d.event("/p/new.js".into(), RawKind::Remove, t1, w);
        assert!(d.poll(t1 + ms(3), fs.probe()).is_empty()); // grace
        let out = d.poll(t1 + ms(50), fs.probe());
        assert_eq!(kinds(&out), [("/p/new.js".to_owned(), ChangeKind::Removed)]);
        // A temp file never reported is still dropped, and a new empty file is created.
        fs.set("/p/empty.js", FileState::Empty);
        d.event("/p/empty.js".into(), RawKind::Create, t1 + ms(55), w);
        assert!(d.poll(t1 + ms(58), fs.probe()).is_empty()); // grace
        let out = d.poll(t1 + ms(100), fs.probe());
        assert_eq!(
            kinds(&out),
            [("/p/empty.js".to_owned(), ChangeKind::Created)]
        );
        d.event("/p/x.tmp".into(), RawKind::Create, t1 + ms(110), w);
        d.event("/p/x.tmp".into(), RawKind::Remove, t1 + ms(110), w);
        assert!(d.poll(t1 + ms(120), fs.probe()).is_empty());
    }

    #[test]
    fn removing_a_known_file_flagged_as_created_is_a_removal() {
        // A file that existed before the watch and was never edited during it: the consumer
        // read it, so its deletion (flagged create + remove by FSEvents) is a removal.
        let fs = Fs::new();
        let mut d = Debouncer::new(opts());
        d.known("/p/re-export.js".into());
        let t0 = Instant::now();
        let w = SystemTime::now();
        d.event("/p/re-export.js".into(), RawKind::Create, t0, w);
        d.event("/p/re-export.js".into(), RawKind::Remove, t0, w);
        assert!(d.poll(t0 + ms(3), fs.probe()).is_empty()); // grace
        let out = d.poll(t0 + ms(50), fs.probe());
        assert_eq!(
            kinds(&out),
            [("/p/re-export.js".to_owned(), ChangeKind::Removed)]
        );
        // The same events for a file nobody read are still a temp file.
        d.event("/p/other.js".into(), RawKind::Create, t0 + ms(60), w);
        d.event("/p/other.js".into(), RawKind::Remove, t0 + ms(60), w);
        assert!(d.poll(t0 + ms(63), fs.probe()).is_empty());
        assert!(d.poll(t0 + ms(120), fs.probe()).is_empty());
    }

    #[test]
    fn a_known_file_flagged_as_created_is_modified() {
        // FSEvents keeps the "created" flag on later edits of a file created a moment ago
        // (or copied in just before the watch started): an edit, not a new file.
        let fs = Fs::new();
        fs.set("/p/a.js", FileState::Present);
        let mut d = Debouncer::new(opts());
        let t0 = Instant::now();
        let w = SystemTime::now();
        d.event("/p/a.js".into(), RawKind::Create, t0, w);
        let out = d.poll(t0 + ms(3), fs.probe());
        assert_eq!(kinds(&out), [("/p/a.js".to_owned(), ChangeKind::Created)]);
        d.event("/p/a.js".into(), RawKind::Create, t0 + ms(10), w);
        d.event("/p/a.js".into(), RawKind::Modify, t0 + ms(10), w);
        let out = d.poll(t0 + ms(13), fs.probe());
        assert_eq!(kinds(&out), [("/p/a.js".to_owned(), ChangeKind::Modified)]);
        d.known("/p/b.js".into());
        fs.set("/p/b.js", FileState::Present);
        d.event("/p/b.js".into(), RawKind::Create, t0 + ms(20), w);
        let out = d.poll(t0 + ms(23), fs.probe());
        assert_eq!(kinds(&out), [("/p/b.js".to_owned(), ChangeKind::Modified)]);
        // Deleted, then restored: a new file again.
        fs.set("/p/a.js", FileState::Missing);
        d.event("/p/a.js".into(), RawKind::Remove, t0 + ms(30), w);
        assert!(d.poll(t0 + ms(33), fs.probe()).is_empty()); // grace
        let out = d.poll(t0 + ms(80), fs.probe());
        assert_eq!(kinds(&out), [("/p/a.js".to_owned(), ChangeKind::Removed)]);
        fs.set("/p/a.js", FileState::Present);
        d.event("/p/a.js".into(), RawKind::Create, t0 + ms(90), w);
        let out = d.poll(t0 + ms(93), fs.probe());
        assert_eq!(kinds(&out), [("/p/a.js".to_owned(), ChangeKind::Created)]);
    }

    #[test]
    fn mtimes_before_the_start_predate_unless_coarse() {
        let start = UNIX_EPOCH + Duration::new(1_000, 500);
        assert!(predates_watch(
            UNIX_EPOCH + Duration::new(1_000, 499),
            start
        ));
        assert!(!predates_watch(start, start));
        assert!(!predates_watch(UNIX_EPOCH + Duration::new(1_001, 1), start));
        // Whole seconds: a coarse file system may have rounded a later write down.
        assert!(!predates_watch(UNIX_EPOCH + Duration::new(1_000, 0), start));
    }

    /// FSEvents replays writes made just before the stream started (seen on fresh CI VMs);
    /// files nobody touched after the watch began must not be reported.
    #[test]
    fn files_written_before_the_watch_starts_are_not_reported() {
        use std::sync::Mutex;
        let dir = TempDir::new("watch-stale");
        for i in 0..20 {
            dir.write(&format!("src/old{i}.js"), "export {};\n");
        }
        let seen: Arc<Mutex<Vec<FileChange>>> = Arc::default();
        let sink = Arc::clone(&seen);
        let _w = watch(dir.path(), DebounceOptions::default(), move |b| {
            sink.lock().unwrap().extend(b.changes);
        })
        .unwrap();
        // Give late native events time to arrive, then make a real change and wait for it
        // (FSEvents delivers in order, so anything stale is in by then). Under load the
        // native stream can start late and miss a write, so rewrite until one is seen.
        thread::sleep(ms(300));
        let fresh = dir.path().join("src/new.js");
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut n = 0;
        'wait: while Instant::now() < deadline {
            n += 1;
            dir.write("src/new.js", &format!("export const n = {n};\n"));
            for _ in 0..100 {
                if seen.lock().unwrap().iter().any(|c| c.path == fresh) {
                    break 'wait;
                }
                thread::sleep(ms(5));
            }
        }
        thread::sleep(ms(100));
        let paths: Vec<PathBuf> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.path.clone())
            .collect();
        assert!(paths.contains(&fresh), "{paths:?}");
        assert!(
            paths.iter().all(|p| *p == fresh || !p.is_file()),
            "untouched files reported: {paths:?}"
        );
    }

    /// A canonical temporary directory removed on drop (macOS `/var` is `/private/var`,
    /// which is what the native watcher reports).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> TempDir {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            let dir =
                std::env::temp_dir().join(format!("ts-r-{label}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            TempDir(std::fs::canonicalize(&dir).expect("canonicalize temp dir"))
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, rel: &str, content: &str) {
            let p = self.0.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).expect("create dir");
            }
            std::fs::write(p, content).expect("write file");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
