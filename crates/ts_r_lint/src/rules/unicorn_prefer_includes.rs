//! `unicorn/prefer-includes`: existence checks written with `indexOf` / `lastIndexOf`
//! (`a.indexOf(b) !== -1`, `>= 0`, `=== -1`, `< 0`, ...) become `a.includes(b)` /
//! `!a.includes(b)`, and `a.some(x => x === b)` becomes `a.includes(b)`.
//!
//! - `indexOf` checks: the call is the left operand, with at most two arguments; a `0`
//!   `fromIndex` is dropped. Skipped: receivers named `_`, `lodash`, `underscore`; a
//!   regular-expression search value on a receiver that may be a string (an array or a
//!   known non-string receiver is fine); a `NaN` search value (`includes` finds it,
//!   `indexOf` doesn't); `lastIndexOf` with a `fromIndex` or a spread. Reported on the
//!   method name; no fix when a comment outside the receiver and arguments would be lost.
//! - `some` checks: one argument, a non-async non-generator function of one parameter that
//!   only returns `param === value` (either side), the parameter used nowhere else, no
//!   `this`, `arguments` or self-reference in a `function`, a non-`NaN` value, and a
//!   receiver not known to be a non-array. Fixed when the value has no side effects
//!   (suggested otherwise), never when the callback holds a comment.

use super::prelude::*;
use super::unicorn_static::{Static, has_side_effect, static_value};
use super::unicorn_text::{
    call_argument_text, is_parenthesized, member_object_text, needs_semicolon, paren_span,
    unary_replacement_needs_parens, would_remove_comments,
};
use super::unicorn_types::{
    is_array, is_known_non_string, is_regexp, should_skip_known_non_array_receiver,
};
use super::unicorn_util::{space_around_keyword, with_edits};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-includes",
        "Prefer `.includes()` over `.indexOf()`, `.lastIndexOf()`, and `Array#some()` when checking for existence or non-existence.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<PreferIncludes>,
};

#[derive(Debug, Default)]
struct PreferIncludes;

impl Rule for PreferIncludes {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match cx.node(node) {
            Node::Binary { .. } => check_index_of(cx, node),
            _ => check_some(cx, node),
        }
    }
}

/// `x.name(...)` (any member, computed or optional, with an identifier property): the
/// call's receiver, property and arguments. Optional chains are skipped (ESTree wraps them).
fn named_method_call<'a>(
    cx: &Ctx<'a, '_>,
    call: NodeId,
    names: &[&str],
) -> Option<(NodeId, NodeId, &'a [NodeId])> {
    let ast = cx.ast();
    let Node::Call { callee, args, .. } = *ast.node(call) else {
        return None;
    };
    if chain_has_optional(ast, call) {
        return None;
    }
    let callee = skip_parens(ast, callee);
    let (obj, prop) = match *ast.node(callee) {
        Node::Member { obj, prop, .. } => (obj, prop),
        Node::Index { obj, index, .. } => (obj, index),
        _ => return None,
    };
    let name = match *ast.node(prop) {
        Node::Ident { name } => ast.atom(name),
        _ => return None,
    };
    names
        .contains(&name)
        .then(|| (skip_parens(ast, obj), prop, ast.list(args)))
}

fn is_number_literal(ast: &ts_r_ast::Ast, id: NodeId, value: f64) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::NumberLit { value: v } if v.get() == value)
}

fn is_negative_one(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::Unary { op: UnaryOp::Minus, arg }
        if is_number_literal(ast, arg, 1.0))
}

fn is_nan_value(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    matches!(static_value(cx, id), Some(Static::Num(n)) if n.is_nan())
}

fn check_index_of(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Node::Binary { left, op, right } = *ast.node(node) else {
        return;
    };
    let call = skip_parens(ast, left);
    let Some((target, prop, args)) = named_method_call(cx, call, &["indexOf", "lastIndexOf"])
    else {
        return;
    };
    if matches!(ident_name(ast, target), Some("_" | "lodash" | "underscore")) || args.len() > 2 {
        return;
    }
    let method = ident_name(ast, prop).unwrap_or("indexOf");
    if let Some(&first) = args.first() {
        if is_regexp(cx, first) && !is_array(cx, target) && !is_known_non_string(cx, target) {
            return;
        }
        if is_nan_value(cx, first) {
            return;
        }
    }
    if method == "lastIndexOf"
        && (args.len() > 1
            || args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. })))
    {
        return;
    }
    let matched = (matches!(
        op,
        BinaryOp::StrictNotEq
            | BinaryOp::NotEq
            | BinaryOp::Gt
            | BinaryOp::StrictEq
            | BinaryOp::Eq
            | BinaryOp::LtEq
    ) && is_negative_one(ast, right))
        || (matches!(op, BinaryOp::GtEq | BinaryOp::Lt) && is_number_literal(ast, right, 0.0));
    if !matched {
        return;
    }
    let negative = matches!(
        op,
        BinaryOp::StrictEq | BinaryOp::Eq | BinaryOp::Lt | BinaryOp::LtEq
    );
    let target_text = member_object_text(cx, target, if negative { target } else { node });
    let mut kept: Vec<NodeId> = args.to_vec();
    if kept.len() == 2 && is_number_literal(ast, kept[1], 0.0) {
        kept.truncate(1);
    }
    let arg_text: Vec<String> = kept.iter().map(|&a| call_argument_text(cx, a)).collect();
    let mut replacement = format!(
        "{}{target_text}.includes({})",
        if negative { "!" } else { "" },
        arg_text.join(", ")
    );
    if negative && unary_replacement_needs_parens(cx, node) {
        replacement = format!("({replacement})");
        if needs_semicolon(cx, cx.span(node).start, &replacement) {
            replacement.insert(0, ';');
        }
    }
    let preserved: Vec<Span> = std::iter::once(target)
        .chain(kept.iter().copied())
        .map(|n| paren_span(cx, n))
        .collect();
    let fix = (!would_remove_comments(cx, cx.span(node), &preserved)).then(|| {
        with_edits(
            Fix::replace(cx.span(node), replacement),
            space_around_keyword(cx, node),
        )
    });
    let msg = format!("Use `.includes()`, rather than `.{method}()`, when checking for existence.");
    cx.report_node(prop, msg).id("prefer-includes").fix_opt(fix);
}

/// The expression a one-parameter, non-async, non-generator function returns.
fn single_return_expression(ast: &ts_r_ast::Ast, f: NodeId) -> Option<NodeId> {
    let (flags, params, body) = match *ast.node(f) {
        Node::Arrow {
            flags,
            params,
            body,
            ..
        }
        | Node::FunctionExpr {
            flags,
            params,
            body: Some(body),
            ..
        } => (flags, params, body),
        _ => return None,
    };
    if flags.intersects(FnFlags::ASYNC | FnFlags::GENERATOR) || ast.list(params).len() != 1 {
        return None;
    }
    match *ast.node(body) {
        Node::Block { stmts } => match ast.list(stmts) {
            [s] => match *ast.node(*s) {
                Node::Return { arg } => arg,
                _ => None,
            },
            _ => None,
        },
        _ => Some(body),
    }
}

/// The parameter's identifier when it is a plain name (TS annotations allowed).
fn plain_param(ast: &ts_r_ast::Ast, f: NodeId) -> Option<NodeId> {
    let (Node::Arrow { params, .. } | Node::FunctionExpr { params, .. }) = *ast.node(f) else {
        return None;
    };
    let &[p] = ast.list(params) else {
        return None;
    };
    match *ast.node(p) {
        Node::Param {
            pattern,
            rest: false,
            init: None,
            ..
        } if matches!(ast.node(pattern), Node::Ident { .. }) => Some(pattern),
        _ => None,
    }
}

/// `this`, `arguments` or the function's own name used inside a `function`.
fn function_self_used(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    let ast = cx.ast();
    let Node::FunctionExpr { name, body, .. } = *ast.node(f) else {
        return false;
    };
    let sem = cx.semantic();
    if let Some(n) = name
        && let Some(s) = sem.symbol_of_decl(n)
        && !sem.symbol(s).refs.is_empty()
    {
        return true;
    }
    let mut found = false;
    let mut stack: Vec<NodeId> = body.into_iter().collect();
    let mut guard = 0;
    while let Some(id) = stack.pop() {
        guard += 1;
        if found || guard > 1_000_000 {
            break;
        }
        match *ast.node(id) {
            // Nested functions and arrows have their own scope for `this` and `arguments`.
            Node::FunctionExpr { .. } | Node::FunctionDecl { .. } | Node::Arrow { .. } => continue,
            Node::This {} => found = true,
            Node::Ident { .. } if ident_name(ast, id) == Some("arguments") => {
                found = sem.reference_at(id).is_some();
            }
            _ => {}
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    found
}

fn check_some(cx: &mut Ctx<'_, '_>, call: NodeId) {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(call)
    else {
        return;
    };
    let Node::Member { obj, prop, .. } = *ast.node(skip_parens(ast, callee)) else {
        return;
    };
    if ident_name(ast, prop) != Some("some") {
        return;
    }
    let &[callback] = ast.list(args) else {
        return;
    };
    let callback_inner = skip_parens(ast, callback);
    let Some(ret) = single_return_expression(ast, callback_inner) else {
        return;
    };
    let Some(param) = plain_param(ast, callback_inner) else {
        return;
    };
    let name = ident_name(ast, param).unwrap_or("");
    let Node::Binary {
        left,
        op: BinaryOp::StrictEq,
        right,
    } = *ast.node(ret)
    else {
        return;
    };
    let (left, right) = (skip_parens(ast, left), skip_parens(ast, right));
    let (search, param_use) = if ident_name(ast, left) == Some(name) {
        (right, left)
    } else if ident_name(ast, right) == Some(name) {
        (left, right)
    } else {
        return;
    };
    if should_skip_known_non_array_receiver(cx, obj) || is_nan_value(cx, search) {
        return;
    }
    let sem = cx.semantic();
    let Some(sym) = sem.symbol_of_decl(param) else {
        return;
    };
    if sem
        .symbol(sym)
        .refs
        .iter()
        .any(|&r| sem.reference(r).node != param_use)
        || function_self_used(cx, callback_inner)
    {
        return;
    }
    let fix = cx
        .ast()
        .comments_in(cx.span(callback_inner))
        .is_empty()
        .then(|| {
            let mut text = cx.text(search).to_owned();
            if is_parenthesized(cx, search) && !is_parenthesized(cx, callback_inner) {
                text = format!("({text})");
            }
            Fix::replace(cx.span(prop), "includes").and(cx.span(callback_inner), text)
        });
    let side_effect = has_side_effect(ast, search);
    let report = cx
        .report_node(
            prop,
            "Use `.includes()` instead of `.some()` when checking value existence.",
        )
        .id("prefer-includes-over-some/error");
    match fix {
        Some(fix) if side_effect => {
            report.suggest(
                "prefer-includes-over-some/suggestion",
                "Replace `.some()` with `.includes()`.",
                fix,
            );
        }
        fix => {
            report.fix_opt(fix);
        }
    }
}
