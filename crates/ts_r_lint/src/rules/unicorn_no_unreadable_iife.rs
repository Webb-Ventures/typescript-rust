//! `unicorn/no-unreadable-iife`: an immediately called arrow whose expression body is
//! wrapped in parentheses (`(() => (a ? b : c))()`) is reported at the parenthesized body.
//! The suggestion turns the body into `{ return <body>; }`, unless a comment inside the
//! parentheses but outside the body would be lost.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unreadable-iife",
        "Disallow unreadable IIFEs.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<NoUnreadableIife>,
};

#[derive(Debug, Default)]
struct NoUnreadableIife;

impl Rule for NoUnreadableIife {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, .. } = *cx.node(node) else {
            return;
        };
        let Node::Arrow { flags, body, .. } = *ast.node(skip_parens(ast, callee)) else {
            return;
        };
        if !flags.contains(FnFlags::EXPRESSION_BODY)
            || !matches!(ast.node(body), Node::Paren { .. })
        {
            return;
        }
        let range = cx.span(body);
        let inner = cx.span(skip_parens(ast, body));
        let loses_comment = ast
            .comments_in(range)
            .iter()
            .any(|c| c.span.start < inner.start || c.span.end > inner.end);
        let text = format!("{{ return {}; }}", inner.text(cx.src()));
        let report = cx
            .report(
                range,
                "IIFE with parenthesized arrow function body is considered unreadable.",
            )
            .id("no-unreadable-iife");
        if !loses_comment {
            report.suggest(
                "suggestion",
                "Use a block statement body.",
                Fix::replace(range, text),
            );
        }
    }
}
