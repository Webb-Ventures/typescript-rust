//! `unicorn/require-array-join-separator`: `array.join()` must pass its separator.
//!
//! Reported: `x.join()` (no arguments, not an optional call, receiver not known to be a
//! non-array), and `[].join.call(x)` / `Array.prototype.join.call(x)` (one non-spread
//! argument, nothing optional). The report runs from the `(` of an empty call, or from the
//! end of the last argument token, to the `)`. The fix inserts `','` before the `)`
//! (`, ','` after an argument, ` ',',` after a trailing comma).

use super::prelude::*;
use super::unicorn_types::should_skip_known_non_array_receiver;
use super::unicorn_util_h5::method_call_of;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/require-array-join-separator",
        "Enforce using the separator argument with `Array#join()`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<RequireArrayJoinSeparator>,
};

#[derive(Debug, Default)]
struct RequireArrayJoinSeparator;

/// `[].join` or `Array.prototype.join` (non-optional, non-computed).
fn is_array_prototype_join(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    let Node::Member {
        obj,
        prop,
        optional: false,
    } = *ast.node(skip_parens(ast, id))
    else {
        return false;
    };
    if ident_name(ast, prop) != Some("join") {
        return false;
    }
    let obj = skip_parens(ast, obj);
    matches!(*ast.node(obj), Node::ArrayLit { elements } if ast.list(elements).is_empty())
        || matches!(*ast.node(obj), Node::Member { obj: o, prop: p, optional: false }
            if ident_name(ast, p) == Some("prototype") && ident_name(ast, skip_parens(ast, o)) == Some("Array"))
}

impl Rule for RequireArrayJoinSeparator {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let prototype_form =
            if let Some((obj, _, _)) = method_call_of(ast, node, &["join"], 0, Some(0), None) {
                if should_skip_known_non_array_receiver(cx, obj) {
                    return;
                }
                false
            } else if let Some((obj, _, _)) =
                method_call_of(ast, node, &["call"], 1, Some(1), Some(false))
                && is_array_prototype_join(ast, obj)
            {
                true
            } else {
                return;
            };
        let src = cx.src();
        let span = cx.span(node);
        if !src[..span.end as usize].ends_with(')') {
            return;
        }
        let close = span.end - 1;
        let before = skip_trivia_backward(ast, src, close as usize) as u32;
        if before == 0 {
            return;
        }
        let trailing_comma = src.as_bytes()[before as usize - 1] == b',';
        let (start, text) = if prototype_form {
            (before, if trailing_comma { " ','," } else { ", ','" })
        } else {
            (before - 1, "','")
        };
        cx.report(
            Span::new(start, span.end),
            "Missing the separator argument.",
        )
        .id("require-array-join-separator")
        .fix(Fix::insert(close, text));
    }
}
