//! `no-compare-neg-zero`: disallow comparing against `-0`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-compare-neg-zero",
        "Disallow comparing against -0",
        ESLINT_REC,
        Category::Correctness,
    )
    .suggestions(),
    create: create_default::<NoCompareNegZero>,
};

#[derive(Debug, Default)]
struct NoCompareNegZero;

fn is_neg_zero(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } => {
            matches!(*ast.node(arg), Node::NumberLit { value } if value.get() == 0.0)
        }
        _ => false,
    }
}

impl Rule for NoCompareNegZero {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { left, op, right } = *cx.node(node) else {
            return;
        };
        if !matches!(
            op,
            BinaryOp::Eq
                | BinaryOp::NotEq
                | BinaryOp::StrictEq
                | BinaryOp::StrictNotEq
                | BinaryOp::Lt
                | BinaryOp::LtEq
                | BinaryOp::Gt
                | BinaryOp::GtEq
        ) {
            return;
        }
        let (l, r) = (skip_parens(ast, left), skip_parens(ast, right));
        let neg = if is_neg_zero(ast, l) {
            l
        } else if is_neg_zero(ast, r) {
            r
        } else {
            return;
        };
        let msg = format!(
            "Do not use the '{}' operator to compare against -0.",
            op.as_str()
        );
        let operand = |id: NodeId| -> String {
            let t = ast.span(id).text(cx.src());
            if matches!(ast.node(id), Node::Seq { .. }) {
                format!("({t})")
            } else {
                t.to_owned()
            }
        };
        let lt = operand(l);
        let rt = operand(r);
        let mut remove = Fix::replace(ast.span(neg), "0");
        if neg == l && is_neg_zero(ast, r) {
            remove = remove.and(ast.span(r), "0");
        }
        let whole = ast.span(node);
        let comments_outside = ast
            .comments_in(whole)
            .iter()
            .any(|c| !ast.span(l).contains_span(c.span) && !ast.span(r).contains_span(c.span));
        let object_shadowed = !cx.global_available(node, "Object");
        let object_is = match op {
            BinaryOp::StrictEq if !comments_outside && !object_shadowed => Some((
                "suggestObjectIs",
                "Replace with 'Object.is()' (changes the comparison to distinguish -0 from +0).",
                format!("Object.is({lt}, {rt})"),
            )),
            BinaryOp::StrictNotEq if !comments_outside && !object_shadowed => Some((
                "suggestNotObjectIs",
                "Replace with '!Object.is()' (changes the comparison to distinguish -0 from +0).",
                format!("!Object.is({lt}, {rt})"),
            )),
            _ => None,
        };
        let mut rep = cx.report_node(node, msg).id("unexpected").suggest(
            "suggestRemoveMinus",
            "Replace '-0' with '0' (keeps the current comparison behavior).",
            remove,
        );
        if let Some((id, desc, text)) = object_is {
            rep = rep.suggest(id, desc, Fix::replace(whole, text));
        }
        let _ = rep;
    }
}
