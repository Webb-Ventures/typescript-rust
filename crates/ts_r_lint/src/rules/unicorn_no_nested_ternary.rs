//! `unicorn/no-nested-ternary`: one level of ternary nesting is allowed when the inner
//! ternary is parenthesized; deeper nesting is not.
//!
//! Only innermost ternaries (no ternary as a direct operand) are checked. Their nesting level
//! is the run of ternary ancestors directly above them, parentheses looked through. At level
//! 1 an unparenthesized ternary is reported and wrapped in `(...)`; from level 2 the report
//! (no fix) goes on the ternary two levels below the outermost one of the run.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-nested-ternary",
        "Disallow nested ternary expressions.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoNestedTernary>,
};

#[derive(Debug, Default)]
struct NoNestedTernary;

impl Rule for NoNestedTernary {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Conditional]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Conditional { test, cons, alt } = *cx.node(node) else {
            return;
        };
        if [test, cons, alt]
            .iter()
            .any(|&n| matches!(ast.node(skip_parens(ast, n)), Node::Conditional { .. }))
        {
            return;
        }
        // The ternaries above `node`, nearest first.
        let mut chain = Vec::new();
        let mut cur = node;
        while let Some(p) = parent_skip_parens(ast, cx.file.parents, cur) {
            if !matches!(ast.node(p), Node::Conditional { .. }) {
                break;
            }
            chain.push(p);
            cur = p;
        }
        match chain.len() {
            0 => {}
            1 => {
                let parenthesized = cx
                    .parent(node)
                    .is_some_and(|p| matches!(cx.node(p), Node::Paren { .. }));
                if !parenthesized {
                    let span = cx.span(node);
                    cx.report(span, "Nested ternary expression should be parenthesized.")
                        .id("should-parenthesized")
                        .fix(Fix::insert(span.start, "(").and(Span::at(span.end), ")"));
                }
            }
            n => {
                let target = if n > 2 { chain[n - 3] } else { node };
                cx.report_node(target, "Do not nest ternary expressions.")
                    .id("too-deep");
            }
        }
    }
}
