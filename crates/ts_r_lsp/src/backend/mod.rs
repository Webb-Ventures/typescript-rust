//! The `Backend` seam: whatever answers type questions for a workspace.
//!
//! L1 has one backend family, `typescript` (the project's own TypeScript), in two protocols:
//!
//! * [`Protocol::Lsp`]: TS 7's native language server (`<native tsc> --lsp --stdio`).
//! * [`Protocol::TsServer`]: tsserver for TS 5/6 (`node lib/tsserver.js ...`), spoken over its
//!   own protocol; the daemon translates LSP requests for it (see `crate::translate`).
//!
//! L3 adds our checker as an in-process backend speaking [`Protocol::Lsp`]: it implements
//! [`Backend`] (messages in through [`Backend::send`], out through the sink given to
//! [`BackendFactory::start`]) and the daemon does not change.

pub mod process;
pub mod typescript;

use serde_json::Value;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

/// Wire protocol a backend speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Protocol {
    /// Language Server Protocol (JSON-RPC 2.0).
    Lsp,
    /// tsserver's request/response/event protocol.
    TsServer,
}

/// How to start a backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendSpec {
    /// Protocol it speaks.
    pub protocol: Protocol,
    /// Human-readable name, e.g. `tsserver 6.0.3`.
    pub label: String,
    /// TypeScript version.
    pub version: String,
    /// Executable.
    pub program: PathBuf,
    /// Arguments.
    pub args: Vec<OsString>,
    /// Extra environment.
    pub env: Vec<(String, String)>,
    /// Working directory.
    pub cwd: PathBuf,
}

/// Something the backend did.
#[derive(Clone, Debug, PartialEq)]
pub enum BackendEvent {
    /// A message from the backend.
    Message(Value),
    /// The backend's output closed (it exited or crashed).
    Exited,
}

/// Callback receiving backend events (called from backend-owned threads).
pub type Sink = Box<dyn Fn(BackendEvent) + Send + Sync>;

/// A running backend.
pub trait Backend: Send + fmt::Debug {
    /// Protocol it speaks.
    fn protocol(&self) -> Protocol;
    /// Queues a message for the backend. Never blocks on the backend.
    fn send(&mut self, msg: &Value) -> io::Result<()>;
    /// OS process id, if it is a separate process (for memory sampling).
    fn pid(&self) -> Option<u32>;
    /// Whether the backend runs inside the daemon (its memory is the daemon's RSS).
    fn in_process(&self) -> bool {
        false
    }
    /// Child processes an in-process backend runs (their trees count towards its memory).
    fn helper_pids(&self) -> Vec<u32> {
        Vec::new()
    }
    /// Stops the backend: closes its input, waits up to `grace`, then kills it. Returns
    /// immediately; the wait happens on a background thread.
    fn shutdown(&mut self, grace: Duration);
}

/// Starts backends. The daemon owns one; tests may substitute their own.
pub trait BackendFactory: Send + fmt::Debug {
    /// Starts a backend described by `spec`, delivering its output to `sink`.
    fn start(&self, spec: &BackendSpec, sink: Sink) -> io::Result<Box<dyn Backend>>;
}

/// Starts backends as child processes.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessFactory;

impl BackendFactory for ProcessFactory {
    fn start(&self, spec: &BackendSpec, sink: Sink) -> io::Result<Box<dyn Backend>> {
        Ok(Box::new(process::ProcessBackend::spawn(spec, sink)?))
    }
}
