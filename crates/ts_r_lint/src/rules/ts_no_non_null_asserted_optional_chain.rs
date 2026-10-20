//! `@typescript-eslint/no-non-null-asserted-optional-chain`: disallow non-null assertions
//! after an optional chain expression.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-non-null-asserted-optional-chain",
        "Disallow non-null assertions after an optional chain expression",
        TS_REC,
        Category::Correctness,
    )
    .suggestions(),
    create: create_default::<NoNonNullOptionalChain>,
};

#[derive(Debug, Default)]
struct NoNonNullOptionalChain;

impl Rule for NoNonNullOptionalChain {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::NonNull]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::NonNull { expr } = *cx.node(node) else {
            return;
        };
        let report_on = if matches!(cx.node(expr), Node::Paren { .. }) {
            // `(a?.b)!`: the parenthesized chain ends there.
            let inner = skip_parens(ast, expr);
            if !chain_has_optional(ast, inner) {
                return;
            }
            inner
        } else {
            if !chain_has_optional(ast, expr) {
                return;
            }
            // `a?.b!.c` keeps the assertion inside the chain.
            let continues = cx.parent(node).is_some_and(|p| match *cx.node(p) {
                Node::Member { obj, .. } | Node::Index { obj, .. } => obj == node,
                Node::Call { callee, .. } => callee == node,
                _ => false,
            });
            if continues {
                return;
            }
            node
        };
        let end = cx.span(node).end;
        cx.report_node(
            report_on,
            "Optional chain expressions can return undefined by design - using a non-null assertion is unsafe and wrong.",
        )
        .id("noNonNullOptionalChain")
        .suggest(
            "suggestRemovingNonNull",
            "You should remove the non-null assertion.",
            Fix::delete(Span::new(end - 1, end)),
        );
    }
}
