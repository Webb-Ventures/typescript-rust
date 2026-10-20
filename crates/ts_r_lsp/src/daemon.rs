//! The shared per-workspace daemon.
//!
//! One event loop thread owns all state; socket readers, backend readers and the memory
//! sampler only send [`Event`]s into its channel, so every client's messages are handled in
//! the order they were sent and no state is shared between threads.
//!
//! * **Clients** are fronts: LSP clients (`ts-r-lsp` on an editor's stdio) and tsserver
//!   clients (the VS Code stand-in). Each tracks its own open documents.
//! * **Slots** each run one backend. In shared mode all clients that need the same backend
//!   share one slot; documents are multiplexed with [`Mirror`] (see `docs.rs`). When two
//!   clients edit the same unsaved file differently, the slot is split and every client gets
//!   its own slot ("per-client mode", K-12) until only one client is left.
//! * **Memory**: backend RSS is sampled every `sample_interval` (the in-process ts-r backend
//!   counts as the daemon's RSS plus its helper processes). Over `ts-r.memoryBudgetMB`
//!   while clients have been quiet for `restart_idle`, the backend is restarted and open
//!   documents are replayed. No activity for `ts-r.idleShutdownMinutes` stops every backend
//!   (the next request restarts it); no clients for `linger` ends the daemon.

use crate::backend::typescript::{self, TsEntry, TsServerOptions};
use crate::backend::{Backend, BackendEvent, BackendFactory, BackendSpec, Protocol};
use crate::docs::{ClientDoc, ClientId, Diverged, Members, Mirror, SyncOp};
use crate::ipc::{self, Endpoint, Stream};
use crate::jsonrpc::{self, IdKey, Kind, codes};
use crate::lint::{self, LintWorker};
use crate::process;
use crate::settings::{ClientLint, LintEnable, Settings, TypeBackend};
use crate::text::Pos;
use crate::translate::{self, TsReqKind};
use crate::uri;
use crate::wire::{self, FrontKind, Hello};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

/// Slot identifier.
pub type SlotId = u64;

/// Something the event loop must handle.
#[derive(Debug)]
pub enum Event {
    /// A front connected.
    Accepted(Stream),
    /// A front sent a message.
    ClientMsg(ClientId, Value),
    /// A front disconnected.
    ClientGone(ClientId),
    /// A backend produced output or exited.
    Backend {
        /// Slot.
        slot: SlotId,
        /// Backend generation (stale generations are ignored).
        generation: u64,
        /// What happened.
        ev: BackendEvent,
    },
    /// Memory sample: `(slot, generation, tree RSS)` and the daemon's own RSS.
    Rss(Vec<(SlotId, u64, Option<u64>)>, Option<u64>),
    /// The lint worker finished a job.
    Lint(Box<lint::Done>),
}

/// How often the event loop wakes up at the latest.
const TICK: Duration = Duration::from_millis(250);
/// Debounce between an edit and the diagnostics request for LSP clients of tsserver.
const GETERR_DEBOUNCE: Duration = Duration::from_millis(120);
/// Pending tsserver requests without an answer are forgotten after this long.
const PENDING_GC: Duration = Duration::from_secs(600);

#[derive(Debug)]
enum Pending {
    /// LSP request from an LSP client to an LSP backend.
    Lsp { client: ClientId, id: Value },
    /// LSP request translated to tsserver.
    Translated {
        client: ClientId,
        id: Value,
        kind: TsReqKind,
    },
    /// tsserver request from a tsserver client.
    Ts {
        client: ClientId,
        seq: Value,
        geterr: bool,
    },
    /// Our own `initialize` to an LSP backend.
    Init,
    /// Our own diagnostics request (for LSP clients of tsserver).
    Geterr,
    /// `textDocument/codeAction` to an LSP backend; lint actions are appended to the answer
    /// if the client's text is still the text they were made for.
    CodeAction {
        client: ClientId,
        id: Value,
        lint: Vec<Value>,
        /// Document key and the lint results `lint` came from.
        linted: Option<(String, Arc<lint::Linted>)>,
    },
    /// Response not needed.
    Ignore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotState {
    Stopped,
    /// LSP backend: `initialize` sent, waiting for the result. Requests are queued.
    Starting,
    Ready,
}

#[derive(Debug)]
struct Slot {
    key: String,
    spec: BackendSpec,
    backend: Option<Box<dyn Backend>>,
    generation: u64,
    state: SlotState,
    members: BTreeSet<ClientId>,
    mirror: Mirror,
    pending: HashMap<i64, (Pending, Instant)>,
    queue: Vec<Value>,
    next_seq: i64,
    // LSP backends.
    init_params: Option<Value>,
    init_result: Option<Value>,
    init_waiters: Vec<(ClientId, Value)>,
    registrations: BTreeMap<String, Value>,
    last_config: Option<Value>,
    // tsserver backends.
    ts_configure: Option<Value>,
    ts_inferred: Option<Value>,
    diags: BTreeMap<String, [Option<Value>; 3]>,
    geterr_due: Option<Instant>,
    geterr_inflight: Option<i64>,
    geterr_again: bool,
    cancel_prefix: Option<String>,
    cancel_files: BTreeSet<i64>,
    // Memory and lifecycle.
    rss: Option<u64>,
    started_at: Option<Instant>,
    budget_marker: Option<Instant>,
    crashes: Vec<Instant>,
    restarts_budget: u32,
    restarts_crash: u32,
    idle_stops: u32,
    first_ready: Option<Duration>,
    /// Last `ts-r/status` (ts-r backend).
    tsr_status: Option<Value>,
}

#[derive(Debug)]
struct Client {
    kind: Option<FrontKind>,
    name: String,
    pid: u32,
    tx: Sender<Vec<u8>>,
    conn: Stream,
    slot: Option<SlotId>,
    docs: BTreeMap<String, ClientDoc>,
    init_params: Option<Value>,
    last_activity: Instant,
    /// LSP request ids already answered with RequestCancelled.
    cancelled: HashSet<IdKey>,
    /// `ts-r.lint.enable` for this client (LSP clients only lint).
    lint: LintEnable,
    /// Lint-only client (the VS Code extension's lint connection): no type backend.
    lint_only: bool,
    /// Configs whose unsupported-rules note this client has been sent.
    lint_notes_seen: BTreeSet<String>,
    /// Whether this client has had its one `window/showMessage` about unsupported rules.
    lint_popup_shown: bool,
    /// Last type-backend diagnostics per document key, published together with lint's.
    type_diags: BTreeMap<String, Vec<Value>>,
}

/// Lint state of one open document.
#[derive(Debug, Default)]
struct LintFile {
    /// Last results per lint client: of the newest linted text equal to the client's text
    /// at the time. Clients with different unsaved text (K-12) keep their own.
    linted: BTreeMap<ClientId, Arc<lint::Linted>>,
    /// Governing config (relative name).
    config: Option<String>,
}

/// Lint counters for `status`.
#[derive(Debug, Default)]
struct LintStats {
    runs: u64,
    total: Duration,
    max: Duration,
    last: Duration,
    parse_errors: u64,
    config_reloads: u64,
}

#[derive(Debug)]
struct ServerReq {
    slot: SlotId,
    backend_id: Value,
    client: ClientId,
}

/// Member view of one slot for [`Mirror`].
struct View<'a> {
    clients: &'a BTreeMap<ClientId, Client>,
    members: &'a BTreeSet<ClientId>,
}

impl Members for View<'_> {
    fn docs_for(&self, key: &str) -> Vec<(ClientId, &ClientDoc)> {
        self.members
            .iter()
            .filter_map(|c| {
                self.clients
                    .get(c)
                    .and_then(|cl| cl.docs.get(key))
                    .map(|d| (*c, d))
            })
            .collect()
    }
    fn all_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .members
            .iter()
            .filter_map(|c| self.clients.get(c))
            .flat_map(|c| c.docs.keys().cloned())
            .collect();
        keys.sort();
        keys.dedup();
        keys
    }
}

fn disk(key: &str) -> Option<String> {
    std::fs::read_to_string(key).ok()
}

/// Edits of one document change, as `(range or whole text, new text)` applied in order.
type Edits = Vec<(Option<(Pos, Pos)>, String)>;

enum DocChange {
    Open(ClientDoc),
    Edits(Edits),
    Close,
}

/// What a client's backend will be.
// Built once per client hello and dropped right away, so its size does not matter.
#[allow(clippy::large_enum_variant)]
enum Choice {
    /// The project's TypeScript.
    TypeScript {
        key: String,
        install: typescript::TsInstall,
        entry: TsEntry,
        opts: TsServerOptions,
    },
    /// Our checker, in this process.
    TsR,
}

impl Choice {
    fn key(&self) -> &str {
        match self {
            Choice::TypeScript { key, .. } => key,
            Choice::TsR => "ts-r",
        }
    }
}

/// Counters reported by `status`.
#[derive(Debug, Default)]
struct Stats {
    splits: u32,
    clients_served: u32,
}

/// The daemon.
#[derive(Debug)]
pub struct Daemon {
    root: PathBuf,
    cancel_dir: PathBuf,
    settings: Settings,
    factory: Box<dyn BackendFactory>,
    /// Starts the in-process ts-r backend.
    tsr_factory: Box<dyn BackendFactory>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    clients: BTreeMap<ClientId, Client>,
    slots: BTreeMap<SlotId, Slot>,
    per_client: bool,
    next_client: ClientId,
    next_slot: SlotId,
    next_server_req: u64,
    server_requests: HashMap<String, ServerReq>,
    last_activity: Instant,
    last_client_left: Option<Instant>,
    next_sample: Instant,
    started: Instant,
    daemon_rss: Option<u64>,
    stats: Stats,
    stopping: bool,
    socket_path: Option<PathBuf>,
    lint_worker: Option<LintWorker>,
    lint_files: BTreeMap<String, LintFile>,
    /// Debounced lint requests: when, and whose texts.
    lint_due: BTreeMap<String, (Instant, BTreeSet<ClientId>)>,
    lint_notes: BTreeMap<String, lint::ConfigNote>,
    lint_stats: LintStats,
    /// Last `ts-r/status` of a ts-r backend (sent to clients as they join).
    tsr_status: Option<Value>,
}

impl Daemon {
    /// A daemon for `root` (canonical). Fronts are fed in with [`Daemon::sender`] /
    /// [`Daemon::accept_loop`].
    pub fn new(
        root: PathBuf,
        cancel_dir: PathBuf,
        settings: Settings,
        factory: Box<dyn BackendFactory>,
    ) -> Daemon {
        let (tx, rx) = mpsc::channel();
        let now = Instant::now();
        let _ = std::fs::create_dir_all(&cancel_dir);
        Daemon {
            root,
            cancel_dir,
            next_sample: now + settings.sample_interval,
            settings,
            factory,
            tsr_factory: Box::new(crate::tsr::TsrFactory),
            tx,
            rx,
            clients: BTreeMap::new(),
            slots: BTreeMap::new(),
            per_client: false,
            next_client: 1,
            next_slot: 1,
            next_server_req: 1,
            server_requests: HashMap::new(),
            last_activity: now,
            last_client_left: Some(now),
            started: now,
            daemon_rss: None,
            stats: Stats::default(),
            stopping: false,
            socket_path: None,
            lint_worker: None,
            lint_files: BTreeMap::new(),
            lint_due: BTreeMap::new(),
            lint_notes: BTreeMap::new(),
            lint_stats: LintStats::default(),
            tsr_status: None,
        }
    }

    /// A sender for events (the accept thread uses it).
    pub fn sender(&self) -> Sender<Event> {
        self.tx.clone()
    }

    /// Accepts connections forever, forwarding them to the event loop.
    pub fn accept_loop(listener: ipc::Listener, tx: Sender<Event>) {
        loop {
            match listener.accept() {
                Ok(s) => {
                    if tx.send(Event::Accepted(s)).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    eprintln!("ts-r-lsp: accept failed: {e}");
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }

    /// Runs the event loop until the daemon should exit.
    pub fn run(mut self) {
        log(&format!(
            "daemon {} serving {}",
            std::process::id(),
            self.root.display()
        ));
        while !self.stopping {
            match self.rx.recv_timeout(self.wait_time()) {
                Ok(ev) => self.handle(ev),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            // Drain whatever else is queued before running timers.
            while let Ok(ev) = self.rx.try_recv() {
                self.handle(ev);
            }
            self.tick();
        }
        // Unlink the socket first so new fronts start a fresh daemon instead of connecting
        // to this one while it shuts down.
        if let Some(p) = &self.socket_path {
            let _ = std::fs::remove_file(p);
        }
        let ids: Vec<SlotId> = self.slots.keys().copied().collect();
        for sid in ids {
            self.stop_slot(sid, "daemon exit");
        }
        for c in self.clients.values() {
            c.conn.shutdown();
        }
        let _ = std::fs::remove_dir_all(&self.cancel_dir);
        log("daemon exiting");
        // Give backend writers a moment to deliver `exit`; backends also exit when their
        // stdin closes with this process.
        thread::sleep(Duration::from_millis(50));
    }

    fn wait_time(&self) -> Duration {
        let now = Instant::now();
        let mut next = now + TICK;
        for s in self.slots.values() {
            if let Some(d) = s.geterr_due {
                next = next.min(d);
            }
        }
        for (d, _) in self.lint_due.values() {
            next = next.min(*d);
        }
        next.saturating_duration_since(now)
    }

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::Accepted(stream) => self.on_accept(stream),
            Event::ClientMsg(cid, msg) => self.on_client_msg(cid, msg),
            Event::ClientGone(cid) => self.remove_client(cid, "disconnected"),
            Event::Backend {
                slot,
                generation,
                ev,
            } => self.on_backend(slot, generation, ev),
            Event::Rss(samples, own) => self.on_rss(samples, own),
            Event::Lint(done) => self.on_lint_done(*done),
        }
    }

    // ------------------------------------------------------------------------------------
    // Connections
    // ------------------------------------------------------------------------------------

    fn on_accept(&mut self, stream: Stream) {
        let cid = self.next_client;
        self.next_client += 1;
        let (Ok(read_half), Ok(write_half)) = (stream.try_clone(), stream.try_clone()) else {
            return;
        };
        let (wtx, wrx) = mpsc::channel::<Vec<u8>>();
        let _ = thread::Builder::new()
            .name(format!("client-{cid}-writer"))
            .spawn(move || {
                let mut w = write_half;
                for body in wrx {
                    if jsonrpc::write_frame(&mut w, &body).is_err() {
                        break;
                    }
                }
                w.shutdown();
            });
        let tx = self.tx.clone();
        let _ = thread::Builder::new()
            .name(format!("client-{cid}-reader"))
            .spawn(move || {
                let mut r = BufReader::with_capacity(1 << 16, read_half);
                while let Ok(Some(body)) = jsonrpc::read_frame(&mut r) {
                    match serde_json::from_slice::<Value>(&body) {
                        Ok(v) => {
                            if tx.send(Event::ClientMsg(cid, v)).is_err() {
                                return;
                            }
                        }
                        Err(e) => log(&format!("client {cid}: bad JSON ({e})")),
                    }
                }
                let _ = tx.send(Event::ClientGone(cid));
            });
        self.clients.insert(
            cid,
            Client {
                kind: None,
                name: format!("client {cid}"),
                pid: 0,
                tx: wtx,
                conn: stream,
                slot: None,
                docs: BTreeMap::new(),
                init_params: None,
                last_activity: Instant::now(),
                cancelled: HashSet::new(),
                lint: LintEnable::Auto,
                lint_only: false,
                lint_notes_seen: BTreeSet::new(),
                lint_popup_shown: false,
                type_diags: BTreeMap::new(),
            },
        );
    }

    fn send_client(&self, cid: ClientId, msg: &Value) {
        if let Some(c) = self.clients.get(&cid)
            && let Ok(body) = serde_json::to_vec(msg)
        {
            let _ = c.tx.send(body);
        }
    }

    fn on_hello(&mut self, cid: ClientId, msg: &Value) {
        let hello = match Hello::from_json(msg) {
            Ok(h) => h,
            Err(e) => {
                self.refuse(cid, &e, "bad hello");
                return;
            }
        };
        let root =
            std::fs::canonicalize(&hello.root).unwrap_or_else(|_| PathBuf::from(&hello.root));
        if root != self.root {
            let e = format!(
                "this daemon serves {}, not {}",
                self.root.display(),
                root.display()
            );
            self.refuse(cid, &e, "wrong root");
            return;
        }
        if hello.kind == FrontKind::Status {
            let status = self.status_json();
            self.send_client(cid, &json!({"tsr": "status", "status": status}));
            if let Some(c) = self.clients.get_mut(&cid) {
                c.kind = Some(FrontKind::Status);
            }
            return;
        }
        for w in self.settings.apply_json(&hello.settings) {
            log(&w);
        }
        let cl = ClientLint::from_json(&hello.settings);
        let lint_only = hello.kind == FrontKind::Lsp && cl.lint_only == Some(true);
        if let Some(c) = self.clients.get_mut(&cid) {
            c.lint = cl.enable.unwrap_or(LintEnable::Auto);
            c.lint_only = lint_only;
        }
        if lint_only {
            if let Some(c) = self.clients.get_mut(&cid) {
                c.kind = Some(hello.kind);
                c.name = hello.name.clone();
                c.pid = hello.pid;
            }
            self.stats.clients_served += 1;
            self.last_client_left = None;
            log(&format!("{} (lsp, lint only) joined", hello.name));
            self.send_client(
                cid,
                &json!({"tsr": "welcome", "daemonPid": std::process::id(), "backend": "ts-r-lint", "slot": null, "notes": []}),
            );
            self.touch(cid);
            return;
        }
        let notes: Vec<&str> = self.settings.l1_notes();
        match self.assign_slot(cid, &hello) {
            Ok(sid) => {
                let label = self.slots[&sid].spec.label.clone();
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.kind = Some(hello.kind);
                    c.name = hello.name.clone();
                    c.pid = hello.pid;
                    c.slot = Some(sid);
                }
                self.slots.get_mut(&sid).expect("slot").members.insert(cid);
                self.stats.clients_served += 1;
                self.last_client_left = None;
                log(&format!(
                    "{} ({}) joined slot {sid} [{label}]",
                    hello.name,
                    hello.kind.as_str()
                ));
                self.send_client(
                    cid,
                    &json!({"tsr": "welcome", "daemonPid": std::process::id(), "backend": label, "slot": sid, "notes": notes}),
                );
                self.touch(cid);
                self.ensure_started(sid);
            }
            Err(e) => {
                log(&format!("{}: {e}", hello.name));
                self.refuse(cid, &e, "no backend");
            }
        }
    }

    fn choose_backend(&self, hello: &Hello) -> Result<Choice, String> {
        // Our checker for LSP clients (tsserver clients reach it through the tsserver
        // bridge once it lands; until then they keep the project's TypeScript).
        if self.settings.type_backend == TypeBackend::TsR
            && hello.kind == FrontKind::Lsp
            && crate::tsr::libs().is_some()
        {
            return Ok(Choice::TsR);
        }
        let found = typescript::find_typescript(&self.root);
        let fallback = || {
            hello
                .fallback_tsserver
                .as_deref()
                .and_then(|p| typescript::typescript_at(Path::new(p)))
        };
        let install = match (hello.kind, &found) {
            (FrontKind::Lsp, Some(ts)) => ts.clone(),
            (_, Some(ts)) if ts.protocol() == Protocol::TsServer => ts.clone(),
            (FrontKind::TsServer, _) => fallback().filter(|f| f.protocol() == Protocol::TsServer).ok_or_else(|| {
                match &found {
                    Some(ts) => format!(
                        "TypeScript {} has no tsserver and no fallback tsserver was given; use the LSP mode",
                        ts.version
                    ),
                    None => format!("no TypeScript found in node_modules of {} or its parents", self.root.display()),
                }
            })?,
            _ => fallback().ok_or_else(|| {
                format!("no TypeScript found in node_modules of {} or its parents", self.root.display())
            })?,
        };
        let entry = install.entry.clone();
        let key = match &entry {
            TsEntry::TsServer(js) => format!("tsserver:{}", js.display()),
            TsEntry::Native(exe) => format!("lsp:{}", exe.display()),
        };
        let opts = TsServerOptions {
            cancellation_pipe: None,
            locale: hello.tsserver.locale.clone(),
            global_plugins: hello.tsserver.global_plugins.clone(),
            plugin_probe_locations: hello.tsserver.plugin_probe_locations.clone(),
            node_fallback: hello.node_path.as_ref().map(PathBuf::from),
        };
        Ok(Choice::TypeScript {
            key,
            install,
            entry,
            opts,
        })
    }

    fn assign_slot(&mut self, cid: ClientId, hello: &Hello) -> Result<SlotId, String> {
        let choice = self.choose_backend(hello)?;
        if !self.per_client
            && let Some((&sid, _)) = self.slots.iter().find(|(_, s)| s.key == choice.key())
        {
            return Ok(sid);
        }
        let _ = cid;
        self.create_slot(&choice)
    }

    fn create_slot(&mut self, choice: &Choice) -> Result<SlotId, String> {
        let sid = self.next_slot;
        self.next_slot += 1;
        let mut cancel_prefix = None;
        let Choice::TypeScript {
            key,
            install,
            entry,
            opts,
        } = choice
        else {
            let spec = crate::tsr::spec(&self.root);
            self.slots
                .insert(sid, Slot::new("ts-r".to_string(), spec, None));
            return Ok(sid);
        };
        let spec = match entry {
            TsEntry::TsServer(js) => {
                let prefix = self
                    .cancel_dir
                    .join(format!("s{sid}-"))
                    .to_string_lossy()
                    .into_owned();
                let mut opts = opts.clone();
                opts.cancellation_pipe = Some(format!("{prefix}*"));
                cancel_prefix = Some(prefix);
                typescript::tsserver_spec(install, js, &self.root, &opts)
                    .ok_or("cannot find `node` to run tsserver (set TS_R_LSP_NODE)")?
            }
            TsEntry::Native(exe) => typescript::native_lsp_spec(install, exe, &self.root),
        };
        self.slots
            .insert(sid, Slot::new(key.clone(), spec, cancel_prefix));
        Ok(sid)
    }

    fn touch(&mut self, cid: ClientId) {
        let now = Instant::now();
        self.last_activity = now;
        if let Some(c) = self.clients.get_mut(&cid) {
            c.last_activity = now;
        }
    }

    /// Answers a hello with an error and drops the client once the error has been written
    /// (the writer thread closes the connection when its queue ends).
    fn refuse(&mut self, cid: ClientId, message: &str, why: &str) {
        self.send_client(cid, &json!({"tsr": "error", "message": message}));
        self.drop_client(cid, why, false);
    }

    fn remove_client(&mut self, cid: ClientId, why: &str) {
        self.drop_client(cid, why, true);
    }

    fn drop_client(&mut self, cid: ClientId, why: &str, close_now: bool) {
        let Some(client) = self.clients.remove(&cid) else {
            return;
        };
        if close_now {
            client.conn.shutdown();
        }
        if client.kind.is_some_and(|k| k != FrontKind::Status) {
            log(&format!("{} left ({why})", client.name));
        }
        // Answer server->client requests this client will never answer.
        let orphaned: Vec<String> = self
            .server_requests
            .iter()
            .filter(|(_, r)| r.client == cid)
            .map(|(k, _)| k.clone())
            .collect();
        for k in orphaned {
            if let Some(r) = self.server_requests.remove(&k) {
                self.send_backend(r.slot, jsonrpc::response(r.backend_id, Value::Null));
            }
        }
        // Other clients stay (another backend's editors, a lint connection): nothing keeps
        // the daemon lingering for this slot, so an emptied slot is stopped at once (the VS Code
        // bridge drops its on-demand tsserver this way).
        let others_stay = self.clients.values().any(is_active);
        if let Some(sid) = client.slot
            && let Some(slot) = self.slots.get_mut(&sid)
        {
            slot.members.remove(&cid);
            slot.init_waiters.retain(|(c, _)| *c != cid);
            if slot.members.is_empty() && !self.per_client && others_stay {
                self.stop_slot(sid, "last client of the slot left");
                self.slots.remove(&sid);
            } else if slot.members.is_empty() && self.per_client {
                self.stop_slot(sid, "last client of a per-client slot left");
                self.slots.remove(&sid);
            } else if slot.state == SlotState::Ready {
                self.reconcile_slot(sid);
            }
        }
        for key in client.docs.keys() {
            self.lint_forget_if_unused(key);
        }
        let active = self.clients.values().filter(|c| c.slot.is_some()).count();
        let active_any = self.clients.values().filter(|c| is_active(c)).count();
        if self.per_client && active <= 1 {
            self.per_client = false;
            // Keep one slot per backend key; the others have no members left.
            log("back to one shared backend");
        }
        if active_any == 0 && self.last_client_left.is_none() {
            self.last_client_left = Some(Instant::now());
        }
    }

    // ------------------------------------------------------------------------------------
    // Client messages
    // ------------------------------------------------------------------------------------

    fn on_client_msg(&mut self, cid: ClientId, msg: Value) {
        let Some(client) = self.clients.get(&cid) else {
            return;
        };
        let kind = client.kind;
        if kind.is_none() {
            self.on_hello(cid, &msg);
            return;
        }
        if let Some(ctl) = wire::control_kind(&msg) {
            self.on_control(cid, ctl, &msg);
            return;
        }
        let Some(sid) = client.slot else {
            if client.lint_only {
                self.touch(cid);
                self.on_lint_only_client(cid, msg);
            }
            return;
        };
        self.touch(cid);
        if !self.slots.contains_key(&sid) {
            return;
        }
        if self.slots[&sid].backend.is_none() {
            self.ensure_started(sid);
        }
        match kind {
            Some(FrontKind::Lsp) => self.on_lsp_client(cid, sid, msg),
            Some(FrontKind::TsServer) => self.on_ts_client(cid, sid, msg),
            _ => {}
        }
    }

    fn on_control(&mut self, cid: ClientId, ctl: &str, msg: &Value) {
        match ctl {
            "cancel" => {
                let Some(seq) = msg.get("seq") else { return };
                let Some(sid) = self.clients.get(&cid).and_then(|c| c.slot) else {
                    return;
                };
                let found = self.slots.get(&sid).and_then(|s| {
                    s.pending.iter().find_map(|(bseq, (p, _))| match p {
                        Pending::Ts {
                            client, seq: s2, ..
                        } if *client == cid && s2 == seq => Some(*bseq),
                        _ => None,
                    })
                });
                if let Some(bseq) = found {
                    self.cancel_ts(sid, bseq);
                }
            }
            "status" => {
                let status = self.status_json();
                self.send_client(cid, &json!({"tsr": "status", "status": status}));
            }
            "stop" => {
                log("stop requested");
                self.stopping = true;
            }
            _ => {}
        }
    }

    fn on_lsp_client(&mut self, cid: ClientId, sid: SlotId, msg: Value) {
        let protocol = self.slots[&sid].spec.protocol;
        match jsonrpc::classify(&msg) {
            Some(Kind::Request { id, method }) => {
                let id = id.clone();
                let method = method.to_string();
                match method.as_str() {
                    "initialize" => self.lsp_initialize(
                        cid,
                        sid,
                        id,
                        msg.get("params").cloned().unwrap_or(Value::Null),
                    ),
                    "shutdown" => self.send_client(cid, &jsonrpc::response(id, Value::Null)),
                    "textDocument/codeAction" => {
                        let params = msg.get("params").cloned().unwrap_or(Value::Null);
                        let lint = self.lint_code_actions(cid, &params);
                        match protocol {
                            Protocol::Lsp => {
                                let linted = params["textDocument"]["uri"]
                                    .as_str()
                                    .map(uri::key_for_uri)
                                    .and_then(|k| {
                                        let l = self.linted_for(cid, &k)?.clone();
                                        Some((k, l))
                                    });
                                let bid = self.slots.get_mut(&sid).expect("slot").seq();
                                let mut m = msg;
                                m["id"] = json!(bid);
                                self.add_pending(
                                    sid,
                                    bid,
                                    Pending::CodeAction {
                                        client: cid,
                                        id,
                                        lint,
                                        linted,
                                    },
                                );
                                self.send_backend(sid, m);
                            }
                            // tsserver code fixes are not translated yet (L1 gap): lint only.
                            Protocol::TsServer => {
                                self.send_client(cid, &jsonrpc::response(id, Value::Array(lint)))
                            }
                        }
                    }
                    _ => {
                        match protocol {
                            Protocol::Lsp => {
                                let bid = self.slots.get_mut(&sid).expect("slot").seq();
                                let mut m = msg;
                                m["id"] = json!(bid);
                                self.add_pending(sid, bid, Pending::Lsp { client: cid, id });
                                self.send_backend(sid, m);
                            }
                            Protocol::TsServer => {
                                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                                match translate::lsp_to_ts(&method, &params) {
                                None => self.send_client(
                                    cid,
                                    &jsonrpc::error_response(
                                        id,
                                        codes::METHOD_NOT_FOUND,
                                        format!("{method} is not supported on the tsserver backend yet"),
                                    ),
                                ),
                                Some(Err(e)) => {
                                    self.send_client(cid, &jsonrpc::error_response(id, codes::INVALID_PARAMS, e))
                                }
                                Some(Ok(t)) => {
                                    let seq = self.ts_request(sid, t.command, t.args, None);
                                    self.add_pending(sid, seq, Pending::Translated { client: cid, id, kind: t.kind });
                                }
                            }
                            }
                        }
                    }
                }
            }
            Some(Kind::Notification { method }) => {
                let method = method.to_string();
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                match method.as_str() {
                    "initialized" => {}
                    "exit" => self.remove_client(cid, "exit"),
                    "textDocument/didOpen" => {
                        let Some(key) = self.lsp_did_open(cid, &params) else {
                            return;
                        };
                        if protocol == Protocol::TsServer {
                            // Another window may already have this file open: no sync op is
                            // sent, so reuse its diagnostics and refresh them.
                            self.publish_cached(cid, sid, &key);
                            self.schedule_geterr(sid);
                        }
                    }
                    "textDocument/didChange" => self.lsp_did_change(cid, &params),
                    "textDocument/didClose" => {
                        let Some(u) = params["textDocument"]["uri"].as_str() else {
                            return;
                        };
                        let u = u.to_string();
                        let had_lint = self.lint_on(cid);
                        self.doc_event(cid, uri::key_for_uri(&u), DocChange::Close);
                        if protocol == Protocol::TsServer || had_lint {
                            self.send_client(
                                cid,
                                &jsonrpc::notification(
                                    "textDocument/publishDiagnostics",
                                    json!({"uri": u, "diagnostics": []}),
                                ),
                            );
                        }
                    }
                    "textDocument/didSave" => {
                        let Some(u) = params["textDocument"]["uri"].as_str() else {
                            return;
                        };
                        let key = uri::key_for_uri(u);
                        self.lint_saved(cid, &key);
                        if protocol == Protocol::Lsp
                            && let Some(doc) = self.slots[&sid].mirror.docs.get(&key)
                        {
                            let m = jsonrpc::notification(
                                "textDocument/didSave",
                                json!({"textDocument": {"uri": doc.uri}}),
                            );
                            self.send_backend(sid, m);
                        }
                    }
                    "$/cancelRequest" => self.lsp_cancel(cid, sid, &params["id"]),
                    "workspace/didChangeConfiguration" => {
                        if let Some(s) = params.get("settings") {
                            for w in self.settings.apply_json(s) {
                                log(&w);
                            }
                            self.lint_settings_changed(cid, s);
                        }
                        if protocol == Protocol::Lsp {
                            self.slots.get_mut(&sid).expect("slot").last_config =
                                Some(params.clone());
                            self.send_backend(sid, msg);
                        }
                    }
                    "workspace/didChangeWatchedFiles" => {
                        self.lint_watched_files(&params);
                        if protocol == Protocol::Lsp {
                            self.send_backend(sid, msg);
                        }
                    }
                    _ => {
                        if protocol == Protocol::Lsp {
                            self.send_backend(sid, msg);
                        }
                    }
                }
            }
            Some(Kind::Response { id }) => {
                let Some(key) = id.as_str() else { return };
                if key.starts_with("tsr-b-") {
                    return;
                }
                if let Some(r) = self.server_requests.remove(key) {
                    let mut m = msg.clone();
                    m["id"] = r.backend_id;
                    self.send_backend(r.slot, m);
                }
            }
            None => {}
        }
    }

    /// `didOpen` from an LSP client: records the document; returns its key.
    fn lsp_did_open(&mut self, cid: ClientId, params: &Value) -> Option<String> {
        let td = &params["textDocument"];
        let (Some(u), Some(text)) = (td["uri"].as_str(), td["text"].as_str()) else {
            return None;
        };
        let mut doc = ClientDoc::new(text, td["languageId"].as_str().unwrap_or("typescript"));
        doc.uri = Some(u.to_string());
        doc.script_kind = translate::script_kind(&doc.language_id).map(str::to_string);
        let key = uri::key_for_uri(u);
        self.doc_event(cid, key.clone(), DocChange::Open(doc));
        Some(key)
    }

    /// `didChange` from an LSP client.
    fn lsp_did_change(&mut self, cid: ClientId, params: &Value) {
        let Some(u) = params["textDocument"]["uri"].as_str() else {
            return;
        };
        let edits: Edits = params["contentChanges"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| {
                let text = c.get("text")?.as_str()?.to_string();
                match c.get("range") {
                    None | Some(Value::Null) => Some((None, text)),
                    Some(r) => crate::text::lsp_range(r).map(|r| (Some(r), text)),
                }
            })
            .collect();
        self.doc_event(cid, uri::key_for_uri(u), DocChange::Edits(edits));
    }

    /// Messages from a lint-only LSP client (no type backend).
    fn on_lint_only_client(&mut self, cid: ClientId, msg: Value) {
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match jsonrpc::classify(&msg) {
            Some(Kind::Request { id, method }) => {
                let id = id.clone();
                let reply = match method {
                    "initialize" => {
                        if let Some(c) = self.clients.get_mut(&cid) {
                            c.init_params = Some(params.clone());
                        }
                        jsonrpc::response(
                            id,
                            json!({
                                "capabilities": lint::lint_only_capabilities(),
                                "serverInfo": { "name": "ts-r-lsp", "version": env!("CARGO_PKG_VERSION") },
                            }),
                        )
                    }
                    "shutdown" => jsonrpc::response(id, Value::Null),
                    "ts-r/status" => {
                        jsonrpc::response(id, self.tsr_status.clone().unwrap_or(Value::Null))
                    }
                    "textDocument/codeAction" => {
                        jsonrpc::response(id, Value::Array(self.lint_code_actions(cid, &params)))
                    }
                    m => jsonrpc::error_response(
                        id,
                        codes::METHOD_NOT_FOUND,
                        format!("{m} is not served on a lint-only connection"),
                    ),
                };
                self.send_client(cid, &reply);
                if method == "initialize"
                    && let Some(st) = &self.tsr_status
                {
                    self.send_client(cid, &jsonrpc::notification("ts-r/status", st.clone()));
                }
            }
            Some(Kind::Notification { method }) => match method {
                "exit" => self.remove_client(cid, "exit"),
                "textDocument/didOpen" => {
                    self.lsp_did_open(cid, &params);
                }
                "textDocument/didChange" => self.lsp_did_change(cid, &params),
                "textDocument/didClose" => {
                    let Some(u) = params["textDocument"]["uri"].as_str() else {
                        return;
                    };
                    let u = u.to_string();
                    self.doc_event(cid, uri::key_for_uri(&u), DocChange::Close);
                    self.send_client(
                        cid,
                        &jsonrpc::notification(
                            "textDocument/publishDiagnostics",
                            json!({"uri": u, "diagnostics": []}),
                        ),
                    );
                }
                "textDocument/didSave" => {
                    if let Some(u) = params["textDocument"]["uri"].as_str() {
                        let key = uri::key_for_uri(u);
                        self.lint_saved(cid, &key);
                    }
                }
                "workspace/didChangeConfiguration" => {
                    if let Some(s) = params.get("settings") {
                        for w in self.settings.apply_json(s) {
                            log(&w);
                        }
                        self.lint_settings_changed(cid, s);
                    }
                }
                "workspace/didChangeWatchedFiles" => self.lint_watched_files(&params),
                _ => {}
            },
            _ => {}
        }
    }

    fn lsp_initialize(&mut self, cid: ClientId, sid: SlotId, id: Value, params: Value) {
        if let Some(c) = self.clients.get_mut(&cid) {
            c.init_params = Some(params.clone());
        }
        let slot = self.slots.get_mut(&sid).expect("slot");
        match slot.spec.protocol {
            Protocol::TsServer => {
                let mut caps = translate::server_capabilities();
                caps["codeActionProvider"] = lint::code_action_provider();
                let result = json!({
                    "capabilities": caps,
                    "serverInfo": { "name": "ts-r-lsp", "version": env!("CARGO_PKG_VERSION") },
                });
                self.send_client(cid, &jsonrpc::response(id, result));
                self.send_notes(cid);
            }
            Protocol::Lsp => {
                if slot.init_params.is_none() {
                    slot.init_params = Some(params);
                }
                slot.init_waiters.push((cid, id));
                if slot.backend.is_none() {
                    self.ensure_started(sid);
                } else if slot.state == SlotState::Ready {
                    self.answer_init_waiters(sid);
                }
            }
        }
    }

    fn send_notes(&self, cid: ClientId) {
        let label = self
            .clients
            .get(&cid)
            .and_then(|c| c.slot)
            .and_then(|s| self.slots.get(&s))
            .map(|s| s.spec.label.clone())
            .unwrap_or_default();
        let mut lines = vec![format!(
            "ts-r-lsp {}: type backend {label}",
            env!("CARGO_PKG_VERSION")
        )];
        lines.extend(self.settings.l1_notes().into_iter().map(str::to_string));
        for l in lines {
            self.send_client(
                cid,
                &jsonrpc::notification("window/logMessage", json!({"type": 3, "message": l})),
            );
        }
        if let Some(st) = &self.tsr_status {
            self.send_client(cid, &jsonrpc::notification("ts-r/status", st.clone()));
        }
    }

    fn answer_init_waiters(&mut self, sid: SlotId) {
        let slot = self.slots.get_mut(&sid).expect("slot");
        let Some(result) = slot.init_result.clone() else {
            return;
        };
        let waiters = std::mem::take(&mut slot.init_waiters);
        let registrations: Vec<Value> = slot.registrations.values().cloned().collect();
        let mut result = result;
        add_lint_capabilities(&mut result["capabilities"]);
        if let Some(obj) = result.as_object_mut() {
            let backend_info = obj.get("serverInfo").cloned();
            obj.insert(
                "serverInfo".into(),
                json!({ "name": "ts-r-lsp", "version": env!("CARGO_PKG_VERSION"), "backend": backend_info }),
            );
        }
        for (cid, id) in waiters {
            self.send_client(cid, &jsonrpc::response(id, result.clone()));
            self.send_notes(cid);
            if !registrations.is_empty() {
                let n = self.next_server_req;
                self.next_server_req += 1;
                let req = jsonrpc::request(
                    json!(format!("tsr-b-{n}")),
                    "client/registerCapability",
                    json!({ "registrations": registrations }),
                );
                self.send_client(cid, &req);
            }
        }
    }

    fn lsp_cancel(&mut self, cid: ClientId, sid: SlotId, id: &Value) {
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        let found = slot.pending.iter().find_map(|(bid, (p, _))| match p {
            Pending::Lsp { client, id: i }
            | Pending::Translated { client, id: i, .. }
            | Pending::CodeAction { client, id: i, .. }
                if *client == cid && i == id =>
            {
                Some(*bid)
            }
            _ => None,
        });
        let Some(bid) = found else { return };
        match slot.spec.protocol {
            Protocol::Lsp => {
                let m = jsonrpc::notification("$/cancelRequest", json!({"id": bid}));
                self.send_backend(sid, m);
            }
            Protocol::TsServer => {
                // Answer now; tsserver may still finish (its answer finds no pending entry and
                // is dropped). No per-id marker: the client may reuse the id.
                slot.pending.remove(&bid);
                self.cancel_ts(sid, bid);
                self.send_client(
                    cid,
                    &jsonrpc::error_response(id.clone(), codes::REQUEST_CANCELLED, "cancelled"),
                );
            }
        }
    }

    fn on_ts_client(&mut self, cid: ClientId, sid: SlotId, msg: Value) {
        if msg.get("type").and_then(Value::as_str) != Some("request") {
            return;
        }
        let command = msg
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let args = msg.get("arguments").cloned().unwrap_or(Value::Null);
        let seq = msg.get("seq").cloned().unwrap_or(json!(0));
        let local_ok = |d: &Daemon| {
            d.send_client(
                cid,
                &json!({"seq": 0, "type": "response", "command": command, "request_seq": seq, "success": true, "body": true}),
            )
        };
        match command.as_str() {
            "updateOpen" => {
                for f in args["openFiles"].as_array().into_iter().flatten() {
                    let Some(file) = f["file"].as_str() else {
                        continue;
                    };
                    let text = match f["fileContent"].as_str() {
                        Some(t) => t.to_string(),
                        None => disk(file).unwrap_or_default(),
                    };
                    let doc = ts_client_doc(file, text, f);
                    self.doc_event(cid, file.to_string(), DocChange::Open(doc));
                }
                for f in args["changedFiles"].as_array().into_iter().flatten() {
                    let Some(file) = f["fileName"].as_str() else {
                        continue;
                    };
                    // tsserver applies a file's textChanges last to first.
                    let edits: Edits = f["textChanges"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .rev()
                        .filter_map(ts_edit)
                        .collect();
                    self.doc_event(cid, file.to_string(), DocChange::Edits(edits));
                }
                for f in args["closedFiles"].as_array().into_iter().flatten() {
                    if let Some(file) = f.as_str() {
                        self.doc_event(cid, file.to_string(), DocChange::Close);
                    }
                }
                local_ok(self);
            }
            "open" => {
                if let Some(file) = args["file"].as_str() {
                    let text = match args["fileContent"].as_str() {
                        Some(t) => t.to_string(),
                        None => disk(file).unwrap_or_default(),
                    };
                    let doc = ts_client_doc(file, text, &args);
                    self.doc_event(cid, file.to_string(), DocChange::Open(doc));
                }
            }
            "close" => {
                if let Some(file) = args["file"].as_str() {
                    self.doc_event(cid, file.to_string(), DocChange::Close);
                }
            }
            "change" => {
                if let Some(file) = args["file"].as_str() {
                    let edit = json!({
                        "start": {"line": args["line"], "offset": args["offset"]},
                        "end": {"line": args["endLine"], "offset": args["endOffset"]},
                        "newText": args["insertString"].as_str().unwrap_or(""),
                    });
                    let edits: Edits = ts_edit(&edit).into_iter().collect();
                    self.doc_event(cid, file.to_string(), DocChange::Edits(edits));
                }
            }
            "exit" => self.remove_client(cid, "exit"),
            _ => {
                if self.slots[&sid].spec.protocol != Protocol::TsServer {
                    self.send_client(
                        cid,
                        &json!({"seq": 0, "type": "response", "command": command, "request_seq": seq,
                                "success": false, "message": "ts-r-lsp: this workspace's backend does not speak tsserver"}),
                    );
                    return;
                }
                match command.as_str() {
                    "configure" => {
                        self.slots.get_mut(&sid).expect("slot").ts_configure = Some(args.clone())
                    }
                    "compilerOptionsForInferredProjects" => {
                        self.slots.get_mut(&sid).expect("slot").ts_inferred = Some(args.clone())
                    }
                    _ => {}
                }
                let geterr = command == "geterr" || command == "geterrForProject";
                let bseq = self.slots.get_mut(&sid).expect("slot").seq();
                let mut m = msg;
                m["seq"] = json!(bseq);
                self.add_pending(
                    sid,
                    bseq,
                    Pending::Ts {
                        client: cid,
                        seq,
                        geterr,
                    },
                );
                self.send_backend(sid, m);
            }
        }
    }

    // ------------------------------------------------------------------------------------
    // Documents
    // ------------------------------------------------------------------------------------

    fn doc_event(&mut self, cid: ClientId, key: String, change: DocChange) {
        let lint_delay = match &change {
            DocChange::Open(_) => Some(Duration::ZERO),
            DocChange::Edits(_) => Some(self.settings.lint_debounce),
            DocChange::Close => None,
        };
        let Some(client) = self.clients.get_mut(&cid) else {
            return;
        };
        let mut applied: Option<Edits> = None;
        match change {
            DocChange::Open(doc) => {
                client.docs.insert(key.clone(), doc);
            }
            DocChange::Edits(edits) => {
                let Some(d) = client.docs.get_mut(&key) else {
                    return;
                };
                for (r, t) in &edits {
                    d.text.apply_change(*r, t);
                }
                applied = Some(edits);
            }
            DocChange::Close => {
                if client.docs.remove(&key).is_none() {
                    return;
                }
                client.type_diags.remove(&key);
            }
        }
        match lint_delay {
            Some(d) => self.schedule_lint(cid, &key, d),
            None => self.lint_forget_if_unused(&key),
        }
        let Some(client) = self.clients.get(&cid) else {
            return;
        };
        let Some(sid) = client.slot else { return };
        let Daemon { slots, clients, .. } = self;
        let Some(slot) = slots.get_mut(&sid) else {
            return;
        };
        if slot.state != SlotState::Ready {
            return;
        }
        let view = View {
            clients,
            members: &slot.members,
        };
        if let Some(edits) = applied {
            let new_text = &clients[&cid].docs[&key].text;
            if let Some(op) = slot.mirror.forward_edits(cid, &key, &edits, new_text) {
                let shared = view.docs_for(&key).len() > 1;
                self.apply_ops(sid, vec![op]);
                if !shared {
                    return;
                }
            }
        }
        let Daemon { slots, clients, .. } = self;
        let slot = slots.get_mut(&sid).expect("slot");
        let view = View {
            clients,
            members: &slot.members,
        };
        match slot.mirror.decide(&key, &view, Some(cid), &disk) {
            Ok(ops) => self.apply_ops(sid, ops),
            Err(d) => self.split(sid, d),
        }
    }

    fn reconcile_slot(&mut self, sid: SlotId) {
        let Daemon { slots, clients, .. } = self;
        let Some(slot) = slots.get_mut(&sid) else {
            return;
        };
        let view = View {
            clients,
            members: &slot.members,
        };
        match slot.mirror.reconcile(&view, &disk) {
            Ok(ops) => self.apply_ops(sid, ops),
            Err(d) => self.split(sid, d),
        }
    }

    fn apply_ops(&mut self, sid: SlotId, ops: Vec<SyncOp>) {
        if ops.is_empty() {
            return;
        }
        let protocol = self.slots[&sid].spec.protocol;
        match protocol {
            Protocol::TsServer => {
                // Cached diagnostics live as long as some client has the file open.
                let slot = self.slots.get_mut(&sid).expect("slot");
                for op in &ops {
                    if let SyncOp::Close { key, .. } = op {
                        slot.diags.remove(key);
                    }
                }
                let args = update_open_args(&ops, &self.root);
                let seq = self.ts_request(sid, "updateOpen", args, None);
                self.add_pending(sid, seq, Pending::Ignore);
                // Diagnostics of files that changed are stale for LSP clients.
                self.schedule_geterr(sid);
            }
            Protocol::Lsp => {
                for op in ops {
                    self.send_backend(sid, lsp_sync_message(&op));
                }
            }
        }
    }

    fn split(&mut self, sid: SlotId, d: Diverged) {
        let members: Vec<ClientId> = self.slots[&sid].members.iter().copied().collect();
        log(&format!(
            "K-12: clients {:?} have different unsaved edits in {}; one backend per client from now on",
            d.clients, d.key
        ));
        if members.len() < 2 {
            return;
        }
        self.per_client = true;
        self.stats.splits += 1;
        let keep = members[0];
        for &c in &members[1..] {
            let (key, spec, cancel_prefix, init_params, configure, inferred) = {
                let s = &self.slots[&sid];
                let init = self
                    .clients
                    .get(&c)
                    .and_then(|cl| cl.init_params.clone())
                    .or_else(|| s.init_params.clone());
                (
                    s.key.clone(),
                    s.spec.clone(),
                    s.cancel_prefix.is_some(),
                    init,
                    s.ts_configure.clone(),
                    s.ts_inferred.clone(),
                )
            };
            let new = self.next_slot;
            self.next_slot += 1;
            let mut spec = spec;
            let prefix = cancel_prefix.then(|| {
                let p = self
                    .cancel_dir
                    .join(format!("s{new}-"))
                    .to_string_lossy()
                    .into_owned();
                // Point the copied command line at this slot's cancellation files.
                for i in 0..spec.args.len() {
                    if spec.args[i] == "--cancellationPipeName" && i + 1 < spec.args.len() {
                        spec.args[i + 1] = format!("{p}*").into();
                    }
                }
                p
            });
            let mut slot = Slot::new(key, spec, prefix);
            slot.init_params = init_params;
            slot.ts_configure = configure;
            slot.ts_inferred = inferred;
            slot.members.insert(c);
            self.slots.insert(new, slot);
            self.slots.get_mut(&sid).expect("slot").members.remove(&c);
            if let Some(cl) = self.clients.get_mut(&c) {
                cl.slot = Some(new);
            }
            self.ensure_started(new);
        }
        let _ = keep;
        self.reconcile_slot(sid);
        let note = format!(
            "ts-r-lsp: two windows have different unsaved edits in {}; using one TypeScript server per window for this workspace",
            d.key
        );
        for &c in &members {
            if self.clients.get(&c).and_then(|cl| cl.kind) == Some(FrontKind::Lsp) {
                self.send_client(
                    c,
                    &jsonrpc::notification(
                        "window/logMessage",
                        json!({"type": 2, "message": note}),
                    ),
                );
            }
        }
    }

    // ------------------------------------------------------------------------------------
    // Backends
    // ------------------------------------------------------------------------------------

    fn add_pending(&mut self, sid: SlotId, seq: i64, p: Pending) {
        if let Some(s) = self.slots.get_mut(&sid) {
            s.pending.insert(seq, (p, Instant::now()));
        }
    }

    /// Sends a tsserver request built by the daemon; returns its seq.
    fn ts_request(&mut self, sid: SlotId, command: &str, args: Value, seq: Option<i64>) -> i64 {
        let slot = self.slots.get_mut(&sid).expect("slot");
        let seq = seq.unwrap_or_else(|| slot.seq());
        let m = json!({"seq": seq, "type": "request", "command": command, "arguments": args});
        self.send_backend(sid, m);
        seq
    }

    fn send_backend(&mut self, sid: SlotId, msg: Value) {
        if self.slots.get(&sid).is_some_and(|s| s.backend.is_none()) {
            self.ensure_started(sid);
        }
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        if slot.state == SlotState::Starting {
            slot.queue.push(msg);
            return;
        }
        if let Some(b) = slot.backend.as_mut()
            && let Err(e) = b.send(&msg)
        {
            log(&format!("slot {sid}: send failed: {e}"));
        }
    }

    fn ensure_started(&mut self, sid: SlotId) {
        let tx = self.tx.clone();
        let root = self.root.clone();
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        if slot.backend.is_some() {
            return;
        }
        // A native LSP backend waits for the editor's `initialize` (`lsp_initialize` starts
        // it), so it negotiates with the editor's capabilities, folders and options.
        if slot.spec.protocol == Protocol::Lsp && slot.init_params.is_none() {
            return;
        }
        slot.generation += 1;
        let generation = slot.generation;
        let sink = Box::new(move |ev| {
            let _ = tx.send(Event::Backend {
                slot: sid,
                generation,
                ev,
            });
        });
        let started = Instant::now();
        let factory = if crate::tsr::is_tsr(&slot.spec) {
            &self.tsr_factory
        } else {
            &self.factory
        };
        match factory.start(&slot.spec, sink) {
            Ok(b) => {
                log(&format!(
                    "slot {sid}: started {} (pid {:?})",
                    slot.spec.label,
                    b.pid()
                ));
                slot.backend = Some(b);
                slot.mirror.clear();
                slot.started_at = Some(started);
                slot.rss = None;
            }
            Err(e) => {
                let msg = format!("ts-r-lsp: cannot start {}: {e}", slot.spec.label);
                log(&msg);
                slot.state = SlotState::Stopped;
                let members: Vec<ClientId> = slot.members.iter().copied().collect();
                for c in members {
                    self.show_error(c, &msg);
                }
                return;
            }
        }
        match slot.spec.protocol {
            Protocol::TsServer => {
                slot.state = SlotState::Ready;
                if slot.first_ready.is_none() {
                    slot.first_ready = Some(started.duration_since(self.started));
                }
                let configure = slot.ts_configure.clone().unwrap_or_else(default_configure);
                let inferred = slot.ts_inferred.clone();
                let seq = self.ts_request(sid, "configure", configure, None);
                self.add_pending(sid, seq, Pending::Ignore);
                if let Some(inf) = inferred {
                    let seq = self.ts_request(sid, "compilerOptionsForInferredProjects", inf, None);
                    self.add_pending(sid, seq, Pending::Ignore);
                }
                self.reconcile_slot(sid);
                // tsserver clients refresh their diagnostics on this event.
                let members: Vec<ClientId> = self.slots[&sid].members.iter().copied().collect();
                for c in members {
                    let Some(cl) = self.clients.get(&c) else {
                        continue;
                    };
                    if cl.kind == Some(FrontKind::TsServer) && !cl.docs.is_empty() {
                        let files: Vec<&String> = cl.docs.keys().collect();
                        let ev = json!({"seq": 0, "type": "event", "event": "projectsUpdatedInBackground",
                                        "body": {"openFiles": files}});
                        self.send_client(c, &ev);
                    }
                }
            }
            Protocol::Lsp => {
                slot.state = SlotState::Starting;
                let mut params = slot
                    .init_params
                    .clone()
                    .unwrap_or_else(|| default_init_params(&root));
                params["processId"] = json!(std::process::id());
                let id = slot.seq();
                slot.pending.insert(id, (Pending::Init, Instant::now()));
                let m = jsonrpc::request(json!(id), "initialize", params);
                if let Some(b) = slot.backend.as_mut() {
                    let _ = b.send(&m);
                }
            }
        }
    }

    fn show_error(&self, cid: ClientId, message: &str) {
        if self.clients.get(&cid).and_then(|c| c.kind) == Some(FrontKind::Lsp) {
            self.send_client(
                cid,
                &jsonrpc::notification(
                    "window/showMessage",
                    json!({"type": 1, "message": message}),
                ),
            );
        }
    }

    fn stop_slot(&mut self, sid: SlotId, why: &str) {
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        let Some(mut b) = slot.backend.take() else {
            return;
        };
        log(&format!("slot {sid}: stopping {} ({why})", slot.spec.label));
        match slot.spec.protocol {
            Protocol::TsServer => {
                let _ = b.send(&json!({"seq": slot.seq(), "type": "request", "command": "exit"}));
            }
            Protocol::Lsp => {
                if slot.state == SlotState::Ready {
                    let _ = b.send(&jsonrpc::request(
                        json!(slot.seq()),
                        "shutdown",
                        Value::Null,
                    ));
                    let _ = b.send(&jsonrpc::notification("exit", Value::Null));
                }
            }
        }
        b.shutdown(Duration::from_secs(3));
        slot.state = SlotState::Stopped;
        slot.mirror.clear();
        slot.diags.clear();
        slot.init_result = None;
        slot.geterr_due = None;
        slot.geterr_inflight = None;
        slot.geterr_again = false;
        slot.rss = None;
        self.fail_pending(
            sid,
            &format!("ts-r-lsp restarted the TypeScript server ({why})"),
        );
        self.server_requests.retain(|_, r| r.slot != sid);
    }

    fn restart_slot(&mut self, sid: SlotId, why: &str) {
        self.stop_slot(sid, why);
        self.ensure_started(sid);
    }

    fn fail_pending(&mut self, sid: SlotId, why: &str) {
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        let pending: Vec<(i64, Pending)> = slot.pending.drain().map(|(k, (p, _))| (k, p)).collect();
        slot.queue.clear();
        let files: Vec<i64> = std::mem::take(&mut slot.cancel_files).into_iter().collect();
        if let Some(prefix) = slot.cancel_prefix.clone() {
            for seq in files {
                let _ = std::fs::remove_file(format!("{prefix}{seq}"));
            }
        }
        for (_, p) in pending {
            match p {
                Pending::Lsp { client, id }
                | Pending::Translated { client, id, .. }
                | Pending::CodeAction { client, id, .. } => {
                    self.send_client(
                        client,
                        &jsonrpc::error_response(id, codes::REQUEST_FAILED, why),
                    );
                }
                Pending::Ts {
                    client,
                    seq,
                    geterr: true,
                } => {
                    let ev = json!({"seq": 0, "type": "event", "event": "requestCompleted", "body": {"request_seq": seq}});
                    self.send_client(client, &ev);
                }
                Pending::Ts {
                    client,
                    seq,
                    geterr: false,
                } => {
                    let r = json!({"seq": 0, "type": "response", "request_seq": seq, "success": false, "message": why});
                    self.send_client(client, &r);
                }
                Pending::Init | Pending::Geterr | Pending::Ignore => {}
            }
        }
    }

    fn cancel_ts(&mut self, sid: SlotId, seq: i64) {
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        if let Some(prefix) = &slot.cancel_prefix
            && std::fs::write(format!("{prefix}{seq}"), b"").is_ok()
        {
            slot.cancel_files.insert(seq);
        }
    }

    fn completed(&mut self, sid: SlotId, seq: i64) {
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        if slot.cancel_files.remove(&seq)
            && let Some(prefix) = &slot.cancel_prefix
        {
            let _ = std::fs::remove_file(format!("{prefix}{seq}"));
        }
    }

    fn on_backend(&mut self, sid: SlotId, generation: u64, ev: BackendEvent) {
        let Some(slot) = self.slots.get(&sid) else {
            return;
        };
        if slot.generation != generation || slot.backend.is_none() {
            return;
        }
        match ev {
            BackendEvent::Exited => self.on_crash(sid),
            BackendEvent::Message(m) => match slot.spec.protocol {
                Protocol::TsServer => self.on_ts_backend(sid, m),
                Protocol::Lsp => self.on_lsp_backend(sid, m),
            },
        }
    }

    fn on_crash(&mut self, sid: SlotId) {
        let slot = self.slots.get_mut(&sid).expect("slot");
        log(&format!(
            "slot {sid}: {} exited unexpectedly",
            slot.spec.label
        ));
        if let Some(mut b) = slot.backend.take() {
            b.shutdown(Duration::ZERO);
        }
        slot.state = SlotState::Stopped;
        slot.mirror.clear();
        slot.diags.clear();
        slot.init_result = None;
        slot.geterr_inflight = None;
        slot.restarts_crash += 1;
        let now = Instant::now();
        slot.crashes
            .retain(|t| now.duration_since(*t) < Duration::from_secs(60));
        slot.crashes.push(now);
        let again = slot.crashes.len() <= 3 && !slot.members.is_empty();
        let label = slot.spec.label.clone();
        let members: Vec<ClientId> = slot.members.iter().copied().collect();
        self.fail_pending(sid, "the TypeScript server crashed");
        if again {
            self.ensure_started(sid);
        } else {
            for c in members {
                self.show_error(
                    c,
                    &format!(
                        "ts-r-lsp: {label} keeps crashing; it will be retried on your next edit"
                    ),
                );
            }
            if let Some(s) = self.slots.get_mut(&sid) {
                s.crashes.clear();
            }
        }
    }

    fn on_lsp_backend(&mut self, sid: SlotId, msg: Value) {
        match jsonrpc::classify(&msg) {
            Some(Kind::Response { id }) => {
                let Some(bid) = id.as_i64() else { return };
                let Some((p, _)) = self
                    .slots
                    .get_mut(&sid)
                    .and_then(|s| s.pending.remove(&bid))
                else {
                    return;
                };
                match p {
                    Pending::Lsp { client, id } => {
                        if self.take_cancelled(client, &id) {
                            return;
                        }
                        let mut m = msg;
                        m["id"] = id;
                        self.send_client(client, &m);
                    }
                    Pending::Init => self.on_lsp_init(sid, msg),
                    Pending::CodeAction {
                        client,
                        id,
                        lint,
                        linted,
                    } => {
                        if self.take_cancelled(client, &id) {
                            return;
                        }
                        // The client may have edited while the backend answered: lint's edits
                        // are unversioned ranges into the text they were made for.
                        let current = linted.is_some_and(|(key, l)| {
                            self.clients
                                .get(&client)
                                .and_then(|c| c.docs.get(&key))
                                .is_some_and(|d| d.text.text() == l.text.text())
                        });
                        let mut m = msg;
                        m["id"] = id;
                        if m.get("error").is_none() {
                            let mut all = m["result"].as_array().cloned().unwrap_or_default();
                            if !self.lint_on(client) {
                                // The ts-r backend's type-aware lint fixes follow the switch.
                                all.retain(|a| a["diagnostics"][0]["source"] != lint::SOURCE);
                            }
                            if current {
                                all.extend(lint);
                            }
                            m["result"] = Value::Array(all);
                        }
                        self.send_client(client, &m);
                    }
                    _ => {}
                }
            }
            Some(Kind::Request { id, method }) => {
                let id = id.clone();
                let method = method.to_string();
                self.on_lsp_server_request(sid, id, &method, msg);
            }
            Some(Kind::Notification { method }) => match method {
                "textDocument/publishDiagnostics" => self.route_lsp_diagnostics(sid, msg),
                "ts-r/status" => self.on_tsr_status(sid, msg),
                "window/logMessage" | "$/logTrace" | "telemetry/event" => {
                    if msg["params"]["type"].as_u64() == Some(1) {
                        log(&format!("slot {sid}: {}", msg["params"]["message"]));
                    }
                }
                _ => self.broadcast_lsp(sid, &msg),
            },
            None => {}
        }
    }

    fn take_cancelled(&mut self, client: ClientId, id: &Value) -> bool {
        match (self.clients.get_mut(&client), IdKey::from_value(id)) {
            (Some(c), Some(k)) => c.cancelled.remove(&k),
            _ => false,
        }
    }

    fn on_lsp_init(&mut self, sid: SlotId, msg: Value) {
        let slot = self.slots.get_mut(&sid).expect("slot");
        if let Some(err) = msg.get("error") {
            let text = format!(
                "ts-r-lsp: {} failed to initialize: {}",
                slot.spec.label, err["message"]
            );
            log(&text);
            let waiters = std::mem::take(&mut slot.init_waiters);
            self.stop_slot(sid, "initialize failed");
            for (c, id) in waiters {
                self.send_client(
                    c,
                    &jsonrpc::error_response(id, codes::INTERNAL_ERROR, text.clone()),
                );
            }
            return;
        }
        slot.init_result = Some(msg.get("result").cloned().unwrap_or(json!({})));
        slot.state = SlotState::Ready;
        if slot.first_ready.is_none()
            && let Some(t) = slot.started_at
        {
            slot.first_ready = Some(t.duration_since(self.started));
        }
        let queue = std::mem::take(&mut slot.queue);
        let config = slot.last_config.clone();
        self.send_backend(sid, jsonrpc::notification("initialized", json!({})));
        if let Some(c) = config {
            self.send_backend(
                sid,
                jsonrpc::notification("workspace/didChangeConfiguration", c),
            );
        }
        self.reconcile_slot(sid);
        for m in queue {
            self.send_backend(sid, m);
        }
        self.answer_init_waiters(sid);
    }

    /// The ts-r backend's project status: kept for `status` and late joiners, and sent to
    /// every LSP client (lint-only connections too: that is how the VS Code extension's
    /// status bar learns it).
    fn on_tsr_status(&mut self, sid: SlotId, msg: Value) {
        if let Some(slot) = self.slots.get_mut(&sid) {
            slot.tsr_status = Some(msg["params"].clone());
        }
        self.tsr_status = Some(msg["params"].clone());
        let targets: Vec<ClientId> = self
            .clients
            .iter()
            .filter(|(_, c)| c.kind == Some(FrontKind::Lsp))
            .map(|(id, _)| *id)
            .collect();
        for c in targets {
            self.send_client(c, &msg);
        }
    }

    fn lsp_members(&self, sid: SlotId) -> Vec<ClientId> {
        self.slots
            .get(&sid)
            .map(|s| {
                s.members
                    .iter()
                    .copied()
                    .filter(|c| self.clients.get(c).and_then(|cl| cl.kind) == Some(FrontKind::Lsp))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn broadcast_lsp(&mut self, sid: SlotId, msg: &Value) {
        for c in self.lsp_members(sid) {
            self.send_client(c, msg);
        }
    }

    fn broadcast_server_request(&mut self, sid: SlotId, method: &str, params: &Value) {
        for c in self.lsp_members(sid) {
            let n = self.next_server_req;
            self.next_server_req += 1;
            self.send_client(
                c,
                &jsonrpc::request(json!(format!("tsr-b-{n}")), method, params.clone()),
            );
        }
    }

    fn on_lsp_server_request(&mut self, sid: SlotId, id: Value, method: &str, msg: Value) {
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let reply_null =
            |d: &mut Daemon| d.send_backend(sid, jsonrpc::response(id.clone(), Value::Null));
        match method {
            "client/registerCapability" => {
                if let Some(slot) = self.slots.get_mut(&sid) {
                    for r in params["registrations"].as_array().into_iter().flatten() {
                        if let Some(rid) = r["id"].as_str() {
                            slot.registrations.insert(rid.to_string(), r.clone());
                        }
                    }
                }
                self.broadcast_server_request(sid, method, &params);
                reply_null(self);
            }
            "client/unregisterCapability" => {
                if let Some(slot) = self.slots.get_mut(&sid) {
                    for r in params["unregisterations"].as_array().into_iter().flatten() {
                        if let Some(rid) = r["id"].as_str() {
                            slot.registrations.remove(rid);
                        }
                    }
                }
                self.broadcast_server_request(sid, method, &params);
                reply_null(self);
            }
            "window/workDoneProgress/create"
            | "workspace/diagnostic/refresh"
            | "workspace/semanticTokens/refresh"
            | "workspace/inlayHint/refresh"
            | "workspace/codeLens/refresh"
            | "workspace/inlineValue/refresh"
            | "workspace/foldingRange/refresh" => {
                self.broadcast_server_request(sid, method, &params);
                reply_null(self);
            }
            _ => {
                // Ask the most recently active client (workspace/configuration, applyEdit, ...).
                let target = self
                    .lsp_members(sid)
                    .into_iter()
                    .max_by_key(|c| self.clients.get(c).map(|cl| cl.last_activity));
                match target {
                    Some(c) => {
                        let n = self.next_server_req;
                        self.next_server_req += 1;
                        let key = format!("tsr-{n}");
                        self.server_requests.insert(
                            key.clone(),
                            ServerReq {
                                slot: sid,
                                backend_id: id,
                                client: c,
                            },
                        );
                        let mut m = msg;
                        m["id"] = json!(key);
                        self.send_client(c, &m);
                    }
                    None if method == "workspace/configuration" => {
                        let n = params["items"].as_array().map_or(0, Vec::len);
                        self.send_backend(
                            sid,
                            jsonrpc::response(id, Value::Array(vec![Value::Null; n])),
                        );
                    }
                    None => self.send_backend(
                        sid,
                        jsonrpc::error_response(id, codes::REQUEST_FAILED, "no editor connected"),
                    ),
                }
            }
        }
    }

    /// Clients that should see results for `key`: members with it open whose text is the
    /// text the backend has. If no member has it open, every member of `kind`.
    fn audience(&self, sid: SlotId, key: &str, kind: FrontKind) -> Vec<(ClientId, Option<String>)> {
        let Some(slot) = self.slots.get(&sid) else {
            return vec![];
        };
        let backend_text = slot.mirror.docs.get(key).map(|d| d.text.text());
        let mut open = Vec::new();
        let mut any_open = false;
        for c in &slot.members {
            let Some(cl) = self.clients.get(c) else {
                continue;
            };
            if cl.kind != Some(kind) {
                continue;
            }
            if let Some(doc) = cl.docs.get(key) {
                any_open = true;
                if backend_text.is_none_or(|t| t == doc.text.text()) {
                    open.push((*c, doc.uri.clone()));
                }
            }
        }
        if any_open {
            return open;
        }
        slot.members
            .iter()
            .filter(|c| self.clients.get(c).and_then(|cl| cl.kind) == Some(kind))
            .map(|c| (*c, None))
            .collect()
    }

    fn route_lsp_diagnostics(&mut self, sid: SlotId, msg: Value) {
        let Some(u) = msg["params"]["uri"].as_str() else {
            return;
        };
        let key = uri::key_for_uri(u);
        let diags = msg["params"]["diagnostics"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for (c, client_uri) in self.audience(sid, &key, FrontKind::Lsp) {
            let uri = client_uri.unwrap_or_else(|| u.to_string());
            self.publish_type_diags(c, &key, uri, diags.clone());
        }
    }

    fn on_ts_backend(&mut self, sid: SlotId, msg: Value) {
        match msg.get("type").and_then(Value::as_str) {
            Some("response") => {
                let Some(seq) = msg.get("request_seq").and_then(Value::as_i64) else {
                    return;
                };
                self.completed(sid, seq);
                let Some((p, _)) = self
                    .slots
                    .get_mut(&sid)
                    .and_then(|s| s.pending.remove(&seq))
                else {
                    return;
                };
                match p {
                    Pending::Ts { client, seq, .. } => {
                        let mut m = msg;
                        m["request_seq"] = seq;
                        self.send_client(client, &m);
                    }
                    Pending::Translated { client, id, kind } => {
                        if self.take_cancelled(client, &id) {
                            return;
                        }
                        let uri_for = self.uri_mapper(client);
                        let res = translate::ts_to_lsp(
                            &kind,
                            msg["success"].as_bool().unwrap_or(false),
                            msg.get("body"),
                            msg["message"].as_str(),
                            &uri_for,
                        );
                        let out = match res {
                            Ok(v) => jsonrpc::response(id, v),
                            Err((code, m)) => jsonrpc::error_response(id, code, m),
                        };
                        self.send_client(client, &out);
                    }
                    _ => {}
                }
            }
            Some("event") => {
                let event = msg
                    .get("event")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                match event.as_str() {
                    "requestCompleted" => {
                        let Some(seq) = msg["body"]["request_seq"].as_i64() else {
                            return;
                        };
                        self.completed(sid, seq);
                        let Some((p, _)) = self
                            .slots
                            .get_mut(&sid)
                            .and_then(|s| s.pending.remove(&seq))
                        else {
                            return;
                        };
                        match p {
                            Pending::Ts { client, seq, .. } => {
                                let mut m = msg;
                                m["body"]["request_seq"] = seq;
                                self.send_client(client, &m);
                            }
                            Pending::Geterr => {
                                let slot = self.slots.get_mut(&sid).expect("slot");
                                slot.geterr_inflight = None;
                                if std::mem::take(&mut slot.geterr_again) {
                                    slot.geterr_due = Some(Instant::now());
                                }
                            }
                            _ => {}
                        }
                    }
                    "syntaxDiag" | "semanticDiag" | "suggestionDiag" => {
                        self.on_ts_diag(sid, &event, msg)
                    }
                    "projectsUpdatedInBackground" => {
                        self.broadcast_ts(sid, &msg);
                        self.schedule_geterr(sid);
                    }
                    "telemetry" | "typingsInstallerPid" | "surveyReady" => {}
                    _ => self.broadcast_ts(sid, &msg),
                }
            }
            _ => {}
        }
    }

    fn broadcast_ts(&self, sid: SlotId, msg: &Value) {
        let Some(slot) = self.slots.get(&sid) else {
            return;
        };
        for c in &slot.members {
            if self.clients.get(c).and_then(|cl| cl.kind) == Some(FrontKind::TsServer) {
                self.send_client(*c, msg);
            }
        }
    }

    fn on_ts_diag(&mut self, sid: SlotId, event: &str, msg: Value) {
        let Some(file) = msg["body"]["file"].as_str().map(str::to_string) else {
            return;
        };
        for (c, _) in self.audience(sid, &file, FrontKind::TsServer) {
            self.send_client(c, &msg);
        }
        let idx = match event {
            "syntaxDiag" => 0,
            "semanticDiag" => 1,
            _ => 2,
        };
        let lsp_targets = self.audience(sid, &file, FrontKind::Lsp);
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        if !slot.mirror.docs.contains_key(&file) {
            return; // late, for a file no client has open any more: don't cache it
        }
        let entry = slot.diags.entry(file.clone()).or_default();
        entry[idx] = Some(msg["body"]["diagnostics"].clone());
        if idx == 0 || lsp_targets.is_empty() {
            // Publish once semantic results are in (syntax-only would flicker).
            return;
        }
        let all: Vec<Value> = entry
            .iter()
            .flatten()
            .filter_map(Value::as_array)
            .flatten()
            .cloned()
            .collect();
        for (c, client_uri) in lsp_targets {
            let Some(client_uri) = client_uri else {
                continue;
            };
            let uri_for = self.uri_mapper(c);
            let diagnostics = translate::diagnostics_to_lsp(&all, &uri_for);
            self.publish_type_diags(c, &file, client_uri, diagnostics);
        }
    }

    fn publish_cached(&mut self, cid: ClientId, sid: SlotId, key: &str) {
        let Some(slot) = self.slots.get(&sid) else {
            return;
        };
        let (Some(entry), Some(backend)) = (slot.diags.get(key), slot.mirror.docs.get(key)) else {
            return;
        };
        let Some(doc) = self.clients.get(&cid).and_then(|c| c.docs.get(key)) else {
            return;
        };
        if entry[1].is_none() || doc.text.text() != backend.text.text() {
            return;
        }
        let all: Vec<Value> = entry
            .iter()
            .flatten()
            .filter_map(Value::as_array)
            .flatten()
            .cloned()
            .collect();
        let uri_for = self.uri_mapper(cid);
        let client_uri = doc.uri.clone().unwrap_or_else(|| uri::path_to_uri(key));
        let diagnostics = translate::diagnostics_to_lsp(&all, &uri_for);
        self.publish_type_diags(cid, key, client_uri, diagnostics);
    }

    /// Maps tsserver paths to the URIs a given client uses.
    fn uri_mapper(&self, cid: ClientId) -> impl Fn(&str) -> String + 'static {
        let known: HashMap<String, String> = self
            .clients
            .get(&cid)
            .map(|c| {
                c.docs
                    .iter()
                    .filter_map(|(k, d)| d.uri.clone().map(|u| (k.clone(), u)))
                    .collect()
            })
            .unwrap_or_default();
        move |path: &str| {
            known
                .get(path)
                .cloned()
                .unwrap_or_else(|| uri::path_to_uri(path))
        }
    }

    fn schedule_geterr(&mut self, sid: SlotId) {
        let has_lsp_docs = self
            .lsp_members(sid)
            .iter()
            .any(|c| self.clients.get(c).is_some_and(|cl| !cl.docs.is_empty()));
        if let Some(slot) = self.slots.get_mut(&sid)
            && slot.spec.protocol == Protocol::TsServer
            && has_lsp_docs
            && slot.state == SlotState::Ready
        {
            slot.geterr_due = Some(Instant::now() + GETERR_DEBOUNCE);
        }
    }

    fn run_geterr(&mut self, sid: SlotId) {
        let files: Vec<String> = {
            let Some(slot) = self.slots.get(&sid) else {
                return;
            };
            let mut f: Vec<String> = self
                .lsp_members(sid)
                .iter()
                .filter_map(|c| self.clients.get(c))
                .flat_map(|c| c.docs.keys().cloned())
                .filter(|k| slot.mirror.docs.contains_key(k))
                .collect();
            f.sort();
            f.dedup();
            f
        };
        let slot = self.slots.get_mut(&sid).expect("slot");
        slot.geterr_due = None;
        if files.is_empty() {
            return;
        }
        if let Some(inflight) = slot.geterr_inflight {
            slot.geterr_again = true;
            self.cancel_ts(sid, inflight);
            return;
        }
        let seq = self.ts_request(sid, "geterr", json!({"files": files, "delay": 0}), None);
        self.add_pending(sid, seq, Pending::Geterr);
        self.slots.get_mut(&sid).expect("slot").geterr_inflight = Some(seq);
    }

    // ------------------------------------------------------------------------------------
    // Timers: diagnostics, memory, idle
    // ------------------------------------------------------------------------------------

    fn tick(&mut self) {
        self.run_due_lints();
        let now = Instant::now();
        let due: Vec<SlotId> = self
            .slots
            .iter()
            .filter(|(_, s)| s.geterr_due.is_some_and(|d| d <= now))
            .map(|(k, _)| *k)
            .collect();
        for sid in due {
            self.run_geterr(sid);
        }
        if now >= self.next_sample {
            self.next_sample = now + self.settings.sample_interval;
            // A process backend is its process tree; an in-process one (ts-r) is the daemon
            // itself plus the trees of the helpers it started.
            let targets: Vec<(SlotId, u64, Option<u32>, Vec<u32>)> = self
                .slots
                .iter()
                .filter_map(|(k, s)| {
                    let b = s.backend.as_ref()?;
                    let pid = if b.in_process() { None } else { Some(b.pid()?) };
                    Some((*k, s.generation, pid, b.helper_pids()))
                })
                .collect();
            let tx = self.tx.clone();
            let _ = thread::Builder::new()
                .name("rss-sampler".into())
                .spawn(move || {
                    let snap = process::Snapshot::take();
                    let own = snap.rss_bytes(std::process::id());
                    let samples = targets
                        .into_iter()
                        .map(|(s, g, pid, helpers)| {
                            let rss = match pid {
                                Some(pid) => snap.tree_rss_bytes(pid),
                                None => own.map(|o| {
                                    o + helpers
                                        .iter()
                                        .filter_map(|&h| snap.tree_rss_bytes(h))
                                        .sum::<u64>()
                                }),
                            };
                            (s, g, rss)
                        })
                        .collect();
                    let _ = tx.send(Event::Rss(samples, own));
                });
        }
        let idle = now.duration_since(self.last_activity);
        if !self.settings.idle_shutdown.is_zero() && idle >= self.settings.idle_shutdown {
            let running: Vec<SlotId> = self
                .slots
                .iter()
                .filter(|(_, s)| s.backend.is_some())
                .map(|(k, _)| *k)
                .collect();
            for sid in running {
                if let Some(s) = self.slots.get_mut(&sid) {
                    s.idle_stops += 1;
                }
                self.stop_slot(sid, "idle");
            }
        }
        if let Some(left) = self.last_client_left
            && !self.clients.values().any(is_active)
            && now.duration_since(left) >= self.settings.linger
        {
            log("no clients left");
            self.stopping = true;
        }
        for slot in self.slots.values_mut() {
            slot.pending.retain(|_, (p, t)| {
                matches!(p, Pending::Geterr | Pending::Init) || now.duration_since(*t) < PENDING_GC
            });
        }
    }

    fn on_rss(&mut self, samples: Vec<(SlotId, u64, Option<u64>)>, own: Option<u64>) {
        self.daemon_rss = own;
        let budget = self.settings.memory_budget_mb * 1024 * 1024;
        let now = Instant::now();
        let quiet = now.duration_since(self.last_activity) >= self.settings.restart_idle;
        for (sid, generation, rss) in samples {
            let Some(slot) = self.slots.get_mut(&sid) else {
                continue;
            };
            if slot.generation != generation || slot.backend.is_none() {
                continue;
            }
            slot.rss = rss;
            let busy = slot.pending.values().any(|(p, _)| {
                matches!(
                    p,
                    Pending::Lsp { .. }
                        | Pending::Translated { .. }
                        | Pending::CodeAction { .. }
                        | Pending::Ts { .. }
                        | Pending::Init
                )
            });
            if let Some(rss) = rss
                && rss > budget
                && quiet
                && !busy
                && slot.state == SlotState::Ready
                && slot.budget_marker != Some(self.last_activity)
            {
                slot.budget_marker = Some(self.last_activity);
                slot.restarts_budget += 1;
                let why = format!(
                    "memory budget: {} MB > {} MB while idle",
                    process::mib(rss),
                    self.settings.memory_budget_mb
                );
                self.restart_slot(sid, &why);
            }
        }
    }

    // ------------------------------------------------------------------------------------
    // Lint (L2)
    // ------------------------------------------------------------------------------------

    /// Whether `cid` gets lint results (LSP clients with `ts-r.lint.enable` not off).
    fn lint_on(&self, cid: ClientId) -> bool {
        self.clients
            .get(&cid)
            .is_some_and(|c| c.kind == Some(FrontKind::Lsp) && c.lint.is_on())
    }

    /// Lint clients that have `key` open.
    fn lint_clients_for(&self, key: &str) -> Vec<ClientId> {
        self.clients
            .iter()
            .filter(|(_, c)| {
                c.kind == Some(FrontKind::Lsp) && c.lint.is_on() && c.docs.contains_key(key)
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// Lints `key` with `cid`'s text after `delay` (moving an earlier pending request, so a
    /// burst of edits lints once).
    fn schedule_lint(&mut self, cid: ClientId, key: &str, delay: Duration) {
        if !self.lint_on(cid) || !lint::is_lintable(key) {
            return;
        }
        let due = self
            .lint_due
            .entry(key.to_string())
            .or_insert_with(|| (Instant::now(), BTreeSet::new()));
        due.0 = Instant::now() + delay;
        due.1.insert(cid);
    }

    /// Drops lint state of a file for clients that no longer lint it.
    fn lint_forget_if_unused(&mut self, key: &str) {
        let clients = self.lint_clients_for(key);
        if clients.is_empty() {
            self.lint_files.remove(key);
            self.lint_due.remove(key);
        } else if let Some(f) = self.lint_files.get_mut(key) {
            f.linted.retain(|c, _| clients.contains(c));
        }
    }

    /// `cid`'s last lint results for `key`.
    fn linted_for(&self, cid: ClientId, key: &str) -> Option<&Arc<lint::Linted>> {
        self.lint_files.get(key)?.linted.get(&cid)
    }

    fn ensure_lint_worker(&mut self) -> bool {
        if self.lint_worker.is_none() {
            let tx = self.tx.clone();
            match LintWorker::spawn(self.root.clone(), move |d| {
                let _ = tx.send(Event::Lint(Box::new(d)));
            }) {
                Ok(w) => self.lint_worker = Some(w),
                Err(e) => {
                    log(&format!("cannot start the lint worker: {e}"));
                    return false;
                }
            }
        }
        true
    }

    fn run_due_lints(&mut self) {
        let now = Instant::now();
        let due: Vec<String> = self
            .lint_due
            .iter()
            .filter(|(_, (t, _))| *t <= now)
            .map(|(k, _)| k.clone())
            .collect();
        for key in due {
            let Some((_, cids)) = self.lint_due.remove(&key) else {
                continue;
            };
            // The requesting clients' texts, else any lint client's that has the file open;
            // each different text once.
            let lint_clients = self.lint_clients_for(&key);
            let mut owners: Vec<ClientId> = cids
                .into_iter()
                .filter(|c| lint_clients.contains(c))
                .collect();
            if owners.is_empty() {
                owners.extend(lint_clients.first());
            }
            let mut jobs: Vec<lint::Job> = Vec::new();
            for c in owners {
                let Some(doc) = self.clients.get(&c).and_then(|cl| cl.docs.get(&key)) else {
                    continue;
                };
                if jobs.iter().all(|j| j.text != doc.text.text()) {
                    jobs.push(lint::Job {
                        key: key.clone(),
                        text: doc.text.text().to_string(),
                        client: c,
                    });
                }
            }
            if !jobs.is_empty()
                && self.ensure_lint_worker()
                && let Some(w) = &self.lint_worker
            {
                for j in jobs {
                    w.submit(j);
                }
            }
        }
    }

    fn on_lint_done(&mut self, d: lint::Done) {
        let s = &mut self.lint_stats;
        s.runs += 1;
        s.total += d.elapsed;
        s.max = s.max.max(d.elapsed);
        s.last = d.elapsed;
        if let Some(mut n) = d.new_config {
            if self.settings.type_backend == TypeBackend::TsR && crate::tsr::libs().is_some() {
                lint::typed_rules_run(&mut n);
            }
            self.lint_notes.insert(n.name.clone(), n);
        }
        if d.config_changed {
            // The worker has the new config already; every open file needs it.
            log("a file a lint config imports changed; re-linting open files");
            self.lint_stats.config_reloads += 1;
            self.lint_open_files();
        }
        let clients = self.lint_clients_for(&d.key);
        if clients.is_empty() {
            return;
        }
        if let Some(cfg) = &d.config {
            for &c in &clients {
                self.lint_note(c, cfg);
            }
        }
        let linted = match d.outcome {
            lint::Outcome::ParseError(_) => {
                // Keep the previous results; the type backend shows the syntax error.
                self.lint_stats.parse_errors += 1;
                return;
            }
            lint::Outcome::Linted(l) => l,
            lint::Outcome::NotLinted(text, _) => lint::Linted::empty(text),
        };
        // The results belong to the clients whose text was linted; the others keep theirs.
        let current: Vec<ClientId> = clients
            .into_iter()
            .filter(|c| {
                self.clients
                    .get(c)
                    .and_then(|cl| cl.docs.get(&d.key))
                    .is_some_and(|doc| doc.text.text() == linted.text.text())
            })
            .collect();
        let linted = Arc::new(linted);
        let file = self.lint_files.entry(d.key.clone()).or_default();
        file.config = d.config;
        for &c in &current {
            file.linted.insert(c, linted.clone());
        }
        for c in current {
            self.publish_merged(c, &d.key);
        }
    }

    /// The once-per-client note about rules a config enables that ts-r-lint does not run
    /// (or about a config that failed to load): a log message with the list every time a
    /// client first uses the config, one `window/showMessage` per client.
    fn lint_note(&mut self, cid: ClientId, config: &str) {
        let Some(msgs) = self
            .lint_notes
            .get(config)
            .and_then(lint::unsupported_message)
        else {
            return;
        };
        let failed = self
            .lint_notes
            .get(config)
            .is_some_and(|n| n.error.is_some());
        let Some(c) = self.clients.get_mut(&cid) else {
            return;
        };
        if !c.lint_notes_seen.insert(config.to_string()) {
            return;
        }
        let popup = failed || !c.lint_popup_shown;
        if !failed {
            c.lint_popup_shown = true;
        }
        let (short, long) = msgs;
        self.send_client(
            cid,
            &jsonrpc::notification("window/logMessage", json!({"type": 3, "message": long})),
        );
        if popup {
            self.send_client(
                cid,
                &jsonrpc::notification(
                    "window/showMessage",
                    json!({"type": if failed { 2 } else { 3 }, "message": short}),
                ),
            );
        }
    }

    /// Sends `cid` its type diagnostics for `key` plus the latest lint diagnostics.
    fn publish_merged(&self, cid: ClientId, key: &str) {
        let Some(c) = self.clients.get(&cid) else {
            return;
        };
        let Some(doc) = c.docs.get(key) else { return };
        let uri = doc.uri.clone().unwrap_or_else(|| uri::path_to_uri(key));
        let mut diags = c.type_diags.get(key).cloned().unwrap_or_default();
        if !self.lint_on(cid) {
            // The ts-r backend's type-aware lint diagnostics follow the lint switch.
            diags.retain(|d| d["source"] != lint::SOURCE);
        }
        if self.lint_on(cid)
            && let Some(l) = self.linted_for(cid, key)
        {
            diags.extend(l.lsp.iter().cloned());
        }
        self.send_client(
            cid,
            &jsonrpc::notification(
                "textDocument/publishDiagnostics",
                json!({"uri": uri, "diagnostics": diags}),
            ),
        );
    }

    /// Type-backend diagnostics for an LSP client: remembered, then published with lint's.
    fn publish_type_diags(&mut self, cid: ClientId, key: &str, uri: String, diags: Vec<Value>) {
        let mut all = diags.clone();
        if !self.lint_on(cid) {
            // The ts-r backend's type-aware lint diagnostics follow the lint switch.
            all.retain(|d| d["source"] != lint::SOURCE);
        }
        let open = match self.clients.get_mut(&cid) {
            Some(c) if c.docs.contains_key(key) => {
                c.type_diags.insert(key.to_string(), diags);
                true
            }
            _ => false,
        };
        if open
            && self.lint_on(cid)
            && let Some(l) = self.linted_for(cid, key)
        {
            all.extend(l.lsp.iter().cloned());
        }
        self.send_client(
            cid,
            &jsonrpc::notification(
                "textDocument/publishDiagnostics",
                json!({"uri": uri, "diagnostics": all}),
            ),
        );
    }

    /// A document was saved: lint at once; a saved config file re-reads configs.
    fn lint_saved(&mut self, cid: ClientId, key: &str) {
        if lint::is_config_file(key) {
            self.lint_reload_configs(key);
        }
        self.schedule_lint(cid, key, Duration::ZERO);
    }

    /// Config files changed: forget loaded configs and re-lint every open file. (No rule
    /// reads other source files yet, so an edit to a source file re-lints only that file;
    /// type-aware rules will add their dependents here when the checker lands.)
    fn lint_reload_configs(&mut self, why: &str) {
        log(&format!(
            "lint config changed ({why}); re-linting open files"
        ));
        self.lint_stats.config_reloads += 1;
        if let Some(w) = &self.lint_worker {
            w.reload_configs();
        }
        self.lint_notes.clear();
        for c in self.clients.values_mut() {
            c.lint_notes_seen.clear();
        }
        self.lint_open_files();
    }

    /// Lints every open file at once.
    fn lint_open_files(&mut self) {
        let mut todo = Vec::new();
        for (id, c) in &self.clients {
            for key in c.docs.keys() {
                todo.push((*id, key.clone()));
            }
        }
        for (c, key) in todo {
            self.schedule_lint(c, &key, Duration::ZERO);
        }
    }

    fn lint_watched_files(&mut self, params: &Value) {
        let changed = params["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["uri"].as_str())
            .map(uri::key_for_uri)
            .find(|k| lint::is_config_file(k));
        if let Some(k) = changed {
            self.lint_reload_configs(&k);
        }
    }

    /// `workspace/didChangeConfiguration`: `ts-r.lint.enable` for this client.
    fn lint_settings_changed(&mut self, cid: ClientId, settings: &Value) {
        let Some(enable) = ClientLint::from_json(settings).enable else {
            return;
        };
        let Some(c) = self.clients.get_mut(&cid) else {
            return;
        };
        let was = c.lint.is_on();
        c.lint = enable;
        if was == enable.is_on() {
            return;
        }
        let keys: Vec<String> = c.docs.keys().cloned().collect();
        log(&format!(
            "{}: ts-r.lint.enable = {}",
            c.name,
            enable.as_str()
        ));
        for key in keys {
            if enable.is_on() {
                self.schedule_lint(cid, &key, Duration::ZERO);
            } else {
                // Republish without lint diagnostics.
                self.publish_merged(cid, &key);
                self.lint_forget_if_unused(&key);
            }
        }
    }

    /// Lint code actions for a `textDocument/codeAction` request, if the client's text is
    /// the text that was linted.
    fn lint_code_actions(&self, cid: ClientId, params: &Value) -> Vec<Value> {
        if !self.lint_on(cid) {
            return Vec::new();
        }
        let Some(u) = params["textDocument"]["uri"].as_str() else {
            return Vec::new();
        };
        let key = uri::key_for_uri(u);
        let (Some(doc), Some(l)) = (
            self.clients.get(&cid).and_then(|c| c.docs.get(&key)),
            self.linted_for(cid, &key),
        ) else {
            return Vec::new();
        };
        if l.text.text() != doc.text.text() {
            return Vec::new();
        }
        let Some((start, end)) = crate::text::lsp_range(&params["range"]) else {
            return Vec::new();
        };
        let only: Vec<String> = params["context"]["only"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        lint::code_actions(l, u, start, end, &only)
    }

    fn lint_status(&self) -> Value {
        let s = &self.lint_stats;
        let avg = if s.runs == 0 {
            0.0
        } else {
            s.total.as_secs_f64() * 1000.0 / s.runs as f64
        };
        json!({
            "worker": self.lint_worker.is_some(),
            "files": self.lint_files.len(),
            "pending": self.lint_due.len(),
            "runs": s.runs,
            "avgMs": avg,
            "maxMs": s.max.as_secs_f64() * 1000.0,
            "lastMs": s.last.as_secs_f64() * 1000.0,
            "parseErrors": s.parse_errors,
            "configReloads": s.config_reloads,
            "configs": self.lint_notes.keys().collect::<Vec<_>>(),
        })
    }

    fn status_json(&self) -> Value {
        let now = Instant::now();
        let clients: Vec<Value> = self
            .clients
            .iter()
            .filter(|(_, c)| c.kind.is_some_and(|k| k != FrontKind::Status))
            .map(|(id, c)| {
                json!({
                    "id": id, "kind": c.kind.map(FrontKind::as_str), "name": c.name, "pid": c.pid,
                    "slot": c.slot, "openDocs": c.docs.len(),
                    "lint": if c.kind == Some(FrontKind::Lsp) { c.lint.as_str() } else { "off" },
                    "lintOnly": c.lint_only,
                    "idleSeconds": now.duration_since(c.last_activity).as_secs_f64(),
                })
            })
            .collect();
        let slots: Vec<Value> = self
            .slots
            .iter()
            .map(|(id, s)| {
                json!({
                    "id": id, "backend": s.spec.label, "protocol": format!("{:?}", s.spec.protocol),
                    "state": format!("{:?}", s.state), "pid": s.backend.as_ref().and_then(|b| b.pid()),
                    "rssMB": s.rss.map(process::mib), "members": s.members, "openDocs": s.mirror.docs.len(),
                    "restarts": {"budget": s.restarts_budget, "crash": s.restarts_crash, "idleStops": s.idle_stops},
                    "firstReadySeconds": s.first_ready.map(|d| d.as_secs_f64()),
                    "diagnosticFiles": s.diags.len(),
                    "pending": s.pending.len(),
                    "tsr": s.tsr_status,
                })
            })
            .collect();
        json!({
            "daemonPid": std::process::id(),
            "version": env!("CARGO_PKG_VERSION"),
            "root": self.root,
            "mode": if self.per_client { "per-client" } else { "shared" },
            "uptimeSeconds": now.duration_since(self.started).as_secs_f64(),
            "idleSeconds": now.duration_since(self.last_activity).as_secs_f64(),
            "daemonRssMB": self.daemon_rss.map(process::mib),
            "settings": self.settings.to_json(),
            "splits": self.stats.splits,
            "clientsServed": self.stats.clients_served,
            "clients": clients,
            "slots": slots,
            "lint": self.lint_status(),
        })
    }
}

impl Slot {
    fn new(key: String, spec: BackendSpec, cancel_prefix: Option<String>) -> Slot {
        Slot {
            key,
            spec,
            backend: None,
            generation: 0,
            state: SlotState::Stopped,
            members: BTreeSet::new(),
            mirror: Mirror::default(),
            pending: HashMap::new(),
            queue: Vec::new(),
            next_seq: 1,
            init_params: None,
            init_result: None,
            init_waiters: Vec::new(),
            registrations: BTreeMap::new(),
            last_config: None,
            ts_configure: None,
            ts_inferred: None,
            diags: BTreeMap::new(),
            geterr_due: None,
            geterr_inflight: None,
            geterr_again: false,
            cancel_prefix,
            cancel_files: BTreeSet::new(),
            rss: None,
            started_at: None,
            budget_marker: None,
            crashes: Vec::new(),
            restarts_budget: 0,
            restarts_crash: 0,
            idle_stops: 0,
            first_ready: None,
            tsr_status: None,
        }
    }

    fn seq(&mut self) -> i64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }
}

fn ts_edit(e: &Value) -> Option<(Option<(Pos, Pos)>, String)> {
    let start = e.get("start").and_then(Pos::from_ts)?;
    let end = e.get("end").and_then(Pos::from_ts)?;
    Some((Some((start, end)), e.get("newText")?.as_str()?.to_string()))
}

fn ts_client_doc(file: &str, text: String, f: &Value) -> ClientDoc {
    let script_kind = f["scriptKindName"].as_str().map(str::to_string);
    let language = match script_kind.as_deref() {
        Some("TSX") => "typescriptreact",
        Some("JS") => "javascript",
        Some("JSX") => "javascriptreact",
        Some(_) => "typescript",
        None => language_from_path(file),
    };
    let mut doc = ClientDoc::new(text, language);
    doc.script_kind = script_kind;
    doc.project_root = f["projectRootPath"].as_str().map(str::to_string);
    doc
}

fn language_from_path(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        _ => "typescript",
    }
}

/// `updateOpen` arguments for a batch of sync ops.
fn update_open_args(ops: &[SyncOp], root: &Path) -> Value {
    let mut open = Vec::new();
    let mut changed = Vec::new();
    let mut closed = Vec::new();
    for op in ops {
        match op {
            SyncOp::Open {
                key,
                language_id,
                text,
                script_kind,
                project_root,
                ..
            } => {
                let kind = script_kind
                    .clone()
                    .or_else(|| translate::script_kind(language_id).map(str::to_string));
                let mut f = json!({
                    "file": key, "fileContent": text,
                    "projectRootPath": project_root.clone().unwrap_or_else(|| root.to_string_lossy().into_owned()),
                });
                if let Some(k) = kind {
                    f["scriptKindName"] = json!(k);
                }
                open.push(f);
            }
            SyncOp::Edits { key, edits, .. } => {
                // tsserver applies a file's textChanges last to first, so reverse our
                // in-order list to get the same result.
                let changes: Vec<Value> = edits
                    .iter()
                    .rev()
                    .map(|((a, b), t)| json!({"start": a.to_ts(), "end": b.to_ts(), "newText": t}))
                    .collect();
                changed.push(json!({"fileName": key, "textChanges": changes}));
            }
            SyncOp::Close { key, .. } => closed.push(json!(key)),
        }
    }
    json!({"openFiles": open, "changedFiles": changed, "closedFiles": closed})
}

/// The LSP notification for one sync op.
fn lsp_sync_message(op: &SyncOp) -> Value {
    match op {
        SyncOp::Open {
            uri,
            language_id,
            text,
            version,
            ..
        } => jsonrpc::notification(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": language_id, "version": version, "text": text}}),
        ),
        SyncOp::Edits {
            uri,
            version,
            edits,
            ..
        } => {
            let changes: Vec<Value> = edits
                .iter()
                .map(|((a, b), t)| json!({"range": crate::text::to_lsp_range(*a, *b), "text": t}))
                .collect();
            jsonrpc::notification(
                "textDocument/didChange",
                json!({"textDocument": {"uri": uri, "version": version}, "contentChanges": changes}),
            )
        }
        SyncOp::Close { uri, .. } => jsonrpc::notification(
            "textDocument/didClose",
            json!({"textDocument": {"uri": uri}}),
        ),
    }
}

/// Whether a client keeps the daemon alive (has a backend slot or a lint-only connection).
fn is_active(c: &Client) -> bool {
    c.slot.is_some() || c.lint_only
}

/// Adds lint's code action kinds to an LSP backend's capabilities.
fn add_lint_capabilities(caps: &mut Value) {
    let Some(obj) = caps.as_object_mut() else {
        return;
    };
    match obj.get_mut("codeActionProvider") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => {
            obj.insert("codeActionProvider".into(), lint::code_action_provider());
        }
        Some(Value::Object(o)) => {
            if let Some(Value::Array(kinds)) = o.get_mut("codeActionKinds") {
                for k in ["quickfix", lint::FIX_ALL_KIND] {
                    if !kinds.iter().any(|x| x == k) {
                        kinds.push(json!(k));
                    }
                }
            }
        }
        _ => {}
    }
}

fn default_configure() -> Value {
    json!({
        "hostInfo": "ts-r-lsp",
        "preferences": {
            "includeCompletionsForModuleExports": true,
            "includeCompletionsWithInsertText": true,
            "allowIncompleteCompletions": true,
            "includeCompletionsForImportStatements": true,
        }
    })
}

fn default_init_params(root: &Path) -> Value {
    let uri = uri::path_to_uri(&root.to_string_lossy());
    json!({
        "processId": null,
        "rootUri": uri,
        "workspaceFolders": [{"uri": uri, "name": root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()}],
        "capabilities": {
            "textDocument": {
                "publishDiagnostics": {"relatedInformation": true},
                "hover": {"contentFormat": ["markdown", "plaintext"]},
                "completion": {"completionItem": {"snippetSupport": false}},
            }
        }
    })
}

/// Writes a timestamped line to the daemon log (stderr).
pub fn log(msg: &str) {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    eprintln!("[{t:.3}] {msg}");
}

/// Runs a daemon for `root` until it exits: takes the daemon lock, binds the endpoint and
/// serves. Returns `Ok(false)` if another daemon already serves this root.
pub fn serve(root: &Path, factory: Box<dyn BackendFactory>) -> std::io::Result<bool> {
    let ep = Endpoint::for_root(root);
    let Some(_lock) = ipc::take_daemon_lock(&ep)? else {
        return Ok(false);
    };
    let listener = ipc::bind(&ep)?;
    let mut daemon = Daemon::new(
        ep.root.clone(),
        ep.cancel_dir(),
        Settings::from_env(),
        factory,
    );
    if cfg!(unix) {
        daemon.socket_path = Some(PathBuf::from(&ep.address));
    }
    let tx = daemon.sender();
    thread::Builder::new()
        .name("accept".into())
        .spawn(move || Daemon::accept_loop(listener, tx))?;
    daemon.run();
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_open_reverses_edits_for_tsserver() {
        let ops = vec![
            SyncOp::Open {
                key: "/w/a.ts".into(),
                uri: "file:///w/a.ts".into(),
                language_id: "typescriptreact".into(),
                text: "x".into(),
                version: 1,
                script_kind: None,
                project_root: None,
            },
            SyncOp::Edits {
                key: "/w/b.ts".into(),
                uri: "file:///w/b.ts".into(),
                version: 2,
                edits: vec![
                    ((Pos::new(0, 0), Pos::new(0, 1)), "XX".into()),
                    ((Pos::new(0, 2), Pos::new(0, 3)), "Y".into()),
                ],
            },
            SyncOp::Close {
                key: "/w/c.ts".into(),
                uri: "file:///w/c.ts".into(),
            },
        ];
        let a = update_open_args(&ops, Path::new("/w"));
        assert_eq!(a["openFiles"][0]["scriptKindName"], "TSX");
        assert_eq!(a["openFiles"][0]["projectRootPath"], "/w");
        let tc = &a["changedFiles"][0]["textChanges"];
        assert_eq!(tc[0]["newText"], "Y");
        assert_eq!(tc[1]["start"], json!({"line": 1, "offset": 1}));
        assert_eq!(a["closedFiles"], json!(["/w/c.ts"]));
    }

    #[test]
    fn lsp_sync_messages() {
        let m = lsp_sync_message(&SyncOp::Edits {
            key: "/a".into(),
            uri: "file:///a".into(),
            version: 3,
            edits: vec![((Pos::new(1, 0), Pos::new(1, 2)), "z".into())],
        });
        assert_eq!(m["method"], "textDocument/didChange");
        assert_eq!(m["params"]["textDocument"]["version"], 3);
        assert_eq!(
            m["params"]["contentChanges"][0]["range"]["end"],
            json!({"line": 1, "character": 2})
        );
    }

    #[test]
    fn languages_from_tsserver_clients() {
        assert_eq!(
            ts_client_doc("/a.tsx", "".into(), &json!({})).language_id,
            "typescriptreact"
        );
        assert_eq!(
            ts_client_doc("/a.ts", "".into(), &json!({"scriptKindName": "JS"})).language_id,
            "javascript"
        );
        assert_eq!(language_from_path("/x.mjs"), "javascript");
    }
}
