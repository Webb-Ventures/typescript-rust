//! `no-octal`: disallow octal literals.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-octal",
        "Disallow octal literals",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoOctal>,
};

#[derive(Debug, Default)]
struct NoOctal;

impl Rule for NoOctal {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::NumberLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let b = cx.text(node).as_bytes();
        if b.len() >= 2 && b[0] == b'0' && b[1].is_ascii_digit() {
            cx.report_node(node, "Octal literals should not be used.")
                .id("noOctal");
        }
    }
}
