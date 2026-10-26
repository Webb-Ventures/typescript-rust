//! `unicorn/no-incorrect-query-selector`: `querySelector()`/`querySelectorAll()` misuse.
//!
//! A query is a plain `<x>.querySelector(sel)`/`querySelectorAll(sel)` (no `?.`, one
//! non-spread argument) whose receiver could be a node.
//!
//! - `first-match`: `qsa(...)[0]`, `qsa(...).at(0)`, `qsa(...).item(0)` (plain, literal `0`)
//!   that isn't written to; fixed to `querySelector(...)` without the access, unless a
//!   comment sits between the call and the end of the access.
//! - `id-selector`: `querySelectorAll("#id")` (a static simple ID selector) not followed by
//!   a first-element access, on the method name.
//! - `length-check`: a `querySelectorAll()` call, or a `const` bound to one, used (through
//!   `!` and global `Boolean()`) as a control-flow test (also inside `&&`/`||`); fixed to
//!   `<x>.length > 0` / `=== 0` unless a comment inside the test or right after it would move
//!   or be lost.
//! - `query-selector-all-nullish`: such a call or `const` compared with `null` or the global
//!   `undefined` (`==`, `===`, `!=`, `!==`, either side).
//! - `query-selector-undefined`: the same for `querySelector()`, `===`/`!==` with `undefined`.

use super::prelude::*;
use super::unicorn_ast::{is_left_hand_side, method_call};
use super::unicorn_explicit_length_check::{boolean_ancestor, is_control_flow_test};
use super::unicorn_no_array_reduce::const_initializer;
use super::unicorn_text::{paren_span, would_remove_comments};
use super::unicorn_util::is_value_not_dom_node;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-incorrect-query-selector",
        "Disallow incorrect `querySelector()` and `querySelectorAll()` usage.",
        NO_PRESET,
        Category::Suspicious,
    )
    .fixable(),
    create: create_default::<NoIncorrectQuerySelector>,
};

#[derive(Debug, Default)]
struct NoIncorrectQuerySelector;

fn is_query(ast: &Ast, id: NodeId, name: &str) -> bool {
    method_call(ast, skip_parens(ast, id)).is_some_and(|m| {
        m.plain()
            && m.name == name
            && m.args.len() == 1
            && !matches!(ast.node(m.args[0]), Node::Spread { .. })
            && !is_value_not_dom_node(ast, m.obj)
    })
}

fn is_qsa(ast: &Ast, id: NodeId) -> bool {
    is_query(ast, id, "querySelectorAll")
}

fn is_zero(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::NumberLit { value } if value.get() == 0.0)
}

/// `qsa(...)[0]`: the call.
fn zero_index(ast: &Ast, id: NodeId) -> Option<NodeId> {
    match *ast.node(id) {
        Node::Index {
            obj,
            optional: false,
            index,
        } if is_qsa(ast, obj) && is_zero(ast, index) => Some(skip_parens(ast, obj)),
        _ => None,
    }
}

/// `qsa(...).at(0)` / `.item(0)`: the call.
fn first_item_call(ast: &Ast, id: NodeId) -> Option<NodeId> {
    let m = method_call(ast, id)?;
    (m.plain()
        && matches!(m.name, "at" | "item")
        && m.args.len() == 1
        && is_qsa(ast, m.obj)
        && is_zero(ast, m.args[0]))
    .then(|| skip_parens(ast, m.obj))
}

fn parent_np(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let mut cur = id;
    for _ in 0..10_000 {
        let p = cx.parent(cur)?;
        if !matches!(cx.node(p), Node::Paren { .. }) {
            return Some(p);
        }
        cur = p;
    }
    None
}

fn part_of_first_access(cx: &Ctx<'_, '_>, call: NodeId) -> bool {
    let ast = cx.ast();
    let Some(p) = parent_np(cx, call) else {
        return false;
    };
    if zero_index(ast, p) == Some(call) || first_item_call(ast, p) == Some(call) {
        return true;
    }
    matches!(*ast.node(p), Node::Member { obj, .. } if skip_parens(ast, obj) == call)
        && parent_np(cx, p).is_some_and(|g| first_item_call(ast, g) == Some(call))
}

fn first_match(cx: &mut Ctx<'_, '_>, node: NodeId, call: NodeId) {
    if is_left_hand_side(cx, node) {
        return;
    }
    let ast = cx.ast();
    let span = cx.span(node);
    let range = Span::new(paren_span(cx, call).end, span.end);
    let fix = (!would_remove_comments(cx, range, &[])).then(|| {
        let Some(m) = method_call(ast, call) else {
            return Fix::delete(range);
        };
        Fix::replace(cx.span(m.prop), "querySelector").and(range, "")
    });
    cx.report(
        span,
        "Prefer `.querySelector()` when only the first match is used.",
    )
    .id("first-match")
    .fix_opt(fix);
}

fn is_simple_id_selector(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('#') else {
        return false;
    };
    let mut chars = rest.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '-')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

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

/// The query `id` is or a `const` is bound to.
fn query_of(cx: &Ctx<'_, '_>, id: NodeId, name: &str) -> bool {
    let ast = cx.ast();
    is_query(ast, id, name)
        || const_initializer(cx, skip_parens(ast, id)).is_some_and(|init| is_query(ast, init, name))
}

fn length_check(cx: &mut Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some(p) = parent_np(cx, node) else {
        return false;
    };
    if matches!(
        ast.node(p),
        Node::VarDeclarator { .. } | Node::Member { .. } | Node::Index { .. }
    ) {
        return false;
    }
    let (ancestor, negative) = boolean_ancestor(cx, node);
    if !is_control_flow_test(cx, ancestor, 0) || !query_of(cx, node, "querySelectorAll") {
        return false;
    }
    let span = cx.span(node);
    let ancestor_span = cx.span(ancestor);
    let src = cx.src();
    let next = skip_trivia_forward(src, ancestor_span.end as usize) as u32;
    let comments_after = !ast
        .comments_in(Span::new(ancestor_span.end, next))
        .is_empty();
    let fix = (!would_remove_comments(cx, ancestor_span, &[span]) && !comments_after).then(|| {
        Fix::replace(
            ancestor_span,
            format!(
                "{}.length {}",
                span.text(src),
                if negative { "=== 0" } else { "> 0" }
            ),
        )
    });
    cx.report(
        span,
        "Check `.length` instead of the `NodeList` object itself.",
    )
    .id("length-check")
    .fix_opt(fix);
    true
}

fn is_undefined(ast: &Ast, id: NodeId) -> bool {
    let id = skip_parens(ast, id);
    matches!(ast.node(id), Node::Ident { .. }) && ident_name(ast, id) == Some("undefined")
}

fn is_nullish(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    matches!(ast.node(id), Node::NullLit {})
        || (is_undefined(ast, id) && cx.is_global_ref(id, "undefined"))
}

/// The nullish operand of a comparison of a `name` query with `null`/`undefined`.
fn comparison(cx: &Ctx<'_, '_>, left: NodeId, right: NodeId, name: &str) -> Option<NodeId> {
    if query_of(cx, left, name) && is_nullish(cx, right) {
        return Some(right);
    }
    if query_of(cx, right, name) && is_nullish(cx, left) {
        return Some(left);
    }
    None
}

impl Rule for NoIncorrectQuerySelector {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Index,
            NodeKind::Call,
            NodeKind::Binary,
            NodeKind::Ident,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *ast.node(node) {
            Node::Index { .. } => {
                if let Some(call) = zero_index(ast, node) {
                    first_match(cx, node, call);
                }
            }
            Node::Call { .. } => {
                if let Some(call) = first_item_call(ast, node) {
                    first_match(cx, node, call);
                    return;
                }
                if length_check(cx, node) {
                    return;
                }
                if !is_qsa(ast, node) || part_of_first_access(cx, node) {
                    return;
                }
                let Some(m) = method_call(ast, node) else {
                    return;
                };
                if static_string(ast, m.args[0]).is_some_and(|s| is_simple_id_selector(&s)) {
                    cx.report(
                        cx.span(m.prop),
                        "Prefer `.querySelector()` for a simple ID selector.",
                    )
                    .id("id-selector");
                }
            }
            Node::Binary { left, op, right } => {
                if !matches!(
                    op,
                    BinaryOp::Eq | BinaryOp::StrictEq | BinaryOp::NotEq | BinaryOp::StrictNotEq
                ) {
                    return;
                }
                let span = cx.span(node);
                if comparison(cx, left, right, "querySelectorAll").is_some() {
                    cx.report(
                        span,
                        "Check `.length` instead of comparing the `NodeList` object with `null` or `undefined`.",
                    )
                    .id("query-selector-all-nullish");
                    return;
                }
                if matches!(op, BinaryOp::StrictEq | BinaryOp::StrictNotEq)
                    && comparison(cx, left, right, "querySelector")
                        .is_some_and(|v| is_undefined(ast, v))
                {
                    cx.report(
                        span,
                        "Compare the result of `.querySelector()` with `null` instead of `undefined`.",
                    )
                    .id("query-selector-undefined");
                }
            }
            Node::Ident { .. } => {
                if cx.semantic().reference_at(node).is_some() {
                    length_check(cx, node);
                }
            }
            _ => {}
        }
    }
}
