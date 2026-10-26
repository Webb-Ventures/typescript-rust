//! `default-case-last`: require the `default` clause to be the last in a `switch`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "default-case-last",
        "Enforce `default` clauses in `switch` statements to be last",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<DefaultCaseLast>,
};

#[derive(Debug, Default)]
struct DefaultCaseLast;

impl Rule for DefaultCaseLast {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Switch]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Switch { cases, .. } = *cx.node(node) else {
            return;
        };
        let cases = cx.ast().list(cases);
        let Some((_, init)) = cases.split_last() else {
            return;
        };
        let default = init
            .iter()
            .copied()
            .find(|&c| matches!(cx.node(c), Node::SwitchCase { test: None, .. }));
        if let Some(d) = default {
            cx.report(cx.span(d), "Default clause should be the last clause.")
                .id("notLast");
        }
    }
}
