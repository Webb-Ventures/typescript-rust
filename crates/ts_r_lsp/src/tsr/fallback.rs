//! The per-project fallback (K-17): when the parity check finds a difference, that project's
//! type diagnostics come from the user's own TypeScript (tsserver for TS 5/6, the native
//! language server for TS 7). Only documents of fallback projects are opened in it, only
//! diagnostics are asked for, and every other feature stays on our checker.
//!
//! The process is started on first use and shared by all fallback projects of the
//! workspace. A language server (TS 7) is asked with pull diagnostics
//! (`textDocument/diagnostic`) after each open and change; pushed diagnostics are taken
//! too. Its output reaches the coordinator as [`Event`]s; [`Fallback::on_message`]
//! turns it into LSP diagnostics.

use super::CMsg;
use crate::backend::process::ProcessBackend;
use crate::backend::typescript::{self, TsEntry, TsServerOptions};
use crate::backend::{Backend, BackendEvent, Protocol};
use crate::jsonrpc::{self, Kind};
use crate::text::TextDoc;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::time::Duration;

/// Output of the fallback process.
#[derive(Debug)]
pub enum Event {
    /// A message from it.
    Message(Value),
    /// It exited.
    Exited(String),
}

/// The running fallback TypeScript.
#[derive(Debug)]
pub struct Fallback {
    backend: Box<dyn Backend>,
    protocol: Protocol,
    label: String,
    /// Text it has of each open document.
    docs: BTreeMap<String, TextDoc>,
    /// tsserver diagnostics per file: syntax, semantic, suggestion.
    diags: BTreeMap<String, [Option<Value>; 3]>,
    seq: i64,
    versions: BTreeMap<String, i64>,
    /// Pull requests in flight: id -> (path, version asked for).
    pulls: BTreeMap<i64, (String, i64)>,
    /// Cleared when the server answers a pull with an error (push only).
    pull: bool,
}

impl Fallback {
    /// Starts the TypeScript found in `root`'s `node_modules` (or its parents).
    pub fn start(root: &Path, tx: Sender<CMsg>) -> io::Result<Fallback> {
        let install = typescript::find_typescript(root).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no TypeScript in node_modules")
        })?;
        let spec = match &install.entry {
            TsEntry::TsServer(js) => {
                typescript::tsserver_spec(&install, js, root, &TsServerOptions::default())
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "node not found"))?
            }
            TsEntry::Native(exe) => typescript::native_lsp_spec(&install, exe, root),
        };
        let sink = Box::new(move |ev| {
            let e = match ev {
                BackendEvent::Message(v) => Event::Message(v),
                BackendEvent::Exited => Event::Exited("process ended".into()),
            };
            let _ = tx.send(CMsg::Fallback(e));
        });
        let backend: Box<dyn Backend> = Box::new(ProcessBackend::spawn(&spec, sink)?);
        let mut f = Fallback {
            backend,
            protocol: spec.protocol,
            label: spec.label.clone(),
            docs: BTreeMap::new(),
            diags: BTreeMap::new(),
            seq: 1,
            versions: BTreeMap::new(),
            pulls: BTreeMap::new(),
            pull: true,
        };
        match f.protocol {
            Protocol::TsServer => {
                f.ts("configure", json!({"hostInfo": "ts-r-lsp fallback"}));
            }
            Protocol::Lsp => {
                let uri = crate::uri::path_to_uri(&root.to_string_lossy());
                let id = f.next_seq();
                let _ = f.backend.send(&jsonrpc::request(
                    json!(id),
                    "initialize",
                    json!({"processId": std::process::id(), "rootUri": uri,
                           "capabilities": {"textDocument": {"publishDiagnostics": {},
                               "diagnostic": {"dynamicRegistration": false}}}}),
                ));
                let _ = f
                    .backend
                    .send(&jsonrpc::notification("initialized", json!({})));
            }
        }
        crate::daemon::log(&format!("ts-r: fallback {} started", f.label));
        Ok(f)
    }

    /// `tsserver 6.0.3`, ...
    pub fn label(&self) -> String {
        self.label.clone()
    }

    /// Process id (for memory reporting).
    pub fn pid(&self) -> Option<u32> {
        self.backend.pid()
    }

    /// The text the fallback has for `path`.
    pub fn text_of(&self, path: &str) -> Option<&str> {
        self.docs.get(path).map(TextDoc::text)
    }

    fn next_seq(&mut self) -> i64 {
        self.seq += 1;
        self.seq
    }

    fn ts(&mut self, command: &str, args: Value) {
        let seq = self.next_seq();
        let _ = self
            .backend
            .send(&json!({"seq": seq, "type": "request", "command": command, "arguments": args}));
    }

    /// Opens a document (or replaces its text).
    pub fn open(&mut self, path: &str, text: &str) {
        if self.docs.contains_key(path) {
            self.change(path, text);
            return;
        }
        self.docs.insert(path.to_string(), TextDoc::new(text));
        match self.protocol {
            Protocol::TsServer => {
                self.ts(
                    "updateOpen",
                    json!({"openFiles": [{"file": path, "fileContent": text}]}),
                );
                self.geterr();
            }
            Protocol::Lsp => {
                self.versions.insert(path.to_string(), 1);
                let lang = language(path);
                let _ = self.backend.send(&jsonrpc::notification(
                    "textDocument/didOpen",
                    json!({"textDocument": {"uri": crate::uri::path_to_uri(path), "languageId": lang, "version": 1, "text": text}}),
                ));
                self.pull_diagnostics(path);
            }
        }
    }

    /// The document's text changed.
    pub fn change(&mut self, path: &str, text: &str) {
        let Some(doc) = self.docs.get_mut(path) else {
            return;
        };
        if doc.text() == text {
            return;
        }
        let end = doc.end_pos();
        *doc = TextDoc::new(text);
        match self.protocol {
            Protocol::TsServer => {
                self.ts(
                    "updateOpen",
                    json!({"changedFiles": [{"fileName": path, "textChanges": [
                        {"start": {"line": 1, "offset": 1}, "end": end.to_ts(), "newText": text}
                    ]}]}),
                );
                self.geterr();
            }
            Protocol::Lsp => {
                let v = self.versions.entry(path.to_string()).or_insert(1);
                *v += 1;
                let v = *v;
                let _ = self.backend.send(&jsonrpc::notification(
                    "textDocument/didChange",
                    json!({"textDocument": {"uri": crate::uri::path_to_uri(path), "version": v},
                           "contentChanges": [{"text": text}]}),
                ));
                self.pull_diagnostics(path);
            }
        }
    }

    /// Closes a document.
    pub fn close(&mut self, path: &str) {
        if self.docs.remove(path).is_none() {
            return;
        }
        self.diags.remove(path);
        match self.protocol {
            Protocol::TsServer => self.ts("updateOpen", json!({"closedFiles": [path]})),
            Protocol::Lsp => {
                let _ = self.backend.send(&jsonrpc::notification(
                    "textDocument/didClose",
                    json!({"textDocument": {"uri": crate::uri::path_to_uri(path)}}),
                ));
            }
        }
    }

    /// Asks a language server for `path`'s diagnostics (LSP pull model).
    fn pull_diagnostics(&mut self, path: &str) {
        if !self.pull {
            return;
        }
        let version = self.versions.get(path).copied().unwrap_or(1);
        let id = self.next_seq();
        // Only the latest pull per file matters.
        self.pulls.retain(|_, (p, _)| p != path);
        self.pulls.insert(id, (path.to_string(), version));
        let _ = self.backend.send(&jsonrpc::request(
            json!(id),
            "textDocument/diagnostic",
            json!({"textDocument": {"uri": crate::uri::path_to_uri(path)}}),
        ));
    }

    fn geterr(&mut self) {
        let files: Vec<&String> = self.docs.keys().collect();
        let args = json!({"files": files, "delay": 0});
        self.ts("geterr", args);
    }

    /// Turns a message from the process into `(path, LSP diagnostics)` when it completes a
    /// file's diagnostics.
    pub fn on_message(&mut self, msg: &Value) -> Option<(String, Vec<Value>)> {
        match self.protocol {
            Protocol::TsServer => {
                let event = msg.get("event")?.as_str()?;
                let idx = match event {
                    "syntaxDiag" => 0,
                    "semanticDiag" => 1,
                    "suggestionDiag" => 2,
                    _ => return None,
                };
                let file = msg["body"]["file"].as_str()?.to_string();
                if !self.docs.contains_key(&file) {
                    return None;
                }
                let entry = self.diags.entry(file.clone()).or_default();
                entry[idx] = Some(msg["body"]["diagnostics"].clone());
                if idx != 1 && entry[1].is_none() {
                    return None;
                }
                // Suggestions are not shown (tsserver's own clients render them faintly).
                let all: Vec<Value> = entry[..2]
                    .iter()
                    .flatten()
                    .filter_map(Value::as_array)
                    .flatten()
                    .cloned()
                    .collect();
                let diags = crate::translate::diagnostics_to_lsp(&all, &|p: &str| {
                    crate::uri::path_to_uri(p)
                });
                Some((file, diags))
            }
            Protocol::Lsp => {
                if let Some(Kind::Notification {
                    method: "textDocument/publishDiagnostics",
                }) = jsonrpc::classify(msg)
                {
                    let uri = msg["params"]["uri"].as_str()?;
                    let path = crate::uri::key_for_uri(uri);
                    let diags = msg["params"]["diagnostics"].as_array()?.clone();
                    return Some((path, diags));
                }
                match jsonrpc::classify(msg) {
                    Some(Kind::Request { id, .. }) => {
                        let _ = self
                            .backend
                            .send(&jsonrpc::response(id.clone(), Value::Null));
                        None
                    }
                    Some(Kind::Response { .. }) => self.on_pull_response(msg),
                    _ => None,
                }
            }
        }
    }

    /// A `textDocument/diagnostic` answer: the file's diagnostics when it is still at the
    /// version asked for.
    fn on_pull_response(&mut self, msg: &Value) -> Option<(String, Vec<Value>)> {
        let id = msg.get("id")?.as_i64()?;
        let (path, version) = self.pulls.remove(&id)?;
        if msg.get("error").is_some() {
            // Not supported (or failed): rely on pushed diagnostics from now on.
            if msg["error"]["code"] == -32601 {
                self.pull = false;
            }
            return None;
        }
        if self.versions.get(&path) != Some(&version) || !self.docs.contains_key(&path) {
            return None;
        }
        let r = &msg["result"];
        if r["kind"] != "full" {
            return None;
        }
        Some((path, r["items"].as_array()?.clone()))
    }

    /// Stops the process.
    pub fn stop(&mut self) {
        if self.protocol == Protocol::TsServer {
            self.ts("exit", Value::Null);
        }
        self.backend.shutdown(Duration::from_secs(2));
    }
}

fn language(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".tsx") {
        "typescriptreact"
    } else if lower.ends_with(".jsx") {
        "javascriptreact"
    } else if lower.ends_with(".js") || lower.ends_with(".mjs") || lower.ends_with(".cjs") {
        "javascript"
    } else {
        "typescript"
    }
}
