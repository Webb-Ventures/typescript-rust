//! `no-lone-blocks`: disallow blocks that neither belong to a statement nor scope a
//! declaration.

use super::prelude::*;
use super::this_binding::is_strict;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-lone-blocks",
        "Disallow unnecessary nested blocks",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoLoneBlocks>,
};

#[derive(Debug, Default)]
struct NoLoneBlocks;

impl NoLoneBlocks {
    /// Does the block directly declare something block-scoped (`let`, `const`, a class, or a
    /// function in strict code)?
    fn scopes_a_binding(cx: &Ctx<'_, '_>, stmts: &[NodeId]) -> bool {
        let ast = cx.ast();
        stmts.iter().any(|&s| match *ast.node(s) {
            Node::VarDecl { kind, .. } => kind != VarKind::Var,
            Node::ClassDecl { .. } => true,
            Node::FunctionDecl { .. } => is_strict(cx, s),
            _ => false,
        })
    }
}

impl Rule for NoLoneBlocks {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Block]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Block { stmts } = *ast.node(node) else {
            return;
        };
        let Some(parent) = cx.parent(node) else {
            return;
        };
        let (lone, nested, only_child) = match *ast.node(parent) {
            Node::Block { stmts } | Node::StaticBlock { body: stmts } => {
                (true, true, ast.list(stmts).len() == 1)
            }
            Node::Program { .. } => (true, false, false),
            Node::SwitchCase { body, .. } => (ast.list(body) != [node], false, false),
            _ => (false, false, false),
        };
        let redundant = if crate::globals::normalize_ecma_version(cx.env().ecma_version) < 2015 {
            lone
        } else {
            (lone && !Self::scopes_a_binding(cx, ast.list(stmts))) || only_child
        };
        if !redundant {
            return;
        }
        let (msg, id) = if nested {
            ("Nested block is redundant.", "redundantNestedBlock")
        } else {
            ("Block is redundant.", "redundantBlock")
        };
        cx.report(cx.span(node), msg).id(id);
    }
}
