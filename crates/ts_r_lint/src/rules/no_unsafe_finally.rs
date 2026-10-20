//! `no-unsafe-finally`: disallow control flow statements in `finally` blocks.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-unsafe-finally",
        "Disallow control flow statements in `finally` blocks",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoUnsafeFinally>,
};

#[derive(Debug, Default)]
struct NoUnsafeFinally;

impl Rule for NoUnsafeFinally {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Return,
            NodeKind::Throw,
            NodeKind::Break,
            NodeKind::Continue,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let kind = ast.kind(node);
        let label = match *ast.node(node) {
            Node::Break { label } | Node::Continue { label } => {
                label.and_then(|l| ident_name(ast, l))
            }
            _ => None,
        };
        let mut cur = node;
        // For unlabeled break/continue: a loop (or switch, for break) inside the finally
        // block is the target.
        while let Some(p) = cx.parent(cur) {
            let pk = ast.kind(p);
            match pk {
                NodeKind::FunctionDecl
                | NodeKind::FunctionExpr
                | NodeKind::Arrow
                | NodeKind::ClassDecl
                | NodeKind::ClassExpr
                | NodeKind::StaticBlock => return,
                NodeKind::For
                | NodeKind::ForIn
                | NodeKind::ForOf
                | NodeKind::While
                | NodeKind::DoWhile
                    if label.is_none() && matches!(kind, NodeKind::Break | NodeKind::Continue) =>
                {
                    return;
                }
                NodeKind::Switch if label.is_none() && kind == NodeKind::Break => return,
                NodeKind::Labeled => {
                    if let (Some(l), Node::Labeled { label: lab, .. }) = (label, ast.node(p))
                        && ident_name(ast, *lab) == Some(l)
                    {
                        return;
                    }
                }
                NodeKind::Try => {
                    if let Node::Try {
                        finalizer: Some(f), ..
                    } = *ast.node(p)
                        && f == cur
                    {
                        let what = match kind {
                            NodeKind::Return => "ReturnStatement",
                            NodeKind::Throw => "ThrowStatement",
                            NodeKind::Break => "BreakStatement",
                            _ => "ContinueStatement",
                        };
                        cx.report_node(node, format!("Unsafe usage of {what}."))
                            .id("unsafeUsage");
                        return;
                    }
                }
                _ => {}
            }
            cur = p;
        }
    }
}
