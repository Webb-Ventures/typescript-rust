//! `no-debugger`: disallow `debugger` statements.

use ts_r_ast::{NodeId, NodeKind};

use super::{ESLINT_REC, create_default, meta};
use crate::rule::{Category, Ctx, Rule, RuleDef};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-debugger",
        "Disallow the use of `debugger`",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoDebugger>,
};

#[derive(Debug, Default)]
struct NoDebugger;

impl Rule for NoDebugger {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Debugger]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        cx.report_node(node, "Unexpected 'debugger' statement.")
            .id("unexpected");
    }
}
