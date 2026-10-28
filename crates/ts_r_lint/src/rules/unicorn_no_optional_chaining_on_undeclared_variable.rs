//! `unicorn/no-optional-chaining-on-undeclared-variable`: `foo?.bar` / `foo.a?.()` where
//! the leftmost name under the chain's innermost optional link is declared nowhere at
//! runtime (no value declaration in any enclosing scope, type-only declarations and
//! imports don't count, and not a configured or built-in global): reading it throws
//! before `?.` can help. Reported once per name occurrence.

use super::prelude::*;
use super::unicorn_static::skip_ts_only;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-optional-chaining-on-undeclared-variable",
        "Disallow optional chaining on undeclared variables.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoOptionalChainingOnUndeclaredVariable>,
};

#[derive(Debug, Default)]
struct NoOptionalChainingOnUndeclaredVariable;

/// The object or callee of the innermost optional link along `id`'s object/callee path.
fn optional_base(ast: &Ast, id: NodeId, depth: u32) -> Option<NodeId> {
    if depth > 10_000 {
        return None;
    }
    let id = skip_ts_only(ast, id);
    let (inner, optional) = match *ast.node(id) {
        Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => (obj, optional),
        Node::Call {
            callee, optional, ..
        } => (callee, optional),
        _ => return None,
    };
    optional_base(ast, inner, depth + 1).or(optional.then_some(inner))
}

fn leftmost_name(ast: &Ast, mut id: NodeId) -> Option<NodeId> {
    for _ in 0..100_000 {
        id = skip_ts_only(ast, id);
        match *ast.node(id) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => id = obj,
            Node::Ident { .. } => return Some(id),
            _ => return None,
        }
    }
    None
}

fn is_type_only(ast: &Ast, d: &crate::semantic::Decl) -> bool {
    match d.kind {
        DeclKind::TypeAlias | DeclKind::Interface | DeclKind::TypeParam => true,
        DeclKind::ImportNamed | DeclKind::ImportDefault | DeclKind::ImportNamespace => {
            matches!(
                *ast.node(d.parent),
                Node::ImportDecl {
                    type_only: true,
                    ..
                }
            ) || matches!(
                *ast.node(d.node),
                Node::ImportSpecifier {
                    type_only: true,
                    ..
                }
            )
        }
        _ => false,
    }
}

fn is_undeclared(cx: &Ctx<'_, '_>, ident: NodeId, name: &str) -> bool {
    let ast = cx.ast();
    let sem = cx.semantic();
    let mut scope = Some(sem.enclosing_scope(ident));
    let mut guard = 0;
    while let Some(sc) = scope {
        guard += 1;
        if guard > 100_000 {
            return false;
        }
        let s = sem.scope(sc);
        for &sym in &s.symbols {
            if sem.name(sym) != name {
                continue;
            }
            let decls = &sem.symbol(sym).decls;
            if decls.is_empty() || decls.iter().any(|d| !is_type_only(ast, d)) {
                return false;
            }
        }
        scope = s.parent;
    }
    !sem.is_global(name)
}

impl Rule for NoOptionalChainingOnUndeclaredVariable {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member, NodeKind::Index, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let optional = match *cx.node(node) {
            Node::Member { optional, .. }
            | Node::Index { optional, .. }
            | Node::Call { optional, .. } => optional,
            _ => return,
        };
        if !optional {
            return;
        }
        // Only the outermost optional link of a chain reports (the others share its base).
        let mut cur = node;
        while let Some(p) = cx.parent(cur) {
            let inner = match *ast.node(p) {
                Node::Member { obj, .. } | Node::Index { obj, .. } => obj,
                Node::Call { callee, .. } => callee,
                Node::Paren { expr }
                | Node::As { expr, .. }
                | Node::Satisfies { expr, .. }
                | Node::NonNull { expr }
                | Node::TypeAssertion { expr, .. }
                | Node::Instantiation { expr, .. } => expr,
                _ => break,
            };
            if inner != cur {
                break;
            }
            if matches!(
                *ast.node(p),
                Node::Member { optional: true, .. }
                    | Node::Index { optional: true, .. }
                    | Node::Call { optional: true, .. }
            ) {
                return;
            }
            cur = p;
        }
        let Some(base) = optional_base(ast, node, 0) else {
            return;
        };
        let Some(ident) = leftmost_name(ast, base) else {
            return;
        };
        let Some(name) = ident_name(ast, ident) else {
            return;
        };
        if !is_undeclared(cx, ident, name) {
            return;
        }
        cx.report(
            cx.span(ident),
            format!("Optional chaining on undeclared variable `{name}` throws a ReferenceError."),
        )
        .id("no-optional-chaining-on-undeclared-variable");
    }
}
