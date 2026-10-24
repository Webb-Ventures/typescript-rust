//! Running Node as a child of ts-r-run and relaying signals to it (Unix).
//!
//! ts-r-run stays the parent so that a signal sent to it (`kill <pid>`, a process manager, a
//! test harness) reaches the program, and a program that can't answer is still stopped.
//!
//! - SIGINT, SIGTERM and SIGHUP are relayed with a handshake. The preload (`js/relay.mjs`)
//!   puts a hidden listener first on each; when the program gets the signal the listener sends
//!   SIGURG to this process ("I have it") and, when the program has no listener of its own,
//!   exits with 128 + the signal number, as Node's default action would end it.
//!   On a signal, ts-r-run waits [`ANSWER`] for that report (Ctrl-C reaches the whole
//!   process group, so the child may already have it); with no report it sends the signal to
//!   the child and waits [`LAST_CHANCE`]; with still no report (a busy loop never runs a
//!   listener) it kills the child and exits with 128 + the signal number.
//! - SIGQUIT, SIGUSR1 (Node's inspector) and SIGUSR2 are passed on as they are.
//! - The exit code is the child's, or 128 + the signal that ended it.
//!
//! Signals are sent with the `kill` utility: the crate has no `unsafe`, and the handful of
//! milliseconds it takes only matters on the rare relay path.

use std::collections::VecDeque;
use std::os::unix::process::ExitStatusExt as _;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use signal_hook::consts::signal::{
    SIGHUP, SIGINT, SIGKILL, SIGQUIT, SIGTERM, SIGURG, SIGUSR1, SIGUSR2,
};
use signal_hook::iterator::Signals;

/// Relayed with the handshake.
const HANDSHAKE: [i32; 3] = [SIGINT, SIGTERM, SIGHUP];
/// Passed on as they are.
const FORWARD: [i32; 3] = [SIGQUIT, SIGUSR1, SIGUSR2];
/// The child's "I have it" report. Ignored by default, so a stray one harms nothing.
const REPORT: i32 = SIGURG;
/// How long to wait for the child's report before sending it the signal.
const ANSWER: Duration = Duration::from_millis(30);
/// How long a child that was sent the signal gets to report before it is killed.
const LAST_CHANCE: Duration = Duration::from_millis(1000);

/// Starts `cmd` and returns the exit code to leave with.
pub fn run_node(mut cmd: Command) -> i32 {
    cmd.env("TS_R_RUN_RELAY", std::process::id().to_string());
    // Catch the signals before the child exists, so an early one isn't lost or fatal.
    let all = HANDSHAKE.iter().chain(&FORWARD).chain(&[REPORT]).copied();
    let mut signals = Signals::new(all).ok();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ts-r-run: cannot start node: {e}");
            return 127;
        }
    };
    let pid = child.id();
    let exited = Arc::new(AtomicBool::new(false));
    let forced = Arc::new(AtomicI32::new(0));
    let handle = signals.as_ref().map(Signals::handle);
    if let Some(mut signals) = signals.take() {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for s in signals.forever() {
                if tx.send(s).is_err() {
                    break;
                }
            }
        });
        let (exited, forced) = (exited.clone(), forced.clone());
        std::thread::spawn(move || relay(&rx, pid, &exited, &forced));
    }
    let status = child.wait();
    exited.store(true, Ordering::SeqCst);
    if let Some(h) = handle {
        h.close();
    }
    let forced = forced.load(Ordering::SeqCst);
    match status {
        _ if forced != 0 => forced,
        Ok(s) => s.code().or(s.signal().map(|n| 128 + n)).unwrap_or(1),
        Err(e) => {
            eprintln!("ts-r-run: lost node: {e}");
            1
        }
    }
}

fn relay(rx: &Receiver<i32>, pid: u32, exited: &AtomicBool, forced: &AtomicI32) {
    let mut queue = VecDeque::new();
    let mut last_report: Option<Instant> = None;
    loop {
        let sig = match queue.pop_front() {
            Some(s) => s,
            None => match rx.recv() {
                Ok(s) => s,
                Err(_) => return,
            },
        };
        if sig == REPORT {
            last_report = Some(Instant::now());
            continue;
        }
        if exited.load(Ordering::SeqCst) {
            continue;
        }
        if !HANDSHAKE.contains(&sig) {
            send(pid, sig);
            continue;
        }
        // The child reported before this process saw its own copy (Ctrl-C).
        if last_report.take().is_some_and(|t| t.elapsed() < ANSWER) {
            continue;
        }
        if wait_report(rx, &mut queue, ANSWER) {
            continue;
        }
        send(pid, sig);
        if wait_report(rx, &mut queue, LAST_CHANCE) || exited.load(Ordering::SeqCst) {
            continue;
        }
        forced.store(128 + sig, Ordering::SeqCst);
        send(pid, SIGKILL);
    }
}

/// Waits up to `limit` for the child's report, keeping other signals for later.
fn wait_report(rx: &Receiver<i32>, queue: &mut VecDeque<i32>, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(REPORT) => return true,
            Ok(s) => queue.push_back(s),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return false,
        }
    }
}

fn send(pid: u32, sig: i32) {
    let name = match sig {
        SIGINT => "INT",
        SIGTERM => "TERM",
        SIGHUP => "HUP",
        SIGQUIT => "QUIT",
        SIGUSR1 => "USR1",
        SIGUSR2 => "USR2",
        _ => "KILL",
    };
    let _ = Command::new("kill")
        .args(["-s", name, &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
