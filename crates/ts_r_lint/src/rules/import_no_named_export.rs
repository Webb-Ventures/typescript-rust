//! `import/no-named-export`: named exports in ES modules (source type `module` only):
//! `export *`, exported declarations and export lists, except lists that only export
//! `default` (`export { x as default }`).

use super::import_util::{ExportDecl, export_decl, module_export_name};
use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-named-export",
        "Forbid named exports.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoNamedExport>,
};

#[derive(Debug, Default)]
struct NoNamedExport;

impl Rule for NoNamedExport {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Program, NodeKind::ModuleBlock]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if cx.env().source_type != crate::env::SourceType::Module {
            return;
        }
        let ast = cx.ast();
        let (Node::Program { stmts } | Node::ModuleBlock { stmts }) = *cx.node(node) else {
            return;
        };
        for &stmt in ast.list(stmts) {
            let named = match export_decl(ast, stmt) {
                Some(ExportDecl::All) => true,
                Some(ExportDecl::Named { .. }) => match *ast.node(stmt) {
                    Node::ExportNamed { specifiers, .. } => {
                        let specs = ast.list(specifiers);
                        specs.is_empty()
                            || specs.iter().any(|&s| match *ast.node(s) {
                                Node::ExportSpecifier {
                                    local, exported, ..
                                } => {
                                    module_export_name(ast, exported.unwrap_or(local))
                                        != Some("default")
                                }
                                _ => true,
                            })
                    }
                    _ => true,
                },
                _ => false,
            };
            if named {
                cx.report_node(stmt, "Named exports are not allowed.");
            }
        }
    }
}
