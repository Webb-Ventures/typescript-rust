//! `prefer-spread`: require spread operators instead of `.apply()`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "prefer-spread",
        "Require spread operators instead of `.apply()`",
        TS_ESLINT_REC,
        Category::Style,
    ),
    create: create_default::<PreferSpread>,
};

#[derive(Debug, Default)]
struct PreferSpread;

fn is_nullish(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::NullLit {} => true,
        Node::Ident { .. } => is_undefined(ast, id),
        Node::Unary {
            op: UnaryOp::Void, ..
        } => true,
        _ => false,
    }
}

impl Rule for PreferSpread {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::Call { callee, args, .. } = *cx.node(node) else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let Node::Member {
            obj: target, prop, ..
        } = *ast.node(callee)
        else {
            return;
        };
        if ident_name(ast, prop) != Some("apply") {
            return;
        }
        let [this_arg, list] = ast.list(args) else {
            return;
        };
        if matches!(ast.node(*list), Node::ArrayLit { .. } | Node::Spread { .. })
            || matches!(ast.node(*this_arg), Node::Spread { .. })
        {
            return;
        }
        let target = skip_parens(ast, target);
        let ok = match *ast.node(target) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => {
                same_tokens(ast, src, skip_parens(ast, obj), skip_parens(ast, *this_arg))
            }
            _ => is_nullish(ast, *this_arg),
        };
        if ok {
            cx.report_node(node, "Use the spread operator instead of '.apply()'.")
                .id("preferSpread");
        }
    }
}
