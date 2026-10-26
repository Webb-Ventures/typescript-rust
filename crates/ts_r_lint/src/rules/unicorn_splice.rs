//! The `splice()` calls a simpler call or statement can replace, shared by
//! `unicorn/no-unnecessary-splice` (which reports them) and `unicorn/no-array-splice`
//! (which leaves them to it). Numbers are literals through `+`/`-` and TS wrappers; the
//! length checks compare the receiver with `isSameReference`.

use super::prelude::*;
use super::unicorn_text::is_same_reference;
use super::unicorn_util::skip_wrappers;
use ts_r_ast::Ast;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpliceReplacement {
    /// The call changes nothing.
    NoOp,
    /// It empties the array (`.length = 0`).
    Empty,
    /// It is `shift()`, `unshift(...)`, `pop()` or `push(...)`, keeping the arguments
    /// from index 2 on.
    Method(&'static str),
}

impl SpliceReplacement {
    pub(crate) fn message_id(self) -> &'static str {
        match self {
            SpliceReplacement::NoOp => "no-op",
            SpliceReplacement::Empty => "empty",
            SpliceReplacement::Method(m) => m,
        }
    }
}

/// `getStaticNumberValue`: a numeric literal, possibly signed.
pub(crate) fn static_number(ast: &Ast, id: NodeId) -> Option<f64> {
    let mut id = skip_wrappers(ast, id);
    let mut negate = false;
    for _ in 0..1000 {
        match *ast.node(id) {
            Node::NumberLit { value } => {
                let v = value.get();
                return Some(if negate { -v } else { v });
            }
            Node::Unary {
                op: op @ (UnaryOp::Minus | UnaryOp::Plus),
                arg,
            } => {
                negate ^= op == UnaryOp::Minus;
                id = skip_wrappers(ast, arg);
            }
            _ => return None,
        }
    }
    None
}

/// Positive or negative zero.
fn is_zero(ast: &Ast, id: NodeId) -> bool {
    static_number(ast, id) == Some(0.0)
}

fn is_one(ast: &Ast, id: NodeId) -> bool {
    static_number(ast, id) == Some(1.0)
}

fn is_zero_or_negative(ast: &Ast, id: NodeId) -> bool {
    static_number(ast, id).is_some_and(|v| v <= 0.0)
}

/// `object.length` (through TS wrappers; not optional, not computed).
fn is_length_of(cx: &Ctx<'_, '_>, id: NodeId, object: NodeId) -> bool {
    let ast = cx.ast();
    matches!(*ast.node(skip_wrappers(ast, id)), Node::Member { obj, optional: false, prop }
        if ident_name(ast, prop) == Some("length") && is_same_reference(cx, obj, object))
}

/// `object.length - 1`.
fn is_length_minus_one_of(cx: &Ctx<'_, '_>, id: NodeId, object: NodeId) -> bool {
    let ast = cx.ast();
    matches!(*ast.node(skip_wrappers(ast, id)), Node::Binary { left, op: BinaryOp::Sub, right }
        if is_one(ast, right) && is_length_of(cx, left, object))
}

/// What `object.splice(args)` can be replaced with, if anything.
pub(crate) fn unnecessary_splice_replacement(
    cx: &Ctx<'_, '_>,
    object: NodeId,
    args: &[NodeId],
) -> Option<SpliceReplacement> {
    let ast = cx.ast();
    match args {
        [] => Some(SpliceReplacement::NoOp),
        &[start] if is_zero(ast, start) => Some(SpliceReplacement::Empty),
        &[start, count] if is_zero(ast, start) && is_length_of(cx, count, object) => {
            Some(SpliceReplacement::Empty)
        }
        &[_, count] if is_zero_or_negative(ast, count) => Some(SpliceReplacement::NoOp),
        &[start, count] if is_zero(ast, start) && is_one(ast, count) => {
            Some(SpliceReplacement::Method("shift"))
        }
        &[start, count, ..] if is_zero(ast, start) && is_zero_or_negative(ast, count) => {
            Some(SpliceReplacement::Method("unshift"))
        }
        &[start, count] if is_length_minus_one_of(cx, start, object) && is_one(ast, count) => {
            Some(SpliceReplacement::Method("pop"))
        }
        &[start, count, ..]
            if is_length_of(cx, start, object) && is_zero_or_negative(ast, count) =>
        {
            Some(SpliceReplacement::Method("push"))
        }
        _ => None,
    }
}
