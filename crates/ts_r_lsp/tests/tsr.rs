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
    assert_eq!(e["changes"][&ua].as_array().unwrap().len(), 1, "{e}");
    assert_eq!(e["changes"][&ub].as_array().unwrap().len(), 2, "{e}");
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
