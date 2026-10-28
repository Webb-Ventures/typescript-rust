//! `unicorn/no-invalid-well-known-symbol-methods`: a class or object member keyed
//! `[Symbol.dispose]`, `[Symbol.iterator]` or `[Symbol.asyncIterator]` (written as a plain
//! `Symbol.name` member) whose value is a function (a method, or a function / arrow as the
//! property or field value; getters and setters are skipped) has the wrong shape:
//! - `dispose` as a generator (no fix);
//! - `dispose` async, or annotated to return `Promise` / `PromiseLike` (or a union holding
//!   one): fixed to `asyncDispose`, unless a sibling with the same `static`-ness is keyed
//!   `Symbol.asyncDispose` (any static spelling) or a spread follows the member;
//! - `iterator` async or promise-returning (a suggestion to use `asyncIterator` for
//!   generators);
//! - `asyncIterator` that is async or promise-returning but not a generator.
//!
//! The key's member expression is reported. Without type information (the oracle has none)
//! only the return type annotation counts.

use super::prelude::*;
use super::unicorn_util_h1::static_property_name;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-invalid-well-known-symbol-methods",
        "Disallow invalid implementations of well-known symbol methods.",
        NO_PRESET,
        Category::Suspicious,
    )
    .fixable()
    .suggestions(),
    create: create_default::<NoInvalidWellKnownSymbolMethods>,
};

#[derive(Debug, Default)]
struct NoInvalidWellKnownSymbolMethods;

/// The member's computed key expression, its value and whether it is `static`.
fn parts(ast: &Ast, member: NodeId) -> Option<(Option<NodeId>, Option<NodeId>, bool)> {
    let computed = |key: NodeId| match *ast.node(key) {
        Node::ComputedKey { expr } => Some(expr),
        _ => None,
    };
    match *ast.node(member) {
        Node::MethodDef {
            modifiers,
            kind,
            key,
            value,
            ..
        } => Some((
            computed(key),
            (!matches!(kind, MethodKind::Get | MethodKind::Set)).then_some(value),
            modifiers.contains(Modifiers::STATIC),
        )),
        Node::PropertyDef {
            modifiers,
            key,
            value,
            ..
        } => Some((computed(key), value, modifiers.contains(Modifiers::STATIC))),
        Node::Property { key, value, .. } => Some((computed(key), Some(value), false)),
        _ => None,
    }
}

/// `Symbol.name` with an identifier property: `(property, name)`.
fn well_known(ast: &Ast, key: NodeId) -> Option<(NodeId, &str)> {
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(key)
    else {
        return None;
    };
    if ident_name(ast, obj) != Some("Symbol") {
        return None;
    }
    Some((prop, ident_name(ast, prop)?))
}

fn is_promise_ref(ast: &Ast, ty: NodeId) -> bool {
    let mut ty = ty;
    while let Node::ParenType { ty: inner } = *ast.node(ty) {
        ty = inner;
    }
    match *ast.node(ty) {
        Node::TypeRef { name, .. } => {
            matches!(ident_name(ast, name), Some("Promise" | "PromiseLike"))
        }
        _ => false,
    }
}

fn returns_promise(ast: &Ast, ret: Option<NodeId>) -> bool {
    let Some(mut ty) = ret else {
        return false;
    };
    while let Node::ParenType { ty: inner } = *ast.node(ty) {
        ty = inner;
    }
    match *ast.node(ty) {
        Node::UnionType { types } => ast.list(types).iter().any(|&t| is_promise_ref(ast, t)),
        _ => is_promise_ref(ast, ty),
    }
}

impl Rule for NoInvalidWellKnownSymbolMethods {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::MethodDef,
            NodeKind::PropertyDef,
            NodeKind::Property,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some((Some(key), Some(value), is_static)) = parts(ast, node) else {
            return;
        };
        let Some((prop, name)) = well_known(ast, key) else {
            return;
        };
        if !matches!(name, "dispose" | "iterator" | "asyncIterator") {
            return;
        }
        let (flags, ret) = match *ast.node(skip_parens(ast, value)) {
            Node::FunctionExpr {
                flags,
                return_type,
                body: Some(_),
                ..
            } => (flags, return_type),
            Node::Arrow {
                flags, return_type, ..
            } => (flags, return_type),
            _ => return,
        };
        let is_async = flags.contains(FnFlags::ASYNC);
        let is_generator = flags.contains(FnFlags::GENERATOR);
        if name == "dispose" && is_generator {
            cx.report_node(
                key,
                "`Symbol.dispose` must not be a generator. Use a normal method for synchronous disposal or `Symbol.asyncDispose` for asynchronous disposal.",
            )
            .id("no-invalid-well-known-symbol-methods/dispose-generator");
            return;
        }
        let promising = is_async || returns_promise(ast, ret);
        if !promising {
            return;
        }
        match name {
            "dispose" => {
                let fix = (!may_shadow(cx, node, is_static))
                    .then(|| Fix::replace(cx.span(prop), "asyncDispose"));
                cx.report_node(
                    key,
                    "`Symbol.dispose` must not return a Promise. Use `Symbol.asyncDispose` instead.",
                )
                .id("no-invalid-well-known-symbol-methods/async-dispose")
                .fix_opt(fix);
            }
            "iterator" => {
                let prop_span = cx.span(prop);
                let report = cx
                    .report_node(
                        key,
                        "`Symbol.iterator` must return a sync iterator. Use `Symbol.asyncIterator` for async iterators.",
                    )
                    .id("no-invalid-well-known-symbol-methods/sync-iterator");
                if is_generator {
                    report.suggest(
                        "no-invalid-well-known-symbol-methods/suggestion",
                        "Use `Symbol.asyncIterator` instead.",
                        Fix::replace(prop_span, "asyncIterator"),
                    );
                }
            }
            _ if !is_generator => {
                cx.report_node(
                    key,
                    "`Symbol.asyncIterator` must return an async iterator directly. Use an async generator or return the iterator synchronously.",
                )
                .id("no-invalid-well-known-symbol-methods/async-iterator");
            }
            _ => {}
        }
    }
}

/// Another member keyed `Symbol.asyncDispose` with the same `static`-ness, or a spread after
/// `member`, would win over the renamed method.
fn may_shadow(cx: &Ctx<'_, '_>, member: NodeId, is_static: bool) -> bool {
    let ast = cx.ast();
    let Some(parent) = cx.parent(member) else {
        return false;
    };
    let siblings = match *ast.node(parent) {
        Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => ast.list(members),
        Node::ObjectLit { props } => ast.list(props),
        _ => return false,
    };
    let at = siblings.iter().position(|&s| s == member).unwrap_or(0);
    siblings.iter().enumerate().any(|(i, &s)| {
        if s == member {
            return false;
        }
        if matches!(ast.node(s), Node::Spread { .. }) {
            return i > at;
        }
        let Some((Some(key), _, other_static)) = parts(ast, s) else {
            return false;
        };
        other_static == is_static
            && matches!(*ast.node(key), Node::Member { obj, .. } | Node::Index { obj, .. }
                if ident_name(ast, obj) == Some("Symbol"))
            && static_property_name(cx, key).as_deref() == Some("asyncDispose")
    })
}
