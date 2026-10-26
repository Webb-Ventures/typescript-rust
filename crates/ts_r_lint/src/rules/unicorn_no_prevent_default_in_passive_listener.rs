//! `unicorn/no-prevent-default-in-passive-listener`: `event.preventDefault()` does nothing
//! in a listener registered with `{passive: true}`.
//!
//! The call is `<event>.preventDefault()` (a static key, `?.`, TS wrappers on the callee and
//! the object) directly in a non-generator function whose first parameter is the plain
//! name `<event>`, never reassigned. The function (through parentheses and TS wrappers) is
//! the second of exactly three arguments of `addEventListener(...)` or
//! `<x>.addEventListener(...)` whose third argument (through wrappers) is an object of
//! plain properties (no spread or computed key) whose last `passive` entry is
//! (through wrappers) `true`. Calls after one of the listener's own `await`/`yield`/`for
//! await` finished, or in a loop part that runs again after one, are not reported (see
//! `no-late-event-control`). The suggestion turns that `true` into `false`.

use super::prelude::*;
use super::unicorn_no_late_current_target_access::{
    enclosing_function, is_repeated_part, suspension_points,
};
use super::unicorn_util::skip_wrappers;
use crate::jsdoc_util::fn_flags;
use ts_r_ast::{Ast, FnFlags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-prevent-default-in-passive-listener",
        "Disallow ineffective `preventDefault()` calls in passive event listeners.",
        NO_PRESET,
        Category::Correctness,
    )
    .suggestions(),
    create: create_default::<NoPreventDefaultInPassiveListener>,
};

#[derive(Debug, Default)]
struct NoPreventDefaultInPassiveListener;

fn static_string(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(skip_parens(ast, id)) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::TemplateLit { parts } => match ast.list(parts) {
            &[quasi] => match *ast.node(quasi) {
                Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } => Some(ast.atom(cooked).to_owned()),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// The static name of a member access: `(object, name)`.
fn member_name(ast: &Ast, id: NodeId) -> Option<(NodeId, String)> {
    match *ast.node(id) {
        Node::Member { obj, prop, .. } => Some((obj, ident_name(ast, prop)?.to_owned())),
        Node::Index { obj, index, .. } => Some((obj, static_string(ast, index)?)),
        _ => None,
    }
}

fn key_name(ast: &Ast, key: NodeId) -> Option<String> {
    match *ast.node(key) {
        Node::Ident { .. } => ident_name(ast, key).map(str::to_owned),
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        _ => None,
    }
}

/// The `true` of `passive: true` in the listener's registration.
fn passive_literal(cx: &Ctx<'_, '_>, listener: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let mut expr = listener;
    let call = loop {
        let p = cx.parent(expr)?;
        match *ast.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. } => expr = p,
            _ => break p,
        }
    };
    let Node::Call { callee, args, .. } = *ast.node(call) else {
        return None;
    };
    let args = ast.list(args);
    if args.len() != 3
        || args[1] != expr
        || args
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
    {
        return None;
    }
    let callee = skip_wrappers(ast, callee);
    let is_add = match ast.node(callee) {
        Node::Ident { .. } => ident_name(ast, callee) == Some("addEventListener"),
        _ => member_name(ast, callee).is_some_and(|(_, n)| n == "addEventListener"),
    };
    if !is_add {
        return None;
    }
    let Node::ObjectLit { props } = *ast.node(skip_wrappers(ast, args[2])) else {
        return None;
    };
    let props = ast.list(props);
    let mut last = None;
    for &p in props {
        let (key, value) = match *ast.node(p) {
            Node::Property { key, value, .. } => (key, Some(value)),
            Node::ShorthandProp { name, .. } => (name, None),
            Node::MethodDef { key, .. } => (key, None),
            _ => return None,
        };
        if matches!(ast.node(key), Node::ComputedKey { .. }) {
            return None;
        }
        if key_name(ast, key).as_deref() == Some("passive") {
            last = Some(value);
        }
    }
    let value = skip_wrappers(ast, last??);
    matches!(ast.node(value), Node::BoolLit { value: true }).then_some(value)
}

impl Rule for NoPreventDefaultInPassiveListener {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, .. } = *ast.node(node) else {
            return;
        };
        let Some((obj, name)) = member_name(ast, skip_wrappers(ast, callee)) else {
            return;
        };
        let event = skip_wrappers(ast, obj);
        if name != "preventDefault" || !matches!(ast.node(event), Node::Ident { .. }) {
            return;
        }
        let Some(listener) = enclosing_function(cx, node) else {
            return;
        };
        let params = match *ast.node(listener) {
            Node::Arrow { params, .. }
            | Node::FunctionExpr { params, .. }
            | Node::FunctionDecl { params, .. } => params,
            _ => return,
        };
        if fn_flags(ast, listener).contains(FnFlags::GENERATOR) {
            return;
        }
        let Some(&param) = ast.list(params).first() else {
            return;
        };
        let Node::Param {
            pattern,
            rest: false,
            init: None,
            ..
        } = *ast.node(param)
        else {
            return;
        };
        if !matches!(ast.node(pattern), Node::Ident { .. }) {
            return;
        }
        let Some(passive) = passive_literal(cx, listener) else {
            return;
        };
        let sem = cx.semantic();
        let Some(sym) = sem
            .reference_at(event)
            .and_then(|r| sem.reference(r).symbol())
        else {
            return;
        };
        let symbol = sem.symbol(sym);
        if !symbol
            .decls
            .iter()
            .any(|d| d.kind == DeclKind::Param && d.name == pattern)
            || symbol
                .refs
                .iter()
                .any(|&r| sem.reference(r).node != pattern && sem.reference(r).is_write())
        {
            return;
        }
        let span = cx.span(node);
        let mut points = Vec::new();
        suspension_points(ast, listener, &mut points);
        if points.iter().any(|&p| {
            let p = cx.span(p);
            p.end <= span.end && !(p.start <= span.start && span.end <= p.end)
        }) {
            return;
        }
        let mut child = node;
        while let Some(cur) = cx.parent(child) {
            if cur == listener {
                break;
            }
            if is_repeated_part(ast, cur, child) {
                let mut inner = Vec::new();
                suspension_points(ast, cur, &mut inner);
                if !inner.is_empty() {
                    return;
                }
            }
            child = cur;
        }
        let fix = Fix::replace(cx.span(passive), "false");
        cx.report(
            span,
            "`preventDefault()` has no effect in a passive event listener.",
        )
        .id("no-prevent-default-in-passive-listener/error")
        .suggest(
            "no-prevent-default-in-passive-listener/suggestion",
            "Change `passive` to `false` to allow event cancellation.",
            fix,
        );
    }
}
