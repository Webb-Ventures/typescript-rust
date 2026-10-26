//! `no-bitwise`: disallow bitwise operators.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-bitwise",
        "Disallow bitwise operators",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create::<NoBitwise>,
};

#[derive(Debug, Default)]
struct NoBitwise {
    allow: Vec<String>,
    int32_hint: bool,
}

impl FromOptions for NoBitwise {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoBitwise {
            allow: str_list(o, "allow"),
            int32_hint: bool_field(o, "int32Hint", false),
        })
    }
}

impl Rule for NoBitwise {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary, NodeKind::Assign, NodeKind::Unary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let op = match *ast.node(node) {
            Node::Binary { op, right, .. } => {
                use BinaryOp::*;
                if !matches!(op, BitXor | BitOr | BitAnd | Shl | Shr | UShr) {
                    return;
                }
                if self.int32_hint
                    && op == BitOr
                    && matches!(*ast.node(right), Node::NumberLit { value } if value.get() == 0.0)
                {
                    return;
                }
                op.as_str()
            }
            Node::Assign { op, .. } => {
                use AssignOp::*;
                if !matches!(
                    op,
                    BitXorAssign | BitOrAssign | BitAndAssign | ShlAssign | ShrAssign | UShrAssign
                ) {
                    return;
                }
                op.as_str()
            }
            Node::Unary {
                op: UnaryOp::BitNot,
                ..
            } => "~",
            _ => return,
        };
        if self.allow.iter().any(|a| a == op) {
            return;
        }
        cx.report(cx.span(node), format!("Unexpected use of '{op}'."))
            .id("unexpected");
    }
}
