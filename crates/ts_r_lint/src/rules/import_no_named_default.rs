//! `import/no-named-default`: `import { default as x }` should be `import x`. Type-only
//! specifiers are left alone.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-named-default",
        "Forbid named default exports.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoNamedDefault>,
};

#[derive(Debug, Default)]
struct NoNamedDefault;

impl Rule for NoNamedDefault {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ImportSpecifier]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::ImportSpecifier {
            type_only: false,
            imported: Some(imported),
            local,
        } = *cx.node(node)
        else {
            return;
        };
        if cx.parent(node).is_some_and(|p| {
            matches!(
                ast.node(p),
                Node::ImportDecl {
                    type_only: true,
                    ..
                }
            )
        }) {
            return;
        }
        let name = match *ast.node(imported) {
            Node::StringLit { value } => ast.atom(value),
            _ => ast.name_of(imported).unwrap_or(""),
        };
        if name != "default" {
            return;
        }
        let local_name = ast.name_of(local).unwrap_or("");
        cx.report_node(
            local,
            format!("Use default import syntax to import '{local_name}'."),
        );
    }
}
