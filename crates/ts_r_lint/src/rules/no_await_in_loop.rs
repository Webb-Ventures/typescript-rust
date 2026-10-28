//! `no-await-in-loop`: disallow `await` in the repeated parts of a loop.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-await-in-loop",
        "Disallow `await` inside of loops",
        NO_PRESET,
        Category::Pedantic,
    ),
    create: create_default::<NoAwaitInLoop>,
};

#[derive(Debug, Default)]
struct NoAwaitInLoop;

impl Rule for NoAwaitInLoop {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Await, NodeKind::ForOf, NodeKind::VarDecl]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let is_using = match *ast.node(node) {
            Node::Await { .. } => false,
            Node::ForOf { is_await: true, .. } => false,
            Node::VarDecl {
                kind: VarKind::AwaitUsing,
                ..
            } => true,
            _ => return,
        };
        let mut child = node;
        let mut parent = cx.parent(node);
        while let Some(p) = parent {
            let looped = match *ast.node(p) {
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. } => {
                    return;
                }
                // An async iteration loop awaits on every step anyway.
                Node::ForOf { is_await: true, .. } => return,
                Node::For {
                    test, update, body, ..
                } => test == Some(child) || update == Some(child) || body == child,
                Node::ForIn { left, body, .. } | Node::ForOf { left, body, .. } => {
                    body == child || (is_using && left == child && child == node)
                }
                Node::While { test, body } | Node::DoWhile { body, test } => {
                    test == child || body == child
                }
                _ => false,
            };
            if looped {
                cx.report(cx.span(node), "Unexpected `await` inside a loop.")
                    .id("unexpectedAwait");
                return;
            }
            child = p;
            parent = cx.parent(p);
        }
    }
}
