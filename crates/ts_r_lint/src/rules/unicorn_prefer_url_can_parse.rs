//! `unicorn/prefer-url-can-parse`: `URL.canParse(value)` over
//! `try { new URL(value); return true; } catch { return false; }` and the same shape that
//! assigns `true`/`false` to one variable.
//!
//! The `try` has a `catch` (no parameter, or a plain name) and no `finally`; its block is
//! `new <URL>(a[, b]);` (no spread) and then `return <bool>;` or `<name> = <bool>;`; the
//! `catch` body is the single opposite statement (the assignment to the same binding).
//! `<URL>` is the global `URL` (a type-only or `declare` binding doesn't count as local)
//! or a runtime `URL` import from `"url"`/`"node:url"`. Arguments with side effects
//! (implicit conversions included) or that contain an object, class, tagged template,
//! template with substitutions, or the global `Symbol` (called or a member read) are
//! skipped. The fix replaces the statement by `return [!]<URL>.canParse(a, b);` or
//! `<name> = [!]<URL>.canParse(a, b);`, unless a comment is inside it.

use super::prelude::*;
use super::unicorn_text::{call_argument_text, has_comment_in};
use super::unicorn_util_h4::has_side_effect;
use crate::semantic::{DeclKind, SymbolId};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-url-can-parse",
        "Prefer `URL.canParse()` over constructing a `URL` in a try/catch for validation.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferUrlCanParse>,
};

#[derive(Debug, Default)]
struct PreferUrlCanParse;

/// Erased at runtime: types, type-only imports, `declare` variables.
fn is_erased(ast: &Ast, kind: DeclKind, node: NodeId, parent: NodeId) -> bool {
    match kind {
        DeclKind::TypeAlias | DeclKind::Interface | DeclKind::TypeParam => true,
        DeclKind::ImportNamed | DeclKind::ImportDefault | DeclKind::ImportNamespace => {
            matches!(
                ast.node(parent),
                Node::ImportDecl {
                    type_only: true,
                    ..
                }
            ) || matches!(
                ast.node(node),
                Node::ImportSpecifier {
                    type_only: true,
                    ..
                }
            )
        }
        DeclKind::Var | DeclKind::Let | DeclKind::Const => {
            matches!(ast.node(parent), Node::VarDecl { modifiers, .. } if modifiers.contains(Modifiers::DECLARE))
        }
        _ => false,
    }
}

fn symbol(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<SymbolId> {
    let sem = cx.semantic();
    sem.reference_at(ident)
        .and_then(|r| sem.reference(r).symbol())
}

/// `ident` is `name` and means the global (or a binding erased at runtime).
fn is_global_or_erased(cx: &Ctx<'_, '_>, ident: NodeId, name: &str) -> bool {
    let ast = cx.ast();
    if !matches!(ast.node(ident), Node::Ident { .. }) || ident_name(ast, ident) != Some(name) {
        return false;
    }
    match symbol(cx, ident) {
        None => true,
        Some(sym) => {
            let decls = &cx.semantic().symbol(sym).decls;
            !decls.is_empty()
                && decls
                    .iter()
                    .all(|d| is_erased(ast, d.kind, d.node, d.parent))
        }
    }
}

fn is_url_constructor(cx: &Ctx<'_, '_>, callee: NodeId) -> bool {
    let ast = cx.ast();
    if !matches!(ast.node(callee), Node::Ident { .. }) {
        return false;
    }
    if is_global_or_erased(cx, callee, "URL") {
        return true;
    }
    let Some(sym) = symbol(cx, callee) else {
        return false;
    };
    cx.semantic().symbol(sym).decls.iter().any(|d| {
        let Node::ImportDecl {
            type_only: false,
            source,
            ..
        } = *ast.node(d.parent)
        else {
            return false;
        };
        let Node::ImportSpecifier {
            type_only: false,
            imported,
            local,
        } = *ast.node(d.node)
        else {
            return false;
        };
        let imported = imported.unwrap_or(local);
        matches!(*ast.node(source), Node::StringLit { value } if matches!(ast.atom(value), "url" | "node:url"))
            && matches!(ast.node(imported), Node::Ident { .. })
            && ident_name(ast, imported) == Some("URL")
    })
}

fn skip_chain_ts(ast: &Ast, mut id: NodeId) -> NodeId {
    loop {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. } => id = expr,
            _ => return id,
        }
    }
}

/// A node whose conversion to a URL string can obviously throw.
fn is_unsafe_node(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::ObjectLit { .. } | Node::ClassExpr { .. } | Node::TaggedTemplate { .. } => true,
        Node::TemplateLit { parts } => ast.list(parts).len() > 1,
        Node::Call { callee, .. } => is_global_or_erased(cx, skip_chain_ts(ast, callee), "Symbol"),
        Node::Member { obj, .. } | Node::Index { obj, .. } => {
            is_global_or_erased(cx, skip_chain_ts(ast, obj), "Symbol")
        }
        _ => false,
    }
}

fn contains_unsafe(cx: &Ctx<'_, '_>, root: NodeId) -> bool {
    let ast = cx.ast();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if is_unsafe_node(cx, id) {
            return true;
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    false
}

fn bool_lit(ast: &Ast, id: NodeId) -> Option<bool> {
    match *ast.node(skip_parens(ast, id)) {
        Node::BoolLit { value } => Some(value),
        _ => None,
    }
}

fn returned_bool(ast: &Ast, stmt: NodeId) -> Option<bool> {
    match *ast.node(stmt) {
        Node::Return { arg: Some(arg) } => bool_lit(ast, arg),
        _ => None,
    }
}

/// `<name> = <bool>;`: `(name ident, value)`.
fn assigned_bool(ast: &Ast, stmt: NodeId) -> Option<(NodeId, bool)> {
    let Node::ExprStmt { expr } = *ast.node(stmt) else {
        return None;
    };
    let Node::Assign {
        left,
        op: AssignOp::Assign,
        right,
    } = *ast.node(expr)
    else {
        return None;
    };
    if !matches!(ast.node(left), Node::Ident { .. }) {
        return None;
    }
    Some((left, bool_lit(ast, right)?))
}

impl Rule for PreferUrlCanParse {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Try]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Try {
            block,
            handler: Some(handler),
            finalizer: None,
        } = *ast.node(node)
        else {
            return;
        };
        let Node::CatchClause { param, body, .. } = *ast.node(handler) else {
            return;
        };
        if param.is_some_and(|p| !matches!(ast.node(p), Node::Ident { .. })) {
            return;
        }
        let (Node::Block { stmts }, Node::Block { stmts: catch_stmts }) =
            (*ast.node(block), *ast.node(body))
        else {
            return;
        };
        let (&[first, second], &[catch_stmt]) = (ast.list(stmts), ast.list(catch_stmts)) else {
            return;
        };
        let Node::ExprStmt { expr } = *ast.node(first) else {
            return;
        };
        let Node::New { callee, args, .. } = *ast.node(expr) else {
            return;
        };
        let args = ast.list(args);
        if !(1..=2).contains(&args.len())
            || args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
            || !is_url_constructor(cx, callee)
            || args
                .iter()
                .any(|&a| contains_unsafe(cx, a) || has_side_effect(ast, a, false, true))
        {
            return;
        }
        let (target, value) = if let (Some(t), Some(c)) =
            (returned_bool(ast, second), returned_bool(ast, catch_stmt))
        {
            if t == c {
                return;
            }
            (None, t)
        } else {
            let (Some((left, t)), Some((catch_left, c))) =
                (assigned_bool(ast, second), assigned_bool(ast, catch_stmt))
            else {
                return;
            };
            if t == c
                || ident_name(ast, left) != ident_name(ast, catch_left)
                || symbol(cx, left) != symbol(cx, catch_left)
            {
                return;
            }
            (Some(left), t)
        };
        let arguments: Vec<String> = args.iter().map(|&a| call_argument_text(cx, a)).collect();
        let call = format!(
            "{}{}.canParse({})",
            if value { "" } else { "!" },
            cx.text(callee),
            arguments.join(", ")
        );
        let replacement = match target {
            None => format!("return {call};"),
            Some(left) => format!("{} = {call};", cx.text(left)),
        };
        let span = cx.span(node);
        let fix = (!has_comment_in(cx, span)).then(|| Fix::replace(span, replacement));
        cx.report(
            span,
            "Prefer `URL.canParse()` over constructing a `URL` in a try/catch for validation.",
        )
        .id("prefer-url-can-parse")
        .fix_opt(fix);
    }
}
