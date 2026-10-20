//! Unused declarations (TS6133, TS6138, TS6192, TS6196, TS6198, TS6199, TS6205), as tsserver
//! 6.0.3 fixes them on the `fix-unused-*` fixtures:
//!
//! - "Remove unused declaration for: 'x'" deletes the declaration: an import specifier (with
//!   its comma), a variable (the whole statement when it is the only one), a destructuring
//!   element, a function, class, interface, type alias or enum with the comments attached
//!   above it (whole lines), a private member, a type parameter, a parameter when every
//!   parameter after it goes too.
//! - "Remove import from 'm'" deletes an import whose bindings are all unused.
//! - "Prefix 'x' with an underscore" renames an unused parameter.
//! - "Delete all unused declarations" deletes everything above except imports.

use super::Ctx;
use super::edit::Edit;
use super::fixes::{Diag, Fix, node_covering};
use super::style;
use ts_r_ast::{Ast, Node, NodeId, Span};
use ts_r_checker::Ck;

pub const CODES: &[u32] = &[6133, 6138, 6192, 6196, 6198, 6199, 6205];

const DELETE: &str = "unusedIdentifier_delete";
const PREFIX: &str = "unusedIdentifier_prefix";

/// What an unused diagnostic is about.
enum Target {
    /// A whole import declaration (`specifier` text).
    Import(NodeId, String),
    /// One binding of an import with others.
    ImportBinding(NodeId),
    Statement(NodeId),
    /// A declarator among others.
    Declarator(NodeId),
    /// A destructuring element.
    PatternElement(NodeId),
    /// A declaration removed with its lines (function, class, ...).
    Declaration(NodeId),
    /// A class member.
    Member(NodeId),
    Param(NodeId),
    TypeParam(NodeId),
    TypeParams(NodeId),
}

pub fn fixes(cx: &Ctx<'_, '_>, _ck: &mut Ck<'_, '_>, d: &Diag<'_>) -> Vec<Fix> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let Some((target, name)) = target(cx, d) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let remove = |t: String| format!("Remove unused declaration for: '{t}'");
    match target {
        Target::Import(n, spec) => {
            out.push(Fix::new(
                format!("Remove import from '{spec}'"),
                DELETE,
                delete_lines(ast, text, n),
            ));
        }
        Target::Statement(n) => {
            let title = match d.code {
                6199 => "Remove variable statement".to_string(),
                6198 => "Remove destructuring".to_string(),
                _ => remove(name),
            };
            out.push(Fix::new(title, DELETE, delete_lines(ast, text, n)));
        }
        Target::Param(p) => {
            if let Some(e) = delete_param(cx, p, &[]) {
                out.push(Fix::new(remove(name.clone()), DELETE, e));
            }
            if let Node::Param { pattern, .. } = *ast.node(p)
                && matches!(ast.node(pattern), Node::Ident { .. })
            {
                out.push(Fix::new(
                    format!("Prefix '{name}' with an underscore"),
                    PREFIX,
                    vec![Edit::insert(ast.span(pattern).start, "_")],
                ));
            }
        }
        Target::TypeParams(n) => {
            let sp = ast.span(n);
            out.push(Fix::new(
                "Remove type parameters",
                DELETE,
                vec![Edit::delete(sp.start, sp.end)],
            ));
        }
        other => {
            if let Some(e) = delete_target(cx, &other) {
                out.push(Fix::new(remove(name), DELETE, e));
            }
        }
    }
    out
}

/// The node an unused diagnostic is about, and the name it reports.
fn target(cx: &Ctx<'_, '_>, d: &Diag<'_>) -> Option<(Target, String)> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let parents = f.parents();
    // The name itself when the diagnostic is on one (a shorthand specifier or binding has
    // the same span), else the node it covers.
    let n = match super::super::locate::ident_at(ast, d.start) {
        Some(i) if ast.span(i).start == d.start && ast.span(i).end == d.end => i,
        _ => node_covering(cx, d.start, d.end)?,
    };
    let name = ast.span(n).text(text).to_string();
    let spec_of = |decl: NodeId| match *ast.node(decl) {
        Node::ImportDecl { source, .. } => match *ast.node(source) {
            Node::StringLit { value } => Some(ast.atom(value).to_string()),
            _ => None,
        },
        _ => None,
    };
    match *ast.node(n) {
        Node::ImportDecl { .. } => return Some((Target::Import(n, spec_of(n)?), name)),
        Node::VarDecl { .. } => return Some((Target::Statement(n), name)),
        Node::TypeParams { .. } => return Some((Target::TypeParams(n), name)),
        Node::ObjectPattern { .. } | Node::ArrayPattern { .. } => {
            let decl = parents.parent(n)?;
            let list = parents.parent(decl)?;
            return match (ast.node(decl), ast.node(list)) {
                (Node::VarDeclarator { .. }, Node::VarDecl { decls, .. })
                    if ast.list(*decls).len() == 1 =>
                {
                    Some((Target::Statement(list), name))
                }
                _ => None,
            };
        }
        Node::Ident { .. } => {}
        _ => return None,
    }
    let p = parents.parent(n)?;
    let t = match *ast.node(p) {
        Node::ImportSpecifier { .. }
        | Node::ImportDefaultSpecifier { .. }
        | Node::ImportNamespaceSpecifier { .. } => {
            let decl = parents.parent(p)?;
            let Node::ImportDecl { specifiers, .. } = *ast.node(decl) else {
                return None;
            };
            if ast.list(specifiers).len() == 1 {
                Target::Import(decl, spec_of(decl)?)
            } else {
                Target::ImportBinding(p)
            }
        }
        Node::VarDeclarator { name: nm, .. } if nm == n => {
            let list = parents.parent(p)?;
            match *ast.node(list) {
                Node::VarDecl { decls, .. } => {
                    let in_for = parents.parent(list).is_some_and(|g| {
                        matches!(
                            ast.node(g),
                            Node::ForIn { .. } | Node::ForOf { .. } | Node::For { .. }
                        )
                    });
                    if in_for {
                        return None;
                    }
                    if ast.list(decls).len() == 1 {
                        Target::Statement(list)
                    } else {
                        Target::Declarator(p)
                    }
                }
                _ => return None,
            }
        }
        Node::BindingProperty { .. } => Target::PatternElement(p),
        Node::ObjectPattern { .. } | Node::ArrayPattern { .. } => Target::PatternElement(n),
        Node::FunctionDecl { name: Some(nm), .. } | Node::ClassDecl { name: Some(nm), .. }
            if nm == n =>
        {
            Target::Declaration(p)
        }
        Node::Interface { name: nm, .. }
        | Node::TypeAlias { name: nm, .. }
        | Node::Enum { name: nm, .. }
            if nm == n =>
        {
            Target::Declaration(p)
        }
        Node::PropertyDef { key, .. } | Node::MethodDef { key, .. } if key == n => {
            Target::Member(p)
        }
        Node::Param { pattern, .. } if pattern == n => Target::Param(p),
        Node::TypeParam { .. } => Target::TypeParam(p),
        _ => return None,
    };
    Some((t, name))
}

/// Edits deleting a target (not `Param`, `Import`, `Statement`, `TypeParams`: callers handle those).
fn delete_target(cx: &Ctx<'_, '_>, t: &Target) -> Option<Vec<Edit>> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let parents = f.parents();
    match *t {
        Target::Import(n, _)
        | Target::Statement(n)
        | Target::Declaration(n)
        | Target::Member(n) => Some(delete_lines(ast, text, n)),
        Target::TypeParams(n) => Some(vec![Edit::delete(ast.span(n).start, ast.span(n).end)]),
        Target::ImportBinding(sp) => {
            let decl = parents.parent(sp)?;
            let Node::ImportDecl { specifiers, .. } = *ast.node(decl) else {
                return None;
            };
            let all = ast.list(specifiers);
            match *ast.node(sp) {
                Node::ImportSpecifier { .. } => {
                    let named: Vec<NodeId> = all
                        .iter()
                        .copied()
                        .filter(|&x| matches!(ast.node(x), Node::ImportSpecifier { .. }))
                        .collect();
                    if named.len() == 1 {
                        // `import D, { x } from`: drop `, { x }`.
                        let prev = all.iter().copied().take_while(|&x| x != sp).last()?;
                        let close = text[ast.span(sp).end as usize..].find('}')? as u32
                            + ast.span(sp).end
                            + 1;
                        return Some(vec![Edit::delete(ast.span(prev).end, close)]);
                    }
                    let i = named.iter().position(|&x| x == sp)?;
                    Some(delete_element(ast, &named, i))
                }
                _ => {
                    let i = all.iter().position(|&x| x == sp)?;
                    if i == 0 {
                        // `import D, { a }` → `import { a }`.
                        let next = *all.get(1)?;
                        let to = text[ast.span(sp).end as usize..ast.span(next).start as usize]
                            .find(',')
                            .map(|c| ast.span(sp).end + c as u32 + 1)?;
                        let to = to
                            + (text[to as usize..].len() - text[to as usize..].trim_start().len())
                                as u32;
                        Some(vec![Edit::delete(ast.span(sp).start, to)])
                    } else {
                        Some(delete_element(ast, all, i))
                    }
                }
            }
        }
        Target::Declarator(dn) => {
            let list = parents.parent(dn)?;
            let Node::VarDecl { decls, .. } = *ast.node(list) else {
                return None;
            };
            let items = ast.list(decls);
            let i = items.iter().position(|&x| x == dn)?;
            Some(delete_element(ast, items, i))
        }
        Target::PatternElement(el) => {
            let pat = parents.parent(el)?;
            match *ast.node(pat) {
                Node::ObjectPattern { props } => {
                    let items = ast.list(props);
                    let i = items.iter().position(|&x| x == el)?;
                    Some(delete_element(ast, items, i))
                }
                Node::ArrayPattern { elements } => {
                    let items = ast.list(elements);
                    let i = items.iter().position(|&x| x == el)?;
                    if i + 1 == items.len() {
                        Some(delete_element(ast, items, i))
                    } else {
                        // Keep the hole: `[, b]`.
                        Some(vec![Edit::delete(ast.span(el).start, ast.span(el).end)])
                    }
                }
                _ => None,
            }
        }
        Target::TypeParam(tp) => {
            let list = parents.parent(tp)?;
            let Node::TypeParams { params } = *ast.node(list) else {
                return None;
            };
            let items = ast.list(params);
            if items.len() == 1 {
                return Some(vec![Edit::delete(ast.span(list).start, ast.span(list).end)]);
            }
            let i = items.iter().position(|&x| x == tp)?;
            Some(delete_element(ast, items, i))
        }
        Target::Param(p) => delete_param(cx, p, &[]),
    }
}

/// Deleting parameter `p` (only when every parameter after it is unused: `also` lists those
/// being deleted with it).
fn delete_param(cx: &Ctx<'_, '_>, p: NodeId, also: &[NodeId]) -> Option<Vec<Edit>> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let func = f.parents().parent(p)?;
    let params = match *ast.node(func) {
        Node::FunctionDecl {
            params,
            body: Some(_),
            ..
        }
        | Node::FunctionExpr {
            params,
            body: Some(_),
            ..
        }
        | Node::Arrow { params, .. } => params,
        _ => return None,
    };
    let items = ast.list(params);
    let i = items.iter().position(|&x| x == p)?;
    if items[i + 1..].iter().any(|x| !also.contains(x)) {
        return None;
    }
    // A method or a callback's parameter list follows a contract: keep it.
    if let Some(g) = f.parents().parent(func)
        && matches!(
            ast.node(g),
            Node::MethodDef { .. } | Node::Call { .. } | Node::New { .. }
        )
    {
        return None;
    }
    // The trailing run of deleted parameters goes in one edit, made for its first one.
    let mut k = i;
    while k > 0 && also.contains(&items[k - 1]) {
        k -= 1;
    }
    if k != i {
        return Some(Vec::new());
    }
    let end = ast.span(items[items.len() - 1]).end;
    let start = if k == 0 {
        ast.span(items[0]).start
    } else {
        ast.span(items[k - 1]).end
    };
    Some(vec![Edit::delete(start, end)])
}

/// Deletes element `i` of a comma-separated list with its separator.
pub(super) fn delete_element(ast: &Ast, items: &[NodeId], i: usize) -> Vec<Edit> {
    let sp = ast.span(items[i]);
    if let Some(&next) = items.get(i + 1) {
        return vec![Edit::delete(sp.start, ast.span(next).start)];
    }
    if i > 0 {
        return vec![Edit::delete(ast.span(items[i - 1]).end, sp.end)];
    }
    vec![Edit::delete(sp.start, sp.end)]
}

/// Deletes a statement or member with the comments attached above it: whole lines when it
/// has its lines to itself.
pub(super) fn delete_lines(ast: &Ast, text: &str, n: NodeId) -> Vec<Edit> {
    let sp = ast.span(n);
    let mut start = sp.start;
    // Comments directly above (no blank line between).
    let ls = style::line_start(text, start);
    if style::blank(text, ls, start) {
        let before = ast.comments_in(Span::new(0, ls));
        let mut cur = ls;
        for c in before.iter().rev() {
            let gap = &text[c.span.end as usize..cur as usize];
            if gap.trim().is_empty() && gap.matches('\n').count() <= 1 {
                let cl = style::line_start(text, c.span.start);
                if !style::blank(text, cl, c.span.start) {
                    break;
                }
                cur = cl;
            } else {
                break;
            }
        }
        start = cur;
    }
    let after = style::next_line(text, sp.end);
    let line_rest_blank = text[sp.end as usize..after as usize].trim().is_empty();
    if style::blank(text, style::line_start(text, start), start) && line_rest_blank {
        return vec![Edit::delete(style::line_start(text, start), after)];
    }
    vec![Edit::delete(start, sp.end)]
}

/// "Delete all unused declarations": every unused declaration but imports.
pub fn delete_all(cx: &Ctx<'_, '_>, _ck: &mut Ck<'_, '_>, all: &[Diag<'_>]) -> Option<Vec<Edit>> {
    let mut targets: Vec<Target> = Vec::new();
    let mut params: Vec<NodeId> = Vec::new();
    for d in all.iter().filter(|d| CODES.contains(&d.code)) {
        let Some((t, _)) = target(cx, d) else {
            continue;
        };
        match t {
            Target::Import(..) | Target::ImportBinding(_) => {}
            Target::Param(p) => params.push(p),
            t => targets.push(t),
        }
    }
    let mut edits: Vec<Edit> = Vec::new();
    let mut count = 0;
    let push = |e: Vec<Edit>, edits: &mut Vec<Edit>| {
        for x in e {
            if !edits
                .iter()
                .any(|y| y.start < x.end && x.start < y.end || y == &x)
            {
                edits.push(x);
            }
        }
    };
    for t in &targets {
        if let Some(e) = delete_target(cx, t) {
            count += 1;
            push(e, &mut edits);
        }
    }
    for &p in &params {
        if let Some(e) = delete_param(cx, p, &params) {
            count += 1;
            push(e, &mut edits);
        }
    }
    (count >= 2).then_some(edits)
}
