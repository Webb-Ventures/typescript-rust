//! `no-new-func`: disallow creating functions with the `Function` constructor.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-new-func",
        "Disallow `new` operators with the `Function` object",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create_default::<NoNewFunc>,
};

#[derive(Debug, Default)]
struct NoNewFunc;

impl Rule for NoNewFunc {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Ident]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if !cx.is_global_ref(node, "Function") {
            return;
        }
        let ast = cx.ast();
        let parents = cx.file.parents;
        let Some(p) = parent_skip_parens(ast, parents, node) else {
            return;
        };
        let target = match *ast.node(p) {
            Node::New { callee, .. } | Node::Call { callee, .. }
                if skip_parens(ast, callee) == node =>
            {
                Some(p)
            }
            Node::Member { obj, .. } | Node::Index { obj, .. }
                if skip_parens(ast, obj) == node
                    && member_name(ast, cx.src(), p)
                        .is_some_and(|n| matches!(&*n, "call" | "apply" | "bind")) =>
            {
                parent_skip_parens(ast, parents, p).filter(|&c| {
                    matches!(*ast.node(c), Node::Call { callee, .. } if skip_parens(ast, callee) == p)
                })
            }
            _ => None,
        };
        if let Some(t) = target {
            cx.report(cx.span(t), "The Function constructor is eval.")
                .id("noFunctionConstructor");
        }
    }
}
