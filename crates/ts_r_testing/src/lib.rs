//! Test support shared by every crate. The main item is [`CappedAlloc`], a global allocator
//! that aborts the process once live heap memory passes a cap, so a runaway test, fuzz loop
//! or corpus run dies at a known size instead of taking the machine down.
//!
//! Install it in any test, example or harness binary that runs generated or real-world input:
//!
//! ```ignore
//! ts_r_testing::cap_memory!(); // default 4096 MB, or TS_R_MEM_CAP_MB
//! ```
//!
//! Child processes (`node`, our binaries) run under [`limited`], which caps the whole process
//! tree by memory and time.

use std::alloc::{GlobalAlloc, Layout, System};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Default cap when `TS_R_MEM_CAP_MB` is not set.
pub const DEFAULT_CAP_MB: usize = 4096;

/// A `System`-backed allocator that aborts when live allocations exceed the cap.
#[derive(Debug)]
pub struct CappedAlloc {
    live: AtomicUsize,
    cap: AtomicUsize, // 0 = not read from the environment yet
    default_cap_mb: usize,
}

impl CappedAlloc {
    pub const fn new(default_cap_mb: usize) -> Self {
        Self {
            live: AtomicUsize::new(0),
            cap: AtomicUsize::new(0),
            default_cap_mb,
        }
    }

    /// Bytes currently allocated through this allocator.
    pub fn live_bytes(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }

    fn cap_bytes(&self) -> usize {
        let cap = self.cap.load(Ordering::Relaxed);
        if cap != 0 {
            return cap;
        }
        // Read the env var without allocating through ourselves: std::env::var_os allocates,
        // so mark the cap as the default first to break the recursion, then refine it.
        let default = self.default_cap_mb.saturating_mul(1024 * 1024);
        self.cap.store(default, Ordering::Relaxed);
        let cap = std::env::var_os("TS_R_MEM_CAP_MB")
            .and_then(|v| v.to_str().and_then(|s| s.trim().parse::<usize>().ok()))
            .map_or(default, |mb| mb.saturating_mul(1024 * 1024));
        self.cap.store(cap, Ordering::Relaxed);
        cap
    }

    fn over_cap(&self) -> ! {
        // Lift the cap first: writing to stderr may allocate, which would re-enter here.
        self.cap.store(usize::MAX, Ordering::Relaxed);
        let msg = b"ts_r_testing: memory cap exceeded, aborting (raise TS_R_MEM_CAP_MB only if the input really needs it)\n";
        let _ = std::io::Write::write_all(&mut std::io::stderr(), msg);
        std::process::abort();
    }
}

// SAFETY: every method forwards to `System` with the caller's layout unchanged; the only
// addition is bookkeeping on an atomic counter, which never touches the returned memory.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for CappedAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let new = self.live.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        if new > self.cap_bytes() {
            self.over_cap();
        }
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let new = self.live.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        if new > self.cap_bytes() {
            self.over_cap();
        }
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.live.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: `ptr` came from `System` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            let grow = new_size - layout.size();
            let new = self.live.fetch_add(grow, Ordering::Relaxed) + grow;
            if new > self.cap_bytes() {
                self.over_cap();
            }
        } else {
            self.live
                .fetch_sub(layout.size() - new_size, Ordering::Relaxed);
        }
        // SAFETY: `ptr` came from `System` with `layout`; `new_size` is the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Installs [`CappedAlloc`] as the global allocator of the current binary.
/// `cap_memory!()` uses 4096 MB; `cap_memory!(512)` sets another default.
/// `TS_R_MEM_CAP_MB` overrides either at run time.
#[macro_export]
macro_rules! cap_memory {
    () => {
        $crate::cap_memory!(4096);
    };
    ($mb:expr) => {
        #[global_allocator]
        static TS_R_CAPPED_ALLOC: $crate::CappedAlloc = $crate::CappedAlloc::new($mb);
    };
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod job;

/// Exit code [`limited`] reports when it killed the tree for running longer than `time_s`.
pub const EXIT_TIME_LIMIT: i32 = 124;
/// Exit code [`limited`] reports when it killed the tree for going over `mem_mb`.
pub const EXIT_MEMORY_LIMIT: i32 = 137;

/// `program` under the repo's resource limits: the whole process tree is capped at `mem_mb`
/// and `time_s`, and killed when either is exceeded (exit code [`EXIT_MEMORY_LIMIT`] or
/// [`EXIT_TIME_LIMIT`]) or when `program` exits, so nothing it left running survives.
///
/// - Unix: runs through `scripts/limit.sh --mem-mb <mem_mb> --time-s <time_s> --` (resident
///   memory of the process group).
/// - Windows: runs in a Job Object with `JOB_OBJECT_LIMIT_JOB_MEMORY` (committed memory of
///   every process in the job) and `KILL_ON_JOB_CLOSE`; the deadline is enforced with
///   `TerminateJobObject`. See `src/job.rs`.
///
/// Configure it like a [`Command`] (`arg`, `args`, `env`, `current_dir`, stdio), then run it
/// with [`Limited::output`] or [`Limited::status`]; only those apply the Windows limits.
pub fn limited(program: impl AsRef<OsStr>, mem_mb: u32, time_s: u32) -> Limited {
    let cmd = if cfg!(windows) {
        Command::new(program)
    } else {
        let limit = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/limit.sh");
        let mut c = Command::new(limit);
        c.arg("--mem-mb")
            .arg(mem_mb.to_string())
            .arg("--time-s")
            .arg(time_s.to_string())
            .arg("--")
            .arg(program);
        c
    };
    Limited {
        cmd,
        mem_mb,
        time_s,
        stdin_set: false,
        stdout_set: false,
        stderr_set: false,
    }
}

/// A command built by [`limited`]. It deliberately does not hand out the inner [`Command`]:
/// running that directly would skip the limits on Windows.
#[derive(Debug)]
pub struct Limited {
    cmd: Command,
    // Only the Windows runner reads these; on Unix they are already limit.sh's arguments.
    #[cfg_attr(not(windows), allow(dead_code))]
    mem_mb: u32,
    #[cfg_attr(not(windows), allow(dead_code))]
    time_s: u32,
    #[cfg_attr(not(windows), allow(dead_code))]
    stdin_set: bool,
    #[cfg_attr(not(windows), allow(dead_code))]
    stdout_set: bool,
    #[cfg_attr(not(windows), allow(dead_code))]
    stderr_set: bool,
}

impl Limited {
    /// See [`Command::arg`].
    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.cmd.arg(arg);
        self
    }

    /// See [`Command::args`].
    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.cmd.args(args);
        self
    }

    /// See [`Command::env`].
    pub fn env(&mut self, key: impl AsRef<OsStr>, val: impl AsRef<OsStr>) -> &mut Self {
        self.cmd.env(key, val);
        self
    }

    /// See [`Command::current_dir`].
    pub fn current_dir(&mut self, dir: impl AsRef<Path>) -> &mut Self {
        self.cmd.current_dir(dir);
        self
    }

    /// See [`Command::stdin`].
    pub fn stdin(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.cmd.stdin(cfg);
        self.stdin_set = true;
        self
    }

    /// See [`Command::stdout`].
    pub fn stdout(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.cmd.stdout(cfg);
        self.stdout_set = true;
        self
    }

    /// See [`Command::stderr`].
    pub fn stderr(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.cmd.stderr(cfg);
        self.stderr_set = true;
        self
    }

    /// Runs the command under the limits and collects its output, like [`Command::output`]
    /// (stdin is null and stdout/stderr are captured unless set).
    pub fn output(&mut self) -> std::io::Result<Output> {
        #[cfg(windows)]
        {
            if !self.stdin_set {
                self.cmd.stdin(Stdio::null());
            }
            if !self.stdout_set {
                self.cmd.stdout(Stdio::piped());
            }
            if !self.stderr_set {
                self.cmd.stderr(Stdio::piped());
            }
            job::run(&mut self.cmd, self.mem_mb, self.time_s, true)
        }
        #[cfg(not(windows))]
        {
            self.cmd.output()
        }
    }

    /// Runs the command under the limits and waits for it, like [`Command::status`] (stdio
    /// is inherited unless set).
    pub fn status(&mut self) -> std::io::Result<ExitStatus> {
        #[cfg(windows)]
        {
            job::run(&mut self.cmd, self.mem_mb, self.time_s, false).map(|o| o.status)
        }
        #[cfg(not(windows))]
        {
            self.cmd.status()
        }
    }
}

/// `path.canonicalize()` without Windows' verbatim prefix: `C:\x`, not `\\?\C:\x`. The tools
/// print and key files by the plain form, so a test comparing paths needs it too.
pub fn canonical(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    let p = path.as_ref().canonicalize()?;
    Ok(match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p,
    })
}
