//! `unicorn/prefer-export-from`: an imported binding re-exported through `export default x`,
//! `export {x as y}` or `export const y = x` (a lone, untyped, otherwise unused `const`)
//! should use `export ... from`. A namespace import can't become a default export.
//! Skipped: import declarations with a binding that has more than one declaration, a
//! `const` re-export with a JSDoc comment attached, and (with `checkUsedVariables: false`)
//! imports whose bindings are used for anything but re-exports.
//!
//! The fix appends the specifier to an `export {...} from` of the same source (a `type`
//! one for type-only imports or exports, if there is one; otherwise a value one with an
//! inline `type`), or appends `export {...} from ...` / `export * as name from ...` at the
//! end of the file; removes the export, and the import too when the re-export was its only
//! use (unless a value import is re-exported as a type). No fix when either declaration
//! holds a comment. When `checkUsedVariables` is off and a binding of the import is never
//! used, the fix becomes a suggestion.

use super::prelude::*;
use super::unicorn_util_h4::linebreak;
use ts_r_ast::{Ast, Modifiers};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-export-from",
        "Prefer `export…from` when re-exporting.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<PreferExportFrom>,
};

#[derive(Debug)]
struct PreferExportFrom {
    check_used_variables: bool,
}

impl FromOptions for PreferExportFrom {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut check_used_variables = true;
        if let Some(o) = opts.first().and_then(Value::as_object)
            && let Some(v) = o.get("checkUsedVariables")
        {
            check_used_variables = v
                .as_bool()
                .ok_or("`checkUsedVariables` must be a boolean")?;
        }
        Ok(PreferExportFrom {
            check_used_variables,
        })
    }
}

/// A specifier name: identifiers and string literals differ even with the same text.
#[derive(Clone, PartialEq, Eq)]
enum Name {
    Ident(String),
    Str(String),
    Namespace,
}

fn specifier_name(ast: &Ast, id: NodeId) -> Option<Name> {
    match *ast.node(id) {
        Node::Ident { name } => Some(Name::Ident(ast.atom(name).to_owned())),
        Node::StringLit { value } => Some(Name::Str(ast.atom(value).to_owned())),
        _ => None,
    }
}

fn default_name() -> Name {
    Name::Ident("default".to_owned())
}

struct Imported {
    specifier: NodeId,
    decl: NodeId,
    name: Name,
    text: String,
    is_type: bool,
    refs: usize,
}

struct Exported {
    /// The reported node: the export specifier or the whole export declaration.
    node: NodeId,
    name: Name,
    text: String,
    is_type: bool,
    jsdoc: bool,
}

fn string_value(ast: &Ast, id: NodeId) -> Option<&str> {
    match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value)),
        _ => None,
    }
}

/// A `/** */` comment (not `/***`) right before `node`, on its own line(s), ending on the
/// line of `node` or the one before.
fn has_attached_jsdoc(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let src = cx.src();
    let start = cx.span(node).start;
    let prev = skip_trivia_backward(ast, src, start as usize) as u32;
    let Some(comment) = ast.comments_between(prev, start).last() else {
        return false;
    };
    let text = comment.span.text(src);
    if !text.starts_with("/**") || text.starts_with("/***") || !text.ends_with("*/") {
        return false;
    }
    // Only whitespace between the comment and the node.
    if !src[comment.span.end as usize..start as usize]
        .trim()
        .is_empty()
    {
        return false;
    }
    let lines = cx.file.lines;
    // The end of the token before the comment (other comments are not tokens).
    let token_before = skip_trivia_backward(ast, src, comment.span.start as usize) as u32;
    let comment_line = lines.line_of(comment.span.start);
    let end_line = lines.line_of(comment.span.end);
    let node_line = lines.line_of(start);
    (token_before == 0 || lines.line_of(token_before) < comment_line)
        && (end_line == node_line || end_line + 1 == node_line)
}

/// The plugin's `needsSemicolon` for a removed top-level declaration: the previous
/// statement ends without `;` in a token the next code could continue.
fn needs_semicolon_before(cx: &Ctx<'_, '_>, stmt: NodeId) -> bool {
    let ast = cx.ast();
    let src = cx.src();
    let end = skip_trivia_backward(ast, src, cx.span(stmt).start as usize);
    if end == 0 || src[..end].ends_with(';') {
        return false;
    }
    let Some(prev) = prev_statement(ast, cx.file.parents, stmt) else {
        return false;
    };
    if cx.span(prev).end as usize == end
        && matches!(
            cx.node(prev),
            Node::ExprStmt { .. }
                | Node::VarDecl { .. }
                | Node::Return { .. }
                | Node::Throw { .. }
                | Node::ExportDefaultExpr { .. }
        )
    {
        return true;
    }
    match src[..end].chars().next_back() {
        Some(']' | ')' | '`' | '\'' | '"') => true,
        Some(c) => c.is_alphanumeric() || c == '_' || c == '$' || c == '/',
        None => false,
    }
}

impl PreferExportFrom {
    fn exported(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<Exported> {
        let ast = cx.ast();
        let parent = parent_skip_parens(ast, cx.file.parents, ident)?;
        match *ast.node(parent) {
            Node::ExportDefaultExpr { .. } => Some(Exported {
                node: parent,
                name: default_name(),
                text: "default".to_owned(),
                is_type: false,
                jsdoc: false,
            }),
            Node::ExportSpecifier {
                type_only,
                local,
                exported,
            } if local == ident => {
                let decl_type = cx.parent(parent).is_some_and(|d| {
                    matches!(
                        ast.node(d),
                        Node::ExportNamed {
                            type_only: true,
                            ..
                        }
                    )
                });
                let e = exported.unwrap_or(local);
                Some(Exported {
                    node: parent,
                    name: specifier_name(ast, e)?,
                    text: cx.text(e).to_owned(),
                    is_type: type_only || decl_type,
                    jsdoc: false,
                })
            }
            Node::VarDeclarator {
                name,
                type_ann: None,
                init: Some(init),
                ..
            } if skip_parens(ast, init) == ident => {
                let id_name = ident_name(ast, name)?;
                let decl = cx.parent(parent)?;
                let Node::VarDecl {
                    modifiers,
                    kind: VarKind::Const,
                    decls,
                } = *ast.node(decl)
                else {
                    return None;
                };
                if ast.list(decls).len() != 1
                    || !modifiers.contains(Modifiers::EXPORT)
                    || modifiers.contains(Modifiers::DEFAULT)
                {
                    return None;
                }
                let sem = cx.semantic();
                let sym = sem.symbol_of_decl(name)?;
                let s = sem.symbol(sym);
                if s.decls.len() != 1 || s.refs.iter().any(|&r| sem.reference(r).node != name) {
                    return None;
                }
                Some(Exported {
                    node: decl,
                    name: Name::Ident(id_name.to_owned()),
                    text: cx.text(name).to_owned(),
                    is_type: false,
                    jsdoc: has_attached_jsdoc(cx, decl),
                })
            }
            _ => None,
        }
    }

    fn imported(
        cx: &Ctx<'_, '_>,
        specifier: NodeId,
        decl: NodeId,
        refs: usize,
    ) -> Option<Imported> {
        let ast = cx.ast();
        let decl_type = matches!(
            ast.node(decl),
            Node::ImportDecl {
                type_only: true,
                ..
            }
        );
        let (name, text, spec_type) = match *ast.node(specifier) {
            Node::ImportDefaultSpecifier { .. } => (default_name(), "default".to_owned(), false),
            Node::ImportNamespaceSpecifier { .. } => (Name::Namespace, "*".to_owned(), false),
            Node::ImportSpecifier {
                type_only,
                imported,
                local,
            } => {
                let i = imported.unwrap_or(local);
                (specifier_name(ast, i)?, cx.text(i).to_owned(), type_only)
            }
            _ => return None,
        };
        Some(Imported {
            specifier,
            decl,
            name,
            text,
            is_type: spec_type || decl_type,
            refs,
        })
    }

    /// From the `from` keyword to the end of the import declaration.
    fn source_text<'a>(cx: &Ctx<'_, 'a>, decl: NodeId) -> &'a str {
        let ast = cx.ast();
        let src = cx.src();
        let Node::ImportDecl { source, .. } = *ast.node(decl) else {
            return "";
        };
        let span = cx.span(decl);
        let from = skip_trivia_backward(ast, src, cx.span(source).start as usize);
        let start = from.saturating_sub(4);
        if !src[start..from].starts_with("from") {
            return "";
        }
        &src[start..span.end as usize]
    }

    /// Edits removing an import/export specifier (or its whole declaration).
    fn remove_specifier(cx: &Ctx<'_, '_>, spec: NodeId, edits: &mut Vec<(Span, String)>) {
        let ast = cx.ast();
        let src = cx.src();
        let Some(decl) = cx.parent(spec) else {
            return;
        };
        let specifiers = match *ast.node(decl) {
            Node::ImportDecl { specifiers, .. } | Node::ExportNamed { specifiers, .. } => {
                ast.list(specifiers)
            }
            _ => return,
        };
        if specifiers.len() == 1 {
            Self::remove_statement(cx, decl, edits);
            return;
        }
        let span = cx.span(spec);
        if matches!(ast.node(spec), Node::ImportSpecifier { .. })
            && specifiers
                .iter()
                .all(|&s| s == spec || !matches!(ast.node(s), Node::ImportSpecifier { .. }))
        {
            let has_default = specifiers
                .iter()
                .any(|&s| matches!(ast.node(s), Node::ImportDefaultSpecifier { .. }));
            let want = if has_default { b',' } else { b'{' };
            let mut start = span.start as usize;
            loop {
                start = skip_trivia_backward(ast, src, start);
                if start == 0 {
                    return;
                }
                if src.as_bytes()[start - 1] == want {
                    start -= 1;
                    break;
                }
                start -= 1;
            }
            let mut end = span.end as usize;
            loop {
                end = skip_trivia_forward(src, end);
                if end >= src.len() {
                    return;
                }
                if src[end..].starts_with("from") {
                    break;
                }
                end += 1;
            }
            let before = skip_trivia_backward(ast, src, start);
            let space = before == start;
            edits.push((
                Span::new(start as u32, end as u32),
                if space { " " } else { "" }.to_owned(),
            ));
            return;
        }
        edits.push((span, String::new()));
        let after = skip_trivia_forward(src, span.end as usize);
        if src.as_bytes().get(after) == Some(&b',') {
            edits.push((Span::new(after as u32, after as u32 + 1), String::new()));
        }
    }

    fn remove_statement(cx: &Ctx<'_, '_>, stmt: NodeId, edits: &mut Vec<(Span, String)>) {
        let text = if needs_semicolon_before(cx, stmt) {
            ";"
        } else {
            ""
        };
        edits.push((cx.span(stmt), text.to_owned()));
    }

    fn remove_import_or_export(cx: &Ctx<'_, '_>, node: NodeId, edits: &mut Vec<(Span, String)>) {
        match cx.node(node) {
            Node::ImportSpecifier { .. }
            | Node::ImportDefaultSpecifier { .. }
            | Node::ImportNamespaceSpecifier { .. }
            | Node::ExportSpecifier { .. } => Self::remove_specifier(cx, node, edits),
            _ => Self::remove_statement(cx, node, edits),
        }
    }

    fn fix(cx: &Ctx<'_, '_>, imported: &Imported, exported: &Exported, exports: &[NodeId]) -> Fix {
        let ast = cx.ast();
        let src = cx.src();
        let Node::ImportDecl { source, .. } = *ast.node(imported.decl) else {
            return Fix { edits: Vec::new() };
        };
        let source_value = string_value(ast, source).unwrap_or("");
        let as_type = imported.is_type || exported.is_type;
        let find = |want_type: bool| {
            exports.iter().copied().find(|&e| match *ast.node(e) {
                Node::ExportNamed {
                    type_only,
                    source: Some(s),
                    ..
                } => type_only == want_type && string_value(ast, s) == Some(source_value),
                _ => false,
            })
        };
        let target = as_type
            .then(|| find(true))
            .flatten()
            .or_else(|| find(false));
        let mut edits = Vec::new();
        let end = Span::at(src.len() as u32);
        let lb = linebreak(src);
        let from = Self::source_text(cx, imported.decl);
        if imported.name == Name::Namespace {
            let ty = if as_type { "type " } else { "" };
            edits.push((end, format!("{lb}export {ty}* as {} {from}", exported.text)));
        } else {
            let mut spec = if exported.name == imported.name {
                exported.text.clone()
            } else {
                format!("{} as {}", imported.text, exported.text)
            };
            let target_is_type = target.is_some_and(|t| {
                matches!(
                    ast.node(t),
                    Node::ExportNamed {
                        type_only: true,
                        ..
                    }
                )
            });
            if as_type && !target_is_type {
                spec = format!("type {spec}");
            }
            match target {
                Some(t) => {
                    let Node::ExportNamed { specifiers, .. } = *ast.node(t) else {
                        return Fix { edits: Vec::new() };
                    };
                    if let Some(&last) = ast.list(specifiers).last() {
                        edits.push((Span::at(cx.span(last).end), format!(", {spec}")));
                    } else {
                        let open = src[cx.span(t).start as usize..]
                            .find('{')
                            .map(|i| cx.span(t).start + i as u32 + 1);
                        if let Some(o) = open {
                            edits.push((Span::at(o), spec));
                        }
                    }
                }
                None => edits.push((end, format!("{lb}export {{{spec}}} {from}"))),
            }
        }
        let side_effects = exported.is_type && !imported.is_type;
        if imported.refs == 1 && !side_effects {
            Self::remove_import_or_export(cx, imported.specifier, &mut edits);
        }
        Self::remove_import_or_export(cx, exported.node, &mut edits);
        super::unicorn_util_h4::fix_of(edits)
    }
}

impl Rule for PreferExportFrom {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Program]
    }
    fn enter(&self, program: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Program { stmts } = *ast.node(program) else {
            return;
        };
        let stmts = ast.list(stmts);
        let exports: Vec<NodeId> = stmts
            .iter()
            .copied()
            .filter(|&s| {
                matches!(*ast.node(s), Node::ExportNamed { source: Some(src), .. }
                    if matches!(ast.node(src), Node::StringLit { .. }))
            })
            .collect();
        let sem = cx.semantic();
        let mut problems: Vec<(NodeId, String, Option<Fix>, bool)> = Vec::new();
        for &decl in stmts {
            let Node::ImportDecl { specifiers, .. } = *ast.node(decl) else {
                continue;
            };
            let specifiers = ast.list(specifiers);
            if specifiers.is_empty() {
                continue;
            }
            let mut vars = Vec::new();
            let mut skip = false;
            for &spec in specifiers {
                let local = match *ast.node(spec) {
                    Node::ImportDefaultSpecifier { local }
                    | Node::ImportNamespaceSpecifier { local }
                    | Node::ImportSpecifier { local, .. } => local,
                    _ => {
                        skip = true;
                        break;
                    }
                };
                let Some(sym) = sem.symbol_of_decl(local) else {
                    skip = true;
                    break;
                };
                let s = sem.symbol(sym);
                if s.decls.len() != 1 {
                    skip = true;
                    break;
                }
                let refs: Vec<NodeId> = s.refs.iter().map(|&r| sem.reference(r).node).collect();
                let Some(imported) = Self::imported(cx, spec, decl, refs.len()) else {
                    skip = true;
                    break;
                };
                let mut exported = Vec::new();
                for &r in &refs {
                    if let Some(e) = Self::exported(cx, r) {
                        if imported.name == Name::Namespace && e.name == default_name() {
                            continue;
                        }
                        exported.push(e);
                    }
                }
                vars.push((imported, exported));
            }
            if skip {
                continue;
            }
            if !self.check_used_variables && vars.iter().any(|(i, e)| i.refs != e.len()) {
                continue;
            }
            let suggest = !self.check_used_variables && vars.iter().any(|(i, _)| i.refs == 0);
            for (imported, exported) in &vars {
                for e in exported.iter().filter(|e| !e.jsdoc) {
                    let export_decl = match ast.node(e.node) {
                        Node::ExportSpecifier { .. } => cx.parent(e.node).unwrap_or(e.node),
                        _ => e.node,
                    };
                    let has_comment = [imported.decl, export_decl]
                        .iter()
                        .any(|&n| !ast.comments_in(cx.span(n)).is_empty());
                    let fix = (!has_comment).then(|| Self::fix(cx, imported, e, &exports));
                    problems.push((e.node, e.text.clone(), fix, suggest));
                }
            }
        }
        for (node, text, fix, suggest) in problems {
            let report = cx
                .report_node(node, format!("Use `export…from` to re-export `{text}`."))
                .id("error");
            match (fix, suggest) {
                (Some(f), false) => {
                    report.fix(f);
                }
                (Some(f), true) => {
                    report.suggest("suggestion", "Switch to `export…from`.", f);
                }
                (None, _) => {}
            }
        }
    }
}
