//! Deterministic stand-ins for tsserver and an LSP server, used by the integration tests
//! (`ts-r-lsp __fake-backend tsserver|lsp`, selected with `TS_R_LSP_TSSERVER_CMD` /
//! `TS_R_LSP_NATIVE_CMD`). Every answer includes the backend's pid and its current copy of
//! the file, so tests can see which backend answered and which text it has.
//!
//! `TS_R_FAKE_ALLOC_MB=<n>` makes the fake touch `n` MiB at start (memory budget tests).
//! The LSP fake answers `textDocument/codeAction` with no actions after
//! `TS_R_FAKE_CODE_ACTION_MS` (default 0).
//! The tsserver fake exits with status 3 on the `fake/crash` command, and takes 300 ms over a
//! `quickinfo` on a file containing `/*slow*/` (cancellation tests).

use crate::jsonrpc;
use crate::text::{Pos, TextDoc};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{self, BufReader, Write};
use std::path::Path;

fn hold_memory() -> Vec<u8> {
    let mb: usize = std::env::var("TS_R_FAKE_ALLOC_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut v = vec![0u8; mb * 1024 * 1024];
    for i in (0..v.len()).step_by(4096) {
        v[i] = 1;
    }
    v
}

fn describe(file: &str, docs: &BTreeMap<String, TextDoc>) -> String {
    let text = docs
        .get(file)
        .map(|d| d.text().to_string())
        .unwrap_or_else(|| "<closed>".into());
    format!("pid={} text={}", std::process::id(), text)
}

/// The fake tsserver.
pub fn run_fake_tsserver(args: &[String]) -> i32 {
    let _mem = hold_memory();
    let cancel_prefix = args
        .iter()
        .position(|a| a == "--cancellationPipeName")
        .and_then(|i| args.get(i + 1))
        .map(|p| p.trim_end_matches('*').to_string());
    let stdin = io::stdin();
    let mut r = BufReader::new(stdin.lock());
    let mut out = io::stdout();
    let mut docs: BTreeMap<String, TextDoc> = BTreeMap::new();
    let mut emit = |v: Value| {
        let body = serde_json::to_vec(&v).unwrap_or_default();
        let _ = write!(out, "Content-Length: {}\r\n\r\n", body.len() + 1);
        let _ = out.write_all(&body);
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    };
    while let Ok(Some(line)) = jsonrpc::read_line_or_frame(&mut r) {
        let Ok(m) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let seq = m["seq"].clone();
        let command = m["command"].as_str().unwrap_or("").to_string();
        let a = &m["arguments"];
        let ok = |body: Value| json!({"seq": 0, "type": "response", "command": command, "request_seq": seq, "success": true, "body": body});
        let file = a["file"].as_str().unwrap_or("").to_string();
        match command.as_str() {
            "updateOpen" => {
                for f in a["openFiles"].as_array().into_iter().flatten() {
                    let name = f["file"].as_str().unwrap_or("").to_string();
                    let text = f["fileContent"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| std::fs::read_to_string(&name).unwrap_or_default());
                    docs.insert(name, TextDoc::new(text));
                }
                for f in a["changedFiles"].as_array().into_iter().flatten() {
                    let name = f["fileName"].as_str().unwrap_or("");
                    if let Some(d) = docs.get_mut(name) {
                        // Same order as tsserver: last change first.
                        for c in f["textChanges"].as_array().into_iter().flatten().rev() {
                            d.apply_ts_edit(c);
                        }
                    }
                }
                for f in a["closedFiles"].as_array().into_iter().flatten() {
                    docs.remove(f.as_str().unwrap_or(""));
                }
                emit(ok(json!(true)));
            }
            "configure" | "compilerOptionsForInferredProjects" => emit(ok(json!(true))),
            "quickinfo" => {
                if describe(&file, &docs).contains("/*slow*/") {
                    std::thread::sleep(std::time::Duration::from_millis(300)); // to cancel it
                }
                let start = Pos::from_ts(a).unwrap_or_default();
                emit(ok(json!({
                    "kind": "const", "kindModifiers": "", "displayString": describe(&file, &docs),
                    "documentation": "", "tags": [],
                    "start": start.to_ts(), "end": Pos::new(start.line, start.character + 1).to_ts(),
                })))
            }
            "definition" | "typeDefinition" | "implementation" => emit(ok(json!([
                {"file": file, "start": {"line": 1, "offset": 1}, "end": {"line": 1, "offset": 2}}
            ]))),
            "references" => emit(ok(json!({"refs": [
                {"file": file, "start": {"line": 1, "offset": 1}, "end": {"line": 1, "offset": 2}, "isDefinition": true, "lineText": ""}
            ]}))),
            "completionInfo" => emit(ok(json!({"isIncomplete": false, "entries": [
                {"name": "fakeCompletion", "kind": "var", "kindModifiers": "", "sortText": "11"}
            ]}))),
            "completionEntryDetails" => emit(ok(json!([
                {"name": "fakeCompletion", "kind": "var", "displayParts": [{"text": "var fakeCompletion: number", "kind": "text"}],
                 "documentation": describe(&file, &docs), "tags": []}
            ]))),
            "geterr" => {
                let req = seq.as_i64().unwrap_or(-1);
                for f in a["files"].as_array().into_iter().flatten() {
                    let name = f.as_str().unwrap_or("");
                    if let Some(p) = &cancel_prefix
                        && Path::new(&format!("{p}{req}")).exists()
                    {
                        break;
                    }
                    let diag = json!({
                        "start": {"line": 1, "offset": 1}, "end": {"line": 1, "offset": 2},
                        "text": describe(name, &docs), "code": 9999, "category": "error",
                    });
                    for (ev, diags) in [
                        ("syntaxDiag", json!([])),
                        ("semanticDiag", json!([diag])),
                        ("suggestionDiag", json!([])),
                    ] {
                        emit(
                            json!({"seq": 0, "type": "event", "event": ev, "body": {"file": name, "diagnostics": diags}}),
                        );
                    }
                }
                emit(
                    json!({"seq": 0, "type": "event", "event": "requestCompleted", "body": {"request_seq": seq}}),
                );
            }
            "fake/crash" => std::process::exit(3),
            "exit" => return 0,
            _ => emit(
                json!({"seq": 0, "type": "response", "command": command, "request_seq": seq,
                             "success": false, "message": format!("Unrecognized JSON command: {command}")}),
            ),
        }
    }
    0
}

/// The fake LSP server (stands in for TS 7's native server).
pub fn run_fake_lsp() -> i32 {
    let _mem = hold_memory();
    let stdin = io::stdin();
    let mut r = BufReader::new(stdin.lock());
    let mut out = io::stdout();
    let mut docs: BTreeMap<String, TextDoc> = BTreeMap::new();
    let mut send = |v: Value| {
        let _ = jsonrpc::write_message(&mut out, &v);
    };
    let publish = |uri: &str, docs: &BTreeMap<String, TextDoc>| {
        let diags = match docs.get(uri) {
            Some(_) => json!([{
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
                "severity": 1, "message": describe(uri, docs),
            }]),
            None => json!([]),
        };
        jsonrpc::notification(
            "textDocument/publishDiagnostics",
            json!({"uri": uri, "diagnostics": diags}),
        )
    };
    while let Ok(Some(body)) = jsonrpc::read_frame(&mut r) {
        let Ok(m) = serde_json::from_slice::<Value>(&body) else {
            continue;
        };
        let p = &m["params"];
        let uri = p["textDocument"]["uri"].as_str().unwrap_or("").to_string();
        match jsonrpc::classify(&m) {
            Some(jsonrpc::Kind::Request { id, method }) => {
                let id = id.clone();
                match method {
                    "initialize" => send(jsonrpc::response(
                        id,
                        json!({
                            "capabilities": {"textDocumentSync": {"openClose": true, "change": 2}, "hoverProvider": true},
                            "serverInfo": {"name": "fake-lsp", "initializationOptions": p["initializationOptions"]},
                        }),
                    )),
                    "textDocument/hover" => send(jsonrpc::response(
                        id,
                        json!({"contents": {"kind": "plaintext", "value": describe(&uri, &docs)}}),
                    )),
                    "textDocument/codeAction" => {
                        let ms = std::env::var("TS_R_FAKE_CODE_ACTION_MS")
                            .ok()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                        std::thread::sleep(std::time::Duration::from_millis(ms));
                        send(jsonrpc::response(id, json!([])));
                    }
                    "shutdown" => send(jsonrpc::response(id, Value::Null)),
                    _ => send(jsonrpc::error_response(
                        id,
                        jsonrpc::codes::METHOD_NOT_FOUND,
                        method.to_string(),
                    )),
                }
            }
            Some(jsonrpc::Kind::Notification { method }) => match method {
                "initialized" => send(jsonrpc::request(
                    json!("fake-reg-1"),
                    "client/registerCapability",
                    json!({"registrations": [{"id": "fake-watch", "method": "workspace/didChangeWatchedFiles",
                                              "registerOptions": {"watchers": [{"globPattern": "**/*.ts"}]}}]}),
                )),
                "textDocument/didOpen" => {
                    docs.insert(
                        uri.clone(),
                        TextDoc::new(p["textDocument"]["text"].as_str().unwrap_or("")),
                    );
                    send(publish(&uri, &docs));
                }
                "textDocument/didChange" => {
                    if let Some(d) = docs.get_mut(&uri) {
                        for c in p["contentChanges"].as_array().into_iter().flatten() {
                            d.apply_lsp_change(c);
                        }
                    }
                    send(publish(&uri, &docs));
                }
                "textDocument/didClose" => {
                    docs.remove(&uri);
                    send(publish(&uri, &docs));
                }
                "exit" => return 0,
                _ => {}
            },
            _ => {}
        }
    }
    0
}
