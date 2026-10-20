//! `require-yield`: require generator functions to contain `yield`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "require-yield",
        "Require generator functions to contain `yield`",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<RequireYield>,
};

#[derive(Debug, Default)]
struct RequireYield;

fn has_yield(ast: &ts_r_ast::Ast, id: NodeId, depth: u32) -> bool {
    if depth > 1000 {
        return true;
    }
    match ast.kind(id) {
        NodeKind::Yield => return true,
        NodeKind::FunctionDecl | NodeKind::FunctionExpr | NodeKind::Arrow => return false,
        _ => {}
    }
    let mut found = false;
    ast.for_each_child(id, |c| {
        if !found && has_yield(ast, c, depth + 1) {
            found = true;
        }
    });
    found
}

impl Rule for RequireYield {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::FunctionDecl, NodeKind::FunctionExpr]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (Node::FunctionDecl {
            flags,
            body: Some(body),
            ..
        }
        | Node::FunctionExpr {
            flags,
            body: Some(body),
            ..
        }) = *cx.node(node)
        else {
            return;
        };
        if !flags.contains(FnFlags::GENERATOR) {
            return;
        }
        if let Node::Block { stmts } = *ast.node(body)
            && ast.list(stmts).is_empty()
        {
            return;
        }
        if has_yield(ast, body, 0) {
            return;
        }
        let head = function_head_span(ast, cx.src(), cx.file.parents, node);
        cx.report(head, "This generator function does not have 'yield'.")
            .id("missingYield");
    }
}
