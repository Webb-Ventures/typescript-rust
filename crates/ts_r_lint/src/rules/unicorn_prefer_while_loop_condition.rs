//! `unicorn/prefer-while-loop-condition`: an infinite loop (`while (true)`, `for (;;)`,
//! `for (; true;)`, `do {} while (true)`, not a labeled statement's body) whose block starts
//! with `if (cond) break;` (no `else`, the consequent only an unlabeled `break`) and has no
//! other `break` for the same loop becomes `while (!cond)`. Not when the condition reads a
//! binding the loop body declares with anything but `var`. The `if` is reported; the fix
//! (withheld when a comment outside the kept rest of the body would be lost) rewrites the
//! loop head and removes the `if`.

use super::prelude::*;
use super::unicorn_ast::remove_statements;
use super::unicorn_conditions::{
    has_unlabeled_break, idents_in, join_fixes, negated_expression_text,
};
use super::unicorn_text::would_remove_comments;
use super::unicorn_util_h4::trailing_comment_same_line;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-while-loop-condition",
        "Prefer putting the condition in the while statement.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferWhileLoopCondition>,
};

#[derive(Debug, Default)]
struct PreferWhileLoopCondition;

fn is_true(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    matches!(
        cx.node(skip_parens(cx.ast(), id)),
        Node::BoolLit { value: true }
    )
}

fn only_break(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    match *cx.node(id) {
        Node::Break { label: None } => true,
        Node::Block { stmts } => matches!(
            cx.ast().list(stmts),
            [only] if matches!(cx.node(*only), Node::Break { label: None })
        ),
        _ => false,
    }
}

/// The condition reads a binding the loop body declares lexically (`let`, `const`, a
/// function or class, ...), which would move out of its scope.
fn unsafe_lift(cx: &Ctx<'_, '_>, test: NodeId, body: Span) -> bool {
    let sem = cx.semantic();
    idents_in(cx.ast(), test).into_iter().any(|id| {
        let Some(sym) = sem.reference_at(id).and_then(|r| sem.reference(r).symbol()) else {
            return false;
        };
        sem.symbol(sym).decls.iter().any(|d| {
            let s = cx.span(d.name);
            d.kind != DeclKind::Var && s.start >= body.start && s.end <= body.end
        })
    })
}

impl Rule for PreferWhileLoopCondition {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::While, NodeKind::For, NodeKind::DoWhile]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (body, infinite) = match *cx.node(node) {
            Node::While { test, body } | Node::DoWhile { body, test } => (body, is_true(cx, test)),
            Node::For {
                init: None,
                test,
                update: None,
                body,
            } => (body, test.is_none_or(|t| is_true(cx, t))),
            _ => return,
        };
        if !infinite {
            return;
        }
        let Node::Block { stmts } = *cx.node(body) else {
            return;
        };
        if cx
            .parent(node)
            .is_some_and(|p| matches!(cx.node(p), Node::Labeled { .. }))
        {
            return;
        }
        let stmts = ast.list(stmts);
        let Some(&first) = stmts.first() else {
            return;
        };
        let Node::If {
            test,
            cons,
            alt: None,
        } = *cx.node(first)
        else {
            return;
        };
        if !only_break(cx, cons)
            || stmts[1..].iter().any(|&s| has_unlabeled_break(ast, s))
            || unsafe_lift(cx, test, cx.span(body))
        {
            return;
        }
        let loop_span = cx.span(node);
        let first_span = cx.span(first);
        let first_end =
            trailing_comment_same_line(cx, first_span.end).map_or(first_span.end, |c| c.end);
        let mut checked = loop_span;
        if matches!(cx.node(node), Node::DoWhile { .. })
            && let Some(c) = trailing_comment_same_line(cx, loop_span.end)
        {
            checked.end = c.end;
        }
        let kept = Span::new(first_end, cx.span(body).end);
        let msg = "Prefer putting the condition in the `while` statement.";
        let id = "prefer-while-loop-condition";
        if would_remove_comments(cx, checked, &[kept]) {
            cx.report(first_span, msg).id(id);
            return;
        }
        let t = skip_parens(ast, test);
        let condition = match *cx.node(t) {
            Node::Unary {
                op: UnaryOp::Not,
                arg,
            } => cx.text(skip_parens(ast, arg)).to_owned(),
            _ => negated_expression_text(cx, t),
        };
        let head = match *cx.node(node) {
            Node::While { test, .. } => Fix::replace(cx.span(test), condition),
            Node::For { .. } => Fix::replace(
                Span::new(loop_span.start, cx.span(body).start),
                format!("while ({condition}) "),
            ),
            _ => Fix::replace(
                Span::new(loop_span.start, loop_span.start + 2),
                format!("while ({condition})"),
            )
            .and(Span::new(cx.span(body).end, loop_span.end), ""),
        };
        let fix = join_fixes(head, remove_statements(cx, first, first, false));
        cx.report(first_span, msg).id(id).fix(fix);
    }
}
