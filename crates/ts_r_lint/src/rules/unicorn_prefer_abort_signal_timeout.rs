//! `unicorn/prefer-abort-signal-timeout`: `AbortSignal.timeout(delay)` over a
//! `const controller = new AbortController()` followed directly by
//! `setTimeout(() => controller.abort(), delay);`.
//!
//! The declaration and controller follow [`super::unicorn_abort`]; the next statement is an
//! expression statement calling the global `setTimeout` (not optional) with exactly the
//! callback and the delay. The controller's other uses must all be plain `.signal` reads,
//! none inside the `setTimeout` statement. The delay is not a sequence; when its value is
//! static (and has no side effects) it is a safe integer in `0..=2^31-1`; a value that is
//! only static through a property that may have changed is skipped. Reported on the
//! controller's name (with its type annotation), with a suggestion from
//! [`super::unicorn_abort::report_abort_controller`].

use super::prelude::*;
use super::unicorn_abort::{
    abort_controller_declarator, abort_reference, next_statement, report_abort_controller,
    signal_members,
};
use super::unicorn_static::{
    Static, mutable_member_access, static_value, static_value_no_side_effects,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-abort-signal-timeout",
        "Prefer `AbortSignal.timeout()` over manually aborting an `AbortController` with `setTimeout()`.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<PreferAbortSignalTimeout>,
};

#[derive(Debug, Default)]
struct PreferAbortSignalTimeout;

const MAX_DELAY: f64 = 2_147_483_647.0;

fn is_valid_delay(cx: &Ctx<'_, '_>, delay: NodeId) -> bool {
    match static_value_no_side_effects(cx, delay) {
        Some(Static::Num(n)) => n.fract() == 0.0 && (0.0..=MAX_DELAY).contains(&n),
        Some(_) => false,
        None => {
            !(mutable_member_access(cx, delay, &mut Vec::new())
                && static_value(cx, delay).is_some())
        }
    }
}

impl Rule for PreferAbortSignalTimeout {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDeclarator]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some((decl, name, _)) = abort_controller_declarator(cx, node) else {
            return;
        };
        let Some(stmt) = next_statement(cx, decl) else {
            return;
        };
        let Node::ExprStmt { expr } = *ast.node(stmt) else {
            return;
        };
        let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *ast.node(expr)
        else {
            return;
        };
        let &[callback, delay] = ast.list(args) else {
            return;
        };
        if !matches!(ast.node(callee), Node::Ident { .. })
            || !cx.is_global_ref(callee, "setTimeout")
            || [callback, delay]
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return;
        }
        let controller = ident_name(ast, name).unwrap_or("");
        let Some(abort) = abort_reference(cx, callback, controller) else {
            return;
        };
        let Some(members) = signal_members(cx, name, &[abort]) else {
            return;
        };
        let delay = skip_parens(ast, delay);
        let stmt_span = cx.span(stmt);
        if matches!(ast.node(delay), Node::Seq { .. })
            || !is_valid_delay(cx, delay)
            || members.iter().any(|&m| {
                let s = cx.span(m);
                s.start >= stmt_span.start && s.end <= stmt_span.end
            })
        {
            return;
        }
        let replacement = format!("AbortSignal.timeout({})", cx.text(delay));
        report_abort_controller(
            cx,
            node,
            stmt,
            stmt,
            replacement,
            &members,
            "prefer-abort-signal-timeout",
            "Prefer `AbortSignal.timeout()` over manually aborting an `AbortController` with `setTimeout()`.",
            "prefer-abort-signal-timeout/suggestion",
            "Replace with `AbortSignal.timeout()`.",
        );
    }
}
