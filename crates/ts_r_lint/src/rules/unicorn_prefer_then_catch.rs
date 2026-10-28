//! `unicorn/prefer-then-catch`: `p.then(onFulfilled, onRejected)` does not catch a rejection
//! from `onFulfilled`; `p.then(onFulfilled).catch(onRejected)` does. Calls with a nullish
//! handler (`null`, `void x`, the global `undefined`) are fine. With type information the
//! receiver must be the lib's `Promise` (or `any`/`unknown`) and the result must have a
//! callable `catch`. The suggestion moves an identifier or function handler into `.catch()`
//! when no comment would be lost or left behind.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_text::{paren_span, would_remove_comments};
use super::unicorn_util::skip_wrappers;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-then-catch",
        "Prefer `.then().catch()` over `.then(…, …)` for error handling.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<PreferThenCatch>,
};

#[derive(Debug, Default)]
struct PreferThenCatch;

fn is_nullish(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_wrappers(ast, id);
    match *ast.node(id) {
        Node::NullLit {} => true,
        Node::Unary {
            op: UnaryOp::Void, ..
        } => true,
        Node::Ident { .. } => {
            ident_name(ast, id) == Some("undefined") && cx.semantic().refers_to_global(id)
        }
        _ => false,
    }
}

fn is_unknown_type(ti: &dyn TypeInfo, t: Ty) -> bool {
    t == Ty::UNRESOLVED || ti.flags(t).intersects(TypeFlags::ANY | TypeFlags::UNKNOWN)
}

fn has_callable_catch(ti: &dyn TypeInfo, t: Ty, depth: u32) -> bool {
    if depth > 8 {
        return false;
    }
    if ti.flags(t).contains(TypeFlags::UNION) {
        return ti
            .union_members(t)
            .into_iter()
            .all(|m| has_callable_catch(ti, m, depth + 1));
    }
    ti.property_of_type(t, "catch")
        .is_some_and(|s| !ti.signatures(ti.type_of_symbol(s), false).is_empty())
}

/// Without type information the result is assumed to have a callable `.catch()`.
fn can_then_result_catch(cx: &Ctx<'_, '_>, call: NodeId, receiver: NodeId) -> bool {
    let ti = cx.types();
    if !ti.is_available() {
        return true;
    }
    let Some(t) = ti.type_at(receiver) else {
        return true;
    };
    let t = ti.non_nullable(t);
    if is_unknown_type(ti, t) {
        return true;
    }
    let native = ti
        .type_symbol(t)
        .or_else(|| ti.alias_symbol(t))
        .is_some_and(|s| {
            ti.symbol_name(s) == "Promise" && ti.symbol_decls(s).iter().any(|d| d.in_lib)
        });
    native
        && ti
            .type_at(call)
            .is_some_and(|r| has_callable_catch(ti, r, 0))
}

fn skip_space(src: &str, mut pos: usize) -> usize {
    while let Some(c) = src[pos..].chars().next() {
        if !(c.is_whitespace() || c == '\u{feff}') {
            break;
        }
        pos += c.len_utf8();
    }
    pos
}

fn comment_at(src: &str, pos: usize) -> bool {
    src[pos..].starts_with("//") || src[pos..].starts_with("/*")
}

/// The suggestion's edits: the handler with the comma before it (and a trailing comma)
/// removed, and `.catch(handler)` after the call.
fn suggestion(cx: &Ctx<'_, '_>, call: NodeId, handler: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let src = cx.src();
    if !matches!(
        ast.node(skip_wrappers(ast, handler)),
        Node::Ident { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
    ) {
        return None;
    }
    let outer = paren_span(cx, handler);
    let before = skip_trivia_backward(ast, src, outer.start as usize);
    if before == 0 || src.as_bytes()[before - 1] != b',' {
        return None;
    }
    let start = before as u32 - 1;
    let after = skip_trivia_forward(src, outer.end as usize);
    let end = if src.as_bytes().get(after) == Some(&b',') {
        after as u32 + 1
    } else {
        outer.end
    };
    let removed = Span::new(start, end);
    if would_remove_comments(cx, removed, &[cx.span(skip_parens(ast, handler))]) {
        return None;
    }
    // A comment right after the handler (or after its trailing comma) would be left behind.
    let mut next = skip_space(src, outer.end as usize);
    if !comment_at(src, next) && src.as_bytes().get(next) == Some(&b',') {
        next = skip_space(src, next + 1);
    }
    if comment_at(src, next) {
        return None;
    }
    let text = outer.text(src);
    Some(Fix::replace(removed, "").and(
        Span::new(cx.span(call).end, cx.span(call).end),
        format!(".catch({text})"),
    ))
}

impl Rule for PreferThenCatch {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node).filter(|m| m.plain() && m.name == "then") else {
            return;
        };
        let &[fulfilled, rejected] = call.args else {
            return;
        };
        let spread = |id| matches!(ast.node(skip_parens(ast, id)), Node::Spread { .. });
        if spread(fulfilled) || spread(rejected) {
            return;
        }
        if is_nullish(cx, fulfilled)
            || is_nullish(cx, rejected)
            || !can_then_result_catch(cx, node, call.obj)
        {
            return;
        }
        let fix = suggestion(cx, node, rejected);
        let report = cx
            .report(
                cx.span(call.prop),
                "Prefer `.then(…).catch(…)` over passing a rejection handler to `.then()`.",
            )
            .id("prefer-then-catch");
        if let Some(fix) = fix {
            report.suggest(
                "prefer-then-catch/suggestion",
                "Move the rejection handler to `.catch()`.",
                fix,
            );
        }
    }
}
