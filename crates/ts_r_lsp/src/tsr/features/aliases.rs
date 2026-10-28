//! Import and export aliases in reference searches.
//!
//! A search starts from the symbol at the cursor followed through *transparent* aliases:
//! an import or export specifier that keeps its name (`import { a }`, `export { a }`,
//! `export { a } from "m"`) and a default import (`import d from "m"`, which names the
//! module's default export). It stops at the first alias that gives a new name
//! (`import { a as b }`, `export { a as b }`, `import * as ns`, `import x = ...`,
//! `export default expr`) or at the declared symbol. A name then refers to the search
//! symbol when the symbol is on the name's alias chain, so a search from a declaration
//! also finds every alias of it under another name and that alias's uses, and a search
//! from a renaming alias finds only what goes through it. Learned from tsc 7's answers on
//! the fourslash tests and `tsc: tsc/internal/ls/importTracker.go` (v7.0.2).

use ts_r_ast::{Modifiers, Node, NodeId};
use ts_r_binder::SymbolFlags;
use ts_r_checker::{Checker, Loc};
use ts_r_diagnostics::FileId;
use ts_r_types::SymId;

/// The symbols a name standing for `x` goes through: `x`, every alias after it, and the
/// symbol they resolve to (just `x` when it is no alias).
pub fn chain(c: &Checker<'_>, x: SymId) -> Vec<SymId> {
    if !c.sym_flags(x).intersects(SymbolFlags::ALIAS) {
        return vec![x];
    }
    let mut out = vec![x];
    if let Some(r) = c.parts(x).next() {
        for a in c.link.alias_chain(r) {
            let s = c.sym_ref(a);
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    if let Some(t) = c.resolve_alias(x)
        && !out.contains(&t)
    {
        out.push(t);
    }
    out
}

/// The alias declaration node of `x`, if `x` is an alias.
fn alias_decl(c: &Checker<'_>, x: SymId) -> Option<Loc> {
    if !c.sym_flags(x).intersects(SymbolFlags::ALIAS) {
        return None;
    }
    c.decls(x).into_iter().find(|d| {
        matches!(
            c.node(*d),
            Node::ImportSpecifier { .. }
                | Node::ImportDefaultSpecifier { .. }
                | Node::ImportNamespaceSpecifier { .. }
                | Node::ImportEquals { .. }
                | Node::ExportSpecifier { .. }
                | Node::ExportDefaultExpr { .. }
                | Node::ExportAssignment { .. }
                | Node::ExportAll { .. }
        )
    })
}

/// Whether the alias `x` keeps the name of what it stands for (see the module docs). A
/// default import names its own local for a rename.
fn is_transparent(c: &Checker<'_>, x: SymId, rename: bool) -> bool {
    match alias_decl(c, x).map(|d| *c.node(d)) {
        Some(Node::ImportSpecifier { imported, .. }) => imported.is_none(),
        Some(Node::ExportSpecifier { exported, .. }) => exported.is_none(),
        Some(Node::ImportDefaultSpecifier { .. }) => !rename,
        _ => false,
    }
}

/// What the alias `x` stands for, one step on.
pub fn immediate(c: &Checker<'_>, x: SymId) -> Option<SymId> {
    chain(c, x).get(1).copied()
}

/// What a name on the imported side of a renaming specifier (`a` of `{ a as b }` or
/// `export { a as b }`) or the entity of `export default a` / `export = a` stands for:
/// the specifier's immediate target.
pub fn imported_side(c: &Checker<'_>, l: Loc) -> Option<SymId> {
    let p = c.parent(l)?;
    let on = match *c.node(p) {
        Node::ImportSpecifier {
            imported: Some(i), ..
        } => i == l.node,
        Node::ExportSpecifier {
            local,
            exported: Some(_),
            ..
        } => local == l.node,
        Node::ExportDefaultExpr { expr } | Node::ExportAssignment { expr } => expr == l.node,
        _ => false,
    };
    if !on {
        return None;
    }
    immediate(c, c.sym_of_decl(p)?)
}

/// Where a search from `x` starts: `x` followed through transparent aliases.
pub fn start(c: &Checker<'_>, x: SymId, rename: bool) -> SymId {
    let mut s = x;
    for _ in 0..64 {
        if !is_transparent(c, s, rename) {
            break;
        }
        match immediate(c, s) {
            Some(t) if t != s => s = t,
            _ => break,
        }
    }
    s
}

/// Whether `s` is an alias.
pub fn is_alias(c: &Checker<'_>, s: SymId) -> bool {
    c.sym_flags(s).intersects(SymbolFlags::ALIAS)
}

/// The alias declarations of a file: import clauses and export specifiers, at the top
/// level and in module blocks.
fn alias_nodes(c: &Checker<'_>, f: FileId) -> Vec<NodeId> {
    let ast = c.ast(f);
    let mut out = Vec::new();
    let mut stack: Vec<(NodeId, u32)> = vec![(ast.root, 0)];
    while let Some((n, depth)) = stack.pop() {
        match *ast.node(n) {
            Node::Program { stmts } | Node::ModuleBlock { stmts } => {
                for &s in ast.list(stmts) {
                    stack.push((s, depth + 1));
                }
            }
            Node::Module { body: Some(b), .. } if depth < 32 => stack.push((b, depth + 1)),
            Node::ImportDecl { specifiers, .. } | Node::ExportNamed { specifiers, .. } => {
                out.extend(ast.list(specifiers).iter().copied());
            }
            // `import x = require("m")`; an entity alias (`import x = N.y`) is a name of
            // its own that searches for `N.y` do not follow.
            Node::ImportEquals { module_ref, .. }
                if matches!(ast.node(module_ref), Node::ExternalModuleRef { .. }) =>
            {
                out.push(n)
            }
            _ => {}
        }
    }
    out
}

/// The name an alias declaration introduces (`b` of `{ a as b }`, `d` of `import d`).
fn introduced_name(c: &Checker<'_>, f: FileId, n: NodeId) -> Option<NodeId> {
    match *c.ast(f).node(n) {
        Node::ImportSpecifier { local, .. }
        | Node::ImportDefaultSpecifier { local }
        | Node::ImportNamespaceSpecifier { local } => Some(local),
        Node::ExportSpecifier {
            local, exported, ..
        } => Some(exported.unwrap_or(local)),
        Node::ImportEquals { name, .. } => Some(name),
        _ => None,
    }
}

/// The names under which aliases in `files` stand for `s` (besides `name` itself).
pub fn alias_names(c: &Checker<'_>, files: &[FileId], s: SymId, name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for &f in files {
        let ast = c.ast(f);
        for n in alias_nodes(c, f) {
            let Some(id) = introduced_name(c, f, n) else {
                continue;
            };
            let text = match *ast.node(id) {
                Node::Ident { name } => ast.atom(name),
                Node::StringLit { value } => ast.atom(value),
                _ => continue,
            };
            if text == name || out.iter().any(|o| o == text) {
                continue;
            }
            let Some(x) = c.sym_of_decl(Loc::new(f, n)) else {
                continue;
            };
            if x != s && chain(c, x).contains(&s) {
                out.push(text.to_string());
            }
        }
    }
    out
}

/// On the `default` keyword of `export default ...`: the declaration (or `export default`
/// statement) it belongs to and the keyword's span.
pub fn default_keyword_at(c: &Checker<'_>, file: FileId, offset: u32) -> Option<(Loc, u32, u32)> {
    let text = c.file(file).text.as_str();
    let bytes = text.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let mut s = offset as usize;
    while s > 0 && is_word(bytes[s - 1]) {
        s -= 1;
    }
    let mut e = offset as usize;
    while e < bytes.len() && is_word(bytes[e]) {
        e += 1;
    }
    if text.get(s..e) != Some("default") {
        return None;
    }
    let ast = c.ast(file);
    let mut n = super::locate::node_at(ast, s as u32)?;
    for _ in 0..4 {
        let sp = ast.span(n);
        if sp.start <= s as u32 {
            match *ast.node(n) {
                Node::ExportDefaultExpr { .. } => {
                    return Some((Loc::new(file, n), s as u32, e as u32));
                }
                Node::FunctionDecl { modifiers, .. } | Node::ClassDecl { modifiers, .. }
                    if modifiers.contains(Modifiers::DEFAULT) =>
                {
                    return Some((Loc::new(file, n), s as u32, e as u32));
                }
                _ => {}
            }
        }
        n = c.file(file).parents().parent(n)?;
    }
    None
}

/// The `default` keyword of the declaration `d` when `d` is `export default ...`.
pub fn default_keyword_of(c: &Checker<'_>, d: Loc) -> Option<(u32, u32)> {
    let ast = c.ast(d.file);
    let named = match *ast.node(d.node) {
        Node::ExportDefaultExpr { .. } => false,
        Node::FunctionDecl {
            modifiers, name, ..
        }
        | Node::ClassDecl {
            modifiers, name, ..
        } if modifiers.contains(Modifiers::DEFAULT) => name.is_some(),
        _ => return None,
    };
    if named {
        return None;
    }
    let sp = ast.span(d.node);
    let text = c.file(d.file).text.as_str();
    let head = text.get(sp.start as usize..sp.end as usize)?;
    let i = head.find("default")?;
    let s = sp.start + i as u32;
    Some((s, s + 7))
}
