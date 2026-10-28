//! `no-new`: disallow `new` expressions whose result is thrown away.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-new",
        "Disallow `new` operators outside of assignments or comparisons",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoNew>,
};

#[derive(Debug, Default)]
struct NoNew;

impl Rule for NoNew {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ExprStmt]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::ExprStmt { expr } = *cx.node(node) else {
            return;
        };
        if matches!(cx.node(skip_parens(cx.ast(), expr)), Node::New { .. }) {
            cx.report(cx.span(node), "Do not use 'new' for side effects.")
                .id("noNewStatement");
        }
    }
}
