//! `guard-for-in`: require the body of a `for-in` loop to be filtered by an `if`.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "guard-for-in",
        "Require `for-in` loops to include an `if` statement",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create_default::<GuardForIn>,
};

#[derive(Debug, Default)]
struct GuardForIn;

/// `continue;` or `{ continue; }`.
fn is_continue(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::Continue { .. } => true,
        Node::Block { stmts } => {
            let s = ast.list(stmts);
            s.len() == 1 && matches!(ast.node(s[0]), Node::Continue { .. })
        }
        _ => false,
    }
}

impl Rule for GuardForIn {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ForIn]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::ForIn { body, .. } = *cx.node(node) else {
            return;
        };
        let ast = cx.ast();
        let guarded = match *ast.node(body) {
            Node::Empty {} | Node::If { .. } => true,
            Node::Block { stmts } => match ast.list(stmts) {
                [] => true,
                [only] => matches!(ast.node(*only), Node::If { .. }),
                [first, ..] => {
                    matches!(*ast.node(*first), Node::If { cons, .. } if is_continue(ast, cons))
                }
            },
            _ => false,
        };
        if !guarded {
            cx.report(
                cx.span(node),
                "The body of a for-in should be wrapped in an if statement to filter unwanted properties from the prototype.",
            )
            .id("wrap");
        }
    }
}
