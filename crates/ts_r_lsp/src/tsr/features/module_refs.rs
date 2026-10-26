//! References to a module (observed on tsc 7.0.2's answers to `findAllRefsForModule`,
//! `referencesForExternalModuleNames`, `findAllRefsModuleDotExports` and the `from` /
//! `require` markers of `referencesForStatementKeywords`; `tsc:
//! tsc/internal/ls/findallreferences.go` explains them).
//!
//! A module is found from a module specifier (`from "./a"`, `require("./a")`, `import("./a")`,
//! an import type, `export * from`), the name of an ambient `declare module "m"`, the path
//! of a `/// <reference path="..." />`, or `module` of `module.exports` (the file's own
//! module). Its references are, in every file searched: each specifier that resolves to the
//! same module, each `/// <reference path>` naming that file, the names of the ambient
//! module's declarations, and, in a CommonJS JavaScript file that is the module, the
//! `module` of each `module.exports`. A reference covers the text between the quotes.
//!
//! The specifier of an import type with a qualifier (`import("./a").T`) refers to the
//! export, not the module. A script named by a specifier has no module references (its
//! literal is found as a string, [`super::string_refs`]).

use ts_r_ast::{Ast, Node, NodeId};
use ts_r_binder::program::ModuleTarget;
use ts_r_checker::Checker;
use ts_r_diagnostics::FileId;

use super::locate::node_at;

/// The module the string literal `lit` of `file` names, as a specifier or as the name of
/// `declare module "m"`.
pub(super) fn literal_module(c: &Checker<'_>, file: FileId, lit: NodeId) -> Option<ModuleTarget> {
    let ast = c.ast(file);
    if !matches!(ast.node(lit), Node::StringLit { .. }) {
        return None;
    }
    if let Some(i) = c
        .bound(file)
        .module_requests
        .iter()
        .position(|r| r.node == lit)
    {
        return is_module(c, c.link.module_target(file, i as u32));
    }
    let parent = c.file(file).parents().parent(lit)?;
    match *ast.node(parent) {
        Node::Module { name, .. } if name == lit => {
            let Node::StringLit { value } = *ast.node(lit) else {
                return None;
            };
            c.link
                .find_ambient_module(ast.atom(value))
                .map(ModuleTarget::Ambient)
        }
        _ => None,
    }
}

/// The module named at `offset`: a specifier or ambient module name under the cursor, or
/// the path of a `/// <reference path="..." />` holding it.
pub(super) fn module_at(c: &Checker<'_>, file: FileId, offset: u32) -> Option<ModuleTarget> {
    let ast = c.ast(file);
    let lit = [Some(offset), offset.checked_sub(1)]
        .into_iter()
        .flatten()
        .filter_map(|o| node_at(ast, o))
        .find(|&n| matches!(ast.node(n), Node::StringLit { .. }) && ast.span(n).start < offset);
    if let Some(lit) = lit {
        return literal_module(c, file, lit);
    }
    // `module` of `module.exports` in a CommonJS file stands for the file's module.
    if let Some(n) = super::locate::ident_at(ast, offset)
        && c.file(file).is_commonjs_module()
        && let Some(p) = c.file(file).parents().parent(n)
        && let Node::Member { obj, prop, .. } = *ast.node(p)
        && obj == n
        && matches!(*ast.node(n), Node::Ident { name } if ast.atom(name) == "module")
        && matches!(*ast.node(prop), Node::Ident { name } if ast.atom(name) == "exports")
    {
        return Some(ModuleTarget::File(file));
    }
    reference_paths(c, file)
        .into_iter()
        .find(|&(s, e, _)| s <= offset && offset <= e)
        .and_then(|(_, _, f)| f)
        .and_then(|f| is_module(c, ModuleTarget::File(f)))
}

/// `t` when it is a module: an ambient module, or a file that is an ES or CommonJS module
/// or JSON. A script named by a specifier has no module references (its literals are
/// found as strings).
fn is_module(c: &Checker<'_>, t: ModuleTarget) -> Option<ModuleTarget> {
    match t {
        ModuleTarget::Ambient(_) => Some(t),
        ModuleTarget::File(f) => {
            let sf = c.file(f);
            (sf.bound.is_module || sf.is_commonjs_module() || sf.is_json()).then_some(t)
        }
        ModuleTarget::Unresolved => None,
    }
}

/// The spans in `files` that refer to module `target` (see the module docs), in file order
/// then source order.
pub(super) fn module_references(
    c: &Checker<'_>,
    files: &[FileId],
    target: ModuleTarget,
) -> Vec<(FileId, u32, u32)> {
    let mut out = Vec::new();
    for &f in files {
        let ast = c.ast(f);
        let first = out.len();
        let parents = c.file(f).parents();
        for (i, r) in c.bound(f).module_requests.iter().enumerate() {
            if c.link.module_target(f, i as u32) == target && !names_export(ast, parents, r.node) {
                let (s, e) = inner_span(ast, r.node);
                out.push((f, s, e));
            }
        }
        if let ModuleTarget::File(t) = target {
            for (s, e, to) in reference_paths(c, f) {
                if to == Some(t) {
                    out.push((f, s, e));
                }
            }
            if t == f && c.file(f).is_js() {
                module_exports_heads(ast, &mut out, f);
            }
        }
        out[first..].sort_unstable_by_key(|r| r.1);
    }
    if let ModuleTarget::Ambient(_) = target {
        for r in c.link.module_symbols(target) {
            let s = c.sym_ref(r);
            for d in c.decls(s) {
                if !files.contains(&d.file) {
                    continue;
                }
                if let Node::Module { name, .. } = *c.node(d)
                    && matches!(c.ast(d.file).node(name), Node::StringLit { .. })
                {
                    let (s, e) = inner_span(c.ast(d.file), name);
                    out.push((d.file, s, e));
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Whether specifier `lit` is that of an import type with a qualifier (`import("m").A`):
/// it refers to the export, not the module.
fn names_export(ast: &Ast, parents: &ts_r_ast::ParentMap, lit: NodeId) -> bool {
    let mut n = lit;
    for _ in 0..2 {
        let Some(p) = parents.parent(n) else {
            return false;
        };
        match *ast.node(p) {
            Node::ImportType { qualifier, .. } => return qualifier.is_some(),
            Node::TypeRef { .. } => n = p,
            _ => return false,
        }
    }
    false
}

/// The text of a string literal without its quotes.
fn inner_span(ast: &Ast, n: NodeId) -> (u32, u32) {
    super::string_refs::inner_span(ast, n)
}

/// `module` of each `module.exports` in `ast` that is not a declared name.
fn module_exports_heads(ast: &Ast, out: &mut Vec<(FileId, u32, u32)>, f: FileId) {
    struct V<'o> {
        out: &'o mut Vec<(FileId, u32, u32)>,
        f: FileId,
    }
    impl ts_r_ast::visit::Visitor for V<'_> {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> ts_r_ast::visit::Walk {
            if let Node::Member { obj, prop, .. } = *ast.node(id)
                && matches!(*ast.node(obj), Node::Ident { name } if ast.atom(name) == "module")
                && matches!(*ast.node(prop), Node::Ident { name } if ast.atom(name) == "exports")
            {
                let s = ast.span(obj);
                self.out.push((self.f, s.start, s.end));
            }
            ts_r_ast::visit::Walk::Continue
        }
    }
    ts_r_ast::visit::walk(ast, ast.root, &mut V { out, f });
}

/// The `/// <reference path="..." />` directives at the top of `file`: the span of each
/// path (without quotes) and the program file it names.
fn reference_paths(c: &Checker<'_>, file: FileId) -> Vec<(u32, u32, Option<FileId>)> {
    let sf = c.file(file);
    if !sf.references.iter().any(|(k, _)| *k == "path") {
        return Vec::new();
    }
    let text = sf.text.as_str();
    let dir = match sf.path.rfind('/') {
        Some(i) => &sf.path[..i],
        None => "",
    };
    let mut out = Vec::new();
    let mut pos = 0usize;
    // Directives come before the first statement: read lines while they are blank or
    // comments, at most this many.
    for _ in 0..4096 {
        if pos >= text.len() {
            break;
        }
        let end = text[pos..].find('\n').map_or(text.len(), |i| pos + i + 1);
        let line = &text[pos..end];
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if trimmed.trim().is_empty() {
            pos = end;
            continue;
        }
        if !trimmed.starts_with("//") {
            break;
        }
        if let Some((s, e)) = path_value(trimmed) {
            let value = &trimmed[s..e];
            let start = (pos + indent + s) as u32;
            let to = resolve_path(c, dir, value);
            out.push((start, start + value.len() as u32, to));
        }
        pos = end;
    }
    out
}

/// The byte range of the quoted `path` value in a `/// <reference path="..." />` line.
fn path_value(line: &str) -> Option<(usize, usize)> {
    let rest = line.strip_prefix("///")?;
    let r = rest.trim_start();
    let r = r.strip_prefix("<reference")?;
    let i = r.find("path")?;
    let after = &r[i + 4..];
    let eq = after.trim_start().strip_prefix('=')?.trim_start();
    let q = eq.chars().next().filter(|&q| q == '"' || q == '\'')?;
    let vs = line.len() - eq.len() + 1;
    let len = line[vs..].find(q)?;
    Some((vs, vs + len))
}

/// The program file `value` names relative to `dir` (as written, or with a TypeScript
/// extension added).
fn resolve_path(c: &Checker<'_>, dir: &str, value: &str) -> Option<FileId> {
    let value = value.replace('\\', "/");
    let full = if value.starts_with('/') || value.get(1..2) == Some(":") {
        normalize(&value)
    } else {
        normalize(&format!("{dir}/{value}"))
    };
    let candidates = [
        full.clone(),
        format!("{full}.ts"),
        format!("{full}.tsx"),
        format!("{full}.d.ts"),
    ];
    candidates.iter().find_map(|p| {
        c.files
            .iter()
            .position(|f| f.path == *p)
            .map(|i| FileId(i as u32))
    })
}

/// `p` with `.` and `..` segments removed (the first segment, a root or drive, stays).
fn normalize(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for (i, seg) in p.split('/').enumerate() {
        match seg {
            "" if i > 0 => {}
            "." => {}
            ".." if parts.len() > 1 => {
                parts.pop();
            }
            ".." => {}
            s => parts.push(s),
        }
    }
    let joined = parts.join("/");
    if joined.is_empty() {
        "/".into()
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_paths() {
        assert_eq!(normalize("/p/c/../a.ts"), "/p/a.ts");
        assert_eq!(normalize("/p/./a.ts"), "/p/a.ts");
        assert_eq!(normalize("C:/p/../a.ts"), "C:/a.ts");
        assert_eq!(normalize("/../a.ts"), "/a.ts");
    }

    #[test]
    fn finds_reference_path_values() {
        let line = "/// <reference path=\"./a.ts\" />";
        let (s, e) = path_value(line).unwrap();
        assert_eq!(&line[s..e], "./a.ts");
        assert_eq!(path_value("/// <reference types=\"node\" />"), None);
        assert_eq!(path_value("// <reference path=\"x\" />"), None);
    }
}
