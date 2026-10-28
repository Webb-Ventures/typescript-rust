//! `unicorn/no-useless-spread`: spreads that copy for nothing.
//!
//! - `[...[a]]`, `{...{a}}`, `f(...[a])`: an array or object literal spread straight into
//!   a list is inlined (holes become `undefined` in arguments). Not for objects with a
//!   `__proto__: value` setter or a `super` inside; `new Set(...[...x])` is reported
//!   without a fix.
//! - `Object.assign(target, {...a, ...b})`: a source object made only of spreads (after a
//!   guaranteed non-spread target) is suggested as `a, b`.
//! - `new Set(...iterable)` (also `Map`, `WeakMap`, `WeakSet`) is reported.
//! - `[...iterable]` passed where an iterable is accepted (`new Map(…)`, `Promise.all(…)`,
//!   `Array.from(…)` and the typed-array `from`, `Object.fromEntries(…)`) is unwrapped;
//!   in `for…of` and `yield*` that is only a suggestion (the body may change the source).
//! - `[...x]` around an expression that already makes a new array (`.slice()`, `.concat()`,
//!   `.split()`, `Object.keys()`, `Array.from()`, `await Promise.all()`...) is unwrapped,
//!   except `new Array(n)` (holes) and `.slice()` on something not known to be an array.
//!
//! Unwrapping `[...x]` drops the brackets and the spread, adds parentheses around `x` when
//! it binds looser than a member access or would start a statement badly, wraps a
//! multi-line `return`/`throw` argument in parentheses, gives up for a multi-line `yield`,
//! and keeps a space after a keyword.

use super::prelude::*;
use super::unicorn_text::{is_parenthesized, needs_parens_at_expression_start, paren_text};
use super::unicorn_types::{TYPED_ARRAYS, is_array, is_known_non_array};
use super::unicorn_util::{fix_of, keyword_argument_parent, parenthesize_keyword_argument};
use super::unicorn_util::{in_unparenthesized_new_callee, skip_wrappers, space_around_keyword};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-spread",
        "Disallow unnecessary spread.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoUselessSpread>,
};

#[derive(Debug, Default)]
struct NoUselessSpread;

impl FromOptions for NoUselessSpread {
    fn from_options(_: &[Value]) -> Result<Self, String> {
        Ok(NoUselessSpread)
    }
}

const COLLECTIONS: &[&str] = &["Map", "WeakMap", "Set", "WeakSet"];
const CLONE_METHODS: &[&str] = &[
    "flat",
    "slice",
    "splice",
    "toReversed",
    "toSorted",
    "toSpliced",
    "with",
];
const TYPED_CLONE_METHODS: &[&str] = &[
    "flat",
    "slice",
    "splice",
    "toReversed",
    "toSorted",
    "toSpliced",
    "with",
    "filter",
    "flatMap",
    "map",
];

/// `[...x]` (a trailing comma allowed): the spread's argument.
fn single_spread(ast: &Ast, id: NodeId) -> Option<NodeId> {
    let Node::ArrayLit { elements } = *ast.node(id) else {
        return None;
    };
    match *ast.list(elements) {
        [e] => match *ast.node(e) {
            Node::Spread { arg } => Some(arg),
            _ => None,
        },
        _ => None,
    }
}

/// `new Name(...)` with a callee in `names` and exactly `args` arguments; a spread among
/// them only passes with `allow_spread`.
fn is_new_of(
    ast: &Ast,
    id: NodeId,
    names: &[&str],
    args: usize,
    allow_spread: bool,
) -> Option<&'static str> {
    let Node::New {
        callee, args: list, ..
    } = *ast.node(id)
    else {
        return None;
    };
    let list = ast.list(list);
    if list.len() != args
        || (!allow_spread
            && list
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. })))
    {
        return None;
    }
    let name = ident_name(ast, skip_parens(ast, callee))?;
    COLLECTIONS
        .iter()
        .chain(["Array"].iter())
        .find(|&&n| n == name && names.contains(&n))
        .copied()
}

/// `Object.method(...)`-style call: non-optional, a non-computed member on an identifier in
/// `objects`, a method in `methods`; `args` exact or `min_args` minimum count.
fn static_method_call<'a>(
    ast: &'a Ast,
    id: NodeId,
    objects: &[&str],
    methods: &[&str],
    args: Option<usize>,
    min_args: usize,
) -> Option<(&'a str, &'a str)> {
    let Node::Call {
        callee,
        optional: false,
        args: list,
        ..
    } = *ast.node(id)
    else {
        return None;
    };
    let n = ast.list(list).len();
    if args.is_some_and(|a| a != n) || n < min_args {
        return None;
    }
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    let object = ident_name(ast, skip_parens(ast, obj))?;
    let method = ident_name(ast, prop)?;
    (objects.contains(&object) && methods.contains(&method)).then_some((object, method))
}

/// `x.method(...)` without `?.`, method in `methods`.
fn plain_method_call(ast: &Ast, id: NodeId, methods: &[&str]) -> bool {
    let Node::Call {
        callee,
        optional: false,
        ..
    } = *ast.node(id)
    else {
        return false;
    };
    matches!(*ast.node(skip_parens(ast, callee)), Node::Member { optional: false, prop, .. }
        if ident_name(ast, prop).is_some_and(|m| methods.contains(&m)))
}

fn callee_object(ast: &Ast, call: NodeId) -> Option<NodeId> {
    let Node::Call { callee, .. } = *ast.node(call) else {
        return None;
    };
    match *ast.node(skip_parens(ast, callee)) {
        Node::Member { obj, .. } => Some(skip_parens(ast, obj)),
        _ => None,
    }
}

fn is_strong_precedence(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::ArrayLit { .. }
            | Node::Call { .. }
            | Node::Ident { .. }
            | Node::ImportCall { .. }
            | Node::NullLit {}
            | Node::BoolLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::StringLit { .. }
            | Node::RegexLit { .. }
            | Node::Member { .. }
            | Node::Index { .. }
            | Node::New { .. }
            | Node::Super {}
            | Node::TaggedTemplate { .. }
            | Node::TemplateLit { .. }
            | Node::This {}
            | Node::NonNull { .. }
    )
}

fn next_token(src: &str, pos: u32) -> u32 {
    skip_trivia_forward(src, pos as usize) as u32
}

fn prev_token_end(cx: &Ctx<'_, '_>, pos: u32) -> u32 {
    skip_trivia_backward(cx.ast(), cx.src(), pos as usize) as u32
}

fn byte_at(src: &str, pos: u32) -> Option<u8> {
    src.as_bytes().get(pos as usize).copied()
}

fn remove(pos: u32, len: u32) -> (Span, String) {
    (Span::new(pos, pos + len), String::new())
}

/// The fix that turns `[...x]` into `x`, or `None` when it gives up.
fn unwrap_single_spread(cx: &Ctx<'_, '_>, array: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let src = cx.src();
    let lines = cx.file.lines;
    let arg = single_spread(ast, array)?;
    let span = cx.span(array);
    let open = span.start;
    let spread = next_token(src, open + 1);
    let third = next_token(src, spread + 3);
    let same_line = lines.line_of(open + 1) == lines.line_of(third);
    let keyword_parent = keyword_argument_parent(cx, array);
    if keyword_parent.is_some_and(|k| matches!(cx.node(k), Node::Yield { .. })) && !same_line {
        return None;
    }
    let mut edits = vec![remove(open, 1)];
    let add_parens = !is_parenthesized(cx, arg)
        && (in_unparenthesized_new_callee(cx, array)
            || needs_parens_at_expression_start(cx, arg, array)
            || (!is_strong_precedence(ast, arg)
                && !is_strong_precedence(ast, skip_wrappers(ast, arg))));
    if add_parens {
        edits.push((Span::at(open + 1), "(".into()));
    }
    edits.push(remove(spread, 3));
    let close = span.end - 1;
    edits.push(remove(close, 1));
    if add_parens {
        edits.push((Span::at(close), ")".into()));
    }
    let before_close = prev_token_end(cx, close);
    if before_close > 0 && byte_at(src, before_close - 1) == Some(b',') {
        edits.push(remove(before_close - 1, 1));
    }
    if let Some(k) = keyword_parent
        && matches!(cx.node(k), Node::Return { .. } | Node::Throw { .. })
        && !same_line
    {
        edits.extend(parenthesize_keyword_argument(cx, k));
        return Some(fix_of(edits));
    }
    edits.extend(space_around_keyword(cx, array));
    Some(fix_of(edits))
}

fn is_proto_setter(cx: &Ctx<'_, '_>, prop: NodeId) -> bool {
    let ast = cx.ast();
    let Node::Property { key, .. } = *ast.node(prop) else {
        return false;
    };
    !matches!(ast.node(key), Node::ComputedKey { .. })
        && key_name(ast, cx.src(), key).as_deref() == Some("__proto__")
}

/// `[...x]`'s `x` already is a fresh array: `(known, heuristic)` — known from types, or a
/// call that is assumed to return a new array.
fn array_clone(cx: &Ctx<'_, '_>, node: NodeId) -> (bool, bool) {
    let ast = cx.ast();
    let known = plain_method_call(ast, node, TYPED_CLONE_METHODS)
        && callee_object(ast, node).is_some_and(|o| is_array(cx, o))
        && is_array(cx, node);
    if known {
        return (true, false);
    }
    if is_known_non_array(cx, node) {
        return (false, false);
    }
    let non_array_receiver = || callee_object(ast, node).is_some_and(|o| is_known_non_array(cx, o));
    let heuristic = if plain_method_call(ast, node, CLONE_METHODS) {
        !non_array_receiver()
    } else if plain_method_call(ast, node, &["concat"]) {
        !(callee_object(ast, node).and_then(|o| ident_name(ast, o)) == Some("Iterator")
            || non_array_receiver())
    } else {
        plain_method_call(ast, node, &["split"])
            || static_method_call(ast, node, &["Object"], &["keys", "values"], Some(1), 0).is_some()
            || matches!(*ast.node(node), Node::Await { arg }
                if static_method_call(ast, skip_parens(ast, arg), &["Promise"], &["all", "allSettled"], Some(1), 0).is_some())
            || static_method_call(ast, node, &["Array"], &["from", "of"], None, 0).is_some()
            || is_new_any_args(ast, node, "Array")
    };
    (false, heuristic)
}

fn is_new_any_args(ast: &Ast, id: NodeId, name: &str) -> bool {
    matches!(*ast.node(id), Node::New { callee, .. } if ident_name(ast, skip_parens(ast, callee)) == Some(name))
}

impl NoUselessSpread {
    fn spread_in_list(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Some(spread) = parent_skip_parens(ast, cx.file.parents, node) else {
            return;
        };
        if !matches!(ast.node(spread), Node::Spread { .. }) {
            return;
        }
        let Some(list) = cx.parent(spread) else {
            return;
        };
        let is_object = matches!(ast.node(node), Node::ObjectLit { .. });
        let parent_description = match (ast.node(list), is_object) {
            (Node::ObjectLit { .. }, true) => "object literal",
            (Node::ArrayLit { .. }, false) => "array literal",
            (Node::Call { args, .. } | Node::New { args, .. }, false)
                if ast.list(*args).contains(&spread) =>
            {
                "arguments"
            }
            _ => return,
        };
        if let Node::ObjectLit { props } = *ast.node(node)
            && (ast.list(props).iter().any(|&p| is_proto_setter(cx, p))
                || super::unicorn_ast::contains_node(ast, node, |n| matches!(n, Node::Super {})))
        {
            return;
        }
        let spread_start = cx.span(spread).start;
        let message = format!(
            "Spread an {} literal in {parent_description} is unnecessary.",
            if is_object { "object" } else { "array" }
        );
        let is_new = matches!(ast.node(list), Node::New { .. });
        if is_new
            && single_spread(ast, node).is_some()
            && is_new_of(ast, list, COLLECTIONS, 1, true).is_some()
        {
            cx.report(Span::new(spread_start, spread_start + 3), message)
                .id("spread-in-list");
            return;
        }
        let mut edits = vec![remove(spread_start, 3)];
        // Parentheses around the literal.
        let mut cur = node;
        while let Some(p) = cx.parent(cur) {
            if !matches!(cx.node(p), Node::Paren { .. }) {
                break;
            }
            let s = cx.span(p);
            edits.push(remove(s.start, 1));
            edits.push(remove(s.end - 1, 1));
            cur = p;
        }
        let span = cx.span(node);
        edits.push(remove(span.start, 1));
        let close = span.end - 1;
        edits.push(remove(close, 1));
        let penultimate = prev_token_end(cx, close);
        if penultimate > span.start + 1 && byte_at(src, penultimate - 1) == Some(b',') {
            edits.push(remove(penultimate - 1, 1));
        }
        let elements: &[NodeId] = match *ast.node(node) {
            Node::ArrayLit { elements } => ast.list(elements),
            Node::ObjectLit { props } => ast.list(props),
            _ => &[],
        };
        if elements.is_empty() {
            let after = next_token(src, cx.span(spread).end);
            if byte_at(src, after) == Some(b',') {
                edits.push(remove(after, 1));
            }
        }
        if matches!(ast.node(list), Node::Call { .. } | Node::New { .. }) {
            let mut pos = span.start + 1;
            for (i, &e) in elements.iter().enumerate() {
                let is_hole = matches!(ast.node(e), Node::Hole {});
                let comma = if i == elements.len() - 1 {
                    (penultimate > span.start + 1 && byte_at(src, penultimate - 1) == Some(b','))
                        .then(|| penultimate - 1)
                } else {
                    let from = if is_hole { pos } else { cx.span(e).end };
                    let at = next_token(src, from);
                    (byte_at(src, at) == Some(b',')).then_some(at)
                };
                if let Some(c) = comma {
                    pos = c + 1;
                    if is_hole {
                        edits.push((Span::at(c), "undefined".into()));
                    }
                }
            }
        }
        cx.report(Span::new(spread_start, spread_start + 3), message)
            .id("spread-in-list")
            .fix(fix_of(edits));
    }

    fn object_assign(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::ObjectLit { props } = *ast.node(node) else {
            return;
        };
        let props = ast.list(props);
        if props.is_empty()
            || !props.iter().all(|&p| match *ast.node(p) {
                Node::Spread { arg } => {
                    !matches!(ast.node(skip_parens(ast, arg)), Node::ObjectLit { .. })
                }
                _ => false,
            })
        {
            return;
        }
        let Some(call) = parent_skip_parens(ast, cx.file.parents, node) else {
            return;
        };
        if static_method_call(ast, call, &["Object"], &["assign"], None, 2).is_none() {
            return;
        }
        let Node::Call { args, .. } = *ast.node(call) else {
            return;
        };
        let args = ast.list(args);
        let Some(index) = args.iter().position(|&a| skip_parens(ast, a) == node) else {
            return;
        };
        if index == 0
            || !args[..index]
                .iter()
                .any(|&a| !matches!(ast.node(a), Node::Spread { .. }))
        {
            return;
        }
        let span = cx.span(node);
        let first = cx.span(props[0]).start;
        let has_comments = !cx.ast().comments_in(span).is_empty();
        let replacement = props
            .iter()
            .filter_map(|&p| match *ast.node(p) {
                Node::Spread { arg } => Some(paren_text(cx, skip_parens(ast, arg)).to_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(", ");
        let message =
            "`Object.assign(…)` source object with only spread properties is unnecessary.";
        let report = cx
            .report(Span::new(first, first + 3), message)
            .id("spread-in-object-assign");
        if !has_comments {
            report.suggest(
                "suggestion/remove-object-assign-spread",
                "Remove the object literal wrapper.",
                Fix::replace(span, replacement),
            );
        }
    }

    fn collection_constructor(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(name) = is_new_of(ast, node, COLLECTIONS, 1, true) else {
            return;
        };
        let Node::New { args, .. } = *ast.node(node) else {
            return;
        };
        let arg = ast.list(args)[0];
        let Node::Spread { arg: inner } = *ast.node(arg) else {
            return;
        };
        if matches!(ast.node(skip_parens(ast, inner)), Node::ArrayLit { .. }) {
            return;
        }
        let span = cx.span(arg);
        cx.report(
            span,
            format!("`new {name}(…)` accepts a single iterable argument, spreading is misleading."),
        )
        .id("spread-in-collection-constructor");
    }

    fn iterable_to_array(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if single_spread(ast, node).is_none() {
            return;
        }
        let Some(parent) = parent_skip_parens(ast, cx.file.parents, node) else {
            return;
        };
        let first_arg = || match *ast.node(parent) {
            Node::Call { args, .. } | Node::New { args, .. } => {
                ast.list(args).first().map(|&a| skip_parens(ast, a)) == Some(node)
            }
            _ => false,
        };
        let (id, message, suggest) = match *ast.node(parent) {
            Node::ForOf { right, .. } if skip_parens(ast, right) == node => (
                "iterable-to-array-in-for-of",
                "`for…of` can iterate over an iterable, it's unnecessary to convert to an array."
                    .to_owned(),
                true,
            ),
            Node::Yield {
                delegate: true,
                arg: Some(arg),
            } if skip_parens(ast, arg) == node => (
                "iterable-to-array-in-yield-star",
                "`yield*` can delegate to an iterable, it's unnecessary to convert to an array."
                    .to_owned(),
                true,
            ),
            Node::New { .. } if first_arg() => {
                let Some(name) = is_new_of(ast, parent, COLLECTIONS, 1, false) else {
                    return;
                };
                (
                    "iterable-to-array",
                    format!(
                        "`new {name}(…)` accepts an iterable as an argument, it's unnecessary to convert to an array."
                    ),
                    false,
                )
            }
            Node::Call { .. } if first_arg() => {
                let mut objects = vec!["Array"];
                objects.extend_from_slice(TYPED_ARRAYS);
                let Some((object, method)) = static_method_call(
                    ast,
                    parent,
                    &["Promise"],
                    &["all", "allSettled", "any", "race"],
                    Some(1),
                    0,
                )
                .or_else(|| static_method_call(ast, parent, &objects, &["from"], Some(1), 0))
                .or_else(|| {
                    static_method_call(ast, parent, &["Object"], &["fromEntries"], Some(1), 0)
                }) else {
                    return;
                };
                (
                    "iterable-to-array",
                    format!(
                        "`{object}.{method}(…)` accepts an iterable as an argument, it's unnecessary to convert to an array."
                    ),
                    false,
                )
            }
            _ => return,
        };
        let fix = unwrap_single_spread(cx, node);
        let report = cx.report(cx.span(node), message).id(id);
        match (fix, suggest) {
            (Some(fix), true) => {
                report.suggest(
                    "suggestion/remove-iterable-array",
                    "Remove the array conversion.",
                    fix,
                );
            }
            (fix, _) => {
                report.fix_opt(fix);
            }
        }
    }

    fn clone_array(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(arg) = single_spread(ast, node) else {
            return;
        };
        let arg = skip_parens(ast, arg);
        let (known, heuristic) = array_clone(cx, arg);
        if !known && !heuristic {
            return;
        }
        let no_fix = is_new_any_args(ast, arg, "Array")
            || (plain_method_call(ast, arg, &["slice"]) && !known);
        let fix = if no_fix {
            None
        } else {
            unwrap_single_spread(cx, node)
        };
        cx.report(cx.span(node), "Unnecessarily cloning an array.")
            .id("clone-array")
            .fix_opt(fix);
    }
}

impl Rule for NoUselessSpread {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ArrayLit, NodeKind::ObjectLit, NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match cx.node(node) {
            Node::ArrayLit { .. } => {
                self.spread_in_list(node, cx);
                self.iterable_to_array(node, cx);
                self.clone_array(node, cx);
            }
            Node::ObjectLit { .. } => {
                self.spread_in_list(node, cx);
                self.object_assign(node, cx);
            }
            Node::New { .. } => self.collection_constructor(node, cx),
            _ => {}
        }
    }
}
