//! `import/exports-last`: export statements before the last non-export statement of the
//! file.

use super::import_util::is_export_statement;
use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/exports-last",
        "Ensure all exports appear after other statements.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<ExportsLast>,
};

#[derive(Debug, Default)]
struct ExportsLast;

impl Rule for ExportsLast {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Program]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Program { stmts } = *cx.node(node) else {
            return;
        };
        let stmts = ast.list(stmts);
        let Some(last) = stmts.iter().rposition(|&s| !is_export_statement(ast, s)) else {
            return;
        };
        for &s in &stmts[..last] {
            if is_export_statement(ast, s) {
                cx.report_node(s, "Export statements should appear at the end of the file");
            }
        }
    }
}
