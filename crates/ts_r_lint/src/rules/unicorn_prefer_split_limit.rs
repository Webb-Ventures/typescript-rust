//! `unicorn/prefer-split-limit`: `s.split(sep)` with a non-empty static string or a regex
//! literal separator, of which only the first `n` parts are read (`[i]`, `.at(i)` with a
//! static non-negative `i`, or an array destructuring of `n` elements without a rest), gets
//! the limit `n` appended as a second argument.

use super::prelude::*;
use super::unicorn_ast::{is_left_hand_side, is_value_not_usable, method_call};
use super::unicorn_static::{Static, static_value_no_side_effects};
use super::unicorn_text::static_string_value;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-split-limit",
        "Prefer `String#split()` with a limit.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<PreferSplitLimit>,
};

#[derive(Debug, Default)]
struct PreferSplitLimit;

/// `2 ** 32 - 1`, the largest limit `split` takes.
const MAX_LIMIT: f64 = 4_294_967_295.0;

impl Rule for PreferSplitLimit {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Index,
            NodeKind::Call,
            NodeKind::VarDeclarator,
            NodeKind::Assign,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *ast.node(node) {
            Node::Index { obj, index, .. } => {
                if is_left_hand_side(cx, node) {
                    return;
                }
                let Some(split) = split_without_limit(cx, obj) else {
                    return;
                };
                if let Some(i) = index_value(cx, index) {
                    report(cx, cx.span(node), split, i + 1);
                }
            }
            Node::Call { .. } => {
                let Some(m) = method_call(ast, node) else {
                    return;
                };
                let &[arg] = m.args else {
                    return;
                };
                if m.name != "at" || matches!(ast.node(arg), Node::Spread { .. }) {
                    return;
                }
                let Some(split) = split_without_limit(cx, m.obj) else {
                    return;
                };
                if let Some(i) = index_value(cx, arg) {
                    report(cx, cx.span(node), split, i + 1);
                }
            }
            Node::VarDeclarator {
                name,
                type_ann,
                init: Some(init),
                ..
            } => {
                // ESTree's pattern span takes in the type annotation.
                let mut span = cx.span(name);
                if let Some(t) = type_ann {
                    span.end = cx.span(t).end;
                }
                destructuring(cx, name, span, init);
            }
            Node::Assign {
                left,
                op: AssignOp::Assign,
                right,
            } => {
                if is_value_not_usable(cx, node) {
                    destructuring(cx, left, cx.span(left), right);
                }
            }
            _ => {}
        }
    }
}

/// `const [a, b] = s.split(sep)` (or the assignment form): the element count, unless the
/// pattern is empty or ends in a rest element.
fn destructuring(cx: &mut Ctx<'_, '_>, pattern: NodeId, span: Span, value: NodeId) {
    let ast = cx.ast();
    let (Node::ArrayPattern { elements } | Node::ArrayLit { elements }) = *ast.node(pattern) else {
        return;
    };
    let elements = ast.list(elements);
    match elements.last() {
        None => return,
        Some(&last) if matches!(ast.node(last), Node::Rest { .. } | Node::Spread { .. }) => return,
        _ => {}
    }
    if let Some(split) = split_without_limit(cx, value) {
        report(cx, span, split, elements.len() as u64);
    }
}

/// `x.split(sep)` (through parentheses) with exactly one argument, a non-empty static
/// string or a regex literal.
fn split_without_limit(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let call = skip_parens(ast, id);
    let m = method_call(ast, call)?;
    let &[sep] = m.args else {
        return None;
    };
    if m.name != "split" {
        return None;
    }
    let supported = match ast.node(sep) {
        Node::RegexLit { .. } => true,
        _ => static_string_value(ast, sep).is_some_and(|s| !s.is_empty()),
    };
    supported.then_some(call)
}

/// A static, side-effect free safe integer in `0..2**32 - 2`.
fn index_value(cx: &Ctx<'_, '_>, id: NodeId) -> Option<u64> {
    let Some(Static::Num(n)) = static_value_no_side_effects(cx, id) else {
        return None;
    };
    (n.fract() == 0.0 && (0.0..MAX_LIMIT - 1.0).contains(&n)).then_some(n as u64)
}

/// Appends the limit before the call's `)`, after a trailing comma when there is one.
fn report(cx: &mut Ctx<'_, '_>, span: Span, split: NodeId, limit: u64) {
    let ast = cx.ast();
    let src = cx.src();
    let close = cx.span(split).end.saturating_sub(1);
    let fix = match (*ast.node(split), src.as_bytes().get(close as usize)) {
        (Node::Call { args, .. }, Some(b')')) => ast.list(args).last().map(|&last| {
            let after = skip_trivia_forward(src, cx.span(last).end as usize);
            if src.as_bytes().get(after) == Some(&b',') {
                Fix::insert(close, format!(" {limit},"))
            } else {
                Fix::insert(close, format!(", {limit}"))
            }
        }),
        _ => None,
    };
    cx.report(span, "Prefer `String#split()` with a limit.")
        .id("prefer-split-limit")
        .fix_opt(fix);
}
