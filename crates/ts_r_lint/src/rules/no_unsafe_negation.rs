//! `no-unsafe-negation`: disallow negating the left operand of relational operators.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-unsafe-negation",
        "Disallow negating the left operand of relational operators",
        ESLINT_REC,
        Category::Correctness,
    )
    .suggestions(),
    create: create::<NoUnsafeNegation>,
};

#[derive(Debug, Default)]
struct NoUnsafeNegation {
    ordering: bool,
}

impl FromOptions for NoUnsafeNegation {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoUnsafeNegation {
            ordering: bool_field(obj_opt(opts, 0), "enforceForOrderingRelations", false),
        })
    }
}

impl Rule for NoUnsafeNegation {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { left, op, .. } = *cx.node(node) else {
            return;
        };
        let relevant = match op {
            BinaryOp::In | BinaryOp::InstanceOf => true,
            BinaryOp::Lt | BinaryOp::LtEq | BinaryOp::Gt | BinaryOp::GtEq => self.ordering,
            _ => false,
        };
        if !relevant {
            return;
        }
        let Node::Unary {
            op: UnaryOp::Not,
            arg,
        } = *ast.node(left)
        else {
            return;
        };
        let src = cx.src();
        let whole = ast.span(node);
        let neg = ast.span(left);
        let _ = arg;
        let inner = &src[neg.start as usize + 1..whole.end as usize];
        let ops = op.as_str();
        cx.report(neg, format!("Unexpected negating the left operand of '{ops}' operator."))
            .id("unexpected")
            .suggest(
                "suggestNegatedExpression",
                format!(
                    "Negate '{ops}' expression instead of its left operand. This changes the current behavior."
                ),
                Fix::replace(whole, format!("!({inner})")),
            )
            .suggest(
                "suggestParenthesisedNegation",
                "Wrap negation in '()' to make the intention explicit. This preserves the current behavior.",
                Fix::replace(neg, format!("({})", neg.text(src))),
            );
    }
}
