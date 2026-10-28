//! `unicorn/no-global-object-property-assignment`: a property of `globalThis` / `global` /
//! `self` / `window` (the unshadowed global, through TS wrappers) with a static name is
//! written: assigned (also compound and logical), updated, destructured into or used as a
//! `for-in`/`of` head, through parentheses and TS wrappers. `delete` is allowed. The member
//! expression is reported.

use super::prelude::*;
use super::unicorn_ast::is_left_hand_side;
use super::unicorn_static::{is_global_ident, static_value};
use super::unicorn_util::skip_wrappers;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-global-object-property-assignment",
        "Disallow assigning properties on the global object.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoGlobalObjectPropertyAssignment>,
};

#[derive(Debug, Default)]
struct NoGlobalObjectPropertyAssignment;

const GLOBAL_OBJECTS: &[&str] = &["global", "globalThis", "self", "window"];

impl Rule for NoGlobalObjectPropertyAssignment {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member, NodeKind::Index]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (obj, index) = match *cx.node(node) {
            Node::Member { obj, .. } => (obj, None),
            Node::Index { obj, index, .. } => (obj, Some(index)),
            _ => return,
        };
        let obj = skip_wrappers(ast, obj);
        let Some(name) = ident_name(ast, obj) else {
            return;
        };
        if !GLOBAL_OBJECTS.contains(&name) || !is_global_ident(cx, obj, name) {
            return;
        }
        if !is_left_hand_side(cx, node) {
            return;
        }
        // The outermost wrapper: a `delete` operand is not a write here.
        let mut top = node;
        while let Some(p) = cx.parent(top)
            && matches!(
                ast.node(p),
                Node::Paren { .. }
                    | Node::As { .. }
                    | Node::Satisfies { .. }
                    | Node::NonNull { .. }
                    | Node::TypeAssertion { .. }
            )
        {
            top = p;
        }
        if let Some(p) = cx.parent(top)
            && matches!(
                ast.node(p),
                Node::Unary {
                    op: UnaryOp::Delete,
                    ..
                }
            )
        {
            return;
        }
        if let Some(index) = index
            && static_value(cx, index).is_none()
        {
            return;
        }
        cx.report_node(node, "Do not assign properties on the global object.")
            .id("no-global-object-property-assignment");
    }
}
