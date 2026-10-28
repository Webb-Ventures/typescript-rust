//! `unicorn/no-exports-in-scripts`: a file whose first line is a hashbang (`#!`) is a
//! script run directly, so every export in it is reported: `export` declarations and lists,
//! `export default`, `export * from`, and TypeScript's `export =`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-exports-in-scripts",
        "Disallow exports in scripts.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoExportsInScripts>,
};

#[derive(Debug, Default)]
struct NoExportsInScripts;

impl Rule for NoExportsInScripts {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::ExportNamed,
            NodeKind::ExportAll,
            NodeKind::ExportDefaultExpr,
            NodeKind::ExportAssignment,
            NodeKind::VarDecl,
            NodeKind::FunctionDecl,
            NodeKind::ClassDecl,
            NodeKind::Interface,
            NodeKind::TypeAlias,
            NodeKind::Enum,
            NodeKind::Module,
            NodeKind::ImportEquals,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if !cx.src().starts_with("#!") {
            return;
        }
        let n = cx.node(node);
        let is_export = match n {
            Node::ExportNamed { .. }
            | Node::ExportAll { .. }
            | Node::ExportDefaultExpr { .. }
            | Node::ExportAssignment { .. } => true,
            _ => n.modifiers().contains(Modifiers::EXPORT),
        };
        if is_export {
            cx.report_node(node, "Do not use exports in scripts.")
                .id("no-exports-in-scripts");
        }
    }
}
