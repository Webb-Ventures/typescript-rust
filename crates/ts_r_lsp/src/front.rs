//! Thin stdio fronts that editors start. They find or spawn the workspace daemon, say hello
//! and then shuttle messages.
//!
//! * [`run_lsp_front`]: an LSP server on stdio (what Neovim, Zed, Helix and the VS Code
//!   extension's LSP mode start). The root comes from `--root`, then the `initialize`
//!   request (`rootUri`, `workspaceFolders`, `rootPath`), then the cwd. If the daemon
//!   connection drops (crash), the front respawns it and replays `initialize`, `initialized`
//!   and its open documents; requests in flight get a `RequestFailed` error.
//! * [`run_tsserver_front`]: a tsserver stand-in on stdio (newline-delimited JSON in,
//!   `Content-Length` frames out), for editors that drive tsserver directly (VS Code's
//!   built-in TypeScript extension through `typescript.tsserver.nodePath`, or tests). It
//!   watches the editor's `--cancellationPipeName` files and forwards cancellations. If the
//!   daemon goes away it exits; editors restart tsserver and reopen their files themselves.

use crate::ipc::{self, Endpoint, Stream};
use crate::jsonrpc::{self, IdKey, Kind, codes};
use crate::text::TextDoc;
use crate::uri;
use crate::wire::{FrontKind, Hello, TsServerArgs};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Options shared by the fronts.
#[derive(Clone, Debug, Default)]
pub struct FrontOptions {
    /// Workspace root override.
    pub root: Option<PathBuf>,
    /// `ts-r.*` settings from the command line.
    pub settings: Value,
    /// tsserver to fall back to when the project has none.
    pub fallback_tsserver: Option<String>,
    /// Node binary for the daemon to run tsserver with, when not on PATH.
    pub node_path: Option<String>,
    /// Display name.
    pub name: Option<String>,
}

/// Connects to (or spawns) the daemon for `root` and completes the hello handshake.
/// Returns the stream and the welcome message.
pub fn connect(root: &Path, hello: &Hello) -> io::Result<(Stream, Value)> {
    // A daemon that is shutting down may refuse or drop us; retry briefly.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match connect_once(root, hello) {
            Ok(x) => return Ok(x),
            // A refusal from a live daemon (wrong root, no TypeScript) is final.
            Err(e) if e.kind() == io::ErrorKind::InvalidInput || Instant::now() >= deadline => {
                return Err(e);
            }
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn connect_once(root: &Path, hello: &Hello) -> io::Result<(Stream, Value)> {
    let ep = Endpoint::for_root(root);
    let exe = std::env::current_exe()?;
    let mut stream = ipc::ensure_daemon(&ep, &exe)?;
    jsonrpc::write_message(&mut stream, &hello.to_json())?;
    let mut r = BufReader::new(stream.try_clone()?);
    let body = jsonrpc::read_frame(&mut r)?.ok_or_else(|| {
        io::Error::new(io::ErrorKind::UnexpectedEof, "daemon closed during hello")
    })?;
    let welcome: Value = serde_json::from_slice(&body).map_err(io::Error::other)?;
    match welcome.get("tsr").and_then(Value::as_str) {
        Some("welcome") => Ok((stream, welcome)),
        Some("status") => Ok((stream, welcome)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            welcome["message"]
                .as_str()
                .unwrap_or("daemon refused the connection")
                .to_string(),
        )),
    }
}

/// Asks the daemon for its status (does not start one).
pub fn status(root: &Path) -> io::Result<Value> {
    let ep = Endpoint::for_root(root);
    let mut stream = ipc::connect(&ep)?;
    jsonrpc::write_message(
        &mut stream,
        &Hello::new(FrontKind::Status, &ep.root).to_json(),
    )?;
    let mut r = BufReader::new(stream.try_clone()?);
    let body = jsonrpc::read_frame(&mut r)?.ok_or_else(|| io::Error::other("no answer"))?;
    let v: Value = serde_json::from_slice(&body).map_err(io::Error::other)?;
    Ok(v.get("status").cloned().unwrap_or(v))
}

/// Asks the daemon for `root` to exit.
pub fn stop(root: &Path) -> io::Result<()> {
    let ep = Endpoint::for_root(root);
    let mut stream = ipc::connect(&ep)?;
    jsonrpc::write_message(
        &mut stream,
        &Hello::new(FrontKind::Status, &ep.root).to_json(),
    )?;
    jsonrpc::write_message(&mut stream, &json!({"tsr": "stop"}))?;
    Ok(())
}

fn root_from_initialize(params: &Value) -> Option<PathBuf> {
    let from_uri = |v: &Value| v.as_str().and_then(uri::uri_to_path).map(PathBuf::from);
    from_uri(&params["rootUri"])
        .or_else(|| {
            params["workspaceFolders"]
                .as_array()?
                .first()
                .and_then(|f| from_uri(&f["uri"]))
        })
        .or_else(|| params["rootPath"].as_str().map(PathBuf::from))
}

enum FrontEvent {
    FromEditor(Value),
    EditorEof,
    DaemonGone(u64),
}

/// State the LSP front keeps to survive a daemon restart.
#[derive(Default)]
struct Replay {
    initialize: Option<Value>,
    initialized: bool,
    docs: BTreeMap<String, (String, i64, TextDoc)>,
    in_flight: BTreeSet<IdKey>,
}

impl Replay {
    fn observe(&mut self, msg: &Value) {
        match jsonrpc::classify(msg) {
            Some(Kind::Request { id, method }) => {
                if method == "initialize" {
                    self.initialize = Some(msg.clone());
                }
                if let Some(k) = IdKey::from_value(id) {
                    self.in_flight.insert(k);
                }
            }
            Some(Kind::Notification { method }) => {
                let p = &msg["params"];
                let u = p["textDocument"]["uri"].as_str().unwrap_or("").to_string();
                match method {
                    "initialized" => self.initialized = true,
                    "textDocument/didOpen" => {
                        let lang = p["textDocument"]["languageId"]
                            .as_str()
                            .unwrap_or("typescript")
                            .to_string();
                        let version = p["textDocument"]["version"].as_i64().unwrap_or(0);
                        let text = p["textDocument"]["text"].as_str().unwrap_or("");
                        self.docs.insert(u, (lang, version, TextDoc::new(text)));
                    }
                    "textDocument/didChange" => {
                        if let Some((_, v, doc)) = self.docs.get_mut(&u) {
                            *v = p["textDocument"]["version"].as_i64().unwrap_or(*v + 1);
                            for c in p["contentChanges"].as_array().into_iter().flatten() {
                                doc.apply_lsp_change(c);
                            }
                        }
                    }
                    "textDocument/didClose" => {
                        self.docs.remove(&u);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn messages(&self) -> Vec<Value> {
        let mut out = Vec::new();
        if let Some(init) = &self.initialize {
            let mut m = init.clone();
            m["id"] = json!("tsr-reinit");
            out.push(m);
        }
        if self.initialized {
            out.push(jsonrpc::notification("initialized", json!({})));
        }
        for (u, (lang, version, doc)) in &self.docs {
            out.push(jsonrpc::notification(
                "textDocument/didOpen",
                json!({"textDocument": {"uri": u, "languageId": lang, "version": version, "text": doc.text()}}),
            ));
        }
        out
    }
}

type SharedOut = Arc<Mutex<io::Stdout>>;

fn write_out(out: &SharedOut, msg: &Value) {
    let mut o = out.lock().unwrap_or_else(|e| e.into_inner());
    let _ = jsonrpc::write_message(&mut *o, msg);
}

/// Runs the LSP front on stdio until the editor exits. Returns the process exit code.
pub fn run_lsp_front(opts: FrontOptions) -> i32 {
    let out: SharedOut = Arc::new(Mutex::new(io::stdout()));
    let (tx, rx) = mpsc::channel::<FrontEvent>();
    {
        let tx = tx.clone();
        thread::spawn(move || {
            let stdin = io::stdin();
            let mut r = BufReader::with_capacity(1 << 16, stdin.lock());
            while let Ok(Some(body)) = jsonrpc::read_frame(&mut r) {
                match serde_json::from_slice::<Value>(&body) {
                    Ok(v) => {
                        if tx.send(FrontEvent::FromEditor(v)).is_err() {
                            return;
                        }
                    }
                    Err(e) => eprintln!("ts-r-lsp: bad JSON from editor: {e}"),
                }
            }
            let _ = tx.send(FrontEvent::EditorEof);
        });
    }

    // Wait for `initialize` to learn the root.
    let init = loop {
        match rx.recv() {
            Ok(FrontEvent::FromEditor(m)) => match jsonrpc::classify(&m) {
                Some(Kind::Request {
                    method: "initialize",
                    ..
                }) => break m,
                Some(Kind::Request { id, .. }) => write_out(
                    &out,
                    &jsonrpc::error_response(
                        id.clone(),
                        codes::SERVER_NOT_INITIALIZED,
                        "send initialize first",
                    ),
                ),
                Some(Kind::Notification { method: "exit" }) => return 0,
                _ => {}
            },
            _ => return 0,
        }
    };
    let params = init.get("params").cloned().unwrap_or(Value::Null);
    let root = opts
        .root
        .clone()
        .or_else(|| root_from_initialize(&params))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let mut hello = Hello::new(FrontKind::Lsp, &root);
    hello.name = opts
        .name
        .clone()
        .unwrap_or_else(|| format!("lsp front {}", std::process::id()));
    hello.settings = merge_settings(&opts.settings, &params["initializationOptions"]);
    hello.fallback_tsserver = opts.fallback_tsserver.clone();
    hello.node_path = opts.node_path.clone();

    let mut replay = Replay::default();
    let generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let swallow_reinit = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let start = |replay: &Replay, out: &SharedOut| -> Option<Stream> {
        match connect(&root, &hello) {
            Ok((stream, _welcome)) => {
                let generation_now =
                    generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                let read = stream.try_clone().ok()?;
                let out2 = out.clone();
                let tx2 = tx.clone();
                let swallow = swallow_reinit.clone();
                thread::spawn(move || {
                    let mut r = BufReader::with_capacity(1 << 16, read);
                    while let Ok(Some(body)) = jsonrpc::read_frame(&mut r) {
                        let Ok(v) = serde_json::from_slice::<Value>(&body) else {
                            continue;
                        };
                        if v.get("id").and_then(Value::as_str) == Some("tsr-reinit") {
                            swallow.store(false, std::sync::atomic::Ordering::SeqCst);
                            continue;
                        }
                        if let Some(Kind::Response { id }) = jsonrpc::classify(&v)
                            && let Some(k) = IdKey::from_value(id)
                        {
                            let _ = tx2.send(FrontEvent::FromEditor(
                                json!({"tsr-front": "answered", "id": k.to_value()}),
                            ));
                        }
                        write_out(&out2, &v);
                    }
                    let _ = tx2.send(FrontEvent::DaemonGone(generation_now));
                });
                let mut s = stream;
                for m in replay.messages() {
                    if m["id"] == json!("tsr-reinit") {
                        swallow_reinit.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    let _ = jsonrpc::write_message(&mut s, &m);
                }
                Some(s)
            }
            Err(e) => {
                eprintln!("ts-r-lsp: {e}");
                None
            }
        }
    };

    let mut stream = match start(&replay, &out) {
        Some(s) => s,
        None => {
            let id = init["id"].clone();
            write_out(
                &out,
                &jsonrpc::error_response(
                    id,
                    codes::INTERNAL_ERROR,
                    format!("ts-r-lsp could not start for {}", root.display()),
                ),
            );
            return 1;
        }
    };
    replay.observe(&init);
    let _ = jsonrpc::write_message(&mut stream, &init);
    let mut shutdown = false;
    let mut reconnects: Vec<Instant> = Vec::new();
    loop {
        let Ok(ev) = rx.recv() else { return 1 };
        match ev {
            FrontEvent::FromEditor(m) => {
                if m.get("tsr-front").is_some() {
                    if let Some(k) = IdKey::from_value(&m["id"]) {
                        replay.in_flight.remove(&k);
                    }
                    continue;
                }
                replay.observe(&m);
                let kind = jsonrpc::classify(&m);
                if matches!(
                    kind,
                    Some(Kind::Request {
                        method: "shutdown",
                        ..
                    })
                ) {
                    shutdown = true;
                }
                let exit = matches!(kind, Some(Kind::Notification { method: "exit" }));
                let _ = jsonrpc::write_message(&mut stream, &m);
                if exit {
                    stream.shutdown();
                    return if shutdown { 0 } else { 1 };
                }
            }
            FrontEvent::EditorEof => {
                stream.shutdown();
                return 0;
            }
            FrontEvent::DaemonGone(g) => {
                if g != generation.load(std::sync::atomic::Ordering::SeqCst) {
                    continue;
                }
                for id in std::mem::take(&mut replay.in_flight) {
                    write_out(
                        &out,
                        &jsonrpc::error_response(
                            id.to_value(),
                            codes::REQUEST_FAILED,
                            "ts-r-lsp daemon restarted",
                        ),
                    );
                }
                let now = Instant::now();
                reconnects.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
                reconnects.push(now);
                if shutdown || reconnects.len() > 5 {
                    return 1;
                }
                eprintln!("ts-r-lsp: daemon connection lost; reconnecting");
                thread::sleep(Duration::from_millis(100));
                match start(&replay, &out) {
                    Some(s) => stream = s,
                    None => return 1,
                }
            }
        }
    }
}

fn merge_settings(base: &Value, extra: &Value) -> Value {
    let mut out = base.as_object().cloned().unwrap_or_default();
    if let Some(e) = extra.as_object() {
        for (k, v) in e {
            out.insert(k.clone(), v.clone());
        }
    }
    Value::Object(out)
}

/// Runs the tsserver stand-in on stdio. `args` are tsserver's command-line arguments.
pub fn run_tsserver_front(args: &[String], opts: FrontOptions) -> i32 {
    let ts_args = TsServerArgs::parse(args);
    let out: SharedOut = Arc::new(Mutex::new(io::stdout()));
    let (tx, rx) = mpsc::channel::<FrontEvent>();
    {
        let tx = tx.clone();
        thread::spawn(move || {
            let stdin = io::stdin();
            let mut r = BufReader::with_capacity(1 << 16, stdin.lock());
            while let Ok(Some(line)) = jsonrpc::read_line_or_frame(&mut r) {
                match serde_json::from_slice::<Value>(&line) {
                    Ok(v) => {
                        if tx.send(FrontEvent::FromEditor(v)).is_err() {
                            return;
                        }
                    }
                    Err(e) => eprintln!("ts-r-lsp: bad tsserver request: {e}"),
                }
            }
            let _ = tx.send(FrontEvent::EditorEof);
        });
    }

    // Find the root: option, environment, or the first file the editor opens.
    let mut root = opts
        .root
        .clone()
        .or_else(|| std::env::var_os("TS_R_WORKSPACE_ROOT").map(PathBuf::from));
    let mut buffered: Vec<Value> = Vec::new();
    let mut answered_locally: BTreeSet<i64> = BTreeSet::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while root.is_none() {
        let wait = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(FrontEvent::FromEditor(m)) => {
                if let Some(r) = project_root_of(&m) {
                    root = Some(r);
                }
                let command = m["command"].as_str().unwrap_or("");
                if matches!(command, "configure" | "compilerOptionsForInferredProjects")
                    && let Some(seq) = m["seq"].as_i64()
                {
                    // The editor may wait for these before opening files; answer now.
                    answered_locally.insert(seq);
                    write_tsserver_out(
                        &out,
                        &json!({"seq": 0, "type": "response", "command": command, "request_seq": seq, "success": true}),
                    );
                }
                buffered.push(m);
            }
            Ok(FrontEvent::EditorEof) => return 0,
            Ok(FrontEvent::DaemonGone(_)) => {}
            Err(_) => {
                root = std::env::current_dir().ok();
                break;
            }
        }
    }
    let root = root.unwrap_or_else(|| PathBuf::from("."));
    let mut hello = Hello::new(FrontKind::TsServer, &root);
    hello.name = opts
        .name
        .clone()
        .unwrap_or_else(|| format!("tsserver front {}", std::process::id()));
    hello.settings = opts.settings.clone();
    hello.tsserver = ts_args.clone();
    hello.fallback_tsserver = opts.fallback_tsserver.clone();
    hello.node_path = opts.node_path.clone();
    let (mut stream, _welcome) = match connect(&root, &hello) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("ts-r-lsp: {e}");
            return 1;
        }
    };

    // Requests the editor may cancel through files named <pipe prefix><seq>.
    let outstanding: Arc<Mutex<BTreeSet<i64>>> = Arc::new(Mutex::new(BTreeSet::new()));
    let suppressed = Arc::new(Mutex::new(answered_locally));
    {
        let read = match stream.try_clone() {
            Ok(r) => r,
            Err(_) => return 1,
        };
        let out = out.clone();
        let tx = tx.clone();
        let outstanding = outstanding.clone();
        let suppressed = suppressed.clone();
        thread::spawn(move || {
            let mut r = BufReader::with_capacity(1 << 16, read);
            while let Ok(Some(body)) = jsonrpc::read_frame(&mut r) {
                let Ok(v) = serde_json::from_slice::<Value>(&body) else {
                    continue;
                };
                let done = match v["type"].as_str() {
                    Some("response") => v["request_seq"].as_i64(),
                    Some("event") if v["event"] == "requestCompleted" => {
                        v["body"]["request_seq"].as_i64()
                    }
                    _ => None,
                };
                if let Some(seq) = done {
                    lock(&outstanding).remove(&seq);
                    if v["type"] == "response" && lock(&suppressed).remove(&seq) {
                        continue;
                    }
                }
                write_tsserver_out(&out, &v);
            }
            let _ = tx.send(FrontEvent::DaemonGone(0));
        });
    }
    // The cancellation watcher and the forwarding loop write through clones of one socket,
    // and a frame is several writes: one frame at a time, or the daemon loses framing.
    let frame_gate = Arc::new(Mutex::new(()));
    if let Some(pipe) = ts_args.cancellation_pipe.clone() {
        let prefix = pipe.trim_end_matches('*').to_string();
        let outstanding = outstanding.clone();
        let frame_gate = frame_gate.clone();
        let mut ctl = match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return 1,
        };
        thread::spawn(move || {
            let mut sent: BTreeSet<i64> = BTreeSet::new();
            loop {
                thread::sleep(Duration::from_millis(50));
                let seqs: Vec<i64> = lock(&outstanding).iter().copied().collect();
                sent.retain(|s| seqs.contains(s));
                for seq in seqs {
                    if !sent.contains(&seq) && Path::new(&format!("{prefix}{seq}")).exists() {
                        sent.insert(seq);
                        let cancel = json!({"tsr": "cancel", "seq": seq});
                        if write_gated(&frame_gate, &mut ctl, &cancel).is_err() {
                            return;
                        }
                    }
                }
            }
        });
    }
    let forward = |stream: &mut Stream, m: &Value| -> bool {
        if m["type"] == "request"
            && let Some(seq) = m["seq"].as_i64()
        {
            lock(&outstanding).insert(seq);
        }
        let exit = m["command"] == "exit";
        let _ = write_gated(&frame_gate, stream, m);
        exit
    };
    for m in buffered {
        if forward(&mut stream, &m) {
            return 0;
        }
    }
    loop {
        match rx.recv() {
            Ok(FrontEvent::FromEditor(m)) => {
                if forward(&mut stream, &m) {
                    stream.shutdown();
                    return 0;
                }
            }
            Ok(FrontEvent::EditorEof) => {
                stream.shutdown();
                return 0;
            }
            Ok(FrontEvent::DaemonGone(_)) | Err(_) => {
                eprintln!("ts-r-lsp: daemon connection lost");
                return 1;
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Writes one whole frame while holding `gate`, shared by every writer of the same socket.
fn write_gated<W: Write>(gate: &Mutex<()>, w: &mut W, msg: &Value) -> io::Result<()> {
    let _held = lock(gate);
    jsonrpc::write_message(w, msg)
}

fn write_tsserver_out(out: &SharedOut, msg: &Value) {
    let Ok(body) = serde_json::to_vec(msg) else {
        return;
    };
    let mut o = lock(out);
    let _ = write!(o, "Content-Length: {}\r\n\r\n", body.len() + 1);
    let _ = o.write_all(&body);
    let _ = o.write_all(b"\n");
    let _ = o.flush();
}

fn project_root_of(m: &Value) -> Option<PathBuf> {
    let a = &m["arguments"];
    a["openFiles"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(std::iter::once(a))
        .find_map(|f| {
            f["projectRootPath"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clone of one byte sink that takes a byte per write, like a socket handle under load.
    struct Trickle(Arc<Mutex<Vec<u8>>>);

    impl Write for Trickle {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let Some(&b) = buf.first() else { return Ok(0) };
            lock(&self.0).push(b);
            thread::yield_now();
            Ok(1)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn gated_writers_on_one_socket_keep_frames_whole() {
        let sink = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(Mutex::new(()));
        let writers: Vec<_> = (0..2)
            .map(|t| {
                let (sink, gate) = (sink.clone(), gate.clone());
                thread::spawn(move || {
                    let mut w = Trickle(sink);
                    for seq in 0..100 {
                        let m = json!({"tsr": "cancel", "seq": seq, "from": t});
                        write_gated(&gate, &mut w, &m).unwrap();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        let bytes = lock(&sink).clone();
        let mut r = BufReader::new(&bytes[..]);
        let mut n = 0;
        while let Some(body) = jsonrpc::read_frame(&mut r).unwrap() {
            serde_json::from_slice::<Value>(&body).unwrap();
            n += 1;
        }
        assert_eq!(n, 200);
    }

    #[test]
    fn roots_from_initialize() {
        assert_eq!(
            root_from_initialize(&json!({"rootUri": "file:///w/a%20b"})),
            Some(PathBuf::from("/w/a b"))
        );
        assert_eq!(
            root_from_initialize(
                &json!({"rootUri": null, "workspaceFolders": [{"uri": "file:///w/x", "name": "x"}]})
            ),
            Some(PathBuf::from("/w/x"))
        );
        assert_eq!(
            root_from_initialize(&json!({"rootPath": "/w/y"})),
            Some(PathBuf::from("/w/y"))
        );
        assert_eq!(root_from_initialize(&json!({})), None);
    }

    #[test]
    fn project_roots_from_tsserver_requests() {
        let m = json!({"command": "updateOpen", "arguments": {"openFiles": [{"file": "/w/a.ts", "projectRootPath": "/w"}]}});
        assert_eq!(project_root_of(&m), Some(PathBuf::from("/w")));
        let m =
            json!({"command": "open", "arguments": {"file": "/w/a.ts", "projectRootPath": "/v"}});
        assert_eq!(project_root_of(&m), Some(PathBuf::from("/v")));
        assert_eq!(
            project_root_of(&json!({"command": "configure", "arguments": {}})),
            None
        );
    }

    #[test]
    fn replay_tracks_documents() {
        let mut r = Replay::default();
        r.observe(&jsonrpc::request(json!(1), "initialize", json!({})));
        r.observe(&jsonrpc::notification("initialized", json!({})));
        r.observe(&jsonrpc::notification(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": "file:///a.ts", "languageId": "typescript", "version": 1, "text": "ab"}}),
        ));
        r.observe(&jsonrpc::notification(
            "textDocument/didChange",
            json!({"textDocument": {"uri": "file:///a.ts", "version": 2},
                   "contentChanges": [{"range": {"start": {"line": 0, "character": 2}, "end": {"line": 0, "character": 2}}, "text": "c"}]}),
        ));
        assert_eq!(r.in_flight.len(), 1);
        let msgs = r.messages();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["id"], "tsr-reinit");
        assert_eq!(msgs[2]["params"]["textDocument"]["text"], "abc");
        assert_eq!(msgs[2]["params"]["textDocument"]["version"], 2);
        r.observe(&jsonrpc::notification(
            "textDocument/didClose",
            json!({"textDocument": {"uri": "file:///a.ts"}}),
        ));
        assert_eq!(r.messages().len(), 2);
    }

    #[test]
    fn settings_merge() {
        let m = merge_settings(
            &json!({"memoryBudgetMB": 1}),
            &json!({"memoryBudgetMB": 2, "lint": "off"}),
        );
        assert_eq!(m, json!({"memoryBudgetMB": 2, "lint": "off"}));
    }
}
