//! `@typescript-eslint/no-useless-empty-export`: `export {}` in a file that already imports
//! or exports something.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-useless-empty-export",
        "Disallow empty exports that don't change anything in a module file",
        NO_PRESET,
        Category::Suspicious,
    )
    .fixable(),
    create: create_default::<NoUselessEmptyExport>,
};

#[derive(Debug, Default)]
struct NoUselessEmptyExport;

impl Rule for NoUselessEmptyExport {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Program]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if [".d.ts", ".d.cts", ".d.mts"]
            .iter()
            .any(|e| cx.file.path.ends_with(e))
        {
            return;
        }
        let ast = cx.ast();
        let Node::Program { stmts } = *cx.node(node) else {
            return;
        };
        let mut empty = Vec::new();
        let mut other = false;
        for &s in ast.list(stmts) {
            match *ast.node(s) {
                Node::ExportNamed { specifiers, .. } if ast.list(specifiers).is_empty() => {
                    empty.push(s)
                }
                Node::ExportNamed { .. }
                | Node::ExportAll { .. }
                | Node::ExportDefaultExpr { .. }
                | Node::ExportAssignment { .. }
                | Node::ImportDecl { .. }
                | Node::ImportEquals { .. } => other = true,
                ref n if n.modifiers().contains(Modifiers::EXPORT) => other = true,
                _ => {}
            }
        }
        if !other {
            return;
        }
        for s in empty {
            let span = cx.span(s);
            cx.report_node(s, "Empty export does nothing and can be removed.")
                .id("uselessExport")
                .fix(Fix::delete(span));
        }
    }
}
