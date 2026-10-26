//! `unicorn/no-impossible-length-comparison`: comparing `x.length` / `x.size` (through TS
//! wrappers, on either side) with a finite static number it can never be below (or is
//! always at least) has a constant result: `x.length < 0` is always false, `x.length >= 0`
//! always true. `this`/`super` receivers, optional chains, objects known to have a
//! non-collection `length`, and `x.width && x.length ...` shape checks are left alone
//! (the helpers are shared with `explicit-length-check`).

use super::prelude::*;
use super::unicorn_explicit_length_check::{has_shape_property_check, known_non_collection_length};
use super::unicorn_static::{Static, static_value_control_flow};
use super::unicorn_util::{has_optional_chain, skip_wrappers};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-impossible-length-comparison",
        "Disallow impossible comparisons against `.length` or `.size`.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoImpossibleLengthComparison>,
};

#[derive(Debug, Default)]
struct NoImpossibleLengthComparison;

/// The operator with its operands swapped.
fn flip(op: BinaryOp) -> BinaryOp {
    match op {
        BinaryOp::Lt => BinaryOp::Gt,
        BinaryOp::LtEq => BinaryOp::GtEq,
        BinaryOp::Gt => BinaryOp::Lt,
        BinaryOp::GtEq => BinaryOp::LtEq,
        other => other,
    }
}

/// `x.length` / `x.size`, non-computed and not optional: the property name.
fn length_or_size(ast: &Ast, id: NodeId) -> Option<&str> {
    match *ast.node(id) {
        Node::Member {
            optional: false,
            prop,
            ..
        } => ident_name(ast, prop).filter(|n| matches!(*n, "length" | "size")),
        _ => None,
    }
}

/// What `<non-negative integer> op value` always gives, if constant.
fn constant_result(op: BinaryOp, value: f64) -> Option<bool> {
    if !value.is_finite() {
        return None;
    }
    match op {
        BinaryOp::Lt => (value <= 0.0).then_some(false),
        BinaryOp::LtEq | BinaryOp::StrictEq | BinaryOp::Eq => (value < 0.0).then_some(false),
        BinaryOp::Gt | BinaryOp::StrictNotEq | BinaryOp::NotEq => (value < 0.0).then_some(true),
        BinaryOp::GtEq => (value <= 0.0).then_some(true),
        _ => None,
    }
}

impl Rule for NoImpossibleLengthComparison {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { left, op, right } = *ast.node(node) else {
            return;
        };
        if !matches!(
            op,
            BinaryOp::Lt
                | BinaryOp::LtEq
                | BinaryOp::Gt
                | BinaryOp::GtEq
                | BinaryOp::StrictEq
                | BinaryOp::StrictNotEq
                | BinaryOp::Eq
                | BinaryOp::NotEq
        ) {
            return;
        }
        let (l, r) = (skip_wrappers(ast, left), skip_wrappers(ast, right));
        let (member, property, op, other) = if let Some(p) = length_or_size(ast, l) {
            (l, p, op, r)
        } else if let Some(p) = length_or_size(ast, r) {
            (r, p, flip(op), l)
        } else {
            return;
        };
        let Node::Member { obj, .. } = *ast.node(member) else {
            return;
        };
        if matches!(
            ast.node(skip_wrappers(ast, obj)),
            Node::This {} | Node::Super {}
        ) || has_optional_chain(ast, obj)
            || known_non_collection_length(cx, member)
            || has_shape_property_check(cx, node, member)
        {
            return;
        }
        let Some(Static::Num(value)) = static_value_control_flow(cx, other) else {
            return;
        };
        let Some(result) = constant_result(op, value) else {
            return;
        };
        cx.report_node(
            node,
            format!(
                "This comparison is always {result} because `.{property}` is always a non-negative integer."
            ),
        )
        .id("no-impossible-length-comparison");
    }
}
