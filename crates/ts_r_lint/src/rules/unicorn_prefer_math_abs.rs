//! `unicorn/prefer-math-abs`: `x < 0 ? -x : x` (and its mirrored forms, with `-x`,
//! `0 - x`, `x * -1` or `-1 * x` as the negation) is `Math.abs(x)`, and
//! `x > n || x < -n` (either order, either side of each comparison) is
//! `Math.abs(x) > n`. Operands that may be BigInts (literals, `BigInt()`, annotations,
//! JSDoc, definitions) are left alone. The rewrite is a suggestion, since `Math.abs`
//! coerces and normalizes `-0`; it is offered for side-effect free names, literals and
//! `this`, when no comment would be lost.

use super::prelude::*;
use super::unicorn_text::{
    is_parenthesized, is_same_reference, paren_span, paren_text, would_remove_comments,
};
use super::unicorn_types::{BIGINT_MIXED_UNION, binding_annotation, is_bigint_with};
use super::unicorn_util::{skip_wrappers, space_around_keyword, with_edits};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-math-abs",
        "Prefer `Math.abs()` over manual absolute value expressions and symmetric range checks.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<PreferMathAbs>,
};

#[derive(Debug, Default)]
struct PreferMathAbs;

fn invert(op: BinaryOp) -> BinaryOp {
    match op {
        BinaryOp::Gt => BinaryOp::Lt,
        BinaryOp::GtEq => BinaryOp::LtEq,
        BinaryOp::Lt => BinaryOp::Gt,
        _ => BinaryOp::GtEq,
    }
}

fn op_text(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Gt => ">",
        BinaryOp::GtEq => ">=",
        BinaryOp::Lt => "<",
        _ => "<=",
    }
}

fn is_zero(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::NumberLit { value } if value.get() == 0.0)
}

/// `-1` written as a unary minus on the literal `1`.
fn is_negative_one(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::Unary { op: UnaryOp::Minus, arg }
        if matches!(*ast.node(arg), Node::NumberLit { value } if value.get() == 1.0))
}

/// A `bigint` keyword or BigInt literal type, alone or in a union or intersection.
fn is_bigint_annotation(ast: &Ast, ty: NodeId, depth: u32) -> bool {
    if depth > 100 {
        return false;
    }
    match *ast.node(ty) {
        Node::KeywordType {
            kind: KeywordTypeKind::BigInt,
        }
        | Node::BigIntLit { .. } => true,
        Node::UnionType { types } | Node::IntersectionType { types } => ast
            .list(types)
            .iter()
            .any(|&t| is_bigint_annotation(ast, t, depth + 1)),
        _ => false,
    }
}

/// The type asserted by `as`, `satisfies` or `<T>` (through `!`).
fn asserted_type(ast: &Ast, mut id: NodeId) -> Option<NodeId> {
    for _ in 0..1000 {
        match *ast.node(id) {
            Node::NonNull { expr } | Node::Paren { expr } => id = expr,
            Node::As { ty, .. } | Node::Satisfies { ty, .. } | Node::TypeAssertion { ty, .. } => {
                return Some(ty);
            }
            _ => return None,
        }
    }
    None
}

fn is_bigint_expression(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    let ast = cx.ast();
    if depth > 100 {
        return false;
    }
    let node = skip_parens(ast, id);
    match *ast.node(node) {
        Node::BigIntLit { .. } => return true,
        Node::Call {
            callee,
            optional: false,
            args,
            ..
        } if ident_name(ast, callee) == Some("BigInt")
            && matches!(ast.list(args), [a] if !matches!(ast.node(*a), Node::Spread { .. })) =>
        {
            return true;
        }
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } if is_bigint_expression(cx, arg, depth + 1) => return true,
        _ => {}
    }
    asserted_type(ast, node).is_some_and(|t| is_bigint_annotation(ast, t, 0))
        || is_bigint_with(cx, node, &BIGINT_MIXED_UNION)
}

/// A parameter or variable declaration of the name has a BigInt annotation, default or
/// initializer.
fn has_bigint_definition(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let ident = skip_wrappers(ast, id);
    if !matches!(ast.node(ident), Node::Ident { .. }) {
        return false;
    }
    let sem = cx.semantic();
    let Some(s) = sem.symbol_of_ident(ident) else {
        return false;
    };
    sem.symbol(s).decls.iter().any(|d| {
        let value = match d.kind {
            DeclKind::Param => cx.parent(d.name).and_then(|p| match *ast.node(p) {
                Node::Param { pattern, init, .. } if pattern == d.name => init,
                _ => None,
            }),
            DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using => {
                match *ast.node(d.node) {
                    Node::VarDeclarator { name, init, .. } if name == d.name => init,
                    _ => return false,
                }
            }
            _ => return false,
        };
        binding_annotation(cx, d.name)
            .0
            .is_some_and(|t| is_bigint_annotation(ast, t, 0))
            || value.is_some_and(|v| is_bigint_expression(cx, v, 0))
    })
}

fn can_be_bigint(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    is_bigint_expression(cx, id, 0) || has_bigint_definition(cx, id)
}

/// A relational comparison with no possibly-BigInt operand: `(left, op, right)`.
fn comparison(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, BinaryOp, NodeId)> {
    let ast = cx.ast();
    let Node::Binary { left, op, right } = *ast.node(skip_parens(ast, id)) else {
        return None;
    };
    if !matches!(
        op,
        BinaryOp::Gt | BinaryOp::GtEq | BinaryOp::Lt | BinaryOp::LtEq
    ) || can_be_bigint(cx, left)
        || can_be_bigint(cx, right)
    {
        return None;
    }
    Some((left, op, right))
}

/// `-value`, `0 - value`, `value * -1` or `-1 * value`.
fn is_negation_of(cx: &Ctx<'_, '_>, id: NodeId, value: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(skip_parens(ast, id)) {
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } => is_same_reference(cx, arg, value),
        Node::Binary {
            left,
            op: BinaryOp::Sub,
            right,
        } => is_zero(ast, left) && is_same_reference(cx, right, value),
        Node::Binary {
            left,
            op: BinaryOp::Mul,
            right,
        } => {
            (is_same_reference(cx, left, value) && is_negative_one(ast, right))
                || (is_negative_one(ast, left) && is_same_reference(cx, right, value))
        }
        _ => false,
    }
}

/// `x < 0 ? -x : x` and its mirrors: `x`.
fn ternary_value(cx: &Ctx<'_, '_>, test: NodeId, cons: NodeId, alt: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let (left, op, right) = comparison(cx, test)?;
    let (value, op) = if is_zero(ast, right) {
        (left, op)
    } else if is_zero(ast, left) {
        (right, invert(op))
    } else {
        return None;
    };
    let matched = match op {
        BinaryOp::Lt | BinaryOp::LtEq => {
            is_negation_of(cx, cons, value) && is_same_reference(cx, alt, value)
        }
        _ => is_same_reference(cx, cons, value) && is_negation_of(cx, alt, value),
    };
    matched.then_some(value)
}

/// `value > threshold` as `(value, threshold, op)` with `op` one of `>`/`>=`.
fn greater(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId, BinaryOp)> {
    let (left, op, right) = comparison(cx, id)?;
    Some(match op {
        BinaryOp::Gt | BinaryOp::GtEq => (left, right, op),
        _ => (right, left, invert(op)),
    })
}

/// `value < threshold` as `(value, threshold, op)` with `op` one of `<`/`<=`.
fn less(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId, BinaryOp)> {
    let (left, op, right) = comparison(cx, id)?;
    Some(match op {
        BinaryOp::Lt | BinaryOp::LtEq => (left, right, op),
        _ => (right, left, invert(op)),
    })
}

/// `x > n || x < -n` (either order): `(x, n, op)`.
fn range_check(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> Option<(NodeId, NodeId, BinaryOp)> {
    let pair = |g: NodeId, l: NodeId| {
        let (value, threshold, op) = greater(cx, g)?;
        let (lvalue, negative, lop) = less(cx, l)?;
        (is_same_reference(cx, value, lvalue)
            && invert(op) == lop
            && is_negation_of(cx, negative, threshold))
        .then_some((value, threshold, op))
    };
    pair(a, b).or_else(|| pair(b, a))
}

fn is_side_effect_free_reference(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(skip_wrappers(ast, id)),
        Node::Ident { .. }
            | Node::This {}
            | Node::NumberLit { .. }
            | Node::StringLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
    )
}

/// `getComparisonOperandText`: an unparenthesized `as`/`satisfies` gets parentheses.
fn comparison_operand_text(cx: &Ctx<'_, '_>, id: NodeId) -> String {
    let text = paren_text(cx, id);
    if !is_parenthesized(cx, id) && matches!(cx.node(id), Node::As { .. } | Node::Satisfies { .. })
    {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

impl Rule for PreferMathAbs {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Conditional, NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (replacement, kept) = match *ast.node(node) {
            Node::Conditional { test, cons, alt } => {
                let Some(value) = ternary_value(cx, test, cons, alt) else {
                    return;
                };
                let value = skip_parens(ast, value);
                let text = is_side_effect_free_reference(ast, value)
                    .then(|| format!("Math.abs({})", paren_text(cx, value)));
                (text, vec![paren_span(cx, value)])
            }
            Node::Binary {
                left,
                op: BinaryOp::LogicalOr,
                right,
            } => {
                let Some((value, threshold, op)) = range_check(cx, left, right) else {
                    return;
                };
                let (value, threshold) = (skip_parens(ast, value), skip_parens(ast, threshold));
                let text = (is_side_effect_free_reference(ast, value)
                    && is_side_effect_free_reference(ast, threshold))
                .then(|| {
                    format!(
                        "Math.abs({}) {} {}",
                        paren_text(cx, value),
                        op_text(op),
                        comparison_operand_text(cx, threshold)
                    )
                });
                (text, vec![paren_span(cx, value), paren_span(cx, threshold)])
            }
            _ => return,
        };
        let span = cx.span(node);
        let fix = replacement
            .filter(|_| !would_remove_comments(cx, span, &kept))
            .map(|text| with_edits(Fix::replace(span, text), space_around_keyword(cx, node)));
        let report = cx
            .report_node(node, "Prefer `Math.abs()` to simplify this expression.")
            .id("prefer-math-abs");
        if let Some(fix) = fix {
            report.suggest(
                "prefer-math-abs/suggestion",
                "Replace with `Math.abs()`.",
                fix,
            );
        }
    }
}
