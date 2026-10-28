//! `unicorn/no-unnecessary-splice`: `splice()` calls a simpler form does (see
//! `unicorn_splice`): no-ops, emptying (`.length = 0`, only when the result is unused),
//! `shift`, `unshift`, `pop` and `push`. Fixed only when the result is unused and no
//! comment would go; a no-op statement is removed when it sits in a block or the program
//! and nothing in it has side effects.

use super::prelude::*;
use super::unicorn_ast::{is_value_not_usable, method_call, remove_statements};
use super::unicorn_splice::{SpliceReplacement, unnecessary_splice_replacement};
use super::unicorn_static::has_side_effect;
use super::unicorn_text::{has_comment_in, member_object_text, paren_text};
use super::unicorn_types::should_skip_known_non_array_receiver;
use super::unicorn_util::{has_optional_chain, skip_wrappers};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unnecessary-splice",
        "Disallow `Array#splice()` when simpler alternatives exist.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoUnnecessarySplice>,
};

#[derive(Debug, Default)]
struct NoUnnecessarySplice;

/// An optional chain inside a computed key somewhere along the member/call chain.
fn optional_chain_in_computed_key(ast: &Ast, id: NodeId) -> bool {
    let mut id = id;
    for _ in 0..100_000 {
        id = skip_wrappers(ast, id);
        match *ast.node(id) {
            Node::Index { obj, index, .. } => {
                if has_optional_chain(ast, index) {
                    return true;
                }
                id = obj;
            }
            Node::Member { obj, .. } => id = obj,
            Node::Call { callee, .. } => id = callee,
            _ => return false,
        }
    }
    false
}

/// A comment starting at or after `end` on the line `end` is on.
fn comment_after_on_line(cx: &Ctx<'_, '_>, end: u32) -> bool {
    let src = cx.src();
    let line_end = src[end as usize..]
        .find(['\n', '\r', '\u{2028}', '\u{2029}'])
        .map_or(src.len(), |i| end as usize + i);
    cx.ast()
        .comments_in(Span::new(end, src.len() as u32))
        .iter()
        .any(|c| c.span.start >= end && (c.span.start as usize) < line_end)
}

fn fix(
    cx: &Ctx<'_, '_>,
    call: NodeId,
    object: NodeId,
    args: &[NodeId],
    replacement: SpliceReplacement,
) -> Option<Fix> {
    let ast = cx.ast();
    let span = cx.span(call);
    if !is_value_not_usable(cx, call) || has_comment_in(cx, span) {
        return None;
    }
    match replacement {
        SpliceReplacement::NoOp => {
            let stmt = cx.parent(call)?;
            let removable = matches!(*ast.node(stmt), Node::ExprStmt { .. })
                && cx.parent(stmt).is_some_and(|p| {
                    matches!(ast.node(p), Node::Program { .. } | Node::Block { .. })
                });
            if !removable
                || has_comment_in(cx, cx.span(stmt))
                || comment_after_on_line(cx, cx.span(stmt).end)
                || has_side_effect(ast, object)
                || args.iter().any(|&a| has_side_effect(ast, a))
            {
                return None;
            }
            Some(remove_statements(cx, stmt, stmt, false))
        }
        SpliceReplacement::Empty => Some(Fix::replace(
            span,
            format!("{}.length = 0", member_object_text(cx, object, call)),
        )),
        SpliceReplacement::Method(method) => {
            let kept = args.get(2..).unwrap_or_default();
            if method == "push" && kept.iter().any(|&a| has_side_effect(ast, a)) {
                return None;
            }
            let kept: Vec<&str> = kept.iter().map(|&a| paren_text(cx, a)).collect();
            Some(Fix::replace(
                span,
                format!(
                    "{}.{method}({})",
                    member_object_text(cx, object, call),
                    kept.join(", ")
                ),
            ))
        }
    }
}

impl Rule for NoUnnecessarySplice {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(m) = method_call(ast, node) else {
            return;
        };
        if m.name != "splice"
            || !m.plain()
            || has_optional_chain(ast, m.obj)
            || optional_chain_in_computed_key(ast, m.obj)
        {
            return;
        }
        let Some(replacement) = unnecessary_splice_replacement(cx, m.obj, m.args) else {
            return;
        };
        if should_skip_known_non_array_receiver(cx, m.obj)
            || (replacement == SpliceReplacement::Empty && !is_value_not_usable(cx, node))
        {
            return;
        }
        let message = match replacement {
            SpliceReplacement::NoOp => "This `splice()` call does not change the array.",
            SpliceReplacement::Empty => {
                "Prefer setting `.length = 0` over `.splice()` to empty an array."
            }
            SpliceReplacement::Method("shift") => "Prefer `.shift()` over `.splice()`.",
            SpliceReplacement::Method("unshift") => "Prefer `.unshift()` over `.splice()`.",
            SpliceReplacement::Method("pop") => "Prefer `.pop()` over `.splice()`.",
            SpliceReplacement::Method(_) => "Prefer `.push()` over `.splice()`.",
        };
        let fix = fix(cx, node, m.obj, m.args, replacement);
        cx.report_node(m.prop, message)
            .id(replacement.message_id())
            .fix_opt(fix);
    }
}
