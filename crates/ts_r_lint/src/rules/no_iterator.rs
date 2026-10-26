//! `no-iterator`: disallow the `__iterator__` property.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-iterator",
        "Disallow the use of the `__iterator__` property",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create_default::<NoIterator>,
};

#[derive(Debug, Default)]
struct NoIterator;

impl Rule for NoIterator {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member, NodeKind::Index]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if member_name(cx.ast(), cx.src(), node).as_deref() == Some("__iterator__") {
            cx.report(cx.span(node), "Reserved name '__iterator__'.")
                .id("noIterator");
        }
    }
}
