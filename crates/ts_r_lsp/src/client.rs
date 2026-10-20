//! A scripted client for driving `ts-r-lsp` (or a raw backend) from tests and benchmarks.
//!
//! [`ScriptedClient`] spawns a process and talks LSP (`Content-Length` both ways) or
//! tsserver (JSON lines in, `Content-Length` out) to it. Server-to-client requests that a
//! real editor would answer (`client/registerCapability`, `workspace/configuration`,
//! `window/workDoneProgress/create`) are answered automatically.

use crate::jsonrpc::{self, Kind};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{self, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

/// Which protocol the client speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// LSP.
    Lsp,
    /// tsserver.
    TsServer,
}

/// A process driven by a script.
#[derive(Debug)]
pub struct ScriptedClient {
    dialect: Dialect,
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    backlog: VecDeque<Value>,
    next_id: i64,
    /// Server-to-client requests that were answered automatically.
    pub server_requests: Vec<Value>,
    /// When the process was spawned.
    pub spawned_at: Instant,
}

impl ScriptedClient {
    /// Spawns `cmd` with piped stdio (stderr inherited).
    pub fn spawn(mut cmd: Command, dialect: Dialect) -> io::Result<ScriptedClient> {
        let spawned_at = Instant::now();
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdout = child.stdout.take().expect("piped");
        let stdin = child.stdin.take().expect("piped");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut r = BufReader::with_capacity(1 << 16, stdout);
            while let Ok(Some(body)) = jsonrpc::read_frame(&mut r) {
                if let Ok(v) = serde_json::from_slice::<Value>(&body)
                    && tx.send(v).is_err()
                {
                    return;
                }
            }
        });
        Ok(ScriptedClient {
            dialect,
            child,
            stdin,
            rx,
            backlog: VecDeque::new(),
            next_id: 1,
            server_requests: Vec::new(),
            spawned_at,
        })
    }

    /// The process id.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Sends a raw message.
    pub fn send(&mut self, msg: &Value) -> io::Result<()> {
        match self.dialect {
            Dialect::Lsp => jsonrpc::write_message(&mut self.stdin, msg),
            Dialect::TsServer => {
                let mut line = serde_json::to_vec(msg).map_err(io::Error::other)?;
                line.extend_from_slice(b"\r\n");
                self.stdin.write_all(&line)?;
                self.stdin.flush()
            }
        }
    }

    /// Sends an LSP request (or tsserver request) and returns its id / seq.
    pub fn request(&mut self, method: &str, params: Value) -> io::Result<i64> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = match self.dialect {
            Dialect::Lsp => jsonrpc::request(json!(id), method, params),
            Dialect::TsServer => {
                json!({"seq": id, "type": "request", "command": method, "arguments": params})
            }
        };
        self.send(&msg)?;
        Ok(id)
    }

    /// Sends an LSP notification.
    pub fn notify(&mut self, method: &str, params: Value) -> io::Result<()> {
        self.send(&jsonrpc::notification(method, params))
    }

    fn auto_answer(&mut self, m: &Value) -> bool {
        if self.dialect != Dialect::Lsp {
            return false;
        }
        if let Some(Kind::Request { id, method }) = jsonrpc::classify(m) {
            let result = match method {
                "workspace/configuration" => {
                    Value::Array(vec![
                        Value::Null;
                        m["params"]["items"].as_array().map_or(0, Vec::len)
                    ])
                }
                _ => Value::Null,
            };
            let reply = jsonrpc::response(id.clone(), result);
            let _ = self.send(&reply);
            self.server_requests.push(m.clone());
            return true;
        }
        false
    }

    /// Waits for the first message matching `pred` (earlier non-matching ones stay queued).
    pub fn wait_for(&mut self, timeout: Duration, pred: impl Fn(&Value) -> bool) -> Option<Value> {
        if let Some(i) = self.backlog.iter().position(&pred) {
            return self.backlog.remove(i);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let m = self.rx.recv_timeout(left).ok()?;
            if self.auto_answer(&m) {
                continue;
            }
            if pred(&m) {
                return Some(m);
            }
            self.backlog.push_back(m);
        }
    }

    /// Waits for the response to `id` (LSP) or `request_seq` (tsserver).
    pub fn response(&mut self, id: i64, timeout: Duration) -> Option<Value> {
        match self.dialect {
            Dialect::Lsp => self.wait_for(timeout, |m| {
                m["id"] == json!(id) && m.get("method").is_none()
            }),
            Dialect::TsServer => self.wait_for(timeout, |m| {
                m["type"] == "response" && m["request_seq"] == json!(id)
            }),
        }
    }

    /// Request + response.
    pub fn call(&mut self, method: &str, params: Value, timeout: Duration) -> Option<Value> {
        let id = self.request(method, params).ok()?;
        self.response(id, timeout)
    }

    /// Drops queued messages.
    pub fn clear_backlog(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            self.auto_answer(&m);
        }
        self.backlog.clear();
    }

    /// LSP `shutdown` + `exit`, then waits up to 5 s for the process.
    pub fn shutdown(mut self) -> Option<std::process::ExitStatus> {
        if self.dialect == Dialect::Lsp {
            let _ = self.call("shutdown", Value::Null, Duration::from_secs(5));
            let _ = self.notify("exit", Value::Null);
        } else {
            let _ = self.send(&json!({"seq": 0, "type": "request", "command": "exit"}));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(Some(s)) = self.child.try_wait() {
                return Some(s);
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                return self.child.wait().ok();
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Kills the process.
    pub fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `initialize` params for a workspace root.
pub fn initialize_params(root: &std::path::Path) -> Value {
    let uri = crate::uri::path_to_uri(&root.to_string_lossy());
    json!({
        "processId": std::process::id(),
        "rootUri": uri,
        "workspaceFolders": [{"uri": uri, "name": "ws"}],
        "capabilities": {"textDocument": {"publishDiagnostics": {}, "hover": {"contentFormat": ["markdown"]}}},
    })
}

/// `didOpen` params for a file on disk.
pub fn did_open_params(path: &std::path::Path, text: &str) -> Value {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("ts");
    let language = match ext {
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        _ => "typescript",
    };
    json!({"textDocument": {"uri": crate::uri::path_to_uri(&path.to_string_lossy()), "languageId": language, "version": 1, "text": text}})
}
