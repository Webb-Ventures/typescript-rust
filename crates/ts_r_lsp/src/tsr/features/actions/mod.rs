//! Code actions on our checker (L4): organize imports, quick fixes for diagnostics and
//! refactors, as tsserver offers them. Everything here works in byte offsets; edits become LSP
//! `WorkspaceEdit`s (UTF-16 ranges) only in [`Action::to_lsp`].
//!
//! Behaviour comes from observing tsserver on the fixtures in `tests/fixtures/actions/`
//! (`tests/oracle/actions.js` records its output), never from its source.

mod edit;
mod exports;
mod extract_type;
mod fixes;
mod functions;
mod imports;
mod members;
mod organize;
mod refactor;
pub mod rename;
mod rewrite;
mod style;
mod unused;

pub use edit::{Action, FileEdits};
pub use organize::Mode as OrganizeMode;

use super::{Ctx, LspLines};
use crate::text::Pos;
use serde_json::Value;
use ts_r_checker::Ck;
use ts_r_diagnostics::Diagnostic;

/// Code action kinds the ts-r backend offers.
pub const KINDS: &[&str] = &[
    "quickfix",
    "refactor",
    "refactor.extract",
    "refactor.rewrite",
    "refactor.inline",
    "refactor.move",
    "source.organizeImports",
    "source.removeUnusedImports",
    "source.sortImports",
    "source.addMissingImports.ts",
    "source.removeUnused.ts",
];

/// What a `textDocument/codeAction` request asks for.
#[derive(Debug)]
pub struct Request<'a> {
    /// Selection (byte offsets in the file).
    pub start: u32,
    pub end: u32,
    /// `context.only` (empty: everything but source actions).
    pub only: Vec<String>,
    /// `context.diagnostics` of ours (`source: "ts-r"`), with their byte spans and codes.
    pub diagnostics: Vec<(u32, u32, u32, &'a Value)>,
    /// Every diagnostic of the file from the last check (for "fix all").
    pub file_diags: &'a [Diagnostic],
    /// The user asked (`context.triggerKind` 1 or absent: the refactor menu, Ctrl+.), not
    /// the editor on a cursor move (2). tsserver offers some refactors at a bare cursor only
    /// when invoked.
    pub invoked: bool,
    /// `context.tsr.targetFile`: the file chosen for "Move to file" (an interactive refactor;
    /// tsserver's `interactiveRefactorArguments.targetFile`), absolute.
    pub target_file: Option<String>,
    /// One level of indentation from the editor's settings (`context.tsr.format`:
    /// tsserver's `indentSize`/`convertTabsToSpaces` or LSP's `tabSize`/`insertSpaces`).
    pub indent: Option<String>,
    /// `context.tsr.interactive`: the client can ask the user for the target of Move to
    /// file (tsserver's `includeInteractiveActions`).
    pub interactive: bool,
}

impl Request<'_> {
    /// One level of indentation for new code: the editor's setting, else the file's.
    pub fn indent_unit(&self, text: &str) -> String {
        self.indent
            .clone()
            .unwrap_or_else(|| style::indent_unit(text))
    }

    /// Whether kind `k` is wanted: `only` lists `k` or a parent of it. Source actions are
    /// only returned when asked for (as tsserver's clients ask).
    pub fn wants(&self, k: &str) -> bool {
        if self.only.is_empty() {
            return !k.starts_with("source");
        }
        self.only.iter().any(|o| {
            k == o || (k.starts_with(o.as_str()) && k.as_bytes().get(o.len()) == Some(&b'.'))
        })
    }
}

/// `textDocument/codeAction`: LSP `CodeAction`s for the request.
pub fn code_actions(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    file_diags: &[Diagnostic],
    params: &Value,
) -> Value {
    let c = cx.checker;
    let text = c.file(cx.file).text.as_str();
    let lines = LspLines::new(text);
    let off = |v: &Value| Pos::from_lsp(v).map(|p| lines.offset(text, p));
    let start = off(&params["range"]["start"]).unwrap_or(0);
    let end = off(&params["range"]["end"]).unwrap_or(start).max(start);
    let only = only_kinds(params);
    let mut diagnostics = Vec::new();
    for d in params["context"]["diagnostics"]
        .as_array()
        .into_iter()
        .flatten()
    {
        // Ours, or the fallback TypeScript's (projects that failed the parity check).
        if d["source"] != "ts-r" && d["source"] != "ts" {
            continue;
        }
        let Some(code) = d["code"].as_u64() else {
            continue;
        };
        let (Some(s), Some(e)) = (off(&d["range"]["start"]), off(&d["range"]["end"])) else {
            continue;
        };
        diagnostics.push((s, e, code as u32, d));
    }
    let invoked = params["context"]["triggerKind"].as_u64() != Some(2);
    let target_file = params["context"]["tsr"]["targetFile"].as_str().map(|t| {
        if t.starts_with("file:") {
            crate::uri::uri_to_path(t).unwrap_or_else(|| t.to_string())
        } else {
            t.to_string()
        }
    });
    let req = Request {
        start,
        end,
        only,
        diagnostics,
        file_diags,
        invoked,
        target_file,
        indent: indent_option(&params["context"]["tsr"]["format"]),
        interactive: params["context"]["tsr"]["interactive"].as_bool() == Some(true),
    };
    let actions = collect(cx, ck, &req);
    Value::Array(actions.iter().map(|a| a.to_lsp(c)).collect())
}

/// Every action for a request, in the order they are offered: quick fixes (per diagnostic,
/// then fix-all), refactors, source actions.
pub fn collect(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, req: &Request<'_>) -> Vec<Action> {
    let mut out = Vec::new();
    if req.wants("quickfix") {
        fixes::quick_fixes(cx, ck, req, &mut out);
    }
    if req.wants("refactor") || req.only.iter().any(|o| o.starts_with("refactor.")) {
        refactor::refactors(cx, ck, req, &mut out);
    }
    organize_actions(cx, req, &mut out);
    if req.wants("source.addMissingImports.ts")
        && let Some(mut a) = fixes::add_all_missing_imports(cx, ck, req)
    {
        a.kind = "source.addMissingImports.ts".into();
        a.title = "Add all missing imports".into();
        out.push(a);
    }
    if req.wants("source.removeUnused.ts")
        && let Some(mut a) = fixes::remove_all_unused(cx, ck, req)
    {
        a.kind = "source.removeUnused.ts".into();
        a.title = "Remove all unused code".into();
        out.push(a);
    }
    out
}

/// `ts-r/moveToFileSuggestions` `{textDocument, range}` (tsserver's
/// `getMoveToRefactoringFileSuggestions`): `{newFileName, files}`, the path Move to a new
/// file would create and the files Move to file can target: the program's source files with
/// the same extension, outside `node_modules`, files of a directory before its
/// subdirectories. `null` when nothing at the range can move.
pub fn move_suggestions(cx: &Ctx<'_, '_>, params: &Value) -> Value {
    let c = cx.checker;
    let f = c.file(cx.file);
    let text = f.text.as_str();
    let lines = LspLines::new(text);
    let off = |v: &Value| Pos::from_lsp(v).map(|p| lines.offset(text, p));
    let start = off(&params["range"]["start"]).unwrap_or(0);
    let end = off(&params["range"]["end"]).unwrap_or(start).max(start);
    let req = Request {
        start,
        end,
        only: vec!["refactor.move.newFile".into()],
        diagnostics: Vec::new(),
        file_diags: &[],
        invoked: true,
        target_file: None,
        indent: None,
        interactive: false,
    };
    let Some(new_file) = refactor::new_file_target(cx, &req) else {
        return Value::Null;
    };
    let ext = |p: &str| -> &'static str {
        [
            ".d.ts", ".tsx", ".mts", ".cts", ".ts", ".jsx", ".mjs", ".cjs", ".js",
        ]
        .into_iter()
        .find(|x| p.ends_with(x))
        .unwrap_or("")
    };
    // Observed: code from a `.tsx`/`.jsx` file may also go to a `.ts`/`.js` file; every other
    // extension only lists its own.
    let mine = ext(&f.path);
    let allowed: &[&str] = match mine {
        ".tsx" => &[".tsx", ".ts"],
        ".jsx" => &[".jsx", ".js"],
        _ => std::slice::from_ref(&mine),
    };
    let mut files: Vec<&str> = c
        .files
        .iter()
        .filter(|o| {
            o.path != f.path
                && !o.is_lib
                && !o.is_external
                && !o.bound.is_declaration
                && !o.path.contains("/node_modules/")
                && allowed.contains(&ext(&o.path))
        })
        .map(|o| o.path.as_str())
        .collect();
    files.sort_by(|a, b| tree_order(a, b));
    serde_json::json!({"newFileName": new_file, "files": files})
}

/// Path order with a directory's files before its subdirectories.
fn tree_order(a: &str, b: &str) -> std::cmp::Ordering {
    let pa: Vec<&str> = a.split('/').collect();
    let pb: Vec<&str> = b.split('/').collect();
    for i in 0..pa.len().min(pb.len()) {
        if pa[i] != pb[i] {
            let a_file = i + 1 == pa.len();
            let b_file = i + 1 == pb.len();
            return b_file.cmp(&a_file).then_with(|| pa[i].cmp(pb[i]));
        }
    }
    pa.len().cmp(&pb.len())
}

/// Organize kinds with their modes and titles.
const ORGANIZE: [(&str, OrganizeMode, &str); 3] = [
    (
        "source.organizeImports",
        OrganizeMode::All,
        "Organize Imports",
    ),
    (
        "source.removeUnusedImports",
        OrganizeMode::RemoveUnused,
        "Remove Unused Imports",
    ),
    (
        "source.sortImports",
        OrganizeMode::SortAndCombine,
        "Sort Imports",
    ),
];

fn organize_actions(cx: &Ctx<'_, '_>, req: &Request<'_>, out: &mut Vec<Action>) {
    for (kind, mode, title) in ORGANIZE {
        if req.wants(kind) {
            let mut a = Action::new(title, kind);
            a.edit(&cx.checker.file(cx.file).path, organize::organize(cx, mode));
            out.push(a);
        }
    }
}

/// One indentation level from format options (tsserver's or LSP's names).
fn indent_option(f: &Value) -> Option<String> {
    let size = f["indentSize"]
        .as_u64()
        .or_else(|| f["tabSize"].as_u64())?
        .clamp(1, 16) as usize;
    let spaces = f["convertTabsToSpaces"]
        .as_bool()
        .or_else(|| f["insertSpaces"].as_bool())
        .unwrap_or(true);
    Some(if spaces {
        " ".repeat(size)
    } else {
        "\t".into()
    })
}

fn only_kinds(params: &Value) -> Vec<String> {
    params["context"]["only"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

/// The answer to a request that only asks for organize-imports kinds (what editors send on
/// save): it needs the binder, not a checked file, so it skips the check.
pub fn organize_only(cx: &Ctx<'_, '_>, params: &Value) -> Option<Value> {
    let only = only_kinds(params);
    if only.is_empty() || !only.iter().all(|o| ORGANIZE.iter().any(|(k, ..)| k == o)) {
        return None;
    }
    let req = Request {
        start: 0,
        end: 0,
        only,
        diagnostics: Vec::new(),
        file_diags: &[],
        invoked: true,
        target_file: None,
        indent: None,
        interactive: false,
    };
    let mut out = Vec::new();
    organize_actions(cx, &req, &mut out);
    Some(Value::Array(
        out.iter().map(|a| a.to_lsp(cx.checker)).collect(),
    ))
}

/// Organize imports of the request's file (`mode` as tsserver's `organizeImports` modes),
/// as one action (empty edits when nothing changes).
pub fn organize_imports(cx: &Ctx<'_, '_>, mode: OrganizeMode) -> Action {
    let mut a = Action::new("Organize Imports", "source.organizeImports");
    a.edit(&cx.checker.file(cx.file).path, organize::organize(cx, mode));
    a
}
