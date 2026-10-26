//! Language features over the checker. Requests run on the project thread inside a check
//! session; positions arrive as LSP (UTF-16) and are turned into byte offsets here, and every
//! range goes back out through [`LspLines`].

pub mod actions;
mod aliases;
mod class_items;
mod complete;
mod constructor_refs;
mod definitions;
mod doc_source;
mod expected;
mod generic_context;
mod hover;
mod inherit;
mod jsdoc;
mod jsdoc_items;
mod jsx_items;
mod keywords;
pub mod locate;
mod multiline;
mod nav;
mod outline;
mod position;
mod quick_info;
mod ref_meaning;
mod signature;
mod signature_site;
mod strings;
mod type_args;

use super::convert::LspLines;
use crate::jsonrpc::codes;
use crate::text::Pos;
use serde_json::{Value, json};
use std::collections::HashMap;
use ts_r_checker::{Checker, Ck, Loc, ProgramFiles};
use ts_r_diagnostics::{Diagnostic, FileId};

/// Workers kept per session (each has checked one file).
pub const KEPT_CKS: usize = 4;

/// A worker that has checked one file in the current session, with that file's
/// diagnostics.
#[derive(Debug)]
pub struct KeptCk<'c, 'p> {
    pub file: FileId,
    pub ck: Ck<'c, 'p>,
    pub diags: Vec<Diagnostic>,
}

/// Index in `cks` of a worker that has checked `file` in this session (checking it now if
/// needed; the least recently used worker is reused).
pub fn ensure_ck<'c, 'p>(
    checker: &'c Checker<'p>,
    cks: &mut Vec<KeptCk<'c, 'p>>,
    file: FileId,
) -> usize {
    if let Some(i) = cks.iter().position(|k| k.file == file) {
        let e = cks.remove(i);
        cks.push(e);
        return cks.len() - 1;
    }
    let mut ck = if cks.len() >= KEPT_CKS {
        cks.remove(0).ck
    } else {
        Ck::new(checker, file)
    };
    let diags = ck.check_source_file(file);
    cks.push(KeptCk { file, ck, diags });
    cks.len() - 1
}

/// What a request runs against.
#[derive(Debug)]
pub struct Ctx<'c, 'p> {
    pub program: &'p ProgramFiles,
    pub checker: &'c Checker<'p>,
    pub file: FileId,
}

type Answer = Result<Value, (i64, String)>;

/// Answers one request. `cks` must hold a worker for `cx.file` (the last one).
pub fn handle<'c, 'p>(
    cx: &Ctx<'c, 'p>,
    cks: &mut Vec<KeptCk<'c, 'p>>,
    method: &str,
    params: &Value,
) -> Answer {
    if method == "textDocument/codeAction"
        && let Some(v) = actions::organize_only(cx, params)
    {
        return Ok(v);
    }
    let k = ensure_ck(cx.checker, cks, cx.file);
    let ck = &mut cks[k].ck;
    let text = cx.checker.file(cx.file).text.as_str();
    let lines = LspLines::new(text);
    let offset = super::position(params).map(|p: Pos| lines.offset(text, p));
    let at =
        |what: &str| offset.ok_or_else(|| (codes::INVALID_PARAMS, format!("{what}: no position")));
    Ok(match method {
        "textDocument/hover" => hover::hover(cx, ck, &lines, at(method)?),
        "textDocument/definition" => nav::definition(cx, ck, at(method)?),
        "textDocument/typeDefinition" => nav::type_definition(cx, ck, at(method)?),
        "textDocument/references" => {
            let include = params["context"]["includeDeclaration"]
                .as_bool()
                .unwrap_or(true);
            let refs = nav::references(cx, ck, at(method)?, nav::Scope::Project);
            nav::locations(
                cx,
                refs.iter()
                    .filter(|r| include || !r.is_decl)
                    .map(|r| r.loc_span()),
            )
        }
        "textDocument/documentHighlight" => {
            let offset = at(method)?;
            let d = keywords::Doc::new(cx.checker, cx.file);
            if let Some(mut spans) = keywords::keyword_highlights(&d, offset) {
                spans.sort_unstable();
                spans.dedup();
                let out = spans
                    .iter()
                    .map(|&(s, e)| json!({"range": lines.range(text, s, e), "kind": 2}))
                    .collect();
                return Ok(Value::Array(out));
            }
            let refs = nav::references(cx, ck, offset, nav::Scope::File);
            let out: Vec<Value> = refs
                .iter()
                .map(|r| {
                    json!({
                        "range": lines.range(text, r.start, r.end),
                        "kind": if r.is_write { 3 } else { 2 },
                    })
                })
                .collect();
            Value::Array(out)
        }
        "textDocument/prepareRename" => nav::prepare_rename(cx, ck, &lines, at(method)?)?,
        "textDocument/rename" => {
            let new_name = params["newName"]
                .as_str()
                .ok_or((codes::INVALID_PARAMS, "rename: no newName".to_string()))?;
            nav::rename(cx, ck, at(method)?, new_name)?
        }
        "textDocument/completion" => complete::completion(cx, ck, at(method)?),
        "completionItem/resolve" => complete::resolve(cx, ck, params),
        "textDocument/signatureHelp" => signature::signature_help(cx, ck, at(method)?),
        "textDocument/documentSymbol" => outline::document_symbols(cx, &lines),
        "textDocument/semanticTokens/full" => outline::semantic_tokens(cx, ck, &lines),
        "ts-r/moveToFileSuggestions" => actions::move_suggestions(cx, params),
        "textDocument/codeAction" => {
            let diags = std::mem::take(&mut cks[k].diags);
            let v = actions::code_actions(cx, &mut cks[k].ck, &diags, params);
            cks[k].diags = diags;
            v
        }
        _ => {
            return Err((
                codes::METHOD_NOT_FOUND,
                format!("{method} is not supported by the ts-r backend"),
            ));
        }
    })
}

/// Per-file line tables for building locations in many files.
#[derive(Debug, Default)]
pub struct LinesCache {
    map: HashMap<FileId, LspLines>,
}

impl LinesCache {
    /// LSP `Location` of a byte range in `file`.
    pub fn location(&mut self, checker: &Checker<'_>, file: FileId, start: u32, end: u32) -> Value {
        let f = checker.file(file);
        let lines = self
            .map
            .entry(file)
            .or_insert_with(|| LspLines::new(&f.text));
        json!({
            "uri": crate::uri::path_to_uri(&f.path),
            "range": lines.range(&f.text, start, end),
        })
    }

    pub fn range(&mut self, checker: &Checker<'_>, file: FileId, start: u32, end: u32) -> Value {
        let f = checker.file(file);
        let lines = self
            .map
            .entry(file)
            .or_insert_with(|| LspLines::new(&f.text));
        lines.range(&f.text, start, end)
    }
}

/// The byte span a declaration is shown at: its name, else the whole declaration.
pub fn decl_span(checker: &Checker<'_>, d: Loc) -> (u32, u32) {
    let ast = checker.ast(d.file);
    let n = ts_r_binder::decl_name_node(ast, d.node).unwrap_or(d.node);
    let s = ast.span(n);
    (s.start, s.end)
}

/// Semantic token legend (TypeScript's token types and modifiers).
pub fn semantic_legend() -> Value {
    json!({
        "tokenTypes": outline::TOKEN_TYPES,
        "tokenModifiers": outline::TOKEN_MODIFIERS,
    })
}
