//! `no-async-promise-executor`: disallow an async function as a Promise executor.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-async-promise-executor",
        "Disallow using an async function as a Promise executor",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoAsyncPromiseExecutor>,
};

#[derive(Debug, Default)]
struct NoAsyncPromiseExecutor;

impl Rule for NoAsyncPromiseExecutor {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::New { callee, args, .. } = *cx.node(node) else {
            return;
        };
        if !cx.is_global_ref(callee, "Promise") {
            return;
        }
        let Some(&first) = ast.list(args).first() else {
            return;
        };
        let f = skip_parens(ast, first);
        let is_async = match *ast.node(f) {
            Node::FunctionExpr { flags, .. } | Node::Arrow { flags, .. } => {
                flags.contains(FnFlags::ASYNC)
            }
            _ => false,
        };
        if is_async {
            let start = cx.span(f).start;
            cx.report(
                Span::new(start, start + 5),
                "Promise executor functions should not be async.",
            )
            .id("async");
        }
    }
}
