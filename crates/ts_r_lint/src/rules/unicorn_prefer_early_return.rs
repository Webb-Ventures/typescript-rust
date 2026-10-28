//! `unicorn/prefer-early-return`: a function body that ends in an `if` (no `else`)
//! wrapping more than `maximumStatements` statements (default 1) should return early.
//!
//! The rewrite is the one of `prefer-continue` with `return`: a fix when it is safe and no
//! comment follows the `if`, the same edit as a suggestion when only that comment is in
//! the way, and for a relational test a suggestion flipping the operator instead. A
//! `let`/`const` moved out of the `if` must not clash with a parameter (or `arguments`, a
//! `var` of the function). `checkShortBodies` reports the reverse, as in `prefer-continue`.

use super::prelude::*;
use super::unicorn_conditions::{
    can_rewrite_to_early_exit, comment_after, early_exit_text, negated_if_condition,
    opposite_comparison_condition, short_body_problem,
};
use super::unicorn_prefer_continue::consequent_count;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-early-return",
        "Prefer early returns over conditionals wrapping the remainder of the function body.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<PreferEarlyReturn>,
};

#[derive(Debug)]
struct PreferEarlyReturn {
    maximum: usize,
    short_bodies: bool,
}

impl FromOptions for PreferEarlyReturn {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let obj = obj_opt(opts, 0);
        let maximum = obj
            .get("maximumStatements")
            .and_then(Value::as_u64)
            .unwrap_or(1);
        Ok(PreferEarlyReturn {
            maximum: usize::try_from(maximum).unwrap_or(usize::MAX),
            short_bodies: bool_field(obj, "checkShortBodies", false),
        })
    }
}

/// A name the function's own scope declares: a parameter, a `var`, a hoisted function, or
/// `arguments` for a non-arrow function.
fn function_scope_has(cx: &Ctx<'_, '_>, function: NodeId, body: NodeId, names: &[&str]) -> bool {
    let sem = cx.semantic();
    if !matches!(cx.node(function), Node::Arrow { .. }) && names.contains(&"arguments") {
        return true;
    }
    [function, body]
        .iter()
        .filter_map(|&n| sem.scope_of_node(n))
        .any(|scope| {
            sem.scope(scope).symbols.iter().any(|&s| {
                names.contains(&sem.name(s))
                    && !sem
                        .symbol(s)
                        .decls
                        .iter()
                        .all(|d| d.kind == DeclKind::FunctionExprName)
            })
        })
}

impl Rule for PreferEarlyReturn {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
            NodeKind::Arrow,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let body = match *cx.node(node) {
            Node::FunctionDecl { body: Some(b), .. }
            | Node::FunctionExpr { body: Some(b), .. }
            | Node::Arrow { body: b, .. } => b,
            _ => return,
        };
        let Node::Block { stmts } = *ast.node(body) else {
            return;
        };
        let list = ast.list(stmts);
        let wrapper = list.last().copied().filter(|&s| {
            matches!(*ast.node(s), Node::If { cons, alt: None, .. }
                if consequent_count(ast, cons) > self.maximum)
        });
        let Some(stmt) = wrapper else {
            if !self.short_bodies {
                return;
            }
            if let Some((guard, fix)) = short_body_problem(cx, body, self.maximum, false) {
                cx.report(
                    cx.span(guard),
                    "Prefer conditional wrapping over an early return for a short body.",
                )
                .id("prefer-early-return/short-body")
                .fix_opt(fix);
            }
            return;
        };
        let span = cx.span(stmt);
        let can_rewrite = can_rewrite_to_early_exit(cx, stmt, list.len(), &|names| {
            function_scope_has(cx, node, body, names)
        });
        let rewrite = can_rewrite
            .then(|| negated_if_condition(cx, stmt))
            .flatten()
            .map(|c| Fix::replace(span, early_exit_text(cx, stmt, "return", &c)));
        let (fix, suggestion) = if comment_after(cx, span.end) {
            (None, rewrite)
        } else {
            (rewrite, None)
        };
        let opposite = fix
            .as_ref()
            .and_then(|_| opposite_comparison_condition(cx, stmt))
            .map(|c| Fix::replace(span, early_exit_text(cx, stmt, "return", &c)));
        let report = cx
            .report(
                span,
                "Prefer an early return over wrapping the remainder of the function body in an `if` statement.",
            )
            .id("prefer-early-return")
            .fix_opt(fix);
        if let Some(s) = opposite {
            report.suggest(
                "prefer-early-return/opposite-comparison",
                "Rewrite to an early return using the opposite comparison (changes behavior for NaN and values coerced to NaN).",
                s,
            );
        } else if let Some(s) = suggestion {
            report.suggest(
                "prefer-early-return/suggestion",
                "Rewrite to an early return.",
                s,
            );
        }
    }
}
