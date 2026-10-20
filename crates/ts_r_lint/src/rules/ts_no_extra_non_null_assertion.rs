//! `@typescript-eslint/no-extra-non-null-assertion`: disallow extra non-null assertions.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-extra-non-null-assertion",
        "Disallow extra non-null assertions",
        TS_REC,
        Category::Correctness,
    )
    .fixable(),
    create: create_default::<NoExtraNonNull>,
};

#[derive(Debug, Default)]
struct NoExtraNonNull;

impl Rule for NoExtraNonNull {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::NonNull]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(p) = parent_skip_parens(ast, cx.file.parents, node) else {
            return;
        };
        let inner = |e: NodeId| skip_parens(ast, e) == node;
        let extra = match *cx.node(p) {
            Node::NonNull { expr } => inner(expr),
            Node::Member {
                obj,
                optional: true,
                ..
            }
            | Node::Index {
                obj,
                optional: true,
                ..
            } => inner(obj),
            Node::Call {
                callee,
                optional: true,
                ..
            } => inner(callee),
            _ => false,
        };
        if !extra {
            return;
        }
        let span = cx.span(node);
        let bang = Span::new(span.end - 1, span.end);
        cx.report(span, "Forbidden extra non-null assertion.")
            .id("noExtraNonNullAssertion")
            .fix(Fix::delete(bang));
    }
}
