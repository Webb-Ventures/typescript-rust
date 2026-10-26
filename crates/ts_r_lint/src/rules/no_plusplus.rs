//! `no-plusplus`: disallow `++` and `--`.

use super::prelude::*;
use ts_r_ast::{Ast, ParentMap};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-plusplus",
        "Disallow the unary operators `++` and `--`",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create::<NoPlusplus>,
};

#[derive(Debug, Default)]
struct NoPlusplus {
    allow_for_loop_afterthoughts: bool,
}

impl FromOptions for NoPlusplus {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoPlusplus {
            allow_for_loop_afterthoughts: bool_field(
                obj_opt(opts, 0),
                "allowForLoopAfterthoughts",
                false,
            ),
        })
    }
}

/// True when `id` is a `for` loop's update, directly or as an element of comma sequences
/// that are the update.
fn in_for_update(ast: &Ast, parents: &ParentMap, mut id: NodeId) -> bool {
    loop {
        let Some(p) = parent_skip_parens(ast, parents, id) else {
            return false;
        };
        match *ast.node(p) {
            // Every element of a comma sequence is part of the afterthought.
            Node::Seq { .. } => id = p,
            Node::For {
                update: Some(u), ..
            } => return skip_parens(ast, u) == id,
            _ => return false,
        }
    }
}

impl Rule for NoPlusplus {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Update]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Update { op, .. } = *cx.node(node) else {
            return;
        };
        if self.allow_for_loop_afterthoughts && in_for_update(cx.ast(), cx.file.parents, node) {
            return;
        }
        cx.report(
            cx.span(node),
            format!("Unary operator '{}' used.", op.as_str()),
        )
        .id("unexpectedUnaryOp");
    }
}
