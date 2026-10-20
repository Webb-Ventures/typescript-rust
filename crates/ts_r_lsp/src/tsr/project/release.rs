//! Giving back the memory of closed files (M5 history, editor memory).
//!
//! A session's checker keeps everything checking computed until the session ends: the types
//! and per-symbol state of every file it checked, function bodies included, in the session's
//! own level and, after edits, in the project's stable level (`base`). An edit ends the
//! session, but closing files does not, so before this a project kept the results of every
//! file ever opened. Now a close starts a quiet timer ([`quiet`], `TS_R_LSP_RELEASE_MS`,
//! default 2000 ms; closing more files restarts it). When it runs out with no work queued:
//!
//! * no open file left in the project: the program, its levels, shapes and indexes are
//!   dropped; the project waits (document texts and closes still tracked) until a file of it
//!   is opened again or a request arrives, then reloads.
//! * otherwise the session ends and the next one starts on a fresh checker and a rebuilt
//!   stable level; the shared lib and dependency levels are kept. Open files keep their
//!   published diagnostics; requests check their file again on demand (`ensure_ck`).
//!
//! Hooks in `project.rs` are one line each: `note_close` in `on_own`, the session's wait and
//! its idle branch, and [`Project::release`] at the top of the thread loop.

use super::{PMsg, Project, empty_program};
use std::sync::{Arc, Condvar, Mutex, Once, OnceLock};
use std::time::{Duration, Instant};
use ts_r_checker::Layer;

/// Quiet time after the last close before memory is released.
pub fn quiet() -> Duration {
    static Q: OnceLock<Duration> = OnceLock::new();
    *Q.get_or_init(|| {
        let ms = std::env::var("TS_R_LSP_RELEASE_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(2000);
        Duration::from_millis(ms)
    })
}

/// Per project: a pending release and counters.
#[derive(Debug, Default)]
pub struct ReleaseState {
    /// The last close, while a release is pending.
    closed_at: Option<Instant>,
    /// The program was dropped (no open files); the next open or request reloads it.
    unloaded: bool,
    pub releases: u32,
    pub unloads: u32,
}

impl ReleaseState {
    /// A document of the project was closed.
    pub fn note_close(&mut self) {
        self.closed_at = Some(Instant::now());
    }

    /// How long an idle session may block before the release is due (`None`: forever).
    pub fn wait(&self) -> Option<Duration> {
        self.closed_at.map(|t| quiet().saturating_sub(t.elapsed()))
    }

    /// The quiet time after the last close has passed.
    pub fn due(&self) -> bool {
        self.closed_at.is_some_and(|t| t.elapsed() >= quiet())
    }
}

/// The lowest levels of `base` that hold no project file (lib, dependencies): shared with
/// other projects, kept while the stable level is rebuilt.
fn shared_part(base: Option<&Arc<Layer>>) -> Option<Arc<Layer>> {
    let mut cur = base;
    while let Some(l) = cur {
        let project = l
            .files()
            .iter()
            .any(|f| ts_r_history::levels::base_level(f) >= ts_r_history::levels::PROJECT);
        if !project {
            return Some(l.clone());
        }
        cur = l.parent();
    }
    None
}

impl Project {
    /// Releases what closed files left behind when it is due (see the module docs). Returns
    /// false when the thread must stop.
    pub(super) fn release(&mut self, rx: &std::sync::mpsc::Receiver<PMsg>) -> bool {
        if !self.release.due() && !self.release.unloaded {
            return true;
        }
        self.release.closed_at = None;
        if self.owned.is_empty() {
            if !self.release.unloaded {
                self.unload();
            }
            return self.wait_while_unloaded(rx);
        }
        // Rebuild the stable level (it holds the closed files' results) on the kept shared
        // levels; the session's own level goes with the session.
        let keep = shared_part(self.base.as_ref());
        self.base = None;
        self.relayout = true;
        self.update_program();
        self.arrange();
        drop(keep);
        give_back_soon();
        self.release.releases += 1;
        crate::daemon::log(&format!(
            "ts-r {}: released closed files' check results ({} open)",
            self.name,
            self.owned.len()
        ));
        true
    }

    fn unload(&mut self) {
        self.program = Arc::new(empty_program(&self.options));
        self.base = None;
        self.shapes = None;
        self.index.clear();
        self.importers.clear();
        self.api.clear();
        self.published.clear();
        self.todo.clear();
        self.hot.clear();
        self.dirty.clear();
        self.reload = true;
        self.relayout = true;
        self.release.unloaded = true;
        self.release.unloads += 1;
        give_back_soon();
        crate::daemon::log(&format!(
            "ts-r {}: no open files, program released",
            self.name
        ));
    }

    /// Keeps the overlay, options and ownership current without a program; returns when a
    /// message needs the program (it goes back to the backlog), false on `Stop`.
    fn wait_while_unloaded(&mut self, rx: &std::sync::mpsc::Receiver<PMsg>) -> bool {
        loop {
            match self.next(rx, None) {
                None | Some(PMsg::Stop) => return false,
                Some(PMsg::Doc { path, text, edits }) => {
                    // The reload reads the overlay; nothing is stale yet.
                    self.on_doc(path, text, edits);
                    self.dirty.clear();
                }
                Some(PMsg::Own {
                    path, own: false, ..
                }) => {
                    self.owned.remove(&path);
                }
                Some(PMsg::Config { options, roots }) => {
                    self.options = *options;
                    self.roots = roots;
                    self.sound.invalidate();
                }
                Some(PMsg::Fallback(on)) => self.fallback = on,
                Some(m) => {
                    self.backlog.push_front(m);
                    self.release.unloaded = false;
                    return true;
                }
            }
        }
    }
}

/// Calls [`give_back`] once things are quiet: [`SETTLE`] after the last release of any project
/// (projects release together after a batch of closes, and shared files are freed by the last
/// one), on one helper thread.
fn give_back_soon() {
    static DUE: Mutex<Option<Instant>> = Mutex::new(None);
    static WAKE: Condvar = Condvar::new();
    static STARTED: Once = Once::new();
    if let Ok(mut due) = DUE.lock() {
        *due = Some(Instant::now() + SETTLE);
    }
    WAKE.notify_one();
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("ts-r give-back".into())
            .spawn(|| {
                let Ok(mut due) = DUE.lock() else {
                    return;
                };
                loop {
                    match *due {
                        None => match WAKE.wait(due) {
                            Ok(g) => due = g,
                            Err(_) => return,
                        },
                        Some(t) if t > Instant::now() => {
                            match WAKE.wait_timeout(due, t - Instant::now()) {
                                Ok((g, _)) => due = g,
                                Err(_) => return,
                            }
                        }
                        Some(_) => {
                            *due = None;
                            drop(due);
                            give_back();
                            let live = ts_r_history::live_layers();
                            let mb = live.iter().map(|l| l.2).sum::<usize>() as f64 / 1048576.0;
                            crate::daemon::log(&format!(
                                "ts-r: freed memory returned to the OS ({} live levels, {mb:.1} MB of level tables)",
                                live.len()
                            ));
                            due = match DUE.lock() {
                                Ok(g) => g,
                                Err(_) => return,
                            };
                        }
                    }
                }
            });
    });
}

/// Quiet time before [`give_back`] runs.
const SETTLE: Duration = Duration::from_millis(500);

/// Asks the allocator to return free pages to the OS. Freed check results sit between live
/// allocations of the same size classes, so without this the RSS barely moves.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn give_back() {
    unsafe extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
    }
    // SAFETY: libSystem's documented entry point; a null zone means every zone and a goal of
    // 0 means "as much as possible". It only madvises free pages; no Rust object is touched.
    unsafe {
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[allow(unsafe_code)]
fn give_back() {
    unsafe extern "C" {
        fn malloc_trim(pad: usize) -> i32;
    }
    // SAFETY: glibc's documented `malloc_trim`; it only releases free heap memory.
    unsafe {
        malloc_trim(0);
    }
}

#[cfg(not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
fn give_back() {}
