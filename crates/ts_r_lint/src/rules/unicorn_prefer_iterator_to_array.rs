//! `unicorn/prefer-iterator-to-array`: `[...it]` (the single element a spread) where `it`
//! is surely an iterator: `Iterator.from(x)` / `Iterator.concat(...)` (fixed), or
//! `.entries()` / `.keys()` / `.values()` / `.matchAll(x)` (a suggestion, the receiver may
//! be an array), possibly followed by one-argument `.drop` / `.filter` / `.flatMap` /
//! `.map` / `.take` calls; all plain (no `?.`, no spread in counted arguments). The array
//! is left alone where an iterable is accepted anyway: a `for-of` right side, `yield*`, a
//! spread in an array or call, or the first argument of `new Map/WeakMap/Set/WeakSet(x)`,
//! `new <TypedArray>(x, ...)`, `Promise.all/allSettled/any/race(x)`,
//! `Array/<TypedArray>.from(x, ...)` or `Object.fromEntries(x, ...)`.
//!
//! The replacement is `it.toArray()` (the object parenthesized as a member object needs,
//! the whole wrapped when it is an unparenthesized `new` callee), spaced from a glued
//! keyword; no fix when a comment lies outside the spread argument.

use super::prelude::*;
use super::unicorn_ast::{MethodCall, method_call, no_spread_in, would_remove_comments};
use super::unicorn_text::member_object_text;
use super::unicorn_types::TYPED_ARRAYS;
use super::unicorn_util::{in_unparenthesized_new_callee, space_around_keyword, with_edits};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-iterator-to-array",
        "Prefer `Iterator#toArray()` over temporary arrays from iterator spreads.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferIteratorToArray>,
};

#[derive(Debug, Default)]
struct PreferIteratorToArray;

#[derive(PartialEq)]
enum Kind {
    Fix,
    Suggestion,
}

fn plain_call<'a>(ast: &'a Ast, id: NodeId) -> Option<MethodCall<'a>> {
    method_call(ast, id).filter(MethodCall::plain)
}

fn object_is(ast: &Ast, call: &MethodCall<'_>, names: &[&str]) -> bool {
    ident_name(ast, skip_parens(ast, call.obj)).is_some_and(|n| names.contains(&n))
}

fn iterator_kind(ast: &Ast, id: NodeId) -> Option<Kind> {
    let mut id = skip_parens(ast, id);
    for _ in 0..10_000 {
        let call = plain_call(ast, id)?;
        let n = call.args.len();
        if object_is(ast, &call, &["Iterator"])
            && ((call.name == "from" && n == 1 && no_spread_in(ast, call.args, 1))
                || call.name == "concat")
        {
            return Some(Kind::Fix);
        }
        if (matches!(call.name, "entries" | "keys" | "values") && n == 0)
            || (call.name == "matchAll" && n == 1 && no_spread_in(ast, call.args, 1))
        {
            return Some(Kind::Suggestion);
        }
        if !(matches!(call.name, "drop" | "filter" | "flatMap" | "map" | "take")
            && n == 1
            && no_spread_in(ast, call.args, 1))
        {
            return None;
        }
        id = skip_parens(ast, call.obj);
    }
    None
}

/// The array stands where an iterable is accepted as is.
fn accepts_iterable(cx: &Ctx<'_, '_>, array: NodeId) -> bool {
    let ast = cx.ast();
    let Some(parent) = parent_skip_parens(ast, cx.file.parents, array) else {
        return false;
    };
    let is_first = |args: &[NodeId]| args.first().is_some_and(|&a| skip_parens(ast, a) == array);
    match *ast.node(parent) {
        Node::ForOf { right, .. } => skip_parens(ast, right) == array,
        Node::Yield {
            delegate: true,
            arg: Some(arg),
        } => skip_parens(ast, arg) == array,
        Node::Spread { .. } => cx.parent(parent).is_some_and(|g| {
            matches!(
                ast.node(g),
                Node::ArrayLit { .. } | Node::Call { .. } | Node::New { .. }
            )
        }),
        Node::New {
            callee,
            has_args: true,
            args,
            ..
        } => {
            let args = ast.list(args);
            let Some(name) = ident_name(ast, skip_parens(ast, callee)) else {
                return false;
            };
            let ok = (matches!(name, "Map" | "WeakMap" | "Set" | "WeakSet") && args.len() == 1)
                || (TYPED_ARRAYS.contains(&name) && !args.is_empty());
            ok && no_spread_in(ast, args, 1) && is_first(args)
        }
        Node::Call { .. } => {
            let Some(call) = plain_call(ast, parent) else {
                return false;
            };
            let n = call.args.len();
            let ok = (object_is(ast, &call, &["Promise"])
                && matches!(call.name, "all" | "allSettled" | "any" | "race")
                && n == 1)
                || (call.name == "from" && n >= 1 && {
                    let objects: Vec<&str> = std::iter::once("Array")
                        .chain(TYPED_ARRAYS.iter().copied())
                        .collect();
                    object_is(ast, &call, &objects)
                })
                || (object_is(ast, &call, &["Object"]) && call.name == "fromEntries" && n >= 1);
            ok && no_spread_in(ast, call.args, 1) && is_first(call.args)
        }
        _ => false,
    }
}

impl Rule for PreferIteratorToArray {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ArrayLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::ArrayLit { elements } = *cx.node(node) else {
            return;
        };
        let &[only] = ast.list(elements) else {
            return;
        };
        let Node::Spread { arg } = *ast.node(only) else {
            return;
        };
        if accepts_iterable(cx, node) {
            return;
        }
        let Some(kind) = iterator_kind(ast, arg) else {
            return;
        };
        let fix = (!would_remove_comments(ast, cx.span(node), &[cx.span(arg)])).then(|| {
            let mut text = format!("{}.toArray()", member_object_text(cx, arg, node));
            if in_unparenthesized_new_callee(cx, node) {
                text = format!("({text})");
            }
            with_edits(
                Fix::replace(cx.span(node), text),
                space_around_keyword(cx, node),
            )
        });
        let report = cx
            .report_node(
                node,
                "Prefer `Iterator#toArray()` over a temporary spread array.",
            )
            .id("prefer-iterator-to-array");
        match (fix, kind) {
            (Some(f), Kind::Fix) => {
                report.fix(f);
            }
            (Some(f), Kind::Suggestion) => {
                report.suggest(
                    "prefer-iterator-to-array/suggestion",
                    "Use `Iterator#toArray()`.",
                    f,
                );
            }
            (None, _) => {}
        }
    }
}
