//! `unicorn/no-useless-continue`: an unlabeled `continue` directly in a block that is the
//! last thing its loop body does (through trailing block positions and `if` branches up
//! to the loop body; `switch` cases and `try` blocks stop the walk). The fix removes the
//! statement with [`remove_statements`], unless that would drop a comment.

use super::prelude::*;
use super::unicorn_ast::remove_statements;
use super::unicorn_text::would_remove_comments;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-continue",
        "Disallow useless `continue` statements.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoUselessContinue>,
};

#[derive(Debug, Default)]
struct NoUselessContinue;

fn is_useless(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    if !cx
        .parent(node)
        .is_some_and(|p| matches!(ast.node(p), Node::Block { .. }))
    {
        return false;
    }
    let mut cur = node;
    for _ in 0..100_000 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        match *ast.node(p) {
            Node::Block { stmts } => {
                if ast.list(stmts).last() != Some(&cur) {
                    return false;
                }
            }
            Node::If { cons, alt, .. } if cons == cur || alt == Some(cur) => {}
            Node::For { body, .. }
            | Node::ForIn { body, .. }
            | Node::ForOf { body, .. }
            | Node::While { body, .. }
            | Node::DoWhile { body, .. } => return body == cur,
            _ => return false,
        }
        cur = p;
    }
    false
}

impl Rule for NoUselessContinue {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Continue]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if !matches!(cx.node(node), Node::Continue { label: None }) || !is_useless(cx, node) {
            return;
        }
        let span = cx.span(node);
        let fix = (!would_remove_comments(cx, span, &[]))
            .then(|| remove_statements(cx, node, node, false));
        cx.report(span, "Unnecessary `continue` statement.")
            .id("no-useless-continue")
            .fix_opt(fix);
    }
}
