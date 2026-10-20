//! `no-duplicate-case`: disallow duplicate case labels.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-duplicate-case",
        "Disallow duplicate case labels",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoDuplicateCase>,
};

#[derive(Debug, Default)]
struct NoDuplicateCase;

impl Rule for NoDuplicateCase {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Switch]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Switch { cases, .. } = *cx.node(node) else {
            return;
        };
        let mut tests: Vec<NodeId> = Vec::new();
        for &c in ast.list(cases) {
            let Node::SwitchCase { test: Some(t), .. } = *ast.node(c) else {
                continue;
            };
            let t = skip_parens(ast, t);
            if tests
                .iter()
                .any(|&prev| same_tokens(ast, cx.src(), prev, t))
            {
                cx.report_node(c, "Duplicate case label.").id("unexpected");
            } else {
                tests.push(t);
            }
        }
    }
}
