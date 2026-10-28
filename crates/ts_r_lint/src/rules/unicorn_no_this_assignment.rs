//! `unicorn/no-this-assignment`: `this` assigned to a plain name (`const self = this`,
//! `self = this`) is reported on the declarator or the assignment. Destructuring, member
//! targets, `this` inside a larger expression and class fields are left alone.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-this-assignment",
        "Disallow assigning `this` to a variable.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoThisAssignment>,
};

#[derive(Debug, Default)]
struct NoThisAssignment;

impl Rule for NoThisAssignment {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDeclarator, NodeKind::Assign]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (target, value) = match *cx.node(node) {
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } => (name, init),
            Node::Assign { left, right, .. } => (skip_parens(ast, left), right),
            _ => return,
        };
        let Some(name) = ident_name(ast, target) else {
            return;
        };
        if !matches!(ast.node(skip_parens(ast, value)), Node::This {}) {
            return;
        }
        let message = format!("Do not assign `this` to `{name}`.");
        cx.report_node(node, message).id("no-this-assignment");
    }
}
