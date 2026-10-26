//! `no-new-object` (deprecated alias of `no-object-constructor`'s `new` case): disallow
//! `new Object()`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-new-object",
        "Disallow `Object` constructors",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoNewObject>,
};

#[derive(Debug, Default)]
struct NoNewObject;

impl Rule for NoNewObject {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::New { callee, .. } = *cx.node(node) else {
            return;
        };
        if cx.is_global_ref(callee, "Object") {
            cx.report(
                cx.span(node),
                "The object literal notation {} is preferable.",
            )
            .id("preferLiteral");
        }
    }
}
