//! `unicorn/prefer-continue`: a loop body that ends in an `if` (no `else`) wrapping more than
//! `maximumStatements` statements (default 1) should exit early with `continue` instead.
//!
//! An `if` whose body already ends in `return`/`break`/`continue`/`throw` is left alone.
//! The fix negates the condition into `if (..) { continue; }`, then unwraps the body after
//! a blank line; it needs the conditions of [`can_rewrite_to_early_exit`] and no comment
//! right after the `if`. A relational test also gets a suggestion flipping the operator
//! instead (different for `NaN`).
//!
//! With `checkShortBodies`, when no wrapper is reported, the opposite applies: the last
//! `if (..) continue;` guard followed by 1 to `maximumStatements` statements (or with an
//! `else` of that size) should wrap them instead.

use super::prelude::*;
use super::unicorn_conditions::{
    can_rewrite_to_early_exit, comment_after, early_exit_text, negated_if_condition,
    opposite_comparison_condition, short_body_problem,
};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-continue",
        "Prefer early continues over conditionals wrapping the remainder of the loop body.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<PreferContinue>,
};

#[derive(Debug)]
struct PreferContinue {
    maximum: usize,
    short_bodies: bool,
}

impl FromOptions for PreferContinue {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let obj = obj_opt(opts, 0);
        let maximum = obj
            .get("maximumStatements")
            .and_then(Value::as_u64)
            .unwrap_or(1);
        Ok(PreferContinue {
            maximum: usize::try_from(maximum).unwrap_or(usize::MAX),
            short_bodies: bool_field(obj, "checkShortBodies", false),
        })
    }
}

/// The number of non-empty statements in an `if` consequent.
pub(crate) fn consequent_count(ast: &Ast, cons: NodeId) -> usize {
    match *ast.node(cons) {
        Node::Empty {} => 0,
        Node::Block { stmts } => ast
            .list(stmts)
            .iter()
            .filter(|&&s| !matches!(ast.node(s), Node::Empty {}))
            .count(),
        _ => 1,
    }
}

fn exits_loop(ast: &Ast, cons: NodeId) -> bool {
    let last = match *ast.node(cons) {
        Node::Block { stmts } => ast
            .list(stmts)
            .iter()
            .rev()
            .find(|&&s| !matches!(ast.node(s), Node::Empty {}))
            .copied(),
        _ => Some(cons),
    };
    last.is_some_and(|s| {
        matches!(
            ast.node(s),
            Node::Return { .. } | Node::Break { .. } | Node::Continue { .. } | Node::Throw { .. }
        )
    })
}

impl Rule for PreferContinue {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::For,
            NodeKind::ForIn,
            NodeKind::ForOf,
            NodeKind::While,
            NodeKind::DoWhile,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let body = match *cx.node(node) {
            Node::For { body, .. }
            | Node::ForIn { body, .. }
            | Node::ForOf { body, .. }
            | Node::While { body, .. }
            | Node::DoWhile { body, .. } => body,
            _ => return,
        };
        let Node::Block { stmts } = *ast.node(body) else {
            return;
        };
        let list = ast.list(stmts);
        let wrapper = list.last().copied().filter(|&s| {
            matches!(*ast.node(s), Node::If { cons, alt: None, .. }
                if consequent_count(ast, cons) > self.maximum && !exits_loop(ast, cons))
        });
        let Some(stmt) = wrapper else {
            if !self.short_bodies {
                return;
            }
            if let Some((guard, fix)) = short_body_problem(cx, body, self.maximum, true) {
                cx.report(
                    cx.span(guard),
                    "Prefer conditional wrapping over an early continue for a short body.",
                )
                .id("prefer-continue/short-body")
                .fix_opt(fix);
            }
            return;
        };
        let span = cx.span(stmt);
        let fixable = can_rewrite_to_early_exit(cx, stmt, list.len(), &|_| false)
            && !comment_after(cx, span.end);
        let fix = fixable
            .then(|| negated_if_condition(cx, stmt))
            .flatten()
            .map(|c| Fix::replace(span, early_exit_text(cx, stmt, "continue", &c)));
        let suggestion = fix
            .as_ref()
            .and_then(|_| opposite_comparison_condition(cx, stmt))
            .map(|c| Fix::replace(span, early_exit_text(cx, stmt, "continue", &c)));
        let report = cx
            .report(
                span,
                "Prefer an early continue over wrapping the remainder of the loop body in an `if` statement.",
            )
            .id("prefer-continue")
            .fix_opt(fix);
        if let Some(s) = suggestion {
            report.suggest(
                "prefer-continue/opposite-comparison",
                "Rewrite to an early continue using the opposite comparison (changes behavior for NaN and values coerced to NaN).",
                s,
            );
        }
    }
}
