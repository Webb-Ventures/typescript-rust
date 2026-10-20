//! LSP <-> tsserver translation, for LSP clients on a TS 5/6 (tsserver) backend.
//!
//! Pure functions, no I/O. Supported in L1: push diagnostics (from tsserver's
//! `syntaxDiag`/`semanticDiag`/`suggestionDiag` events), hover (`quickinfo`), definition,
//! completion (`completionInfo`) with resolve (`completionEntryDetails`, including auto-import
//! edits in the same file), references and rename. Everything else answers "method not
//! found" (see the crate AGENTS.md for the gap list).
//!
//! Positions: LSP is 0-based line / UTF-16 column, tsserver is 1-based line / 1-based UTF-16
//! offset, so no text is needed to convert.

use crate::text::{Pos, ts_span_to_lsp_range};
use crate::uri;
use serde_json::{Map, Value, json};

/// Characters tsserver accepts as completion trigger characters.
pub const COMPLETION_TRIGGERS: &[&str] = &[".", "\"", "'", "`", "/", "@", "<", "#", " "];

/// What an outstanding translated request was, so its response can be mapped back.
#[derive(Clone, Debug, PartialEq)]
pub enum TsReqKind {
    /// `textDocument/hover`.
    Hover,
    /// `textDocument/definition` (also typeDefinition / implementation, which share shapes).
    Locations,
    /// `textDocument/completion`; the context is stored in each item's `data`.
    Completion {
        /// File the completion was requested in.
        file: String,
        /// Where.
        pos: Pos,
    },
    /// `completionItem/resolve`; keeps the item to fill in.
    CompletionResolve {
        /// The item as the client sent it.
        item: Value,
    },
    /// `textDocument/references`.
    References {
        /// Whether to keep the declaration.
        include_declaration: bool,
    },
    /// `textDocument/rename`.
    Rename {
        /// The new name.
        new_name: String,
    },
    /// `textDocument/prepareRename`.
    PrepareRename,
}

/// A tsserver request built from an LSP request.
#[derive(Clone, Debug, PartialEq)]
pub struct Translated {
    /// tsserver command.
    pub command: &'static str,
    /// tsserver arguments.
    pub args: Value,
    /// How to map the response.
    pub kind: TsReqKind,
}

/// Server capabilities advertised to LSP clients of a tsserver backend.
pub fn server_capabilities() -> Value {
    json!({
        "positionEncoding": "utf-16",
        "textDocumentSync": { "openClose": true, "change": 2, "save": { "includeText": false } },
        "hoverProvider": true,
        "definitionProvider": true,
        "typeDefinitionProvider": true,
        "implementationProvider": true,
        "referencesProvider": true,
        "renameProvider": { "prepareProvider": true },
        "completionProvider": { "triggerCharacters": COMPLETION_TRIGGERS, "resolveProvider": true }
    })
}

/// tsserver `scriptKindName` for an LSP language id.
pub fn script_kind(language_id: &str) -> Option<&'static str> {
    match language_id {
        "typescript" => Some("TS"),
        "typescriptreact" => Some("TSX"),
        "javascript" => Some("JS"),
        "javascriptreact" => Some("JSX"),
        _ => None,
    }
}

fn text_document_file(params: &Value) -> Result<String, String> {
    let uri = params
        .pointer("/textDocument/uri")
        .and_then(Value::as_str)
        .ok_or("missing textDocument.uri")?;
    Ok(uri::key_for_uri(uri))
}

fn position(params: &Value) -> Result<Pos, String> {
    params
        .get("position")
        .and_then(Pos::from_lsp)
        .ok_or_else(|| "missing position".to_string())
}

fn file_location(file: &str, pos: Pos) -> Value {
    json!({ "file": file, "line": pos.line + 1, "offset": pos.character + 1 })
}

/// Translates an LSP request. `None`: not supported on this backend. `Err`: bad params.
pub fn lsp_to_ts(method: &str, params: &Value) -> Option<Result<Translated, String>> {
    let simple = |command: &'static str, kind: TsReqKind| -> Result<Translated, String> {
        let file = text_document_file(params)?;
        let pos = position(params)?;
        Ok(Translated {
            command,
            args: file_location(&file, pos),
            kind,
        })
    };
    Some(match method {
        "textDocument/hover" => simple("quickinfo", TsReqKind::Hover),
        "textDocument/definition" => simple("definition", TsReqKind::Locations),
        "textDocument/typeDefinition" => simple("typeDefinition", TsReqKind::Locations),
        "textDocument/implementation" => simple("implementation", TsReqKind::Locations),
        "textDocument/references" => {
            let include_declaration = params
                .pointer("/context/includeDeclaration")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            simple(
                "references",
                TsReqKind::References {
                    include_declaration,
                },
            )
        }
        "textDocument/prepareRename" => simple("rename", TsReqKind::PrepareRename),
        "textDocument/rename" => (|| {
            let new_name = params
                .get("newName")
                .and_then(Value::as_str)
                .ok_or("missing newName")?
                .to_string();
            let file = text_document_file(params)?;
            let pos = position(params)?;
            let mut args = file_location(&file, pos);
            args["findInComments"] = json!(false);
            args["findInStrings"] = json!(false);
            Ok(Translated {
                command: "rename",
                args,
                kind: TsReqKind::Rename { new_name },
            })
        })(),
        "textDocument/completion" => (|| {
            let file = text_document_file(params)?;
            let pos = position(params)?;
            let mut args = file_location(&file, pos);
            args["includeExternalModuleExports"] = json!(true);
            args["includeInsertTextCompletions"] = json!(true);
            if let Some(ctx) = params.get("context") {
                if let Some(k) = ctx.get("triggerKind").and_then(Value::as_u64) {
                    args["triggerKind"] = json!(k);
                }
                if let Some(c) = ctx.get("triggerCharacter").and_then(Value::as_str)
                    && COMPLETION_TRIGGERS.contains(&c)
                {
                    args["triggerCharacter"] = json!(c);
                }
            }
            Ok(Translated {
                command: "completionInfo",
                args,
                kind: TsReqKind::Completion { file, pos },
            })
        })(),
        "completionItem/resolve" => (|| {
            let data = params.get("data").ok_or("completion item has no data")?;
            let file = data
                .get("file")
                .and_then(Value::as_str)
                .ok_or("bad item data")?;
            let line = data
                .get("line")
                .and_then(Value::as_u64)
                .ok_or("bad item data")?;
            let offset = data
                .get("offset")
                .and_then(Value::as_u64)
                .ok_or("bad item data")?;
            let mut entry = Map::new();
            entry.insert(
                "name".into(),
                data.get("name").cloned().unwrap_or(Value::Null),
            );
            for k in ["source", "data"] {
                if let Some(v) = data.get(k).filter(|v| !v.is_null()) {
                    entry.insert(k.into(), v.clone());
                }
            }
            Ok(Translated {
                command: "completionEntryDetails",
                args: json!({ "file": file, "line": line, "offset": offset, "entryNames": [entry] }),
                kind: TsReqKind::CompletionResolve {
                    item: params.clone(),
                },
            })
        })(),
        _ => return None,
    })
}

/// Maps a tsserver response to an LSP result. `Err((code, message))` becomes an LSP error.
pub fn ts_to_lsp(
    kind: &TsReqKind,
    success: bool,
    body: Option<&Value>,
    message: Option<&str>,
    uri_for: &dyn Fn(&str) -> String,
) -> Result<Value, (i64, String)> {
    let failed = || {
        (
            crate::jsonrpc::codes::REQUEST_FAILED,
            message.unwrap_or("tsserver request failed").to_string(),
        )
    };
    match kind {
        // "No content available" is a normal answer for position-based queries.
        TsReqKind::Hover => Ok(if success {
            body.map(hover).unwrap_or(Value::Null)
        } else {
            Value::Null
        }),
        TsReqKind::Locations => Ok(if success {
            body.and_then(Value::as_array)
                .map(|locs| {
                    Value::Array(locs.iter().filter_map(|l| location(l, uri_for)).collect())
                })
                .unwrap_or(Value::Null)
        } else {
            Value::Null
        }),
        TsReqKind::References {
            include_declaration,
        } => {
            if !success {
                return Ok(Value::Null);
            }
            let refs = body.and_then(|b| b.get("refs")).and_then(Value::as_array);
            Ok(Value::Array(
                refs.into_iter()
                    .flatten()
                    .filter(|r| {
                        *include_declaration
                            || !r
                                .get("isDefinition")
                                .and_then(Value::as_bool)
                                .unwrap_or(false)
                    })
                    .filter_map(|r| location(r, uri_for))
                    .collect(),
            ))
        }
        TsReqKind::PrepareRename => {
            let info = body.and_then(|b| b.get("info")).filter(|_| success);
            match info {
                Some(i) if i.get("canRename").and_then(Value::as_bool) == Some(true) => {
                    let range = i.get("triggerSpan").and_then(ts_span_to_lsp_range);
                    Ok(match range {
                        Some(range) => json!({
                            "range": range,
                            "placeholder": i.get("displayName").cloned().unwrap_or(json!("")),
                        }),
                        None => Value::Null,
                    })
                }
                Some(i) => Err((
                    crate::jsonrpc::codes::REQUEST_FAILED,
                    i.get("localizedErrorMessage")
                        .and_then(Value::as_str)
                        .unwrap_or("You cannot rename this element.")
                        .to_string(),
                )),
                None => Ok(Value::Null),
            }
        }
        TsReqKind::Rename { new_name } => {
            if !success {
                return Err(failed());
            }
            let body = body.ok_or_else(failed)?;
            if !body
                .pointer("/info/canRename")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                let msg = body
                    .pointer("/info/localizedErrorMessage")
                    .and_then(Value::as_str)
                    .unwrap_or("You cannot rename this element.");
                return Err((crate::jsonrpc::codes::REQUEST_FAILED, msg.to_string()));
            }
            let mut changes = Map::new();
            for group in body
                .get("locs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(file) = group.get("file").and_then(Value::as_str) else {
                    continue;
                };
                let edits: Vec<Value> = group
                    .get("locs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|l| {
                        let range = ts_span_to_lsp_range(l)?;
                        let prefix = l.get("prefixText").and_then(Value::as_str).unwrap_or("");
                        let suffix = l.get("suffixText").and_then(Value::as_str).unwrap_or("");
                        Some(json!({ "range": range, "newText": format!("{prefix}{new_name}{suffix}") }))
                    })
                    .collect();
                changes.insert(uri_for(file), Value::Array(edits));
            }
            Ok(json!({ "changes": changes }))
        }
        TsReqKind::Completion { file, pos } => {
            if !success {
                return Ok(Value::Null);
            }
            Ok(completion_list(body, file, *pos))
        }
        TsReqKind::CompletionResolve { item } => {
            let mut item = item.clone();
            if success && let Some(details) = body.and_then(Value::as_array).and_then(|a| a.first())
            {
                resolve_item(&mut item, details);
            }
            Ok(item)
        }
    }
}

fn location(v: &Value, uri_for: &dyn Fn(&str) -> String) -> Option<Value> {
    let file = v.get("file")?.as_str()?;
    Some(json!({ "uri": uri_for(file), "range": ts_span_to_lsp_range(v)? }))
}

/// Joins tsserver `SymbolDisplayPart[]` (or passes a plain string through).
pub fn display_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

fn tags_markdown(tags: Option<&Value>) -> String {
    let mut out = String::new();
    for tag in tags.and_then(Value::as_array).into_iter().flatten() {
        let Some(name) = tag.get("name").and_then(Value::as_str) else {
            continue;
        };
        let text = tag.get("text").map(display_text).unwrap_or_default();
        out.push_str(&format!("\n\n*@{name}*"));
        if !text.is_empty() {
            out.push_str(" — ");
            out.push_str(&text);
        }
    }
    out
}

fn hover(body: &Value) -> Value {
    let display = body
        .get("displayString")
        .and_then(Value::as_str)
        .unwrap_or("");
    if display.is_empty() {
        return Value::Null;
    }
    let mut md = format!("```typescript\n{display}\n```");
    let doc = body
        .get("documentation")
        .map(display_text)
        .unwrap_or_default();
    if !doc.is_empty() {
        md.push_str("\n\n");
        md.push_str(&doc);
    }
    md.push_str(&tags_markdown(body.get("tags")));
    let mut out = json!({ "contents": { "kind": "markdown", "value": md } });
    if let Some(r) = ts_span_to_lsp_range(body) {
        out["range"] = r;
    }
    out
}

/// LSP `CompletionItemKind` for a tsserver `ScriptElementKind`.
pub fn completion_kind(kind: &str) -> u32 {
    match kind {
        "primitive type" | "keyword" => 14,
        "var" | "local var" | "let" | "parameter" | "alias" => 6,
        "const" | "using" | "await using" | "string" => 21,
        "property" | "getter" | "setter" | "accessor" => 10,
        "function" | "local function" => 3,
        "method" | "construct" | "call" | "index" => 2,
        "enum" => 13,
        "enum member" => 20,
        "module" | "external module name" | "script" => 9,
        "class" | "local class" | "type" => 7,
        "interface" => 8,
        "warning" => 1,
        "directory" => 19,
        "file" => 17,
        "type parameter" => 25,
        "constructor" => 4,
        _ => 10,
    }
}

fn completion_list(body: Option<&Value>, file: &str, pos: Pos) -> Value {
    let Some(body) = body else { return Value::Null };
    // Old servers return a bare entry array.
    let (entries, incomplete) = match body {
        Value::Array(a) => (a.as_slice(), false),
        _ => (
            body.get("entries")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            body.get("isIncomplete")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
    };
    let items: Vec<Value> = entries
        .iter()
        .filter_map(|e| {
            let name = e.get("name")?.as_str()?;
            let kind = e.get("kind").and_then(Value::as_str).unwrap_or("");
            let modifiers = e.get("kindModifiers").and_then(Value::as_str).unwrap_or("");
            let mut item = json!({
                "label": name,
                "kind": completion_kind(kind),
                "sortText": e.get("sortText").cloned().unwrap_or(json!("11")),
                "data": {
                    "file": file, "line": pos.line + 1, "offset": pos.character + 1, "name": name,
                    "source": e.get("source").cloned().unwrap_or(Value::Null),
                    "data": e.get("data").cloned().unwrap_or(Value::Null),
                },
            });
            let insert = e.get("insertText").and_then(Value::as_str);
            if let Some(span) = e.get("replacementSpan").and_then(ts_span_to_lsp_range) {
                item["textEdit"] = json!({ "range": span, "newText": insert.unwrap_or(name) });
            } else if let Some(t) = insert {
                item["insertText"] = json!(t);
            }
            if e.get("isSnippet").and_then(Value::as_bool).unwrap_or(false) {
                item["insertTextFormat"] = json!(2);
            }
            if let Some(f) = e.get("filterText") {
                item["filterText"] = f.clone();
            }
            if e.get("isRecommended")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                item["preselect"] = json!(true);
            }
            if modifiers.split(',').any(|m| m == "deprecated") {
                item["tags"] = json!([1]);
            }
            let source_display = e
                .get("sourceDisplay")
                .map(display_text)
                .filter(|s| !s.is_empty());
            let detail = e.pointer("/labelDetails/detail").and_then(Value::as_str);
            if source_display.is_some() || detail.is_some() {
                let mut ld = Map::new();
                if let Some(d) = detail {
                    ld.insert("detail".into(), json!(d));
                }
                if let Some(s) = source_display {
                    ld.insert("description".into(), json!(s));
                }
                item["labelDetails"] = Value::Object(ld);
            }
            Some(item)
        })
        .collect();
    json!({ "isIncomplete": incomplete, "items": items })
}

fn resolve_item(item: &mut Value, details: &Value) {
    let detail = details
        .get("displayParts")
        .map(display_text)
        .unwrap_or_default();
    if !detail.is_empty() {
        item["detail"] = json!(detail);
    }
    let mut doc = details
        .get("documentation")
        .map(display_text)
        .unwrap_or_default();
    doc.push_str(&tags_markdown(details.get("tags")));
    if !doc.trim().is_empty() {
        item["documentation"] = json!({ "kind": "markdown", "value": doc.trim_start() });
    }
    let file = item
        .pointer("/data/file")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let mut edits = Vec::new();
    for action in details
        .get("codeActions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for change in action
            .get("changes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if change.get("fileName").and_then(Value::as_str) != Some(file.as_str()) {
                continue;
            }
            for tc in change
                .get("textChanges")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let (Some(range), Some(text)) = (
                    ts_span_to_lsp_range(tc),
                    tc.get("newText").and_then(Value::as_str),
                ) {
                    edits.push(json!({ "range": range, "newText": text }));
                }
            }
        }
    }
    if !edits.is_empty() {
        item["additionalTextEdits"] = Value::Array(edits);
    }
}

/// tsserver diagnostics (from `*Diag` events) -> LSP diagnostics.
pub fn diagnostics_to_lsp(diags: &[Value], uri_for: &dyn Fn(&str) -> String) -> Vec<Value> {
    diags
        .iter()
        .filter_map(|d| {
            let range = ts_span_to_lsp_range(d)?;
            let severity = match d.get("category").and_then(Value::as_str) {
                Some("error") => 1,
                Some("warning") => 2,
                Some("suggestion") => 4,
                _ => 3,
            };
            let mut out = json!({
                "range": range,
                "severity": severity,
                "source": d.get("source").and_then(Value::as_str).unwrap_or("ts"),
                "message": d.get("text").and_then(Value::as_str).unwrap_or(""),
            });
            if let Some(code) = d.get("code") {
                out["code"] = code.clone();
            }
            let mut tags = Vec::new();
            if d.get("reportsUnnecessary").is_some_and(|v| v.as_bool().unwrap_or(true)) {
                tags.push(1);
            }
            if d.get("reportsDeprecated").is_some_and(|v| v.as_bool().unwrap_or(true)) {
                tags.push(2);
            }
            if !tags.is_empty() {
                out["tags"] = json!(tags);
            }
            let related: Vec<Value> = d
                .get("relatedInformation")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|r| {
                    let span = r.get("span")?;
                    Some(json!({
                        "location": { "uri": uri_for(span.get("file")?.as_str()?), "range": ts_span_to_lsp_range(span)? },
                        "message": r.get("message").and_then(Value::as_str).unwrap_or(""),
                    }))
                })
                .collect();
            if !related.is_empty() {
                out["relatedInformation"] = Value::Array(related);
            }
            Some(out)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri_for(p: &str) -> String {
        uri::path_to_uri(p)
    }

    fn pos_params(method_extra: Value) -> Value {
        let mut p = json!({ "textDocument": { "uri": "file:///w/a.ts" }, "position": { "line": 2, "character": 4 } });
        if let (Some(p), Some(e)) = (p.as_object_mut(), method_extra.as_object()) {
            for (k, v) in e {
                p.insert(k.clone(), v.clone());
            }
        }
        p
    }

    #[test]
    fn hover_round_trip() {
        let t = lsp_to_ts("textDocument/hover", &pos_params(json!({})))
            .unwrap()
            .unwrap();
        assert_eq!(t.command, "quickinfo");
        assert_eq!(t.args, json!({"file": "/w/a.ts", "line": 3, "offset": 5}));
        let body = json!({
            "displayString": "const x: number", "documentation": [{"text": "The x.", "kind": "text"}],
            "tags": [{"name": "deprecated", "text": "use y"}],
            "start": {"line": 3, "offset": 5}, "end": {"line": 3, "offset": 6}
        });
        let r = ts_to_lsp(&t.kind, true, Some(&body), None, &uri_for).unwrap();
        let md = r["contents"]["value"].as_str().unwrap();
        assert!(md.starts_with("```typescript\nconst x: number\n```"));
        assert!(md.contains("The x.") && md.contains("*@deprecated* — use y"));
        assert_eq!(r["range"]["start"], json!({"line": 2, "character": 4}));
        // No info at the position: null, not an error.
        assert_eq!(
            ts_to_lsp(
                &t.kind,
                false,
                None,
                Some("No content available."),
                &uri_for
            )
            .unwrap(),
            Value::Null
        );
    }

    #[test]
    fn definition_and_references() {
        let t = lsp_to_ts("textDocument/definition", &pos_params(json!({})))
            .unwrap()
            .unwrap();
        let body = json!([{ "file": "/w/b c.ts", "start": {"line": 1, "offset": 1}, "end": {"line": 1, "offset": 4} }]);
        let r = ts_to_lsp(&t.kind, true, Some(&body), None, &uri_for).unwrap();
        assert_eq!(r[0]["uri"], "file:///w/b%20c.ts");
        assert_eq!(r[0]["range"]["end"], json!({"line": 0, "character": 3}));

        let t = lsp_to_ts(
            "textDocument/references",
            &pos_params(json!({"context": {"includeDeclaration": false}})),
        )
        .unwrap()
        .unwrap();
        let body = json!({"refs": [
            { "file": "/w/a.ts", "start": {"line": 1, "offset": 7}, "end": {"line": 1, "offset": 8}, "isDefinition": true },
            { "file": "/w/a.ts", "start": {"line": 3, "offset": 5}, "end": {"line": 3, "offset": 6}, "isDefinition": false }
        ]});
        let r = ts_to_lsp(&t.kind, true, Some(&body), None, &uri_for).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 1);
    }

    #[test]
    fn completion_and_resolve() {
        let params = pos_params(json!({"context": {"triggerKind": 2, "triggerCharacter": "."}}));
        let t = lsp_to_ts("textDocument/completion", &params)
            .unwrap()
            .unwrap();
        assert_eq!(t.command, "completionInfo");
        assert_eq!(t.args["triggerCharacter"], ".");
        let body = json!({ "isIncomplete": false, "entries": [
            { "name": "toFixed", "kind": "method", "kindModifiers": "declare", "sortText": "11" },
            { "name": "old", "kind": "property", "kindModifiers": "deprecated", "sortText": "11",
              "insertText": "?.old", "replacementSpan": {"start": {"line": 3, "offset": 4}, "end": {"line": 3, "offset": 5}} },
            { "name": "readFile", "kind": "function", "sortText": "16", "source": "/n/fs.d.ts",
              "sourceDisplay": [{"text": "node:fs", "kind": "text"}], "hasAction": true, "data": {"exportName": "readFile"} }
        ]});
        let r = ts_to_lsp(&t.kind, true, Some(&body), None, &uri_for).unwrap();
        let items = r["items"].as_array().unwrap();
        assert_eq!(items[0]["kind"], 2);
        assert_eq!(items[1]["textEdit"]["newText"], "?.old");
        assert_eq!(items[1]["tags"], json!([1]));
        assert_eq!(items[2]["labelDetails"]["description"], "node:fs");
        assert_eq!(items[2]["data"]["line"], 3);

        let t = lsp_to_ts("completionItem/resolve", &items[2])
            .unwrap()
            .unwrap();
        assert_eq!(t.command, "completionEntryDetails");
        assert_eq!(t.args["entryNames"][0]["source"], "/n/fs.d.ts");
        assert_eq!(t.args["entryNames"][0]["data"]["exportName"], "readFile");
        let details = json!([{
            "name": "readFile", "displayParts": [{"text": "function readFile(): void"}],
            "documentation": "Reads.", "codeActions": [{ "description": "Add import", "changes": [
                { "fileName": "/w/a.ts", "textChanges": [{ "start": {"line": 1, "offset": 1}, "end": {"line": 1, "offset": 1}, "newText": "import { readFile } from \"node:fs\";\n" }] },
                { "fileName": "/w/other.ts", "textChanges": [] }
            ]}]
        }]);
        let r = ts_to_lsp(&t.kind, true, Some(&details), None, &uri_for).unwrap();
        assert_eq!(r["detail"], "function readFile(): void");
        assert_eq!(r["documentation"]["value"], "Reads.");
        assert_eq!(r["additionalTextEdits"].as_array().unwrap().len(), 1);
        assert_eq!(r["label"], "readFile");
    }

    #[test]
    fn rename_success_and_refusal() {
        let t = lsp_to_ts("textDocument/rename", &pos_params(json!({"newName": "z"})))
            .unwrap()
            .unwrap();
        let body = json!({ "info": {"canRename": true}, "locs": [
            { "file": "/w/a.ts", "locs": [{ "start": {"line": 1, "offset": 7}, "end": {"line": 1, "offset": 8} },
                                          { "start": {"line": 2, "offset": 10}, "end": {"line": 2, "offset": 11}, "prefixText": "x: " }] }
        ]});
        let r = ts_to_lsp(&t.kind, true, Some(&body), None, &uri_for).unwrap();
        let edits = &r["changes"]["file:///w/a.ts"];
        assert_eq!(edits[1]["newText"], "x: z");
        let refused =
            json!({ "info": {"canRename": false, "localizedErrorMessage": "nope"}, "locs": [] });
        let p = lsp_to_ts("textDocument/prepareRename", &pos_params(json!({})))
            .unwrap()
            .unwrap();
        let ok = json!({ "info": {"canRename": true, "displayName": "XXbY",
            "triggerSpan": {"start": {"line": 1, "offset": 5}, "end": {"line": 1, "offset": 9}}}, "locs": [] });
        let r = ts_to_lsp(&p.kind, true, Some(&ok), None, &uri_for).unwrap();
        assert_eq!(r["placeholder"], "XXbY");
        assert_eq!(r["range"]["end"], json!({"line": 0, "character": 8}));
        assert!(ts_to_lsp(&p.kind, true, Some(&refused), None, &uri_for).is_err());
        assert_eq!(
            ts_to_lsp(&t.kind, true, Some(&refused), None, &uri_for)
                .unwrap_err()
                .1,
            "nope"
        );
    }

    #[test]
    fn unsupported_and_bad_params() {
        assert!(lsp_to_ts("textDocument/codeLens", &json!({})).is_none());
        assert!(
            lsp_to_ts("textDocument/hover", &json!({}))
                .unwrap()
                .is_err()
        );
        assert!(
            lsp_to_ts("completionItem/resolve", &json!({"label": "x"}))
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn diagnostics() {
        let d = json!([
            { "start": {"line": 1, "offset": 7}, "end": {"line": 1, "offset": 8}, "text": "Type 'string' is not assignable to type 'number'.", "code": 2322, "category": "error" },
            { "start": {"line": 2, "offset": 1}, "end": {"line": 2, "offset": 4}, "text": "unused", "code": 6133, "category": "suggestion", "reportsUnnecessary": {},
              "relatedInformation": [{ "span": {"file": "/w/b.ts", "start": {"line": 1, "offset": 1}, "end": {"line": 1, "offset": 2}}, "message": "here" }] },
            { "bad": true }
        ]);
        let l = diagnostics_to_lsp(d.as_array().unwrap(), &uri_for);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0]["severity"], 1);
        assert_eq!(l[0]["code"], 2322);
        assert_eq!(l[0]["range"]["start"], json!({"line": 0, "character": 6}));
        assert_eq!(l[1]["severity"], 4);
        assert_eq!(l[1]["tags"], json!([1]));
        assert_eq!(
            l[1]["relatedInformation"][0]["location"]["uri"],
            "file:///w/b.ts"
        );
    }
}
