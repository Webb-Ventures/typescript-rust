//! `import/no-default-export`: `export default ...` and `export { x as default }` in ES
//! modules (files with source type `module` only). Both are reported at the token after
//! `export` (`default` or `{`); an aliased default names the local binding in its message.

use super::import_util::{ExportDecl, export_decl, module_export_name, second_token};
use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-default-export",
        "Forbid default exports.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoDefaultExport>,
};

#[derive(Debug, Default)]
struct NoDefaultExport;

impl Rule for NoDefaultExport {
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
            let at = second_token(cx.src(), cx.span(stmt).start);
            match export_decl(ast, stmt) {
                Some(ExportDecl::Default) => {
                    cx.report(at, "Prefer named exports.");
                }
                Some(ExportDecl::Named { .. }) => {
                    let Node::ExportNamed { specifiers, .. } = *ast.node(stmt) else {
                        continue;
                    };
                    for &spec in ast.list(specifiers) {
                        let Node::ExportSpecifier {
                            local, exported, ..
                        } = *ast.node(spec)
                        else {
                            continue;
                        };
                        let exported = module_export_name(ast, exported.unwrap_or(local));
                        if exported != Some("default") {
                            continue;
                        }
                        let local = module_export_name(ast, local).unwrap_or("");
                        cx.report(
                            at,
                            format!(
                                "Do not alias `{local}` as `default`. Just export `{local}` itself instead."
                            ),
                        );
                    }
                }
                _ => {}
            }
        }
    }
}
