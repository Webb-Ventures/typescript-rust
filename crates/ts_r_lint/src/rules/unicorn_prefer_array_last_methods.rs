//! `unicorn/prefer-array-last-methods`: `a.reverse().find(f)` (or `toReversed()`) searches
//! from the end; `a.findLast(f)` says so without the copy or the mutation. Also `findIndex`,
//! `indexOf` and `reduce`. The suggestion renames the method and removes the reversing call,
//! unless that would drop a comment outside the receiver.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_text::{paren_span, would_remove_comments};
use super::unicorn_util_h2::remove_method_call;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-array-last-methods",
        "Prefer last-oriented array methods over `Array#reverse()` or `Array#toReversed()` followed by a method.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<PreferArrayLastMethods>,
};

#[derive(Debug, Default)]
struct PreferArrayLastMethods;

impl Rule for PreferArrayLastMethods {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node).filter(|m| m.plain()) else {
            return;
        };
        let replacement = match call.name {
            "find" => "findLast",
            "findIndex" => "findLastIndex",
            "indexOf" => "lastIndexOf",
            "reduce" => "reduceRight",
            _ => return,
        };
        let reversing = skip_parens(ast, call.obj);
        let Some(rev) = method_call(ast, reversing)
            .filter(|m| m.plain() && m.args.is_empty())
            .filter(|m| matches!(m.name, "reverse" | "toReversed"))
        else {
            return;
        };
        let (method, reversing_method) = (call.name, rev.name);
        let prop = cx.span(call.prop);
        let fix = (!would_remove_comments(cx, cx.span(reversing), &[paren_span(cx, rev.obj)]))
            .then(|| remove_method_call(cx, reversing, Fix::replace(prop, replacement)));
        let report = cx
            .report(
                prop,
                format!(
                    "Prefer `Array#{replacement}()` over `Array#{reversing_method}().{method}()`."
                ),
            )
            .id("prefer-array-last-methods");
        if let Some(fix) = fix {
            report.suggest(
                "replace",
                format!("Replace `.{reversing_method}().{method}()` with `.{replacement}()`."),
                fix,
            );
        }
    }
}
