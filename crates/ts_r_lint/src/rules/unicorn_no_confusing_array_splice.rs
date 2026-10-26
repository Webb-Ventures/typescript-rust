//! `unicorn/no-confusing-array-splice`: `x.splice(start, count, element)` /
//! `x.toSpliced(..)` (exactly three non-spread arguments, plain call, receiver not skipped
//! as a known non-array) that
//!
//! - replaces one element (`count` a static number that truncates to 1,
//!   `replace-one-element`), or
//! - inserts at `-1` (`start` truncates to -1 and `count` to 0, `insert-at-negative-one`).
//!
//! Reported at the method name. Suggestions: `toSpliced` becomes `x.with(start, element)`
//! unless `start` is a negative number or `x.length`; a standalone `splice` on a simple
//! receiver (`a`, `this`, `a.b.c`) becomes `x[start] = element` when `start` is a whole
//! non-negative number, an identifier other than `NaN`/`undefined`, a canonical index string
//! or any other non-literal (none when a comment outside the kept pieces would be lost); the
//! `-1` insertion on a simple receiver gets `Math.max(x.length - 1, 0)` as its start when
//! `start` holds no comment and `element` has no side effect.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_splice::static_number;
use super::unicorn_static::has_side_effect;
use super::unicorn_text::{
    has_comment_in, member_object_text, paren_span, paren_text, static_string_value,
    would_remove_comments,
};
use super::unicorn_types::should_skip_known_non_array_receiver;
use super::unicorn_util::space_around_keyword;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-confusing-array-splice",
        "Disallow confusing uses of `Array#{splice,toSpliced}()`.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<NoConfusingArraySplice>,
};

#[derive(Debug, Default)]
struct NoConfusingArraySplice;

const REPLACE: &str = "replace-one-element";
const INSERT: &str = "insert-at-negative-one";

fn unwrap_ts(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => id = expr,
            _ => break,
        }
    }
    id
}

fn is_literal(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
            | Node::BigIntLit { .. }
    )
}

/// A bracket assignment writes the element `splice` would replace.
fn is_replaceable_index(ast: &Ast, id: NodeId) -> bool {
    let id = unwrap_ts(ast, id);
    if let Some(v) = static_number(ast, id) {
        return v.fract() == 0.0 && v.abs() <= 9_007_199_254_740_991.0 && v >= 0.0;
    }
    if matches!(ast.node(id), Node::Ident { .. }) {
        return !matches!(ident_name(ast, id), Some("NaN" | "undefined"));
    }
    if let Some(s) = static_string_value(ast, id) {
        let b = s.as_bytes();
        return !b.is_empty() && b.iter().all(u8::is_ascii_digit) && (b[0] != b'0' || b.len() == 1);
    }
    !is_literal(ast, id)
}

fn is_simple_receiver(ast: &Ast, id: NodeId, depth: u32) -> bool {
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::Ident { .. } | Node::This {} => true,
        Node::Member {
            obj,
            optional: false,
            prop,
        } => {
            depth < 1000
                && matches!(
                    ast.node(prop),
                    Node::Ident { .. } | Node::PrivateName { .. }
                )
                && is_simple_receiver(ast, obj, depth + 1)
        }
        _ => false,
    }
}

fn is_length_of(cx: &Ctx<'_, '_>, id: NodeId, object: NodeId) -> bool {
    let ast = cx.ast();
    matches!(*ast.node(unwrap_ts(ast, id)), Node::Member { obj, optional: false, prop }
        if ident_name(ast, prop) == Some("length") && is_same_reference(cx, obj, object))
}

fn message_id(ast: &Ast, start: NodeId, count: NodeId) -> Option<&'static str> {
    let count = static_number(ast, count).map(|v| v.trunc().max(0.0));
    if count == Some(1.0) {
        return Some(REPLACE);
    }
    (static_number(ast, start).map(f64::trunc) == Some(-1.0) && count == Some(0.0))
        .then_some(INSERT)
}

impl Rule for NoConfusingArraySplice {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, id) else {
            return;
        };
        if !matches!(call.name, "splice" | "toSpliced")
            || !call.plain()
            || call.args.len() != 3
            || call
                .args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return;
        }
        let [start, count, element] = [call.args[0], call.args[1], call.args[2]];
        let Some(message_id) = message_id(ast, start, count) else {
            return;
        };
        if should_skip_known_non_array_receiver(cx, call.obj) {
            return;
        }
        let object = skip_parens(ast, call.obj);
        let object_text = || member_object_text(cx, call.obj, id);
        let span = cx.span(id);
        let with_keyword_space = |fix: Fix| {
            space_around_keyword(cx, id)
                .into_iter()
                .fold(fix, |f, (s, t)| f.and(s, t))
        };
        let suggestion = if message_id == REPLACE {
            let kept = [call.obj, start, element].map(|n| paren_span(cx, n));
            if would_remove_comments(cx, span, &kept) {
                None
            } else if call.name == "toSpliced" {
                let negative = static_number(ast, start).is_some_and(|v| v.trunc() < 0.0);
                (!negative && !is_length_of(cx, start, object)).then(|| {
                    with_keyword_space(Fix::replace(
                        span,
                        format!(
                            "{}.with({}, {})",
                            object_text(),
                            paren_text(cx, start),
                            paren_text(cx, element)
                        ),
                    ))
                })
            } else {
                let standalone = parent_skip_parens(ast, cx.file.parents, id)
                    .is_some_and(|p| matches!(ast.node(p), Node::ExprStmt { .. }));
                (standalone
                    && is_simple_receiver(ast, object, 0)
                    && is_replaceable_index(ast, start))
                .then(|| {
                    with_keyword_space(Fix::replace(
                        span,
                        format!(
                            "{}[{}] = {}",
                            object_text(),
                            paren_text(cx, start),
                            paren_text(cx, element)
                        ),
                    ))
                })
            }
        } else {
            let start_span = cx.span(skip_parens(ast, start));
            (is_simple_receiver(ast, object, 0)
                && !has_comment_in(cx, start_span)
                && !has_side_effect(ast, element))
            .then(|| {
                Fix::replace(
                    start_span,
                    format!("Math.max({}.length - 1, 0)", object_text()),
                )
            })
        };
        let message = if message_id == REPLACE {
            format!(
                "Prefer a direct element replacement instead of `{}()`.",
                call.name
            )
        } else {
            format!("Avoid using `{}()` to insert at `-1`.", call.name)
        };
        let r = cx.report(cx.span(call.prop), message).id(message_id);
        if let Some(f) = suggestion {
            if message_id == REPLACE {
                r.suggest(
                    "suggestion-replace-one-element",
                    "Use direct element replacement.",
                    f,
                );
            } else {
                r.suggest(
                    "suggestion-insert-at-negative-one",
                    "Resolve the insertion index explicitly.",
                    f,
                );
            }
        }
    }
}
