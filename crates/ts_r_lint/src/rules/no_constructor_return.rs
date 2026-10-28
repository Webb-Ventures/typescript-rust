//! `no-constructor-return`: disallow returning a value from a class constructor.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-constructor-return",
        "Disallow returning value from constructor",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoConstructorReturn>,
};

#[derive(Debug, Default)]
struct NoConstructorReturn;

impl Rule for NoConstructorReturn {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Return]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Return { arg: Some(_) } = *cx.node(node) else {
            return;
        };
        let ast = cx.ast();
        let parents = cx.file.parents;
        let Some(f) = parents
            .ancestors(node)
            .find(|&a| is_function(ast.node(a).kind()))
        else {
            return;
        };
        let is_ctor = parents.parent(f).is_some_and(|m| {
            matches!(
                *ast.node(m),
                Node::MethodDef { kind: MethodKind::Constructor, value, .. } if value == f
            )
        });
        if is_ctor {
            cx.report(cx.span(node), "Unexpected return statement in constructor.")
                .id("unexpected");
        }
    }
}
