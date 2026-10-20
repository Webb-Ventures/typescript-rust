//! End-to-end tests against a real TypeScript install. Skipped unless pointed at one:
//!
//! ```sh
//! TS_R_LSP_TEST_TS6=/path/to/node_modules   # contains typescript@5 or @6 (tsserver)
//! TS_R_LSP_TEST_TS7=/path/to/node_modules   # contains typescript@7 and its platform package
//! cargo test -p ts_r_lsp --test real_typescript
//! ```
#![cfg(unix)]

use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};
use ts_r_lsp::client::{Dialect, ScriptedClient, did_open_params, initialize_params};

ts_r_testing::cap_memory!();

const BIN: &str = env!("CARGO_BIN_EXE_ts-r-lsp");
const T: Duration = Duration::from_secs(60);

struct Project {
    dir: PathBuf,
    proj: PathBuf,
}

impl Project {
    fn new(name: &str, node_modules: &str) -> Project {
        let dir = PathBuf::from(format!("/tmp/tsr-real-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let proj = dir.join("proj");
        fs::create_dir_all(&proj).unwrap();
        std::os::unix::fs::symlink(node_modules, proj.join("node_modules")).unwrap();
        fs::write(proj.join("tsconfig.json"), r#"{"compilerOptions":{"strict":true,"target":"es2022","module":"esnext"},"include":["*.ts"]}"#).unwrap();
        fs::write(proj.join("lib.ts"), "/** Adds numbers. */\nexport function add(a: number, b: number): number {\n  return a + b;\n}\n").unwrap();
        fs::write(proj.join("main.ts"), "import { add } from \"./lib\";\nconst total: number = add(1, 2);\nconst wrong: string = total;\nexport { wrong };\n").unwrap();
        let proj = fs::canonicalize(proj).unwrap();
        Project { dir, proj }
    }

    fn cmd(&self, name: &str) -> Command {
        let mut c = Command::new(BIN);
        c.args(["--name", name])
            .current_dir(&self.proj)
            .env("TS_R_CACHE_DIR", self.dir.join("cache"))
            .env("TS_R_LSP_TYPE_BACKEND", "typescript")
            .env_remove("TS_R_LSP_TSSERVER_CMD")
            .env_remove("TS_R_LSP_NATIVE_CMD");
        c
    }

    fn uri(&self, f: &str) -> String {
        ts_r_lsp::uri::path_to_uri(&self.proj.join(f).to_string_lossy())
    }

    fn window(&self, name: &str) -> ScriptedClient {
        let mut c = ScriptedClient::spawn(self.cmd(name), Dialect::Lsp).unwrap();
        let r = c
            .call("initialize", initialize_params(&self.proj), T)
            .unwrap();
        assert!(r.get("result").is_some(), "{r}");
        c.notify("initialized", json!({})).unwrap();
        let text = fs::read_to_string(self.proj.join("main.ts")).unwrap();
        c.notify(
            "textDocument/didOpen",
            did_open_params(&self.proj.join("main.ts"), &text),
        )
        .unwrap();
        c
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = Command::new(BIN)
            .args(["stop", "--root", &self.proj.to_string_lossy()])
            .env("TS_R_CACHE_DIR", self.dir.join("cache"))
            .output();
        std::thread::sleep(Duration::from_millis(200));
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Waits for diagnostics on main.ts, pulling them if the server prefers pull diagnostics.
fn diagnostics(c: &mut ScriptedClient, uri: &str) -> Vec<Value> {
    let deadline = Instant::now() + T;
    loop {
        let u = uri.to_string();
        if let Some(m) = c.wait_for(Duration::from_secs(2), |m| {
            m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == json!(u)
        }) {
            let d = m["params"]["diagnostics"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if !d.is_empty() {
                return d;
            }
        }
        if let Some(r) = c.call(
            "textDocument/diagnostic",
            json!({"textDocument": {"uri": uri}}),
            T,
        ) && let Some(items) = r["result"]["items"].as_array()
            && !items.is_empty()
        {
            return items.clone();
        }
        assert!(Instant::now() < deadline, "no diagnostics");
    }
}

fn exercise(p: &Project) {
    let main = p.uri("main.ts");
    let mut a = p.window("A");
    let d = diagnostics(&mut a, &main);
    assert!(d.iter().any(|d| d["code"] == 2322), "{d:?}");
    let mut b = p.window("B");
    let d = diagnostics(&mut b, &main);
    assert!(d.iter().any(|d| d["code"] == 2322), "{d:?}");
    let pos = json!({"textDocument": {"uri": main}, "position": {"line": 1, "character": 23}});
    let h = a.call("textDocument/hover", pos.clone(), T).unwrap();
    let text = h["result"]["contents"].to_string();
    assert!(text.contains("add(a: number, b: number): number"), "{h}");
    let def = b.call("textDocument/definition", pos.clone(), T).unwrap();
    assert!(def["result"].to_string().contains("lib.ts"), "{def}");
    let compl = a
        .call(
            "textDocument/completion",
            json!({"textDocument": {"uri": main}, "position": {"line": 1, "character": 22}}),
            T,
        )
        .unwrap();
    assert!(
        compl["result"].to_string().contains("\"add\""),
        "{}",
        compl["result"]
            .to_string()
            .chars()
            .take(300)
            .collect::<String>()
    );
    a.shutdown();
    b.shutdown();
}

#[test]
fn typescript_5_or_6_through_tsserver() {
    let Ok(nm) = std::env::var("TS_R_LSP_TEST_TS6") else {
        eprintln!("skipped: set TS_R_LSP_TEST_TS6 to a node_modules dir with typescript@5/6");
        return;
    };
    exercise(&Project::new("ts6", &nm));
}

#[test]
fn typescript_7_native_language_server() {
    let Ok(nm) = std::env::var("TS_R_LSP_TEST_TS7") else {
        eprintln!("skipped: set TS_R_LSP_TEST_TS7 to a node_modules dir with typescript@7");
        return;
    };
    exercise(&Project::new("ts7", &nm));
}

#[test]
fn tsserver_stand_in_shares_tsserver_with_an_lsp_window() {
    let Ok(nm) = std::env::var("TS_R_LSP_TEST_TS6") else {
        eprintln!("skipped: set TS_R_LSP_TEST_TS6");
        return;
    };
    let p = Project::new("standin", &nm);
    let main = p.proj.join("main.ts").to_string_lossy().into_owned();
    let mut vscode = ScriptedClient::spawn(
        {
            let mut c = p.cmd("VS Code");
            c.args([
                "--root",
                &p.proj.to_string_lossy(),
                "tsserver",
                "--useInferredProjectPerProjectRoot",
                "--cancellationPipeName",
                "/tmp/tsr-real-cancel*",
            ]);
            c
        },
        Dialect::TsServer,
    )
    .unwrap();
    vscode
        .call(
            "configure",
            json!({"hostInfo": "vscode", "preferences": {}}),
            T,
        )
        .unwrap();
    let text = fs::read_to_string(&main).unwrap();
    vscode
        .call(
            "updateOpen",
            json!({"openFiles": [{"file": main, "fileContent": text, "projectRootPath": p.proj}]}),
            T,
        )
        .unwrap();
    let seq = vscode
        .request("geterr", json!({"files": [main], "delay": 0}))
        .unwrap();
    let diag = vscode
        .wait_for(T, |m| m["event"] == "semanticDiag")
        .unwrap();
    assert!(
        diag["body"]["diagnostics"].to_string().contains("2322"),
        "{diag}"
    );
    let done = vscode
        .wait_for(T, |m| m["event"] == "requestCompleted")
        .unwrap();
    assert_eq!(done["body"]["request_seq"], seq);
    let qi = vscode
        .call(
            "quickinfo",
            json!({"file": main, "line": 2, "offset": 24}),
            T,
        )
        .unwrap();
    assert!(
        qi["body"]["displayString"]
            .as_str()
            .unwrap()
            .contains("add(a: number"),
        "{qi}"
    );
    // An LSP window joins the same tsserver and sees the same diagnostics.
    let mut w = p.window("LSP");
    let d = diagnostics(&mut w, &p.uri("main.ts"));
    assert!(d.iter().any(|d| d["code"] == 2322));
    let status = Command::new(BIN)
        .args(["status", "--root", &p.proj.to_string_lossy()])
        .env("TS_R_CACHE_DIR", p.dir.join("cache"))
        .output()
        .unwrap();
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["slots"].as_array().unwrap().len(), 1, "{status}");
    w.shutdown();
    vscode.shutdown();
}
