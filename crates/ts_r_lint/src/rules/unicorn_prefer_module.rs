//! `unicorn/prefer-module`: CommonJS in a module. Reports `"use strict"` directives (fixed
//! in `.mjs`, a suggestion elsewhere), a `return` outside any function, and global
//! references to `exports`, `require`, `module`, `__filename` and `__dirname`. `.cjs` files
//! are skipped.
//!
//! Suggestions: `__dirname`/`__filename` become `import.meta.*` or the `fileURLToPath`
//! form (parenthesized as a `new` callee when a call would capture the arguments, and
//! always under the TS parser); a top-level `require("x");` becomes `import "x";`, and a
//! lone top-level `const id = require("x")` (identifier, or object pattern of plain
//! `key: value` identifiers) becomes `import id from "x"`; a top-level `exports = v` /
//! `module.exports = v` becomes `export default v`, and `exports.k = v` with an identifier
//! `v` (and no comments in the assignment) becomes `export {v as k}`. JSON sources get
//! `with {type: 'json'}` (in the source's quote) and only the default-import form.

use super::prelude::*;
use super::unicorn_util::in_unparenthesized_new_callee;
use super::unicorn_util_h4::{fix_of, next_token_start, rename_identifier_edit};
use crate::semantic::{Resolution, SymbolFlags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-module",
        "Prefer JavaScript modules (ESM) over CommonJS.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferModule>,
};

#[derive(Debug, Default)]
struct PreferModule;

type Edits = Vec<(Span, String)>;

/// `cx.semantic()` sees `ident` as a reference to an undeclared (or configured) global.
fn is_global_reference(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let sem = cx.semantic();
    match sem.reference_at(ident).map(|r| sem.reference(r).resolution) {
        Some(Resolution::Global | Resolution::Unresolved) => true,
        Some(Resolution::Symbol(s)) => sem.symbol(s).flags.contains(SymbolFlags::COMMENT_GLOBAL),
        None => false,
    }
}

/// `text` quoted with `'`, escaped like a JS string literal.
fn quote_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\u{b}' => out.push_str("\\v"),
            '\0' => out.push_str("\\u{0}"),
            c if (c as u32) <= 0x1f || c == '\u{7f}' || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Deletes every parenthesis wrapping `id`.
fn remove_parens(cx: &Ctx<'_, '_>, id: NodeId, edits: &mut Edits) {
    let mut cur = id;
    while let Some(p) = cx.parent(cur) {
        if !matches!(cx.node(p), Node::Paren { .. }) {
            break;
        }
        let s = cx.span(p);
        edits.push((Span::new(s.start, s.start + 1), String::new()));
        edits.push((Span::new(s.end - 1, s.end), String::new()));
        cur = p;
    }
}

/// Deletes the whitespace right after `end`.
fn remove_spaces_after(cx: &Ctx<'_, '_>, end: u32, edits: &mut Edits) {
    let rest = &cx.src()[end as usize..];
    let len = rest.len() - rest.trim_start().len();
    if len > 0 {
        edits.push((Span::new(end, end + len as u32), String::new()));
    }
}

/// The value of a string literal or expression-free template.
fn static_string<'a>(cx: &Ctx<'_, 'a>, id: NodeId) -> Option<&'a str> {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value)),
        Node::TemplateLit { parts } => match ast.list(parts) {
            [only] => match *ast.node(*only) {
                Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } => Some(ast.atom(cooked)),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// `node` (a statement or expression, parentheses looked through) is a statement of the
/// program body.
fn top_level_statement(cx: &Ctx<'_, '_>, stmt: NodeId) -> bool {
    matches!(cx.node(stmt), Node::ExprStmt { .. })
        && cx
            .parent(stmt)
            .is_some_and(|p| matches!(cx.node(p), Node::Program { .. }))
}

fn fix_require(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let call = parent_skip_parens(ast, cx.file.parents, ident)?;
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(call)
    else {
        return None;
    };
    let &[arg] = ast.list(args) else {
        return None;
    };
    if skip_parens(ast, callee) != ident {
        return None;
    }
    let source = skip_parens(ast, arg);
    let value = static_string(cx, source)?;
    let source_text = match ast.node(source) {
        Node::StringLit { .. } => cx.text(source).to_owned(),
        _ => quote_string(value),
    };
    let is_json = value.ends_with(".json");
    let quote = &source_text[..1];
    let attributes = if is_json {
        format!(" with {{type: {quote}json{quote}}}")
    } else {
        String::new()
    };
    let call_span = cx.span(call);
    let close = Span::new(call_span.end - 1, call_span.end);
    let open_at = next_token_start(cx, cx.span(skip_parens_up(cx, callee)).end);
    let open = Span::new(open_at, open_at + 1);
    let mut edits: Edits = vec![(cx.span(source), source_text)];
    let parent = parent_skip_parens(ast, cx.file.parents, call)?;
    if top_level_statement(cx, parent) {
        edits.push((cx.span(ident), "import".to_owned()));
        edits.push((open, " ".to_owned()));
        edits.push((close, attributes));
    } else {
        let Node::VarDeclarator {
            name,
            type_ann: None,
            init: Some(init),
            ..
        } = *ast.node(parent)
        else {
            return None;
        };
        if skip_parens(ast, init) != call {
            return None;
        }
        let props = match *ast.node(name) {
            Node::Ident { .. } => None,
            Node::ObjectPattern { props } if !is_json => Some(ast.list(props)),
            _ => return None,
        };
        let decl = cx.parent(parent)?;
        let Node::VarDecl {
            kind: VarKind::Const,
            decls,
            ..
        } = *ast.node(decl)
        else {
            return None;
        };
        if ast.list(decls).len() != 1
            || !cx
                .parent(decl)
                .is_some_and(|p| matches!(cx.node(p), Node::Program { .. }))
        {
            return None;
        }
        let mut keys = Vec::new();
        for &p in props.unwrap_or(&[]) {
            let Node::BindingProperty {
                key,
                value,
                init: None,
            } = *ast.node(p)
            else {
                return None;
            };
            if !matches!(ast.node(key), Node::Ident { .. })
                || value.is_some_and(|v| !matches!(ast.node(v), Node::Ident { .. }))
            {
                return None;
            }
            if value.is_some() {
                keys.push(key);
            }
        }
        let src = cx.src();
        let decl_start = cx.span(decl).start;
        if !src[decl_start as usize..].starts_with("const") {
            return None;
        }
        edits.push((Span::new(decl_start, decl_start + 5), "import".to_owned()));
        let name_end = cx.span(name).end;
        let eq = next_token_start(cx, name_end);
        if src.as_bytes().get(eq as usize) != Some(&b'=') {
            return None;
        }
        remove_spaces_after(cx, name_end, &mut edits);
        remove_spaces_after(cx, eq + 1, &mut edits);
        edits.push((Span::new(eq, eq + 1), " from ".to_owned()));
        edits.push((cx.span(ident), String::new()));
        edits.push((open, String::new()));
        edits.push((close, attributes));
        for key in keys {
            let key_end = cx.span(key).end;
            let colon = next_token_start(cx, key_end);
            if src.as_bytes().get(colon as usize) != Some(&b':') {
                return None;
            }
            remove_spaces_after(cx, key_end, &mut edits);
            remove_spaces_after(cx, colon + 1, &mut edits);
            edits.push((Span::new(colon, colon + 1), " as ".to_owned()));
        }
    }
    for n in [ident, call, source] {
        remove_parens(cx, n, &mut edits);
    }
    Some(fix_of(edits))
}

/// `id` with the parentheses around it.
fn skip_parens_up(cx: &Ctx<'_, '_>, id: NodeId) -> NodeId {
    let mut cur = id;
    while let Some(p) = cx.parent(cur) {
        if !matches!(cx.node(p), Node::Paren { .. }) {
            break;
        }
        cur = p;
    }
    cur
}

/// `target = ...` is a whole top-level statement: the plain assignment and its statement.
fn top_level_assignment(cx: &Ctx<'_, '_>, target: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let assign = parent_skip_parens(ast, cx.file.parents, target)?;
    let Node::Assign {
        left,
        op: AssignOp::Assign,
        ..
    } = *ast.node(assign)
    else {
        return None;
    };
    if skip_parens(ast, left) != target {
        return None;
    }
    let stmt = parent_skip_parens(ast, cx.file.parents, assign)?;
    top_level_statement(cx, stmt).then_some(assign)
}

/// The suggestion for `exports` / `module.exports` (`target`).
fn fix_exports(cx: &Ctx<'_, '_>, target: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let mut edits = Edits::new();
    if let Some(assign) = top_level_assignment(cx, target) {
        // `exports = value` -> `export default value`
        let end = cx.span(target).end;
        edits.push((cx.span(target), "export default ".to_owned()));
        remove_spaces_after(cx, end, &mut edits);
        let src = cx.src();
        let mut eq = end as usize;
        loop {
            eq = skip_trivia_forward(src, eq);
            match src.as_bytes().get(eq) {
                Some(b'=') => break,
                Some(_) => eq += 1,
                None => return None,
            }
        }
        let eq = eq as u32;
        edits.push((Span::new(eq, eq + 1), String::new()));
        remove_spaces_after(cx, eq + 1, &mut edits);
        remove_parens(cx, assign, &mut edits);
        remove_parens(cx, target, &mut edits);
        return Some(fix_of(edits));
    }
    // `exports.name = local` -> `export {local as name}`
    let member = parent_skip_parens(ast, cx.file.parents, target)?;
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(member)
    else {
        return None;
    };
    let exported = ident_name(ast, prop)?;
    if skip_parens(ast, obj) != target {
        return None;
    }
    let assign = top_level_assignment(cx, member)?;
    let Node::Assign { right, .. } = *ast.node(assign) else {
        return None;
    };
    let local = ident_name(ast, skip_parens(ast, right))?;
    if !ast.comments_in(cx.span(assign)).is_empty() {
        return None;
    }
    edits.push((cx.span(assign), format!("export {{{local} as {exported}}}")));
    remove_parens(cx, assign, &mut edits);
    Some(fix_of(edits))
}

/// `module.exports` when `module` (`ident`) is its object.
fn module_exports(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let member = parent_skip_parens(ast, cx.file.parents, ident)?;
    match *ast.node(member) {
        Node::Member {
            obj,
            optional: false,
            prop,
        } if skip_parens(ast, obj) == ident && ident_name(ast, prop) == Some("exports") => {
            Some(member)
        }
        _ => None,
    }
}

impl PreferModule {
    fn identifier(cx: &mut Ctx<'_, '_>, node: NodeId) {
        let ast = cx.ast();
        let Some(name) = ident_name(ast, node) else {
            return;
        };
        if !matches!(
            name,
            "exports" | "require" | "module" | "__filename" | "__dirname"
        ) || !is_global_reference(cx, node)
        {
            return;
        }
        let message = format!("Do not use \"{name}\".");
        match name {
            "__filename" | "__dirname" => {
                let options: [(&'static str, &str, &str, bool); 2] = if name == "__dirname" {
                    [
                        (
                            "suggestion/import-meta-dirname",
                            "Replace `__dirname` with `import.meta.dirname`.",
                            "import.meta.dirname",
                            false,
                        ),
                        (
                            "suggestion/import-meta-url-to-dirname",
                            "Replace `__dirname` with `…(import.meta.url)`.",
                            "path.dirname(url.fileURLToPath(import.meta.url))",
                            true,
                        ),
                    ]
                } else {
                    [
                        (
                            "suggestion/import-meta-filename",
                            "Replace `__filename` with `import.meta.filename`.",
                            "import.meta.filename",
                            false,
                        ),
                        (
                            "suggestion/import-meta-url-to-filename",
                            "Replace `__filename` with `…(import.meta.url)`.",
                            "url.fileURLToPath(import.meta.url)",
                            true,
                        ),
                    ]
                };
                let wrap_new = in_unparenthesized_new_callee(cx, node);
                let typescript = cx.env().typescript;
                let fixes = options.map(|(id, desc, replacement, is_call)| {
                    let text = if (is_call || typescript) && wrap_new {
                        format!("({replacement})")
                    } else {
                        replacement.to_owned()
                    };
                    let (span, text) = rename_identifier_edit(cx, node, &text);
                    (id, desc, Fix::replace(span, text))
                });
                let mut report = cx.report_node(node, message).id("error/identifier");
                for (id, desc, fix) in fixes {
                    report = report.suggest(id, desc, fix);
                }
            }
            _ => {
                let fix = match name {
                    "require" => fix_require(cx, node)
                        .map(|f| ("suggestion/import", "Switch to `import`.", f)),
                    "exports" => fix_exports(cx, node)
                        .map(|f| ("suggestion/export", "Switch to `export`.", f)),
                    _ => module_exports(cx, node)
                        .and_then(|m| fix_exports(cx, m))
                        .map(|f| ("suggestion/export", "Switch to `export`.", f)),
                };
                let report = cx.report_node(node, message).id("error/identifier");
                if let Some((id, desc, fix)) = fix {
                    report.suggest(id, desc, fix);
                }
            }
        }
    }
}

impl Rule for PreferModule {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Directive, NodeKind::Return, NodeKind::Ident]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let path = cx.file.path.to_ascii_lowercase();
        if path.ends_with(".cjs") {
            return;
        }
        match cx.node(node) {
            Node::Directive { .. } => {
                let text = cx.text(node);
                if !text.starts_with("'use strict'") && !text.starts_with("\"use strict\"") {
                    return;
                }
                let span = cx.span(node);
                let mut edits = vec![(span, String::new())];
                remove_spaces_after(cx, span.end, &mut edits);
                let fix = fix_of(edits);
                let report = cx
                    .report_node(node, "Do not use \"use strict\" directive.")
                    .id("error/use-strict-directive");
                if path.ends_with(".mjs") {
                    report.fix(fix);
                } else {
                    report.suggest(
                        "suggestion/use-strict-directive",
                        "Remove \"use strict\" directive.",
                        fix,
                    );
                }
            }
            Node::Return { .. } => {
                let in_function = ancestors_inclusive(cx.file.parents, node)
                    .skip(1)
                    .any(|a| is_function(cx.node(a).kind()));
                if !in_function {
                    let start = cx.span(node).start;
                    cx.report(
                        Span::new(start, start + 6),
                        "\"return\" should be used inside a function.",
                    )
                    .id("error/global-return");
                }
            }
            _ => Self::identifier(cx, node),
        }
    }
}
