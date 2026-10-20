//! `no-with`: disallow `with` statements.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-with",
        "Disallow `with` statements",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoWith>,
};

#[derive(Debug, Default)]
struct NoWith;

impl Rule for NoWith {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::With]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let s = cx.span(node).start;
        cx.report(Span::new(s, s + 4), "Unexpected use of 'with' statement.")
            .id("unexpectedWith");
    }
}
