//! `unicorn/require-module-specifiers`: an empty `{}` in an import or export, reported from
//! `{` to `}`.
//! - `import d, {} from "m"`: fixed by dropping the comma and braces (a space goes before a
//!   glued `from`).
//! - `import type {} from "m"`: fixed by deleting the statement (`;` when the previous
//!   statement would run on), unless it holds a comment.
//! - `import {} from "m"` and `export {} from "m"`: suggestions to delete the statement
//!   (not with a comment inside) and to make it a side-effect import (drop the braces and
//!   `from`; `export` becomes `import`).
//! - `export {}` with no source, or `export type {} ...`: fixed by deleting the statement,
//!   unless it holds a comment. A plain `export {}` is left alone in a `.d.ts` file, and in
//!   any file where it is the only module marker: no runtime import, no runtime export, and
//!   no earlier `export {}`.

use super::prelude::*;
use super::unicorn_text::needs_semicolon;
use ts_r_ast::{Ast, Modifiers};
use ts_r_span::Span;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/require-module-specifiers",
        "Require non-empty specifier list in import and export statements.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<RequireModuleSpecifiers>,
};

#[derive(Debug, Default)]
struct RequireModuleSpecifiers;

/// The previous token ends at `end`: its start when it is exactly `text`.
fn token_before(ast: &Ast, src: &str, pos: u32, text: &str) -> Option<u32> {
    let end = skip_trivia_backward(ast, src, pos as usize);
    let start = end.checked_sub(text.len())?;
    if src.get(start..end) != Some(text) {
        return None;
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    if text.chars().all(is_word) && src[..start].chars().next_back().is_some_and(is_word) {
        return None;
    }
    Some(start as u32)
}

/// `{` and `}` (their start offsets) of the empty list whose `}` ends before `pos`.
fn braces_before(ast: &Ast, src: &str, pos: u32) -> Option<(u32, u32)> {
    let close = token_before(ast, src, pos, "}")?;
    let open = token_before(ast, src, close, "{")?;
    Some((open, close))
}

fn one(at: u32) -> Span {
    Span::new(at, at + 1)
}

fn is_runtime_import(ast: &Ast, id: NodeId) -> bool {
    let Node::ImportDecl {
        type_only: false,
        specifiers,
        ..
    } = *ast.node(id)
    else {
        return false;
    };
    let specifiers = ast.list(specifiers);
    specifiers.is_empty()
        || specifiers.iter().any(|&s| {
            !matches!(
                ast.node(s),
                Node::ImportSpecifier {
                    type_only: true,
                    ..
                }
            )
        })
}

fn is_bare_marker(ast: &Ast, id: NodeId) -> bool {
    matches!(
        *ast.node(id),
        Node::ExportNamed { type_only: false, specifiers, source: None, .. }
            if ast.list(specifiers).is_empty()
    )
}

/// An export statement that makes the file a module at runtime.
fn is_runtime_export(ast: &Ast, id: NodeId) -> Option<bool> {
    let node = ast.node(id);
    Some(match *node {
        Node::ExportDefaultExpr { .. } => true,
        Node::ExportAll { type_only, .. } => !type_only,
        Node::ExportNamed {
            type_only,
            specifiers,
            source,
            ..
        } => {
            let specifiers = ast.list(specifiers);
            !type_only
                && ((source.is_some() && specifiers.is_empty())
                    || specifiers.iter().any(|&s| {
                        !matches!(
                            ast.node(s),
                            Node::ExportSpecifier {
                                type_only: true,
                                ..
                            }
                        )
                    }))
        }
        Node::VarDecl { .. }
        | Node::FunctionDecl { .. }
        | Node::ClassDecl { .. }
        | Node::Interface { .. }
        | Node::TypeAlias { .. }
        | Node::Enum { .. }
        | Node::Module { .. }
            if node.modifiers().contains(Modifiers::EXPORT) =>
        {
            // Type declarations and `declare`d ones export only types.
            node.modifiers().contains(Modifiers::DEFAULT)
                || !(matches!(node, Node::Interface { .. } | Node::TypeAlias { .. })
                    || node.modifiers().contains(Modifiers::DECLARE))
        }
        _ => return None,
    })
}

fn has_conflicting_marker(ast: &Ast, program: NodeId, decl: NodeId) -> bool {
    let Node::Program { stmts } = *ast.node(program) else {
        return false;
    };
    let mut before = true;
    ast.list(stmts).iter().any(|&s| {
        if s == decl {
            before = false;
            return false;
        }
        if matches!(ast.node(s), Node::ImportDecl { .. }) {
            return is_runtime_import(ast, s);
        }
        if is_bare_marker(ast, s) {
            return before;
        }
        is_runtime_export(ast, s).unwrap_or(false)
    })
}

/// Deleting the statement, with a `;` when the previous statement would run on.
fn remove_statement(cx: &Ctx<'_, '_>, id: NodeId) -> Fix {
    let span = cx.span(id);
    let text = if needs_semicolon(cx, span.start, "[") {
        ";"
    } else {
        ""
    };
    Fix::replace(span, text)
}

impl Rule for RequireModuleSpecifiers {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ImportDecl, NodeKind::ExportNamed]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let span = cx.span(node);
        let has_comments = !ast.comments_in(span).is_empty();
        match *cx.node(node) {
            Node::ImportDecl {
                type_only,
                specifiers,
                source,
                ..
            } => {
                let specifiers = ast.list(specifiers);
                if specifiers.iter().any(|&s| {
                    matches!(
                        ast.node(s),
                        Node::ImportSpecifier { .. } | Node::ImportNamespaceSpecifier { .. }
                    )
                }) {
                    return;
                }
                let Some(from) = token_before(ast, src, cx.span(source).start, "from") else {
                    return;
                };
                let Some((open, close)) = braces_before(ast, src, from) else {
                    return;
                };
                let report = Span::new(open, close + 1);
                let message = "import statement without specifiers is not allowed.";
                let default = specifiers.len() == 1;
                if type_only && !default {
                    let fix = (!has_comments).then(|| remove_statement(cx, node));
                    cx.report(report, message).id("error").fix_opt(fix);
                    return;
                }
                if default {
                    let Some(comma) = token_before(ast, src, open, ",") else {
                        return;
                    };
                    let mut fix = Fix::replace(one(close), "")
                        .and(one(open), "")
                        .and(one(comma), "");
                    if close + 1 == from {
                        fix = fix.and(Span::at(from), " ");
                    }
                    cx.report(report, message).id("error").fix(fix);
                    return;
                }
                let remove = (!has_comments).then(|| remove_statement(cx, node));
                let side_effect = Fix::replace(one(open), "")
                    .and(one(close), "")
                    .and(Span::new(from, from + 4), "");
                let mut r = cx.report(report, message).id("error");
                if let Some(fix) = remove {
                    r = r.suggest(
                        "suggestion/remove-declaration",
                        "Remove this import statement.",
                        fix,
                    );
                }
                r.suggest(
                    "suggestion/to-side-effect-import",
                    "Switch to side effect import.",
                    side_effect,
                );
            }
            Node::ExportNamed {
                type_only,
                specifiers,
                source,
                ..
            } => {
                if !ast.list(specifiers).is_empty() {
                    return;
                }
                if source.is_none() && !type_only {
                    let path = &cx.file.path;
                    let is_dts = [".d.ts", ".d.mts", ".d.cts"]
                        .iter()
                        .any(|ext| path.ends_with(ext));
                    let Some(program) = cx.parent(node) else {
                        return;
                    };
                    if is_dts || !has_conflicting_marker(ast, program, node) {
                        return;
                    }
                }
                let from = match source {
                    Some(source) => {
                        let Some(from) = token_before(ast, src, cx.span(source).start, "from")
                        else {
                            return;
                        };
                        Some(from)
                    }
                    None => None,
                };
                let braces = match from {
                    Some(from) => braces_before(ast, src, from),
                    None => {
                        let mut end = span.end;
                        if let Some(semi) = token_before(ast, src, end, ";") {
                            end = semi;
                        }
                        braces_before(ast, src, end)
                    }
                };
                let Some((open, close)) = braces else {
                    return;
                };
                let report = Span::new(open, close + 1);
                let message = "export statement without specifiers is not allowed.";
                let Some(from) = from.filter(|_| !type_only) else {
                    let fix = (!has_comments).then(|| remove_statement(cx, node));
                    cx.report(report, message).id("error").fix_opt(fix);
                    return;
                };
                let remove = (!has_comments).then(|| remove_statement(cx, node));
                let mut side_effect = Fix::replace(one(open), "")
                    .and(one(close), "")
                    .and(Span::new(from, from + 4), "");
                if src[span.start as usize..].starts_with("export") {
                    side_effect = side_effect.and(Span::new(span.start, span.start + 6), "import");
                }
                let mut r = cx.report(report, message).id("error");
                if let Some(fix) = remove {
                    r = r.suggest(
                        "suggestion/remove-declaration",
                        "Remove this export statement.",
                        fix,
                    );
                }
                r.suggest(
                    "suggestion/to-side-effect-import",
                    "Switch to side effect import.",
                    side_effect,
                );
            }
            _ => {}
        }
    }
}
