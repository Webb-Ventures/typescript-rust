//! Whether an expression is a native `URL`, from syntax alone (no type checker), for
//! `unicorn/prefer-url-href`, `no-url-in-search-params` and
//! `no-invalid-url-protocol-comparison`.
//!
//! - A `new` of the global `URL` (no runtime `URL` binding declared before it in any
//!   enclosing scope; type-only bindings don't count) or of a value import of `URL` from
//!   `url`/`node:url` is a URL; any other `new` is not.
//! - A binding with one declaration, declared before the use: a `const` name's initializer
//!   decides, else its annotation (variables and parameters).
//! - A call of a binding with one declaration: its return type annotation.
//! - `as`/`<T>` annotations decide unless unknown; `satisfies`, `!` and parentheses are
//!   looked through; a sequence is its last expression; a `?:` needs both branches.
//! - Annotations: a reference to `URL` without a local binding, or to the `url` module's
//!   `URL` (also `import("url").URL`), is a URL; local aliases are followed; a local class is
//!   not; other references are unknown. Unions mixing URL and non-URL are unknown;
//!   intersections and unions are URLs when every member is.

use super::prelude::*;
use super::unicorn_abort::is_type_only_decl;
use crate::semantic::{Decl, DeclKind, SymbolId};
use ts_r_ast::Ast;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum UrlType {
    Url,
    NonUrl,
    Unknown,
}

const MAX_DEPTH: u32 = 64;

fn combine(types: &[UrlType]) -> UrlType {
    if types.iter().all(|&t| t == UrlType::Url) {
        UrlType::Url
    } else if !types.contains(&UrlType::Unknown) && types.contains(&UrlType::NonUrl) {
        UrlType::NonUrl
    } else {
        UrlType::Unknown
    }
}

fn combine_union(types: &[UrlType]) -> UrlType {
    if types.contains(&UrlType::Url) && types.contains(&UrlType::NonUrl) {
        UrlType::Unknown
    } else {
        combine(types)
    }
}

fn is_url_source(ast: &Ast, source: NodeId) -> bool {
    let string = |id: NodeId| match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value)),
        _ => None,
    };
    let mut value = string(source);
    if value.is_none() {
        ast.for_each_child(source, |c| value = value.or(string(c)));
    }
    matches!(value, Some("url" | "node:url"))
}

/// `import {URL} from "url"` (also `type`-only, also renamed).
fn is_url_import(ast: &Ast, decl: &Decl) -> bool {
    if decl.kind != DeclKind::ImportNamed {
        return false;
    }
    let Node::ImportDecl { source, .. } = *ast.node(decl.parent) else {
        return false;
    };
    let Node::ImportSpecifier {
        imported, local, ..
    } = *ast.node(decl.node)
    else {
        return false;
    };
    let imported = imported.unwrap_or(local);
    is_url_source(ast, source)
        && matches!(ast.node(imported), Node::Ident { .. })
        && ident_name(ast, imported) == Some("URL")
}

fn is_type_only(ast: &Ast, d: &Decl) -> bool {
    is_type_only_decl(ast, d.kind, d.node, d.parent)
}

fn is_value_url_import(ast: &Ast, d: &Decl) -> bool {
    is_url_import(ast, d) && !is_type_only(ast, d)
}

/// A runtime binding named `URL` declared before `node` in a scope around it.
fn has_visible_value_url(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let sem = cx.semantic();
    let ast = cx.ast();
    let at = cx.span(node).start;
    let mut scope = Some(sem.enclosing_scope(node));
    for _ in 0..10_000 {
        let Some(s) = scope else {
            return false;
        };
        let Some(sym) = sem.lookup(s, "URL") else {
            return false;
        };
        let symbol = sem.symbol(sym);
        if symbol
            .decls
            .iter()
            .any(|d| !is_type_only(ast, d) && cx.span(d.name).start <= at)
        {
            return true;
        }
        scope = sem.scope(symbol.scope).parent;
    }
    false
}

fn resolved(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<SymbolId> {
    let sem = cx.semantic();
    sem.reference_at(ident)
        .and_then(|r| sem.reference(r).symbol())
}

fn is_url_constructor(cx: &Ctx<'_, '_>, callee: NodeId) -> bool {
    let ast = cx.ast();
    if !matches!(ast.node(callee), Node::Ident { .. }) {
        return false;
    }
    let sym = resolved(cx, callee);
    let decls = sym.map_or(&[][..], |s| &cx.semantic().symbol(s).decls[..]);
    if ident_name(ast, callee) == Some("URL")
        && !has_visible_value_url(cx, callee)
        && (sym.is_none() || decls.iter().all(|d| is_type_only(ast, d)))
    {
        return true;
    }
    decls.iter().any(|d| is_value_url_import(ast, d))
}

fn is_new_url(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    matches!(*cx.node(id), Node::New { callee, .. } if is_url_constructor(cx, callee))
}

/// The binding a type name refers to. A parameter or return type annotation doesn't see
/// the types declared in the function's body.
fn type_symbol(cx: &Ctx<'_, '_>, name: NodeId) -> Option<SymbolId> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let mut sym = resolved(cx, name)?;
    let text = ident_name(ast, name)?;
    let at = cx.span(name);
    let mut cur = name;
    for _ in 0..10_000 {
        let Some(p) = cx.parent(cur) else {
            break;
        };
        cur = p;
        let body = match *ast.node(p) {
            Node::FunctionDecl { body: Some(b), .. }
            | Node::FunctionExpr { body: Some(b), .. }
            | Node::Arrow { body: b, .. } => b,
            _ => continue,
        };
        let body = cx.span(body);
        let inside = |s: Span| body.start <= s.start && s.end <= body.end;
        if inside(at) {
            break;
        }
        for _ in 0..64 {
            let symbol = sem.symbol(sym);
            if !symbol
                .decls
                .first()
                .is_some_and(|d| inside(cx.span(d.name)))
            {
                break;
            }
            sym = sem.lookup(sem.scope(symbol.scope).parent?, text)?;
        }
    }
    Some(sym)
}

struct Resolver<'c, 'a, 'b> {
    cx: &'c Ctx<'a, 'b>,
    types: Vec<SymbolId>,
    vars: Vec<SymbolId>,
}

impl Resolver<'_, '_, '_> {
    fn reference(&mut self, name: NodeId, depth: u32) -> UrlType {
        let cx = self.cx;
        let ast = cx.ast();
        if !matches!(ast.node(name), Node::Ident { .. }) {
            return UrlType::Unknown;
        }
        let sym = type_symbol(cx, name);
        let decl = sym.and_then(|s| cx.semantic().symbol(s).decls.first().copied());
        let (Some(sym), Some(decl)) = (sym, decl) else {
            return if ident_name(ast, name) == Some("URL") {
                UrlType::Url
            } else {
                UrlType::Unknown
            };
        };
        if self.types.contains(&sym) || depth > MAX_DEPTH {
            return UrlType::Unknown;
        }
        if is_url_import(ast, &decl) {
            return UrlType::Url;
        }
        self.types.push(sym);
        let t = match (decl.kind, *ast.node(decl.node)) {
            (DeclKind::TypeAlias, Node::TypeAlias { ty, .. }) => {
                self.annotation(Some(ty), depth + 1)
            }
            (DeclKind::Class, _) => UrlType::NonUrl,
            _ => UrlType::Unknown,
        };
        self.types.retain(|&s| s != sym);
        t
    }

    fn annotation(&mut self, ty: Option<NodeId>, depth: u32) -> UrlType {
        let ast = self.cx.ast();
        let Some(ty) = ty else {
            return UrlType::Unknown;
        };
        if depth > MAX_DEPTH {
            return UrlType::Unknown;
        }
        match *ast.node(ty) {
            Node::ParenType { ty } => self.annotation(Some(ty), depth + 1),
            Node::TypeRef { name, .. } => self.reference(name, depth + 1),
            Node::UnionType { types } => {
                let ts: Vec<UrlType> = ast
                    .list(types)
                    .iter()
                    .map(|&t| self.annotation(Some(t), depth + 1))
                    .collect();
                combine_union(&ts)
            }
            Node::IntersectionType { types } => {
                let ts: Vec<UrlType> = ast
                    .list(types)
                    .iter()
                    .map(|&t| self.annotation(Some(t), depth + 1))
                    .collect();
                combine(&ts)
            }
            Node::ImportType { arg, qualifier, .. } => {
                if is_url_source(ast, arg)
                    && qualifier.is_some_and(|q| {
                        matches!(ast.node(q), Node::Ident { .. })
                            && ident_name(ast, q) == Some("URL")
                    })
                {
                    UrlType::Url
                } else {
                    UrlType::NonUrl
                }
            }
            _ => UrlType::NonUrl,
        }
    }

    fn variable(&mut self, ident: NodeId, depth: u32) -> UrlType {
        let cx = self.cx;
        let ast = cx.ast();
        let sem = cx.semantic();
        let Some(sym) = resolved(cx, ident) else {
            return UrlType::Unknown;
        };
        let symbol = sem.symbol(sym);
        if self.vars.contains(&sym) || symbol.decls.len() != 1 || depth > MAX_DEPTH {
            return UrlType::Unknown;
        }
        let decl = symbol.decls[0];
        if cx.span(decl.name).start > cx.span(ident).start {
            return UrlType::Unknown;
        }
        self.vars.push(sym);
        let (from_init, type_ann) = match *ast.node(decl.node) {
            Node::VarDeclarator {
                name,
                type_ann,
                init,
                ..
            } if name == decl.name => {
                let init = match init {
                    Some(init)
                        if decl.kind == DeclKind::Const
                            && matches!(ast.node(name), Node::Ident { .. }) =>
                    {
                        self.expression(init, depth + 1)
                    }
                    _ => UrlType::Unknown,
                };
                (init, type_ann)
            }
            Node::Param {
                pattern, type_ann, ..
            } if pattern == decl.name => (UrlType::Unknown, type_ann),
            _ => (UrlType::Unknown, None),
        };
        let annotation = self.annotation(type_ann, depth + 1);
        self.vars.retain(|&s| s != sym);
        if from_init == UrlType::Unknown {
            annotation
        } else {
            from_init
        }
    }

    fn function_return(&mut self, id: NodeId, depth: u32) -> UrlType {
        let cx = self.cx;
        let ast = cx.ast();
        let Node::Call { callee, .. } = *ast.node(id) else {
            return UrlType::Unknown;
        };
        if !matches!(ast.node(callee), Node::Ident { .. }) {
            return UrlType::Unknown;
        }
        let Some(sym) = resolved(cx, callee) else {
            return UrlType::Unknown;
        };
        let &[decl] = &cx.semantic().symbol(sym).decls[..] else {
            return UrlType::Unknown;
        };
        let return_type = match *ast.node(decl.node) {
            Node::FunctionDecl { return_type, .. }
            | Node::FunctionExpr { return_type, .. }
            | Node::Arrow { return_type, .. } => return_type,
            _ => None,
        };
        self.annotation(return_type, depth + 1)
    }

    fn expression(&mut self, id: NodeId, depth: u32) -> UrlType {
        let cx = self.cx;
        let ast = cx.ast();
        if depth > MAX_DEPTH {
            return UrlType::Unknown;
        }
        match *ast.node(id) {
            Node::Ident { .. } => return self.variable(id, depth + 1),
            Node::As { expr, ty } | Node::TypeAssertion { expr, ty } => {
                return match self.annotation(Some(ty), depth + 1) {
                    UrlType::Unknown => self.expression(expr, depth + 1),
                    t => t,
                };
            }
            Node::Satisfies { expr, .. } | Node::NonNull { expr } | Node::Paren { expr } => {
                return self.expression(expr, depth + 1);
            }
            Node::Seq { exprs } => {
                return match ast.list(exprs).last() {
                    Some(&last) => self.expression(last, depth + 1),
                    None => UrlType::Unknown,
                };
            }
            Node::Conditional { cons, alt, .. } => {
                let ts = [
                    self.expression(cons, depth + 1),
                    self.expression(alt, depth + 1),
                ];
                return combine(&ts);
            }
            // Without type information any other constructor makes a non-URL.
            Node::New { .. } => {
                return if is_new_url(cx, id) {
                    UrlType::Url
                } else {
                    UrlType::NonUrl
                };
            }
            _ => {}
        }
        self.function_return(id, depth + 1)
    }
}

/// What `id` is known to be.
pub(crate) fn url_type(cx: &Ctx<'_, '_>, id: NodeId) -> UrlType {
    Resolver {
        cx,
        types: Vec::new(),
        vars: Vec::new(),
    }
    .expression(id, 0)
}

/// `id` is known to be a native `URL`.
pub(crate) fn is_url(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    url_type(cx, id) == UrlType::Url
}

/// `id` constructs a new native `URL` (through TS wrappers, parentheses, the last expression
/// of a sequence, or both branches of a `?:`).
pub(crate) fn is_fresh_url(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    fresh(cx, id, 0)
}

fn fresh(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    let ast = cx.ast();
    if depth > MAX_DEPTH {
        return false;
    }
    let mut id = id;
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::TypeAssertion { expr, .. }
            | Node::Paren { expr }
            | Node::NonNull { expr } => id = expr,
            Node::Seq { exprs } => match ast.list(exprs).last() {
                Some(&last) => id = last,
                None => return false,
            },
            Node::Conditional { cons, alt, .. } => {
                return fresh(cx, cons, depth + 1) && fresh(cx, alt, depth + 1);
            }
            Node::New { .. } => return is_new_url(cx, id),
            _ => return false,
        }
    }
    false
}
