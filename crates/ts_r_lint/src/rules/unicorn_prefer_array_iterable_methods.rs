//! `unicorn/prefer-array-iterable-methods`: `for (const|let [index, value] of x.entries())`
//! (not `for await`; a one- or two-slot array pattern; `entries` plain with no arguments)
//! over a known array `x` where exactly one half of the pair is unused (a hole, a missing
//! slot, or an identifier with no reference besides its own binding).
//!
//! - Index unused: iterate `x` directly (`unused-index`). Not when the value is a rest or
//!   has a default. The fix puts the value binding in place of the pattern and drops
//!   `.entries()`; none when a comment outside the value binding is in the pattern or a
//!   comment outside `x` is in the call.
//! - Value unused: use `x.keys()` (`unused-value`); the index must be an identifier. The
//!   fix keeps the index binding and renames the method; none with a comment in the pattern.
//!
//! The new binding gets a space where the keyword before or after touched the pattern.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_text::{paren_span, would_remove_comments};
use super::unicorn_types::is_array;
use super::unicorn_util::remove_method_call_into;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-array-iterable-methods",
        "Prefer iterating an array directly or with `Array#keys()` over `Array#entries()` when the index or value is unused.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferArrayIterableMethods>,
};

#[derive(Debug, Default)]
struct PreferArrayIterableMethods;

/// The one- or two-slot array pattern of a `const`/`let` loop head.
fn array_pattern(ast: &Ast, left: NodeId) -> Option<(NodeId, Option<NodeId>, Option<NodeId>)> {
    let Node::VarDecl { kind, decls, .. } = *ast.node(left) else {
        return None;
    };
    if !matches!(kind, VarKind::Const | VarKind::Let) {
        return None;
    }
    let [d] = *ast.list(decls) else {
        return None;
    };
    let Node::VarDeclarator { name, .. } = *ast.node(d) else {
        return None;
    };
    let Node::ArrayPattern { elements } = *ast.node(name) else {
        return None;
    };
    let slot = |e: NodeId| (!matches!(ast.node(e), Node::Hole {})).then_some(e);
    match *ast.list(elements) {
        [a] => Some((name, slot(a), None)),
        [a, b] => Some((name, slot(a), slot(b))),
        _ => None,
    }
}

fn is_unused(cx: &Ctx<'_, '_>, element: Option<NodeId>) -> bool {
    let Some(element) = element else {
        return true;
    };
    if !matches!(cx.node(element), Node::Ident { .. }) {
        return false;
    }
    let sem = cx.semantic();
    let Some(sym) = sem.symbol_of_decl(element) else {
        return false;
    };
    sem.symbol(sym)
        .refs
        .iter()
        .all(|&r| sem.reference(r).node == element)
}

/// The pattern's replacement, spaced off a keyword that touches it.
fn pattern_text(cx: &Ctx<'_, '_>, pattern: NodeId, text: &str) -> String {
    let ast = cx.ast();
    let src = cx.src();
    let span = cx.span(pattern);
    let before = skip_trivia_backward(ast, src, span.start as usize) == span.start as usize;
    let after = skip_trivia_forward(src, span.end as usize) == span.end as usize;
    format!(
        "{}{text}{}",
        if before { " " } else { "" },
        if after { " " } else { "" }
    )
}

impl Rule for PreferArrayIterableMethods {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ForOf]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::ForOf {
            is_await: false,
            left,
            right,
            ..
        } = *ast.node(id)
        else {
            return;
        };
        let Some((pattern, index, value)) = array_pattern(ast, left) else {
            return;
        };
        let call_id = skip_parens(ast, right);
        let Some(call) = method_call(ast, call_id) else {
            return;
        };
        if call.name != "entries"
            || !call.args.is_empty()
            || !call.plain()
            || !is_array(cx, call.obj)
        {
            return;
        }
        let index_unused = is_unused(cx, index);
        let value_unused = is_unused(cx, value);
        if index_unused == value_unused {
            return;
        }
        let pattern_span = cx.span(pattern);
        if index_unused {
            let Some(value) = value else {
                return;
            };
            if matches!(
                ast.node(value),
                Node::Rest { .. } | Node::AssignPattern { .. }
            ) {
                return;
            }
            let fix = (!would_remove_comments(cx, pattern_span, &[cx.span(value)])
                && !would_remove_comments(cx, cx.span(call_id), &[paren_span(cx, call.obj)]))
            .then(|| {
                let replaced =
                    Fix::replace(pattern_span, pattern_text(cx, pattern, cx.text(value)));
                remove_method_call_into(cx, call_id, replaced)
            });
            let r = cx
                .report(
                    cx.span(call.prop),
                    "Prefer iterating the array directly over `Array#entries()` when the index is unused.",
                )
                .id("unused-index");
            if let Some(f) = fix {
                r.fix(f);
            }
            return;
        }
        let Some(index) = index.filter(|&i| matches!(ast.node(i), Node::Ident { .. })) else {
            return;
        };
        let fix = (!would_remove_comments(cx, pattern_span, &[])).then(|| {
            Fix::replace(pattern_span, pattern_text(cx, pattern, cx.text(index)))
                .and(cx.span(call.prop), "keys")
        });
        let r = cx
            .report(
                cx.span(call.prop),
                "Prefer `Array#keys()` over `Array#entries()` when the value is unused.",
            )
            .id("unused-value");
        if let Some(f) = fix {
            r.fix(f);
        }
    }
}
