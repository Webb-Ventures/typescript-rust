//! `no-proto`: disallow the `__proto__` property.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-proto",
        "Disallow the use of the `__proto__` property",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create_default::<NoProto>,
};

#[derive(Debug, Default)]
struct NoProto;

impl Rule for NoProto {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member, NodeKind::Index]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if member_name(cx.ast(), cx.src(), node).as_deref() == Some("__proto__") {
            cx.report(cx.span(node), "The '__proto__' property is deprecated.")
                .id("unexpectedProto");
        }
    }
}
