//! `unicorn/empty-brace-spaces`: no whitespace-only gap between the braces of an empty
//! block, class body, static block or object literal. The gap is reported and removed;
//! braces holding a comment are left alone.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/empty-brace-spaces",
        "Enforce no spaces between braces.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<EmptyBraceSpaces>,
};

#[derive(Debug, Default)]
struct EmptyBraceSpaces;

impl Rule for EmptyBraceSpaces {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Block,
            NodeKind::StaticBlock,
            NodeKind::ObjectLit,
            NodeKind::ClassDecl,
            NodeKind::ClassExpr,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let children = match *cx.node(node) {
            Node::Block { stmts } => stmts,
            Node::StaticBlock { body } => body,
            Node::ObjectLit { props } => props,
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
            _ => return,
        };
        if !ast.list(children).is_empty() {
            return;
        }
        let src = cx.src();
        let span = cx.span(node);
        let Some(close) = span.end.checked_sub(1) else {
            return;
        };
        if src.as_bytes().get(close as usize) != Some(&b'}') {
            return;
        }
        let inside = src[span.start as usize..close as usize].trim_end();
        let open = span.start + inside.len() as u32;
        if !inside.ends_with('{') || open == close {
            return;
        }
        let gap = Span::new(open, close);
        cx.report(gap, "Do not add spaces between braces.")
            .id("empty-brace-spaces")
            .fix(Fix::delete(gap));
    }
}
