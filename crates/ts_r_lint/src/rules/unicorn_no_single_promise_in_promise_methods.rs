//! `unicorn/no-single-promise-in-promise-methods`: `Promise.all/any/race([x])` with one
//! non-spread element. The array is reported.
//!
//! - Awaited `race`, and awaited `all` as a statement, are fixed to `await x` (unless a
//!   comment outside `x` would be lost, or the call is written to).
//! - Other awaited `all`s are fixed only as `const [v] = await Promise.all([x])` (a single
//!   plain name, no annotation; also a `[v] = ...` statement) -> `const v = await x`, or
//!   `(await Promise.all([x]))[0]` as an initializer or assigned value -> `await x`.
//! - `any` (it wraps rejections in `AggregateError`) and non-awaited `race`/`any` get
//!   suggestions: use the value directly (parenthesized unless a name or member access),
//!   and switch to `Promise.resolve(x)`.

use super::prelude::*;
use super::unicorn_ast::{is_left_hand_side, promise_method_call};
use super::unicorn_text::{
    is_parenthesized, needs_parens_at_expression_start, needs_semicolon, paren_span, paren_text,
    would_remove_comments,
};
use super::unicorn_util::{space_around_keyword, with_edits};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-single-promise-in-promise-methods",
        "Disallow passing single-element arrays to `Promise` methods.",
        NO_PRESET,
        Category::Suspicious,
    )
    .fixable()
    .suggestions(),
    create: create_default::<NoSinglePromiseInPromiseMethods>,
};

#[derive(Debug, Default)]
struct NoSinglePromiseInPromiseMethods;

/// The ESTree parent (parentheses skipped).
fn es_parent(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    parent_skip_parens(cx.ast(), cx.file.parents, id)
}

/// The element's text as an `await` operand.
fn awaited_text(cx: &Ctx<'_, '_>, elem: NodeId) -> String {
    let text = paren_text(cx, elem);
    if !is_parenthesized(cx, elem) && precedence(cx.ast(), elem) < 16 {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

fn unwrap_awaited(cx: &Ctx<'_, '_>, call: NodeId, elem: NodeId) -> Fix {
    with_edits(
        Fix::replace(cx.span(call), awaited_text(cx, elem)),
        space_around_keyword(cx, call),
    )
}

fn unwrap_non_awaited(cx: &Ctx<'_, '_>, call: NodeId, elem: NodeId) -> Fix {
    let mut text = paren_text(cx, elem).to_owned();
    if !is_parenthesized(cx, elem)
        && (needs_parens_at_expression_start(cx, elem, call)
            || !matches!(
                cx.node(elem),
                Node::Ident { .. } | Node::Member { .. } | Node::Index { .. }
            ))
    {
        text = format!("({text})");
    }
    if needs_semicolon(cx, cx.span(call).start, &text) {
        text = format!(";{text}");
    }
    with_edits(
        Fix::replace(cx.span(call), text),
        space_around_keyword(cx, call),
    )
}

fn switch_to_resolve(cx: &Ctx<'_, '_>, prop: NodeId, array: NodeId) -> Fix {
    let ast = cx.ast();
    let src = cx.src();
    let a = cx.span(array);
    let mut fix = Fix::replace(cx.span(prop), "resolve")
        .and(Span::new(a.start, a.start + 1), "")
        .and(Span::new(a.end - 1, a.end), "");
    let before = skip_trivia_backward(ast, src, (a.end - 1) as usize) as u32;
    if before > 0 && src.as_bytes()[before as usize - 1] == b',' {
        fix = fix.and(Span::new(before - 1, before), "");
    }
    fix
}

/// `[v]` with one plain name and no type annotation.
fn single_name_pattern(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    // An assignment target is still an array literal here.
    let (Node::ArrayPattern { elements } | Node::ArrayLit { elements }) = *cx.node(id) else {
        return None;
    };
    match cx.ast().list(elements) {
        [only] if matches!(cx.node(*only), Node::Ident { .. }) => Some(*only),
        _ => None,
    }
}

fn fix_all_first_element(cx: &Ctx<'_, '_>, call: NodeId, elem: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let promise = paren_span(cx, elem);
    if would_remove_comments(cx, cx.span(call), &[promise]) {
        return None;
    }
    let awaited = es_parent(cx, call)?;
    if !matches!(cx.node(awaited), Node::Await { .. }) {
        return None;
    }
    let parent = es_parent(cx, awaited)?;
    let pattern = match *cx.node(parent) {
        Node::VarDeclarator {
            name,
            type_ann: None,
            init: Some(init),
            ..
        } if skip_parens(ast, init) == awaited => single_name_pattern(cx, name).map(|e| (name, e)),
        Node::Assign {
            left,
            op: AssignOp::Assign,
            right,
        } if skip_parens(ast, right) == awaited
            && es_parent(cx, parent)
                .is_some_and(|g| matches!(cx.node(g), Node::ExprStmt { .. })) =>
        {
            single_name_pattern(cx, left).map(|e| (left, e))
        }
        _ => None,
    };
    if let Some((pattern, name)) = pattern {
        if would_remove_comments(cx, cx.span(pattern), &[]) {
            return None;
        }
        let mut edits = space_around_keyword(cx, pattern);
        edits.extend(space_around_keyword(cx, call));
        edits.push((cx.span(call), awaited_text(cx, elem)));
        return Some(with_edits(
            Fix::replace(cx.span(pattern), cx.text(name)),
            edits,
        ));
    }
    // `(await Promise.all([x]))[0]` as an initializer or assigned value.
    let Node::Index { obj, index, .. } = *cx.node(parent) else {
        return None;
    };
    let zero = matches!(*cx.node(skip_parens(ast, index)), Node::NumberLit { value } if value.get() == 0.0);
    if !zero || skip_parens(ast, obj) != awaited {
        return None;
    }
    let safe = es_parent(cx, parent).is_some_and(|g| match *cx.node(g) {
        Node::VarDeclarator { init: Some(i), .. } => skip_parens(ast, i) == parent,
        Node::Assign { right, .. } => skip_parens(ast, right) == parent,
        _ => false,
    });
    if !safe || would_remove_comments(cx, cx.span(parent), &[promise]) {
        return None;
    }
    Some(Fix::replace(
        cx.span(parent),
        format!("await {}", awaited_text(cx, elem)),
    ))
}

impl Rule for NoSinglePromiseInPromiseMethods {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = promise_method_call(ast, node, &["all", "any", "race"]) else {
            return;
        };
        let array = skip_parens(ast, call.args[0]);
        let Node::ArrayLit { elements } = *ast.node(array) else {
            return;
        };
        let elem = match ast.list(elements) {
            [only] if !matches!(ast.node(*only), Node::Hole {} | Node::Spread { .. }) => *only,
            _ => return,
        };
        let elem = skip_parens(ast, elem);
        let method = call.name;
        let prop = call.prop;
        let msg =
            format!("Wrapping single-element array with `Promise.{method}()` is unnecessary.");
        let id = "no-single-promise-in-promise-methods/error";
        let can_unwrap = !is_left_hand_side(cx, node)
            && !would_remove_comments(cx, cx.span(node), &[paren_span(cx, elem)]);
        let parent = es_parent(cx, node);
        let awaited = parent.is_some_and(|p| matches!(cx.node(p), Node::Await { .. }));
        let statement = parent
            .and_then(|p| es_parent(cx, p))
            .is_some_and(|g| matches!(cx.node(g), Node::ExprStmt { .. }));
        let span = cx.span(array);
        if method != "any" && awaited && (method != "all" || statement) {
            let fix = can_unwrap.then(|| unwrap_awaited(cx, node, elem));
            cx.report(span, msg).id(id).fix_opt(fix);
            return;
        }
        if method == "all" {
            let fix = fix_all_first_element(cx, node, elem);
            cx.report(span, msg).id(id).fix_opt(fix);
            return;
        }
        let unwrap = can_unwrap.then(|| {
            if awaited {
                unwrap_awaited(cx, node, elem)
            } else {
                unwrap_non_awaited(cx, node, elem)
            }
        });
        let resolve = switch_to_resolve(cx, prop, array);
        let mut r = cx.report(span, msg).id(id);
        if let Some(f) = unwrap {
            r = r.suggest(
                "no-single-promise-in-promise-methods/unwrap",
                "Use the value directly.",
                f,
            );
        }
        r.suggest(
            "no-single-promise-in-promise-methods/use-promise-resolve",
            "Switch to `Promise.resolve(…)`.",
            resolve,
        );
    }
}
