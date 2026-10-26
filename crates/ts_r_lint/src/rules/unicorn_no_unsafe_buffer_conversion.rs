//! `unicorn/no-unsafe-buffer-conversion`: converting an ArrayBuffer view through `.buffer`
//! without its `byteOffset` and `byteLength` reads the whole underlying buffer.
//!
//! Three shapes take `<view>.buffer` (a non-computed `.buffer`, `?.` allowed):
//! `new <TypedArray>(buffer[, offset[, length]])` (global constructor, 1-3 arguments),
//! `<Buffer>.from(buffer[, offset[, length]])` (the global `Buffer`, on `globalThis`/
//! `window`/`self`/`global`, or imported from `buffer`) and `buffer.slice([start[, end]])`.
//! With fewer than all arguments it is reported. With all of them it is fine when they are
//! `<view>.byteOffset` and `<view>.byteLength` (divided by a same-sized `BYTES_PER_ELEMENT`
//! for wider typed arrays; `byteOffset + byteLength` as a slice end), and reported only when
//! one is known wrong: offset `0`, `undefined`, `<view>.byteLength`/`.length`; length
//! `undefined`, `<view>.buffer.byteLength`, a mismatched element size.
//!
//! The suggestion rewrites the call with the view's offset and length; it is withheld for a
//! `?.` on `.buffer` or the call, a comment in the call, or a view with side effects
//! (getters count).

use super::prelude::*;
use super::unicorn_prefer_spread::is_buffer_reference;
use super::unicorn_static::has_side_effect_with_getters;
use super::unicorn_text::{is_same_reference, member_object_needs_parens};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unsafe-buffer-conversion",
        "Prevent unsafe use of ArrayBuffer view `.buffer`.",
        NO_PRESET,
        Category::Correctness,
    )
    .suggestions(),
    create: create_default::<NoUnsafeBufferConversion>,
};

#[derive(Debug, Default)]
struct NoUnsafeBufferConversion;

const TYPED_ARRAYS: &[(&str, u8)] = &[
    ("Int8Array", 1),
    ("Uint8Array", 1),
    ("Uint8ClampedArray", 1),
    ("Int16Array", 2),
    ("Uint16Array", 2),
    ("Float16Array", 2),
    ("Int32Array", 4),
    ("Uint32Array", 4),
    ("Float32Array", 4),
    ("Float64Array", 8),
    ("BigInt64Array", 8),
    ("BigUint64Array", 8),
];

fn bytes_per_element(name: &str) -> Option<u8> {
    TYPED_ARRAYS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|&(_, b)| b)
}

/// `<obj>.<name>` (non-computed, through parentheses): the object.
fn member(ast: &Ast, id: NodeId, names: &[&str]) -> Option<NodeId> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Member { obj, prop, .. }
            if ident_name(ast, prop).is_some_and(|n| names.contains(&n)) =>
        {
            Some(obj)
        }
        _ => None,
    }
}

fn is_view_member(cx: &Ctx<'_, '_>, id: NodeId, name: &str, view: NodeId) -> bool {
    member(cx.ast(), id, &[name]).is_some_and(|obj| is_same_reference(cx, obj, view))
}

fn is_zero(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::NumberLit { value } if value.get() == 0.0)
}

fn is_global_undefined(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    matches!(ast.node(id), Node::Ident { .. })
        && ident_name(ast, id) == Some("undefined")
        && cx.is_global_ref(id, "undefined")
}

fn unsafe_offset(cx: &Ctx<'_, '_>, id: NodeId, view: NodeId) -> bool {
    let ast = cx.ast();
    is_zero(ast, id)
        || is_global_undefined(cx, id)
        || member(ast, id, &["byteLength", "length"])
            .is_some_and(|o| is_same_reference(cx, o, view))
}

fn unsafe_length(cx: &Ctx<'_, '_>, id: NodeId, view: NodeId) -> bool {
    let ast = cx.ast();
    is_global_undefined(cx, id)
        || member(ast, id, &["byteLength"])
            .and_then(|o| member(ast, o, &["buffer"]))
            .is_some_and(|o| is_same_reference(cx, o, view))
}

/// `<view>.byteLength / <X>.BYTES_PER_ELEMENT`: X's element size.
fn divided_length(cx: &Ctx<'_, '_>, id: NodeId, view: NodeId) -> Option<Option<u8>> {
    let ast = cx.ast();
    let Node::Binary {
        left,
        op: BinaryOp::Div,
        right,
    } = *ast.node(skip_parens(ast, id))
    else {
        return None;
    };
    if !is_view_member(cx, left, "byteLength", view) {
        return None;
    }
    let obj = skip_parens(ast, member(ast, right, &["BYTES_PER_ELEMENT"])?);
    if !matches!(ast.node(obj), Node::Ident { .. }) {
        return None;
    }
    Some(ident_name(ast, obj).and_then(bytes_per_element))
}

/// `(view, optional .buffer)` for `<view>.buffer`.
fn buffer_view(ast: &Ast, id: NodeId) -> Option<(NodeId, bool)> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Member {
            obj,
            optional,
            prop,
        } if ident_name(ast, prop) == Some("buffer") => Some((skip_parens(ast, obj), optional)),
        _ => None,
    }
}

/// The view's own text, parenthesized when a member access on it needs it.
fn view_text(cx: &Ctx<'_, '_>, view: NodeId) -> String {
    let text = cx.span(view).text(cx.src());
    if member_object_needs_parens(cx, view, view) {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

fn no_spread(ast: &Ast, args: &[NodeId]) -> bool {
    !args
        .iter()
        .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
}

fn report(cx: &mut Ctx<'_, '_>, node: NodeId, view: NodeId, can_suggest: bool, text: String) {
    let ast = cx.ast();
    let span = cx.span(node);
    let suggest =
        can_suggest && ast.comments_in(span).is_empty() && !has_side_effect_with_getters(ast, view);
    let r = cx
        .report(
            span,
            "Preserve `byteOffset` and `byteLength` when converting an ArrayBuffer view through `.buffer`.",
        )
        .id("no-unsafe-buffer-conversion/error");
    if suggest {
        r.suggest(
            "no-unsafe-buffer-conversion/suggestion",
            "Preserve `byteOffset` and `byteLength`.",
            Fix::replace(span, text),
        );
    }
}

impl Rule for NoUnsafeBufferConversion {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *ast.node(node) {
            Node::New { callee, args, .. } => {
                let args = ast.list(args);
                let callee = skip_parens(ast, callee);
                if !(1..=3).contains(&args.len()) || !no_spread(ast, args) {
                    return;
                }
                let Some(name) = ident_name(ast, callee)
                    .filter(|_| matches!(ast.node(callee), Node::Ident { .. }))
                else {
                    return;
                };
                let Some(size) = bytes_per_element(name) else {
                    return;
                };
                if !cx.is_global_ref(callee, name) {
                    return;
                }
                let Some((view, optional)) = buffer_view(ast, args[0]) else {
                    return;
                };
                if let (Some(&offset), Some(&length)) = (args.get(1), args.get(2)) {
                    let divided = divided_length(cx, length, view);
                    let safe_offset = is_view_member(cx, offset, "byteOffset", view);
                    let safe_length = if size == 1 {
                        is_view_member(cx, length, "byteLength", view)
                    } else {
                        divided == Some(Some(size))
                    };
                    if safe_offset && safe_length {
                        return;
                    }
                    let unsafe_len = !safe_length
                        && (unsafe_length(cx, length, view)
                            || is_view_member(cx, length, "byteLength", view)
                            || matches!(divided, Some(Some(s)) if s != size));
                    if !unsafe_offset(cx, offset, view) && !unsafe_len {
                        return;
                    }
                }
                let v = view_text(cx, view);
                let length = if size == 1 {
                    format!("{v}.byteLength")
                } else {
                    format!("{v}.byteLength / {name}.BYTES_PER_ELEMENT")
                };
                let text = format!("new {name}({v}.buffer, {v}.byteOffset, {length})");
                report(cx, node, view, !optional, text);
            }
            Node::Call {
                callee,
                optional: optional_call,
                args,
                ..
            } => {
                let member_callee = skip_parens(ast, callee);
                let Node::Member { obj, prop, .. } = *ast.node(member_callee) else {
                    return;
                };
                let args = ast.list(args);
                if !no_spread(ast, args) {
                    return;
                }
                let callee_text = cx.span(member_callee).text(cx.src()).to_owned();
                match ident_name(ast, prop) {
                    Some("from") if (1..=3).contains(&args.len()) => {
                        if !is_buffer_reference(cx, obj) {
                            return;
                        }
                        let Some((view, optional)) = buffer_view(ast, args[0]) else {
                            return;
                        };
                        if let (Some(&offset), Some(&length)) = (args.get(1), args.get(2)) {
                            let safe_offset = is_view_member(cx, offset, "byteOffset", view);
                            let safe_length = is_view_member(cx, length, "byteLength", view);
                            if safe_offset && safe_length {
                                return;
                            }
                            let unsafe_len = unsafe_length(cx, length, view)
                                || matches!(divided_length(cx, length, view), Some(Some(s)) if s > 1);
                            if !unsafe_offset(cx, offset, view) && !unsafe_len {
                                return;
                            }
                        }
                        let v = view_text(cx, view);
                        let text =
                            format!("{callee_text}({v}.buffer, {v}.byteOffset, {v}.byteLength)");
                        report(cx, node, view, !optional && !optional_call, text);
                    }
                    Some("slice") if args.len() <= 2 => {
                        let Some((view, optional)) = buffer_view(ast, obj) else {
                            return;
                        };
                        if let (Some(&start), Some(&end)) = (args.first(), args.get(1)) {
                            let safe_start = is_view_member(cx, start, "byteOffset", view);
                            let safe_end = matches!(*ast.node(skip_parens(ast, end)), Node::Binary { left, op: BinaryOp::Add, right }
                                if is_view_member(cx, left, "byteOffset", view)
                                    && is_view_member(cx, right, "byteLength", view));
                            if safe_start && safe_end {
                                return;
                            }
                            let unsafe_end = (safe_start
                                && is_view_member(cx, end, "byteLength", view))
                                || unsafe_length(cx, end, view);
                            if !unsafe_offset(cx, start, view) && !unsafe_end {
                                return;
                            }
                        }
                        let v = view_text(cx, view);
                        let text = format!(
                            "{callee_text}({v}.byteOffset, {v}.byteOffset + {v}.byteLength)"
                        );
                        report(cx, node, view, !optional && !optional_call, text);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}
