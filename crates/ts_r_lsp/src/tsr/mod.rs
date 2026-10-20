//! The `ts-r` type backend (L3): our own checker, in the daemon's process, behind the same
//! [`Backend`] seam as the project's TypeScript. It speaks [`Protocol::Lsp`]: messages come
//! in through [`Backend::send`] and go out through the sink, so the daemon routes, mirrors
//! and fans out exactly as for TS 7's native server.
//!
//! Threads:
//!
//! * the **coordinator** (`tsr`) owns open documents (UTF-16 edits are turned into byte edits
//!   here), finds each document's project (the nearest `tsconfig.json`/`jsconfig.json` that
//!   includes it, else the inferred project), starts project threads, routes requests to the
//!   owning project, runs the parity check (K-17) and the per-project TypeScript fallback;
//! * one thread per **project** (`project.rs`) with its program and live checker;
//! * short-lived **parity** jobs (`parity.rs`) and the fallback tsserver's reader threads;
//! * the **input watcher** (`watch.rs`): native watching of every project's inputs, sending
//!   settled changes into the coordinator's loop.

mod bridge;
pub mod convert;
pub mod fallback;
pub mod features;
pub mod parity;
pub mod project;
pub mod watch;

use crate::backend::{Backend, BackendEvent, BackendFactory, BackendSpec, Protocol, Sink};
use crate::jsonrpc::{self, IdKey, Kind, codes};
use crate::text::{Pos, TextDoc};
use convert::ByteEdit;
use project::{PMsg, Project};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use ts_r_checker::LibStore;
use ts_r_config::{CompilerOptions, DirEntry, FileSystem, RealFs};

/// Stack size of project threads (the checker recurses).
const PROJECT_STACK: usize = 256 << 20;
/// Key of the inferred project.
pub const INFERRED: &str = "(inferred)";

/// The lib files, located and parsed once per process (shared by every project and every
/// backend restart; parsed lib files are immutable).
pub fn libs() -> Option<Arc<LibStore>> {
    static LIBS: OnceLock<Option<Arc<LibStore>>> = OnceLock::new();
    LIBS.get_or_init(|| LibStore::locate().map(Arc::new))
        .clone()
}

/// State shared by the coordinator and project threads.
pub struct Shared {
    sink: Sink,
    /// Lib files.
    pub libs: Arc<LibStore>,
    cancelled: Mutex<HashSet<IdKey>>,
    /// Workspace root.
    pub root: PathBuf,
    /// Process id of the fallback TypeScript (0 = not running), for memory sampling.
    fallback_pid: AtomicU32,
    /// The coordinator's inbox (project threads report their inputs there).
    coordinator: Sender<CMsg>,
}

impl fmt::Debug for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shared")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl Shared {
    /// Sends a message to the daemon.
    pub fn send(&self, msg: Value) {
        (self.sink)(BackendEvent::Message(msg));
    }

    /// Whether request `id` was cancelled (forgets it).
    pub fn take_cancelled(&self, id: &Value) -> bool {
        let Some(k) = IdKey::from_value(id) else {
            return false;
        };
        self.cancelled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&k)
    }

    /// A project's program files after a load (lib files and `node_modules` left out): the
    /// coordinator watches their directories.
    pub fn report_inputs(&self, project: &str, files: Vec<String>) {
        let _ = self.coordinator.send(CMsg::Inputs {
            project: project.to_string(),
            files,
        });
    }

    fn cancel(&self, id: &Value) {
        if let Some(k) = IdKey::from_value(id) {
            self.cancelled
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(k);
        }
    }
}

/// The real file system with open documents' unsaved text on top.
#[derive(Debug)]
pub struct OverlayFs<'a> {
    real: RealFs,
    docs: &'a HashMap<String, Arc<str>>,
}

impl<'a> OverlayFs<'a> {
    /// An overlay of `docs` (path → text).
    pub fn new(docs: &'a HashMap<String, Arc<str>>) -> OverlayFs<'a> {
        OverlayFs {
            real: RealFs::new(),
            docs,
        }
    }
}

impl FileSystem for OverlayFs<'_> {
    fn read_file(&self, path: &str) -> Option<String> {
        match self.docs.get(path) {
            Some(t) => Some(t.to_string()),
            None => self.real.read_file(path),
        }
    }
    fn file_exists(&self, path: &str) -> bool {
        self.docs.contains_key(path) || self.real.file_exists(path)
    }
    fn directory_exists(&self, path: &str) -> bool {
        self.real.directory_exists(path)
    }
    fn read_dir(&self, path: &str) -> Vec<DirEntry> {
        self.real.read_dir(path)
    }
    fn realpath(&self, path: &str) -> String {
        if self.docs.contains_key(path) && !self.real.file_exists(path) {
            return path.to_string();
        }
        self.real.realpath(path)
    }
    fn case_sensitive(&self) -> bool {
        self.real.case_sensitive()
    }
    fn cwd(&self) -> String {
        self.real.cwd()
    }
}

// ----------------------------------------------------------------------------------------
// Backend
// ----------------------------------------------------------------------------------------

/// Messages to the coordinator.
#[derive(Debug)]
pub enum CMsg {
    /// An LSP message from the daemon.
    Lsp(Value),
    /// A parity job finished.
    Parity(Box<parity::Outcome>),
    /// The fallback TypeScript said something.
    Fallback(fallback::Event),
    /// Files changed on disk (the input watcher).
    Watched(Vec<(String, watch::ChangeKind)>),
    /// A project loaded its program: its input files.
    Inputs { project: String, files: Vec<String> },
    /// Stop.
    Stop,
}

/// Starts the in-process ts-r backend.
#[derive(Clone, Copy, Debug, Default)]
pub struct TsrFactory;

impl BackendFactory for TsrFactory {
    fn start(&self, spec: &BackendSpec, sink: Sink) -> io::Result<Box<dyn Backend>> {
        let libs = libs().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "the TypeScript lib files (lib.es5.d.ts, ...) were not found next to ts-r-lsp; set TS_R_LIB_DIR",
            )
        })?;
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            sink,
            libs,
            cancelled: Mutex::new(HashSet::new()),
            root: spec.cwd.clone(),
            fallback_pid: AtomicU32::new(0),
            coordinator: tx.clone(),
        });
        let server = Server::new(shared.clone(), tx.clone(), rx);
        let handle = thread::Builder::new()
            .name("tsr".into())
            .spawn(move || server.run())?;
        Ok(Box::new(TsrBackend {
            tx,
            handle: Some(handle),
            shared,
        }))
    }
}

/// A running ts-r backend (handle to the coordinator thread).
#[derive(Debug)]
pub struct TsrBackend {
    tx: Sender<CMsg>,
    handle: Option<thread::JoinHandle<()>>,
    shared: Arc<Shared>,
}

impl Backend for TsrBackend {
    fn protocol(&self) -> Protocol {
        Protocol::Lsp
    }

    fn send(&mut self, msg: &Value) -> io::Result<()> {
        self.tx
            .send(CMsg::Lsp(msg.clone()))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "ts-r backend stopped"))
    }

    fn pid(&self) -> Option<u32> {
        // In-process: see `in_process` and `helper_pids`.
        None
    }

    fn in_process(&self) -> bool {
        true
    }

    fn helper_pids(&self) -> Vec<u32> {
        match self.shared.fallback_pid.load(Ordering::Relaxed) {
            0 => Vec::new(),
            pid => vec![pid],
        }
    }

    fn shutdown(&mut self, grace: Duration) {
        let _ = self.tx.send(CMsg::Stop);
        if let Some(h) = self.handle.take() {
            let _ = thread::Builder::new()
                .name("tsr-stop".into())
                .spawn(move || {
                    let deadline = Instant::now() + grace.max(Duration::from_millis(100));
                    while !h.is_finished() && Instant::now() < deadline {
                        thread::sleep(Duration::from_millis(5));
                    }
                    if h.is_finished() {
                        let _ = h.join();
                    }
                });
        }
    }
}

// ----------------------------------------------------------------------------------------
// Coordinator
// ----------------------------------------------------------------------------------------

/// A parsed config as far as ownership needs it.
#[derive(Debug)]
pub struct ConfigInfo {
    /// Absolute config path.
    pub path: String,
    /// Compiler options.
    pub options: CompilerOptions,
    /// Root files.
    pub file_names: Vec<String>,
    set: HashSet<String>,
    /// Referenced configs (solution-style configs).
    pub references: Vec<String>,
    /// Files whose contents went into this config (itself and `extends`).
    pub inputs: Vec<String>,
    /// Parse and file-discovery diagnostics of the config (`ts-r/projectInfo` reports them).
    pub diagnostics: Vec<ts_r_config::ConfigDiagnostic>,
}

/// An open document.
#[derive(Debug)]
struct OpenDoc {
    uri: String,
    text: TextDoc,
    shared: Arc<str>,
    owner: String,
    /// Opened with [`bridge::MIRROR_LANGUAGE_ID`]: another engine serves it; projects read its
    /// text, none owns it (no diagnostics, no fallback TypeScript).
    mirror: bool,
}

/// Parity / fallback state of a project.
#[derive(Clone, Debug, PartialEq)]
pub enum ProjectState {
    /// Our checker; the parity check has not finished (or is not needed).
    Checking,
    /// Our checker; parity confirmed.
    Matched,
    /// The user's TypeScript gives the diagnostics (parity found a difference).
    Fallback,
    /// The parity check could not run; our checker stays.
    Unknown,
}

impl ProjectState {
    fn as_str(&self) -> &'static str {
        match self {
            ProjectState::Checking => "checking",
            ProjectState::Matched => "matched",
            ProjectState::Fallback => "fallback",
            ProjectState::Unknown => "unverified",
        }
    }
}

#[derive(Debug)]
struct ProjectHandle {
    tx: Sender<PMsg>,
    handle: Option<thread::JoinHandle<()>>,
    config: Option<Arc<ConfigInfo>>,
    state: ProjectState,
    parity: Option<parity::Summary>,
    /// The files of its program as of its last load (lib files and `node_modules` left out).
    inputs: HashSet<String>,
}

/// The coordinator.
struct Server {
    shared: Arc<Shared>,
    tx: Sender<CMsg>,
    rx: Receiver<CMsg>,
    docs: BTreeMap<String, OpenDoc>,
    configs: HashMap<String, Option<Arc<ConfigInfo>>>,
    projects: BTreeMap<String, ProjectHandle>,
    parity: parity::Runner,
    fallback: Option<fallback::Fallback>,
    /// Times the fallback TypeScript exited; after `MAX_FALLBACK_RESTARTS` projects go back
    /// to our checker (state `unknown`) instead of restarting it again.
    fallback_exits: u32,
    init_options: Value,
    stopped: bool,
    /// Native watching of the projects' inputs (started with the first project; `None` when
    /// it could not start or `TS_R_LSP_WATCH=off`).
    watch: Option<watch::InputWatch>,
    /// Directories handed to the watcher.
    watched_dirs: HashSet<String>,
}

/// Restarts of a crashing fallback TypeScript before giving up on it.
const MAX_FALLBACK_RESTARTS: u32 = 3;

impl Server {
    fn new(shared: Arc<Shared>, tx: Sender<CMsg>, rx: Receiver<CMsg>) -> Server {
        Server {
            parity: parity::Runner::new(shared.root.clone()),
            shared,
            tx,
            rx,
            docs: BTreeMap::new(),
            configs: HashMap::new(),
            projects: BTreeMap::new(),
            fallback: None,
            fallback_exits: 0,
            init_options: Value::Null,
            stopped: false,
            watch: None,
            watched_dirs: HashSet::new(),
        }
    }

    fn run(mut self) {
        while !self.stopped {
            let Ok(m) = self.rx.recv() else { break };
            self.handle(m);
        }
        for p in self.projects.values() {
            let _ = p.tx.send(PMsg::Stop);
        }
        if let Some(f) = self.fallback.as_mut() {
            f.stop();
        }
        self.parity.stop();
        for p in self.projects.values_mut() {
            if let Some(h) = p.handle.take() {
                let _ = h.join();
            }
        }
    }

    fn handle(&mut self, m: CMsg) {
        match m {
            CMsg::Stop => self.stopped = true,
            CMsg::Lsp(v) => self.on_lsp(v),
            CMsg::Parity(o) => self.on_parity(*o),
            CMsg::Fallback(e) => self.on_fallback(e),
            CMsg::Watched(changes) => {
                let changes = changes.into_iter().map(|(p, k)| (p, k.lsp())).collect();
                self.on_changes(changes);
            }
            CMsg::Inputs { project, files } => self.on_inputs(&project, files),
        }
    }

    fn reply(&self, id: Value, result: Value) {
        self.shared.send(jsonrpc::response(id, result));
    }

    fn on_lsp(&mut self, msg: Value) {
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match jsonrpc::classify(&msg) {
            Some(Kind::Request { id, method }) => {
                let id = id.clone();
                match method {
                    "initialize" => {
                        self.init_options = params["initializationOptions"].clone();
                        self.reply(
                            id,
                            json!({
                                "capabilities": capabilities(),
                                "serverInfo": {"name": "ts-r", "version": env!("CARGO_PKG_VERSION")},
                            }),
                        );
                    }
                    "shutdown" => self.reply(id, Value::Null),
                    "ts-r/status" => {
                        let s = self.status();
                        self.reply(id, s);
                    }
                    "ts-r/projectInfo" => {
                        let info = self.project_info(&params);
                        self.reply(id, info);
                    }
                    "ts-r/usedBy" => {
                        let used = self.used_by(&params);
                        self.reply(id, used);
                    }
                    "workspace/symbol" | "workspace/executeCommand" => self.reply(id, Value::Null),
                    "workspace/willRenameFiles" => self.will_rename_files(id, params),
                    m => {
                        // `completionItem/resolve` names its document in the item's data.
                        let uri = params["textDocument"]["uri"]
                            .as_str()
                            .or_else(|| params["data"]["uri"].as_str());
                        let Some(uri) = uri else {
                            self.shared.send(jsonrpc::error_response(
                                id,
                                codes::METHOD_NOT_FOUND,
                                format!("{m} is not supported by the ts-r backend"),
                            ));
                            return;
                        };
                        let path = crate::uri::key_for_uri(uri);
                        let owner = match self.docs.get(&path) {
                            Some(d) => d.owner.clone(),
                            None => self.owner_of(&path),
                        };
                        self.ensure_project(&owner);
                        if let Some(p) = self.projects.get(&owner) {
                            let _ = p.tx.send(PMsg::Request {
                                id,
                                method: m.to_string(),
                                params,
                                path,
                                reply: None,
                            });
                        }
                    }
                }
            }
            Some(Kind::Notification { method }) => match method {
                "exit" => self.stopped = true,
                "$/cancelRequest" => self.shared.cancel(&params["id"]),
                "textDocument/didOpen" => self.did_open(&params),
                "textDocument/didChange" => self.did_change(&params),
                "textDocument/didClose" => self.did_close(&params),
                "workspace/didChangeWatchedFiles" => self.watched(&params),
                "ts-r/reloadProjects" => self.reload_projects(),
                _ => {}
            },
            _ => {}
        }
    }

    /// `workspace/willRenameFiles`: every loaded project, and the projects owning the renamed
    /// files (for a folder: those of its open documents), computes its edits; the tsconfigs
    /// of those projects get theirs; one helper thread waits for the parts and answers with
    /// their merge (`rename::merge`). tsserver also answers only for loaded projects.
    fn will_rename_files(&mut self, id: Value, params: Value) {
        let renames = rename_pairs(&params);
        let mut owners: BTreeSet<String> = BTreeSet::new();
        for (from, _) in &renames {
            let under = format!("{from}/");
            match self.docs.get(from) {
                Some(d) => {
                    owners.insert(d.owner.clone());
                }
                None => {
                    let in_folder: Vec<String> = self
                        .docs
                        .iter()
                        .filter(|(p, _)| p.starts_with(&under))
                        .map(|(_, d)| d.owner.clone())
                        .collect();
                    if in_folder.is_empty() {
                        let o = self.owner_of(from);
                        // A folder (or an unknown file) has no project of its own.
                        if o != INFERRED || self.projects.contains_key(INFERRED) {
                            owners.insert(o);
                        }
                    }
                    owners.extend(in_folder);
                }
            }
        }
        for o in &owners {
            self.ensure_project(o);
        }
        // tsconfig edits, from the open text of a config or the disk.
        let mut parts: Vec<Value> = Vec::new();
        for key in self.projects.keys() {
            if key == INFERRED {
                continue;
            }
            let text = match self.docs.get(key) {
                Some(d) => Some(d.text.text().to_string()),
                None => std::fs::read_to_string(key).ok(),
            };
            if let Some(e) = text
                .and_then(|t| features::actions::rename::config_workspace_edit(key, &t, &renames))
            {
                parts.push(e);
            }
        }
        let (tx, rx) = mpsc::channel();
        let mut asked = 0;
        for p in self.projects.values() {
            let sent = p.tx.send(PMsg::Request {
                id: id.clone(),
                method: "workspace/willRenameFiles".into(),
                params: params.clone(),
                path: String::new(),
                reply: Some(tx.clone()),
            });
            if sent.is_ok() {
                asked += 1;
            }
        }
        drop(tx);
        let shared = self.shared.clone();
        let spawned = thread::Builder::new()
            .name("tsr-rename".into())
            .spawn(move || {
                let mut answers = Vec::with_capacity(asked + parts.len());
                // A project that stops (or panics) drops its sender: `recv` ends.
                while answers.len() < asked {
                    match rx.recv_timeout(Duration::from_secs(60)) {
                        Ok(v) => answers.push(v),
                        Err(_) => break,
                    }
                }
                // Projects answer in any order: merge in a fixed one.
                answers.sort_by_cached_key(|v| v.to_string());
                answers.extend(parts);
                if shared.take_cancelled(&id) {
                    shared.send(jsonrpc::error_response(
                        id,
                        codes::REQUEST_CANCELLED,
                        "cancelled",
                    ));
                    return;
                }
                shared.send(jsonrpc::response(
                    id,
                    features::actions::rename::merge(answers),
                ));
            });
        if spawned.is_err() {
            crate::daemon::log("ts-r: could not start the rename thread");
        }
    }

    // ------------------------------------------------------------------------------------
    // Documents
    // ------------------------------------------------------------------------------------

    fn did_open(&mut self, params: &Value) {
        let td = &params["textDocument"];
        let (Some(uri), Some(text)) = (td["uri"].as_str(), td["text"].as_str()) else {
            return;
        };
        let path = crate::uri::key_for_uri(uri);
        let owner = self.owner_of(&path);
        let shared: Arc<str> = Arc::from(text);
        let mirror = td["languageId"].as_str() == Some(bridge::MIRROR_LANGUAGE_ID);
        self.docs.insert(
            path.clone(),
            OpenDoc {
                uri: uri.to_string(),
                text: TextDoc::new(text),
                shared: shared.clone(),
                owner: owner.clone(),
                mirror,
            },
        );
        self.broadcast_doc(&path, Some(shared), Vec::new());
        if mirror {
            // Only its text: its project is served elsewhere and is not started for it.
            return;
        }
        self.ensure_project(&owner);
        if let Some(p) = self.projects.get(&owner) {
            let _ = p.tx.send(PMsg::Own {
                path: path.clone(),
                uri: uri.to_string(),
                own: true,
            });
        }
        if self.projects.get(&owner).map(|p| &p.state) == Some(&ProjectState::Fallback) {
            match self.fallback.as_mut() {
                Some(f) => f.open(&path, text),
                // Started on the first open document of a fallback project.
                None => self.start_fallback_for(&owner),
            }
        }
    }

    fn did_change(&mut self, params: &Value) {
        let Some(uri) = params["textDocument"]["uri"].as_str() else {
            return;
        };
        let path = crate::uri::key_for_uri(uri);
        let Some(doc) = self.docs.get_mut(&path) else {
            return;
        };
        let mut edits = Vec::new();
        for c in params["contentChanges"].as_array().into_iter().flatten() {
            let Some(t) = c.get("text").and_then(Value::as_str) else {
                continue;
            };
            let range = match c.get("range") {
                None | Some(Value::Null) => None,
                Some(r) => crate::text::lsp_range(r),
            };
            let (s, e) = match range {
                Some((a, b)) => (doc.text.offset_at(a), doc.text.offset_at(b)),
                None => (0, doc.text.text().len()),
            };
            edits.push(ByteEdit {
                start: s as u32,
                end: e.max(s) as u32,
                new_len: t.len() as u32,
            });
            doc.text.apply_change(range, t);
        }
        let shared: Arc<str> = Arc::from(doc.text.text());
        doc.shared = shared.clone();
        let owner = doc.owner.clone();
        if let Some(f) = self.fallback.as_mut()
            && !doc.mirror
            && self.projects.get(&owner).map(|p| &p.state) == Some(&ProjectState::Fallback)
        {
            f.change(&path, &shared);
        }
        self.broadcast_doc(&path, Some(shared), edits);
    }

    fn did_close(&mut self, params: &Value) {
        let Some(uri) = params["textDocument"]["uri"].as_str() else {
            return;
        };
        let path = crate::uri::key_for_uri(uri);
        let Some(doc) = self.docs.remove(&path) else {
            return;
        };
        if !doc.mirror {
            if let Some(p) = self.projects.get(&doc.owner) {
                let _ = p.tx.send(PMsg::Own {
                    path: path.clone(),
                    uri: doc.uri.clone(),
                    own: false,
                });
            }
            if let Some(f) = self.fallback.as_mut() {
                f.close(&path);
            }
        }
        self.broadcast_doc(&path, None, Vec::new());
    }

    fn broadcast_doc(&self, path: &str, text: Option<Arc<str>>, edits: Vec<ByteEdit>) {
        for p in self.projects.values() {
            let _ = p.tx.send(PMsg::Doc {
                path: path.to_string(),
                text: text.clone(),
                edits: edits.clone(),
            });
        }
    }

    fn watched(&mut self, params: &Value) {
        let changes = params["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| {
                let uri = c["uri"].as_str()?;
                Some((
                    crate::uri::key_for_uri(uri),
                    c["type"].as_u64().unwrap_or(2),
                ))
            })
            .collect();
        self.on_changes(changes);
    }

    /// `ts-r/reloadProjects` (the editor's "Reload Projects"): every config is read again, and
    /// every input file that is not open is read again from disk.
    fn reload_projects(&mut self) {
        let closed: BTreeSet<String> = self
            .projects
            .values()
            .flat_map(|p| p.inputs.iter())
            .filter(|p| !self.docs.contains_key(*p))
            .cloned()
            .collect();
        for p in closed {
            self.broadcast_doc(&p, None, Vec::new());
        }
        self.reload_configs();
    }

    /// A project loaded its program: watch the directories of its files and, when the set
    /// changed, tell bridges (`ts-r/inputsChanged`: `ts-r/usedBy` may answer differently).
    fn on_inputs(&mut self, project: &str, files: Vec<String>) {
        self.watch_files(&files);
        let Some(p) = self.projects.get_mut(project) else {
            return;
        };
        let files: HashSet<String> = files.into_iter().collect();
        if p.inputs != files {
            p.inputs = files;
            self.shared.send(jsonrpc::notification(
                "ts-r/inputsChanged",
                json!({"project": if project == INFERRED { Value::Null } else { json!(project) }}),
            ));
        }
    }

    /// Watches the directories of `files` (the watcher starts on first use).
    fn watch_files(&mut self, files: &[String]) {
        let mut dirs: Vec<String> = Vec::new();
        for f in files {
            if watch::ignored(f) {
                continue;
            }
            let dir = ts_r_config::path::directory_of(f);
            if !self.watched_dirs.contains(dir) {
                self.watched_dirs.insert(dir.to_string());
                dirs.push(dir.to_string());
            }
        }
        if dirs.is_empty() {
            return;
        }
        if self.watch.is_none() {
            if std::env::var("TS_R_LSP_WATCH").is_ok_and(|v| v == "off") {
                return;
            }
            let tx = self.tx.clone();
            match watch::InputWatch::start(move |changes| {
                let _ = tx.send(CMsg::Watched(changes));
            }) {
                Ok(w) => self.watch = Some(w),
                Err(e) => {
                    crate::daemon::log(&format!("ts-r: cannot watch project inputs: {e}"));
                    return;
                }
            }
        }
        if let Some(w) = &self.watch {
            w.add_dirs(dirs);
        }
    }

    /// Files changed on disk, as `(path, FileChangeType)`: from a client's
    /// `workspace/didChangeWatchedFiles` or from the input watcher.
    fn on_changes(&mut self, changes: Vec<(String, u64)>) {
        let mut config_changed = false;
        for (path, kind) in changes {
            let name = ts_r_config::path::file_name(&path);
            let created_or_deleted = matches!(kind, 1 | 3);
            // `ts-r.toml`, `tsrconfig.json`: the sound-mode configuration (projects re-read it
            // on reload).
            if name.ends_with(".json") || name == "ts-r.toml" || created_or_deleted {
                config_changed = true;
            }
            // A new directory may get sources: watch it too.
            if kind == 1
                && let Some(w) = &self.watch
                && !watch::ignored(&path)
                && Path::new(&path).is_dir()
            {
                w.add_new_dir(path.clone());
            }
            if !self.docs.contains_key(&path) {
                // Changed on disk and not open: projects re-read it if they use it.
                self.broadcast_doc(&path, None, Vec::new());
            }
        }
        if config_changed {
            self.reload_configs();
        }
    }

    /// Config files (or the set of files) changed: re-read configs, re-home open documents
    /// and give projects their new options and roots.
    fn reload_configs(&mut self) {
        self.configs.clear();
        let keys: Vec<String> = self.projects.keys().cloned().collect();
        for k in keys {
            if k == INFERRED {
                continue;
            }
            let info = self.config(&k);
            if let Some(info) = &info {
                self.watch_files(&info.inputs);
            }
            if let (Some(info), Some(p)) = (info, self.projects.get_mut(&k)) {
                p.config = Some(info.clone());
                let _ = p.tx.send(PMsg::Config {
                    options: Box::new(info.options.clone()),
                    roots: info.file_names.clone(),
                });
            }
        }
        let paths: Vec<String> = self.docs.keys().cloned().collect();
        for path in paths {
            let owner = self.owner_of(&path);
            let Some(doc) = self.docs.get(&path) else {
                continue;
            };
            if doc.owner == owner || doc.mirror {
                continue;
            }
            let (old, uri) = (doc.owner.clone(), doc.uri.clone());
            if let Some(p) = self.projects.get(&old) {
                let _ = p.tx.send(PMsg::Own {
                    path: path.clone(),
                    uri: uri.clone(),
                    own: false,
                });
            }
            self.ensure_project(&owner);
            if let Some(p) = self.projects.get(&owner) {
                let _ = p.tx.send(PMsg::Own {
                    path: path.clone(),
                    uri,
                    own: true,
                });
            }
            if let Some(d) = self.docs.get_mut(&path) {
                d.owner = owner;
            }
        }
        self.send_status();
    }

    // ------------------------------------------------------------------------------------
    // Projects
    // ------------------------------------------------------------------------------------

    /// Parses (once) a config file.
    fn config(&mut self, path: &str) -> Option<Arc<ConfigInfo>> {
        if let Some(c) = self.configs.get(path) {
            return c.clone();
        }
        let fs = RealFs::new();
        let info = if fs.file_exists(path) {
            let parsed = ts_r_config::load_tsconfig(&fs, path);
            let mut inputs = vec![parsed.config_path.clone()];
            inputs.extend(parsed.extended_config_paths.iter().cloned());
            Some(Arc::new(ConfigInfo {
                path: path.to_string(),
                set: parsed.file_names.iter().cloned().collect(),
                file_names: parsed.file_names,
                options: parsed.options,
                references: parsed
                    .references
                    .iter()
                    .map(|r| r.config_path.clone())
                    .collect(),
                inputs,
                diagnostics: parsed.diagnostics,
            }))
        } else {
            None
        };
        self.configs.insert(path.to_string(), info.clone());
        info
    }

    /// The project that owns `path`: the nearest `tsconfig.json` / `jsconfig.json` upwards
    /// whose files include it (or one of the configs it references), else the inferred
    /// project.
    fn owner_of(&mut self, path: &str) -> String {
        let mut dir = ts_r_config::path::directory_of(path).to_string();
        let mut guard = 0;
        while guard < 128 {
            guard += 1;
            for name in ["tsconfig.json", "jsconfig.json"] {
                let cand = format!("{}/{name}", dir.trim_end_matches('/'));
                let Some(info) = self.config(&cand) else {
                    continue;
                };
                if info.set.contains(path) {
                    return cand;
                }
                for r in info.references.clone() {
                    if let Some(ri) = self.config(&r)
                        && ri.set.contains(path)
                    {
                        return r;
                    }
                }
            }
            let parent = ts_r_config::path::directory_of(&dir).to_string();
            if parent == dir || parent.is_empty() {
                break;
            }
            dir = parent;
        }
        INFERRED.to_string()
    }

    fn ensure_project(&mut self, key: &str) {
        if self.projects.contains_key(key) {
            return;
        }
        let config = if key == INFERRED {
            None
        } else {
            self.config(key)
        };
        let (options, roots) = match &config {
            Some(c) => (c.options.clone(), c.file_names.clone()),
            None => (CompilerOptions::new(), Vec::new()),
        };
        if let Some(c) = &config {
            // The tsconfig chain; the config's directory also sees new configs and sources.
            self.watch_files(&c.inputs);
        }
        let name = self.display_name(key);
        let project = Project::new(
            key.to_string(),
            name.clone(),
            self.shared.clone(),
            options,
            roots,
        );
        let (tx, rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name(format!("tsr-project {name}"))
            .stack_size(PROJECT_STACK)
            .spawn(move || project.run(rx))
            .ok();
        // Bring it up to date with every open document.
        for (path, d) in &self.docs {
            let _ = tx.send(PMsg::Doc {
                path: path.clone(),
                text: Some(d.shared.clone()),
                edits: Vec::new(),
            });
        }
        crate::daemon::log(&format!("ts-r: project {name} started"));
        self.projects.insert(
            key.to_string(),
            ProjectHandle {
                tx,
                handle,
                config: config.clone(),
                state: ProjectState::Checking,
                parity: None,
                inputs: HashSet::new(),
            },
        );
        if let Some(c) = config {
            self.parity.request(c, self.tx.clone());
        } else if let Some(p) = self.projects.get_mut(key) {
            p.state = ProjectState::Unknown;
        }
        self.send_status();
    }

    fn display_name(&self, key: &str) -> String {
        if key == INFERRED {
            return key.to_string();
        }
        let root = self.shared.root.to_string_lossy().replace('\\', "/");
        key.strip_prefix(&format!("{}/", root.trim_end_matches('/')))
            .unwrap_or(key)
            .to_string()
    }

    // ------------------------------------------------------------------------------------
    // Parity and fallback (K-17)
    // ------------------------------------------------------------------------------------

    fn on_parity(&mut self, o: parity::Outcome) {
        let key = o.config.clone();
        let Some(p) = self.projects.get_mut(&key) else {
            return;
        };
        let fallback_now = matches!(o.summary.result, parity::Result::Mismatch);
        p.state = match o.summary.result {
            parity::Result::Match => ProjectState::Matched,
            parity::Result::Mismatch => ProjectState::Fallback,
            parity::Result::Error => ProjectState::Unknown,
        };
        crate::daemon::log(&format!(
            "ts-r: parity {}: {} ({})",
            o.summary.project,
            o.summary.result.as_str(),
            o.summary.detail
        ));
        p.parity = Some(o.summary);
        if fallback_now {
            let _ = p.tx.send(PMsg::Fallback(true));
            self.start_fallback_for(&key);
        }
        self.send_status();
    }

    /// Opens the project's documents in the user's TypeScript (started on first use).
    fn start_fallback_for(&mut self, key: &str) {
        if self.fallback.is_none() {
            if !self.docs.values().any(|d| d.owner == key && !d.mirror) {
                // Nothing to check yet (e.g. a bridge only asked `ts-r/projectInfo`): start
                // the process when one of the project's documents opens.
                return;
            }
            match fallback::Fallback::start(&self.shared.root, self.tx.clone()) {
                Ok(f) => {
                    let pid = f.pid().unwrap_or(0);
                    self.shared.fallback_pid.store(pid, Ordering::Relaxed);
                    self.fallback = Some(f);
                }
                Err(e) => {
                    crate::daemon::log(&format!("ts-r: cannot start the fallback TypeScript: {e}"));
                    if let Some(p) = self.projects.get_mut(key) {
                        p.state = ProjectState::Unknown;
                        let _ = p.tx.send(PMsg::Fallback(false));
                    }
                    return;
                }
            }
        }
        let Some(f) = self.fallback.as_mut() else {
            return;
        };
        for (path, d) in &self.docs {
            if d.owner == key && !d.mirror {
                f.open(path, d.text.text());
            }
        }
    }

    fn on_fallback(&mut self, e: fallback::Event) {
        match e {
            fallback::Event::Message(v) => {
                let Some(f) = self.fallback.as_mut() else {
                    return;
                };
                let Some((path, diagnostics)) = f.on_message(&v) else {
                    return;
                };
                let Some(doc) = self.docs.get(&path) else {
                    return;
                };
                let in_fallback = self.projects.get(&doc.owner).map(|p| &p.state)
                    == Some(&ProjectState::Fallback);
                if !in_fallback || Some(doc.text.text()) != f.text_of(&path) {
                    return;
                }
                self.shared.send(jsonrpc::notification(
                    "textDocument/publishDiagnostics",
                    json!({"uri": doc.uri, "diagnostics": diagnostics}),
                ));
            }
            fallback::Event::Exited(why) => {
                crate::daemon::log(&format!("ts-r: fallback TypeScript exited ({why})"));
                self.fallback = None;
                self.shared.fallback_pid.store(0, Ordering::Relaxed);
                self.fallback_exits += 1;
                let keys: Vec<String> = self
                    .projects
                    .iter()
                    .filter(|(_, p)| p.state == ProjectState::Fallback)
                    .map(|(k, _)| k.clone())
                    .collect();
                if self.fallback_exits > MAX_FALLBACK_RESTARTS {
                    crate::daemon::log("ts-r: fallback TypeScript keeps exiting; using ts-r");
                    for k in &keys {
                        if let Some(p) = self.projects.get_mut(k) {
                            p.state = ProjectState::Unknown;
                            let _ = p.tx.send(PMsg::Fallback(false));
                        }
                    }
                } else {
                    for k in keys {
                        self.start_fallback_for(&k);
                    }
                }
                self.send_status();
            }
        }
    }

    fn status(&self) -> Value {
        let projects: Vec<Value> = self
            .projects
            .iter()
            .map(|(k, p)| {
                let open = self
                    .docs
                    .values()
                    .filter(|d| &d.owner == k && !d.mirror)
                    .count();
                json!({
                    "project": self.display_name(k),
                    "config": if k == INFERRED { Value::Null } else { json!(k) },
                    "state": p.state.as_str(),
                    "openFiles": open,
                    "parity": p.parity.as_ref().map(parity::Summary::to_json),
                })
            })
            .collect();
        let fallback = self
            .projects
            .values()
            .filter(|p| p.state == ProjectState::Fallback)
            .count();
        let checking = self
            .projects
            .values()
            .filter(|p| p.state == ProjectState::Checking)
            .count();
        let total = self.projects.len();
        let summary = if fallback == 0 {
            format!("ts-r checker on {total} project(s)")
                + &if checking > 0 {
                    format!(", parity check running for {checking}")
                } else {
                    String::new()
                }
        } else {
            format!(
                "ts-r checker on {} of {total} project(s); {fallback} use your TypeScript for errors (parity difference)",
                total - fallback
            )
        };
        json!({
            "backend": "ts-r",
            "version": env!("CARGO_PKG_VERSION"),
            "summary": summary,
            "fallbackTypeScript": self.fallback.as_ref().map(|f| f.label()),
            "fallbackPid": self.fallback.as_ref().and_then(|f| f.pid()),
            "projects": projects,
        })
    }

    fn send_status(&self) {
        self.shared
            .send(jsonrpc::notification("ts-r/status", self.status()));
    }
}

/// Server capabilities of the ts-r backend.
pub fn capabilities() -> Value {
    json!({
        "positionEncoding": "utf-16",
        "textDocumentSync": {"openClose": true, "change": 2, "save": {"includeText": false}},
        "hoverProvider": true,
        "definitionProvider": true,
        "typeDefinitionProvider": true,
        "referencesProvider": true,
        "documentHighlightProvider": true,
        "renameProvider": {"prepareProvider": true},
        "completionProvider": {
            "triggerCharacters": [".", "\"", "'", "`", "/", "@", "<", "#"],
            "resolveProvider": true,
        },
        "signatureHelpProvider": {"triggerCharacters": ["(", ",", "<"], "retriggerCharacters": [")"]},
        "documentSymbolProvider": true,
        "codeActionProvider": {"codeActionKinds": features::actions::KINDS},
        "workspace": {"fileOperations": {"willRename": {"filters": [
            {"scheme": "file", "pattern": {"glob": "**/*.{ts,tsx,mts,cts,js,jsx,mjs,cjs}", "matches": "file"}},
            {"scheme": "file", "pattern": {"glob": "**/*", "matches": "folder"}},
        ]}}},
        "semanticTokensProvider": {
            "legend": features::semantic_legend(),
            "full": true,
            "range": false,
        },
    })
}

/// Position of an LSP request (`params.position`).
/// `workspace/willRenameFiles` / `didRenameFiles` params as (old path, new path) pairs.
pub fn rename_pairs(params: &Value) -> Vec<(String, String)> {
    params["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let from = crate::uri::uri_to_path(f["oldUri"].as_str()?)?;
            let to = crate::uri::uri_to_path(f["newUri"].as_str()?)?;
            Some((from, to))
        })
        .collect()
}

pub fn position(params: &Value) -> Option<Pos> {
    Pos::from_lsp(params.get("position")?)
}

/// Whether a backend spec is the ts-r backend.
pub fn is_tsr(spec: &BackendSpec) -> bool {
    spec.label.starts_with("ts-r ")
}

/// The spec of the ts-r backend for a workspace root.
pub fn spec(root: &Path) -> BackendSpec {
    BackendSpec {
        protocol: Protocol::Lsp,
        label: format!("ts-r {}", env!("CARGO_PKG_VERSION")),
        version: env!("CARGO_PKG_VERSION").to_string(),
        program: PathBuf::new(),
        args: Vec::new(),
        env: Vec::new(),
        cwd: root.to_path_buf(),
    }
}
