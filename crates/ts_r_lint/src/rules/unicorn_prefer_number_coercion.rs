//! `unicorn/prefer-number-coercion`: calls of the global `parseFloat` / `Number.parseFloat`
//! (with an argument) and of `parseInt` / `Number.parseInt` with a static radix of `10`
//! (also `radix = 10`), found as ESLint's reference tracker finds them (through the global
//! object and aliases), not through a computed `Number["parseInt"]`. Reported at the callee.
//! `parseFloat` suggests `Number` as the callee (not for optional calls, spread-only
//! arguments or a commented callee); `parseInt(x, 10)` suggests `Math.trunc(Number(x))`
//! when it has exactly those two arguments, `x` is not a spread, the radix has no side
//! effects, the call is not optional and only comments inside `x` would go.

use super::prelude::*;
use super::unicorn_static::{Static, has_side_effect, static_value_control_flow};
use super::unicorn_text::{call_argument_text, has_comment_in, paren_span, would_remove_comments};
use super::unicorn_util::{global_path, space_around_keyword, with_edits};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-number-coercion",
        "Prefer `Number()` over `parseFloat()` and base-10 `parseInt()`.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<PreferNumberCoercion>,
};

#[derive(Debug, Default)]
struct PreferNumberCoercion;

impl Rule for PreferNumberCoercion {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call {
            callee,
            optional,
            args,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let callee = skip_parens(ast, callee);
        if matches!(ast.node(callee), Node::Index { .. }) {
            return;
        }
        let Some(method) = global_path(cx, callee) else {
            return;
        };
        if !matches!(
            method.as_str(),
            "parseFloat" | "Number.parseFloat" | "parseInt" | "Number.parseInt"
        ) {
            return;
        }
        let optional = optional || matches!(*ast.node(callee), Node::Member { optional: true, .. });
        let args = ast.list(args);
        let is_spread = |a: NodeId| matches!(ast.node(a), Node::Spread { .. });
        let callee_span = cx.span(callee);
        if method.ends_with("parseFloat") {
            if args.is_empty() {
                return;
            }
            let fix = (!(optional
                || args.iter().all(|&a| is_spread(a))
                || has_comment_in(cx, callee_span)))
            .then(|| {
                with_edits(
                    Fix::replace(callee_span, "Number"),
                    space_around_keyword(cx, callee),
                )
            });
            let report = cx
                .report(callee_span, format!("Prefer `Number()` over `{method}()`."))
                .id("parse-float");
            if let Some(fix) = fix {
                report.suggest(
                    "suggestion-number",
                    "Use `Number()` when the whole value should be numeric.",
                    fix,
                );
            }
            return;
        }
        let Some(&radix) = args.get(1) else {
            return;
        };
        if is_spread(radix) {
            return;
        }
        let radix_value = match *ast.node(skip_parens(ast, radix)) {
            Node::Assign {
                op: AssignOp::Assign,
                right,
                ..
            } => static_value_control_flow(cx, right),
            _ => static_value_control_flow(cx, radix),
        };
        if !matches!(radix_value, Some(Static::Num(n)) if n == 10.0) {
            return;
        }
        let first = args[0];
        let blocked = args.len() != 2
            || is_spread(first)
            || has_side_effect(ast, radix)
            || optional
            || would_remove_comments(cx, cx.span(node), &[paren_span(cx, first)]);
        let fix = (!blocked).then(|| {
            let text = format!("Math.trunc(Number({}))", call_argument_text(cx, first));
            with_edits(
                Fix::replace(cx.span(node), text),
                space_around_keyword(cx, node),
            )
        });
        let report = cx
            .report(
                callee_span,
                format!("Prefer `Number()` with explicit integer conversion over `{method}()` with radix `10`."),
            )
            .id("parse-int");
        if let Some(fix) = fix {
            report.suggest(
                "suggestion-trunc",
                format!("Replace `{method}()` with `Math.trunc(Number())`."),
                fix,
            );
        }
    }
}
