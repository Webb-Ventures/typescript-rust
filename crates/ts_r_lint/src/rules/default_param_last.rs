//! `default-param-last`: parameters with defaults (or optional ones) come last.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "default-param-last",
        "Enforce default parameters to be last",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<DefaultParamLast>,
};

#[derive(Debug, Default)]
pub(crate) struct DefaultParamLast;

impl Rule for DefaultParamLast {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
            NodeKind::Arrow,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let params = match *ast.node(node) {
            Node::FunctionDecl { params, .. }
            | Node::FunctionExpr { params, .. }
            | Node::Arrow { params, .. } => ast.list(params),
            _ => return,
        };
        let mut late = Vec::new();
        let mut seen_plain = false;
        for &p in params.iter().rev() {
            let Node::Param {
                rest,
                optional,
                init,
                ..
            } = *ast.node(p)
            else {
                continue;
            };
            if rest {
                continue;
            }
            if init.is_some() || optional {
                if seen_plain {
                    late.push(p);
                }
            } else {
                seen_plain = true;
            }
        }
        for &p in late.iter().rev() {
            cx.report(cx.span(p), "Default parameters should be last.")
                .id("shouldBeLast");
        }
    }
}
