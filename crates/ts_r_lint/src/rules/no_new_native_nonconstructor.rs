//! `no-new-native-nonconstructor`: disallow `new` operators with global non-constructor
//! functions.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-new-native-nonconstructor",
        "Disallow `new` operators with global non-constructor functions",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoNewNativeNonconstructor>,
};

#[derive(Debug, Default)]
struct NoNewNativeNonconstructor;

impl Rule for NoNewNativeNonconstructor {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::New { callee, .. } = *cx.node(node) else {
            return;
        };
        for name in ["Symbol", "BigInt"] {
            if cx.is_global_ref(callee, name) {
                cx.report_node(
                    skip_parens(cx.ast(), callee),
                    format!("`{name}` cannot be called as a constructor."),
                )
                .id("noNewNonconstructor");
            }
        }
    }
}
