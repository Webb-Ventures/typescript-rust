//! Actions and their edits, in byte offsets until [`Action::to_lsp`].

use super::LspLines;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use ts_r_checker::Checker;

/// One replacement in a file (byte offsets).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub start: u32,
    pub end: u32,
    pub text: String,
}

impl Edit {
    pub fn new(start: u32, end: u32, text: impl Into<String>) -> Edit {
        Edit {
            start,
            end,
            text: text.into(),
        }
    }

    pub fn insert(at: u32, text: impl Into<String>) -> Edit {
        Edit::new(at, at, text)
    }

    pub fn delete(start: u32, end: u32) -> Edit {
        Edit::new(start, end, "")
    }
}

/// Edits of one file.
pub type FileEdits = Vec<Edit>;

/// A code action: title, kind and a workspace edit.
#[derive(Clone, Debug, Default)]
pub struct Action {
    pub title: String,
    pub kind: String,
    pub preferred: bool,
    /// The LSP diagnostic it fixes.
    pub diagnostic: Option<Value>,
    /// tsserver's fix id (`fixMissingImport`, ...): fixes with one have a fix-all.
    pub fix_id: Option<&'static str>,
    /// This is the fix-all action of `fix_id`.
    pub fix_all: bool,
    /// tsserver's refactor and action name (`Extract Symbol/constant_scope_0`).
    pub refactor: Option<String>,
    /// Edits by file path (absolute, as in the program).
    pub edits: BTreeMap<String, FileEdits>,
    /// Files to create: path and text.
    pub creates: BTreeMap<String, String>,
    /// File renames: old path, new path (done after the edits).
    pub renames: Vec<(String, String)>,
    /// No edit yet: the client asks the user (the target of Move to file) and asks again.
    pub interactive: bool,
}

impl Action {
    pub fn new(title: impl Into<String>, kind: impl Into<String>) -> Action {
        Action {
            title: title.into(),
            kind: kind.into(),
            ..Action::default()
        }
    }

    /// Adds edits to `path` (they must not overlap edits already there).
    pub fn edit(&mut self, path: &str, edits: FileEdits) {
        if edits.is_empty() {
            return;
        }
        self.edits
            .entry(path.to_string())
            .or_default()
            .extend(edits);
    }

    pub fn is_empty(&self) -> bool {
        !self.interactive
            && self.edits.values().all(Vec::is_empty)
            && self.creates.is_empty()
            && self.renames.is_empty()
    }

    /// The LSP `CodeAction`. Texts of edited files come from the checker's program.
    pub fn to_lsp(&self, c: &Checker<'_>) -> Value {
        let mut a = json!({
            "title": self.title,
            "kind": self.kind,
        });
        if !self.interactive {
            a["edit"] = self.workspace_edit(c);
        }
        if self.preferred {
            a["isPreferred"] = json!(true);
        }
        if let Some(d) = &self.diagnostic {
            a["diagnostics"] = json!([d]);
        }
        if let Some(id) = self.fix_id {
            a["data"] = json!({"fixId": id, "fixAll": self.fix_all});
        }
        if let Some(r) = &self.refactor {
            a["data"] = json!({"refactor": r});
            if self.interactive {
                a["data"]["interactive"] = json!(true);
            }
        }
        a
    }

    /// An LSP `WorkspaceEdit`: `changes` when only existing files are edited, else
    /// `documentChanges` (creates and renames first, as clients apply them in order).
    pub fn workspace_edit(&self, c: &Checker<'_>) -> Value {
        let mut per_file: Vec<(String, Vec<Value>)> = Vec::new();
        for (path, edits) in &self.edits {
            let text = c
                .files
                .iter()
                .find(|f| f.path == *path)
                .map(|f| f.text.as_str())
                .unwrap_or("");
            per_file.push((crate::uri::path_to_uri(path), lsp_edits(text, edits)));
        }
        if self.creates.is_empty() && self.renames.is_empty() {
            let mut changes = serde_json::Map::new();
            for (uri, edits) in per_file {
                changes.insert(uri, Value::Array(edits));
            }
            return json!({ "changes": changes });
        }
        let mut doc = Vec::new();
        for (path, text) in &self.creates {
            let uri = crate::uri::path_to_uri(path);
            doc.push(json!({"kind": "create", "uri": uri, "options": {"ignoreIfExists": true}}));
            doc.push(json!({
                "textDocument": {"uri": uri, "version": null},
                "edits": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "newText": text}],
            }));
        }
        for (uri, edits) in per_file {
            doc.push(json!({"textDocument": {"uri": uri, "version": null}, "edits": edits}));
        }
        for (from, to) in &self.renames {
            doc.push(json!({
                "kind": "rename",
                "oldUri": crate::uri::path_to_uri(from),
                "newUri": crate::uri::path_to_uri(to),
            }));
        }
        json!({ "documentChanges": doc })
    }
}

/// LSP `TextEdit`s for byte edits of `text`, in source order. An insertion at the start of
/// a replacement joins it (clients may apply edits at one position in either order).
pub fn lsp_edits(text: &str, edits: &[Edit]) -> Vec<Value> {
    let lines = LspLines::new(text);
    let mut sorted: Vec<Edit> = edits.to_vec();
    sorted.sort_by_key(|e| (e.start, e.end));
    let mut merged: Vec<Edit> = Vec::with_capacity(sorted.len());
    for e in sorted {
        if let Some(prev) = merged.last_mut()
            && prev.start == prev.end
            && prev.start == e.start
        {
            prev.text.push_str(&e.text);
            prev.end = e.end;
            continue;
        }
        merged.push(e);
    }
    merged
        .into_iter()
        .map(|e| json!({"range": lines.range(text, e.start, e.end), "newText": e.text}))
        .collect()
}
