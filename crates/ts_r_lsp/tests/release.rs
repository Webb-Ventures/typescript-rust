//! Closing files gives their check results back (`tsr/project/release.rs`): after a quiet
//! time a project with open files restarts on a fresh checker, and a project without any
//! drops its program; both answer exactly as before when used again. Through a real LSP front
//! on the ts-r backend. Skipped when the lib files are missing.
#![cfg(unix)]

use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};
use ts_r_lsp::client::{Dialect, ScriptedClient, did_open_params, initialize_params};

ts_r_testing::cap_memory!();

const BIN: &str = env!("CARGO_BIN_EXE_ts-r-lsp");
const T: Duration = Duration::from_secs(20);

const TSCONFIG: &str = r#"{"compilerOptions": {"strict": true, "module": "nodenext", "target": "es2022", "noEmit": true}, "include": ["src"]}"#;
const A: &str = "export function len(s: string): number {\n  return s.length;\n}\nexport const bad: number = \"x\";\n";
const B: &str = "import { len } from \"./a.js\";\nexport const n: string = len(\"abc\");\n";
const C: &str = "export const c = 1;\n";

struct Env {
    dir: PathBuf,
    proj: PathBuf,
}

impl Env {
    fn new(name: &str) -> Env {
        let dir = PathBuf::from(format!("/tmp/tsr-release-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let proj = dir.join("proj");
        fs::create_dir_all(proj.join("src")).unwrap();
        fs::write(proj.join("tsconfig.json"), TSCONFIG).unwrap();
        fs::write(proj.join("src/a.ts"), A).unwrap();
        fs::write(proj.join("src/b.ts"), B).unwrap();
        fs::write(proj.join("src/c.ts"), C).unwrap();
        let proj = fs::canonicalize(&proj).unwrap();
        Env { dir, proj }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.proj)
            .env("TS_R_CACHE_DIR", self.dir.join("cache"))
            .env("TS_R_LSP_LINGER_MS", "300")
            .env("TS_R_LSP_TYPE_BACKEND", "ts-r")
            .env("TS_R_LSP_PARITY", "off")
            .env("TS_R_LSP_RELEASE_MS", "100")
            .env_remove("TS_R_LSP_TSSERVER_CMD")
            .env_remove("TS_R_LSP_NATIVE_CMD")
            .env_remove("TS_R_WORKSPACE_ROOT");
        c
    }

    fn uri(&self, name: &str) -> String {
        ts_r_lsp::uri::path_to_uri(&self.proj.join(name).to_string_lossy())
    }

    fn lsp(&self) -> ScriptedClient {
        let mut c =
            ScriptedClient::spawn(self.cmd(&["--name", "w1", "--lint", "off"]), Dialect::Lsp)
                .unwrap();
        let r = c
            .call("initialize", initialize_params(&self.proj), T)
            .expect("initialize answered");
        assert!(r.get("result").is_some(), "initialize failed: {r}");
        c.notify("initialized", json!({})).unwrap();
        c
    }

    fn open(&self, c: &mut ScriptedClient, name: &str) {
        let path = self.proj.join(name);
        let text = fs::read_to_string(&path).unwrap();
        c.notify("textDocument/didOpen", did_open_params(&path, &text))
            .unwrap();
    }

    fn close(&self, c: &mut ScriptedClient, name: &str) {
        c.notify(
            "textDocument/didClose",
            json!({"textDocument": {"uri": self.uri(name)}}),
        )
        .unwrap();
    }

    /// Waits until the daemon log has `n` lines containing `what`.
    fn wait_log(&self, what: &str, n: usize) -> bool {
        let deadline = Instant::now() + T;
        while Instant::now() < deadline {
            let count: usize = fs::read_dir(self.dir.join("cache/lsp"))
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
                .filter_map(|e| fs::read_to_string(e.path()).ok())
                .map(|t| t.lines().filter(|l| l.contains(what)).count())
                .sum();
            if count >= n {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self
            .cmd(&["stop", "--root", &self.proj.to_string_lossy()])
            .output();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn diags_for(c: &mut ScriptedClient, uri: &str, pred: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    let deadline = Instant::now() + T;
    while Instant::now() < deadline {
        let Some(m) = c.wait_for(deadline - Instant::now(), |m| {
            m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri
        }) else {
            break;
        };
        let d = m["params"]["diagnostics"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if pred(&d) {
            return d;
        }
    }
    panic!("no matching diagnostics for {uri}");
}

fn codes(d: &[Value]) -> Vec<u64> {
    d.iter().filter_map(|x| x["code"].as_u64()).collect()
}

fn hover(c: &mut ScriptedClient, uri: &str, line: u32, character: u32) -> String {
    let r = c
        .call(
            "textDocument/hover",
            json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": character}}),
            T,
        )
        .expect("hover answered");
    assert!(r.get("error").is_none(), "{r}");
    r["result"]["contents"]["value"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

#[test]
fn closed_files_are_released_and_recomputed_on_demand() {
    if ts_r_lsp::tsr::libs().is_none() {
        eprintln!("skipped: lib files missing (cargo xtask upstream sync --minimal)");
        return;
    }
    let env = Env::new("close");
    let mut c = env.lsp();
    let (ua, ub) = (env.uri("src/a.ts"), env.uri("src/b.ts"));
    env.open(&mut c, "src/a.ts");
    env.open(&mut c, "src/b.ts");
    env.open(&mut c, "src/c.ts");
    // tsc 7: a.ts(4,14) TS2322; b.ts(2,14) TS2322 (number to string).
    assert_eq!(codes(&diags_for(&mut c, &ua, |d| !d.is_empty())), [2322]);
    assert_eq!(codes(&diags_for(&mut c, &ub, |d| !d.is_empty())), [2322]);
    assert!(hover(&mut c, &ua, 0, 17).contains("function len"));

    // Closing b.ts and c.ts: the project restarts on a fresh checker; a.ts still answers.
    env.close(&mut c, "src/b.ts");
    env.close(&mut c, "src/c.ts");
    assert!(env.wait_log("released closed files' check results (1 open)", 1));
    assert!(hover(&mut c, &ua, 3, 14).contains("const bad: number"));
    // An edit after the release is checked as usual: fixing the error clears it.
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": ua, "version": 2},
               "contentChanges": [{"range": {"start": {"line": 3, "character": 27}, "end": {"line": 3, "character": 30}}, "text": "1"}]}),
    )
    .unwrap();
    assert!(diags_for(&mut c, &ua, |d| d.is_empty()).is_empty());

    // Closing the last file drops the program; reopening reloads it with the open edit gone
    // (a.ts is read from disk again) and the same diagnostics.
    env.close(&mut c, "src/a.ts");
    assert!(env.wait_log("no open files, program released", 1));
    env.open(&mut c, "src/b.ts");
    assert_eq!(codes(&diags_for(&mut c, &ub, |d| !d.is_empty())), [2322]);
    let h = hover(&mut c, &ub, 0, 10);
    assert!(h.contains("len(s: string): number"), "{h:?}");
    env.open(&mut c, "src/a.ts");
    assert_eq!(codes(&diags_for(&mut c, &ua, |d| !d.is_empty())), [2322]);
    c.shutdown();
}
