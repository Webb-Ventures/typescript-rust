//! `no-caller`: disallow `arguments.caller` and `arguments.callee`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-caller",
        "Disallow the use of `arguments.caller` or `arguments.callee`",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create_default::<NoCaller>,
};

#[derive(Debug, Default)]
struct NoCaller;

impl Rule for NoCaller {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Member { obj, prop, .. } = *cx.node(node) else {
            return;
        };
        let ast = cx.ast();
        if ident_name(ast, obj) != Some("arguments") {
            return;
        }
        let Some(name) = ident_name(ast, prop) else {
            return;
        };
        if matches!(name, "callee" | "caller") {
            cx.report(cx.span(node), format!("Avoid arguments.{name}."))
                .id("unexpected");
        }
    }
}
