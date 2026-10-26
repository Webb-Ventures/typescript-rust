//! `no-void`: disallow the `void` operator.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-void",
        "Disallow `void` operators",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create::<NoVoid>,
};

#[derive(Debug, Default)]
struct NoVoid {
    allow_as_statement: bool,
}

impl FromOptions for NoVoid {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoVoid {
            allow_as_statement: bool_field(obj_opt(opts, 0), "allowAsStatement", false),
        })
    }
}

impl Rule for NoVoid {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Unary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Unary {
            op: UnaryOp::Void, ..
        } = *cx.node(node)
        else {
            return;
        };
        if self.allow_as_statement
            && parent_skip_parens(cx.ast(), cx.file.parents, node)
                .is_some_and(|p| matches!(cx.node(p), Node::ExprStmt { .. }))
        {
            return;
        }
        cx.report(
            cx.span(node),
            "Expected 'undefined' and instead saw 'void'.",
        )
        .id("noVoid");
    }
}
