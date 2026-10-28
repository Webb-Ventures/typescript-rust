//! `unicorn/no-array-sort-for-min-max`: `x.sort(cmp)[0]`, `x.toSorted(cmp).at(0)` or
//! `.at(-1)` where `cmp` is `(a, b) => a - b` or `(a, b) => b - a` (an arrow or function
//! expression with two plain parameters whose body only returns the difference). Not when
//! the read is written to (assignment, update, `delete`, `for` head), any link of the
//! receiver is optional, or the receiver is known not to be an array/typed array or to be a
//! BigInt typed array. The first element of an ascending sort is the minimum.
//!
//! The suggestion replaces the read with `Math.min(...x)`/`Math.max(...x)` (`x` wrapped in
//! parentheses unless it is a primary or member/call expression; the whole wrapped when it
//! is an unparenthesized `new` callee), unless `x` is `super` or a comment outside `x`
//! would be lost.

use super::prelude::*;
use super::unicorn_ast::{is_left_hand_side, method_call, no_spread_in};
use super::unicorn_text::would_remove_comments;
use super::unicorn_types::{is_known_bigint_typed_array, is_known_non_indexed_collection};
use super::unicorn_util::{
    has_optional_chain, in_unparenthesized_new_callee, space_around_keyword,
};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-array-sort-for-min-max",
        "Disallow sorting arrays to get the minimum or maximum value.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<NoArraySortForMinMax>,
};

#[derive(Debug, Default)]
struct NoArraySortForMinMax;

fn is_number(ast: &Ast, id: NodeId, value: f64) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::NumberLit { value: v } if v.get() == value)
}

fn is_negative_one(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::Unary { op: UnaryOp::Minus, arg }
        if is_number(ast, arg, 1.0))
}

fn skip_ts(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..1000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. } => id = expr,
            _ => break,
        }
    }
    id
}

/// A concise arrow body, or the argument of a body that is only `return x;`.
fn return_expression(ast: &Ast, f: NodeId) -> Option<NodeId> {
    let body = match *ast.node(f) {
        Node::Arrow { flags, body, .. } => {
            if flags.contains(FnFlags::EXPRESSION_BODY) {
                return Some(body);
            }
            body
        }
        Node::FunctionExpr { body, .. } => body?,
        _ => return None,
    };
    let Node::Block { stmts } = *ast.node(body) else {
        return None;
    };
    match *ast.list(stmts) {
        [s] => match *ast.node(s) {
            Node::Return { arg } => arg,
            _ => None,
        },
        _ => None,
    }
}

/// The parameter's name when it is a plain identifier (no default, rest or pattern).
fn plain_param(ast: &Ast, p: NodeId) -> Option<&str> {
    match *ast.node(p) {
        Node::Param {
            rest: false,
            pattern,
            init: None,
            ..
        } if matches!(ast.node(pattern), Node::Ident { .. }) => ident_name(ast, pattern),
        _ => None,
    }
}

/// `Some(true)` for `(a, b) => a - b`, `Some(false)` for `(a, b) => b - a`.
fn ascending(ast: &Ast, comparator: NodeId) -> Option<bool> {
    let f = skip_parens(ast, comparator);
    let (flags, params) = match *ast.node(f) {
        Node::Arrow { flags, params, .. } | Node::FunctionExpr { flags, params, .. } => {
            (flags, ast.list(params))
        }
        _ => return None,
    };
    if flags.contains(FnFlags::ASYNC) || flags.contains(FnFlags::GENERATOR) {
        return None;
    }
    let [p1, p2] = *params else {
        return None;
    };
    let (a, b) = (plain_param(ast, p1)?, plain_param(ast, p2)?);
    if a == b {
        return None;
    }
    let body = skip_ts(ast, return_expression(ast, f)?);
    let Node::Binary {
        left,
        op: BinaryOp::Sub,
        right,
    } = *ast.node(body)
    else {
        return None;
    };
    let name = |id: NodeId| {
        let id = skip_parens(ast, id);
        match ast.node(id) {
            Node::Ident { .. } => ident_name(ast, id),
            _ => None,
        }
    };
    match (name(left)?, name(right)?) {
        (l, r) if l == a && r == b => Some(true),
        (l, r) if l == b && r == a => Some(false),
        _ => None,
    }
}

/// The receiver of `x.sort(cmp)` / `x.toSorted(cmp)` and whether `cmp` sorts ascending.
fn sorted_source(cx: &Ctx<'_, '_>, call: NodeId) -> Option<(NodeId, bool)> {
    let ast = cx.ast();
    let call = method_call(ast, skip_parens(ast, call))?;
    if !matches!(call.name, "sort" | "toSorted")
        || call.args.len() != 1
        || !no_spread_in(ast, call.args, 1)
        || !call.plain()
        || has_optional_chain(ast, call.obj)
    {
        return None;
    }
    let up = ascending(ast, call.args[0])?;
    if is_known_non_indexed_collection(cx, call.obj) || is_known_bigint_typed_array(cx, call.obj) {
        return None;
    }
    Some((skip_parens(ast, call.obj), up))
}

fn is_strong_precedence(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::ArrayLit { .. }
            | Node::Call { .. }
            | Node::Ident { .. }
            | Node::ImportCall { .. }
            | Node::NullLit {}
            | Node::BoolLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::StringLit { .. }
            | Node::RegexLit { .. }
            | Node::Member { .. }
            | Node::Index { .. }
            | Node::New { .. }
            | Node::Super {}
            | Node::TaggedTemplate { .. }
            | Node::TemplateLit { .. }
            | Node::This {}
            | Node::NonNull { .. }
    )
}

fn report(cx: &mut Ctx<'_, '_>, node: NodeId, source: NodeId, up: bool, first: bool) {
    let ast = cx.ast();
    let method = if up == first { "min" } else { "max" };
    let extreme = if method == "min" {
        "minimum"
    } else {
        "maximum"
    };
    let span = cx.span(node);
    let suggestion = (!matches!(ast.node(source), Node::Super {})
        && !would_remove_comments(cx, span, &[cx.span(source)]))
    .then(|| {
        let text = cx.text(source);
        let text = if is_strong_precedence(ast, source) {
            text.to_owned()
        } else {
            format!("({text})")
        };
        let replacement = format!("Math.{method}(...{text})");
        let fixed = if in_unparenthesized_new_callee(cx, node) {
            format!("({replacement})")
        } else {
            replacement.clone()
        };
        let mut fix = Fix::replace(span, fixed);
        for (s, t) in space_around_keyword(cx, node) {
            fix = fix.and(s, t);
        }
        (replacement, fix)
    });
    let r = cx
        .report(
            span,
            format!("Use `Math.{method}()` instead of sorting to get the {extreme} value."),
        )
        .id("no-array-sort-for-min-max/error");
    if let Some((replacement, fix)) = suggestion {
        r.suggest(
            "no-array-sort-for-min-max/suggestion",
            format!("Replace with `{replacement}`."),
            fix,
        );
    }
}

impl Rule for NoArraySortForMinMax {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Index, NodeKind::Call]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (obj, first) = match *ast.node(id) {
            Node::Index {
                obj,
                optional: false,
                index,
            } if is_number(ast, index, 0.0) => (obj, true),
            Node::Call { .. } => {
                let Some(call) = method_call(ast, id) else {
                    return;
                };
                if call.name != "at"
                    || call.args.len() != 1
                    || !no_spread_in(ast, call.args, 1)
                    || !call.plain()
                {
                    return;
                }
                let first = if is_number(ast, call.args[0], 0.0) {
                    true
                } else if is_negative_one(ast, call.args[0]) {
                    false
                } else {
                    return;
                };
                (call.obj, first)
            }
            _ => return,
        };
        if is_left_hand_side(cx, id) {
            return;
        }
        let Some((source, up)) = sorted_source(cx, obj) else {
            return;
        };
        report(cx, id, source, up, first);
    }
}
