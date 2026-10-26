//! `unicorn/require-array-sort-compare`: `x.sort()` / `x.toSorted()` (not optionally called)
//! with no argument or `undefined`, on a receiver not known to be a non-array. Two
//! suggestions add or replace the comparator with `(a, b) => a - b` or
//! `(a, b) => a.localeCompare(b)`, unless replacing would drop a comment.

use super::prelude::*;
use super::unicorn_ast::{call_tokens, method_call};
use super::unicorn_text::would_remove_comments;
use super::unicorn_types::is_known_non_array;
use super::unicorn_util::parenthesized_span;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/require-array-sort-compare",
        "Require a compare function when calling `Array#sort()` or `Array#toSorted()`.",
        NO_PRESET,
        Category::Suspicious,
    )
    .suggestions(),
    create: create_default::<RequireArraySortCompare>,
};

#[derive(Debug, Default)]
struct RequireArraySortCompare;

impl Rule for RequireArraySortCompare {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node) else {
            return;
        };
        if !matches!(call.name, "sort" | "toSorted") || call.optional_call {
            return;
        }
        let first = match *call.args {
            [] => None,
            [a] if !matches!(ast.node(a), Node::Spread { .. }) => {
                if !is_undefined(ast, skip_parens(ast, a)) {
                    return;
                }
                Some(a)
            }
            _ => return,
        };
        if is_known_non_array(cx, call.obj) {
            return;
        }
        let fixes = match first {
            Some(a) => {
                let range = parenthesized_span(cx, a);
                (!would_remove_comments(cx, range, &[])).then(|| {
                    [
                        Fix::replace(range, "(a, b) => a - b"),
                        Fix::replace(range, "(a, b) => a.localeCompare(b)"),
                    ]
                })
            }
            None => call_tokens(cx, node).map(|(_, close, _)| {
                [
                    Fix::insert(close, "(a, b) => a - b"),
                    Fix::insert(close, "(a, b) => a.localeCompare(b)"),
                ]
            }),
        };
        let mut report = cx
            .report(
                cx.span(call.prop),
                "Pass a compare function to avoid sorting elements as strings.",
            )
            .id("require-array-sort-compare");
        if let Some([numeric, string]) = fixes {
            report = report
                .suggest(
                    "require-array-sort-compare/numeric",
                    "Sort numerically.",
                    numeric,
                )
                .suggest(
                    "require-array-sort-compare/string",
                    "Sort strings with `String#localeCompare()`.",
                    string,
                );
        }
        let _ = report;
    }
}
