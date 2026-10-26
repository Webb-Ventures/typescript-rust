//! `unicorn/prefer-iterator-helpers`: an array materialized from an iterator only to call a
//! method `Iterator` has too.
//!
//! - `[...it].m(cb)` / `Array.from(it).m(cb)` (`it` an iterator expression; `Array.from`
//!   with one argument and no type arguments) for `every/find/forEach/some(cb)` or
//!   `reduce(cb[, init])`, plain and non-computed, when `cb` can't see the array argument: it
//!   must be a function (inline, or a same-file function declaration or never-reassigned
//!   variable initialized with one) with at most two parameters (three for `reduce`), no
//!   trailing rest, and no `arguments` in a non-arrow. Reported at the temporary array;
//!   the suggestion puts `it` in its place.
//! - `.slice(a[, b])` (no type arguments) on `x.toArray()` (plain, no type arguments, no
//!   open optional chain) or one of the temporary arrays above, with every bound a
//!   statically known non-negative safe integer, not a plain copy: `drop(a)` (when `a > 0`)
//!   and `take(b - a)` (`take(0)` when `b <= a`). Reported at `slice`; the suggestion is
//!   `it.drop(a).take(n).toArray()`.
//!
//! The iterator keeps its parentheses, gains some when it is an open optional chain, starts
//! with `{`, `<`, `function`, `class` or `async`, or can't be a member object; a `;` is
//! added when the code would continue the previous line. No suggestion when a comment
//! outside the iterator would be lost.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_iter::{is_iterator_expression, unwrap_expression};
use super::unicorn_static::{Static, static_value_control_flow};
use super::unicorn_text::{
    has_unparenthesized_optional_chain, is_parenthesized, member_object_needs_parens,
    needs_semicolon, paren_span, paren_text, would_remove_comments,
};
use super::unicorn_util::space_around_keyword;
use crate::semantic::Resolution;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-iterator-helpers",
        "Prefer iterator helpers over temporary arrays from iterators.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<PreferIteratorHelpers>,
};

#[derive(Debug, Default)]
struct PreferIteratorHelpers;

fn has_type_args(ast: &Ast, id: NodeId) -> bool {
    matches!(
        *ast.node(id),
        Node::Call {
            type_args: Some(_),
            ..
        }
    )
}

fn mentions_arguments(ast: &Ast, root: NodeId) -> bool {
    let mut stack = vec![root];
    let mut guard = 0u32;
    while let Some(n) = stack.pop() {
        guard += 1;
        if guard > 1_000_000 {
            return true;
        }
        if matches!(ast.node(n), Node::Ident { .. }) && ident_name(ast, n) == Some("arguments") {
            return true;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

/// `getFunctionFromIdentifier`: the same-file function a never-reassigned name holds.
fn function_from_identifier(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let Resolution::Symbol(sym) = sem.reference(sem.reference_at(ident)?).resolution else {
        return None;
    };
    let symbol = sem.symbol(sym);
    let [decl] = symbol.decls[..] else {
        return None;
    };
    if symbol.refs.iter().any(|&r| {
        let r = sem.reference(r);
        r.is_write() && !r.flags.contains(crate::semantic::RefFlags::INIT)
    }) {
        return None;
    }
    match *ast.node(decl.node) {
        Node::FunctionDecl { body: Some(_), .. } => Some(decl.node),
        Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } if name == decl.name => {
            let init = unwrap_expression(ast, init);
            matches!(
                ast.node(init),
                Node::FunctionExpr { .. } | Node::Arrow { .. }
            )
            .then_some(init)
        }
        _ => None,
    }
}

fn can_observe_array(cx: &Ctx<'_, '_>, callback: NodeId, index: usize) -> bool {
    let ast = cx.ast();
    let mut f = unwrap_expression(ast, callback);
    if matches!(ast.node(f), Node::Ident { .. }) {
        match function_from_identifier(cx, f) {
            Some(g) => f = g,
            None => return true,
        }
    }
    let params = match *ast.node(f) {
        Node::Arrow { params, .. }
        | Node::FunctionExpr { params, .. }
        | Node::FunctionDecl { params, .. } => ast.list(params),
        _ => return true,
    };
    params.len() > index
        || params
            .last()
            .is_some_and(|&p| matches!(ast.node(p), Node::Param { rest: true, .. }))
        || (!matches!(ast.node(f), Node::Arrow { .. }) && mentions_arguments(ast, f))
}

/// `[...it]` or `Array.from(it)`: `it`.
fn temporary_array_iterator(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::ArrayLit { elements } => {
            let [only] = *ast.list(elements) else {
                return None;
            };
            let Node::Spread { arg } = *ast.node(only) else {
                return None;
            };
            is_iterator_expression(cx, arg).then_some(arg)
        }
        Node::Call { .. } => {
            let call = method_call(ast, id)?;
            let obj = skip_parens(ast, call.obj);
            let ok = !has_type_args(ast, id)
                && call.name == "from"
                && call.plain()
                && matches!(ast.node(obj), Node::Ident { .. })
                && ident_name(ast, obj) == Some("Array")
                && call.args.len() == 1
                && !matches!(ast.node(call.args[0]), Node::Spread { .. });
            (ok && is_iterator_expression(cx, call.args[0])).then(|| call.args[0])
        }
        _ => None,
    }
}

/// `x.toArray()`: `x`.
fn to_array_receiver(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let call = method_call(ast, id)?;
    (call.name == "toArray"
        && call.args.is_empty()
        && call.plain()
        && !has_type_args(ast, id)
        && !has_unparenthesized_optional_chain(cx, id))
    .then_some(call.obj)
}

fn first_token_word(text: &str) -> &str {
    let end = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
        .unwrap_or(text.len());
    if end == 0 {
        &text[..text.chars().next().map_or(0, char::len_utf8)]
    } else {
        &text[..end]
    }
}

fn iterator_text(cx: &Ctx<'_, '_>, iterator: NodeId, replaced: NodeId) -> String {
    let iterator = skip_parens(cx.ast(), iterator);
    let text = paren_text(cx, iterator);
    let wrap = !is_parenthesized(cx, iterator)
        && (has_unparenthesized_optional_chain(cx, iterator)
            || matches!(
                first_token_word(cx.text(iterator)),
                "{" | "<" | "function" | "class" | "async"
            )
            || member_object_needs_parens(cx, iterator, replaced));
    if wrap {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

fn suggestion(cx: &Ctx<'_, '_>, node: NodeId, iterator: NodeId, suffix: &str) -> Option<Fix> {
    let span = cx.span(node);
    if would_remove_comments(cx, span, &[paren_span(cx, iterator)]) {
        return None;
    }
    let text = iterator_text(cx, iterator, node) + suffix;
    let semicolon = if needs_semicolon(cx, span.start, &text) {
        ";"
    } else {
        ""
    };
    Some(space_around_keyword(cx, node).into_iter().fold(
        Fix::replace(span, format!("{semicolon}{text}")),
        |f, (s, t)| f.and(s, t),
    ))
}

fn safe_bound(v: Option<Static>) -> Option<i64> {
    match v? {
        Static::Num(n) if n >= 0.0 && n.fract() == 0.0 && n <= 9_007_199_254_740_991.0 => {
            Some(n as i64)
        }
        _ => None,
    }
}

fn slice_calls(cx: &Ctx<'_, '_>, args: &[NodeId]) -> Option<String> {
    let bounds: Vec<i64> = args
        .iter()
        .map(|&a| safe_bound(static_value_control_flow(cx, a)))
        .collect::<Option<_>>()?;
    let start = bounds[0];
    let end = bounds.get(1).copied();
    if end.is_some_and(|e| e <= start) {
        return Some("take(0)".to_owned());
    }
    let mut calls = Vec::new();
    if start > 0 {
        calls.push(format!("drop({start})"));
    }
    if let Some(e) = end {
        calls.push(format!("take({})", e - start));
    }
    (!calls.is_empty()).then(|| calls.join("."))
}

impl PreferIteratorHelpers {
    fn check_slice(&self, id: NodeId, cx: &mut Ctx<'_, '_>) -> bool {
        let ast = cx.ast();
        let Some(call) = method_call(ast, id) else {
            return false;
        };
        if call.name != "slice"
            || !(1..=2).contains(&call.args.len())
            || !call.plain()
            || has_type_args(ast, id)
        {
            return false;
        }
        let array = skip_parens(ast, call.obj);
        let Some(iterator) =
            to_array_receiver(cx, array).or_else(|| temporary_array_iterator(cx, array))
        else {
            return false;
        };
        if matches!(ast.node(skip_parens(ast, iterator)), Node::Super {}) {
            return false;
        }
        let Some(replacement) = slice_calls(cx, call.args) else {
            return false;
        };
        let fix = suggestion(cx, id, iterator, &format!(".{replacement}.toArray()"));
        let r = cx
            .report(
                cx.span(call.prop),
                format!(
                    "Prefer `{replacement}` before materializing the iterator instead of slicing the array."
                ),
            )
            .id("prefer-iterator-helpers/slice");
        if let Some(f) = fix {
            r.suggest(
                "prefer-iterator-helpers/slice-suggestion",
                format!("Use `{replacement}.toArray()`."),
                f,
            );
        }
        true
    }

    fn check_terminal(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, id) else {
            return;
        };
        let index = match call.name {
            "every" | "find" | "forEach" | "some" if call.args.len() == 1 => 2,
            "reduce" if (1..=2).contains(&call.args.len()) => 3,
            _ => return,
        };
        if !call.plain()
            || call
                .args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
            || can_observe_array(cx, call.args[0], index)
        {
            return;
        }
        let array = skip_parens(ast, call.obj);
        let Some(iterator) = temporary_array_iterator(cx, array) else {
            return;
        };
        let method = call.name;
        let fix = suggestion(cx, array, iterator, "");
        let r = cx
            .report(
                cx.span(array),
                format!("Prefer `Iterator#{method}()` over materializing an array."),
            )
            .id("prefer-iterator-helpers");
        if let Some(f) = fix {
            r.suggest(
                "prefer-iterator-helpers/suggestion",
                format!("Use `Iterator#{method}()`."),
                f,
            );
        }
    }
}

impl Rule for PreferIteratorHelpers {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        if !self.check_slice(id, cx) {
            self.check_terminal(id, cx);
        }
    }
}
