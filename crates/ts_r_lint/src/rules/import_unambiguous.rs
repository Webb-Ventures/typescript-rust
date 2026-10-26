//! `import/unambiguous`: an ES module (source type `module`) with no top-level `import`
//! or `export` declaration (or TypeScript `export =`) would parse the same as a script.
//! `import x = require()` and `import()` don't count.

use super::import_util::export_decl;
use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/unambiguous",
        "Forbid potentially ambiguous parse goal (`script` vs. `module`).",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<Unambiguous>,
};

#[derive(Debug, Default)]
struct Unambiguous;

impl Rule for Unambiguous {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Program]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if cx.env().source_type != crate::env::SourceType::Module {
            return;
        }
        let ast = cx.ast();
        let Node::Program { stmts } = *cx.node(node) else {
            return;
        };
        let module = ast.list(stmts).iter().any(|&s| {
            matches!(
                ast.node(s),
                Node::ImportDecl { .. } | Node::ExportAssignment { .. }
            ) || export_decl(ast, s).is_some()
        });
        if !module {
            cx.report_node(node, "This module could be parsed as a valid script.");
        }
    }
}
