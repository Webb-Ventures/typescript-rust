//! `unicorn/prefer-negative-index`: `x.slice(-1)` over `x.slice(x.length - 1)`.
//!
//! The methods `slice`, `subarray` (arguments 0 and 1), `splice`, `toSpliced` and `at`
//! (argument 0) are read from a direct call `x.slice(...)`, from `Owner.slice.call(x, ...)`
//! or from `Owner.slice.apply(x, [...])`, where the owner is `[]`, `''` (only for `slice`) or
//! `Name.prototype` with a name that has the method (`Array`, `String`, `ArrayBuffer`, the
//! typed arrays). A member name is an identifier key, also a computed one (`x[slice]`).
//!
//! An argument counts when it is `<len> - n` with `n` a positive safe integer literal and
//! `<len>` either `x.length` (the same reference as the receiver, through TS wrappers) or,
//! recursively, another such subtraction (`x.length - 1 - 2`). The fix removes the
//! `x.length` with its parentheses and the whitespace after it, then the whitespace between
//! the `-` and a following number; it is dropped when a comment sits in the removed access.
//! It is a fix when every counted argument is `x.length - 1` (the same for any length),
//! otherwise a suggestion.

use super::prelude::*;
use super::unicorn_util::{parenthesized_span, skip_wrappers};
use super::unicorn_util_h5::{is_same_reference, static_string, would_remove_comments};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-negative-index",
        "Prefer negative index over `.length - index` when possible.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferNegativeIndex>,
};

#[derive(Debug, Default)]
struct PreferNegativeIndex;

const TYPED_ARRAYS: &[&str] = &[
    "Int8Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "Int16Array",
    "Uint16Array",
    "Int32Array",
    "Uint32Array",
    "Float16Array",
    "Float32Array",
    "Float64Array",
    "BigInt64Array",
    "BigUint64Array",
];

/// The argument positions a method takes an index at.
fn index_arguments(method: &str) -> Option<&'static [usize]> {
    match method {
        "slice" | "subarray" => Some(&[0, 1]),
        "splice" | "toSpliced" | "at" => Some(&[0]),
        _ => None,
    }
}

/// True when the global `owner` has `method` on its prototype (as far as this rule knows).
fn owner_has_method(owner: &str, method: &str) -> bool {
    let typed = TYPED_ARRAYS.contains(&owner);
    match method {
        "slice" => typed || matches!(owner, "Array" | "String" | "ArrayBuffer"),
        "subarray" => typed,
        "splice" | "toSpliced" => owner == "Array",
        "at" => typed || matches!(owner, "Array" | "String"),
        _ => false,
    }
}

/// The member name of `id` when its key is an identifier (computed or not).
fn member_name(ast: &ts_r_ast::Ast, id: NodeId) -> Option<(&str, NodeId)> {
    match *ast.node(id) {
        Node::Member { obj, prop, .. } => ident_name(ast, prop).map(|n| (n, obj)),
        Node::Index { obj, index, .. } => ident_name(ast, index).map(|n| (n, obj)),
        _ => None,
    }
}

fn is_method_owner(ast: &ts_r_ast::Ast, id: NodeId, method: &str) -> bool {
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::ArrayLit { elements: elems } if ast.list(elems).is_empty() => return true,
        _ => {}
    }
    if method == "slice" && static_string(ast, id) == Some("") {
        return true;
    }
    matches!(member_name(ast, id), Some(("prototype", obj))
        if ident_name(ast, obj).is_some_and(|owner| owner_has_method(owner, method)))
}

impl Rule for PreferNegativeIndex {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *cx.node(node) else {
            return;
        };
        let Some((name, obj)) = member_name(ast, skip_parens(ast, callee)) else {
            return;
        };
        let args = ast.list(args);
        let (method, target, list): (&str, NodeId, Vec<Option<NodeId>>) =
            if index_arguments(name).is_some() {
                (name, obj, args.iter().map(|&a| Some(a)).collect())
            } else if name == "call" || name == "apply" {
                let Some((method, owner)) = member_name(ast, skip_parens(ast, obj)) else {
                    return;
                };
                if index_arguments(method).is_none() || !is_method_owner(ast, owner, method) {
                    return;
                }
                let Some(&target) = args.first() else {
                    return;
                };
                if name == "call" {
                    (method, target, args[1..].iter().map(|&a| Some(a)).collect())
                } else {
                    let Some(&second) = args.get(1) else {
                        return;
                    };
                    let Node::ArrayLit { elements: elems } = *ast.node(second) else {
                        return;
                    };
                    let elems = ast
                        .list(elems)
                        .iter()
                        .map(|&e| (!matches!(ast.node(e), Node::Hole {})).then_some(e))
                        .collect();
                    (method, target, elems)
                }
            } else {
                return;
            };
        let Some(indexes) = index_arguments(method) else {
            return;
        };
        let counted: Vec<(NodeId, NodeId)> = indexes
            .iter()
            .filter_map(|&i| list.get(i).copied().flatten())
            .filter_map(|arg| length_node(cx, arg, target, 0).map(|len| (arg, len)))
            .collect();
        if counted.is_empty() {
            return;
        }
        let fix = removal_fix(cx, &counted);
        let exact = counted
            .iter()
            .all(|&(arg, _)| is_length_minus_one(cx, arg, target));
        let r = cx
            .report_node(
                node,
                format!("Prefer negative index over length minus index for `{method}`."),
            )
            .id("prefer-negative-index");
        if let Some(f) = fix {
            if exact {
                r.fix(f);
            } else {
                r.suggest(
                    "prefer-negative-index/suggestion",
                    "Use a negative index.",
                    f,
                );
            }
        }
    }
}

/// `object.length` (not optional, not computed; through TS wrappers) of the same reference.
fn is_length_of(cx: &Ctx<'_, '_>, id: NodeId, object: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(skip_wrappers(ast, id)) {
        Node::Member {
            obj,
            prop,
            optional: false,
        } => ident_name(ast, prop) == Some("length") && is_same_reference(cx, obj, object),
        _ => false,
    }
}

/// The `x.length` operand of `x.length - n [- m ...]`, `n` a positive safe integer.
fn length_node(cx: &Ctx<'_, '_>, id: NodeId, object: NodeId, depth: u32) -> Option<NodeId> {
    if depth > 64 {
        return None;
    }
    let ast = cx.ast();
    let Node::Binary {
        left,
        op: BinaryOp::Sub,
        right,
    } = *ast.node(skip_wrappers(ast, id))
    else {
        return None;
    };
    match *ast.node(skip_wrappers(ast, right)) {
        Node::NumberLit { value }
            if value.get() > 0.0
                && value.get().fract() == 0.0
                && value.get() <= 9_007_199_254_740_991.0 => {}
        _ => return None,
    }
    if is_length_of(cx, left, object) {
        return Some(skip_parens(ast, left));
    }
    length_node(cx, left, object, depth + 1)
}

/// A static number: a numeric literal under any `+`/`-` signs (through TS wrappers).
fn static_number(ast: &ts_r_ast::Ast, id: NodeId, depth: u32) -> Option<f64> {
    if depth > 64 {
        return None;
    }
    match *ast.node(skip_wrappers(ast, id)) {
        Node::NumberLit { value } => Some(value.get()),
        Node::Unary {
            op: UnaryOp::Plus,
            arg,
        } => static_number(ast, arg, depth + 1),
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } => static_number(ast, arg, depth + 1).map(|v| -v),
        _ => None,
    }
}

fn is_length_minus_one(cx: &Ctx<'_, '_>, id: NodeId, object: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(skip_wrappers(ast, id)) {
        Node::Binary {
            left,
            op: BinaryOp::Sub,
            right,
        } => static_number(ast, right, 0) == Some(1.0) && is_length_of(cx, left, object),
        _ => false,
    }
}

/// Removes each `x.length` (with its parentheses and the whitespace after it) and the
/// whitespace between the `-` and a number that follows.
fn removal_fix(cx: &Ctx<'_, '_>, counted: &[(NodeId, NodeId)]) -> Option<Fix> {
    let src = cx.src();
    let bytes = src.as_bytes();
    let mut fix: Option<Fix> = None;
    let add = |fix: &mut Option<Fix>, span: Span| {
        *fix = Some(match fix.take() {
            None => Fix::delete(span),
            Some(f) => f.and(span, ""),
        });
    };
    for &(_, len) in counted {
        let range = parenthesized_span(cx, len);
        if would_remove_comments(cx, range, &[]) {
            return None;
        }
        let end = src[range.end as usize..]
            .char_indices()
            .find(|(_, c)| !c.is_whitespace())
            .map_or(src.len(), |(i, _)| range.end as usize + i);
        add(&mut fix, Span::new(range.start, end as u32));
        let op = skip_trivia_forward(src, range.end as usize);
        if bytes.get(op) == Some(&b'-') && !matches!(bytes.get(op + 1), Some(b'-' | b'=')) {
            let num = src[op + 1..]
                .char_indices()
                .find(|(_, c)| !c.is_whitespace())
                .map_or(src.len(), |(i, _)| op + 1 + i);
            let starts_number = match bytes.get(num) {
                Some(b) if b.is_ascii_digit() => true,
                Some(b'.') => bytes.get(num + 1).is_some_and(u8::is_ascii_digit),
                _ => false,
            };
            if starts_number && num > op + 1 {
                add(&mut fix, Span::new(op as u32 + 1, num as u32));
            }
        }
    }
    fix
}
