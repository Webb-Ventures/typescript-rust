//! `unicorn/no-array-method-this-argument`: no `thisArg` in array callbacks.
//!
//! - `x.every|filter|find|findLast|findIndex|findLastIndex|flatMap|forEach|map|some(cb, t)`
//!   (exactly two non-spread arguments, not an optional call) unless the callee is a known
//!   library function (`_.map`, `React.Children.forEach`, `R.find`, ...), `cb` can't be a
//!   function, or the receiver is known not to be an array.
//! - `Array.from(src, cb, t)` / `Array.fromAsync(src, cb, t)` (exactly three, nothing
//!   optional) unless `cb` can't be a function.
//!
//! The `thisArg` is reported. Removing it (the preceding comma through its parentheses; not
//! when that would drop a comment) is a fix for an arrow callback when `t` is an identifier,
//! a literal, `this` or a static string, and a suggestion for other arrow callbacks. For any
//! other callback both "remove" and "bind" (`cb.bind(t)`, wrapping `cb` when it needs it as a
//! member object) are suggestions.

use super::prelude::*;
use super::unicorn_types::should_skip_known_non_array_receiver;
use super::unicorn_util::parenthesized_span;
use super::unicorn_util_h5::{
    is_parenthesized, is_value_not_function, member_object_needs_parens, method_call_of,
    node_matches_path, parenthesized_text, static_string, would_remove_comments,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-array-method-this-argument",
        "Disallow using the `this` argument in array methods.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<NoArrayMethodThisArgument>,
};

#[derive(Debug, Default)]
struct NoArrayMethodThisArgument;

const METHODS: &[&str] = &[
    "every",
    "filter",
    "find",
    "findLast",
    "findIndex",
    "findLastIndex",
    "flatMap",
    "forEach",
    "map",
    "some",
];

const IGNORED: &[&str] = &[
    "lodash.every",
    "_.every",
    "underscore.every",
    "lodash.filter",
    "_.filter",
    "underscore.filter",
    "Vue.filter",
    "R.filter",
    "lodash.find",
    "_.find",
    "underscore.find",
    "R.find",
    "lodash.findLast",
    "_.findLast",
    "underscore.findLast",
    "R.findLast",
    "lodash.findIndex",
    "_.findIndex",
    "underscore.findIndex",
    "R.findIndex",
    "lodash.findLastIndex",
    "_.findLastIndex",
    "underscore.findLastIndex",
    "R.findLastIndex",
    "lodash.flatMap",
    "_.flatMap",
    "lodash.forEach",
    "_.forEach",
    "React.Children.forEach",
    "Children.forEach",
    "R.forEach",
    "lodash.map",
    "_.map",
    "underscore.map",
    "React.Children.map",
    "Children.map",
    "jQuery.map",
    "$.map",
    "R.map",
    "lodash.some",
    "_.some",
    "underscore.some",
];

impl Rule for NoArrayMethodThisArgument {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, .. } = *cx.node(node) else {
            return;
        };
        if let Some((obj, prop, &[callback, this_arg])) =
            method_call_of(ast, node, METHODS, 2, Some(2), None)
        {
            let callee = skip_parens(ast, callee);
            if IGNORED.iter().any(|p| node_matches_path(ast, callee, p))
                || is_value_not_function(ast, skip_parens(ast, callback))
                || should_skip_known_non_array_receiver(cx, obj)
            {
                return;
            }
            let method = ident_name(ast, prop).unwrap_or_default();
            report(
                cx,
                callback,
                this_arg,
                format!("Do not use the `this` argument in `Array#{method}()`."),
                "error-prototype-method",
            );
            return;
        }
        if let Some((obj, prop, &[_, callback, this_arg])) =
            method_call_of(ast, node, &["from", "fromAsync"], 3, Some(3), Some(false))
        {
            if ident_name(ast, obj) != Some("Array")
                || is_value_not_function(ast, skip_parens(ast, callback))
            {
                return;
            }
            let method = ident_name(ast, prop).unwrap_or_default();
            report(
                cx,
                callback,
                this_arg,
                format!("Do not use the `this` argument in `Array.{method}()`."),
                "error-static-method",
            );
        }
    }
}

/// `, thisArg` with its parentheses: from the comma before it.
fn removal_span(cx: &Ctx<'_, '_>, callback: NodeId, this_arg: NodeId) -> Span {
    let ast = cx.ast();
    let prev_end = parenthesized_span(cx, skip_parens(ast, callback)).end;
    let arg = parenthesized_span(cx, skip_parens(ast, this_arg));
    let comments = ast.comments_in(Span::new(prev_end, arg.start));
    let src = cx.src();
    let comma = (prev_end..arg.start)
        .find(|&i| {
            src.as_bytes().get(i as usize) == Some(&b',')
                && !comments.iter().any(|c| c.span.start <= i && i < c.span.end)
        })
        .unwrap_or(prev_end);
    Span::new(comma, arg.end)
}

fn is_simple_this_argument(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::Ident { .. }
            | Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
            | Node::This {}
    ) || static_string(ast, id).is_some()
}

fn report(
    cx: &mut Ctx<'_, '_>,
    callback: NodeId,
    this_arg: NodeId,
    message: String,
    id: &'static str,
) {
    let ast = cx.ast();
    let span = removal_span(cx, callback, this_arg);
    let remove = (!would_remove_comments(cx, span, &[])).then(|| Fix::delete(span));
    let inner_this = skip_parens(ast, this_arg);
    let inner_callback = skip_parens(ast, callback);
    if matches!(ast.node(inner_callback), Node::Arrow { .. }) {
        let simple = is_simple_this_argument(ast, inner_this);
        let r = cx.report_node(inner_this, message).id(id);
        match remove {
            Some(fix) if simple => {
                r.fix(fix);
            }
            Some(fix) => {
                r.suggest("suggestion-remove", "Remove this argument.", fix);
            }
            None => {}
        }
        return;
    }
    let bind = remove.as_ref().map(|remove| {
        let callback_span = parenthesized_span(cx, inner_callback);
        let mut fix = remove.clone();
        let mut bound = format!(".bind({})", parenthesized_text(cx, inner_this));
        if !is_parenthesized(cx, inner_callback)
            && member_object_needs_parens(cx, inner_callback, inner_callback)
        {
            fix = fix.and(Span::at(callback_span.start), "(");
            bound.insert(0, ')');
        }
        fix.and(Span::at(callback_span.end), bound)
    });
    let r = cx.report_node(inner_this, message).id(id);
    if let (Some(remove), Some(bind)) = (remove, bind) {
        r.suggest("suggestion-remove", "Remove this argument.", remove)
            .suggest("suggestion-bind", "Use a bound function.", bind);
    }
}
