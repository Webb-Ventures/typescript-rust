//! `unicorn/no-unreadable-new-expression`: a member access straight off a `new` expression
//! (`new Foo().bar`, `(new Foo)[x]`, reported on the property), and a `new` whose
//! constructor is not an identifier or a chain of plain `.name` members on one (reported on
//! the constructor). Parentheses are looked through, as in ESTree.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unreadable-new-expression",
        "Disallow unreadable `new` expressions.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoUnreadableNewExpression>,
};

#[derive(Debug, Default)]
struct NoUnreadableNewExpression;

fn is_simple_constructor(ast: &Ast, mut id: NodeId) -> bool {
    // Each step moves to the member's object: the walk ends.
    loop {
        id = skip_parens(ast, id);
        match *ast.node(id) {
            Node::Ident { .. } => return true,
            Node::Member { obj, prop, .. } if matches!(ast.node(prop), Node::Ident { .. }) => {
                id = obj;
            }
            _ => return false,
        }
    }
}

impl Rule for NoUnreadableNewExpression {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member, NodeKind::Index, NodeKind::New]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::Member { obj, prop, .. }
            | Node::Index {
                obj, index: prop, ..
            } => {
                if matches!(ast.node(skip_parens(ast, obj)), Node::New { .. }) {
                    let prop = skip_parens(ast, prop);
                    cx.report_node(
                        prop,
                        "Do not access members directly from a `new` expression.",
                    )
                    .id("member-access");
                }
            }
            Node::New { callee, .. } => {
                if !is_simple_constructor(ast, callee) {
                    let callee = skip_parens(ast, callee);
                    cx.report_node(callee, "Do not use a complex expression as a constructor.")
                        .id("complex-constructor");
                }
            }
            _ => {}
        }
    }
}
