//! `unicorn/no-new-buffer`: `Buffer.from()` / `Buffer.alloc()` instead of the deprecated
//! `new Buffer()`.
//!
//! The callee must be the identifier `Buffer` (through parentheses). The method is guessed
//! from the first argument: none means `from`; a spread means unknown; a number (the
//! unicorn `is_number` guess) means `alloc`; an array literal or template means `from`; a
//! statically known string second argument (an encoding) means `from`; a first argument
//! whose static value is a string or an array means `from`; anything else is unknown.
//!
//! A known method is fixed, except `alloc` with more arguments than the size: dropping them
//! is only a suggestion, and not even that when a comment sits between the comma after the
//! size and the end of the `new`, or a dropped argument has a side effect. An unknown method
//! gets `from` and (with the same condition) `alloc` suggestions. The fix appends `.method`
//! to the callee, removes the dropped arguments and turns the `new` into a call.

use super::prelude::*;
use super::unicorn_ast::remove_argument;
use super::unicorn_static::{Static, has_side_effect, static_value_control_flow};
use super::unicorn_text::has_comment_in;
use super::unicorn_types::is_number;
use super::unicorn_util::new_to_call;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-new-buffer",
        "Enforce the use of `Buffer.from()` and `Buffer.alloc()` instead of the deprecated `new Buffer()`.",
        NO_PRESET,
        Category::Suspicious,
    )
    .fixable()
    .suggestions(),
    create: create_default::<NoNewBuffer>,
};

#[derive(Debug, Default)]
struct NoNewBuffer;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Method {
    From,
    Alloc,
}

impl Method {
    fn name(self) -> &'static str {
        match self {
            Method::From => "from",
            Method::Alloc => "alloc",
        }
    }
}

impl Rule for NoNewBuffer {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::New { callee, args, .. } = *cx.node(node) else {
            return;
        };
        if ident_name(ast, skip_parens(ast, callee)) != Some("Buffer") {
            return;
        }
        let args = ast.list(args);
        let span = cx.span(node);
        match infer_method(cx, args) {
            None => {
                let mut sugg = vec![(Method::From, fix(cx, node, Method::From))];
                if can_remove_ignored(cx, node) {
                    sugg.push((Method::Alloc, fix(cx, node, Method::Alloc)));
                }
                let mut r = cx
                    .report(
                        span,
                        "`new Buffer()` is deprecated, use `Buffer.alloc()` or `Buffer.from()` instead.",
                    )
                    .id("error-unknown");
                for (m, f) in sugg {
                    r = r.suggest(
                        "suggestion",
                        format!("Switch to `Buffer.{}()`.", m.name()),
                        f,
                    );
                }
            }
            Some(m) => {
                let message = format!(
                    "`new Buffer()` is deprecated, use `Buffer.{}()` instead.",
                    m.name()
                );
                if m != Method::Alloc || args.len() == 1 {
                    let f = fix(cx, node, m);
                    cx.report(span, message).id("error").fix(f);
                } else if can_remove_ignored(cx, node) {
                    let f = fix(cx, node, m);
                    cx.report(span, message).id("error").suggest(
                        "suggestion",
                        format!("Switch to `Buffer.{}()`.", m.name()),
                        f,
                    );
                } else {
                    cx.report(span, message).id("error");
                }
            }
        }
    }
}

fn infer_method(cx: &Ctx<'_, '_>, args: &[NodeId]) -> Option<Method> {
    let ast = cx.ast();
    let Some(&first) = args.first() else {
        return Some(Method::From);
    };
    if matches!(ast.node(first), Node::Spread { .. }) {
        return None;
    }
    if is_number(cx, first) {
        return Some(Method::Alloc);
    }
    if matches!(
        ast.node(skip_parens(ast, first)),
        Node::ArrayLit { .. } | Node::TemplateLit { .. }
    ) {
        return Some(Method::From);
    }
    if let Some(&second) = args.get(1)
        && matches!(static_value_control_flow(cx, second), Some(Static::Str(_)))
    {
        return Some(Method::From);
    }
    match static_value_control_flow(cx, first) {
        Some(Static::Str(_) | Static::Array(_)) => Some(Method::From),
        _ => None,
    }
}

/// The arguments after the size can be dropped: no comment from the comma after the size to
/// the end of the `new`, and no side effect in them.
fn can_remove_ignored(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Node::New { args, .. } = *ast.node(node) else {
        return false;
    };
    let args = ast.list(args);
    let [size, ignored @ ..] = args else {
        return true;
    };
    if ignored.is_empty() {
        return true;
    }
    let src = cx.src();
    let size_end = super::unicorn_util::parenthesized_span(cx, *size).end;
    let comma = skip_trivia_forward(src, size_end as usize) as u32;
    let end = cx.span(node).end;
    !has_comment_in(cx, Span::new(comma, end)) && ignored.iter().all(|&a| !has_side_effect(ast, a))
}

fn fix(cx: &Ctx<'_, '_>, node: NodeId, m: Method) -> Fix {
    let ast = cx.ast();
    let Node::New { callee, args, .. } = *ast.node(node) else {
        return Fix::default();
    };
    let n = ast.list(args).len();
    let mut out = Fix::insert(
        cx.span(skip_parens(ast, callee)).end,
        format!(".{}", m.name()),
    );
    if m == Method::Alloc {
        for i in (1..n).rev() {
            if let Some(f) = remove_argument(cx, node, i) {
                for e in f.edits {
                    out = out.and(e.span, e.text);
                }
            }
        }
    }
    for e in new_to_call(cx, node).edits {
        out = out.and(e.span, e.text);
    }
    out
}
