//! `unicorn/no-constant-zero-expression`: `x * 0`, `0 * x`, `x & 0`, `0 & x` and `0 / x`
//! (the literal `0`, parentheses allowed) always give `0`, `-0` or `NaN`. When the whole
//! expression's static value is exactly `+0` and it holds no comment, a suggestion
//! replaces it with `0`.

use super::prelude::*;
use super::unicorn_static::{Static, static_value_control_flow};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-constant-zero-expression",
        "Disallow arithmetic and bitwise operations that always evaluate to `0`.",
        NO_PRESET,
        Category::Suspicious,
    )
    .suggestions(),
    create: create_default::<NoConstantZeroExpression>,
};

#[derive(Debug, Default)]
struct NoConstantZeroExpression;

fn is_zero(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::NumberLit { value } if value.get() == 0.0)
}

impl Rule for NoConstantZeroExpression {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { left, op, right } = *cx.node(node) else {
            return;
        };
        let erased = match op {
            BinaryOp::Mul | BinaryOp::BitAnd => is_zero(ast, left) || is_zero(ast, right),
            BinaryOp::Div => is_zero(ast, left),
            _ => false,
        };
        if !erased {
            return;
        }
        let span = cx.span(node);
        let plus_zero = matches!(static_value_control_flow(cx, node), Some(Static::Num(v)) if v == 0.0 && v.is_sign_positive());
        let suggest = plus_zero && ast.comments_in(span).is_empty();
        let report = cx
            .report(
                span,
                "This operation always evaluates to `0` (or `-0`/`NaN`), which is likely a mistake.",
            )
            .id("no-constant-zero-expression/error");
        if suggest {
            report.suggest(
                "no-constant-zero-expression/suggestion",
                "Replace this expression with `0`.",
                Fix::replace(span, "0"),
            );
        }
    }
}
