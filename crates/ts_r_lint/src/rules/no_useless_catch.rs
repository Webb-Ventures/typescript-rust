//! `no-useless-catch`: disallow unnecessary `catch` clauses.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-useless-catch",
        "Disallow unnecessary `catch` clauses",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoUselessCatch>,
};

#[derive(Debug, Default)]
struct NoUselessCatch;

impl Rule for NoUselessCatch {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::CatchClause]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::CatchClause {
            param: Some(param),
            body,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let Some(pname) = ident_name(ast, param) else {
            return;
        };
        let Node::Block { stmts } = *ast.node(body) else {
            return;
        };
        let stmts = ast.list(stmts);
        if stmts.len() != 1 {
            return;
        }
        let Node::Throw { arg } = *ast.node(stmts[0]) else {
            return;
        };
        if ident_name(ast, arg) != Some(pname) {
            return;
        }
        let Some(try_node) = cx.parent(node) else {
            return;
        };
        let Node::Try { finalizer, .. } = *ast.node(try_node) else {
            return;
        };
        if finalizer.is_some() {
            cx.report_node(node, "Unnecessary catch clause.")
                .id("unnecessaryCatchClause");
        } else {
            cx.report_node(try_node, "Unnecessary try/catch wrapper.")
                .id("unnecessaryCatch");
        }
    }
}
