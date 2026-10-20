//! "Convert export": a top-level declaration's export between named and default, with the
//! importers in other files rewritten. Observed on tsserver 6.0.3 (fixtures
//! `refactor-export-to-*`; never from its source):
//!
//! | Action (kind) | Offered at | Result |
//! |---|---|---|
//! | `Convert named export to default export` (`refactor.rewrite.export.default`) | `export function` / `class` / `interface` / `type` / `enum` / `namespace` / `const x = ...` (one declarator) in a file without a default export | functions, classes, interfaces: `export default <decl>`; type aliases, enums, namespaces, and constants used in their file or annotated: `export` dropped and `export default X` after the statement (a blank line before it after a namespace); other constants: `export default <initializer>` |
//! | `Convert default export to named export` (`refactor.rewrite.export.named`) | `export default function f` / `class C` / `interface I`, or a selection covering `export default x;` | `default` dropped; `export default x;` becomes `export { x };` |
//!
//! - Where: a cursor (or a selection's start) on a token of the statement itself, its leading
//!   comments and whitespace included: the `export`/`default`/declaration keywords, the name,
//!   the type parameter and parameter list punctuation, a keyword return type, the braces of a
//!   class, interface or enum body, the `;`. Not on a parameter, a member, a heritage clause or
//!   a body. A constant only from `export` or its `;`. `export default x;` only with a selection
//!   covering the whole statement. On a cursor move (`implicit`) only a non-empty selection.
//! - Not offered: `let`/`var`, several declarators or a destructuring; anonymous default
//!   exports; `export default <expression>`; `declare` declarations (tsserver writes
//!   `export default declare ...`). We also decline what tsserver gets wrong: a name exported
//!   twice, `import()` types or dynamic imports of the file (tsserver fails or would need types),
//!   one import naming the export twice, a default and a named import of it in one statement.
//! - Importers (every file of the program whose import resolves to this file):
//!   `import { a, b }` → `import a, { b }`, `import { a as x }` → `import x`, `export { a } from`
//!   → `export { default as a } from`, `ns.a` → `ns.default` (namespace imports and
//!   `import ns = require`); and back: `import x, { b }` → `import { b, a as x }`,
//!   `{ default as x }` → `{ a as x }`, `export { default } from` → `export { a as default }`,
//!   `import x, * as ns` → `import * as ns` and a new `import { a as x }` after it. `export *`
//!   needs nothing. Semicolons follow the file.

use super::edit::Edit;
use super::{Action, Ctx, Request, style};
use ts_r_ast::{Ast, Modifiers, Node, NodeId, Span, VarKind};
use ts_r_binder::program::ModuleTarget;
use ts_r_binder::{BoundFile, RequestKind};
use ts_r_checker::SourceFile;
use ts_r_diagnostics::FileId;

const REFACTOR: &str = "Convert export";
const TO_DEFAULT: &str = "Convert named export to default export";
const TO_NAMED: &str = "Convert default export to named export";

// ---- tokens and trivia ----

/// The comment of `ast` containing `pos` (start inclusive, end exclusive).
fn comment_at(ast: &Ast, pos: u32) -> Option<Span> {
    let before = ast.comments_in(Span::new(0, pos + 1));
    before
        .last()
        .map(|c| c.span)
        .filter(|s| s.start <= pos && pos < s.end)
}

/// The start of the token at `pos`: `pos` itself inside a token, else the next token after
/// the whitespace and comments around `pos` (tsserver gives a position in a token's leading
/// trivia to that token).
pub(super) fn token_start(ast: &Ast, text: &str, pos: u32) -> u32 {
    let b = text.as_bytes();
    let mut p = pos.min(text.len() as u32);
    loop {
        if let Some(c) = comment_at(ast, p) {
            p = c.end;
            continue;
        }
        match b.get(p as usize) {
            Some(c) if c.is_ascii_whitespace() => p += 1,
            _ => return p,
        }
    }
}

/// The end of the token before `pos` (whitespace and comments skipped backwards).
pub(super) fn prev_token_end(ast: &Ast, text: &str, pos: u32) -> u32 {
    let b = text.as_bytes();
    let mut p = pos.min(text.len() as u32);
    loop {
        while p > 0 && b[p as usize - 1].is_ascii_whitespace() {
            p -= 1;
        }
        if p == 0 {
            return 0;
        }
        match comment_at(ast, p - 1) {
            Some(c) => p = c.start,
            None => return p,
        }
    }
}

/// The statement of `stmts` whose token range holds `pos`: from the end of the statement
/// before it (its leading trivia) to its end.
fn statement_at(ast: &Ast, stmts: &[NodeId], pos: u32) -> Option<NodeId> {
    let mut prev_end = 0;
    for &s in stmts {
        let sp = ast.span(s);
        if prev_end <= pos && pos < sp.end {
            return Some(s);
        }
        prev_end = sp.end;
    }
    None
}

// ---- where the refactor applies ----

/// What a statement can be converted to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Conversion {
    ToDefault,
    ToNamed,
}

/// Whether the token starting at `q` belongs to statement `st` itself rather than to a node
/// inside it (names and keyword types count as the statement's own tokens).
fn own_token(ast: &Ast, text: &str, st: NodeId, q: u32) -> bool {
    let sp = ast.span(st);
    if q < sp.start || q >= sp.end {
        return false;
    }
    let mut covered: Vec<(u32, u32)> = Vec::new();
    let leaf = |n: NodeId| {
        matches!(
            ast.node(n),
            Node::Ident { .. } | Node::KeywordType { .. } | Node::ThisType {}
        )
    };
    // `extends`/`implements` start a node in tsserver's tree: cover the keyword too.
    let keyword_before = |first: NodeId, kws: &[&str]| -> u32 {
        let s = ast.span(first).start;
        let e = prev_token_end(ast, text, s);
        for kw in kws {
            if text[..e as usize].ends_with(kw) {
                return e - kw.len() as u32;
            }
        }
        s
    };
    match *ast.node(st) {
        Node::VarDecl { decls, .. } => {
            let list = ast.list(decls);
            if let (Some(&first), Some(&last)) = (list.first(), list.last()) {
                let kw = keyword_before(first, &["const", "let", "var"]);
                covered.push((kw, ast.span(last).end));
            }
        }
        Node::Interface {
            name,
            type_params,
            extends,
            body,
            ..
        } => {
            let _ = name;
            push_type_params(ast, type_params, &mut covered);
            let ext = ast.list(extends);
            if let (Some(&first), Some(&last)) = (ext.first(), ext.last()) {
                covered.push((keyword_before(first, &["extends"]), ast.span(last).end));
            }
            for &m in ast.list(body) {
                covered.push(span_pair(ast, m));
            }
        }
        Node::ClassDecl {
            decorators,
            type_params,
            heritage,
            members,
            ..
        } => {
            for &d in ast.list(decorators) {
                covered.push(span_pair(ast, d));
            }
            push_type_params(ast, type_params, &mut covered);
            let her = ast.list(heritage);
            if let (Some(&first), Some(&last)) = (her.first(), her.last()) {
                covered.push((
                    keyword_before(first, &["extends", "implements"]),
                    ast.span(last).end,
                ));
            }
            for &m in ast.list(members) {
                covered.push(span_pair(ast, m));
            }
        }
        _ => {
            ast.for_each_child(st, |c| {
                if leaf(c) {
                    return;
                }
                if let Node::TypeParams { params } = *ast.node(c) {
                    for &p in ast.list(params) {
                        covered.push(span_pair(ast, p));
                    }
                    return;
                }
                covered.push(span_pair(ast, c));
            });
        }
    }
    !covered.iter().any(|&(s, e)| s <= q && q < e)
}

fn span_pair(ast: &Ast, n: NodeId) -> (u32, u32) {
    let sp = ast.span(n);
    (sp.start, sp.end)
}

fn push_type_params(ast: &Ast, tp: Option<NodeId>, out: &mut Vec<(u32, u32)>) {
    if let Some(tp) = tp
        && let Node::TypeParams { params } = *ast.node(tp)
    {
        for &p in ast.list(params) {
            out.push(span_pair(ast, p));
        }
    }
}

/// The declared name of an exportable top-level declaration (`None`: not convertible).
fn decl_name(ast: &Ast, st: NodeId) -> Option<NodeId> {
    match *ast.node(st) {
        Node::FunctionDecl { name, body, .. } => {
            body?;
            name
        }
        Node::ClassDecl { name, .. } => name,
        Node::Interface { name, .. } => Some(name),
        Node::TypeAlias { name, .. } | Node::Enum { name, .. } => Some(name),
        Node::Module {
            name,
            body: Some(_),
            ..
        } if matches!(ast.node(name), Node::Ident { .. }) => Some(name),
        Node::VarDecl {
            kind: VarKind::Const,
            decls,
            ..
        } => match ast.list(decls) {
            [d] => match *ast.node(*d) {
                Node::VarDeclarator {
                    name,
                    init: Some(_),
                    ..
                } if matches!(ast.node(name), Node::Ident { .. }) => Some(name),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// Names the file exports (top-level `export` declarations and export specifiers), with
/// `default` for default exports and `export=` for `export =`.
fn exported_names<'a>(ast: &'a Ast, stmts: &[NodeId]) -> Vec<&'a str> {
    let mut out = Vec::new();
    for &s in stmts {
        let n = ast.node(s);
        let m = n.modifiers();
        match *n {
            Node::ExportDefaultExpr { .. } => out.push("default"),
            Node::ExportAssignment { .. } => out.push("export="),
            Node::ExportNamed { specifiers, .. } => {
                for &sp in ast.list(specifiers) {
                    if let Node::ExportSpecifier {
                        local, exported, ..
                    } = *ast.node(sp)
                        && let Some(name) = ast.name_of(exported.unwrap_or(local))
                    {
                        out.push(name);
                    }
                }
            }
            Node::ExportAll {
                exported: Some(e), ..
            } => {
                if let Some(name) = ast.name_of(e) {
                    out.push(name);
                }
            }
            _ if m.contains(Modifiers::EXPORT) && m.contains(Modifiers::DEFAULT) => {
                out.push("default");
            }
            _ if m.contains(Modifiers::EXPORT) => {
                if let Node::VarDecl { decls, .. } = *n {
                    for &d in ast.list(decls) {
                        if let Node::VarDeclarator { name, .. } = *ast.node(d)
                            && let Some(t) = ast.name_of(name)
                        {
                            out.push(t);
                        }
                    }
                } else if let Some(id) = decl_name(ast, s)
                    && let Some(t) = ast.name_of(id)
                {
                    out.push(t);
                }
            }
            _ => {}
        }
    }
    out
}

/// "Convert export" actions at the request's position.
pub fn convert_export(cx: &Ctx<'_, '_>, req: &Request<'_>, out: &mut Vec<Action>) {
    if !req.wants("refactor.rewrite.export.default") && !req.wants("refactor.rewrite.export.named")
    {
        return;
    }
    let f = cx.checker.file(cx.file);
    if !f.bound.is_module || (!req.invoked && req.start == req.end) {
        return;
    }
    let ast = &f.ast;
    let text = f.text.as_str();
    let stmts = style::top_level(ast);
    let Some(st) = statement_at(ast, &stmts, req.start) else {
        return;
    };
    let node = ast.node(st);
    let m = node.modifiers();
    let (conv, name_id) = if let Node::ExportDefaultExpr { expr } = *node {
        // `export default x;`: only a selection covering the statement.
        let sp = ast.span(st);
        let (s, e) = (req.start, req.end);
        let e = prev_token_end(ast, text, e).max(s);
        if s > sp.start || e < sp.end || !matches!(ast.node(expr), Node::Ident { .. }) {
            return;
        }
        (Conversion::ToNamed, expr)
    } else {
        if !m.contains(Modifiers::EXPORT) || m.contains(Modifiers::DECLARE) {
            return;
        }
        let q = token_start(ast, text, req.start);
        if !own_token(ast, text, st, q) {
            return;
        }
        let Some(id) = decl_name(ast, st) else {
            return;
        };
        if m.contains(Modifiers::DEFAULT) {
            if !matches!(
                node,
                Node::FunctionDecl { .. } | Node::ClassDecl { .. } | Node::Interface { .. }
            ) {
                return;
            }
            (Conversion::ToNamed, id)
        } else {
            // A constant converts only from `export` or its `;`.
            if let Node::VarDecl { decls, .. } = *node
                && let Some(&d) = ast.list(decls).first()
                && q >= ast.span(d).start
                && q < ast.span(d).end
            {
                return;
            }
            (Conversion::ToDefault, id)
        }
    };
    let (kind, title) = match conv {
        Conversion::ToDefault => ("refactor.rewrite.export.default", TO_DEFAULT),
        Conversion::ToNamed => ("refactor.rewrite.export.named", TO_NAMED),
    };
    if !req.wants(kind) {
        return;
    }
    let Some(name) = ast.name_of(name_id) else {
        return;
    };
    let exported = exported_names(ast, &stmts);
    let edits = match conv {
        Conversion::ToDefault => {
            if exported.iter().any(|&n| n == "default" || n == "export=")
                || exported.iter().filter(|&&n| n == name).count() != 1
            {
                return;
            }
            to_default_here(cx, st, name)
        }
        Conversion::ToNamed => {
            if exported.contains(&name) || exported.contains(&"export=") {
                return;
            }
            if let Node::ExportDefaultExpr { .. } = *node {
                // A global or unresolved name can't be exported by name.
                if f.bound.resolved(name_id).is_none() {
                    return;
                }
                let sp = ast.span(st);
                let semi = if sp.text(text).trim_end().ends_with(';') {
                    ";"
                } else {
                    ""
                };
                Some(vec![Edit::new(
                    sp.start,
                    sp.end,
                    format!("export {{ {name} }}{semi}"),
                )])
            } else {
                let at = ast.span(st).start;
                if !text[at as usize..].starts_with("export") {
                    return;
                }
                let d = token_start(ast, text, at + 6);
                if !text[d as usize..].starts_with("default") {
                    return;
                }
                Some(vec![Edit::delete(d, token_start(ast, text, d + 7))])
            }
        }
    };
    let Some(edits) = edits else {
        return;
    };
    let mut a = Action::new(title, kind);
    a.refactor = Some(format!("{REFACTOR}/{title}"));
    a.edit(&f.path, edits);
    // Importers.
    for (i, other) in cx.checker.files.iter().enumerate() {
        if i == cx.file.0 as usize || other.is_lib || other.is_external {
            continue;
        }
        let Some(oe) = importer_edits(cx, FileId(i as u32), other, conv, name) else {
            return;
        };
        a.edit(&other.path, oe);
    }
    out.push(a);
}

/// The edits of this file for named → default.
fn to_default_here(cx: &Ctx<'_, '_>, st: NodeId, name: &str) -> Option<Vec<Edit>> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let b = &f.bound;
    let sp = ast.span(st);
    if !text[sp.start as usize..].starts_with("export") {
        return None;
    }
    let after_export = token_start(ast, text, sp.start + 6);
    let drop_export = Edit::delete(sp.start, after_export);
    let nl = style::newline(text);
    let semi = style::semi(ast, text);
    let export_after = |blank: bool| {
        Edit::insert(
            sp.end,
            format!(
                "{nl}{}export default {name}{semi}",
                if blank { nl } else { "" }
            ),
        )
    };
    Some(match *ast.node(st) {
        Node::FunctionDecl { .. } | Node::ClassDecl { .. } | Node::Interface { .. } => {
            vec![Edit::insert(after_export, "default ")]
        }
        Node::TypeAlias { .. } | Node::Enum { .. } => vec![drop_export, export_after(false)],
        Node::Module { .. } => vec![drop_export, export_after(true)],
        Node::VarDecl { decls, .. } => {
            let &d = ast.list(decls).first()?;
            let Node::VarDeclarator {
                name: id,
                type_ann,
                init: Some(init),
                ..
            } = *ast.node(d)
            else {
                return None;
            };
            let sym = b.symbol_of_decl(d).or_else(|| b.symbol_of_decl(id))?;
            let used = b.refs.resolved.iter().any(|&(_, s)| s == sym);
            if used || type_ann.is_some() {
                vec![drop_export, export_after(false)]
            } else {
                vec![Edit::new(sp.start, ast.span(init).start, "export default ")]
            }
        }
        _ => return None,
    })
}

/// Edits of one importer of this file (`None`: a use we can't follow, so no action).
fn importer_edits(
    cx: &Ctx<'_, '_>,
    fid: FileId,
    file: &SourceFile,
    conv: Conversion,
    name: &str,
) -> Option<Vec<Edit>> {
    let ast = &file.ast;
    let text = file.text.as_str();
    let b = &file.bound;
    let parents = file.parents();
    let (from, to) = match conv {
        Conversion::ToDefault => (name, "default"),
        Conversion::ToNamed => ("default", name),
    };
    let mut edits = Vec::new();
    for (ri, r) in b.module_requests.iter().enumerate() {
        if cx.checker.link.module_target(fid, ri as u32) != ModuleTarget::File(cx.file) {
            continue;
        }
        match r.kind {
            RequestKind::DynamicImport | RequestKind::ImportType | RequestKind::Augmentation => {
                return None;
            }
            _ => {}
        }
        let st = parents.parent(r.node)?;
        match *ast.node(st) {
            Node::ImportDecl {
                has_clause: false, ..
            }
            | Node::ExportAll { .. } => {}
            Node::ImportDecl {
                specifiers, source, ..
            } => {
                let specs = ast.list(specifiers);
                for &sp in specs {
                    if let Node::ImportNamespaceSpecifier { local } = *ast.node(sp) {
                        namespace_uses(ast, b, parents, sp, local, from, to, &mut edits);
                    }
                }
                match conv {
                    Conversion::ToDefault => {
                        edits.extend(named_to_default_import(ast, text, specs, name)?);
                    }
                    Conversion::ToNamed => {
                        edits.extend(default_to_named_import(ast, text, st, specs, source, name)?);
                    }
                }
            }
            Node::ImportEquals { name: local, .. } => {
                namespace_uses(ast, b, parents, st, local, from, to, &mut edits);
            }
            Node::ExportNamed { specifiers, .. } => {
                for &sp in ast.list(specifiers) {
                    let Node::ExportSpecifier {
                        local, exported, ..
                    } = *ast.node(sp)
                    else {
                        continue;
                    };
                    if ast.name_of(local) != Some(from) {
                        continue;
                    }
                    let lsp = ast.span(local);
                    match conv {
                        Conversion::ToDefault => {
                            let t = if exported.is_some() {
                                "default".to_string()
                            } else {
                                format!("default as {name}")
                            };
                            edits.push(Edit::new(lsp.start, lsp.end, t));
                        }
                        Conversion::ToNamed => {
                            let ex = exported.and_then(|e| ast.name_of(e)).unwrap_or("default");
                            let t = if ex == name {
                                name.to_string()
                            } else {
                                format!("{name} as {ex}")
                            };
                            let ssp = ast.span(sp);
                            edits.push(Edit::new(ssp.start, ssp.end, t));
                        }
                    }
                }
            }
            _ => return None,
        }
    }
    Some(edits)
}

/// `ns.from` → `ns.to` for the uses of namespace import `local` (declared by `decl`).
#[allow(clippy::too_many_arguments)]
fn namespace_uses(
    ast: &Ast,
    b: &BoundFile,
    parents: &ts_r_ast::ParentMap,
    decl: NodeId,
    local: NodeId,
    from: &str,
    to: &str,
    edits: &mut Vec<Edit>,
) {
    let Some(sym) = b.symbol_of_decl(decl).or_else(|| b.symbol_of_decl(local)) else {
        return;
    };
    for &(n, s) in &b.refs.resolved {
        if s != sym {
            continue;
        }
        let Some(p) = parents.parent(n) else {
            continue;
        };
        let prop = match *ast.node(p) {
            Node::Member { obj, prop, .. } if obj == n => prop,
            Node::QualifiedName { left, right } if left == n => right,
            _ => continue,
        };
        if ast.name_of(prop) == Some(from) {
            let sp = ast.span(prop);
            edits.push(Edit::new(sp.start, sp.end, to));
        }
    }
}

/// The open and close braces of an import's named specifiers.
fn braces(ast: &Ast, text: &str, named: &[NodeId]) -> Option<(u32, u32)> {
    let first = ast.span(*named.first()?).start;
    let last = ast.span(*named.last()?).end;
    let open = text[..first as usize].rfind('{')? as u32;
    let close = last + text[last as usize..].find('}')? as u32;
    Some((open, close))
}

/// `import { name as x, ... }` → `import x, { ... }` (`None`: can't).
fn named_to_default_import(
    ast: &Ast,
    text: &str,
    specs: &[NodeId],
    name: &str,
) -> Option<Vec<Edit>> {
    let named: Vec<NodeId> = specs
        .iter()
        .copied()
        .filter(|&s| matches!(ast.node(s), Node::ImportSpecifier { .. }))
        .collect();
    let imported = |s: NodeId| match *ast.node(s) {
        Node::ImportSpecifier {
            imported, local, ..
        } => ast.name_of(imported.unwrap_or(local)),
        _ => None,
    };
    let hits: Vec<usize> = (0..named.len())
        .filter(|&i| imported(named[i]) == Some(name))
        .collect();
    let &[j] = hits.as_slice() else {
        return if hits.is_empty() {
            Some(Vec::new())
        } else {
            None
        };
    };
    if specs
        .iter()
        .any(|&s| matches!(ast.node(s), Node::ImportDefaultSpecifier { .. }))
    {
        return None;
    }
    let Node::ImportSpecifier { local, .. } = *ast.node(named[j]) else {
        return None;
    };
    let local = ast.name_of(local)?;
    let (open, close) = braces(ast, text, &named)?;
    if named.len() == 1 {
        return Some(vec![Edit::new(open, close + 1, local)]);
    }
    let mut edits = super::unused::delete_element(ast, &named, j);
    edits.push(Edit::insert(open, format!("{local}, ")));
    Some(edits)
}

/// `import x, { ... }` → `import { ..., name as x }`, `{ default as x }` → `{ name as x }`.
fn default_to_named_import(
    ast: &Ast,
    text: &str,
    st: NodeId,
    specs: &[NodeId],
    source: NodeId,
    name: &str,
) -> Option<Vec<Edit>> {
    let named: Vec<NodeId> = specs
        .iter()
        .copied()
        .filter(|&s| matches!(ast.node(s), Node::ImportSpecifier { .. }))
        .collect();
    let spec_of = |local: &str| {
        if local == name {
            name.to_string()
        } else {
            format!("{name} as {local}")
        }
    };
    let mut edits = Vec::new();
    for &s in &named {
        if let Node::ImportSpecifier {
            imported: Some(imp),
            local,
            ..
        } = *ast.node(s)
            && ast.name_of(imp) == Some("default")
        {
            let sp = ast.span(s);
            edits.push(Edit::new(sp.start, sp.end, spec_of(ast.name_of(local)?)));
        }
    }
    let Some(&def) = specs
        .iter()
        .find(|&&s| matches!(ast.node(s), Node::ImportDefaultSpecifier { .. }))
    else {
        return Some(edits);
    };
    if !edits.is_empty() {
        return None;
    }
    let Node::ImportDefaultSpecifier { local } = *ast.node(def) else {
        return None;
    };
    let spec = spec_of(ast.name_of(local)?);
    let dsp = ast.span(def);
    if let Some(&ns) = specs
        .iter()
        .find(|&&s| matches!(ast.node(s), Node::ImportNamespaceSpecifier { .. }))
    {
        // `import x, * as ns from "m"` → `import * as ns from "m"` and `import { a as x } from "m"`.
        edits.push(Edit::delete(dsp.start, ast.span(ns).start));
        let ssp = ast.span(st);
        let semi = if ssp.text(text).trim_end().ends_with(';') {
            ";"
        } else {
            ""
        };
        let nl = style::newline(text);
        edits.push(Edit::insert(
            ssp.end,
            format!(
                "{nl}import {{ {spec} }} from {}{semi}",
                ast.span(source).text(text)
            ),
        ));
        return Some(edits);
    }
    if named.is_empty() {
        edits.push(Edit::new(dsp.start, dsp.end, format!("{{ {spec} }}")));
        return Some(edits);
    }
    let (open, _) = braces(ast, text, &named)?;
    edits.push(Edit::delete(dsp.start, open));
    let last = ast.span(*named.last()?).end;
    edits.push(Edit::insert(last, format!(", {spec}")));
    Some(edits)
}
