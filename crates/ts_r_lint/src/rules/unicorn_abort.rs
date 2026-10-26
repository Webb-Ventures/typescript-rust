//! The `AbortController` shapes behind `unicorn/prefer-abort-signal-timeout` (and the
//! `AbortSignal.any()` variant): a `const controller = new AbortController()` that only the
//! statements after it abort, and whose other uses all read `controller.signal`, can become
//! a plain `AbortSignal`.

use super::prelude::*;
use super::unicorn_ast::{is_left_hand_side, remove_statements};
use super::unicorn_static::{Static, static_value};
use super::unicorn_text::would_remove_comments;
use crate::semantic::{DeclKind, SymbolId};
use ts_r_ast::Ast;

/// The statement after `stmt` in its statement list (`None` when it is last, or when
/// `stmt` is not in a list).
pub(crate) fn next_statement(cx: &Ctx<'_, '_>, stmt: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let list = match *ast.node(cx.parent(stmt)?) {
        Node::Program { stmts } | Node::Block { stmts } | Node::ModuleBlock { stmts } => stmts,
        Node::SwitchCase { body, .. } | Node::StaticBlock { body } => body,
        _ => return None,
    };
    let list = ast.list(list);
    let i = list.iter().position(|&s| s == stmt)?;
    list.get(i + 1).copied()
}

/// A declaration that only names a type (`type`, `interface`, `import type`, `{type X}`).
pub(crate) fn is_type_only_decl(ast: &Ast, kind: DeclKind, node: NodeId, parent: NodeId) -> bool {
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
        _ => false,
    }
}

/// No runtime binding named `name` is visible at `node`, so inserted code reaches the
/// global.
pub(crate) fn is_global_name_available(cx: &Ctx<'_, '_>, name: &str, node: NodeId) -> bool {
    let sem = cx.semantic();
    let Some(sym) = sem.lookup(sem.enclosing_scope(node), name) else {
        return true;
    };
    sem.symbol(sym)
        .decls
        .iter()
        .all(|d| is_type_only_decl(cx.ast(), d.kind, d.node, d.parent))
}

/// The declared controller of `const <name> = new AbortController()`, alone in a non-exported
/// declaration, with the global `AbortController` and `AbortSignal`: `(VarDecl, name, init)`.
pub(crate) fn abort_controller_declarator(
    cx: &Ctx<'_, '_>,
    declarator: NodeId,
) -> Option<(NodeId, NodeId, NodeId)> {
    let ast = cx.ast();
    let Node::VarDeclarator {
        name,
        init: Some(init),
        ..
    } = *ast.node(declarator)
    else {
        return None;
    };
    let decl = cx.parent(declarator)?;
    let Node::VarDecl {
        modifiers,
        kind: VarKind::Const,
        decls,
    } = *ast.node(decl)
    else {
        return None;
    };
    if modifiers.contains(Modifiers::EXPORT)
        || ast.list(decls).len() != 1
        || !matches!(ast.node(name), Node::Ident { .. })
    {
        return None;
    }
    let Node::New { callee, args, .. } = *ast.node(init) else {
        return None;
    };
    if !ast.list(args).is_empty()
        || !matches!(ast.node(callee), Node::Ident { .. })
        || !cx.is_global_ref(callee, "AbortController")
        || !is_global_name_available(cx, "AbortSignal", name)
    {
        return None;
    }
    Some((decl, name, init))
}

/// The `controller` of a callback that does nothing but `controller.abort()`: a
/// non-async, non-generator function without parameters whose concise body, or only
/// statement, is the call.
pub(crate) fn abort_reference(cx: &Ctx<'_, '_>, callback: NodeId, name: &str) -> Option<NodeId> {
    let ast = cx.ast();
    let (flags, params, body) = match *ast.node(callback) {
        Node::Arrow {
            flags,
            params,
            body,
            ..
        } => (flags, params, body),
        Node::FunctionExpr {
            flags,
            params,
            body: Some(body),
            ..
        } => (flags, params, body),
        _ => return None,
    };
    if flags.intersects(FnFlags::ASYNC | FnFlags::GENERATOR) || !ast.list(params).is_empty() {
        return None;
    }
    let expr = match *ast.node(body) {
        Node::Block { stmts } => match ast.list(stmts) {
            &[only] => match *ast.node(only) {
                Node::ExprStmt { expr } => expr,
                _ => return None,
            },
            _ => return None,
        },
        _ => body,
    };
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(skip_parens(ast, expr))
    else {
        return None;
    };
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    (ident_name(ast, prop) == Some("abort")
        && matches!(ast.node(obj), Node::Ident { .. })
        && ident_name(ast, obj) == Some(name)
        && ast.list(args).is_empty())
    .then_some(obj)
}

fn parent_np(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let mut cur = id;
    loop {
        let p = cx.parent(cur)?;
        if matches!(cx.node(p), Node::Paren { .. }) {
            cur = p;
        } else {
            return Some(p);
        }
    }
}

/// `node` with the parentheses and TS wrappers around it, and the parent beyond them.
fn outermost_ts(cx: &Ctx<'_, '_>, node: NodeId) -> (NodeId, Option<NodeId>) {
    let mut cur = node;
    loop {
        let Some(p) = cx.parent(cur) else {
            return (cur, None);
        };
        match *cx.node(p) {
            Node::Paren { .. } => cur = p,
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
                if expr == cur =>
            {
                cur = p
            }
            _ => return (cur, Some(p)),
        }
    }
}

/// `controller.signal` that can be replaced by the new signal: read, not aliased, not
/// written, and not asked for its `reason` or `throwIfAborted`.
fn signal_member(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let member = parent_np(cx, ident)?;
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(member)
    else {
        return None;
    };
    if skip_parens(ast, obj) != ident
        || ident_name(ast, prop) != Some("signal")
        || is_left_hand_side(cx, member)
    {
        return None;
    }
    let (outer, parent) = outermost_ts(cx, member);
    let parent = parent?;
    let reason_read = match *ast.node(parent) {
        Node::Member { obj, prop, .. } => {
            obj == outer && matches!(ident_name(ast, prop), Some("reason" | "throwIfAborted"))
        }
        Node::Index { obj, index, .. } => {
            obj == outer
                && matches!(
                    static_value(cx, index),
                    Some(Static::Str(s)) if s == "reason" || s == "throwIfAborted"
                )
        }
        _ => false,
    };
    if reason_read
        || matches!(
            ast.node(parent),
            Node::VarDeclarator { .. } | Node::Assign { .. }
        )
    {
        return None;
    }
    Some(member)
}

/// The `controller.signal` reads of the controller declared by `name`, when every other
/// reference is one of `abort_refs` (which must mean the same binding). `None` when there
/// are no reads or the controller is used any other way.
pub(crate) fn signal_members(
    cx: &Ctx<'_, '_>,
    name: NodeId,
    abort_refs: &[NodeId],
) -> Option<Vec<NodeId>> {
    let sem = cx.semantic();
    let sym = sem.symbol_of_decl(name)?;
    if abort_refs
        .iter()
        .any(|&r| sem.symbol_of_ident(r) != Some(sym))
    {
        return None;
    }
    let mut members = Vec::new();
    for &r in &sem.symbol(sym).refs {
        let reference = sem.reference(r);
        let ident = reference.node;
        if ident == name || abort_refs.contains(&ident) {
            continue;
        }
        if reference.is_write() {
            return None;
        }
        members.push(signal_member(cx, ident)?);
    }
    (!members.is_empty()).then_some(members)
}

fn has_conflict(cx: &Ctx<'_, '_>, name: &str, sym: SymbolId, node: NodeId) -> bool {
    let sem = cx.semantic();
    sem.lookup(sem.enclosing_scope(node), name)
        .is_some_and(|s| s != sym)
}

/// Reports the controller with a suggestion that replaces its initializer by
/// `replacement`, its type annotation by `AbortSignal`, renames `controller` to `signal`
/// and `abortController` to `abortSignal` (unless the new name is taken where it would be
/// used), replaces the `.signal` reads and removes `first..=last`. No suggestion when a
/// comment would be lost.
#[allow(clippy::too_many_arguments)]
pub(crate) fn report_abort_controller(
    cx: &mut Ctx<'_, '_>,
    declarator: NodeId,
    first: NodeId,
    last: NodeId,
    replacement: String,
    members: &[NodeId],
    message_id: &'static str,
    message: &str,
    suggestion_id: &'static str,
    suggestion: &str,
) {
    let ast = cx.ast();
    let Node::VarDeclarator {
        name,
        type_ann,
        init: Some(init),
        ..
    } = *ast.node(declarator)
    else {
        return;
    };
    let Some(decl) = cx.parent(declarator) else {
        return;
    };
    let sem = cx.semantic();
    let Some(sym) = sem.symbol_of_decl(name) else {
        return;
    };
    let old = ident_name(ast, name).unwrap_or("");
    let renamed = match old {
        "abortController" => "abortSignal",
        "controller" => "signal",
        _ => old,
    };
    let new_name = if renamed != old
        && (has_conflict(cx, renamed, sym, name)
            || members.iter().any(|&m| has_conflict(cx, renamed, sym, m)))
    {
        old
    } else {
        renamed
    };
    let name_span = cx.span(name);
    let id_span = match type_ann {
        Some(t) => Span::new(name_span.start, cx.span(t).end),
        None => name_span,
    };
    let src = cx.src();
    let last_end = cx.span(last).end;
    let mut withheld = would_remove_comments(cx, Span::new(cx.span(decl).end, last_end), &[]);
    // A comment after the last removed statement on its line.
    let line_end = src[last_end as usize..]
        .find(['\n', '\r', '\u{2028}', '\u{2029}'])
        .map_or(src.len(), |i| last_end as usize + i);
    withheld |= !ast
        .comments_in(Span::new(last_end, line_end as u32))
        .is_empty();
    withheld |= would_remove_comments(cx, cx.span(init), &[]);
    if type_ann.is_some() {
        withheld |= would_remove_comments(cx, Span::new(name_span.end, id_span.end), &[]);
    }
    withheld |= members
        .iter()
        .any(|&m| would_remove_comments(cx, cx.span(m), &[]));
    let fix = (!withheld).then(|| {
        let mut fix = Fix::replace(cx.span(init), replacement);
        if type_ann.is_some() {
            fix = fix.and(Span::new(name_span.end, id_span.end), ": AbortSignal");
        }
        if new_name != old {
            fix = fix.and(name_span, new_name);
        }
        for &m in members {
            fix = fix.and(cx.span(m), new_name);
        }
        for edit in remove_statements(cx, first, last, false).edits {
            fix = fix.and(edit.span, edit.text);
        }
        fix
    });
    let report = cx.report(id_span, message).id(message_id);
    if let Some(fix) = fix {
        report.suggest(suggestion_id, suggestion, fix);
    }
}
