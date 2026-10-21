//! Child processes with a timeout and bounded captured output.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Keep at most this much of each stream (the tail) by default.
pub const MAX_CAPTURE: usize = 256 * 1024;

/// How long the output may stay open after the child exits (a process it left running
/// holds the pipes); then what was read so far is the output.
const DRAIN_GRACE: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub struct Output {
    /// `None` when killed by a signal or by the timeout.
    pub code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    /// Whether `stdout` is everything the process wrote (nothing dropped for the capture
    /// limit, and the stream ended).
    pub stdout_complete: bool,
    pub elapsed: Duration,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// First non-empty stderr line (or stdout's when stderr is empty), for messages.
    pub fn first_line(&self) -> String {
        let pick = |s: &str| {
            s.lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(String::from)
        };
        pick(&self.stderr)
            .or_else(|| pick(&self.stdout))
            .unwrap_or_default()
    }
}

/// A stream being read on its own thread.
struct Drain {
    /// The captured tail, and whether anything was dropped before it.
    buf: Arc<Mutex<(Vec<u8>, bool)>>,
    done: Receiver<()>,
    max: usize,
}

fn drain(mut r: impl Read + Send + 'static, max: usize) -> Drain {
    let buf = Arc::new(Mutex::new((Vec::new(), false)));
    let (tx, done) = mpsc::channel();
    let shared = buf.clone();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match r.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut guard = shared.lock().unwrap_or_else(|e| e.into_inner());
                    let (buf, dropped) = &mut *guard;
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.len() > 2 * max {
                        let cut = buf.len() - max;
                        buf.drain(..cut);
                        *dropped = true;
                    }
                }
            }
        }
        let _ = tx.send(());
    });
    Drain { buf, done, max }
}

impl Drain {
    /// The captured tail, waiting for the end of the stream until `deadline`. Also whether
    /// the stream ended by then, and whether the tail is all of it.
    fn finish(self, deadline: Instant) -> (String, bool, bool) {
        let closed = self
            .done
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_ok();
        let mut guard = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        let (buf, dropped) = &mut *guard;
        if buf.len() > self.max {
            let cut = buf.len() - self.max;
            buf.drain(..cut);
            *dropped = true;
        }
        let text = String::from_utf8_lossy(buf).into_owned();
        (text, closed, closed && !*dropped)
    }
}

/// Runs `cmd` (stdin closed), killing it after `timeout`. Only the direct child is killed;
/// run anything that spawns its own children under `scripts/limit.sh`. Output still open
/// [`DRAIN_GRACE`] after the child ended is cut off there (noted in `stderr`).
pub fn run(cmd: &mut Command, timeout: Duration) -> Output {
    run_capturing(cmd, timeout, MAX_CAPTURE)
}

/// [`run`] keeping up to `max` bytes of each stream.
pub fn run_capturing(cmd: &mut Command, timeout: Duration, max: usize) -> Output {
    let start = Instant::now();
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Output {
                code: None,
                timed_out: false,
                stdout: String::new(),
                stderr: format!("failed to start {:?}: {e}", cmd.get_program()),
                stdout_complete: false,
                elapsed: start.elapsed(),
            };
        }
    };
    let out = child.stdout.take().map(|r| drain(r, max));
    let err = child.stderr.take().map(|r| drain(r, max));
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    timed_out = true;
                    break child.wait().ok();
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break None,
        }
    };
    let deadline = Instant::now() + DRAIN_GRACE;
    let none = || (String::new(), true, true);
    let (stdout, out_closed, stdout_complete) = out.map_or_else(none, |d| d.finish(deadline));
    let (mut stderr, err_closed, _) = err.map_or_else(none, |d| d.finish(deadline));
    if !(out_closed && err_closed) {
        stderr.push_str("\n(output cut off: still open after the process ended)\n");
    }
    Output {
        code: if timed_out {
            None
        } else {
            status.and_then(|s| s.code())
        },
        timed_out,
        stdout,
        stderr,
        stdout_complete,
        elapsed: start.elapsed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_and_times_out() {
        let o = run(
            Command::new("sh").args(["-c", "echo hi; echo err >&2; exit 3"]),
            Duration::from_secs(10),
        );
        assert_eq!(o.code, Some(3));
        assert_eq!(o.stdout.trim(), "hi");
        assert!(o.stdout_complete);
        assert_eq!(o.first_line(), "err");
        let o = run(Command::new("sleep").arg("5"), Duration::from_millis(100));
        assert!(o.timed_out);
        assert!(!o.ok());
    }

    #[test]
    fn dropped_output_is_reported() {
        let o = run_capturing(
            Command::new("sh").args(["-c", "yes 0123456789 | head -c 100000"]),
            Duration::from_secs(10),
            1000,
        );
        assert_eq!(o.stdout.len(), 1000);
        assert!(!o.stdout_complete);
        let o = run_capturing(
            Command::new("sh").args(["-c", "yes 0123456789 | head -c 100000"]),
            Duration::from_secs(10),
            200_000,
        );
        assert_eq!(o.stdout.len(), 100_000);
        assert!(o.stdout_complete);
    }

    // Unix only: Windows' sh (MSYS) waits for its background jobs before it exits, so the child
    // itself runs for the whole `sleep` there (8 s on a CI runner) and nothing is left holding
    // the pipes.
    #[cfg(unix)]
    #[test]
    fn output_held_open_by_a_leftover_process_does_not_block() {
        let o = run(
            Command::new("sh").args(["-c", "echo hi; sleep 5 &"]),
            Duration::from_secs(60),
        );
        assert!(o.elapsed < Duration::from_secs(4), "{:?}", o.elapsed);
        assert_eq!(o.code, Some(0));
        assert_eq!(o.stdout.trim(), "hi");
        assert!(o.stderr.contains("output cut off"), "{}", o.stderr);
        assert!(!o.stdout_complete);
    }

    #[cfg(unix)]
    #[test]
    fn limit_sh_kills_what_the_command_left_running() {
        let limit = concat!(env!("CARGO_MANIFEST_DIR"), "/../scripts/limit.sh");
        let o = run(
            Command::new(limit).args(["--", "sh", "-c", "echo hi; sleep 7.25 &"]),
            Duration::from_secs(60),
        );
        assert_eq!(o.code, Some(0), "{}", o.stderr);
        assert_eq!(o.stdout.trim(), "hi");
        assert!(o.elapsed < Duration::from_secs(4), "{:?}", o.elapsed);
        assert!(!o.stderr.contains("output cut off"), "{}", o.stderr);
        // SIGKILL is delivered asynchronously: allow the process a moment to go.
        let gone = (0..40).any(|_| {
            let left = Command::new("pgrep")
                .args(["-f", "sleep 7.25"])
                .output()
                .unwrap();
            std::thread::sleep(Duration::from_millis(50));
            left.stdout.is_empty()
        });
        assert!(gone, "`sleep 7.25` left running");
    }
}
