//! Lint in the daemon (L2), through the LSP protocol: diagnostics on open/change/save, the
//! debounce, code actions, the unsupported-rules note, `ts-r.lint.enable` and config
//! reloads. Projects use `.oxlintrc.json` so no Node is needed; type backends are the fake
//! tsserver (`ts-r-lsp __fake-backend`).
#![cfg(unix)]

use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};
use ts_r_lsp::client::{Dialect, ScriptedClient, did_open_params, initialize_params};

ts_r_testing::cap_memory!(1024);

const BIN: &str = env!("CARGO_BIN_EXE_ts-r-lsp");
const T: Duration = Duration::from_secs(10);

const OXLINT: &str = r#"{
  "categories": { "correctness": "off" },
  "rules": {
    "no-var": "error",
    "no-console": "warn",
    "no-debugger": "error",
    "typescript/no-floating-promises": "error",
    "react/jsx-key": "error"
  }
}
"#;

const A_TS: &str = "var x = 1;\nconsole.log(x);\nfunction f() {\n  debugger;\n}\nf();\n";

struct Env {
    dir: PathBuf,
    proj: PathBuf,
    extra: Vec<(String, String)>,
}

impl Env {
    fn new(name: &str, with_config: bool) -> Env {
        let dir = PathBuf::from(format!("/tmp/tsr-lint-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let proj = dir.join("proj");
        let ts = proj.join("node_modules/typescript");
        fs::create_dir_all(ts.join("lib")).unwrap();
        fs::write(ts.join("package.json"), r#"{"version":"6.0.0"}"#).unwrap();
        fs::write(ts.join("lib/tsserver.js"), "").unwrap();
        if with_config {
            fs::write(proj.join(".oxlintrc.json"), OXLINT).unwrap();
        }
        fs::write(proj.join("a.ts"), A_TS).unwrap();
        fs::write(proj.join("b.ts"), "var y = 2;\nexport { y };\n").unwrap();
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

    /// TypeScript 7: the type backend is the (fake) native LSP server.
    fn native(self) -> Env {
        let ts = self.proj.join("node_modules/typescript");
        fs::write(ts.join("package.json"), r#"{"version":"7.0.0"}"#).unwrap();
        let exe = self
            .proj
            .join("node_modules/@typescript")
            .join(ts_r_lsp::backend::typescript::platform_package())
            .join("lib/tsc");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(exe, "").unwrap();
        let cmd = json!([BIN, "__fake-backend", "lsp"]).to_string();
        self.with("TS_R_LSP_NATIVE_CMD", &cmd)
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
            .env("TS_R_LSP_LINGER_MS", "400")
            .env("TS_R_LSP_TYPE_BACKEND", "typescript")
            .env_remove("TS_R_WORKSPACE_ROOT");
        for (k, v) in &self.extra {
            c.env(k, v);
        }
        c
    }

    fn uri(&self, name: &str) -> String {
        ts_r_lsp::uri::path_to_uri(&self.proj.join(name).to_string_lossy())
    }

    /// An initialized LSP client with `initializationOptions`.
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

    /// The VS Code extension's lint connection.
    fn lint_only(&self, name: &str, enable: &str) -> ScriptedClient {
        self.lsp(
            name,
            json!({"lintOnly": true, "ts-r": {"lint": {"enable": enable}}}),
        )
    }

    fn open(&self, c: &mut ScriptedClient, name: &str) {
        let path = self.proj.join(name);
        let text = fs::read_to_string(&path).unwrap();
        c.notify("textDocument/didOpen", did_open_params(&path, &text))
            .unwrap();
    }

    fn status(&self) -> Value {
        let out = self
            .cmd(&["status", "--root", &self.proj.to_string_lossy()])
            .output()
            .unwrap();
        serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
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

fn is_publish(m: &Value, uri: &str) -> bool {
    m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == json!(uri)
}

fn lint_codes(m: &Value) -> Vec<String> {
    m["params"]["diagnostics"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|d| d["source"] == "ts-r-lint")
        .map(|d| d["code"].as_str().unwrap_or("").to_string())
        .collect()
}

/// Waits for diagnostics for `uri` whose lint codes satisfy `pred`.
fn wait_lint(c: &mut ScriptedClient, uri: &str, pred: impl Fn(&[String]) -> bool) -> Value {
    c.wait_for(T, |m| is_publish(m, uri) && pred(&lint_codes(m)))
        .unwrap_or_else(|| panic!("no matching diagnostics for {uri}"))
}

fn edit(c: &mut ScriptedClient, uri: &str, version: i64, range: [u32; 4], text: &str) {
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri, "version": version},
               "contentChanges": [{"range": {"start": {"line": range[0], "character": range[1]},
                                             "end": {"line": range[2], "character": range[3]}},
                                   "text": text}]}),
    )
    .unwrap();
}

fn code_actions(c: &mut ScriptedClient, uri: &str, line: u32, only: Option<&[&str]>) -> Vec<Value> {
    let ch = 4;
    let mut context = json!({"diagnostics": []});
    if let Some(o) = only {
        context["only"] = json!(o);
    }
    let r = c
        .call(
            "textDocument/codeAction",
            json!({"textDocument": {"uri": uri},
                   "range": {"start": {"line": line, "character": ch}, "end": {"line": line, "character": ch}},
                   "context": context}),
            T,
        )
        .expect("codeAction answered");
    r["result"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("bad codeAction answer: {r}"))
}

#[test]
fn diagnostics_on_open_change_and_save() {
    let env = Env::new("basic", true);
    let mut c = env.lint_only("lint", "on");
    let uri = env.uri("a.ts");
    env.open(&mut c, "a.ts");
    let m = wait_lint(&mut c, &uri, |codes| codes.len() == 3);
    let d = m["params"]["diagnostics"].as_array().unwrap();
    assert_eq!(lint_codes(&m), ["no-var", "no-console", "no-debugger"]);
    assert_eq!(
        d[0]["range"],
        json!({"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 10}})
    );
    assert_eq!(d[0]["severity"], 1);
    assert_eq!(d[1]["severity"], 2, "warn rules are warnings");
    assert_eq!(
        d[0]["codeDescription"]["href"],
        "https://eslint.org/docs/latest/rules/no-var"
    );
    // No diagnostics for unsupported rules.
    assert!(!m.to_string().contains("jsx-key"));

    // An edit (debounced) re-lints the new text.
    edit(&mut c, &uri, 2, [0, 0, 0, 3], "let");
    let m = wait_lint(&mut c, &uri, |codes| codes == ["no-console", "no-debugger"]);
    assert_eq!(m["params"]["diagnostics"][0]["range"]["start"]["line"], 1);

    // Save lints at once.
    c.clear_backlog();
    c.notify(
        "textDocument/didSave",
        json!({"textDocument": {"uri": uri}}),
    )
    .unwrap();
    wait_lint(&mut c, &uri, |codes| codes.len() == 2);

    let s = env.status();
    assert_eq!(s["lint"]["files"], 1);
    assert!(s["lint"]["runs"].as_u64().unwrap() >= 3, "{s}");
    assert_eq!(s["lint"]["configs"], json!([".oxlintrc.json"]));
    assert!(s["clients"][0]["lintOnly"].as_bool().unwrap());

    // Closing clears them and forgets the file.
    c.notify(
        "textDocument/didClose",
        json!({"textDocument": {"uri": uri}}),
    )
    .unwrap();
    c.wait_for(T, |m| {
        is_publish(m, &uri) && m["params"]["diagnostics"] == json!([])
    })
    .expect("cleared on close");
    let deadline = Instant::now() + T;
    while env.status()["lint"]["files"] != 0 {
        assert!(Instant::now() < deadline, "lint state not dropped");
        std::thread::sleep(Duration::from_millis(50));
    }
    c.shutdown();
}

#[test]
fn edits_are_debounced_and_linted_once() {
    let env = Env::new("debounce", true).with("TS_R_LSP_LINT_DEBOUNCE_MS", "300");
    let mut c = env.lint_only("lint", "on");
    let uri = env.uri("a.ts");
    env.open(&mut c, "a.ts");
    wait_lint(&mut c, &uri, |codes| codes.len() == 3);
    let runs_before = env.status()["lint"]["runs"].as_u64().unwrap();
    c.clear_backlog();
    // Five keystrokes 20 ms apart, typing a comment at the end of the file.
    let mut last_edit = Instant::now();
    for (i, ch) in "// hi".chars().enumerate() {
        if i > 0 {
            std::thread::sleep(Duration::from_millis(20));
        }
        edit(
            &mut c,
            &uri,
            2 + i as i64,
            [6, i as u32, 6, i as u32],
            &ch.to_string(),
        );
        last_edit = Instant::now();
    }
    let m = c
        .wait_for(T, |m| is_publish(m, &uri))
        .expect("diagnostics after the burst");
    let waited = last_edit.elapsed();
    // The debounce is 300 ms; allow for timer and scheduling slack on shared CI runners.
    assert!(
        waited >= Duration::from_millis(200),
        "published {waited:?} after the last edit, before the debounce ended"
    );
    assert_eq!(lint_codes(&m).len(), 3);
    assert!(
        c.wait_for(Duration::from_millis(600), |m| is_publish(m, &uri))
            .is_none(),
        "one publish per burst"
    );
    assert_eq!(
        env.status()["lint"]["runs"].as_u64().unwrap(),
        runs_before + 1,
        "the burst was linted once"
    );
    c.shutdown();
}

#[test]
fn code_actions_fix_suggest_and_disable() {
    let env = Env::new("actions", true).with("TS_R_LSP_LINT_DEBOUNCE_MS", "0");
    let mut c = env.lint_only("lint", "on");
    let uri = env.uri("a.ts");
    env.open(&mut c, "a.ts");
    wait_lint(&mut c, &uri, |codes| codes.len() == 3);

    let titles = |a: &[Value]| -> Vec<String> {
        a.iter()
            .map(|x| x["title"].as_str().unwrap().to_string())
            .collect()
    };
    let a = code_actions(&mut c, &uri, 0, None);
    assert_eq!(
        titles(&a),
        [
            "Fix this no-var problem",
            "Disable no-var for this line",
            "Disable no-var for the entire file"
        ]
    );
    let fix = &a[0]["edit"]["changes"][&uri][0];
    assert_eq!(fix["newText"], "let");
    assert_eq!(fix["range"]["end"], json!({"line": 0, "character": 3}));
    assert_eq!(a[0]["kind"], "quickfix");
    assert_eq!(a[0]["diagnostics"][0]["code"], "no-var");

    let a = code_actions(&mut c, &uri, 1, None);
    assert_eq!(titles(&a)[0], "Remove the console.log().");
    let a = code_actions(&mut c, &uri, 3, None);
    let line = a
        .iter()
        .find(|x| x["title"] == "Disable no-debugger for this line")
        .unwrap();
    let e = &line["edit"]["changes"][&uri][0];
    assert_eq!(e["newText"], "  // eslint-disable-next-line no-debugger\n");
    assert_eq!(e["range"]["start"], json!({"line": 3, "character": 0}));

    // source.fixAll: one pass of every fix.
    let a = code_actions(&mut c, &uri, 0, Some(&["source.fixAll"]));
    assert_eq!(a.len(), 1);
    assert_eq!(a[0]["kind"], "source.fixAll.ts-r");
    // Nothing else when only quick fixes of another kind are wanted.
    assert!(code_actions(&mut c, &uri, 0, Some(&["refactor"])).is_empty());

    // Apply "disable for this line": the problem goes away, and the directive counts as used.
    edit(
        &mut c,
        &uri,
        2,
        [3, 0, 3, 0],
        "  // eslint-disable-next-line no-debugger\n",
    );
    wait_lint(&mut c, &uri, |codes| codes == ["no-var", "no-console"]);
    // Apply "disable for the entire file" for no-var.
    let a = code_actions(&mut c, &uri, 0, None);
    let file = a
        .iter()
        .find(|x| x["title"] == "Disable no-var for the entire file")
        .unwrap();
    let e = &file["edit"]["changes"][&uri][0];
    assert_eq!(e["newText"], "/* eslint-disable no-var */\n");
    edit(
        &mut c,
        &uri,
        3,
        [0, 0, 0, 0],
        e["newText"].as_str().unwrap(),
    );
    wait_lint(&mut c, &uri, |codes| codes == ["no-console"]);

    // Stale results give no actions: an edit whose lint has not run yet.
    let env2 = Env::new("actions-stale", true).with("TS_R_LSP_LINT_DEBOUNCE_MS", "2000");
    let mut d = env2.lint_only("lint", "on");
    let uri2 = env2.uri("a.ts");
    env2.open(&mut d, "a.ts");
    wait_lint(&mut d, &uri2, |codes| codes.len() == 3);
    edit(&mut d, &uri2, 2, [5, 0, 5, 0], " ");
    assert!(code_actions(&mut d, &uri2, 0, None).is_empty());
    d.shutdown();
    c.shutdown();
}

#[test]
fn disable_line_inside_jsx_children_is_a_jsx_comment_that_works() {
    let env = Env::new("jsx", true).with("TS_R_LSP_LINT_DEBOUNCE_MS", "0");
    fs::write(
        env.proj.join("v.tsx"),
        "export const v = (\n  <div>\n   {console.log('x')}\n  </div>\n);\n",
    )
    .unwrap();
    let mut c = env.lint_only("lint", "on");
    let uri = env.uri("v.tsx");
    env.open(&mut c, "v.tsx");
    wait_lint(&mut c, &uri, |codes| codes == ["no-console"]);
    let a = code_actions(&mut c, &uri, 2, None);
    let line = a
        .iter()
        .find(|x| x["title"] == "Disable no-console for this line")
        .unwrap();
    let e = &line["edit"]["changes"][&uri][0];
    assert_eq!(
        e["newText"],
        "   {/* eslint-disable-next-line no-console */}\n"
    );
    edit(
        &mut c,
        &uri,
        2,
        [2, 0, 2, 0],
        e["newText"].as_str().unwrap(),
    );
    wait_lint(&mut c, &uri, |codes| codes.is_empty());
    c.shutdown();
}

#[test]
fn lint_actions_are_dropped_if_the_text_changes_while_the_backend_answers() {
    let env = Env::new("pending-actions", true)
        .native()
        .with("TS_R_LSP_LINT_DEBOUNCE_MS", "0")
        .with("TS_R_FAKE_CODE_ACTION_MS", "400");
    let uri = env.uri("a.ts");
    let mut c = env.lsp("a", json!({"ts-r": {"lint": {"enable": "on"}}}));
    env.open(&mut c, "a.ts");
    wait_lint(&mut c, &uri, |codes| codes.len() == 3);
    // Lint's actions are appended to the backend's answer...
    assert!(!code_actions(&mut c, &uri, 0, None).is_empty());
    // ...but not after an edit made while the backend answered: their ranges are into the
    // old text (the no-var fix would replace the new comment).
    let id = c
        .request(
            "textDocument/codeAction",
            json!({"textDocument": {"uri": uri},
                   "range": {"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 1}},
                   "context": {"diagnostics": []}}),
        )
        .unwrap();
    edit(&mut c, &uri, 2, [0, 0, 0, 0], "/* c */ ");
    let r = c.response(id, T).expect("codeAction answered");
    assert_eq!(r["result"], json!([]));
    c.shutdown();
}

#[test]
fn unsupported_rules_are_one_info_message_not_per_file_errors() {
    let env = Env::new("unsupported", true);
    let mut c = env.lint_only("lint", "on");
    env.open(&mut c, "a.ts");
    let shown = c
        .wait_for(T, |m| m["method"] == "window/showMessage")
        .expect("one note");
    assert_eq!(shown["params"]["type"], 3, "info, not an error: {shown}");
    let text = shown["params"]["message"].as_str().unwrap();
    assert!(text.contains(".oxlintrc.json enables 2 rule(s)"), "{text}");
    let logged = c
        .wait_for(T, |m| {
            m["method"] == "window/logMessage"
                && m["params"]["message"]
                    .as_str()
                    .is_some_and(|s| s.contains("react/jsx-key (plugin rule"))
        })
        .expect("the full list in the log");
    assert!(
        logged
            .to_string()
            .contains("typescript/no-floating-promises")
    );
    wait_lint(&mut c, &env.uri("a.ts"), |codes| codes.len() == 3);
    // A second file under the same config: no second note.
    env.open(&mut c, "b.ts");
    wait_lint(&mut c, &env.uri("b.ts"), |codes| codes == ["no-var"]);
    assert!(
        c.wait_for(Duration::from_millis(400), |m| m["method"]
            == "window/showMessage")
            .is_none()
    );
    c.shutdown();
}

#[test]
fn lint_enable_off_on_and_merging_with_type_diagnostics() {
    let env = Env::new("enable", true).with("TS_R_LSP_LINT_DEBOUNCE_MS", "0");
    let uri = env.uri("a.ts");

    // A window with the type backend and lint on: one publish carries both.
    let mut both = env.lsp("both", json!({"ts-r": {"lint": {"enable": "on"}}}));
    env.open(&mut both, "a.ts");
    let m = both
        .wait_for(T, |m| {
            is_publish(m, &uri)
                && lint_codes(m).len() == 3
                && m["params"]["diagnostics"].to_string().contains("pid=")
        })
        .expect("type and lint diagnostics together");
    assert!(m["params"]["diagnostics"].as_array().unwrap().len() >= 4);
    // Its code actions come from lint (the fake tsserver has none).
    assert!(!code_actions(&mut both, &uri, 0, None).is_empty());

    // A window with lint off (the ESLint extension's case) never sees lint diagnostics.
    let mut typed = env.lsp("typed", json!({"ts-r": {"lint": {"enable": "off"}}}));
    env.open(&mut typed, "a.ts");
    let m = typed
        .wait_for(T, |m| {
            is_publish(m, &uri) && m["params"]["diagnostics"].to_string().contains("pid=")
        })
        .expect("type diagnostics");
    assert!(lint_codes(&m).is_empty());
    assert!(code_actions(&mut typed, &uri, 0, None).is_empty());

    // A lint-only connection with lint off publishes nothing...
    let mut lint = env.lint_only("lint", "off");
    env.open(&mut lint, "a.ts");
    assert!(
        lint.wait_for(Duration::from_millis(500), |m| is_publish(m, &uri))
            .is_none()
    );
    // ...until the setting is turned on, and clears when it is turned off again.
    lint.notify(
        "workspace/didChangeConfiguration",
        json!({"settings": {"ts-r": {"lint": {"enable": "on"}}}}),
    )
    .unwrap();
    wait_lint(&mut lint, &uri, |codes| codes.len() == 3);
    lint.notify(
        "workspace/didChangeConfiguration",
        json!({"settings": {"ts-r": {"lint": {"enable": "off"}}}}),
    )
    .unwrap();
    lint.wait_for(T, |m| {
        is_publish(m, &uri) && m["params"]["diagnostics"] == json!([])
    })
    .expect("cleared when turned off");
    let s = env.status();
    let lint_flags: Vec<&str> = s["clients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["lint"].as_str().unwrap())
        .collect();
    assert_eq!(lint_flags, ["on", "off", "off"]);
    lint.shutdown();
    typed.shutdown();
    both.shutdown();
}

#[test]
fn windows_with_different_unsaved_text_keep_their_own_lint_results() {
    let env = Env::new("divergent", true).with("TS_R_LSP_LINT_DEBOUNCE_MS", "0");
    let uri = env.uri("a.ts");
    let on = json!({"ts-r": {"lint": {"enable": "on"}}});
    let mut a = env.lsp("a", on.clone());
    env.open(&mut a, "a.ts");
    env.open(&mut a, "b.ts");
    wait_lint(&mut a, &uri, |codes| codes.len() == 3);
    let mut b = env.lsp("b", on);
    env.open(&mut b, "a.ts");
    wait_lint(&mut b, &uri, |codes| codes.len() == 3);
    // Both windows edit a.ts without saving (K-12: one backend each); B deletes the `var`
    // line, so its results no longer have no-var.
    edit(&mut a, &uri, 2, [5, 0, 5, 0], "// a\n");
    wait_lint(&mut a, &uri, |codes| codes.len() == 3);
    edit(&mut b, &uri, 2, [0, 0, 1, 0], "");
    wait_lint(&mut b, &uri, |codes| codes == ["no-console", "no-debugger"]);
    assert_eq!(env.status()["mode"], "per-client");
    // A's text is unchanged: its code actions are still there...
    let titles: Vec<Value> = code_actions(&mut a, &uri, 0, None)
        .iter()
        .map(|x| x["title"].clone())
        .collect();
    assert!(
        titles.contains(&json!("Fix this no-var problem")),
        "{titles:?}"
    );
    // ...and type diagnostics republished for it carry A's lint results, not B's.
    a.clear_backlog();
    edit(&mut a, &env.uri("b.ts"), 3, [0, 0, 0, 0], " ");
    let m = a
        .wait_for(T, |m| {
            is_publish(m, &uri) && m["params"]["diagnostics"].to_string().contains("pid=")
        })
        .expect("type diagnostics for a.ts");
    assert_eq!(lint_codes(&m), ["no-var", "no-console", "no-debugger"]);
    b.shutdown();
    a.shutdown();
}

#[test]
fn a_saved_config_change_relints_open_files() {
    let env = Env::new("reload", true).with("TS_R_LSP_LINT_DEBOUNCE_MS", "0");
    let mut c = env.lint_only("lint", "on");
    let uri = env.uri("a.ts");
    env.open(&mut c, "a.ts");
    wait_lint(&mut c, &uri, |codes| codes.len() == 3);
    let cfg = env.proj.join(".oxlintrc.json");
    fs::write(
        &cfg,
        OXLINT.replace("\"no-var\": \"error\"", "\"no-var\": \"off\""),
    )
    .unwrap();
    c.notify(
        "workspace/didChangeWatchedFiles",
        json!({"changes": [{"uri": env.uri(".oxlintrc.json"), "type": 2}]}),
    )
    .unwrap();
    wait_lint(&mut c, &uri, |codes| codes == ["no-console", "no-debugger"]);
    // Saving the config in the editor works too.
    fs::write(&cfg, OXLINT).unwrap();
    c.notify(
        "textDocument/didSave",
        json!({"textDocument": {"uri": env.uri(".oxlintrc.json")}}),
    )
    .unwrap();
    wait_lint(&mut c, &uri, |codes| codes.len() == 3);
    assert_eq!(env.status()["lint"]["configReloads"], 2);
    c.shutdown();
}

#[test]
fn files_without_a_lint_config_get_no_lint_diagnostics() {
    let env = Env::new("noconfig", false);
    let mut c = env.lint_only("lint", "on");
    let uri = env.uri("a.ts");
    env.open(&mut c, "a.ts");
    let m = c.wait_for(T, |m| is_publish(m, &uri)).expect("a publish");
    assert_eq!(m["params"]["diagnostics"], json!([]));
    assert!(
        c.wait_for(Duration::from_millis(300), |m| m["method"]
            == "window/showMessage")
            .is_none()
    );
    c.shutdown();
}

#[test]
fn a_file_that_does_not_parse_keeps_its_last_results() {
    let env = Env::new("parse", true).with("TS_R_LSP_LINT_DEBOUNCE_MS", "0");
    let mut c = env.lint_only("lint", "on");
    let uri = env.uri("a.ts");
    env.open(&mut c, "a.ts");
    wait_lint(&mut c, &uri, |codes| codes.len() == 3);
    c.clear_backlog();
    edit(&mut c, &uri, 2, [5, 0, 5, 0], "if (");
    let deadline = Instant::now() + T;
    while env.status()["lint"]["parseErrors"] != 1 {
        assert!(Instant::now() < deadline, "no parse error recorded");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        c.wait_for(Duration::from_millis(200), |m| is_publish(m, &uri))
            .is_none(),
        "no flicker while the text does not parse"
    );
    edit(&mut c, &uri, 3, [5, 0, 5, 4], "");
    wait_lint(&mut c, &uri, |codes| codes.len() == 3);
    c.shutdown();
}

#[test]
fn eslint_flat_config_is_read_through_node_once_then_from_the_cache() {
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("skipped: node is not on PATH");
        return;
    }
    let mut env = Env::new("eslint", false).with("TS_R_LSP_LINT_DEBOUNCE_MS", "0");
    fs::write(
        env.proj.join("eslint.config.mjs"),
        "export default [{ files: ['**/*.ts'], rules: { 'no-var': 'error' } }];\n",
    )
    .unwrap();
    let uri = env.uri("a.ts");
    let mut c = env.lint_only("lint", "on");
    env.open(&mut c, "a.ts");
    wait_lint(&mut c, &uri, |codes| codes == ["no-var"]);
    c.shutdown();
    let root = env.proj.to_string_lossy().into_owned();
    let _ = env.cmd(&["stop", "--root", &root]).output();
    let deadline = Instant::now() + T;
    while !env.status().is_null() {
        assert!(Instant::now() < deadline, "daemon did not stop");
        std::thread::sleep(Duration::from_millis(50));
    }
    // A new daemon without a usable Node still gets the config: from the lint-config cache
    // that ts-r-lint uses too.
    let no_node = env.dir.join("no-node");
    env.extra
        .push(("TS_R_NODE".into(), no_node.to_string_lossy().into_owned()));
    let mut c = env.lint_only("lint", "on");
    env.open(&mut c, "a.ts");
    wait_lint(&mut c, &uri, |codes| codes == ["no-var"]);
    c.shutdown();
}

#[test]
fn a_changed_import_of_an_eslint_config_relints_open_files() {
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("skipped: node is not on PATH");
        return;
    }
    let env = Env::new("eslint-import", false).with("TS_R_LSP_LINT_DEBOUNCE_MS", "0");
    fs::write(
        env.proj.join("eslint.config.mjs"),
        "import rules from './shared-lint.mjs';\nexport default [{ files: ['**/*.ts'], rules }];\n",
    )
    .unwrap();
    let shared = env.proj.join("shared-lint.mjs");
    fs::write(&shared, "export default { 'no-var': 'error' };\n").unwrap();
    let (a, b) = (env.uri("a.ts"), env.uri("b.ts"));
    let mut c = env.lint_only("lint", "on");
    env.open(&mut c, "a.ts");
    env.open(&mut c, "b.ts");
    wait_lint(&mut c, &a, |codes| codes == ["no-var"]);
    wait_lint(&mut c, &b, |codes| codes == ["no-var"]);
    // The imported rules change on disk (not a config file name the daemon watches); the
    // next lint notices, loads the config again and re-lints the other open file too.
    fs::write(
        &shared,
        "export default { 'no-var': 'off', 'no-debugger': 'error' };\n",
    )
    .unwrap();
    edit(&mut c, &a, 2, [5, 0, 5, 0], " ");
    wait_lint(&mut c, &a, |codes| codes == ["no-debugger"]);
    wait_lint(&mut c, &b, |codes| codes.is_empty());
    c.shutdown();
}
