//! `no-useless-call`: disallow `.call()` / `.apply()` that pass the receiver the call
//! would get anyway.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-useless-call",
        "Disallow unnecessary calls to `.call()` and `.apply()`",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoUselessCall>,
};

#[derive(Debug, Default)]
struct NoUselessCall;

fn is_nullish(ast: &Ast, id: NodeId) -> bool {
    let id = skip_parens(ast, id);
    matches!(ast.node(id), Node::NullLit {})
        || is_undefined(ast, id)
        || matches!(
            ast.node(id),
            Node::Unary {
                op: UnaryOp::Void,
                ..
            }
        )
}

impl Rule for NoUselessCall {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *ast.node(node) else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let Node::Member { obj, prop, .. } = *ast.node(callee) else {
            return;
        };
        let args = ast.list(args);
        let method = ident_name(ast, prop);
        let applies = match method {
            Some("call") => !args.is_empty(),
            Some("apply") => args.len() == 2 && matches!(ast.node(args[1]), Node::ArrayLit { .. }),
            _ => false,
        };
        if !applies
            || args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return;
        }
        let applied = skip_parens(ast, obj);
        let this_arg = args[0];
        let useless = match *ast.node(applied) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => {
                let expected = skip_parens(ast, obj);
                let actual = skip_parens(ast, this_arg);
                same_tokens(ast, cx.src(), expected, actual)
            }
            _ => is_nullish(ast, this_arg),
        };
        if useless {
            let name = method.unwrap_or_default();
            cx.report(cx.span(node), format!("Unnecessary '.{name}()'."))
                .id("unnecessaryCall");
        }
    }
}
