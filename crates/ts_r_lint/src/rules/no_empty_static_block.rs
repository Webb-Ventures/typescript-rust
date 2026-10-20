//! `no-empty-static-block`: disallow empty static blocks.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-empty-static-block",
        "Disallow empty static blocks",
        ESLINT_REC,
        Category::Correctness,
    )
    .suggestions(),
    create: create_default::<NoEmptyStaticBlock>,
};

#[derive(Debug, Default)]
struct NoEmptyStaticBlock;

impl Rule for NoEmptyStaticBlock {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::StaticBlock]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::StaticBlock { body } = *cx.node(node) else {
            return;
        };
        if !cx.ast().list(body).is_empty() {
            return;
        }
        let span = cx.span(node);
        let Some(open) = find_char(cx.src(), span.start, b'{') else {
            return;
        };
        let braces = Span::new(open, span.end);
        if !cx.ast().comments_in(braces).is_empty() {
            return;
        }
        cx.report(braces, "Unexpected empty static block.")
            .id("unexpected")
            .suggest(
                "suggestComment",
                "Add comment inside empty static block.",
                Fix::replace(braces, "{ /* empty */ }"),
            );
    }
}
