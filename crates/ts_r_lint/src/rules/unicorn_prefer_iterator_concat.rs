//! `unicorn/prefer-iterator-concat`: an array made only of two or more spreads, `[...a,
//! ...b]`, that is only iterated (the right side of `for ... of`, `yield*`, or the first
//! argument of `Array.from`/`TypedArray.from`, `Object.fromEntries`, `Promise.all`/
//! `allSettled`/`any`/`race`, or `new Map`/`WeakMap`/`Set`/`WeakSet`/`TypedArray` with one
//! argument) can be `Iterator.concat(a, b)`.
//!
//! Skipped: a spread of `x.toArray()`, a spread of something known to be a string
//! (`Iterator.concat` rejects primitives), and `new Set([...s1, ...s2])` of known sets with
//! the global `Set`. The replacement keeps each spread argument's text with its parentheses;
//! a comment elsewhere in the array means a report without a fix. The fix is automatic when
//! the consumer only stores the values (the `new` forms, one-argument `from` and
//! `Object.fromEntries`) and no argument after the first has a side effect; otherwise it is a
//! suggestion.

use super::prelude::*;
use super::unicorn_ast::{method_call, no_spread_in};
use super::unicorn_text::{has_possible_side_effect, paren_span, would_remove_comments};
use super::unicorn_types::{TYPED_ARRAYS, builtin_collection_type, is_string};
use super::unicorn_util::{space_around_keyword, with_edits};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-iterator-concat",
        "Prefer `Iterator.concat(…)` over temporary spread arrays.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferIteratorConcat>,
};

#[derive(Debug, Default)]
struct PreferIteratorConcat;

const MESSAGE: &str =
    "Use `Iterator.concat(…)` instead of creating a temporary array from spreads.";
const COLLECTIONS: &[&str] = &["Map", "WeakMap", "Set", "WeakSet"];
const PROMISE_METHODS: &[&str] = &["all", "allSettled", "any", "race"];

/// How the array's consumer uses it.
#[derive(PartialEq, Eq)]
enum Consumer {
    /// Only stores the values: the fix is automatic.
    Storing,
    /// Iterates it some other way: the fix is a suggestion.
    Other,
}

/// `Name.method(...)` with a plain (non-optional, non-computed) member on a name in
/// `objects`.
fn static_method<'a>(
    cx: &Ctx<'_, 'a>,
    call: NodeId,
    objects: &[&str],
    methods: &[&str],
) -> Option<&'a [NodeId]> {
    let ast = cx.ast();
    let m = method_call(ast, call)?;
    if !m.plain() || !methods.contains(&m.name) {
        return None;
    }
    let Node::Ident { .. } = *ast.node(m.obj) else {
        return None;
    };
    let obj = ident_name(ast, m.obj)?;
    objects.contains(&obj).then_some(m.args)
}

fn is_array_or_typed(name: &str) -> bool {
    name == "Array" || TYPED_ARRAYS.contains(&name)
}

/// The consumer of `array` (the array's parent, through parentheses), if it only iterates it.
fn consumer(cx: &Ctx<'_, '_>, array: NodeId) -> Option<Consumer> {
    let ast = cx.ast();
    let parent = parent_skip_parens(ast, cx.file.parents, array)?;
    match *ast.node(parent) {
        Node::ForOf { right, .. } if skip_parens(ast, right) == array => Some(Consumer::Other),
        Node::Yield {
            delegate: true,
            arg: Some(arg),
        } if skip_parens(ast, arg) == array => Some(Consumer::Other),
        Node::New { callee, args, .. } => {
            let args = ast.list(args);
            if args.len() != 1 || skip_parens(ast, args[0]) != array {
                return None;
            }
            let Node::Ident { .. } = *ast.node(callee) else {
                return None;
            };
            let name = ident_name(ast, callee)?;
            (COLLECTIONS.contains(&name) || TYPED_ARRAYS.contains(&name))
                .then_some(Consumer::Storing)
        }
        Node::Call { .. } => {
            let m = method_call(ast, parent)?;
            if m.args.first().map(|&a| skip_parens(ast, a)) != Some(array) {
                return None;
            }
            let obj = match *ast.node(m.obj) {
                Node::Ident { .. } => ident_name(ast, m.obj)?,
                _ => return None,
            };
            if !m.plain() {
                return None;
            }
            match m.name {
                "from" if is_array_or_typed(obj) => Some(if m.args.len() == 1 {
                    Consumer::Storing
                } else {
                    Consumer::Other
                }),
                "fromEntries" if obj == "Object" && m.args.len() == 1 => Some(Consumer::Storing),
                _ if static_method(cx, parent, &["Promise"], PROMISE_METHODS).is_some()
                    && m.args.len() == 1
                    && no_spread_in(ast, m.args, 1) =>
                {
                    Some(Consumer::Other)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// `x.toArray()`: no arguments, no optional chaining.
fn is_to_array_call(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    method_call(cx.ast(), id).is_some_and(|m| m.name == "toArray" && m.plain() && m.args.is_empty())
}

impl Rule for PreferIteratorConcat {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ArrayLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::ArrayLit { elements } = *ast.node(node) else {
            return;
        };
        let elements = ast.list(elements);
        if elements.len() < 2 {
            return;
        }
        let mut args = Vec::with_capacity(elements.len());
        for &e in elements {
            let Node::Spread { arg } = *ast.node(e) else {
                return;
            };
            args.push(arg);
        }
        let Some(consumer) = consumer(cx, node) else {
            return;
        };
        if args
            .iter()
            .any(|&a| is_to_array_call(cx, skip_parens(ast, a)) || is_string(cx, a))
        {
            return;
        }
        // `new Set([...a, ...b])` of known sets.
        if let Some(parent) = parent_skip_parens(ast, cx.file.parents, node)
            && let Node::New { callee, .. } = *ast.node(parent)
            && cx.is_global_ref(callee, "Set")
            && args
                .iter()
                .all(|&a| builtin_collection_type(cx, a) == Some("Set"))
        {
            return;
        }

        let kept: Vec<Span> = args.iter().map(|&a| paren_span(cx, a)).collect();
        let span = cx.span(node);
        if would_remove_comments(cx, span, &kept) {
            cx.report(span, MESSAGE).id("prefer-iterator-concat");
            return;
        }
        let src = cx.src();
        let mut text = String::from("Iterator.concat(");
        for (i, k) in kept.iter().enumerate() {
            if i > 0 {
                text.push_str(", ");
            }
            text.push_str(k.text(src));
        }
        text.push(')');
        let fix = with_edits(Fix::replace(span, text), space_around_keyword(cx, node));
        let auto = consumer == Consumer::Storing
            && args[1..].iter().all(|&a| !has_possible_side_effect(cx, a));
        let report = cx.report(span, MESSAGE).id("prefer-iterator-concat");
        if auto {
            report.fix(fix);
        } else {
            report.suggest(
                "prefer-iterator-concat/suggestion",
                "Use `Iterator.concat(…)`.",
                fix,
            );
        }
    }
}
