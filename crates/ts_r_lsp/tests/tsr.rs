//! End-to-end tests of the ts-r backend (our checker in the daemon): diagnostics, edits,
//! dependents, cancellation and language features, through a real LSP front. Expected
//! diagnostics come from tsc 7 (same codes and positions). Skipped when the lib files are
//! missing (`cargo xtask upstream sync --minimal`).
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

fn libs_present() -> bool {
    let ok = ts_r_lsp::tsr::libs().is_some();
    if !ok {
        eprintln!("skipped: lib files missing (cargo xtask upstream sync --minimal)");
    }
    ok
}

#[derive(Debug)]
pub struct Env {
    pub dir: PathBuf,
    pub proj: PathBuf,
    extra: Vec<(String, String)>,
}

impl Env {
    pub fn new(name: &str, files: &[(&str, &str)]) -> Env {
        let dir = PathBuf::from(format!("/tmp/tsr-l3-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let proj = dir.join("proj");
        fs::create_dir_all(&proj).unwrap();
        for (p, text) in files {
            let path = proj.join(p);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let proj = fs::canonicalize(&proj).unwrap();
        Env {
            dir,
            proj,
            extra: vec![("TS_R_LSP_PARITY".into(), "off".into())],
        }
    }

    pub fn with(mut self, k: &str, v: &str) -> Env {
        self.extra.retain(|(a, _)| a != k);
        self.extra.push((k.into(), v.into()));
        self
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.proj)
            .env("TS_R_CACHE_DIR", self.dir.join("cache"))
            .env("TS_R_LSP_LINGER_MS", "300")
            .env("TS_R_LSP_TYPE_BACKEND", "ts-r")
            .env_remove("TS_R_LSP_TSSERVER_CMD")
            .env_remove("TS_R_LSP_NATIVE_CMD")
            .env_remove("TS_R_WORKSPACE_ROOT");
        for (k, v) in &self.extra {
            c.env(k, v);
        }
        c
    }

    pub fn uri(&self, name: &str) -> String {
        ts_r_lsp::uri::path_to_uri(&self.proj.join(name).to_string_lossy())
    }

    pub fn lsp(&self, name: &str) -> ScriptedClient {
        self.lsp_lint(name, "off")
    }

    pub fn lsp_lint(&self, name: &str, lint: &str) -> ScriptedClient {
        self.lsp_lint_with(name, lint, &[])
    }

    /// An LSP client started with extra command-line arguments.
    pub fn lsp_with(&self, name: &str, args: &[&str]) -> ScriptedClient {
        self.lsp_lint_with(name, "off", args)
    }

    fn lsp_lint_with(&self, name: &str, lint: &str, args: &[&str]) -> ScriptedClient {
        let mut argv = vec!["--name", name, "--lint", lint];
        argv.extend_from_slice(args);
        let mut c = ScriptedClient::spawn(self.cmd(&argv), Dialect::Lsp).unwrap();
        let r = c
            .call("initialize", initialize_params(&self.proj), T)
            .expect("initialize answered");
        assert!(r.get("result").is_some(), "initialize failed: {r}");
        c.notify("initialized", json!({})).unwrap();
        c
    }

    pub fn open(&self, c: &mut ScriptedClient, name: &str) {
        let path = self.proj.join(name);
        let text = fs::read_to_string(&path).unwrap();
        c.notify("textDocument/didOpen", did_open_params(&path, &text))
            .unwrap();
    }

    pub fn status(&self) -> Option<Value> {
        let out = self
            .cmd(&["status", "--root", &self.proj.to_string_lossy()])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        serde_json::from_slice(&out.stdout).ok()
    }

    pub fn stop(&self) {
        let _ = self
            .cmd(&["stop", "--root", &self.proj.to_string_lossy()])
            .output();
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Waits for diagnostics of `uri` satisfying `pred`.
pub fn diags_for(
    c: &mut ScriptedClient,
    uri: &str,
    pred: impl Fn(&[Value]) -> bool,
) -> Option<Vec<Value>> {
    let deadline = Instant::now() + T;
    while Instant::now() < deadline {
        let m = c.wait_for(deadline - Instant::now(), |m| {
            m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri
        })?;
        let d = m["params"]["diagnostics"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if pred(&d) {
            return Some(d);
        }
    }
    None
}

fn codes(d: &[Value]) -> Vec<u64> {
    d.iter().filter_map(|x| x["code"].as_u64()).collect()
}

fn change(c: &mut ScriptedClient, uri: &str, version: i64, range: Value, text: &str) {
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri, "version": version},
               "contentChanges": [{"range": range, "text": text}]}),
    )
    .unwrap();
}

fn range(l1: u32, c1: u32, l2: u32, c2: u32) -> Value {
    json!({"start": {"line": l1, "character": c1}, "end": {"line": l2, "character": c2}})
}

const TSCONFIG: &str = r#"{"compilerOptions": {"strict": true, "module": "nodenext", "target": "es2022", "noEmit": true}, "include": ["src"]}"#;

#[test]
fn diagnostics_edits_and_dependents() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "diags",
        &[
            ("tsconfig.json", TSCONFIG),
            (
                "src/a.ts",
                "export function len(s: string): number {\n  return s.length;\n}\nexport const bad: number = \"x\";\n",
            ),
            (
                "src/b.ts",
                "import { len } from \"./a.js\";\nexport const n: number = len(\"abc\");\n",
            ),
        ],
    );
    let mut c = env.lsp("w1");
    let (ua, ub) = (env.uri("src/a.ts"), env.uri("src/b.ts"));
    env.open(&mut c, "src/a.ts");
    env.open(&mut c, "src/b.ts");
    // tsc 7: a.ts(4,14): error TS2322: Type 'string' is not assignable to type 'number'.
    let d = diags_for(&mut c, &ua, |d| !d.is_empty()).expect("a.ts diagnostics");
    assert_eq!(codes(&d), vec![2322], "{d:?}");
    assert_eq!(d[0]["range"]["start"], json!({"line": 3, "character": 13}));
    assert_eq!(d[0]["source"], "ts-r");
    assert_eq!(d[0]["data"]["kind"], "semantic");
    assert!(diags_for(&mut c, &ub, |d| d.is_empty()).is_some());

    // Fix the error: a.ts clears.
    change(&mut c, &ua, 2, range(3, 27, 3, 30), "1");
    assert!(diags_for(&mut c, &ua, |d| d.is_empty()).is_some());

    // A syntax error is pushed (syntax first), then cleared.
    change(&mut c, &ua, 3, range(1, 17, 1, 17), " +");
    let d = diags_for(&mut c, &ua, |d| !d.is_empty()).expect("syntax error");
    assert!(d.iter().any(|x| x["data"]["kind"] == "syntax"), "{d:?}");
    change(&mut c, &ua, 4, range(1, 17, 1, 19), "");
    assert!(diags_for(&mut c, &ua, |d| d.is_empty()).is_some());

    // Changing len's signature re-checks the open dependent b.ts.
    // tsc 7: b.ts(2,30): error TS2345: Argument of type 'string' is not assignable to
    // parameter of type 'number'.
    change(&mut c, &ua, 5, range(0, 23, 0, 29), "number");
    let d = diags_for(&mut c, &ub, |d| !d.is_empty()).expect("b.ts re-checked");
    assert_eq!(codes(&d), vec![2345], "{d:?}");
    assert_eq!(d[0]["range"]["start"], json!({"line": 1, "character": 29}));
    c.shutdown();
}

#[test]
fn type_aware_lint_runs_on_our_checker() {
    if !libs_present() {
        return;
    }
    let oxlint = r#"{"categories": {"correctness": "off"}, "rules": {"no-var": "error", "typescript/no-floating-promises": "error"}}"#;
    let env = Env::new(
        "typedlint",
        &[
            ("tsconfig.json", TSCONFIG),
            (".oxlintrc.json", oxlint),
            (
                "src/a.ts",
                "async function f(): Promise<number> {\n  return 1;\n}\nf();\nvar x = 1;\nexport { x };\n",
            ),
        ],
    );
    let mut c = env.lsp_lint("w1", "on");
    let ua = env.uri("src/a.ts");
    env.open(&mut c, "src/a.ts");
    let rules = |d: &[Value]| -> Vec<String> {
        let mut r: Vec<String> = d
            .iter()
            .filter(|x| x["source"] == "ts-r-lint")
            .filter_map(|x| x["code"].as_str().map(str::to_string))
            .collect();
        r.sort();
        r
    };
    // The untyped rule from the lint worker, the type-aware one from the ts-r backend.
    let d = diags_for(&mut c, &ua, |d| rules(d).len() == 2).expect("both lint diagnostics");
    assert_eq!(
        rules(&d),
        vec!["@typescript-eslint/no-floating-promises", "no-var"],
        "{d:?}"
    );
    let fp = d
        .iter()
        .find(|x| x["code"] == "@typescript-eslint/no-floating-promises")
        .unwrap();
    assert_eq!(fp["range"]["start"], json!({"line": 3, "character": 0}));
    // Its suggestions are quick fixes (from the ts-r backend; the lint worker adds no-var's).
    let r = result(
        &mut c,
        "textDocument/codeAction",
        json!({"textDocument": {"uri": ua}, "range": range(3, 0, 3, 3), "context": {"diagnostics": []}}),
    );
    let titles: Vec<&str> = r
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["title"].as_str())
        .collect();
    assert!(
        titles.contains(&"Add void operator to ignore."),
        "{titles:?}"
    );
    assert!(
        titles.contains(&"Disable @typescript-eslint/no-floating-promises for this line"),
        "{titles:?}"
    );
    // `void` marks the promise as handled: the type-aware diagnostic goes away.
    change(&mut c, &ua, 2, range(3, 0, 3, 0), "void ");
    let d = diags_for(&mut c, &ua, |d| rules(d) == ["no-var"]).expect("re-linted");
    assert!(codes(&d).is_empty(), "{d:?}");
    c.shutdown();
}

/// Type-aware fixes are offered only from results for the current text: after an edit, the
/// published spans are moved, but a fix's edits would still point at the old offsets.
#[test]
fn type_aware_fixes_wait_for_the_current_text() {
    if !libs_present() {
        return;
    }
    let oxlint = r#"{"categories": {"correctness": "off"}, "rules": {"typescript/no-unnecessary-type-assertion": "error"}}"#;
    let env = Env::new(
        "typedfix",
        &[
            ("tsconfig.json", TSCONFIG),
            (".oxlintrc.json", oxlint),
            ("src/a.ts", "const x: number = 1;\nx!;\nexport { x };\n"),
        ],
    );
    let mut c = env.lsp_lint("w1", "on");
    let ua = env.uri("src/a.ts");
    env.open(&mut c, "src/a.ts");
    let rule = "@typescript-eslint/no-unnecessary-type-assertion";
    let has_rule = |d: &[Value]| d.iter().any(|x| x["code"] == rule);
    diags_for(&mut c, &ua, has_rule).expect("type-aware diagnostic");
    // The rule's fix edits, as (start, end) ranges.
    let fix_ranges = |r: &Value| -> Vec<Value> {
        r.as_array()
            .into_iter()
            .flatten()
            .filter(|a| {
                a["title"]
                    .as_str()
                    .is_some_and(|t| t.starts_with("Fix this"))
            })
            .flat_map(|a| a["edit"]["changes"][ua.as_str()].as_array().cloned())
            .flatten()
            .map(|e| e["range"].clone())
            .collect()
    };
    let actions = |c: &mut ScriptedClient| {
        result(
            c,
            "textDocument/codeAction",
            json!({"textDocument": {"uri": ua}, "range": range(1, 0, 1, 3), "context": {"diagnostics": []}}),
        )
    };
    // An edit on the line above (byte offsets move, line 1 does not), and a request before
    // the file is checked again.
    change(&mut c, &ua, 2, range(0, 0, 0, 0), "/* c */ ");
    let stale = actions(&mut c);
    for r in fix_ranges(&stale) {
        assert_eq!(r, range(1, 1, 1, 2), "{stale}");
    }
    // Once checked again, the fix is offered at the current offsets.
    let deadline = Instant::now() + T;
    loop {
        let fresh = actions(&mut c);
        let ranges = fix_ranges(&fresh);
        if !ranges.is_empty() {
            assert_eq!(ranges, vec![range(1, 1, 1, 2)], "{fresh}");
            break;
        }
        assert!(Instant::now() < deadline, "no fix offered: {fresh}");
        std::thread::sleep(Duration::from_millis(50));
    }
    c.shutdown();
}

#[test]
fn rapid_edits_end_on_the_latest_text() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "rapid",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/a.ts", "export const x: number = 0;\n"),
        ],
    );
    let mut c = env.lsp("w1");
    let ua = env.uri("src/a.ts");
    env.open(&mut c, "src/a.ts");
    assert!(diags_for(&mut c, &ua, |d| d.is_empty()).is_some());
    // 50 keystrokes appending a string, ending in an error, without waiting.
    let mut col = 25;
    for (i, ch) in "\"abcdefghijklmnopqrstuvwxyz0123456789abcdefghijk\""
        .chars()
        .enumerate()
    {
        change(
            &mut c,
            &ua,
            2 + i as i64,
            range(0, col, 0, col),
            &ch.to_string(),
        );
        col += 1;
    }
    // Final text: `export const x: number = 0"abc...k";` is a syntax error; the last
    // published set must be for the final text (a semicolon is expected).
    let d = diags_for(&mut c, &ua, |d| d.iter().any(|x| x["code"] == 1005)).expect("final");
    assert!(!d.is_empty());
    let st = c
        .call("ts-r/status", json!({}), T)
        .expect("status answered");
    assert_eq!(st["result"]["backend"], "ts-r", "{st}");
    c.shutdown();
}

#[test]
fn inferred_project_and_close() {
    if !libs_present() {
        return;
    }
    let env = Env::new("inferred", &[("loose.ts", "let s: string = 1;\n")]);
    let mut c = env.lsp("w1");
    let u = env.uri("loose.ts");
    env.open(&mut c, "loose.ts");
    let d = diags_for(&mut c, &u, |d| !d.is_empty()).expect("diagnostics");
    assert_eq!(codes(&d), vec![2322]);
    c.notify("textDocument/didClose", json!({"textDocument": {"uri": u}}))
        .unwrap();
    assert!(diags_for(&mut c, &u, |d| d.is_empty()).is_some());
    c.shutdown();
}

#[test]
fn deleting_an_imported_file_rechecks_its_importers() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "deleted",
        &[
            ("a.ts", "import { b } from \"./b\";\nexport const a = b;\n"),
            ("b.ts", "export const b = 1;\n"),
        ],
    );
    let mut c = env.lsp("w1");
    let ua = env.uri("a.ts");
    env.open(&mut c, "a.ts");
    assert!(diags_for(&mut c, &ua, |d| d.is_empty()).is_some());
    fs::remove_file(env.proj.join("b.ts")).unwrap();
    c.notify(
        "workspace/didChangeWatchedFiles",
        json!({"changes": [{"uri": env.uri("b.ts"), "type": 3}]}),
    )
    .unwrap();
    // tsc: a.ts(1,19): error TS2307: Cannot find module './b' or its corresponding type
    // declarations.
    let d = diags_for(&mut c, &ua, |d| !d.is_empty()).expect("a.ts re-checked");
    assert_eq!(codes(&d), vec![2307], "{d:?}");
    c.shutdown();
}

const WATCH_A: &str = "import { v } from \"./b.js\";\nexport const n: number = v;\n";
const WATCH_B_NUMBER: &str = "export const v: number = 1;\n";
const WATCH_B_STRING: &str = "export const v: string = \"x\";\n";

/// Writes `text` to `path` until a.ts's diagnostics change (the first write can land before
/// the watch on its directory is armed), within a bounded time.
fn write_until_rechecked(
    c: &mut ScriptedClient,
    path: &std::path::Path,
    text: &str,
    uri: &str,
    pred: impl Fn(&[Value]) -> bool,
) -> Option<Vec<Value>> {
    let deadline = Instant::now() + T;
    while Instant::now() < deadline {
        fs::write(path, text).unwrap();
        let round = (Instant::now() + Duration::from_secs(2)).min(deadline);
        while let Some(m) = c.wait_for(round.saturating_duration_since(Instant::now()), |m| {
            m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri
        }) {
            let d = m["params"]["diagnostics"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if pred(&d) {
                return Some(d);
            }
        }
    }
    None
}

#[test]
fn an_unopened_import_changed_on_disk_reaches_the_checker() {
    if !libs_present() {
        return;
    }
    // The reviewer's case: a.ts is open, b.ts is not, and b.ts's export changes from a number
    // to a string on disk. No client sends `workspace/didChangeWatchedFiles` (VS Code's
    // bridge registers no watchers): the daemon watches the project's inputs itself.
    let env = Env::new(
        "watch",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/a.ts", WATCH_A),
            ("src/b.ts", WATCH_B_NUMBER),
        ],
    );
    let mut c = env.lsp("w1");
    let ua = env.uri("src/a.ts");
    env.open(&mut c, "src/a.ts");
    assert!(diags_for(&mut c, &ua, |d| d.is_empty()).is_some());
    assert!(hover_text(&mut c, &ua, 1, 25).contains("const v: number"));
    // tsc: src/a.ts(2,14): error TS2322: Type 'string' is not assignable to type 'number'.
    let d = write_until_rechecked(
        &mut c,
        &env.proj.join("src/b.ts"),
        WATCH_B_STRING,
        &ua,
        |d| !d.is_empty(),
    )
    .expect("a.ts re-checked after b.ts changed on disk");
    assert_eq!(codes(&d), vec![2322], "{d:?}");
    assert_eq!(d[0]["range"]["start"], json!({"line": 1, "character": 13}));
    let h = hover_text(&mut c, &ua, 1, 25);
    assert!(h.contains("const v: string"), "{h}");
    c.shutdown();
}

#[test]
fn files_created_in_a_new_directory_are_seen() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "watch-dir",
        &[
            ("tsconfig.json", TSCONFIG),
            (
                "src/a.ts",
                "import { d } from \"./new/d.js\";\nexport const n: number = d;\n",
            ),
        ],
    );
    let mut c = env.lsp("w1");
    let ua = env.uri("src/a.ts");
    env.open(&mut c, "src/a.ts");
    // tsc: src/a.ts(1,19): error TS2307: Cannot find module './new/d.js' or its corresponding
    // type declarations.
    let d = diags_for(&mut c, &ua, |d| !d.is_empty()).expect("diagnostics");
    assert_eq!(codes(&d), vec![2307], "{d:?}");
    // The directory first, then (after its creation was seen) the file in it.
    fs::create_dir(env.proj.join("src/new")).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let d = write_until_rechecked(
        &mut c,
        &env.proj.join("src/new/d.ts"),
        "export const d: string = \"x\";\n",
        &ua,
        |d| codes(d) != [2307],
    )
    .expect("a.ts re-checked after src/new/d.ts appeared");
    // tsc: src/a.ts(2,14): error TS2322: Type 'string' is not assignable to type 'number'.
    assert_eq!(codes(&d), vec![2322], "{d:?}");
    c.shutdown();
}

#[test]
fn reload_projects_reads_closed_inputs_again() {
    if !libs_present() {
        return;
    }
    // Without the watcher, the editor's "Reload Projects" (`ts-r/reloadProjects`) is what
    // makes the backend read b.ts again.
    let env = Env::new(
        "reload",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/a.ts", WATCH_A),
            ("src/b.ts", WATCH_B_NUMBER),
        ],
    )
    .with("TS_R_LSP_WATCH", "off");
    let mut c = env.lsp("w1");
    let ua = env.uri("src/a.ts");
    env.open(&mut c, "src/a.ts");
    assert!(diags_for(&mut c, &ua, |d| d.is_empty()).is_some());
    fs::write(env.proj.join("src/b.ts"), WATCH_B_STRING).unwrap();
    c.notify("ts-r/reloadProjects", json!({})).unwrap();
    let d = diags_for(&mut c, &ua, |d| !d.is_empty()).expect("a.ts re-checked");
    assert_eq!(codes(&d), vec![2322], "{d:?}");
    assert!(hover_text(&mut c, &ua, 1, 25).contains("const v: string"));
    c.shutdown();
}

#[test]
fn a_mirrored_document_from_another_engine_is_read_but_not_owned() {
    if !libs_present() {
        return;
    }
    // The bridge serves `y/` with TypeScript and `x/` with ts-r; x/a.ts imports y/b.ts, which
    // has an unsaved change from number to string in the editor.
    let cfg = r#"{"compilerOptions": {"strict": true, "module": "nodenext", "target": "es2022", "noEmit": true}}"#;
    let env = Env::new(
        "mirror",
        &[
            ("x/tsconfig.json", cfg),
            (
                "x/a.ts",
                "import { v } from \"../y/b.js\";\nexport const n: number = v;\n",
            ),
            ("y/tsconfig.json", cfg),
            ("y/b.ts", WATCH_B_NUMBER),
            ("y/c.ts", "export const c = 1;\n"),
        ],
    );
    let mut c = env.lsp("w1");
    let (ua, ub, uc) = (env.uri("x/a.ts"), env.uri("y/b.ts"), env.uri("y/c.ts"));
    env.open(&mut c, "x/a.ts");
    assert!(diags_for(&mut c, &ua, |d| d.is_empty()).is_some());
    // x's program reads y/b.ts: `ts-r/usedBy` says so (and nothing uses y/c.ts).
    let x_config = env
        .proj
        .join("x/tsconfig.json")
        .to_string_lossy()
        .into_owned();
    let deadline = Instant::now() + T;
    let used = loop {
        let used = result(&mut c, "ts-r/usedBy", json!({"uris": [ub, uc]}));
        if used.get(&ub).is_some() || Instant::now() > deadline {
            break used;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(used, json!({ub.clone(): [x_config]}), "{used}");
    c.notify(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": ub, "languageId": "ts-r-mirror", "version": 1, "text": WATCH_B_STRING}}),
    )
    .unwrap();
    // tsc: x/a.ts(2,14): error TS2322: Type 'string' is not assignable to type 'number'.
    let d = diags_for(&mut c, &ua, |d| !d.is_empty()).expect("a.ts sees b.ts's open text");
    assert_eq!(codes(&d), vec![2322], "{d:?}");
    assert!(hover_text(&mut c, &ua, 1, 25).contains("const v: string"));
    // Edits reach x too.
    change(&mut c, &ub, 2, range(0, 16, 0, 22), "number");
    change(&mut c, &ub, 3, range(0, 25, 0, 28), "1");
    assert!(diags_for(&mut c, &ua, |d| d.is_empty()).is_some());
    // The mirror is not y's open file: y never started, b.ts got no diagnostics.
    let st = result(&mut c, "ts-r/status", json!({}));
    let projects: Vec<&str> = st["projects"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["project"].as_str())
        .collect();
    assert_eq!(projects, vec!["x/tsconfig.json"], "{st}");
    assert!(
        c.wait_for(Duration::from_millis(300), |m| {
            m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == ub
        })
        .is_none()
    );
    // Back to string, then closed without saving: x reads b.ts from disk again (number).
    change(&mut c, &ub, 4, range(0, 16, 0, 22), "string");
    assert!(diags_for(&mut c, &ua, |d| codes(d) == [2322]).is_some());
    c.notify(
        "textDocument/didClose",
        json!({"textDocument": {"uri": ub}}),
    )
    .unwrap();
    assert!(diags_for(&mut c, &ua, |d| d.is_empty()).is_some());
    c.shutdown();
}

#[test]
fn inputs_changed_follows_a_new_import() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "inputs-changed",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/a.ts", "export const n = 1;\n"),
            ("other/b.ts", WATCH_B_NUMBER),
        ],
    );
    let mut c = env.lsp("w1");
    let ua = env.uri("src/a.ts");
    env.open(&mut c, "src/a.ts");
    assert!(diags_for(&mut c, &ua, |d| d.is_empty()).is_some());
    // The first load's file set.
    assert!(
        c.wait_for(T, |m| m["method"] == "ts-r/inputsChanged")
            .is_some()
    );
    // Importing a file outside the project's roots changes its file set.
    change(
        &mut c,
        &ua,
        2,
        range(0, 0, 0, 0),
        "import { v } from \"../other/b.js\";\nexport const w = v;\n",
    );
    let m = c
        .wait_for(T, |m| m["method"] == "ts-r/inputsChanged")
        .expect("ts-r/inputsChanged");
    assert_eq!(
        m["params"]["project"],
        json!(env.proj.join("tsconfig.json").to_string_lossy())
    );
    let used = result(
        &mut c,
        "ts-r/usedBy",
        json!({"uris": [env.uri("other/b.ts")]}),
    );
    assert!(used.get(env.uri("other/b.ts")).is_some(), "{used}");
    c.shutdown();
}

#[test]
fn memory_budget_covers_the_in_process_backend() {
    if !libs_present() {
        return;
    }
    // Enough checked code to put the daemon over the minimum budget.
    let mut big = String::from("let s: string = 1;\n");
    for i in 0..40_000 {
        big.push_str(&format!(
            "export const v{i}: {{ a: number; b: string }} = {{ a: {i}, b: \"{i}\" }};\n"
        ));
    }
    let env = Env::new("budget", &[("loose.ts", &big)])
        .with("TS_R_LSP_SAMPLE_MS", "100")
        .with("TS_R_LSP_RESTART_IDLE_MS", "300");
    let mut c = env.lsp_with("w1", &["--memory-budget-mb", "64"]);
    let u = env.uri("loose.ts");
    env.open(&mut c, "loose.ts");
    let d = diags_for(&mut c, &u, |d| !d.is_empty()).expect("diagnostics");
    assert_eq!(codes(&d), vec![2322]);
    // A restart clears the slot's RSS until the next sample, so wait for both.
    let deadline = Instant::now() + T;
    loop {
        let s = env.status();
        if let Some(s) = &s
            && s["slots"][0]["restarts"]["budget"] == 1
            && s["slots"][0]["rssMB"].as_u64().is_some()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no budget restart with a sampled RSS: {s:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // The document was replayed into the new backend: an incremental edit applies to it.
    change(&mut c, &u, 2, range(0, 16, 0, 17), "\"1\"");
    assert!(diags_for(&mut c, &u, |d| d.is_empty()).is_some());
    c.shutdown();
}

#[test]
fn auto_imports_in_es_modules_name_the_extension() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "esm-import",
        &[
            ("tsconfig.json", TSCONFIG),
            ("package.json", r#"{"type": "module"}"#),
            ("src/b.ts", "export const origin = 1;\n"),
            ("src/a.ts", "ori\n"),
        ],
    );
    let mut c = env.lsp("w1");
    let ua = env.uri("src/a.ts");
    env.open(&mut c, "src/a.ts");
    assert!(diags_for(&mut c, &ua, |d| !d.is_empty()).is_some());
    let r = result(&mut c, "textDocument/completion", at(&ua, 0, 3));
    let origin = r["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == "origin" && i["sortText"] == "16")
        .cloned()
        .expect("auto-import of origin");
    // No import in the file shows a style; the module format requires `.js` (TS2835).
    assert_eq!(origin["detail"], "Auto import from './b.js'", "{origin}");
    assert_eq!(
        origin["additionalTextEdits"][0]["newText"],
        "import { origin } from \"./b.js\";\n"
    );
    c.shutdown();
}

const NAV_A: &str = "/** Length of a string. */\nexport function len(s: string): number {\n  return s.length;\n}\nexport interface Point { x: number; y?: number }\nexport const origin: Point = { x: 0 };\n";
const NAV_B: &str = "import { len, origin, type Point } from \"./a.js\";\nconst n = len(\"abc\");\nconst p: Point = origin;\nconst m = p.x + n;\nexport { m };\n";

fn nav_env(name: &str) -> (Env, ScriptedClient) {
    let env = Env::new(
        name,
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/a.ts", NAV_A),
            ("src/b.ts", NAV_B),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/a.ts");
    env.open(&mut c, "src/b.ts");
    let ub = env.uri("src/b.ts");
    assert!(diags_for(&mut c, &ub, |d| d.is_empty()).is_some());
    (env, c)
}

fn at(uri: &str, line: u32, character: u32) -> Value {
    json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": character}})
}

fn result(c: &mut ScriptedClient, method: &str, params: Value) -> Value {
    let r = c.call(method, params, T).expect("answered");
    assert!(r.get("error").is_none(), "{method}: {r}");
    r["result"].clone()
}

fn hover_text(c: &mut ScriptedClient, uri: &str, line: u32, ch: u32) -> String {
    let r = result(c, "textDocument/hover", at(uri, line, ch));
    r["contents"]["value"].as_str().unwrap_or("").to_string()
}

#[test]
fn hover_shows_types_and_docs() {
    if !libs_present() {
        return;
    }
    let (env, mut c) = nav_env("hover");
    let ub = env.uri("src/b.ts");
    let h = hover_text(&mut c, &ub, 1, 11);
    assert!(h.contains("(alias) function len(s: string): number"), "{h}");
    assert!(!h.contains("import len"), "{h}");
    assert!(h.contains("Length of a string."), "{h}");
    let h = hover_text(&mut c, &ub, 3, 12);
    assert!(h.contains("(property) Point.x: number"), "{h}");
    let h = hover_text(&mut c, &ub, 1, 6);
    assert!(h.contains("const n: number"), "{h}");
    let h = hover_text(&mut c, &ub, 2, 6);
    assert!(h.contains("const p: Point"), "{h}");
    let ua = env.uri("src/a.ts");
    let h = hover_text(&mut c, &ua, 4, 18);
    assert!(h.contains("interface Point"), "{h}");
    let h = hover_text(&mut c, &ua, 1, 20);
    assert!(h.contains("(parameter) s: string"), "{h}");
    c.shutdown();
}

/// Type alias hovers print the aliased type (`Exclude<..>` shows the union it makes); a
/// JSDoc `@typedef`/`@callback` is spelled out, also where a tag's type names it; a generic
/// call that does not resolve shows its instantiated candidate.
#[test]
fn hover_on_type_aliases_and_jsdoc_typedefs() {
    if !libs_present() {
        return;
    }
    let ts = "type A = \"a\" | \"b\";\ntype B = Exclude<A, \"a\">;\ntype O = { x: number };\n\
              class D<T> { constructor(val: T) {} }\nnew D<string>();\n\
              class S { set v(x: string | number) {} get v(): string { return \"\"; } }\n\
              new S().v = new S().v;\n";
    let js = "/**\n * Bar doc\n * @typedef {Object} Bar\n * @property {string} baz\n */\n\
              /** @callback Cb\n * @param {string} x\n */\n\
              /** @type {Bar} */\nexport const bar = { baz: \"\" };\n";
    let env = Env::new(
        "hover-aliases",
        &[
            (
                "tsconfig.json",
                r#"{"compilerOptions": {"allowJs": true, "checkJs": true, "strict": true, "module": "nodenext", "target": "es2022", "noEmit": true}, "include": ["src"]}"#,
            ),
            ("src/a.ts", ts),
            ("src/b.js", js),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/a.ts");
    env.open(&mut c, "src/b.js");
    let ua = env.uri("src/a.ts");
    let ub = env.uri("src/b.js");
    let h = hover_text(&mut c, &ua, 1, 5);
    assert!(h.contains("type B = \"b\""), "{h}");
    let h = hover_text(&mut c, &ua, 2, 5);
    assert!(h.contains("type O = {\n    x: number;\n}"), "{h}");
    // A call that does not resolve shows its candidate with the written type arguments.
    let h = hover_text(&mut c, &ua, 4, 4);
    assert!(
        h.contains("constructor D<string>(val: string): D<string>"),
        "{h}"
    );
    // A setter's name and a write through it show the type it accepts; a read the getter's.
    for (line, ch, want) in [
        (5, 15, "string | number"),
        (6, 8, "string | number"),
        (6, 20, "C.v: string\n"),
    ] {
        let h = hover_text(&mut c, &ua, line, ch);
        assert!(
            h.contains(want) || h.contains(&want.replace("C.", "S.")),
            "{line}:{ch} {h}"
        );
    }
    let want = "type Bar = {\n    baz: string;\n}";
    let h = hover_text(&mut c, &ub, 2, 22);
    assert!(h.contains(want), "{h}");
    assert!(h.contains("```\nBar doc"), "{h:?}");
    assert!(!h.contains("property"), "{h:?}");
    // The name in `@type {Bar}` (a tag type in a comment, outside the statement's span).
    let h = hover_text(&mut c, &ub, 8, 12);
    assert!(h.contains(want), "{h}");
    assert!(h.contains("```\nBar doc"), "{h:?}");
    assert!(!h.contains("property"), "{h:?}");
    let h = hover_text(&mut c, &ub, 5, 15);
    assert!(h.contains("type Cb = (x: string) => any"), "{h}");
    c.shutdown();
}

const NAV_KEYWORDS: &str = "class Base { m() {} }
class D extends Base {
  override m() { super.m(); }
}
const make = () => new D();
let xs: D[] = [];
interface I {}
";

#[test]
fn keyword_definitions_and_type_definitions() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "navkw",
        &[("tsconfig.json", TSCONFIG), ("src/k.ts", NAV_KEYWORDS)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/k.ts");
    let u = env.uri("src/k.ts");
    assert!(diags_for(&mut c, &u, |d| d.is_empty()).is_some());
    let start = |v: &Value, i: usize| v[i]["range"]["start"].clone();
    // `override` goes to the base class's member, `super` to the base class.
    let d = result(&mut c, "textDocument/definition", at(&u, 2, 4));
    assert_eq!(start(&d, 0), json!({"line": 0, "character": 13}), "{d}");
    let d = result(&mut c, "textDocument/definition", at(&u, 2, 18));
    assert_eq!(start(&d, 0), json!({"line": 0, "character": 6}), "{d}");
    // A function's own type stands for its return type.
    let d = result(&mut c, "textDocument/typeDefinition", at(&u, 4, 7));
    assert_eq!(d.as_array().map(Vec::len), Some(1), "{d}");
    assert_eq!(start(&d, 0), json!({"line": 1, "character": 6}), "{d}");
    // `D[]` leads to `D` as well as to `Array`.
    let d = result(&mut c, "textDocument/typeDefinition", at(&u, 5, 5));
    let here: Vec<&Value> = d
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["uri"] == u.as_str())
        .collect();
    assert_eq!(here.len(), 1, "{d}");
    assert_eq!(
        here[0]["range"]["start"],
        json!({"line": 1, "character": 6})
    );
    assert!(d.as_array().unwrap().len() > 1, "{d}");
    // A type-only symbol goes to itself.
    let d = result(&mut c, "textDocument/typeDefinition", at(&u, 6, 10));
    assert_eq!(start(&d, 0), json!({"line": 6, "character": 10}), "{d}");
    // References on a declaration keyword are those of the declared name ...
    let r = result(
        &mut c,
        "textDocument/references",
        json!({"textDocument": {"uri": u}, "position": {"line": 1, "character": 1},
               "context": {"includeDeclaration": true}}),
    );
    let lines: Vec<u64> = r
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["range"]["start"]["line"].as_u64().unwrap())
        .collect();
    assert_eq!(lines, vec![1, 4, 5], "{r}");
    // ... but highlights on `const` are none (tsserver does not move to the name there).
    let h = result(&mut c, "textDocument/documentHighlight", at(&u, 4, 1));
    assert!(h.as_array().is_none_or(Vec::is_empty), "{h}");
    c.shutdown();
}

const NAV_DESTRUCTURE: &str = "interface I { p: number; q: string }
declare const o: I, xs: I[];
let p: number, z: number;
({ p: z } = o);
for ({ p } of xs) {}
for (const { p: w } of xs) {}
const a: I = { p: 1, q: '' };
const u = { p: 2 };
";

#[test]
fn references_through_destructuring_and_contextual_literals() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "navds",
        &[("tsconfig.json", TSCONFIG), ("src/d.ts", NAV_DESTRUCTURE)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/d.ts");
    let u = env.uri("src/d.ts");
    let _ = diags_for(&mut c, &u, |_| true);
    let refs = |c: &mut ScriptedClient, line: u32, ch: u32| -> Vec<(u64, u64)> {
        let r = result(
            c,
            "textDocument/references",
            json!({"textDocument": {"uri": u}, "position": {"line": line, "character": ch},
                   "context": {"includeDeclaration": true}}),
        );
        r.as_array()
            .unwrap()
            .iter()
            .map(|l| {
                let s = &l["range"]["start"];
                (
                    s["line"].as_u64().unwrap(),
                    s["character"].as_u64().unwrap(),
                )
            })
            .collect()
    };
    // `I.p`: the assignment-target keys, the for-of keys and the contextually typed
    // literal, but not the untyped literal or the variable `p`.
    let want = vec![(0, 14), (3, 3), (4, 7), (5, 13), (6, 15)];
    assert_eq!(refs(&mut c, 0, 14), want);
    // From an assignment-target key and from the typed literal: the same set.
    assert_eq!(refs(&mut c, 3, 3), want);
    assert_eq!(refs(&mut c, 6, 15), want);
    c.shutdown();
}

const NAV_LITERAL_KEYS: &str = "class K { \"s\": number = 1; get \"42\"() { return 0 } }
declare const k: K;
k.s; k[\"s\"]; k[42];
const lit = { 0: true };
lit[0];
";

#[test]
fn references_and_rename_at_literal_keys() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "navlk",
        &[("tsconfig.json", TSCONFIG), ("src/l.ts", NAV_LITERAL_KEYS)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/l.ts");
    let u = env.uri("src/l.ts");
    let _ = diags_for(&mut c, &u, |_| true);
    let refs = |c: &mut ScriptedClient, line: u32, ch: u32| -> Vec<(u64, u64)> {
        let r = result(
            c,
            "textDocument/references",
            json!({"textDocument": {"uri": u}, "position": {"line": line, "character": ch},
                   "context": {"includeDeclaration": true}}),
        );
        r.as_array()
            .unwrap()
            .iter()
            .map(|l| {
                let s = &l["range"]["start"];
                (
                    s["line"].as_u64().unwrap(),
                    s["character"].as_u64().unwrap(),
                )
            })
            .collect()
    };
    // A quoted member name, its dotted use and its quoted index are one property, from
    // any of them.
    let s = vec![(0, 11), (2, 2), (2, 8)];
    assert_eq!(refs(&mut c, 2, 2), s);
    assert_eq!(refs(&mut c, 0, 11), s);
    assert_eq!(refs(&mut c, 2, 8), s);
    // A numeric key, from the accessor's `get` keyword and from the index.
    assert_eq!(refs(&mut c, 0, 28), vec![(0, 32), (2, 15)]);
    assert_eq!(refs(&mut c, 2, 15), vec![(0, 32), (2, 15)]);
    // Renaming a numeric key quotes the index but not the key.
    let e = result(
        &mut c,
        "textDocument/rename",
        json!({"textDocument": {"uri": u}, "position": {"line": 4, "character": 4}, "newName": "z"}),
    );
    let edits: Vec<(u64, String)> = e["changes"][u.as_str()]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["range"]["start"]["line"].as_u64().unwrap(),
                t["newText"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        edits,
        vec![(3, "z".to_string()), (4, "\"z\"".to_string())],
        "{e}"
    );
    c.shutdown();
}

#[test]
fn definition_type_definition_references_rename() {
    if !libs_present() {
        return;
    }
    let (env, mut c) = nav_env("nav");
    let (ua, ub) = (env.uri("src/a.ts"), env.uri("src/b.ts"));
    // Definition through the import.
    let d = result(&mut c, "textDocument/definition", at(&ub, 1, 11));
    assert_eq!(d[0]["uri"], ua.as_str(), "{d}");
    assert_eq!(d[0]["range"]["start"], json!({"line": 1, "character": 16}));
    // Module specifier -> the file.
    let d = result(&mut c, "textDocument/definition", at(&ub, 0, 45));
    assert_eq!(d[0]["uri"], ua.as_str(), "{d}");
    // Type definition of `p` is `Point`.
    let d = result(&mut c, "textDocument/typeDefinition", at(&ub, 3, 10));
    assert_eq!(d[0]["uri"], ua.as_str(), "{d}");
    assert_eq!(d[0]["range"]["start"], json!({"line": 4, "character": 17}));
    // References of `len`: declaration, import, call.
    let r = result(
        &mut c,
        "textDocument/references",
        json!({"textDocument": {"uri": ub}, "position": {"line": 1, "character": 11},
               "context": {"includeDeclaration": true}}),
    );
    let got: Vec<(String, u64)> = r
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["uri"].as_str().unwrap().to_string(),
                l["range"]["start"]["line"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![(ua.clone(), 1), (ub.clone(), 0), (ub.clone(), 1)],
        "{r}"
    );
    // References of the property `x`: its declaration and the use.
    let r = result(
        &mut c,
        "textDocument/references",
        json!({"textDocument": {"uri": ub}, "position": {"line": 3, "character": 12},
               "context": {"includeDeclaration": true}}),
    );
    assert!(r.as_array().unwrap().len() >= 2, "{r}");
    // Highlights in one file.
    let h = result(&mut c, "textDocument/documentHighlight", at(&ub, 2, 7));
    assert_eq!(h.as_array().unwrap().len(), 2, "{h}");
    // Rename.
    let p = result(&mut c, "textDocument/prepareRename", at(&ub, 1, 11));
    assert_eq!(p["placeholder"], "len");
    let e = result(
        &mut c,
        "textDocument/rename",
        json!({"textDocument": {"uri": ub}, "position": {"line": 1, "character": 11}, "newName": "size"}),
    );
    // At a use of a name-keeping import only the alias is renamed (as tsc 7 does): the
    // specifier becomes `len as size`, the declaration in `a.ts` stays.
    assert!(e["changes"].get(&ua).is_none(), "{e}");
    let edits = e["changes"][&ub].as_array().unwrap();
    assert_eq!(edits.len(), 2, "{e}");
    assert_eq!(edits[0]["newText"], "len as size", "{e}");
    // Lib symbols cannot be renamed (`length` of `s.length`).
    let r = c
        .call("textDocument/prepareRename", at(&ua, 2, 13), T)
        .unwrap();
    assert!(r.get("error").is_some(), "{r}");
    c.shutdown();
}

#[test]
fn file_renames_and_refactors() {
    if !libs_present() {
        return;
    }
    let (env, mut c) = nav_env("filerename");
    let (ua, ub) = (env.uri("src/a.ts"), env.uri("src/b.ts"));
    // Moving a.ts updates b.ts's import (keeping its `.js` extension).
    let e = result(
        &mut c,
        "workspace/willRenameFiles",
        json!({"files": [{"oldUri": ua, "newUri": env.uri("src/lib/a.ts")}]}),
    );
    let edits = e["changes"][&ub].as_array().expect("b.ts edited");
    assert_eq!(edits.len(), 1, "{e}");
    assert_eq!(edits[0]["newText"], "./lib/a.js", "{e}");
    assert_eq!(
        edits[0]["range"]["start"],
        json!({"line": 0, "character": 41})
    );
    // A renamed directory moves the files under it (together: no import changes); the
    // tsconfig's `include` follows it.
    let e = result(
        &mut c,
        "workspace/willRenameFiles",
        json!({"files": [{"oldUri": env.uri("src"), "newUri": env.uri("source")}]}),
    );
    let changes = e["changes"].as_object().unwrap();
    assert_eq!(changes.len(), 1, "{e}");
    let cfg = changes[&env.uri("tsconfig.json")].as_array().unwrap();
    assert_eq!(cfg[0]["newText"], "\"source\"", "{e}");
    // Refactors at a cursor: move a declaration to a new file.
    let r = result(
        &mut c,
        "textDocument/codeAction",
        json!({"textDocument": {"uri": ub}, "range": range(1, 6, 1, 6), "context": {"diagnostics": []}}),
    );
    let titles: Vec<&str> = r
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["title"].as_str())
        .collect();
    assert!(titles.contains(&"Move to a new file"), "{titles:?}");
    // A selected expression can be extracted.
    let r = result(
        &mut c,
        "textDocument/codeAction",
        json!({"textDocument": {"uri": ub}, "range": range(3, 10, 3, 17), "context": {"diagnostics": [], "only": ["refactor.extract"]}}),
    );
    let titles: Vec<&str> = r
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["title"].as_str())
        .collect();
    assert_eq!(
        titles,
        vec![
            "Extract to function in module scope",
            "Extract to constant in enclosing scope"
        ],
        "{r}"
    );
    c.shutdown();
}

#[test]
fn rename_edits_string_literal_property_accesses() {
    if !libs_present() {
        return;
    }
    let src = "export class C { foo = 1 }\nconst c = new C();\nexport const v = c[\"foo\"] + c.foo;\nc[\"foo\"] = 2;\nexport const w = ({ foo: 1 })[\"foo\"];\n";
    let env = Env::new(
        "rename-literal",
        &[("tsconfig.json", TSCONFIG), ("src/c.ts", src)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/c.ts");
    let u = env.uri("src/c.ts");
    assert!(diags_for(&mut c, &u, |d| d.is_empty()).is_some());
    let e = result(
        &mut c,
        "textDocument/rename",
        json!({"textDocument": {"uri": u}, "position": {"line": 0, "character": 17}, "newName": "bar"}),
    );
    let mut got: Vec<(u64, u64, u64)> = e["changes"][&u]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| {
            let s = &x["range"]["start"];
            let end = &x["range"]["end"];
            assert_eq!(x["newText"], "bar", "{e}");
            (
                s["line"].as_u64().unwrap(),
                s["character"].as_u64().unwrap(),
                end["character"].as_u64().unwrap(),
            )
        })
        .collect();
    got.sort();
    // The declaration, `c["foo"]` (inside the quotes), `c.foo` and the write; not the
    // unrelated object literal's property.
    assert_eq!(
        got,
        vec![(0, 17, 20), (2, 20, 23), (2, 30, 33), (3, 3, 6)],
        "{e}"
    );
    c.shutdown();
}

#[test]
fn outline_and_semantic_tokens() {
    if !libs_present() {
        return;
    }
    let (env, mut c) = nav_env("outline");
    let ua = env.uri("src/a.ts");
    let s = result(
        &mut c,
        "textDocument/documentSymbol",
        json!({"textDocument": {"uri": ua}}),
    );
    let names: Vec<(&str, u64)> = s
        .as_array()
        .unwrap()
        .iter()
        .map(|x| (x["name"].as_str().unwrap(), x["kind"].as_u64().unwrap()))
        .collect();
    assert_eq!(
        names,
        vec![("len", 12), ("Point", 11), ("origin", 14)],
        "{s}"
    );
    assert_eq!(s[1]["children"].as_array().unwrap().len(), 2);
    let t = result(
        &mut c,
        "textDocument/semanticTokens/full",
        json!({"textDocument": {"uri": ua}}),
    );
    let data = t["data"].as_array().unwrap();
    assert_eq!(data.len() % 5, 0);
    // First token: `len` (function, declaration) at 1:16.
    assert_eq!(
        &data[..5],
        &[json!(1), json!(16), json!(3), json!(10), json!(1)],
        "{t}"
    );
    c.shutdown();
}

#[test]
fn completions_and_signature_help() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "complete",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/a.ts", NAV_A),
            (
                "src/c.ts",
                "import { len } from \"./a.js\";\nconst p = { x: 1, y: \"s\" };\n",
            ),
        ],
    );
    let mut c = env.lsp("w1");
    let uc = env.uri("src/c.ts");
    let text = "import { len } from \"./a.js\";\nconst p = { x: 1, y: \"s\" };\np.;\nori\nlen(\n";
    c.notify(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uc, "languageId": "typescript", "version": 1, "text": text}}),
    )
    .unwrap();
    assert!(diags_for(&mut c, &uc, |d| !d.is_empty()).is_some());
    let labels = |r: &Value| -> Vec<String> {
        r["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["label"].as_str().unwrap().to_string())
            .collect()
    };
    // Members after `p.`.
    let r = result(&mut c, "textDocument/completion", at(&uc, 2, 2));
    assert_eq!(labels(&r), vec!["x", "y"], "{r}");
    assert_eq!(r["items"][0]["kind"], 10);
    // Scope names, globals, keywords, and an auto-import for `ori`.
    let r = result(&mut c, "textDocument/completion", at(&uc, 3, 3));
    let l = labels(&r);
    for want in ["len", "p", "Array", "const", "origin"] {
        assert!(
            l.iter().any(|x| x == want),
            "{want} missing: {:?}",
            &l[..l.len().min(30)]
        );
    }
    let origin = r["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == "origin" && i["sortText"] == "16")
        .unwrap()
        .clone();
    assert_eq!(origin["detail"], "Auto import from './a.js'");
    let edit = &origin["additionalTextEdits"][0];
    assert_eq!(edit["newText"], ", origin", "{edit}");
    assert_eq!(edit["range"]["start"], json!({"line": 0, "character": 12}));
    // Resolve adds the detail line and docs.
    let len_item = r["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == "len")
        .unwrap()
        .clone();
    let resolved = result(&mut c, "completionItem/resolve", len_item);
    let detail = resolved["detail"].as_str().unwrap_or("");
    assert!(detail.contains("len(s: string): number"), "{resolved}");
    assert_eq!(resolved["documentation"]["value"], "Length of a string.");
    // Signature help inside `len(`.
    let s = result(&mut c, "textDocument/signatureHelp", at(&uc, 4, 4));
    assert_eq!(s["signatures"][0]["label"], "len(s: string): number", "{s}");
    assert_eq!(s["signatures"][0]["parameters"][0]["label"], json!([4, 13]));
    assert_eq!(s["activeParameter"], 0);
    assert_eq!(
        s["signatures"][0]["documentation"]["value"],
        "Length of a string."
    );
    c.shutdown();
}

/// Function values list `Function`'s members; non-arrow function bodies see `arguments`;
/// declaration names get no list.
#[test]
fn completion_function_members_and_arguments() {
    if !libs_present() {
        return;
    }
    let text =
        "function f(a: number) {\n  const g = () => a;\n  g;\n}\nf.;\nconst h = () => 1;\nh;\n";
    let env = Env::new(
        "complete-fn",
        &[("tsconfig.json", TSCONFIG), ("src/c.ts", text)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/c.ts");
    let uc = env.uri("src/c.ts");
    let labels = |r: &Value| -> Vec<String> {
        r["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["label"].as_str().unwrap().to_string())
            .collect()
    };
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 4, 2)));
    for want in ["apply", "bind", "call", "length", "name", "toString"] {
        assert!(l.iter().any(|x| x == want), "{want} missing: {l:?}");
    }
    // Inside `f` (also through the arrow `g`), but not at the top level.
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 2, 3)));
    assert!(l.iter().any(|x| x == "arguments"), "{l:?}");
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 6, 1)));
    assert!(!l.iter().any(|x| x == "arguments"), "{l:?}");
    // Names being declared get no list.
    for (line, ch) in [(0, 10), (0, 12), (1, 9)] {
        let l = labels(&result(
            &mut c,
            "textDocument/completion",
            at(&uc, line, ch),
        ));
        assert!(l.is_empty(), "{line}:{ch} {l:?}");
    }
    c.shutdown();
}

/// After `f<` with a generic `f` (or `new X<` whatever `X` is) the list is types, as tsc
/// reads a possible type argument list; a plain `a < ` comparison stays a value position.
#[test]
fn completion_in_possible_type_arguments() {
    if !libs_present() {
        return;
    }
    let text = "interface Shape { w: number }\nconst v = 1;\nfunction f<T>(x?: T) {}\n\
                class Bar {}\nf<\nfunction g() { new Bar< }\nconst c = v < \n";
    let env = Env::new(
        "complete-typeargs",
        &[("tsconfig.json", TSCONFIG), ("src/c.ts", text)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/c.ts");
    let uc = env.uri("src/c.ts");
    let labels = |r: &Value| -> Vec<String> {
        r["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|i| i["label"].as_str().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    for (line, ch) in [(4, 2), (5, 23)] {
        let l = labels(&result(
            &mut c,
            "textDocument/completion",
            at(&uc, line, ch),
        ));
        assert!(l.iter().any(|x| x == "Shape"), "{line}:{ch} {l:?}");
        assert!(!l.iter().any(|x| x == "v"), "{line}:{ch} {l:?}");
    }
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 6, 14)));
    assert!(l.iter().any(|x| x == "v"), "{l:?}");
    assert!(!l.iter().any(|x| x == "Shape"), "{l:?}");
    c.shutdown();
}

/// Where a type goes, a namespace is offered only when a type is somewhere below it, and
/// `m.|` in a type lists the types and such namespaces, not values.
#[test]
fn completion_of_namespaces_in_type_positions() {
    if !libs_present() {
        return;
    }
    let text = "namespace Vals { export const v = 1; }\n\
                namespace Outer { export namespace Inner { export interface I {} } export const w = 1; }\n\
                let a: ;\nlet b: Outer.;\nlet c = ;\n";
    let env = Env::new(
        "complete-ns-types",
        &[("tsconfig.json", TSCONFIG), ("src/c.ts", text)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/c.ts");
    let uc = env.uri("src/c.ts");
    let labels = |r: &Value| -> Vec<String> {
        r["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|i| i["label"].as_str().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 2, 7)));
    assert!(l.iter().any(|x| x == "Outer"), "{l:?}");
    assert!(!l.iter().any(|x| x == "Vals"), "{l:?}");
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 3, 13)));
    assert_eq!(l, vec!["Inner".to_string()]);
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 4, 8)));
    assert!(l.iter().any(|x| x == "Vals"), "{l:?}");
    c.shutdown();
}

/// `import { | } from "m"` lists the module's exports not named yet (quoted ones with a
/// local name), then `type`; `ns.|` of a module in an expression leaves out its types.
#[test]
fn completion_in_import_and_export_clauses() {
    if !libs_present() {
        return;
    }
    let m = "export const a = 1;\nexport function b() {}\nexport type T = 1;\n\
             const c = 2;\nexport { c as \"c d\" };\nexport default 3;\n";
    let u = "import { a,  } from \"./m.js\";\nexport {  } from \"./m.js\";\n\
             import * as ns from \"./m.js\";\nns.;\n";
    let env = Env::new(
        "complete-import-clause",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/m.ts", m),
            ("src/u.ts", u),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/u.ts");
    let uu = env.uri("src/u.ts");
    let items = |r: &Value| -> Vec<(String, String)> {
        r["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|i| {
                        let l = i["label"].as_str().unwrap_or("").to_string();
                        let t = i["insertText"].as_str().unwrap_or(&l).to_string();
                        (l, t)
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let l = items(&result(&mut c, "textDocument/completion", at(&uu, 0, 12)));
    let labels: Vec<&str> = l.iter().map(|(x, _)| x.as_str()).collect();
    assert_eq!(labels, ["T", "b", "c d", "type"], "{l:?}");
    assert!(
        l.contains(&("c d".into(), "\"c d\" as c_d".into())),
        "{l:?}"
    );
    let l = items(&result(&mut c, "textDocument/completion", at(&uu, 1, 9)));
    assert!(l.contains(&("c d".into(), "\"c d\"".into())), "{l:?}");
    assert!(l.iter().any(|(x, _)| x == "a"), "{l:?}");
    let l = items(&result(&mut c, "textDocument/completion", at(&uu, 3, 3)));
    assert!(l.iter().any(|(x, _)| x == "b"), "{l:?}");
    assert!(!l.iter().any(|(x, _)| x == "T"), "{l:?}");
    c.shutdown();
}

/// In a JSDoc comment: tag names after `@`, `@tag`s where a tag may start, the documented
/// function's parameters not yet documented at a `@param` name, and types (or a namespace's
/// types) inside a tag's braces; nothing in plain comment text.
#[test]
fn completion_in_jsdoc_comments() {
    if !libs_present() {
        return;
    }
    let text = "/**\n * @par\n * \n * @param b\n * @param \n * @returns {Sh}\n * @type {NS.}\n * some text \n */\n\
                function f(a: number, b: string, c: Shape) {}\n\
                interface Shape {}\n\
                namespace NS { export interface Inner {} export const v = 1; }\n";
    let env = Env::new(
        "complete-jsdoc",
        &[("tsconfig.json", TSCONFIG), ("src/c.ts", text)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/c.ts");
    let uc = env.uri("src/c.ts");
    let labels = |r: &Value| -> Vec<String> {
        r["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|i| i["sortText"].as_str().unwrap_or("") < "15")
                    .map(|i| i["label"].as_str().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 1, 7)));
    assert!(l.iter().any(|x| x == "param") && l.len() == 84, "{l:?}");
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 2, 3)));
    assert!(l.iter().any(|x| x == "@returns"), "{l:?}");
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 4, 10)));
    assert_eq!(l, ["a", "c"]);
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 5, 15)));
    assert!(l.iter().any(|x| x == "Shape"), "{l:?}");
    assert!(!l.iter().any(|x| x == "f" || x == "a"), "{l:?}");
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 6, 13)));
    assert_eq!(l, ["Inner"]);
    let r = result(&mut c, "textDocument/completion", at(&uc, 7, 13));
    assert!(r.is_null(), "{r}");
    c.shutdown();
}

/// An object literal in a generic call offers what the declared parameter allows beyond
/// what was inferred from it (`T` replaced by its constraint), also in nested literals; a
/// context made only of the literal's own members offers the names in scope.
#[test]
fn completion_in_object_literals_of_generic_calls() {
    if !libs_present() {
        return;
    }
    let text = "interface Opts { hello?: boolean; world?: boolean; deep?: { another?: boolean } }\n\
                declare function bar<T extends Opts>(o?: Partial<T>): void;\n\
                bar({ hello: true,  });\n\
                bar({ deep: {  } });\n\
                declare function f7<T>(o: T): void;\n\
                const local = 1;\n\
                f7({ l });\n";
    let env = Env::new(
        "complete-generic-literal",
        &[("tsconfig.json", TSCONFIG), ("src/c.ts", text)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/c.ts");
    let uc = env.uri("src/c.ts");
    let labels = |r: &Value| -> Vec<String> {
        r["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|i| i["sortText"].as_str().unwrap_or("") < "15")
                    .map(|i| i["label"].as_str().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 2, 18)));
    assert_eq!(l, ["deep?", "world?"]);
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 3, 14)));
    assert_eq!(l, ["another?"]);
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 6, 6)));
    assert!(l.iter().any(|x| x == "local"), "{l:?}");
    c.shutdown();
}

/// A promise's awaited members in an async function (written `(await p).m`), `type` after
/// `import` and `from` after a clause with no module, and `import("m").A.|` in types.
#[test]
fn completion_awaited_members_import_keywords_and_import_types() {
    if !libs_present() {
        return;
    }
    let m = "export namespace NS { export interface I {} export const v = 1; }\n";
    let text = "async function f(p: Promise<string>) {\n  p.\n}\n\
                type A = import(\"./m.js\").NS.;\n\
                type B = typeof import(\"./m.js\").NS.;\n\
                import {} \n";
    let bodies = "async function g(p: Promise<string>, ok: boolean) {\n  if (ok) p.\n}\n\
                  async function h(p: Promise<string>) {\n  const x = 1\n  p.\n}\n";
    let env = Env::new(
        "complete-await-import",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/m.ts", m),
            ("src/c.ts", text),
            ("src/d.ts", bodies),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/c.ts");
    let uc = env.uri("src/c.ts");
    let r = result(&mut c, "textDocument/completion", at(&uc, 1, 4));
    let items = r["items"].as_array().cloned().unwrap_or_default();
    assert!(items.iter().any(|i| i["label"] == "then"), "{r}");
    let trim = items.iter().find(|i| i["label"] == "trim").expect("trim");
    assert_eq!(trim["insertText"], "(await p).trim");
    // The body of an `if` gets no `;` (it would end the body); a statement after one
    // without a `;` does.
    env.open(&mut c, "src/d.ts");
    let ud = env.uri("src/d.ts");
    for (line, ch, want) in [(1, 12, "(await p).trim"), (5, 4, ";(await p).trim")] {
        let r = result(&mut c, "textDocument/completion", at(&ud, line, ch));
        let items = r["items"].as_array().cloned().unwrap_or_default();
        let trim = items.iter().find(|i| i["label"] == "trim").expect("trim");
        assert_eq!(trim["insertText"], want, "{line}:{ch}");
    }
    let labels = |r: &Value| -> Vec<String> {
        r["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|i| i["label"].as_str().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 3, 29)));
    assert_eq!(l, ["I"]);
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 4, 36)));
    assert_eq!(l, ["v"]);
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 5, 10)));
    assert_eq!(l, ["from"]);
    c.shutdown();
}

/// Review fixes for import clauses and type positions: a `}` in a comment does not close the
/// clause, an unterminated quoted name before the cursor does not panic, and modules that
/// re-export each other as namespaces are searched once each for a type.
#[test]
fn completion_import_clause_edges_and_namespace_cycles() {
    if !libs_present() {
        return;
    }
    let m = "export const a = 1;\nexport const b = 2;\n";
    let u = "import { a /* } */,  } from \"./m.js\";\n";
    let bad = "import { \"\u{e9}\n, } from \"./m.js\";\n";
    // Each of x and y exports four namespace aliases of the other: 4^17 paths, 2 modules.
    let x = "export * as y1 from \"./y.js\";\nexport * as y2 from \"./y.js\";\n\
             export * as y3 from \"./y.js\";\nexport * as y4 from \"./y.js\";\n";
    let y = "export * as x1 from \"./x.js\";\nexport * as x2 from \"./x.js\";\n\
             export * as x3 from \"./x.js\";\nexport * as x4 from \"./x.js\";\n";
    let t = "import * as nx from \"./x.js\";\nlet v: ;\n";
    let env = Env::new(
        "complete-clause-edges",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/m.ts", m),
            ("src/u.ts", u),
            ("src/bad.ts", bad),
            ("src/x.ts", x),
            ("src/y.ts", y),
            ("src/t.ts", t),
        ],
    );
    let mut c = env.lsp("w1");
    for f in ["src/u.ts", "src/bad.ts", "src/t.ts"] {
        env.open(&mut c, f);
    }
    let labels = |r: &Value| -> Vec<String> {
        r["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|i| i["label"].as_str().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    let l = labels(&result(
        &mut c,
        "textDocument/completion",
        at(&env.uri("src/u.ts"), 0, 20),
    ));
    assert_eq!(l, ["b", "type"], "{l:?}");
    // After the comma on the line below the unterminated `"\u{e9}`.
    let r = result(
        &mut c,
        "textDocument/completion",
        at(&env.uri("src/bad.ts"), 1, 2),
    );
    assert!(r.is_null() || r["items"].is_array(), "{r}");
    let started = std::time::Instant::now();
    let l = labels(&result(
        &mut c,
        "textDocument/completion",
        at(&env.uri("src/t.ts"), 1, 7),
    ));
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert!(!l.iter().any(|x| x == "nx"), "{l:?}");
    c.shutdown();
}

/// Class bodies: the inherited members not written yet (and member keywords) where a member
/// name goes, `this` members among the names in a method, and private or protected members
/// only where they may be read.
#[test]
fn completion_class_members() {
    if !libs_present() {
        return;
    }
    let text = "class B { m() {} protected p = 1; private q = 2; static s() {} }\n\
                class C extends B {\n  \n  foo() { this.p; }\n}\nnew B().;\n\
                export class E { static g(x: E) {} }\n";
    let env = Env::new(
        "complete-class",
        &[("tsconfig.json", TSCONFIG), ("src/c.ts", text)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/c.ts");
    let uc = env.uri("src/c.ts");
    let items = |r: &Value| -> Vec<(String, String)> {
        r["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|i| {
                        let label = i["label"].as_str().unwrap_or_default().to_string();
                        (
                            label,
                            i["sortText"].as_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let has = |l: &[(String, String)], n: &str| l.iter().any(|(x, _)| x == n);
    // A member name in `C`'s body: `B`'s public and protected members and the keywords, no
    // names in scope.
    let l = items(&result(&mut c, "textDocument/completion", at(&uc, 2, 2)));
    for want in ["m", "p", "constructor", "static"] {
        assert!(has(&l, want), "{want} missing: {l:?}");
    }
    for not in ["q", "s", "B", "foo"] {
        assert!(!has(&l, not), "{not} offered: {l:?}");
    }
    // `this.` inside the derived class sees the protected member, not the private one.
    let l = items(&result(&mut c, "textDocument/completion", at(&uc, 3, 15)));
    assert!(has(&l, "p") && has(&l, "m") && has(&l, "foo"), "{l:?}");
    assert!(!has(&l, "q"), "{l:?}");
    // Statements in a method offer the `this` members (sorted after locals).
    let l = items(&result(&mut c, "textDocument/completion", at(&uc, 3, 10)));
    assert!(l.iter().any(|(x, s)| x == "foo" && s == "14"), "{l:?}");
    // Outside the classes only public members.
    let l = items(&result(&mut c, "textDocument/completion", at(&uc, 5, 8)));
    assert!(has(&l, "m") && !has(&l, "p") && !has(&l, "q"), "{l:?}");
    // An exported class is a type in scope inside itself.
    let l = items(&result(&mut c, "textDocument/completion", at(&uc, 6, 30)));
    assert!(has(&l, "E"), "{l:?}");
    c.shutdown();
}

/// JSX: attribute names of the tag's props (optional ones as `name?`), the tag to close after
/// `</`, intrinsic element names for a tag name, and no list in JSX text.
#[test]
fn completion_in_jsx() {
    if !libs_present() {
        return;
    }
    let tsconfig = r#"{"compilerOptions": {"strict": true, "jsx": "preserve", "noEmit": true}, "include": ["src"]}"#;
    let text = "declare namespace JSX {\n  interface Element {}\n  interface IntrinsicElements { div: { id?: string; hidden: boolean } }\n}\n\
                function Foo(props: { size: number; label?: string }) { return null; }\n\
                const a = <div ></div>;\n\
                const b = <Foo size={1} ></Foo>;\n\
                const c = <div>text </;\n\
                const d = <d />;\n\
                const e = <div> some text </div>;\n";
    let env = Env::new(
        "complete-jsx",
        &[("tsconfig.json", tsconfig), ("src/c.tsx", text)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/c.tsx");
    let uc = env.uri("src/c.tsx");
    let labels = |r: &Value| -> Vec<(String, String)> {
        r["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|i| {
                        let label = i["label"].as_str().unwrap_or_default().to_string();
                        (
                            label,
                            i["sortText"].as_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let names = |l: &[(String, String)]| l.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
    // Attribute names of an intrinsic element.
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 5, 15)));
    assert_eq!(names(&l), ["hidden", "id?"], "{l:?}");
    // A component's props, minus the attributes already written.
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 6, 24)));
    assert_eq!(names(&l), ["label?"], "{l:?}");
    // The tag to close.
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 7, 22)));
    assert_eq!(names(&l), ["div>"], "{l:?}");
    // A tag name: intrinsic elements first, then the names in scope.
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 8, 12)));
    assert!(l.iter().any(|(n, s)| n == "div" && s == "11"), "{l:?}");
    assert!(l.iter().any(|(n, _)| n == "Foo"), "{l:?}");
    // JSX text: no list.
    let r = result(&mut c, "textDocument/completion", at(&uc, 9, 20));
    assert!(labels(&r).is_empty(), "{r:?}");
    c.shutdown();
}

/// Inside a string literal the list comes from what the string can be: the literal union a
/// parameter takes, the keys of an indexed object, and files for a module specifier.
#[test]
fn completion_in_string_literals() {
    if !libs_present() {
        return;
    }
    let text = "declare function f(m: \"get\" | \"put\"): void;\nf(\"\");\nconst o = { alpha: 1, \"b c\": 2 };\no[\"\"];\nimport {} from \"./\";\n";
    let env = Env::new(
        "complete-strings",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/c.ts", text),
            ("src/other.ts", "export {};\n"),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/c.ts");
    let uc = env.uri("src/c.ts");
    let labels = |r: &Value| -> Vec<String> {
        r["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|i| i["label"].as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 1, 3)));
    assert_eq!(l, ["get", "put"]);
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 3, 3)));
    assert_eq!(l, ["alpha", "b c"]);
    let l = labels(&result(&mut c, "textDocument/completion", at(&uc, 4, 18)));
    assert!(l.iter().any(|x| x.starts_with("other")), "{l:?}");
    assert!(!l.iter().any(|x| x.starts_with('c')), "{l:?}");
    c.shutdown();
}

/// Completions inside object literals and object binding patterns offer the properties of
/// the contextual or destructured type not written yet; optional ones are labelled `name?`
/// (inserting `name`); a variable's own initializer does not offer the variable.
#[test]
fn completion_object_literals_and_patterns() {
    if !libs_present() {
        return;
    }
    let text = "interface P { a: number; b?: string; c: boolean }\nconst p: P = { a: 1, };\nconst q: P = {\n  a: 1\n  \n};\ndeclare const v: P;\nconst { a, } = v;\nv.;\nconst zz = z;\nfunction g({ }: P) {}\n";
    let env = Env::new(
        "complete-objects",
        &[("tsconfig.json", TSCONFIG), ("src/o.ts", text)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/o.ts");
    let u = env.uri("src/o.ts");
    let labels = |r: &Value| -> Vec<String> {
        r["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|i| i["label"].as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    // `{ a: 1, |}`
    let r = result(&mut c, "textDocument/completion", at(&u, 1, 21));
    assert_eq!(labels(&r), ["b?", "c"], "{r}");
    let b = &r["items"][0];
    assert_eq!(b["insertText"], "b", "{b}");
    assert_eq!(b["filterText"], "b", "{b}");
    // After a member with no comma yet.
    let l = labels(&result(&mut c, "textDocument/completion", at(&u, 4, 2)));
    assert_eq!(l, ["b?", "c"]);
    // `const { a, | } = v`
    let l = labels(&result(&mut c, "textDocument/completion", at(&u, 7, 11)));
    assert_eq!(l, ["b?", "c"]);
    // `v.|`
    let l = labels(&result(&mut c, "textDocument/completion", at(&u, 8, 2)));
    assert_eq!(l, ["a", "b?", "c"]);
    // `const zz = z|`
    let l = labels(&result(&mut c, "textDocument/completion", at(&u, 9, 12)));
    assert!(!l.iter().any(|x| x == "zz"), "{l:?}");
    assert!(l.iter().any(|x| x == "q"), "{l:?}");
    // A parameter's pattern.
    let l = labels(&result(&mut c, "textDocument/completion", at(&u, 10, 13)));
    assert_eq!(l, ["a", "b?", "c"]);
    c.shutdown();
}

/// Module specifier completions follow a package's `exports` patterns (names relative to
/// the typed directory) and the project's `imports` map.
#[test]
fn completion_package_exports_and_imports() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "complete-exports",
        &[
            ("tsconfig.json", TSCONFIG),
            (
                "package.json",
                r##"{"name": "app", "type": "module", "imports": {"#lib/*": "./src/lib/*.js"}}"##,
            ),
            (
                "node_modules/pkg/package.json",
                r#"{"name": "pkg", "exports": {"./sub/*": "./dist/*.js", "./main": "./dist/main.js"}}"#,
            ),
            ("node_modules/pkg/dist/a.d.ts", "export {};\n"),
            ("node_modules/pkg/dist/b/c.d.ts", "export {};\n"),
            ("node_modules/pkg/dist/main.d.ts", "export {};\n"),
            ("src/lib/x.ts", "export {};\n"),
            (
                "src/i.ts",
                "import {} from \"pkg/\";\nimport {} from \"pkg/sub/b/\";\nimport {} from \"#lib/\";\n",
            ),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/i.ts");
    let u = env.uri("src/i.ts");
    let labels = |c: &mut ScriptedClient, line: u32, ch: u32| -> Vec<String> {
        let r = result(c, "textDocument/completion", at(&u, line, ch));
        let mut l: Vec<String> = r["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|i| i["label"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        l.sort();
        l
    };
    // `pkg/`: the exact key and the pattern key's directory.
    assert_eq!(labels(&mut c, 0, 20), ["main", "sub"]);
    // `pkg/sub/b/`: files below the pattern's target, named from the typed directory; the
    // pattern's own `.js` suffix is not repeated.
    assert_eq!(labels(&mut c, 1, 26), ["c"]);
    // `#lib/`: the project's `imports` map, from source files.
    assert_eq!(labels(&mut c, 2, 21), ["x"]);
    c.shutdown();
}

/// Signature help: the overload a call resolves to is active, generic candidates show
/// instantiated, rest tuples expand into parameters, type argument lists and contextually
/// typed parameter lists get help, and private constructors get none from outside.
#[test]
fn signature_help_candidates_and_sites() {
    if !libs_present() {
        return;
    }
    let (a, m) = marked(
        "declare function f(a: number): number;\n\
         declare function f(b: string): number;\n\
         f(¦\"x\");\n\
         declare function g<T>(a: T): T;\n\
         g(¦\n\
         ;declare function w(...args: [string, string] | [number, string, string]): void;\n\
         w(1, \"\", ¦);\n\
         declare function h<T, U>(): void;\n\
         h<number, ¦;\n\
         declare function takesCb(cb: (n: number, s: string) => void): void;\n\
         takesCb((n, ¦));\n\
         class P { private constructor() {} }\n\
         new P(¦);\n\
         declare function opt(c?: number): void;\n\
         opt(¦);\n",
    );
    let env = Env::new("sighelp", &[("tsconfig.json", TSCONFIG), ("src/a.ts", &a)]);
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/a.ts");
    let u = env.uri("src/a.ts");
    let ask = |c: &mut ScriptedClient, (l, ch): (u32, u32)| {
        result(c, "textDocument/signatureHelp", at(&u, l, ch))
    };
    let labels = |s: &Value| -> Vec<String> {
        s["signatures"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|x| x["label"].as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    let s = ask(&mut c, m[0]);
    assert_eq!(s["activeSignature"], 1, "{s}");
    let s = ask(&mut c, m[1]);
    assert_eq!(labels(&s), ["g(a: unknown): unknown"], "{s}");
    let s = ask(&mut c, m[2]);
    assert_eq!(
        labels(&s),
        [
            "w(args_0: string, args_1: string): void",
            "w(args_0: number, args_1: string, args_2: string): void"
        ],
        "{s}"
    );
    assert_eq!(
        (s["activeSignature"].clone(), s["activeParameter"].clone()),
        (json!(1), json!(2))
    );
    let s = ask(&mut c, m[3]);
    assert_eq!(labels(&s), ["h<T, U>(): void"], "{s}");
    assert_eq!(s["activeParameter"], 1);
    let s = ask(&mut c, m[4]);
    assert_eq!(labels(&s), ["cb(n: number, s: string): void"], "{s}");
    assert_eq!(s["activeParameter"], 1);
    assert_eq!(ask(&mut c, m[5]), Value::Null);
    let s = ask(&mut c, m[6]);
    assert_eq!(labels(&s), ["opt(c?: number): void"], "{s}");
    c.shutdown();
}

/// Source with `¦` markers: the text without them and each marker's (line, character).
fn marked(src: &str) -> (String, Vec<(u32, u32)>) {
    let mut text = String::new();
    let mut at = Vec::new();
    let (mut line, mut ch) = (0u32, 0u32);
    for c in src.chars() {
        match c {
            '¦' => at.push((line, ch)),
            '\n' => {
                text.push(c);
                line += 1;
                ch = 0;
            }
            _ => {
                text.push(c);
                ch += c.len_utf16() as u32;
            }
        }
    }
    (text, at)
}

fn completion_labels(r: &Value) -> Vec<String> {
    r["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|i| i["label"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Completions from what a position expects: a type literal in a type argument, literal
/// values, members of an optional value and members that are not identifiers.
#[test]
fn completion_expected_types_and_literals() {
    if !libs_present() {
        return;
    }
    let (a, m) = marked(
        "interface Foo { one: string; two: number; 333: symbol }\n\
         interface Bar<T extends Foo> { foo: T }\n\
         var x: Bar<{ one: string, ¦ }>;\n\
         declare function test<P extends \"a\" | \"b\">(p: P): void;\n\
         test(¦);\n\
         var y: Pick<Foo, \"¦\">;\n\
         declare const o: { \"a b\": 1; c?: { d: number } };\n\
         o.¦;\n\
         o.c?.¦;\n",
    );
    let env = Env::new(
        "complete-expected",
        &[("tsconfig.json", TSCONFIG), ("src/a.ts", &a)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/a.ts");
    let u = env.uri("src/a.ts");
    let ask = |c: &mut ScriptedClient, (l, ch): (u32, u32)| {
        result(c, "textDocument/completion", at(&u, l, ch))
    };
    let l = completion_labels(&ask(&mut c, m[0]));
    assert_eq!(l, ["\"333\"", "two"]);
    let l = completion_labels(&ask(&mut c, m[1]));
    for want in ["\"a\"", "\"b\""] {
        assert!(l.iter().any(|x| x == want), "{want}: {l:?}");
    }
    let l = completion_labels(&ask(&mut c, m[2]));
    assert!(
        l.contains(&"one".to_string()) && l.contains(&"two".to_string()),
        "{l:?}"
    );
    let r = ask(&mut c, m[3]);
    let ab = r["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == "a b")
        .cloned()
        .unwrap_or_else(|| panic!("{r}"));
    assert_eq!(ab["insertText"], "[\"a b\"]", "{ab}");
    assert_eq!(ab["textEdit"]["range"]["start"]["character"], 1, "{ab}");
    assert!(completion_labels(&r).contains(&"c?".to_string()), "{r}");
    let l = completion_labels(&ask(&mut c, m[4]));
    assert_eq!(l, ["d"]);
    c.shutdown();
}

/// Names being declared get no list (a constructor parameter only its modifiers); an
/// unclosed function's names are in scope at the end of the file, except the parameter being
/// written and the later ones.
#[test]
fn completion_definition_locations_and_unclosed_scopes() {
    if !libs_present() {
        return;
    }
    let (a, m) = marked(
        "class ¦\nfunction f(a, ¦) {}\nclass K { constructor(¦) {} }\nenum E { A, ¦ }\n\
         import * as ¦ from \"./b.js\";\nfunction g<T, ¦>() {}\nnamespace ¦N {}\n",
    );
    let (b, mb) =
        marked("export {};\nfunction foo(x: string) {\n  function bar(a: number, b: string = ¦");
    let env = Env::new(
        "complete-definitions",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/a.ts", &a),
            ("src/b.ts", &b),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/a.ts");
    env.open(&mut c, "src/b.ts");
    let ua = env.uri("src/a.ts");
    for (i, &(l, ch)) in m.iter().enumerate() {
        let r = result(&mut c, "textDocument/completion", at(&ua, l, ch));
        let labels = completion_labels(&r);
        match i {
            2 => assert_eq!(
                labels,
                ["override", "private", "protected", "public", "readonly"]
            ),
            6 => assert!(labels.contains(&"K".to_string()), "{labels:?}"),
            _ => assert!(labels.is_empty(), "marker {i}: {labels:?}"),
        }
    }
    let ub = env.uri("src/b.ts");
    let l = completion_labels(&result(
        &mut c,
        "textDocument/completion",
        at(&ub, mb[0].0, mb[0].1),
    ));
    for want in ["a", "x", "foo", "bar"] {
        assert!(l.iter().any(|x| x == want), "{want}: {l:?}");
    }
    assert!(!l.iter().any(|x| x == "b"), "{l:?}");
    c.shutdown();
}

/// At the start of an enum member's name the token before decides: after `,` nothing, but
/// a JSDoc comment in between lifts that (tsc reads a token inside the comment).
#[test]
fn completion_at_enum_member_start_after_jsdoc() {
    if !libs_present() {
        return;
    }
    let (src, marks) = marked(
        "enum Colors {\n    /** blue */\n    ¦Cornflower,\n    ¦Pink\n}\nvar x = Colors.Pink;\n",
    );
    let env = Env::new(
        "complete-enum-jsdoc",
        &[("tsconfig.json", TSCONFIG), ("src/e.ts", src.as_str())],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/e.ts");
    let u = env.uri("src/e.ts");
    let after_doc = result(
        &mut c,
        "textDocument/completion",
        at(&u, marks[0].0, marks[0].1),
    );
    assert!(
        completion_labels(&after_doc).iter().any(|l| l == "Colors"),
        "{after_doc}"
    );
    let after_comma = result(
        &mut c,
        "textDocument/completion",
        at(&u, marks[1].0, marks[1].1),
    );
    assert!(completion_labels(&after_comma).is_empty(), "{after_comma}");
    c.shutdown();
}

/// A CommonJS JavaScript file has `module` and `exports` in scope.
#[test]
fn completion_commonjs_locals() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "complete-commonjs",
        &[
            (
                "tsconfig.json",
                r#"{"compilerOptions": {"allowJs": true, "module": "nodenext", "target": "es2022", "noEmit": true}, "include": ["src"]}"#,
            ),
            ("src/m.js", "const x = require(\"./n.js\");\nexp\n"),
            ("src/n.js", "module.exports = 1;\n"),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/m.js");
    let u = env.uri("src/m.js");
    let r = result(&mut c, "textDocument/completion", at(&u, 1, 3));
    let l: Vec<&str> = r["items"]
        .as_array()
        .map(|a| a.iter().filter_map(|i| i["label"].as_str()).collect())
        .unwrap_or_default();
    assert!(l.contains(&"exports") && l.contains(&"module"), "{l:?}");
    c.shutdown();
}

/// Signature help shows a generic call's instantiated signature, constructors without
/// `new`, and tagged templates (the template text is argument 0).
#[test]
fn signature_help_instantiated_construct_and_tagged() {
    if !libs_present() {
        return;
    }
    let text = "function f<T>(x: T, y: string): T { return x; }\nf(1, \"\");\nclass B { constructor(a: number) {} }\nnew B();\nfunction t(s: TemplateStringsArray, a: number, b: string) {}\nt`x ${1} y ${\"z\"}`;\n";
    let env = Env::new(
        "signature-kinds",
        &[("tsconfig.json", TSCONFIG), ("src/s.ts", text)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/s.ts");
    let u = env.uri("src/s.ts");
    let help = |c: &mut ScriptedClient, line: u32, ch: u32| -> (String, Value) {
        let s = result(c, "textDocument/signatureHelp", at(&u, line, ch));
        let label = s["signatures"][0]["label"]
            .as_str()
            .unwrap_or("")
            .to_string();
        (label, s["activeParameter"].clone())
    };
    assert_eq!(
        help(&mut c, 1, 5),
        ("f(x: 1, y: string): 1".into(), json!(1))
    );
    assert_eq!(help(&mut c, 3, 6), ("B(a: number): B".into(), json!(0)));
    let tagged = "t(s: TemplateStringsArray, a: number, b: string): void";
    assert_eq!(help(&mut c, 5, 3), (tagged.into(), json!(0)));
    assert_eq!(help(&mut c, 5, 6), (tagged.into(), json!(1)));
    assert_eq!(help(&mut c, 5, 15), (tagged.into(), json!(2)));
    c.shutdown();
}

/// Highlight kinds follow tsc 7 (a declaration that gives no value reads; `this.x += 1`
/// writes), and a call goes to the overload it resolves to.
#[test]
fn highlight_kinds_and_overload_definitions() {
    if !libs_present() {
        return;
    }
    let text = "interface I { m(): void; p: string }\nclass C {\n  a;\n  b = 1;\n  constructor() { this.a += 1; }\n}\nlet v;\nconst w = 1;\nfunction f(x: number): number;\nfunction f(x: string): string;\nfunction f(x: any) { return x; }\nf(\"s\");\n";
    let env = Env::new(
        "highlight-kinds",
        &[("tsconfig.json", TSCONFIG), ("src/k.ts", text)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/k.ts");
    let u = env.uri("src/k.ts");
    let kinds = |c: &mut ScriptedClient, line: u32, ch: u32| -> Vec<(u64, u64)> {
        let h = result(c, "textDocument/documentHighlight", at(&u, line, ch));
        let mut v: Vec<(u64, u64)> = h
            .as_array()
            .unwrap()
            .iter()
            .map(|x| {
                (
                    x["range"]["start"]["line"].as_u64().unwrap(),
                    x["kind"].as_u64().unwrap(),
                )
            })
            .collect();
        v.sort();
        v
    };
    assert_eq!(kinds(&mut c, 0, 14), vec![(0, 2)]); // m(): void
    assert_eq!(kinds(&mut c, 0, 25), vec![(0, 2)]); // p: string
    assert_eq!(kinds(&mut c, 2, 2), vec![(2, 2), (4, 3)]); // a; and this.a += 1
    assert_eq!(kinds(&mut c, 3, 2), vec![(3, 3)]); // b = 1
    assert_eq!(kinds(&mut c, 4, 4), vec![(4, 2)]); // constructor
    assert_eq!(kinds(&mut c, 6, 4), vec![(6, 2)]); // let v;
    assert_eq!(kinds(&mut c, 7, 6), vec![(7, 3)]); // const w = 1
    let d = result(&mut c, "textDocument/definition", at(&u, 11, 0));
    let lines: Vec<u64> = d
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["range"]["start"]["line"].as_u64().unwrap())
        .collect();
    assert_eq!(lines, vec![9], "{d}");
    c.shutdown();
}

/// tsc 7 shows an alias under its own name, without an `import` line.
#[test]
fn hover_alias_uses_local_name() {
    if !libs_present() {
        return;
    }
    let b = "import { type Point as P, len } from \"./a.js\";\nimport * as ns from \"./a.js\";\nlet q: P | undefined;\nlen; ns;\n";
    let env = Env::new(
        "hover-alias",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/a.ts", NAV_A),
            ("src/b.ts", b),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/a.ts");
    env.open(&mut c, "src/b.ts");
    let ub = env.uri("src/b.ts");
    let h = hover_text(&mut c, &ub, 2, 7);
    assert!(h.contains("(alias) interface P"), "{h}");
    let h = hover_text(&mut c, &ub, 3, 1);
    assert!(h.contains("(alias) function len(s: string): number"), "{h}");
    let h = hover_text(&mut c, &ub, 3, 6);
    assert!(h.contains("(alias) namespace ns"), "{h}");
    assert!(!h.contains("import "), "{h}");
    c.shutdown();
}

/// The (line, character) of the `k`th byte of the first `needle` in `src` (ASCII sources).
fn pos_of(src: &str, needle: &str, k: usize) -> (u32, u32) {
    let at = src.find(needle).expect("needle in source") + k;
    let before = &src[..at];
    let line = before.matches('\n').count() as u32;
    let col = before.len() - before.rfind('\n').map_or(0, |i| i + 1);
    (line, col as u32)
}

/// Quick info in tsc 7's shape: one line per meaning or overload, resolved signatures at
/// calls, owners with their type parameters or arguments, `(accessor)`, constructors.
#[test]
fn hover_quick_info_lines() {
    if !libs_present() {
        return;
    }
    let src = "namespace N {\n    export function f(x: number): void;\n    export function f(x: string): void;\n    export function f(x: any) {}\n}\nclass G<T> {\n    accessor a = 1;\n    m<U>(u: U, t: T): U { return u; }\n    constructor(x?: number) {}\n    \"lit\"() {}\n}\nconst v = new G<string>();\nv.m(1, \"a\");\nN.f(1);\nN.f;\nv[\"lit\"]();\nfunction opt(c?: () => string) {}\nexport default class {}\n";
    let env = Env::new(
        "hover-quick-info",
        &[("tsconfig.json", TSCONFIG), ("src/q.ts", src)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/q.ts");
    let u = env.uri("src/q.ts");
    let mut h = |needle: &str, k: usize| {
        let (l, ch) = pos_of(src, needle, k);
        let v = hover_text(&mut c, &u, l, ch);
        let v = v.strip_prefix("```typescript\n").unwrap_or(&v);
        v.split("\n```").next().unwrap_or("").to_string()
    };
    assert_eq!(h("N.f(1)", 2), "function N.f(x: number): void");
    assert_eq!(
        h("N.f;", 2),
        "function N.f(x: number): void\nfunction N.f(x: string): void"
    );
    assert_eq!(h("v.m(1", 2), "(method) G<string>.m<1>(u: 1, t: string): 1");
    assert_eq!(h("accessor a", 9), "(accessor) G<T>.a: number");
    assert_eq!(
        h("m<U>", 2),
        "(type parameter) U in G<T>.m<U>(u: U, t: T): U"
    );
    assert_eq!(h("constructor(", 3), "constructor G<T>(x?: number): G<T>");
    assert_eq!(
        h("new G<string>", 4),
        "constructor G<string>(x?: number): G<string>"
    );
    assert_eq!(h("v[\"lit\"]", 3), "(method) G<string>[\"lit\"](): void");
    assert_eq!(
        h("function opt", 10),
        "function opt(c?: () => string): void"
    );
    assert_eq!(h("default class", 2), "class default");
    c.shutdown();
}

#[test]
fn hover_quick_info_through_aliases() {
    if !libs_present() {
        return;
    }
    // A one-member enum, a class exported with `export =`, an ambient module's `export =`
    // and a narrowed alias.
    let a = "export enum K {\n    Only\n}\nexport declare const env: string | undefined;\n";
    let d = "declare class C<T> {\n    m(): void;\n}\nexport = C;\n";
    let amb = "declare module \"amb\" {\n    const inner: number;\n    export = inner;\n}\n";
    let b = "/// <reference path=\"./amb.d.ts\" />\nimport { K, env } from \"./a\";\nimport C = require(\"./d\");\nimport outer = require(\"amb\");\nlet k = K.Only;\ndeclare var x: C<number>;\nx.m;\nconst o = outer;\nif (typeof env === \"string\") {\n    env.length;\n}\n";
    let env = Env::new(
        "hover-quick-info-alias",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/a.ts", a),
            ("src/d.d.ts", d),
            ("src/amb.d.ts", amb),
            ("src/b.ts", b),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/b.ts");
    let u = env.uri("src/b.ts");
    let mut h = |needle: &str, k: usize| {
        let (l, ch) = pos_of(b, needle, k);
        let v = hover_text(&mut c, &u, l, ch);
        let v = v.strip_prefix("```typescript\n").unwrap_or(&v);
        v.split("\n```").next().unwrap_or("").to_string()
    };
    assert_eq!(h("Only;", 1), "(enum member) K.Only = 0");
    assert_eq!(h("x.m;", 2), "(method) C<number>.m(): void");
    assert!(h("= outer;", 3).starts_with("(alias) const outer: number"));
    assert!(h("env.length", 1).starts_with("(alias) const env: string | undefined"));
    c.shutdown();
}

/// Quick info prints through the checker's quick-info mode: optional parameters and methods
/// without the `| undefined` optionality adds (an optional property read keeps it), an
/// optional chain's link with the property's own type, and a property being assigned with
/// its declared (write) type.
#[test]
fn hover_optional_members_and_write_locations() {
    if !libs_present() {
        return;
    }
    let src = "interface I {\n    p?: number;\n    f?(x?: string): void;\n}\ndeclare const i: I;\ni.p;\ni.f;\ndeclare const o: { a?: { b: string } } | undefined;\no?.a?.b;\n";
    // Under exactOptionalPropertyTypes a write shows the declared type.
    let w = "declare const xx: { prop?: number };\nxx.prop = 1;\n";
    let exact = r#"{"compilerOptions": {"strict": true, "exactOptionalPropertyTypes": true, "noEmit": true}, "include": ["src"]}"#;
    for (name, config, file, text) in [
        ("hover-optional", TSCONFIG, "src/o.ts", src),
        ("hover-optional-write", exact, "src/w.ts", w),
    ] {
        let env = Env::new(name, &[("tsconfig.json", config), (file, text)]);
        let mut c = env.lsp("w1");
        env.open(&mut c, file);
        let u = env.uri(file);
        let mut h = |needle: &str, k: usize| {
            let (l, ch) = pos_of(text, needle, k);
            let v = hover_text(&mut c, &u, l, ch);
            let v = v.strip_prefix("```typescript\n").unwrap_or(&v);
            v.split("\n```").next().unwrap_or("").to_string()
        };
        if text == src {
            assert_eq!(h("i.p;", 2), "(property) I.p?: number | undefined");
            assert_eq!(h("i.f;", 2), "(method) I.f?(x?: string): void");
            assert_eq!(h("?.b;", 2), "(property) b: string");
        } else {
            assert_eq!(h("xx.prop =", 3), "(property) prop?: number");
        }
        c.shutdown();
    }
}

/// `o?.next` reads `next` through the object's non-nullable part; destructuring
/// `{ next: { next } }` of an optional `next` does not (tsc finds no property there).
#[test]
fn references_through_optional_objects() {
    if !libs_present() {
        return;
    }
    let src = "interface R {\n    next?: R;\n}\ndeclare const o: R;\no.next?.next;\nfunction f({ next: { next: x } }: R) {}\n";
    let env = Env::new(
        "refs-optional",
        &[("tsconfig.json", TSCONFIG), ("src/r.ts", src)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/r.ts");
    let u = env.uri("src/r.ts");
    let mut refs = |line: u32, ch: u32| -> Vec<(u64, u64)> {
        let r = result(
            &mut c,
            "textDocument/references",
            json!({"textDocument": {"uri": u}, "position": {"line": line, "character": ch},
                   "context": {"includeDeclaration": true}}),
        );
        let mut v: Vec<(u64, u64)> = r
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|l| {
                        (
                            l["range"]["start"]["line"].as_u64().unwrap(),
                            l["range"]["start"]["character"].as_u64().unwrap(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    };
    // The declaration, both accesses and the outer destructuring key; not the inner key.
    assert_eq!(refs(1, 5), vec![(1, 4), (4, 2), (4, 8), (5, 13)]);
    c.shutdown();
}

/// References on a `constructor` keyword: its overloads, `new` of the class and of derived
/// classes without their own constructor, `new this()` in static members and `super(...)`
/// in derived constructors; not a variable holding the class or a class that implements it.
#[test]
fn references_on_a_constructor() {
    if !libs_present() {
        return;
    }
    let (src, marks) = marked(
        "class A {\n    ¦constructor(s: string);\n    ¦constructor(s?: string) {}\n    static f() { return new ¦this(\"\"); }\n}\nclass B extends A {}\nclass C extends B {\n    constructor() { ¦super(\"\"); }\n}\nclass E implements A {\n    constructor() {}\n}\nnew ¦A(\"a\");\nnew ¦B(\"b\");\nnew C();\nconst D = A;\nnew D(\"d\");\n",
    );
    let env = Env::new(
        "refs-constructor",
        &[("tsconfig.json", TSCONFIG), ("src/c.ts", src.as_str())],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/c.ts");
    let u = env.uri("src/c.ts");
    let r = result(
        &mut c,
        "textDocument/references",
        json!({"textDocument": {"uri": u}, "position": {"line": marks[0].0, "character": marks[0].1},
               "context": {"includeDeclaration": true}}),
    );
    let mut got: Vec<(u32, u32)> = r
        .as_array()
        .map(|a| {
            a.iter()
                .map(|l| {
                    (
                        l["range"]["start"]["line"].as_u64().unwrap() as u32,
                        l["range"]["start"]["character"].as_u64().unwrap() as u32,
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    got.sort();
    assert_eq!(got, marks);
    c.shutdown();
}

// ---- Parity check and fallback (K-17) ----

const PARITY_A: &str = "export const bad: number = \"x\";\n";
/// tsc's line for PARITY_A (tsc 7: `src/a.ts(1,14): error TS2322`).
const TSC_LINE: &str =
    "src/a.ts(1,14): error TS2322: Type 'string' is not assignable to type 'number'.";

/// A project with a fake TypeScript 6 install (the fallback is the fake tsserver) and a fake
/// tsc printing `tsc_lines` (and counting its runs in `tsc-runs`).
fn parity_env(name: &str, tsc_lines: &[&str]) -> Env {
    let env = Env::new(
        name,
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/a.ts", PARITY_A),
            (
                "node_modules/typescript/package.json",
                r#"{"version":"6.0.0"}"#,
            ),
            ("node_modules/typescript/lib/tsserver.js", ""),
        ],
    );
    let script = env.dir.join("fake-tsc.sh");
    let mut body = format!("#!/bin/sh\necho run >> {}/tsc-runs\n", env.dir.display());
    for l in tsc_lines {
        body.push_str(&format!("printf '%s\\n' \"{}\"\n", l.replace('"', "\\\"")));
    }
    body.push_str("exit 2\n");
    fs::write(&script, body).unwrap();
    let tsc = json!(["/bin/sh", script.to_string_lossy()]).to_string();
    let tsserver = json!([BIN, "__fake-backend", "tsserver"]).to_string();
    env.with("TS_R_LSP_PARITY", "on")
        .with("TS_R_LSP_PARITY_TSC", &tsc)
        .with("TS_R_LSP_TSSERVER_CMD", &tsserver)
}

/// Waits for a `ts-r/status` whose first project is in `state`.
fn status_with(c: &mut ScriptedClient, state: &str) -> Option<Value> {
    c.wait_for(T, |m| {
        m["method"] == "ts-r/status" && m["params"]["projects"][0]["state"] == state
    })
    .map(|m| m["params"].clone())
}

fn tsc_runs(env: &Env) -> usize {
    fs::read_to_string(env.dir.join("tsc-runs"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[test]
fn parity_match_keeps_our_checker_and_is_cached() {
    if !libs_present() {
        return;
    }
    let env = parity_env("parity-match", &[TSC_LINE]);
    let mut c = env.lsp("w1");
    let ua = env.uri("src/a.ts");
    env.open(&mut c, "src/a.ts");
    let st = status_with(&mut c, "matched").expect("parity matched");
    assert_eq!(st["projects"][0]["parity"]["result"], "match", "{st}");
    let d = diags_for(&mut c, &ua, |d| !d.is_empty()).expect("diagnostics");
    assert_eq!(d[0]["source"], "ts-r");
    c.shutdown();
    env.stop();
    assert_eq!(tsc_runs(&env), 1);
    // A new daemon answers from the cache (same config, lockfile and TypeScript).
    let mut c = env.lsp("w2");
    env.open(&mut c, "src/a.ts");
    let st = status_with(&mut c, "matched").expect("cached parity");
    assert_eq!(st["projects"][0]["parity"]["cached"], true, "{st}");
    assert_eq!(tsc_runs(&env), 1);
    c.shutdown();
}

#[test]
fn parity_mismatch_falls_back_for_diagnostics_only() {
    if !libs_present() {
        return;
    }
    let env = parity_env(
        "parity-fallback",
        &[
            TSC_LINE,
            "src/a.ts(1,1): error TS1234: Something only tsc says.",
        ],
    );
    let mut c = env.lsp("w1");
    let ua = env.uri("src/a.ts");
    env.open(&mut c, "src/a.ts");
    let st = status_with(&mut c, "fallback").expect("fallback");
    let parity = &st["projects"][0]["parity"];
    assert_eq!(parity["result"], "mismatch", "{st}");
    assert_eq!(parity["onlyTheirs"][0], "src/a.ts(1,1): TS1234", "{st}");
    let report = parity["report"].as_str().expect("report path");
    let text = fs::read_to_string(report).expect("report written");
    assert!(text.contains("TS1234"), "{text}");
    assert!(
        st["summary"]
            .as_str()
            .unwrap()
            .contains("use your TypeScript")
    );
    // Diagnostics now come from the user's TypeScript (the fake tsserver's code 9999)...
    let d = diags_for(&mut c, &ua, |d| d.iter().any(|x| x["code"] == 9999)).expect("fallback");
    assert!(d.iter().all(|x| x["source"] != "ts-r"), "{d:?}");
    // ...and follow edits.
    change(&mut c, &ua, 2, range(0, 0, 0, 0), "\n");
    assert!(diags_for(&mut c, &ua, |d| d.iter().any(|x| x["code"] == 9999)).is_some());
    // Other features stay on our checker.
    let h = hover_text(&mut c, &ua, 1, 14);
    assert!(h.contains("const bad: number"), "{h}");
    c.shutdown();
}

#[test]
fn folder_rename_across_projects() {
    if !libs_present() {
        return;
    }
    let cfg = r#"{"compilerOptions": {"strict": true, "module": "esnext", "moduleResolution": "bundler", "target": "es2022", "noEmit": true}, "include": ["src"]}"#;
    let env = Env::new(
        "renameprojects",
        &[
            ("a/tsconfig.json", cfg),
            (
                "a/src/main.ts",
                "import { helper } from \"./lib/util\";\n\nexport const n = helper(1);\n",
            ),
            (
                "a/src/lib/util.ts",
                "export function helper(x: number): number { return x; }\n",
            ),
            ("b/tsconfig.json", cfg),
            (
                "b/src/use.ts",
                "import { helper } from \"../../a/src/lib/util\";\n\nexport const m = helper(2);\n",
            ),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "a/src/main.ts");
    env.open(&mut c, "b/src/use.ts");
    let ub = env.uri("b/src/use.ts");
    assert!(diags_for(&mut c, &ub, |d| d.is_empty()).is_some());
    let e = result(
        &mut c,
        "workspace/willRenameFiles",
        json!({"files": [{"oldUri": env.uri("a/src/lib"), "newUri": env.uri("a/src/core")}]}),
    );
    let text = |uri: &str| -> Vec<String> {
        e["changes"][uri]
            .as_array()
            .into_iter()
            .flatten()
            .map(|x| x["newText"].as_str().unwrap_or("").to_string())
            .collect()
    };
    assert_eq!(text(&env.uri("a/src/main.ts")), ["./core/util"], "{e}");
    assert_eq!(text(&ub), ["../../a/src/core/util"], "{e}");
    assert_eq!(e["changes"].as_object().unwrap().len(), 2, "{e}");
    c.shutdown();
}

/// (file, line, character) of each location in a references answer, sorted.
fn ref_spots(env: &Env, r: &Value) -> Vec<(String, u64, u64)> {
    let mut out: Vec<(String, u64, u64)> = r
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            let uri = l["uri"].as_str().unwrap();
            let file = ["src/a.ts", "src/b.ts", "src/c.ts"]
                .into_iter()
                .find(|f| env.uri(f) == uri)
                .unwrap_or("?")
                .to_string();
            let s = &l["range"]["start"];
            (
                file,
                s["line"].as_u64().unwrap(),
                s["character"].as_u64().unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

const REFS_A: &str = "class Base { ¦m() {} static ¦s() {} }\nclass D extends Base { m() {} static s() {} #p = 1; f() { return this.¦#p; } }\ninterface ¦Foo {}\nnamespace Foo { export const x = 1; }\nlet t: Foo;\nFoo.x;\ninterface I { [\"¦k\"]: number }\nclass K implements I { [\"k\"] = 1; }\nexport function ¦foo() {}\nexport default function () {}\n";
const REFS_B: &str = "import { foo as ¦bar } from \"./a.js\";\nexport { bar as baz };\nbar();\n";
const REFS_C: &str = "import { baz } from \"./b.js\";\nimport d from \"./a.js\";\nbaz(); d();\n";

/// References follow inherited members, private names, computed literal keys, the meaning
/// of the location (an interface merged with a namespace), and aliases that rename.
#[test]
fn references_through_inheritance_meanings_and_aliases() {
    if !libs_present() {
        return;
    }
    let (a, m) = marked(REFS_A);
    let (b, mb) = marked(REFS_B);
    let (cc, _) = marked(REFS_C);
    let env = Env::new(
        "navrefs",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/a.ts", &a),
            ("src/b.ts", &b),
            ("src/c.ts", &cc),
        ],
    );
    let mut c = env.lsp("w1");
    for f in ["src/a.ts", "src/b.ts", "src/c.ts"] {
        env.open(&mut c, f);
    }
    let ua = env.uri("src/a.ts");
    let ub = env.uri("src/b.ts");
    let _ = diags_for(&mut c, &ua, |_| true);
    let mut refs = |uri: &str, (line, ch): (u32, u32)| {
        let r = result(
            &mut c,
            "textDocument/references",
            json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": ch},
                   "context": {"includeDeclaration": true}}),
        );
        ref_spots(&env, &r)
    };
    let s = |v: &[(&str, u64, u64)]| -> Vec<(String, u64, u64)> {
        v.iter().map(|&(f, l, c)| (f.to_string(), l, c)).collect()
    };
    // An override is one with the member it overrides; statics are not inherited.
    assert_eq!(
        refs(&ua, m[0]),
        s(&[("src/a.ts", 0, 13), ("src/a.ts", 1, 23)])
    );
    assert_eq!(refs(&ua, m[1]), s(&[("src/a.ts", 0, 27)]));
    // `this.#p` is the class's own `#p`.
    assert_eq!(
        refs(&ua, m[2]),
        s(&[("src/a.ts", 1, 44), ("src/a.ts", 1, 70)])
    );
    // From the interface: its type uses, not the namespace's.
    assert_eq!(
        refs(&ua, m[3]),
        s(&[("src/a.ts", 2, 10), ("src/a.ts", 4, 7)])
    );
    // `["k"]` in an interface and in the class implementing it.
    assert_eq!(
        refs(&ua, m[4]),
        s(&[("src/a.ts", 6, 16), ("src/a.ts", 7, 25)])
    );
    // From the declaration: every alias under another name and its uses.
    assert_eq!(
        refs(&ua, m[5]),
        s(&[
            ("src/a.ts", 8, 16),
            ("src/b.ts", 0, 9),
            ("src/b.ts", 0, 16),
            ("src/b.ts", 1, 9),
            ("src/b.ts", 1, 16),
            ("src/b.ts", 2, 0),
            ("src/c.ts", 0, 9),
            ("src/c.ts", 2, 0),
        ])
    );
    // From a renaming alias: only what goes through it.
    assert_eq!(
        refs(&ub, mb[0]),
        s(&[
            ("src/b.ts", 0, 16),
            ("src/b.ts", 1, 9),
            ("src/b.ts", 1, 16),
            ("src/b.ts", 2, 0),
            ("src/c.ts", 0, 9),
            ("src/c.ts", 2, 0),
        ])
    );
    // The `default` keyword of an anonymous default export: it and its imports.
    assert_eq!(
        refs(&ua, (9, 8)),
        s(&[("src/a.ts", 9, 7), ("src/c.ts", 1, 7), ("src/c.ts", 2, 7)])
    );
    c.shutdown();
}

const DOCS_A: &str = "export class Target {}
class Base {
  /** Base method. */ m() {} }
class Sub extends Base { m() {} }
interface Opts {
  /** The width. */
  width: number;
  /** Any other key. */
  [k: string]: any;
}
/**
 * Draws. See {@link Target} and {@link https://ts-r.dev|the site}.
 * @param o - the options `@param` stays
 * @param {number} n count @returns nothing
 * @example
 * draw(o, 1)
 */
function draw({ width }: Opts, n: number, o2: Opts) {
  wid¦th; o2.ot¦her; n¦;
}
new Sub().¦m();
dr¦aw;
";

#[test]
fn hover_documentation_follows_tsc() {
    if !libs_present() {
        return;
    }
    let (a, m) = marked(DOCS_A);
    let env = Env::new(
        "hoverdocs",
        &[("tsconfig.json", TSCONFIG), ("src/a.ts", &a)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/a.ts");
    let ua = env.uri("src/a.ts");
    let _ = diags_for(&mut c, &ua, |_| true);
    let doc = |c: &mut ScriptedClient, (l, ch): (u32, u32)| {
        let h = hover_text(c, &ua, l, ch);
        h.split_once("\n```\n")
            .map(|(_, d)| d.trim().to_string())
            .unwrap_or_default()
    };
    // A destructured property: the property's comment.
    assert_eq!(doc(&mut c, m[0]), "The width.");
    // Through an index signature: its comment.
    assert_eq!(doc(&mut c, m[1]), "Any other key.");
    // A parameter: its `@param` tag (types stripped; a later `@tag` starts a new tag).
    assert_eq!(doc(&mut c, m[2]), "count");
    // An override without a comment: the base member's.
    assert_eq!(doc(&mut c, m[3]), "Base method.");
    let d = doc(&mut c, m[4]);
    assert!(d.starts_with("Draws. See [Target](file://"), "{d}");
    assert!(
        d.contains("/src/a.ts#1,14-1,20) and [the site](https://ts-r.dev)."),
        "{d}"
    );
    assert!(
        d.contains("*@param* `o` - the options `@param` stays"),
        "{d}"
    );
    assert!(
        d.contains("*@param* `n` — count\n\n*@returns* — nothing"),
        "{d}"
    );
    assert!(d.contains("*@example*\n```tsx\ndraw(o, 1)\n```"), "{d}");
    c.shutdown();
}

const QI_A: &str = "import m = require(\"./b\");
import { y } from \"./¦b\";
interface I { /** The x. */ x: number }
const i: I = { ¦x: 0 };
[1].forEach(cla¦ss {});
const f = (a: number) =¦> a;
class C<T> {}
C.proto¦type;
m.¦N;
";

const QI_B: &str = "export const y = 1;
export namespace N { export const z = 1; }
";

#[test]
fn hover_names_like_tsc() {
    if !libs_present() {
        return;
    }
    let (a, m) = marked(QI_A);
    let env = Env::new(
        "hovernames",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/a.ts", &a),
            ("src/b.ts", QI_B),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/a.ts");
    let ua = env.uri("src/a.ts");
    let _ = diags_for(&mut c, &ua, |_| true);
    let mut h = |(l, ch): (u32, u32)| hover_text(&mut c, &ua, l, ch);
    assert!(h(m[0]).contains("module \"./b\""), "{}", h(m[0]));
    let x = h(m[1]);
    assert!(
        x.contains("(property) I.x: number") && x.contains("The x."),
        "{x}"
    );
    assert!(
        h(m[2]).contains("(local class) (Anonymous class)"),
        "{}",
        h(m[2])
    );
    assert!(
        h(m[3]).contains("function f(a: number): number"),
        "{}",
        h(m[3])
    );
    assert!(
        h(m[4]).contains("(property) C<T>.prototype: C<any>"),
        "{}",
        h(m[4])
    );
    assert!(h(m[5]).contains("namespace m.N"), "{}", h(m[5]));
    c.shutdown();
}

/// Go to definition follows tsc 7 on calls (`new C()` adds the constructor, shown whole; a
/// called arrow is named by its variable), ambient module names, object literal and
/// binding pattern members (the contextual or destructured type's property, a
/// discriminated union member), index signatures and `typeof this`.
#[test]
fn definitions_of_calls_literals_and_ambient_modules() {
    if !libs_present() {
        return;
    }
    let text = "class C {\n  constructor(n: number) {}\n}\nnew C(1);\nconst f = () => 0;\nf();\n\
                import x from \"m\";\nx;\n\
                interface A { kind: \"a\"; p: number }\ninterface B { kind: \"b\"; p: string }\n\
                const u: A | B = { kind: \"a\", p: 0 };\n\
                const p = 1;\nconst o: { p: number } = { p };\n\
                function g({ p: q }: A) { return q; }\n\
                declare const ix: { [k: string]: boolean };\nix.foo;\n\
                function h(this: A) { type T = typeof this; }\n";
    let env = Env::new(
        "def-calls",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/k.ts", text),
            ("src/m.d.ts", "declare module \"m\";\n"),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/k.ts");
    let u = env.uri("src/k.ts");
    let um = env.uri("src/m.d.ts");
    let spans = |c: &mut ScriptedClient, line: u32, ch: u32| -> Vec<(String, u64, u64, u64, u64)> {
        let d = result(c, "textDocument/definition", at(&u, line, ch));
        let mut v: Vec<_> = d
            .as_array()
            .into_iter()
            .flatten()
            .map(|l| {
                let r = &l["range"];
                let n = |x: &Value| x.as_u64().unwrap();
                (
                    if l["uri"] == um.as_str() { "m" } else { "k" }.to_string(),
                    n(&r["start"]["line"]),
                    n(&r["start"]["character"]),
                    n(&r["end"]["line"]),
                    n(&r["end"]["character"]),
                )
            })
            .collect();
        v.sort();
        v
    };
    let k = |a, b, c, d| ("k".to_string(), a, b, c, d);
    // `new C(1)`: the class and the whole constructor.
    assert_eq!(spans(&mut c, 3, 4), vec![k(0, 6, 0, 7), k(1, 2, 1, 27)]);
    // `f()`: the arrow, named by its variable.
    assert_eq!(spans(&mut c, 5, 0), vec![k(4, 6, 4, 7)]);
    // An import of a shorthand ambient module, and its specifier: the module.
    let m = vec![("m".to_string(), 0, 15, 0, 18)];
    assert_eq!(spans(&mut c, 7, 0), m);
    assert_eq!(spans(&mut c, 6, 15), m);
    // `kind` of a literal typed `A | B` with `kind: "a"`: only `A`'s.
    assert_eq!(spans(&mut c, 10, 20), vec![k(8, 14, 8, 18)]);
    // A shorthand: the variable and the contextual property.
    assert_eq!(
        spans(&mut c, 12, 27),
        vec![k(11, 6, 11, 7), k(12, 11, 12, 12)]
    );
    // A binding key: the destructured type's property.
    assert_eq!(spans(&mut c, 13, 13), vec![k(8, 25, 8, 26)]);
    // A property only an index signature declares.
    assert_eq!(spans(&mut c, 15, 3), vec![k(14, 20, 14, 40)]);
    // `typeof this` goes to the `this` parameter.
    assert_eq!(spans(&mut c, 16, 38), vec![k(16, 11, 16, 15)]);
    c.shutdown();
}

const DOC_LINKS: &str = "class C {
    m() {}
    /** {@link m} @see {C.prototype.m} {@link C#m} */
    p() {}
}
enum E {
    /** {@link Foo} */
    Foo,
}
interface Foo { x: E.Foo }
/** @throws {C} {@link C.
";

#[test]
fn references_rename_hover_and_definition_in_doc_comment_links() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "navlinks",
        &[("tsconfig.json", TSCONFIG), ("src/l.ts", DOC_LINKS)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/l.ts");
    let u = env.uri("src/l.ts");
    let _ = diags_for(&mut c, &u, |_| true);
    let starts = |v: &Value| -> Vec<(u64, u64)> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|l| {
                let s = &l["range"]["start"];
                (
                    s["line"].as_u64().unwrap(),
                    s["character"].as_u64().unwrap(),
                )
            })
            .collect()
    };
    let refs = |c: &mut ScriptedClient, line: u32, ch: u32| -> Vec<(u64, u64)> {
        let r = result(
            c,
            "textDocument/references",
            json!({"textDocument": {"uri": u}, "position": {"line": line, "character": ch},
                   "context": {"includeDeclaration": true}}),
        );
        starts(&r)
    };
    // A bare member name, `C.prototype.m` and `C#m` all name the method, from the
    // declaration or from a link.
    let m = vec![(1, 4), (2, 15), (2, 36), (2, 48)];
    assert_eq!(refs(&mut c, 1, 4), m);
    assert_eq!(refs(&mut c, 2, 15), m);
    // The class, in links and in an unterminated comment's `@throws` and `{@link C.`.
    assert_eq!(
        refs(&mut c, 0, 6),
        vec![(0, 6), (2, 24), (2, 46), (10, 13), (10, 23)]
    );
    // In an enum, a link names the member before an outer type of the same name.
    assert_eq!(refs(&mut c, 6, 15), vec![(6, 15), (7, 4), (9, 21)]);
    assert!(hover_text(&mut c, &u, 2, 15).contains("(method) C.m(): void"));
    let d = result(&mut c, "textDocument/definition", at(&u, 2, 48));
    assert_eq!(starts(&d), vec![(1, 4)], "{d}");
    let e = result(
        &mut c,
        "textDocument/rename",
        json!({"textDocument": {"uri": u}, "position": {"line": 2, "character": 36}, "newName": "k"}),
    );
    let mut lines: Vec<u64> = e["changes"][u.as_str()]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["range"]["start"]["line"].as_u64().unwrap())
        .collect();
    lines.sort();
    assert_eq!(lines, vec![1, 2, 2, 2], "{e}");
    c.shutdown();
}

const STRING_REFS: &str = "type Options = \"one\" | \"two\";
let o: Options = \"one\";
const d: \"one\" = \"one\";
var x = \"plain\";
type T = \"this\";
let t: T = \"this\";
";

#[test]
fn references_and_rename_of_string_literal_types() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "navstrings",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/s.ts", STRING_REFS),
            ("src/e.ts", "const e: \"\\u006fne\" = \"\\u006fne\";\n"),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/s.ts");
    let u = env.uri("src/s.ts");
    let _ = diags_for(&mut c, &u, |_| true);
    let starts = |v: &Value| -> Vec<(u64, u64)> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|l| {
                let s = &l["range"]["start"];
                (
                    s["line"].as_u64().unwrap(),
                    s["character"].as_u64().unwrap(),
                )
            })
            .collect()
    };
    let refs = |c: &mut ScriptedClient, line: u32, ch: u32| -> Vec<(u64, u64)> {
        let r = result(
            c,
            "textDocument/references",
            json!({"textDocument": {"uri": u}, "position": {"line": line, "character": ch},
                   "context": {"includeDeclaration": true}}),
        );
        starts(&r)
    };
    // Literals of the same union type, not the `"one"` of another type.
    assert_eq!(refs(&mut c, 0, 16), vec![(0, 16), (1, 18)]);
    // The `"one"` type's literals here and, written with escapes, in e.ts (its line 0).
    let mut one = refs(&mut c, 2, 18);
    one.sort();
    assert_eq!(one, vec![(0, 10), (0, 23), (2, 10), (2, 18)]);
    // A literal without a contextual type is found alone.
    assert_eq!(refs(&mut c, 3, 9), vec![(3, 9)]);
    let rename = |c: &mut ScriptedClient, line: u32, ch: u32| -> Value {
        result(
            c,
            "textDocument/rename",
            json!({"textDocument": {"uri": u}, "position": {"line": line, "character": ch}, "newName": "uno"}),
        )
    };
    let e = rename(&mut c, 1, 18);
    let mut lines: Vec<u64> = e["changes"][u.as_str()]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["range"]["start"]["line"].as_u64().unwrap())
        .collect();
    lines.sort();
    assert_eq!(lines, vec![0, 1], "{e}");
    // A plain string is no literal type: nothing to rename.
    assert!(rename(&mut c, 3, 9).is_null());
    // Literals written with escapes are read by their value, in any file.
    env.open(&mut c, "src/e.ts");
    let ue = env.uri("src/e.ts");
    let r = result(
        &mut c,
        "textDocument/references",
        json!({"textDocument": {"uri": ue}, "position": {"line": 0, "character": 10},
               "context": {"includeDeclaration": true}}),
    );
    let mut got: Vec<(bool, u64, u64)> = r
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            let s = &l["range"]["start"];
            (
                l["uri"].as_str().unwrap().ends_with("e.ts"),
                s["line"].as_u64().unwrap(),
                s["character"].as_u64().unwrap(),
            )
        })
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![(false, 2, 10), (false, 2, 18), (true, 0, 10), (true, 0, 23)],
        "{r}"
    );
    // A literal whose text is `this` is renamed like any other.
    let e = rename(&mut c, 5, 12);
    let n = e["changes"][u.as_str()].as_array().map_or(0, Vec::len);
    assert_eq!(n, 2, "{e}");
    c.shutdown();
}

const JSX_REFS: &str = "declare namespace JSX {
    interface Element { }
    interface IntrinsicElements { div: { name?: string } }
}
const x = <div name=\"a\" wrong />;
";

#[test]
fn references_of_intrinsic_tags_and_jsx_attributes() {
    if !libs_present() {
        return;
    }
    let tsconfig = r#"{"compilerOptions": {"strict": true, "jsx": "preserve", "noEmit": true}, "include": ["src"]}"#;
    let env = Env::new(
        "navjsx",
        &[("tsconfig.json", tsconfig), ("src/x.tsx", JSX_REFS)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/x.tsx");
    let u = env.uri("src/x.tsx");
    let _ = diags_for(&mut c, &u, |_| true);
    let refs = |c: &mut ScriptedClient, line: u32, ch: u32| -> Vec<(u64, u64)> {
        let r = result(
            c,
            "textDocument/references",
            json!({"textDocument": {"uri": u}, "position": {"line": line, "character": ch},
                   "context": {"includeDeclaration": true}}),
        );
        r.as_array()
            .unwrap()
            .iter()
            .map(|l| {
                let s = &l["range"]["start"];
                (
                    s["line"].as_u64().unwrap(),
                    s["character"].as_u64().unwrap(),
                )
            })
            .collect()
    };
    // The tag is `JSX.IntrinsicElements`'s property; the attribute is its props' property.
    assert_eq!(refs(&mut c, 4, 11), vec![(2, 34), (4, 11)]);
    assert_eq!(refs(&mut c, 2, 34), vec![(2, 34), (4, 11)]);
    assert_eq!(refs(&mut c, 4, 15), vec![(2, 41), (4, 15)]);
    // An attribute that sets no prop is found alone.
    assert_eq!(refs(&mut c, 4, 24), vec![(4, 24)]);
    c.shutdown();
}

/// `(file, line, character)` of each location in a references answer, sorted.
fn ref_names(r: &Value) -> Vec<(String, u64, u64)> {
    let mut v: Vec<(String, u64, u64)> = r
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|l| {
            let uri = l["uri"].as_str().unwrap_or("");
            let file = uri.rsplit('/').next().unwrap_or("").to_string();
            let s = &l["range"]["start"];
            (
                file,
                s["line"].as_u64().unwrap(),
                s["character"].as_u64().unwrap(),
            )
        })
        .collect();
    v.sort();
    v
}

fn refs_at(c: &mut ScriptedClient, uri: &str, line: u32, ch: u32) -> Vec<(String, u64, u64)> {
    let r = result(
        c,
        "textDocument/references",
        json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": ch},
               "context": {"includeDeclaration": true}}),
    );
    ref_names(&r)
}

/// `(file, line, character, newText)` of each edit in a rename answer, sorted.
fn rename_edits(
    c: &mut ScriptedClient,
    uri: &str,
    line: u32,
    ch: u32,
) -> Vec<(String, u64, u64, String)> {
    let e = result(
        c,
        "textDocument/rename",
        json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": ch}, "newName": "z"}),
    );
    let mut v = Vec::new();
    if let Some(m) = e["changes"].as_object() {
        for (u, edits) in m {
            let file = u.rsplit('/').next().unwrap_or("").to_string();
            for t in edits.as_array().unwrap() {
                let s = &t["range"]["start"];
                v.push((
                    file.clone(),
                    s["line"].as_u64().unwrap(),
                    s["character"].as_u64().unwrap(),
                    t["newText"].as_str().unwrap().to_string(),
                ));
            }
        }
    }
    v.sort();
    v
}

fn s(f: &str, l: u64, c: u64) -> (String, u64, u64) {
    (f.to_string(), l, c)
}

const KW_A: &str = "export class C { static x = 1; }\nexport const a = 1;\n";
const KW_B: &str = "import { C } from \"./a\";\nimport * as ns from \"./a\";\nnew C();\ndelete C.x;\nlet v: typeof C | undefined;\nfunction f<T extends C>(t: T) { return t instanceof C; }\ntype K = keyof C;\nexport { f };\n";

#[test]
fn references_from_keywords_and_module_specifiers() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "navkw",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/a.ts", KW_A),
            ("src/b.ts", KW_B),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/a.ts");
    env.open(&mut c, "src/b.ts");
    let ub = env.uri("src/b.ts");
    let _ = diags_for(&mut c, &ub, |_| true);
    let class_c = refs_at(&mut c, &ub, 0, 9);
    assert!(class_c.len() >= 7, "{class_c:?}");
    // `new`, `typeof`, `instanceof`, `extends` and `keyof` read their operand `C`.
    for (line, ch) in [(2, 0), (4, 7), (5, 46), (5, 13), (6, 9)] {
        assert_eq!(refs_at(&mut c, &ub, line, ch), class_c, "at {line}:{ch}");
    }
    // `delete C.x` reads the property.
    assert_eq!(
        refs_at(&mut c, &ub, 3, 0),
        vec![s("a.ts", 0, 24), s("b.ts", 3, 9)]
    );
    // `import` with one specifier reads it; `from` and a specifier read the module.
    assert_eq!(refs_at(&mut c, &ub, 0, 0), class_c);
    let module = vec![s("b.ts", 0, 19), s("b.ts", 1, 21)];
    assert_eq!(refs_at(&mut c, &ub, 0, 13), module);
    assert_eq!(refs_at(&mut c, &ub, 1, 23), module);
    // `as` of `* as ns` reads `ns`; `export` of `export { f }` reads `f`.
    assert_eq!(refs_at(&mut c, &ub, 1, 9), vec![s("b.ts", 1, 12)]);
    assert_eq!(
        refs_at(&mut c, &ub, 7, 0),
        vec![s("b.ts", 5, 9), s("b.ts", 7, 9)]
    );
    // Rename on a declaration keyword renames the declared name.
    let e = rename_edits(&mut c, &ub, 5, 0);
    assert_eq!(e.len(), 2, "{e:?}");
    c.shutdown();
}

const IT_A: &str = "export type T = number;\nexport namespace N { export type U = string; }\n";
const IT_B: &str =
    "const x: import(\"./a\").T = 0;\nconst y: import(\"./a\").N.U = \"\";\nexport { x, y };\n";

#[test]
fn references_through_import_type_qualifiers() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "navit",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/a.ts", IT_A),
            ("src/b.ts", IT_B),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/a.ts");
    env.open(&mut c, "src/b.ts");
    let ub = env.uri("src/b.ts");
    let ua = env.uri("src/a.ts");
    let _ = diags_for(&mut c, &ub, |_| true);
    let t = vec![s("a.ts", 0, 12), s("b.ts", 0, 23)];
    assert_eq!(refs_at(&mut c, &ua, 0, 12), t);
    assert_eq!(refs_at(&mut c, &ub, 0, 23), t);
    let n = vec![s("a.ts", 1, 17), s("b.ts", 1, 23)];
    assert_eq!(refs_at(&mut c, &ub, 1, 23), n);
    let u = vec![s("a.ts", 1, 33), s("b.ts", 1, 25)];
    assert_eq!(refs_at(&mut c, &ub, 1, 25), u);
    // The specifier of a qualified import type is a module reference too (as in tsc 7).
    assert_eq!(
        refs_at(&mut c, &ub, 0, 18),
        vec![s("b.ts", 0, 17), s("b.ts", 1, 17)]
    );
    c.shutdown();
}

const RN_A: &str = "var x = 1;\nexport { x };\nexport { x as y };\nexport const w = 2;\n";
const RN_B: &str = "import { x, w } from \"./a\";\nx; w;\nexport { w };\n";

#[test]
fn rename_stops_at_name_keeping_specifiers() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "navrn",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/a.ts", RN_A),
            ("src/b.ts", RN_B),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/a.ts");
    env.open(&mut c, "src/b.ts");
    let ua = env.uri("src/a.ts");
    let ub = env.uri("src/b.ts");
    let _ = diags_for(&mut c, &ub, |_| true);
    let e = |f: &str, l, c: u64, t: &str| (f.to_string(), l, c, t.to_string());
    // The declaration keeps the name it is exported under.
    assert_eq!(
        rename_edits(&mut c, &ua, 0, 4),
        vec![
            e("a.ts", 0, 4, "z"),
            e("a.ts", 1, 9, "z as x"),
            e("a.ts", 2, 9, "z")
        ]
    );
    // The export specifier renames the export and its importers.
    assert_eq!(
        rename_edits(&mut c, &ua, 1, 9),
        vec![
            e("a.ts", 1, 9, "x as z"),
            e("b.ts", 0, 9, "z"),
            e("b.ts", 1, 0, "z")
        ]
    );
    // An import renames only the local name.
    assert_eq!(
        rename_edits(&mut c, &ub, 1, 0),
        vec![e("b.ts", 0, 9, "x as z"), e("b.ts", 1, 0, "z")]
    );
    // A declaration exported by its own name renames the imports; a re-export keeps it.
    assert_eq!(
        rename_edits(&mut c, &ua, 3, 13),
        vec![
            e("a.ts", 3, 13, "z"),
            e("b.ts", 0, 12, "z"),
            e("b.ts", 1, 3, "z"),
            e("b.ts", 2, 9, "z as w")
        ]
    );
    c.shutdown();
}

/// The labels of a completion answer (`None` when there is no list or it is empty).
fn completion_at(c: &mut ScriptedClient, uri: &str, line: u32, ch: u32) -> Option<Vec<String>> {
    let r = result(c, "textDocument/completion", at(uri, line, ch));
    let items = r["items"].as_array().filter(|a| !a.is_empty())?;
    Some(
        items
            .iter()
            .map(|i| i["label"].as_str().unwrap().to_string())
            .collect(),
    )
}

const CP: &str = "outer: while (true) {\n  inner: for (;;) {\n    break ;\n  }\n}\nconst t = `a ${1} b`;\nconst r = /x/g;\nfunction f(a = 1) {}\ntype A<K = number, L = string> = K;\nfunction g<C extends Array<number>>() {}\n";

#[test]
fn completions_for_labels_literals_and_type_parameters() {
    if !libs_present() {
        return;
    }
    let env = Env::new("cmpl8", &[("tsconfig.json", TSCONFIG), ("src/a.ts", CP)]);
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/a.ts");
    let u = env.uri("src/a.ts");
    let _ = diags_for(&mut c, &u, |_| true);
    // After `break`: the labels around, innermost first.
    assert_eq!(
        completion_at(&mut c, &u, 2, 10),
        Some(vec!["inner".to_string(), "outer".to_string()])
    );
    // In a template's text (also at its ends) and at the end of a regular expression:
    // no list. Inside the hole: a list.
    for ch in [11, 12, 13, 18, 19] {
        assert_eq!(completion_at(&mut c, &u, 5, ch), None, "at 5:{ch}");
    }
    assert!(completion_at(&mut c, &u, 5, 15).is_some());
    assert_eq!(completion_at(&mut c, &u, 6, 14), None);
    // `arguments` in a parameter's default.
    let l = completion_at(&mut c, &u, 7, 15).unwrap_or_default();
    assert!(l.iter().any(|x| x == "arguments"), "{l:?}");
    // A type parameter's default names neither it nor later ones; a constraint's type
    // argument may name the parameter itself.
    let l = completion_at(&mut c, &u, 8, 11).unwrap_or_default();
    assert!(!l.iter().any(|x| x == "K" || x == "L"), "{l:?}");
    let l = completion_at(&mut c, &u, 9, 27).unwrap_or_default();
    assert!(l.iter().any(|x| x == "C"), "{l:?}");
    c.shutdown();
}

const THIS_SRC: &str = "interface R { n: number }\ninterface F { (this: void, n: number): void }\nclass Foo {\n  p: this;\n  m(this: R) {}\n  f() { type Y = typeof this.p; }\n}\nlet g: F = function (this, n) {};\n";

#[test]
fn hover_on_this_parameters_types_and_type_queries() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "hover8",
        &[("tsconfig.json", TSCONFIG), ("src/a.ts", THIS_SRC)],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/a.ts");
    let u = env.uri("src/a.ts");
    let _ = diags_for(&mut c, &u, |_| true);
    // The `this` type, a `this` parameter (declared and contextual), and `typeof this`.
    assert!(hover_text(&mut c, &u, 3, 6).contains("\nthis\n"));
    assert!(hover_text(&mut c, &u, 4, 5).contains("(parameter) this: R"));
    assert!(hover_text(&mut c, &u, 5, 25).contains("this: this"));
    assert!(hover_text(&mut c, &u, 7, 22).contains("(parameter) this: void"));
    c.shutdown();
}

const MC_A: &str = "export class C {}\n";
const MC_B: &str = "/* copyright */\n/// <reference path=\"./a.ts\" />\nimport { C } /* from */ from \"./a\";\nconst t: import(\"./a\").C = new C();\n";

#[test]
fn module_references_past_comments() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "navmc",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/a.ts", MC_A),
            ("src/b.ts", MC_B),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/b.ts");
    let ub = env.uri("src/b.ts");
    let _ = diags_for(&mut c, &ub, |_| true);
    // A directive after a block comment, the `from` keyword and a qualified import type's
    // specifier all refer to the module; a `from` inside a comment is no keyword.
    let module = vec![s("b.ts", 1, 21), s("b.ts", 2, 30), s("b.ts", 3, 17)];
    for (line, ch) in [(1, 23), (2, 25), (3, 18)] {
        assert_eq!(refs_at(&mut c, &ub, line, ch), module, "at {line}:{ch}");
    }
    assert_eq!(refs_at(&mut c, &ub, 2, 17), Vec::new());
    c.shutdown();
}

const JS_CONFIG: &str = r#"{"compilerOptions": {"allowJs": true, "module": "nodenext", "target": "es2022", "noEmit": true}, "include": ["src"]}"#;

#[test]
fn references_through_commonjs_require() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "navcjs",
        &[
            ("tsconfig.json", JS_CONFIG),
            ("src/a.js", "function f() { }\nmodule.exports = { f };\n"),
            ("src/b.js", "const { f } = require('./a');\nf;\n"),
            (
                "src/c.js",
                "class A {\n  constructor() { }\n}\nmodule.exports = A;\n",
            ),
            ("src/d.js", "const A = require('./c');\nnew A;\nA.g;\n"),
        ],
    );
    let mut c = env.lsp("w1");
    for f in ["src/a.js", "src/b.js", "src/c.js", "src/d.js"] {
        env.open(&mut c, f);
    }
    let (ub, uc, ud) = (
        env.uri("src/b.js"),
        env.uri("src/c.js"),
        env.uri("src/d.js"),
    );
    let _ = diags_for(&mut c, &ud, |_| true);
    // A destructured `require` reads the module's export (and the variable it holds).
    assert_eq!(
        refs_at(&mut c, &ub, 1, 0),
        vec![
            s("a.js", 0, 9),
            s("a.js", 1, 19),
            s("b.js", 0, 8),
            s("b.js", 1, 0)
        ]
    );
    // `const A = require(..)` is found from the class, not the class from it.
    assert_eq!(
        refs_at(&mut c, &uc, 0, 6),
        vec![
            s("c.js", 0, 6),
            s("c.js", 3, 17),
            s("d.js", 0, 6),
            s("d.js", 1, 4),
            s("d.js", 2, 0)
        ]
    );
    assert_eq!(refs_at(&mut c, &ud, 1, 4).len(), 3);
    assert_eq!(
        refs_at(&mut c, &uc, 1, 2),
        vec![s("c.js", 1, 2), s("d.js", 1, 4)]
    );
    c.shutdown();
}

#[test]
fn references_in_jsdoc_extends_tags() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "navext",
        &[
            ("tsconfig.json", JS_CONFIG),
            (
                "src/a.js",
                "export class C {}\n/** @extends C */\nexport class P extends C {}\n/** @augments {C} */\nexport class Q extends C {}\n",
            ),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/a.js");
    let ua = env.uri("src/a.js");
    let _ = diags_for(&mut c, &ua, |_| true);
    let all = vec![
        s("a.js", 0, 13),
        s("a.js", 1, 13),
        s("a.js", 2, 23),
        s("a.js", 3, 15),
        s("a.js", 4, 23),
    ];
    assert_eq!(refs_at(&mut c, &ua, 0, 13), all);
    assert_eq!(refs_at(&mut c, &ua, 1, 13), all);
    c.shutdown();
}

#[test]
fn references_in_jsdoc_param_and_template_tags() {
    if !libs_present() {
        return;
    }
    let env = Env::new(
        "navparam",
        &[
            ("tsconfig.json", JS_CONFIG),
            (
                "src/a.js",
                "/**\n * @overload\n * @param {number} x\n * @returns {number}\n *\n * @param {unknown} x\n * @param {Object} o\n * @param {string} o.x\n */\nexport function f(x, o) { return x; }\n/**\n * @template T\n * @return {T}\n */\nexport function g() {}\n",
            ),
        ],
    );
    let mut c = env.lsp("w1");
    env.open(&mut c, "src/a.js");
    let ua = env.uri("src/a.js");
    let _ = diags_for(&mut c, &ua, |_| true);
    // The function's own `@param x`, not the overload's.
    let x = vec![s("a.js", 5, 20), s("a.js", 9, 18), s("a.js", 9, 33)];
    assert_eq!(refs_at(&mut c, &ua, 9, 18), x);
    assert_eq!(refs_at(&mut c, &ua, 5, 20), x);
    // `o.x` is not a reference to `o`.
    assert_eq!(
        refs_at(&mut c, &ua, 9, 21),
        vec![s("a.js", 6, 19), s("a.js", 9, 21)]
    );
    // A use of a `@template` type lists no `@template` name.
    assert_eq!(refs_at(&mut c, &ua, 12, 12), vec![s("a.js", 12, 12)]);
    c.shutdown();
}
