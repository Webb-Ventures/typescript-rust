//! End-to-end tests of fronts + daemon + backends, using the built-in fake backends
//! (`ts-r-lsp __fake-backend tsserver|lsp`) so they need neither node nor TypeScript.
//! Each test gets its own project dir and cache dir, so its own daemon.
#![cfg(unix)]

use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use ts_r_lsp::client::{Dialect, ScriptedClient, did_open_params, initialize_params};

const BIN: &str = env!("CARGO_BIN_EXE_ts-r-lsp");
/// How long to wait for an event. Generous: a loaded CI runner can take seconds to start a
/// daemon and its backend, and a passing test never waits this long.
const T: Duration = Duration::from_secs(30);

struct Env {
    dir: PathBuf,
    proj: PathBuf,
    extra: Vec<(String, String)>,
}

impl Env {
    fn new(name: &str, ts_major: u32) -> Env {
        // Short paths keep the Unix socket under the cache dir.
        let dir = PathBuf::from(format!("/tmp/tsr-it-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let proj = dir.join("proj");
        let ts = proj.join("node_modules/typescript");
        fs::create_dir_all(ts.join("lib")).unwrap();
        fs::write(
            ts.join("package.json"),
            format!(r#"{{"version":"{ts_major}.0.0"}}"#),
        )
        .unwrap();
        if ts_major >= 7 {
            let exe = proj
                .join("node_modules/@typescript")
                .join(ts_r_lsp::backend::typescript::platform_package())
                .join("lib/tsc");
            fs::create_dir_all(exe.parent().unwrap()).unwrap();
            fs::write(exe, "").unwrap();
        } else {
            fs::write(ts.join("lib/tsserver.js"), "").unwrap();
        }
        fs::write(proj.join("a.ts"), "const a = 1;\n").unwrap();
        fs::write(proj.join("b.ts"), "const b = 2;\n").unwrap();
        let proj = fs::canonicalize(&proj).unwrap();
        Env {
            dir,
            proj,
            extra: Vec::new(),
        }
    }

    fn with(mut self, k: &str, v: &str) -> Env {
        self.extra.push((k.into(), v.into()));
        self
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.proj)
            .env("TS_R_CACHE_DIR", self.dir.join("cache"))
            .env(
                "TS_R_LSP_TSSERVER_CMD",
                json!([BIN, "__fake-backend", "tsserver"]).to_string(),
            )
            .env(
                "TS_R_LSP_NATIVE_CMD",
                json!([BIN, "__fake-backend", "lsp"]).to_string(),
            )
            .env("TS_R_LSP_SAMPLE_MS", "150")
            .env("TS_R_LSP_LINGER_MS", "400")
            .env("TS_R_LSP_TYPE_BACKEND", "typescript")
            .env_remove("TS_R_WORKSPACE_ROOT");
        for (k, v) in &self.extra {
            c.env(k, v);
        }
        c
    }

    fn file(&self, name: &str) -> PathBuf {
        self.proj.join(name)
    }

    fn uri(&self, name: &str) -> String {
        ts_r_lsp::uri::path_to_uri(&self.file(name).to_string_lossy())
    }

    /// An initialized LSP window.
    fn lsp(&self, name: &str, init_options: Value) -> ScriptedClient {
        let mut c = ScriptedClient::spawn(self.cmd(&["--name", name]), Dialect::Lsp).unwrap();
        let mut params = initialize_params(&self.proj);
        params["initializationOptions"] = init_options;
        let r = c
            .call("initialize", params, T)
            .expect("initialize answered");
        assert!(r.get("result").is_some(), "initialize failed: {r}");
        c.notify("initialized", json!({})).unwrap();
        c
    }

    fn tsserver(&self, name: &str) -> ScriptedClient {
        let root = self.proj.to_string_lossy().into_owned();
        ScriptedClient::spawn(
            self.cmd(&["--name", name, "--root", &root, "tsserver"]),
            Dialect::TsServer,
        )
        .unwrap()
    }

    fn open(&self, c: &mut ScriptedClient, name: &str) {
        let text = fs::read_to_string(self.file(name)).unwrap();
        c.notify(
            "textDocument/didOpen",
            did_open_params(&self.file(name), &text),
        )
        .unwrap();
    }

    fn status(&self) -> Option<Value> {
        let out = self
            .cmd(&["status", "--root", &self.proj.to_string_lossy()])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        serde_json::from_slice(&out.stdout).ok()
    }

    fn wait_status(&self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + T;
        loop {
            if let Some(s) = self.status()
                && pred(&s)
            {
                return s;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {:?}",
                self.status()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self
            .cmd(&["stop", "--root", &self.proj.to_string_lossy()])
            .output();
        std::thread::sleep(Duration::from_millis(100));
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn hover(c: &mut ScriptedClient, uri: &str) -> String {
    let r = c
        .call(
            "textDocument/hover",
            json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 6}}),
            T,
        )
        .expect("hover answered");
    r["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("no hover: {r}"))
        .to_string()
}

/// `pid=<pid> text=<text>` -> (pid, text)
fn parse_fake(s: &str) -> (u32, String) {
    let s = s.trim_start_matches("```typescript\n");
    let s = s.split("\n```").next().unwrap();
    let (pid, text) = s.trim_start_matches("pid=").split_once(" text=").unwrap();
    (pid.parse().unwrap(), text.to_string())
}

fn diagnostics_for(c: &mut ScriptedClient, uri: &str, contains: &str) -> Value {
    c.wait_for(T, |m| {
        m["method"] == "textDocument/publishDiagnostics"
            && m["params"]["uri"] == json!(uri)
            && m["params"]["diagnostics"].to_string().contains(contains)
    })
    .unwrap_or_else(|| panic!("no diagnostics for {uri} containing {contains:?}"))
}

fn change(c: &mut ScriptedClient, uri: &str, version: i64, line: u32, ch: u32, text: &str) {
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri, "version": version},
               "contentChanges": [{"range": {"start": {"line": line, "character": ch}, "end": {"line": line, "character": ch}}, "text": text}]}),
    )
    .unwrap();
}

#[test]
fn two_lsp_windows_share_one_tsserver_backend() {
    let env = Env::new("share", 6);
    let mut a = env.lsp("A", json!({}));
    let mut b = env.lsp("B", json!({}));
    env.open(&mut a, "a.ts");
    env.open(&mut b, "a.ts");
    diagnostics_for(&mut a, &env.uri("a.ts"), "const a = 1;");
    diagnostics_for(&mut b, &env.uri("a.ts"), "const a = 1;");
    let (pid_a, text_a) = parse_fake(&hover(&mut a, &env.uri("a.ts")));
    let (pid_b, _) = parse_fake(&hover(&mut b, &env.uri("a.ts")));
    assert_eq!(pid_a, pid_b, "both windows must reach the same backend");
    assert_eq!(text_a, "const a = 1;\n");
    let s = env.status().unwrap();
    assert_eq!(s["mode"], "shared");
    assert_eq!(s["slots"].as_array().unwrap().len(), 1);
    assert_eq!(s["clients"].as_array().unwrap().len(), 2);
    assert_eq!(s["slots"][0]["openDocs"], 1);
    // Completion and definition go through the translation layer too.
    let r = a
        .call("textDocument/completion", json!({"textDocument": {"uri": env.uri("a.ts")}, "position": {"line": 0, "character": 6}}), T)
        .unwrap();
    assert_eq!(r["result"]["items"][0]["label"], "fakeCompletion");
    let r = a
        .call("textDocument/definition", json!({"textDocument": {"uri": env.uri("a.ts")}, "position": {"line": 0, "character": 6}}), T)
        .unwrap();
    assert_eq!(r["result"][0]["uri"], json!(env.uri("a.ts")));
    assert!(a.shutdown().unwrap().success());
    assert!(b.shutdown().unwrap().success());
}

#[test]
fn tsserver_stand_in_and_lsp_window_share_the_backend() {
    let env = Env::new("mixed", 6);
    let mut lsp = env.lsp("LSP", json!({}));
    let mut ts = env.tsserver("VS Code");
    let file = env.file("b.ts").to_string_lossy().into_owned();
    let r = ts
        .call(
            "updateOpen",
            json!({"openFiles": [{"file": file, "fileContent": "const b = 2;\n", "projectRootPath": env.proj}]}),
            T,
        )
        .unwrap();
    assert_eq!(r["success"], true);
    let r = ts
        .call(
            "quickinfo",
            json!({"file": file, "line": 1, "offset": 7}),
            T,
        )
        .unwrap();
    let (pid_ts, text) = parse_fake(r["body"]["displayString"].as_str().unwrap());
    assert_eq!(text, "const b = 2;\n");
    env.open(&mut lsp, "b.ts");
    let (pid_lsp, _) = parse_fake(&hover(&mut lsp, &env.uri("b.ts")));
    assert_eq!(pid_ts, pid_lsp);
    // geterr from the tsserver client: events are routed back with its own seq.
    let seq = ts
        .request("geterr", json!({"files": [file], "delay": 0}))
        .unwrap();
    ts.wait_for(T, |m| m["event"] == "semanticDiag")
        .expect("semanticDiag");
    let done = ts
        .wait_for(T, |m| m["event"] == "requestCompleted")
        .expect("requestCompleted");
    assert_eq!(done["body"]["request_seq"], seq);
    // Unknown commands are forwarded and answered with the client's seq.
    let r = ts.call("navtree", json!({"file": file}), T).unwrap();
    assert_eq!(r["success"], false);
    ts.shutdown();
    lsp.shutdown();
}

#[test]
fn clean_window_follows_the_window_with_unsaved_edits() {
    let env = Env::new("follow", 6);
    let mut a = env.lsp("A", json!({}));
    let mut b = env.lsp("B", json!({}));
    env.open(&mut a, "a.ts");
    env.open(&mut b, "a.ts");
    change(&mut b, &env.uri("a.ts"), 2, 0, 12, " // edited in B");
    // Windows are separate connections: order across them is not defined, so let B's own
    // request confirm its edit reached the daemon first.
    let _ = hover(&mut b, &env.uri("a.ts"));
    let (_, text) = parse_fake(&hover(&mut a, &env.uri("a.ts")));
    assert_eq!(text, "const a = 1; // edited in B\n");
    // B sees diagnostics computed on its text; still one backend.
    diagnostics_for(&mut b, &env.uri("a.ts"), "edited in B");
    assert_eq!(env.status().unwrap()["mode"], "shared");
    a.shutdown();
    b.shutdown();
}

#[test]
fn different_unsaved_edits_split_into_one_backend_per_window() {
    let env = Env::new("split", 6);
    let mut a = env.lsp("A", json!({}));
    let mut b = env.lsp("B", json!({}));
    env.open(&mut a, "a.ts");
    env.open(&mut a, "b.ts");
    env.open(&mut b, "a.ts");
    let _ = hover(&mut b, &env.uri("a.ts"));
    change(&mut a, &env.uri("a.ts"), 2, 0, 0, "/*A*/");
    let _ = hover(&mut a, &env.uri("a.ts"));
    change(&mut b, &env.uri("a.ts"), 2, 0, 0, "/*B*/");
    let s = env.wait_status("split", |s| s["mode"] == "per-client");
    assert_eq!(s["slots"].as_array().unwrap().len(), 2);
    assert_eq!(s["splits"], 1);
    let (pa, ta) = parse_fake(&hover(&mut a, &env.uri("a.ts")));
    let (pb, tb) = parse_fake(&hover(&mut b, &env.uri("a.ts")));
    assert_ne!(pa, pb, "each window has its own backend");
    assert_eq!(ta, "/*A*/const a = 1;\n");
    assert_eq!(tb, "/*B*/const a = 1;\n");
    // A's other document survived the split; B's backend does not have it.
    let (_, tb2) = parse_fake(&hover(&mut a, &env.uri("b.ts")));
    assert_eq!(tb2, "const b = 2;\n");
    // When B leaves, the workspace goes back to one shared backend.
    b.shutdown();
    env.wait_status("merge", |s| {
        s["mode"] == "shared" && s["slots"].as_array().unwrap().len() == 1
    });
    let mut c = env.lsp("C", json!({}));
    env.open(&mut c, "a.ts");
    let (pc, _) = parse_fake(&hover(&mut c, &env.uri("a.ts")));
    assert_eq!(pc, pa, "a new window joins the remaining backend");
    a.shutdown();
    c.shutdown();
}

#[test]
fn over_budget_backend_restarts_when_idle_and_replays_documents() {
    let env = Env::new("budget", 6)
        .with("TS_R_FAKE_ALLOC_MB", "96")
        .with("TS_R_LSP_RESTART_IDLE_MS", "300");
    let mut a = env.lsp("A", json!({"memoryBudgetMB": 64}));
    env.open(&mut a, "a.ts");
    change(&mut a, &env.uri("a.ts"), 2, 0, 0, "/*unsaved*/");
    let (pid1, _) = parse_fake(&hover(&mut a, &env.uri("a.ts")));
    let s = env.wait_status("budget restart", |s| {
        s["slots"][0]["restarts"]["budget"] == 1
    });
    assert_eq!(s["settings"]["memoryBudgetMB"], 64);
    // Only one restart per idle period, even though the new backend is over budget too.
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(env.status().unwrap()["slots"][0]["restarts"]["budget"], 1);
    let (pid2, text) = parse_fake(&hover(&mut a, &env.uri("a.ts")));
    assert_ne!(pid1, pid2);
    assert_eq!(text, "/*unsaved*/const a = 1;\n", "unsaved text replayed");
    a.shutdown();
}

#[test]
fn idle_shutdown_stops_backends_and_the_next_request_restarts_them() {
    let env = Env::new("idle", 6).with("TS_R_LSP_IDLE_SHUTDOWN_MS", "500");
    let mut a = env.lsp("A", json!({}));
    env.open(&mut a, "a.ts");
    let (pid1, _) = parse_fake(&hover(&mut a, &env.uri("a.ts")));
    env.wait_status("idle stop", |s| {
        s["slots"][0]["state"] == "Stopped" && s["slots"][0]["restarts"]["idleStops"] == 1
    });
    let (pid2, text) = parse_fake(&hover(&mut a, &env.uri("a.ts")));
    assert_ne!(pid1, pid2);
    assert_eq!(text, "const a = 1;\n");
    a.shutdown();
}

#[test]
fn daemon_exits_after_the_last_client_leaves() {
    let env = Env::new("linger", 6);
    let a = env.lsp("A", json!({}));
    assert!(env.status().is_some());
    a.shutdown();
    let deadline = Instant::now() + T;
    while env.status().is_some() {
        assert!(Instant::now() < deadline, "daemon did not exit");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn crashed_backend_is_restarted_with_documents() {
    let env = Env::new("crash", 6);
    let mut ts = env.tsserver("VS Code");
    let mut lsp = env.lsp("LSP", json!({}));
    env.open(&mut lsp, "a.ts");
    let (pid1, _) = parse_fake(&hover(&mut lsp, &env.uri("a.ts")));
    let r = ts
        .call("fake/crash", json!({}), T)
        .expect("failed response after crash");
    assert_eq!(r["success"], false);
    let (pid2, text) = parse_fake(&hover(&mut lsp, &env.uri("a.ts")));
    assert_ne!(pid1, pid2);
    assert_eq!(text, "const a = 1;\n");
    assert_eq!(env.status().unwrap()["slots"][0]["restarts"]["crash"], 1);
    ts.shutdown();
    lsp.shutdown();
}

#[test]
fn native_lsp_backend_is_shared_and_requests_are_multiplexed() {
    let env = Env::new("native", 7);
    let mut a = env.lsp("A", json!({}));
    let mut b = env.lsp("B", json!({}));
    env.open(&mut a, "a.ts");
    env.open(&mut b, "a.ts");
    // The two opens race (separate connections): whichever reaches the daemon second gets
    // the backend's last diagnostics, so both windows see them.
    diagnostics_for(&mut a, &env.uri("a.ts"), "const a = 1;");
    diagnostics_for(&mut b, &env.uri("a.ts"), "const a = 1;");
    let (pa, _) = parse_fake(&hover(&mut a, &env.uri("a.ts")));
    let (pb, _) = parse_fake(&hover(&mut b, &env.uri("a.ts")));
    assert_eq!(pa, pb);
    // The backend's dynamic registration reached both windows (B got a replay).
    for c in [&mut a, &mut b] {
        c.clear_backlog();
        assert!(
            c.server_requests
                .iter()
                .any(|r| r["method"] == "client/registerCapability"),
            "registration missing: {:?}",
            c.server_requests
        );
    }
    let s = env.status().unwrap();
    assert_eq!(s["slots"][0]["protocol"], "Lsp");
    // A tsserver client cannot use a TS 7 project without a fallback tsserver.
    let mut ts = env.tsserver("VS Code");
    let r = ts.call("configure", json!({}), Duration::from_secs(5));
    assert!(r.is_none_or(|r| r["success"] == true));
    ts.kill();
    a.shutdown();
    b.shutdown();
}

#[test]
fn a_window_opening_a_file_the_native_backend_has_gets_its_diagnostics() {
    let env = Env::new("native-late", 7);
    let mut a = env.lsp("A", json!({}));
    env.open(&mut a, "a.ts");
    diagnostics_for(&mut a, &env.uri("a.ts"), "const a = 1;");
    // B's open sends nothing to the backend (same text), so nothing new is published.
    let mut b = env.lsp("B", json!({}));
    env.open(&mut b, "a.ts");
    diagnostics_for(&mut b, &env.uri("a.ts"), "const a = 1;");
    a.shutdown();
    b.shutdown();
}

#[test]
fn native_lsp_backend_starts_with_the_editors_initialize_params() {
    let env = Env::new("init-params", 7);
    let mut c = ScriptedClient::spawn(env.cmd(&["--name", "A"]), Dialect::Lsp).unwrap();
    let mut params = initialize_params(&env.proj);
    params["initializationOptions"] = json!({"probe": 7});
    let r = c
        .call("initialize", params, T)
        .expect("initialize answered");
    assert_eq!(
        r["result"]["serverInfo"]["backend"]["initializationOptions"],
        json!({"probe": 7}),
        "{r}"
    );
    c.notify("initialized", json!({})).unwrap();
    c.shutdown();
}

#[test]
fn a_cancelled_request_id_can_be_reused() {
    let env = Env::new("cancel-reuse", 6);
    let mut a = env.lsp("A", json!({}));
    env.open(&mut a, "a.ts");
    change(&mut a, &env.uri("a.ts"), 2, 0, 0, "/*slow*/");
    let _ = hover(&mut a, &env.uri("a.ts")); // synced and ready
    let req = json!({"jsonrpc": "2.0", "id": 1000, "method": "textDocument/hover",
                     "params": {"textDocument": {"uri": env.uri("a.ts")}, "position": {"line": 0, "character": 6}}});
    a.send(&req).unwrap();
    a.notify("$/cancelRequest", json!({"id": 1000})).unwrap();
    let r = a.response(1000, T).expect("cancel answered");
    assert_eq!(r["error"]["code"], -32800, "{r}");
    // The slow answer arrives and is dropped; then the id is used again.
    std::thread::sleep(Duration::from_millis(700));
    a.send(&req).unwrap();
    let r = a.response(1000, T).expect("reused id answered");
    assert!(r["result"]["contents"]["value"].is_string(), "{r}");
    a.shutdown();
}

#[test]
fn lsp_front_reconnects_when_the_daemon_dies() {
    let env = Env::new("reconnect", 6);
    let mut a = env.lsp("A", json!({}));
    env.open(&mut a, "a.ts");
    change(&mut a, &env.uri("a.ts"), 2, 0, 0, "/*kept*/");
    let _ = hover(&mut a, &env.uri("a.ts"));
    let old = env.status().unwrap()["daemonPid"].clone();
    let _ = env
        .cmd(&["stop", "--root", &env.proj.to_string_lossy()])
        .output();
    // The front replays its documents after it reconnects: wait for them too.
    env.wait_status("new daemon with the replayed document", |s| {
        s["daemonPid"] != old
            && s["clients"].as_array().is_some_and(|c| c.len() == 1)
            && s["slots"][0]["openDocs"] == 1
    });
    let (_, text) = parse_fake(&hover(&mut a, &env.uri("a.ts")));
    assert_eq!(text, "/*kept*/const a = 1;\n");
    a.shutdown();
}

#[test]
fn closing_a_document_clears_its_diagnostics_and_backend_copy() {
    let env = Env::new("close", 6);
    let mut a = env.lsp("A", json!({}));
    let mut b = env.lsp("B", json!({}));
    env.open(&mut a, "a.ts");
    env.open(&mut b, "a.ts");
    diagnostics_for(&mut a, &env.uri("a.ts"), "const a");
    a.notify(
        "textDocument/didClose",
        json!({"textDocument": {"uri": env.uri("a.ts")}}),
    )
    .unwrap();
    let cleared = a
        .wait_for(T, |m| {
            m["method"] == "textDocument/publishDiagnostics"
                && m["params"]["diagnostics"] == json!([])
        })
        .expect("cleared");
    assert_eq!(cleared["params"]["uri"], json!(env.uri("a.ts")));
    let s = env.status().unwrap();
    assert_eq!(s["slots"][0]["openDocs"], 1, "B still has it open");
    assert_eq!(s["slots"][0]["diagnosticFiles"], 1, "{s}");
    b.notify(
        "textDocument/didClose",
        json!({"textDocument": {"uri": env.uri("a.ts")}}),
    )
    .unwrap();
    // Nobody has it open: its cached diagnostics go too.
    env.wait_status("closed", |s| {
        s["slots"][0]["openDocs"] == 0 && s["slots"][0]["diagnosticFiles"] == 0
    });
    a.shutdown();
    b.shutdown();
}

#[test]
fn unsupported_methods_and_bad_params_are_errors_not_crashes() {
    let env = Env::new("errors", 6);
    let mut a = env.lsp("A", json!({}));
    let r = a
        .call(
            "textDocument/codeLens",
            json!({"textDocument": {"uri": env.uri("a.ts")}}),
            T,
        )
        .unwrap();
    assert_eq!(r["error"]["code"], -32601);
    let r = a
        .call("textDocument/hover", json!({"bogus": true}), T)
        .unwrap();
    assert_eq!(r["error"]["code"], -32602);
    // Still alive.
    env.open(&mut a, "a.ts");
    let _ = hover(&mut a, &env.uri("a.ts"));
    a.shutdown();
}

#[test]
fn status_reports_no_daemon_for_unknown_roots() {
    let env = Env::new("nostatus", 6);
    let out = env
        .cmd(&["status", "--root", "/definitely/not/a/root"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let _ = Path::new(BIN);
}

#[test]
fn a_refused_front_gets_the_reason_at_once() {
    let env = Env::new("refused", 6);
    fs::remove_dir_all(env.proj.join("node_modules")).unwrap();
    let root = env.proj.to_string_lossy().into_owned();
    let started = Instant::now();
    let out = env
        .cmd(&["--root", &root, "tsserver"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no TypeScript found"), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "front retried a final refusal: {:?}",
        started.elapsed()
    );
}
