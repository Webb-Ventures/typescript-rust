//! A backend running as a child process (tsserver or TS 7's native server).
//!
//! Output is always `Content-Length` framed (both tsserver and LSP servers write that).
//! Input is framed for LSP and newline-delimited JSON for tsserver. Writes go through a
//! dedicated writer thread so the daemon's event loop never blocks on a slow backend.

use super::{Backend, BackendEvent, BackendSpec, Protocol, Sink};
use crate::jsonrpc;
use serde_json::Value;
use std::io::{self, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

/// A backend child process.
#[derive(Debug)]
pub struct ProcessBackend {
    protocol: Protocol,
    pid: u32,
    child: Option<Child>,
    tx: Option<Sender<Vec<u8>>>,
}

impl ProcessBackend {
    /// Spawns the process and its reader/writer threads.
    pub fn spawn(spec: &BackendSpec, sink: Sink) -> io::Result<ProcessBackend> {
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("cannot start {}: {e}", spec.program.display()),
            )
        })?;
        let pid = child.id();
        let stdout = child.stdout.take().expect("piped stdout");
        let stdin = child.stdin.take().expect("piped stdin");
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let protocol = spec.protocol;
        thread::Builder::new()
            .name(format!("backend-{pid}-writer"))
            .spawn(move || writer(stdin, rx, protocol))?;
        thread::Builder::new()
            .name(format!("backend-{pid}-reader"))
            .spawn(move || {
                let mut r = BufReader::with_capacity(1 << 16, stdout);
                loop {
                    match jsonrpc::read_frame(&mut r) {
                        Ok(Some(body)) => match serde_json::from_slice::<Value>(&body) {
                            Ok(v) => sink(BackendEvent::Message(v)),
                            Err(e) => eprintln!("ts-r-lsp: backend {pid} sent bad JSON: {e}"),
                        },
                        Ok(None) => break,
                        Err(e) => {
                            eprintln!("ts-r-lsp: backend {pid} output error: {e}");
                            break;
                        }
                    }
                }
                sink(BackendEvent::Exited);
            })?;
        Ok(ProcessBackend {
            protocol,
            pid,
            child: Some(child),
            tx: Some(tx),
        })
    }
}

fn writer(mut stdin: ChildStdin, rx: mpsc::Receiver<Vec<u8>>, protocol: Protocol) {
    for body in rx {
        let res = match protocol {
            Protocol::Lsp => jsonrpc::write_frame(&mut stdin, &body),
            Protocol::TsServer => stdin
                .write_all(&body)
                .and_then(|()| stdin.write_all(b"\n"))
                .and_then(|()| stdin.flush()),
        };
        if res.is_err() {
            break;
        }
    }
    // Dropping stdin closes the pipe: tsserver and LSP servers exit on EOF.
}

impl Backend for ProcessBackend {
    fn protocol(&self) -> Protocol {
        self.protocol
    }

    fn send(&mut self, msg: &Value) -> io::Result<()> {
        let tx = self
            .tx
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "backend stopped"))?;
        let body = serde_json::to_vec(msg).map_err(io::Error::other)?;
        tx.send(body)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "backend writer gone"))
    }

    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }

    fn shutdown(&mut self, grace: Duration) {
        // Closing the channel ends the writer thread, which closes stdin.
        self.tx = None;
        if let Some(mut child) = self.child.take() {
            let _ = thread::Builder::new()
                .name("backend-reaper".into())
                .spawn(move || {
                    let deadline = Instant::now() + grace;
                    loop {
                        match child.try_wait() {
                            Ok(Some(_)) => return,
                            Ok(None) if Instant::now() < deadline => {
                                thread::sleep(Duration::from_millis(20))
                            }
                            _ => {
                                let _ = child.kill();
                                let _ = child.wait();
                                return;
                            }
                        }
                    }
                });
        }
    }
}

impl Drop for ProcessBackend {
    fn drop(&mut self) {
        if self.child.is_some() {
            self.shutdown(Duration::from_secs(2));
        }
    }
}
