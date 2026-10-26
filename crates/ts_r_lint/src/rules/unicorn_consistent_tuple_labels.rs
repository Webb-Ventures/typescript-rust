//! `unicorn/consistent-tuple-labels`: in a tuple type with two or more elements where some
//! but not all elements are labeled (`[a: string, number]`), each unlabeled element is
//! reported. A labeled rest (`...rest: T[]`) counts as labeled.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/consistent-tuple-labels",
        "Enforce consistent labels on tuple elements.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<ConsistentTupleLabels>,
};

#[derive(Debug, Default)]
struct ConsistentTupleLabels;

impl Rule for ConsistentTupleLabels {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::TupleType]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::TupleType { elems } = *cx.node(node) else {
            return;
        };
        let elems = ast.list(elems);
        let labeled = |e: NodeId| match *ast.node(e) {
            Node::NamedTupleMember { .. } => true,
            Node::RestType { ty } => matches!(ast.node(ty), Node::NamedTupleMember { .. }),
            _ => false,
        };
        let unlabeled: Vec<NodeId> = elems.iter().copied().filter(|&e| !labeled(e)).collect();
        if elems.len() < 2 || unlabeled.is_empty() || unlabeled.len() == elems.len() {
            return;
        }
        for e in unlabeled {
            cx.report_node(
                e,
                "This tuple element should have a label, just like the other elements.",
            )
            .id("consistent-tuple-labels");
        }
    }
}
