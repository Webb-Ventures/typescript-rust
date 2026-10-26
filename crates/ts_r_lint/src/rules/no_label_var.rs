//! `no-label-var`: disallow labels that share a name with a variable in scope.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-label-var",
        "Disallow labels that share a name with a variable",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoLabelVar>,
};

#[derive(Debug, Default)]
struct NoLabelVar;

impl Rule for NoLabelVar {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Labeled]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Labeled { label, .. } = *cx.node(node) else {
            return;
        };
        let Some(name) = ident_name(cx.ast(), label) else {
            return;
        };
        let sem = cx.semantic();
        let scope = sem.enclosing_scope(node);
        if sem.lookup(scope, name).is_some() || sem.is_global(name) {
            cx.report(cx.span(node), "Found identifier with same name as label.")
                .id("identifierClashWithLabel");
        }
    }
}
