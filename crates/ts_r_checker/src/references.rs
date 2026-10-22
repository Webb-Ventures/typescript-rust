//! `/// <reference path="..." />` directives that name no program file: program
//! diagnostics, reported even in files with syntax errors.
//!
//! Rules observed from tsc 7.0.2 (at the path value, the text between the quotes; the
//! message repeats the path as written):
//! - A path with a TypeScript extension (`.ts`, `.tsx`, `.d.ts`, `.cts`, `.d.cts`, `.mts`,
//!   `.d.mts`) that is not found: TS6053. One naming the file itself: TS1006.
//! - A JavaScript extension without `allowJs`: TS6504.
//! - Another extension, or an empty path: TS6054, listing the supported extensions.
//! - No extension, and no file with one of the supported extensions appended: TS6231.
//! - Nothing under `noResolve`.
//!
//! The loader (`program::reference_target`) loads every file a reference finds, so "not
//! found" is "no candidate path is a program file". JavaScript and JSON targets are not
//! loaded through references; they are not reported (their existence is unknown here).

use rustc_hash::FxHashSet;
use ts_r_ast::{CommentKind, Node};
use ts_r_diagnostics::Diagnostic;
use ts_r_span::Span;

use crate::diag::{dg, msg};
use crate::program::SourceFile;

/// TypeScript extensions in tsc's resolution order.
pub(crate) const TS_EXTENSIONS: [&str; 7] =
    [".ts", ".tsx", ".d.ts", ".cts", ".d.cts", ".mts", ".d.mts"];
const JS_EXTENSIONS: [&str; 4] = [".js", ".jsx", ".cjs", ".mjs"];

/// A file's path-reference diagnostics against the full program's paths. The caller sets
/// their file id; no semantic checking or parsing of the other files is needed.
pub fn reference_diagnostics(
    f: &SourceFile,
    options: &ts_r_config::CompilerOptions,
    paths: &FxHashSet<&str>,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    // `noResolve`: references are not followed.
    if f.is_lib || f.is_external || f.is_json() || options.no_resolve() == Some(true) {
        return out;
    }
    let allow_js = options.effective().allow_js();
    for (span, written) in path_references(f) {
        // Backslashes are separators (the message shows them as `/`).
        let shown = ts_r_config::path::normalize_slashes(written);
        let value = shown.as_str();
        let dir = ts_r_config::path::directory_of(&f.path);
        let joined = ts_r_config::path::normalize(&ts_r_config::path::combine(dir, value));
        let base = value.rsplit('/').next().unwrap_or(value);
        let d = if TS_EXTENSIONS.iter().any(|e| value.ends_with(e)) {
            if joined == f.path {
                Diagnostic::new(
                    msg(dg::A_FILE_CANNOT_HAVE_A_REFERENCE_TO_ITSELF_1006),
                    span,
                    &[],
                )
            } else if paths.contains(joined.as_str()) {
                continue;
            } else {
                Diagnostic::new(msg(dg::FILE_0_NOT_FOUND_6053), span, &[value])
            }
        } else if JS_EXTENSIONS.iter().any(|e| value.ends_with(e)) {
            if allow_js {
                continue;
            }
            Diagnostic::new(
                msg(dg::FILE_0_IS_A_JAVASCRIPT_FILE_DID_YOU_MEAN_TO_ENABLE_THE_ALLOWJS_OPTION_6504),
                span,
                &[value],
            )
        } else if value.ends_with(".json") {
            continue;
        } else if value.is_empty() || base.contains('.') {
            let list = extension_list(allow_js);
            Diagnostic::new(
                msg(dg::FILE_0_HAS_AN_UNSUPPORTED_EXTENSION_THE_ONLY_SUPPORTED_EXTENSIONS_ARE_1_6054),
                span,
                &[value, &list],
            )
        } else {
            if TS_EXTENSIONS
                .iter()
                .any(|e| paths.contains(format!("{joined}{e}").as_str()))
            {
                continue;
            }
            let list = extension_list(allow_js);
            Diagnostic::new(
                msg(dg::COULD_NOT_RESOLVE_THE_PATH_0_WITH_THE_EXTENSIONS_1_6231),
                span,
                &[value, &list],
            )
        };
        out.push(d);
    }
    // `/// <reference lib="..." />` naming no lib: TS2727 with the closest lib name, else
    // TS2726 (observed: `tsc/libraryResolution/unknown-lib`).
    for (span, value) in lib_references(f) {
        if ts_r_config::lib_file_name(value).is_some() {
            continue;
        }
        let names = ts_r_config::enums::LIBS.iter().map(|(n, _)| *n);
        let d = match crate::relate::spelling_suggestion(value, names) {
            Some(s) => Diagnostic::new(
                msg(dg::CANNOT_FIND_LIB_DEFINITION_FOR_0_DID_YOU_MEAN_1_2727),
                span,
                &[value, &s],
            ),
            None => Diagnostic::new(
                msg(dg::CANNOT_FIND_LIB_DEFINITION_FOR_0_2726),
                span,
                &[value],
            ),
        };
        out.push(d);
    }
    out
}

/// TS2688 for each `/// <reference types="..." />` of `f` whose name is in `missing` (the
/// names the loader could not resolve from `f`), at the name.
pub fn type_reference_diagnostics(f: &SourceFile, missing: &[&str]) -> Vec<Diagnostic> {
    if missing.is_empty() {
        return Vec::new();
    }
    let ast = &f.ast;
    let first_stmt = match ast.node(ast.root) {
        Node::Program { stmts } => ast.list(*stmts).first().map(|&s| ast.span(s).start),
        _ => None,
    }
    .unwrap_or(u32::MAX);
    let mut out = Vec::new();
    for c in &ast.comments {
        if c.span.start >= first_stmt {
            break;
        }
        if c.kind != CommentKind::Line {
            continue;
        }
        let t = c.span.text(&f.text);
        if let Some((at, v)) = attribute_value(t, "types")
            && missing.contains(&v)
        {
            let start = c.span.start + at as u32;
            out.push(Diagnostic::new(
                msg(dg::CANNOT_FIND_TYPE_DEFINITION_FILE_FOR_0_2688),
                Span::new(start, start + v.len() as u32),
                &[v],
            ));
        }
    }
    out
}

/// The `/// <reference lib="..." />` directives in the leading comments of `f`: the span
/// of each value (between the quotes) and the value.
fn lib_references(f: &SourceFile) -> Vec<(Span, &str)> {
    let ast = &f.ast;
    let first_stmt = match ast.node(ast.root) {
        Node::Program { stmts } => ast.list(*stmts).first().map(|&s| ast.span(s).start),
        _ => None,
    }
    .unwrap_or(u32::MAX);
    let mut out = Vec::new();
    for c in &ast.comments {
        if c.span.start >= first_stmt {
            break;
        }
        if c.kind != CommentKind::Line {
            continue;
        }
        if let Some((at, v)) = attribute_value(c.span.text(&f.text), "lib") {
            let start = c.span.start + at as u32;
            out.push((Span::new(start, start + v.len() as u32), v));
        }
    }
    out
}

/// `'.ts', '.tsx', ...` as tsc lists the supported extensions.
fn extension_list(allow_js: bool) -> String {
    let exts: &[&str] = if allow_js {
        &[
            ".ts", ".tsx", ".d.ts", ".js", ".jsx", ".cts", ".d.cts", ".cjs", ".mts", ".d.mts",
            ".mjs",
        ]
    } else {
        &TS_EXTENSIONS
    };
    exts.iter()
        .map(|e| format!("'{e}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `/// <reference path="..." />` directives in the leading comments of `f`: the span
/// of each value (between the quotes) and the value.
fn path_references(f: &SourceFile) -> Vec<(Span, &str)> {
    let ast = &f.ast;
    let first_stmt = match ast.node(ast.root) {
        Node::Program { stmts } => ast.list(*stmts).first().map(|&s| ast.span(s).start),
        _ => None,
    }
    .unwrap_or(u32::MAX);
    let mut out = Vec::new();
    for c in &ast.comments {
        if c.span.start >= first_stmt {
            break;
        }
        if c.kind != CommentKind::Line {
            continue;
        }
        let t = c.span.text(&f.text);
        // Only `path` directives (`types` and `lib` ones are reported elsewhere).
        if crate::program::reference_attribute(t, "types").is_some()
            || crate::program::reference_attribute(t, "lib").is_some()
        {
            continue;
        }
        if let Some((at, v)) = attribute_value(t, "path") {
            let start = c.span.start + at as u32;
            out.push((Span::new(start, start + v.len() as u32), v));
        }
    }
    out
}

/// The offset in `text` and the value of attribute `key` of a `/// <reference .../>`.
fn attribute_value<'t>(text: &'t str, key: &str) -> Option<(usize, &'t str)> {
    let rest = text.strip_prefix("///")?.trim_start();
    let s = rest.strip_prefix("<reference")?;
    let s_at = text.len() - s.len();
    let mut i = 0;
    while let Some(p) = s[i..].find(key) {
        let at = i + p;
        let before_ok = at == 0 || s.as_bytes()[at - 1].is_ascii_whitespace();
        let after = &s[at + key.len()..];
        let after_t = after.trim_start();
        if before_ok && let Some(eq) = after_t.strip_prefix('=') {
            let val = eq.trim_start();
            let q = val.chars().next()?;
            if q == '"' || q == '\'' {
                let body = &val[1..];
                let end = body.find(q)?;
                let off = s_at + (s.len() - body.len());
                return Some((off, &body[..end]));
            }
        }
        i = at + key.len();
    }
    None
}
