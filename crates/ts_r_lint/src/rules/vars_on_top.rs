//! `vars-on-top`: `var` declarations come first in their function, static block or program.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "vars-on-top",
        "Require `var` declarations be placed at the top of their containing scope",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<VarsOnTop>,
};

#[derive(Debug, Default)]
struct VarsOnTop;

/// `node` is in the leading run of declarations of `stmts` (after directives and imports,
/// except in static blocks).
fn on_top(cx: &Ctx<'_, '_>, node: NodeId, stmts: &[NodeId], static_block: bool) -> bool {
    let mut i = 0;
    if !static_block {
        while i < stmts.len() {
            let directive = match *cx.node(stmts[i]) {
                Node::Directive { .. } | Node::ImportDecl { .. } => true,
                Node::ExprStmt { expr } => matches!(cx.node(expr), Node::StringLit { .. }),
                _ => false,
            };
            if !directive {
                break;
            }
            i += 1;
        }
    }
    for &s in &stmts[i..] {
        if !matches!(cx.node(s), Node::VarDecl { .. }) {
            return false;
        }
        if s == node {
            return true;
        }
    }
    false
}

impl Rule for VarsOnTop {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDecl]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if !matches!(
            cx.node(node),
            Node::VarDecl {
                kind: VarKind::Var,
                ..
            }
        ) {
            return;
        }
        let ast = cx.ast();
        let Some(p) = cx.parent(node) else { return };
        let ok = match *cx.node(p) {
            Node::Program { stmts } => on_top(cx, node, ast.list(stmts), false),
            Node::Block { stmts } => {
                cx.parent(p).is_some_and(|f| {
                    matches!(
                        cx.node(f),
                        Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
                    )
                }) && on_top(cx, node, ast.list(stmts), false)
            }
            Node::StaticBlock { body } => on_top(cx, node, ast.list(body), true),
            _ => false,
        };
        if !ok {
            cx.report_node(
                node,
                "All 'var' declarations must be at the top of the function scope.",
            )
            .id("top");
        }
    }
}
