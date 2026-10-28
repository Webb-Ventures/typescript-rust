//! `unicorn/prefer-array-flat-map`: `.flatMap(…)` over `.map(…).flat()` and
//! `.filter(…).flatMap(…)`.
//!
//! - `x.map(f).flat()` / `.flat(1)` (raw `1`; `flat` not optional; the `map` call not an
//!   optional call): skipped for `React.Children` / `Children` receivers and receivers known
//!   not to be arrays. Reported from `map` to the end of the call. The fix removes `.flat()`
//!   and renames `map`; none with a comment outside the `map` call or type arguments on
//!   either call. It is a fix when the callback is an arrow returning an array literal or
//!   free of side effects, else a suggestion.
//! - `x.filter(p => test).flatMap(p => [a, b, ...])`: both single-argument, non-optional,
//!   non-computed, without type arguments; both callbacks plain expression-bodied arrows of
//!   the same untyped identifier; the `flatMap` body an array literal of 2+ elements; no
//!   optional link in `x`, and `x` not known to be a non-indexed collection. Suggests
//!   `x.flatMap(p => test ? [a, b] : [])` unless the call holds a comment.

use super::prelude::*;
use super::unicorn_types::{is_known_non_indexed_collection, should_skip_known_non_array_receiver};
use super::unicorn_util_h5::{
    has_type_arguments, has_unparenthesized_optional_chain, is_parenthesized,
    is_side_effect_free_function, member_object_text, method_call_of, node_matches_path,
    parenthesized_text, remove_method_call, would_remove_comments,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-array-flat-map",
        "Prefer `.flatMap(…)` over `.map(…).flat()` and `.filter(…).flatMap(…)`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferArrayFlatMap>,
};

#[derive(Debug, Default)]
struct PreferArrayFlatMap;

impl Rule for PreferArrayFlatMap {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        check_map_flat(cx, node);
        check_filter_flat_map(cx, node);
    }
}

fn check_map_flat(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Some((flat_obj, _, args)) = method_call_of(ast, node, &["flat"], 0, None, Some(false))
    else {
        return;
    };
    match *args {
        [] => {}
        [only]
            if matches!(ast.node(skip_parens(ast, only)), Node::NumberLit { .. })
                && cx.text(skip_parens(ast, only)) == "1" => {}
        _ => return,
    }
    let map_call = skip_parens(ast, flat_obj);
    let Some((receiver, map_prop, map_args)) =
        method_call_of(ast, map_call, &["map"], 0, None, None)
    else {
        return;
    };
    if node_matches_path(ast, receiver, "React.Children")
        || node_matches_path(ast, receiver, "Children")
        || should_skip_known_non_array_receiver(cx, receiver)
    {
        return;
    }
    let fix = (!would_remove_comments(cx, cx.span(node), &[cx.span(map_call)])
        && !has_type_arguments(cx, node)
        && !has_type_arguments(cx, map_call))
    .then(|| remove_method_call(cx, node))
    .flatten()
    .map(|f| f.and(ast.span(map_prop), "flatMap"));
    let safe = map_args.first().is_some_and(|&a| {
        let a = skip_parens(ast, a);
        matches!(*ast.node(a), Node::Arrow { body, .. }
            if matches!(ast.node(skip_parens(ast, body)), Node::ArrayLit { .. }))
            || is_side_effect_free_function(cx, a)
    });
    let span = Span::new(ast.span(map_prop).start, cx.span(node).end);
    let r = cx
        .report(span, "Prefer `.flatMap(…)` over `.map(…).flat()`.")
        .id("prefer-array-flat-map");
    if let Some(f) = fix {
        if safe {
            r.fix(f);
        } else {
            r.suggest(
                "prefer-array-flat-map/suggestion",
                "Replace `.map(…).flat()` with `.flatMap(…)`.",
                f,
            );
        }
    }
}

/// A plain expression-bodied arrow of one untyped identifier parameter: its parameter
/// identifier and body.
fn simple_arrow(ast: &ts_r_ast::Ast, id: NodeId) -> Option<(NodeId, NodeId)> {
    let Node::Arrow {
        flags,
        type_params: None,
        params,
        return_type: None,
        body,
    } = *ast.node(skip_parens(ast, id))
    else {
        return None;
    };
    if flags.contains(FnFlags::ASYNC) || !flags.contains(FnFlags::EXPRESSION_BODY) {
        return None;
    }
    let &[param] = ast.list(params) else {
        return None;
    };
    let Node::Param {
        pattern,
        rest: false,
        optional: false,
        type_ann: None,
        init: None,
        ..
    } = *ast.node(param)
    else {
        return None;
    };
    ident_name(ast, pattern)?;
    Some((pattern, body))
}

fn check_filter_flat_map(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Some((filter_call, flat_map_prop, &[flat_map_cb])) =
        method_call_of(ast, node, &["flatMap"], 1, Some(1), Some(false))
    else {
        return;
    };
    if has_type_arguments(cx, node) {
        return;
    }
    let filter_call = skip_parens(ast, filter_call);
    let Some((receiver, _, &[filter_cb])) =
        method_call_of(ast, filter_call, &["filter"], 1, Some(1), Some(false))
    else {
        return;
    };
    if has_type_arguments(cx, filter_call)
        || has_unparenthesized_optional_chain(ast, skip_parens(ast, receiver))
        || is_known_non_indexed_collection(cx, receiver)
    {
        return;
    }
    let (Some((filter_param, filter_body)), Some((map_param, map_body))) =
        (simple_arrow(ast, filter_cb), simple_arrow(ast, flat_map_cb))
    else {
        return;
    };
    let mapped = skip_parens(ast, map_body);
    let Node::ArrayLit { elements } = *ast.node(mapped) else {
        return;
    };
    if ast.list(elements).len() < 2 || ident_name(ast, filter_param) != ident_name(ast, map_param) {
        return;
    }
    let receiver = skip_parens(ast, receiver);
    let test = skip_parens(ast, filter_body);
    let test_text = if is_parenthesized(cx, test) {
        parenthesized_text(cx, test).to_owned()
    } else if matches!(
        ast.node(test),
        Node::Arrow { .. }
            | Node::Assign { .. }
            | Node::ClassExpr { .. }
            | Node::Conditional { .. }
            | Node::FunctionExpr { .. }
            | Node::ObjectLit { .. }
            | Node::Seq { .. }
            | Node::As { .. }
            | Node::NonNull { .. }
            | Node::Satisfies { .. }
            | Node::TypeAssertion { .. }
            | Node::Yield { .. }
    ) {
        format!("({})", cx.text(test))
    } else {
        cx.text(test).to_owned()
    };
    let text = format!(
        "{}.flatMap({} => {} ? {} : [])",
        member_object_text(cx, receiver, node),
        cx.text(filter_param),
        test_text,
        cx.text(mapped)
    );
    let fix =
        (!would_remove_comments(cx, cx.span(node), &[])).then(|| Fix::replace(cx.span(node), text));
    let r = cx
        .report_node(
            flat_map_prop,
            "Prefer a single `.flatMap(…)` over `.filter(…).flatMap(…)`.",
        )
        .id("prefer-array-flat-map/filter-flat-map");
    if let Some(f) = fix {
        r.suggest(
            "prefer-array-flat-map/filter-flat-map-suggestion",
            "Replace `.filter(…).flatMap(…)` with a single `.flatMap(…)`.",
            f,
        );
    }
}
