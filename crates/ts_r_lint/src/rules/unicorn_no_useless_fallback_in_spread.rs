//! `unicorn/no-useless-fallback-in-spread`: `{...(foo || {})}` and `{...(foo ?? {})}` in
//! object literals: spreading a nullish value already adds nothing. The fix removes the
//! operator and the empty object (and the parentheses around the logical expression), unless
//! a comment would go with them.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-fallback-in-spread",
        "Disallow useless fallback when spreading in object literals.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoUselessFallbackInSpread>,
};

#[derive(Debug, Default)]
struct NoUselessFallbackInSpread;

impl Rule for NoUselessFallbackInSpread {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ObjectLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let parents = cx.file.parents;
        let Node::ObjectLit { props } = *cx.node(node) else {
            return;
        };
        if !ast.list(props).is_empty() {
            return;
        }
        let Some(logical) = parent_skip_parens(ast, parents, node) else {
            return;
        };
        let Node::Binary {
            left,
            op: BinaryOp::LogicalOr | BinaryOp::Coalesce,
            right,
        } = *ast.node(logical)
        else {
            return;
        };
        if skip_parens(ast, right) != node {
            return;
        }
        // The parentheses around the logical expression, innermost first.
        let mut parens = Vec::new();
        let mut top = logical;
        while let Some(p) = cx.parent(top)
            && matches!(ast.node(p), Node::Paren { .. })
        {
            parens.push(p);
            top = p;
        }
        let Some(spread) = cx.parent(top) else {
            return;
        };
        if !matches!(ast.node(spread), Node::Spread { .. })
            || !cx
                .parent(spread)
                .is_some_and(|o| matches!(ast.node(o), Node::ObjectLit { .. }))
        {
            return;
        }
        let start = cx.span(left).end;
        let end = cx.span(logical).end;
        let fix = ast.comments_in(Span::new(start, end)).is_empty().then(|| {
            let mut fix = Fix::delete(Span::new(start, end));
            let left_parenthesized = matches!(ast.node(left), Node::Paren { .. });
            if left_parenthesized || !matches!(ast.node(left), Node::Seq { .. }) {
                for &p in &parens {
                    let s = cx.span(p);
                    fix = fix
                        .and(Span::new(s.start, s.start + 1), "")
                        .and(Span::new(s.end - 1, s.end), "");
                }
            }
            fix
        });
        cx.report_node(node, "The empty object is useless.")
            .id("no-useless-fallback-in-spread")
            .fix_opt(fix);
    }
}
