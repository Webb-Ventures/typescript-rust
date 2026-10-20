//! Windows resource limits for [`crate::limited`]: each run gets its own Job Object.
//!
//! - `JOB_OBJECT_LIMIT_JOB_MEMORY` caps the committed memory of every process in the job
//!   together (limit.sh on Unix caps resident memory of the process group). An allocation that
//!   would pass the cap fails, and the job posts `JOB_OBJECT_MSG_JOB_MEMORY_LIMIT` to its
//!   completion port; the runner then terminates the job and reports [`EXIT_MEMORY_LIMIT`], as
//!   limit.sh does. The system does not guarantee delivery of that message, so a run that
//!   failed with the job's peak commit within 1/16 of the cap is reported the same way.
//! - The time limit is a deadline on the wait; on expiry the job is terminated and the run
//!   reports [`EXIT_TIME_LIMIT`].
//! - `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, plus an explicit terminate once the child exits,
//!   kills whatever the child left running, as limit.sh kills the rest of its process group.
//!
//! std cannot spawn a process suspended and hand back its main thread, so the child is
//! assigned to the job right after `spawn`: for that brief window it runs unconfined, and a
//! process it starts inside the window is outside the job. Everything it starts afterwards
//! inherits the job (no breakaway is allowed).

use std::io::{self, Read, Write};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::ExitStatusExt;
use std::process::{Child, Command, ExitStatus, Output};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::IO::{
    CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_ASSOCIATE_COMPLETION_PORT,
    JOBOBJECT_BASIC_LIMIT_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectAssociateCompletionPortInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};

use crate::{EXIT_MEMORY_LIMIT, EXIT_TIME_LIMIT};

/// `JOB_OBJECT_MSG_JOB_MEMORY_LIMIT` from winnt.h (windows-sys only has it behind the large
/// `Win32_System_SystemServices` feature).
const JOB_OBJECT_MSG_JOB_MEMORY_LIMIT: u32 = 10;
/// How long one wait on the completion port lasts before checking whether the child exited.
const POLL: Duration = Duration::from_millis(50);
/// At most this many queued job messages are read per check (the loop always ends).
const MAX_MESSAGES: usize = 64;

#[derive(Clone, Copy)]
enum Kill {
    Memory,
    Time,
}

/// An owned kernel handle, closed on drop.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a valid handle this module created and owns; it is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// A job with the memory cap and kill-on-close set, and a completion port for its messages.
/// Fields drop in order: closing `job` kills what is still in it, then `port` is closed.
struct Job {
    job: Handle,
    port: Handle,
    limit_bytes: usize,
}

impl Job {
    fn new(mem_mb: u32) -> io::Result<Job> {
        // SAFETY: null security attributes and a null name are allowed: an unnamed job with
        // default security. The result is checked before use.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Handle(job);
        // SAFETY: INVALID_HANDLE_VALUE with no existing port creates a new completion port
        // that is not tied to a file. The result is checked before use.
        let port =
            unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 1) };
        if port.is_null() {
            return Err(io::Error::last_os_error());
        }
        let port = Handle(port);
        let limit_bytes = (mem_mb as usize).saturating_mul(1024 * 1024);
        let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
            BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                LimitFlags: JOB_OBJECT_LIMIT_JOB_MEMORY | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                ..Default::default()
            },
            JobMemoryLimit: limit_bytes,
            ..Default::default()
        };
        // SAFETY: `job.0` is a live job handle; the pointer and size describe `limits`, a
        // JOBOBJECT_EXTENDED_LIMIT_INFORMATION that lives across the call.
        let ok = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let assoc = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
            CompletionKey: std::ptr::null_mut(),
            CompletionPort: port.0,
        };
        // SAFETY: `job.0` and `port.0` are live handles; the pointer and size describe
        // `assoc`, which lives across the call. `Job` keeps the port open as long as the job.
        let ok = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectAssociateCompletionPortInformation,
                (&raw const assoc).cast(),
                size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Job {
            job,
            port,
            limit_bytes,
        })
    }

    fn assign(&self, child: &Child) -> io::Result<()> {
        // SAFETY: `self.job.0` is a live job handle; the process handle is owned by `child`,
        // which outlives the call.
        let ok = unsafe { AssignProcessToJobObject(self.job.0, child.as_raw_handle()) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Kills every process in the job with `code`.
    fn terminate(&self, code: i32) {
        // SAFETY: `self.job.0` is a live job handle. Failure (an empty job) is harmless.
        unsafe { TerminateJobObject(self.job.0, code as u32) };
    }

    /// Reads the queued job messages, waiting up to `wait` for the first one, and returns
    /// whether one of them says the job hit its memory limit.
    fn memory_limit_hit(&self, wait: Duration) -> bool {
        let mut hit = false;
        let mut wait_ms = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX - 1);
        for _ in 0..MAX_MESSAGES {
            let mut msg = 0u32;
            let mut key = 0usize;
            let mut overlapped: *mut OVERLAPPED = std::ptr::null_mut();
            // SAFETY: `self.port.0` is a live completion port; every out-pointer points at a
            // local of the type the function writes. For job messages `overlapped` receives
            // a process id, never dereferenced here.
            let ok = unsafe {
                GetQueuedCompletionStatus(self.port.0, &mut msg, &mut key, &mut overlapped, wait_ms)
            };
            if ok == 0 {
                break;
            }
            hit |= msg == JOB_OBJECT_MSG_JOB_MEMORY_LIMIT;
            wait_ms = 0;
        }
        hit
    }

    /// Whether the job's peak committed memory came within 1/16 of the cap.
    fn peak_near_limit(&self) -> bool {
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: `self.job.0` is a live job handle; the pointer and size describe `info`,
        // which the call fills in. The returned length is not needed (null is allowed).
        let ok = unsafe {
            QueryInformationJobObject(
                self.job.0,
                JobObjectExtendedLimitInformation,
                (&raw mut info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        };
        ok != 0 && info.PeakJobMemoryUsed >= self.limit_bytes - self.limit_bytes / 16
    }
}

fn read_all(mut r: impl Read + Send + 'static) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        buf
    })
}

/// Runs `cmd` (stdio already configured) in a new job capped at `mem_mb` and `time_s`.
/// Piped stdout/stderr are collected into the result. The kill report goes into the captured
/// stderr, or to this process's stderr when the child inherited it (`stderr_inherited`).
pub(crate) fn run(
    cmd: &mut Command,
    mem_mb: u32,
    time_s: u32,
    stderr_inherited: bool,
) -> io::Result<Output> {
    let job = Job::new(mem_mb)?;
    let mut child = cmd.spawn()?;
    if let Err(e) = job.assign(&child) {
        // A child that already exited cannot join the job, and needs no limits.
        if child.try_wait()?.is_none() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other(format!(
                "could not put {:?} into a job object: {e}",
                cmd.get_program()
            )));
        }
    }
    let stdout = child.stdout.take().map(read_all);
    let stderr = child.stderr.take().map(read_all);

    let deadline = Instant::now() + Duration::from_secs(time_s.into());
    let mut killed = None;
    // An early return drops `job`, and kill-on-close takes the child down with it.
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let now = Instant::now();
        if now >= deadline {
            job.terminate(EXIT_TIME_LIMIT);
            killed = Some(Kill::Time);
            break child.wait()?;
        }
        if job.memory_limit_hit(POLL.min(deadline - now)) {
            job.terminate(EXIT_MEMORY_LIMIT);
            killed = Some(Kill::Memory);
            break child.wait()?;
        }
    };
    if killed.is_none()
        && (job.memory_limit_hit(Duration::ZERO) || (!status.success() && job.peak_near_limit()))
    {
        killed = Some(Kill::Memory);
    }
    // The child is done. What it left running would escape the deadline and hold the output
    // pipes open: kill it, as limit.sh does.
    job.terminate(1);

    let stdout = stdout
        .map(|t| t.join().unwrap_or_default())
        .unwrap_or_default();
    let captured_stderr = stderr.is_some();
    let mut stderr = stderr
        .map(|t| t.join().unwrap_or_default())
        .unwrap_or_default();
    let status = match killed {
        None => status,
        Some(kill) => {
            let program = cmd.get_program();
            let (code, note) = match kill {
                Kill::Memory => (
                    EXIT_MEMORY_LIMIT,
                    format!("ts_r_testing: killed: job memory over {mem_mb} MB: {program:?}\n"),
                ),
                Kill::Time => (
                    EXIT_TIME_LIMIT,
                    format!("ts_r_testing: killed: ran longer than {time_s}s: {program:?}\n"),
                ),
            };
            if captured_stderr {
                stderr.extend_from_slice(note.as_bytes());
            } else if stderr_inherited {
                let _ = io::stderr().write_all(note.as_bytes());
            }
            ExitStatus::from_raw(code as u32)
        }
    };
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}
