//! Helpers for the round-5 eslint-plugin-unicorn rules: comment checks for fixes that
//! replace text, expression text that survives being moved into a call, and the tokens of a
//! call's argument list.

use super::prelude::*;
use super::unicorn_util::parenthesized_span;

/// True when replacing `range` would drop a comment: one lies wholly inside `range` but
/// outside every `kept` range.
pub(crate) fn would_remove_comments(cx: &Ctx<'_, '_>, range: Span, kept: &[Span]) -> bool {
    cx.ast().comments_in(range).iter().any(|c| {
        c.span.end <= range.end
            && !kept
                .iter()
                .any(|k| c.span.start >= k.start && c.span.end <= k.end)
    })
}

/// True when `id` is wrapped in at least one pair of parentheses.
pub(crate) fn is_parenthesized(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    cx.parent(id)
        .is_some_and(|p| matches!(cx.node(p), Node::Paren { .. }))
}

/// The text of `id` to be moved somewhere else: without redundant parentheses unless they
/// hold comments (then the parenthesized text as written), wrapped in parentheses when
/// `needs_parens`.
pub(crate) fn expression_text(cx: &Ctx<'_, '_>, id: NodeId, needs_parens: bool) -> String {
    if is_parenthesized(cx, id) {
        let outer = parenthesized_span(cx, id);
        if would_remove_comments(cx, outer, &[cx.span(id)]) {
            return outer.text(cx.src()).to_owned();
        }
    }
    let text = cx.text(id);
    if needs_parens {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

/// [`expression_text`] for a single call argument: a sequence gets parentheses.
pub(crate) fn call_argument_text(cx: &Ctx<'_, '_>, id: NodeId) -> String {
    let seq = matches!(cx.node(id), Node::Seq { .. });
    expression_text(cx, id, seq)
}

/// The `(` and `)` of a call or `new` expression's argument list, as spans of one byte.
pub(crate) fn call_parens(cx: &Ctx<'_, '_>, call: NodeId) -> Option<(Span, Span)> {
    let src = cx.src();
    let (callee, type_args) = match *cx.node(call) {
        Node::Call {
            callee, type_args, ..
        } => (callee, type_args),
        Node::New {
            callee,
            type_args,
            has_args: true,
            ..
        } => (callee, type_args),
        _ => return None,
    };
    let mut from = parenthesized_span(cx, callee).end;
    if let Some(t) = type_args {
        from = from.max(cx.span(t).end);
    }
    let mut open = skip_trivia_forward(src, from as usize);
    if src.as_bytes().get(open..open + 2) == Some(b"?.") {
        open = skip_trivia_forward(src, open + 2);
    }
    if src.as_bytes().get(open) != Some(&b'(') {
        return None;
    }
    let end = cx.span(call).end as usize;
    if end == 0 || src.as_bytes().get(end - 1) != Some(&b')') {
        return None;
    }
    Some((
        Span::new(open as u32, open as u32 + 1),
        Span::new(end as u32 - 1, end as u32),
    ))
}

/// The source between a call's parentheses (a trailing comma included).
pub(crate) fn call_arguments_text<'a>(cx: &Ctx<'_, 'a>, call: NodeId) -> Option<&'a str> {
    let (open, close) = call_parens(cx, call)?;
    cx.src().get(open.end as usize..close.start as usize)
}

/// The span of the operator `op` that follows `left` (with its parentheses).
pub(crate) fn operator_after(cx: &Ctx<'_, '_>, left: NodeId, op: &str) -> Option<Span> {
    let src = cx.src();
    let at = skip_trivia_forward(src, parenthesized_span(cx, left).end as usize);
    src[at..]
        .starts_with(op)
        .then(|| Span::new(at as u32, (at + op.len()) as u32))
}

/// A literal number with the given value (through parentheses).
pub(crate) fn is_number_literal(cx: &Ctx<'_, '_>, id: NodeId, value: f64) -> bool {
    matches!(*cx.node(skip_parens(cx.ast(), id)), Node::NumberLit { value: v } if v.get() == value)
}

/// `@eslint-community/eslint-utils`' `hasSideEffect` with default options: calls, `new`,
/// assignments, updates, `delete`, `await`, `yield`, `import()` and tagged templates have
/// side effects; reading members does not.
pub(crate) fn has_side_effect(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    fn walk(ast: &ts_r_ast::Ast, id: NodeId, depth: u32) -> bool {
        if depth > 200 {
            return true;
        }
        match ast.node(id) {
            Node::Call { .. }
            | Node::New { .. }
            | Node::Assign { .. }
            | Node::Update { .. }
            | Node::Await { .. }
            | Node::Yield { .. }
            | Node::ImportCall { .. }
            | Node::TaggedTemplate { .. }
            | Node::Unary {
                op: UnaryOp::Delete,
                ..
            } => true,
            Node::FunctionExpr { .. } | Node::Arrow { .. } | Node::ClassExpr { .. } => false,
            n => {
                let mut found = false;
                n.for_each_child(ast, &mut |c| {
                    if !found {
                        found = walk(ast, c, depth + 1);
                    }
                });
                found
            }
        }
    }
    walk(cx.ast(), id, 0)
}
