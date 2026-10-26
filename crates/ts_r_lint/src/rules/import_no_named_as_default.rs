//! `import/no-named-as-default`: a default import named like one of the module's named
//! exports (`import bar from "./bar"` when `./bar` exports `bar`), unless the default and
//! the named export are re-exports of the same module.

use super::import_util::{exports_of, report_parse_errors, string_literal};
use super::prelude::*;
use crate::modules::export_map::has;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-named-as-default",
        "Forbid use of exported name as identifier of default export.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoNamedAsDefault>,
};

#[derive(Debug, Default)]
struct NoNamedAsDefault;

impl Rule for NoNamedAsDefault {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ImportDefaultSpecifier]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::ImportDefaultSpecifier { local } = *ast.node(node) else {
            return;
        };
        let Some(name) = ident_name(ast, local) else {
            return;
        };
        if name == "default" {
            return;
        }
        let Some(decl) = cx.file.parents.parent(node) else {
            return;
        };
        let Node::ImportDecl { source, .. } = *ast.node(decl) else {
            return;
        };
        let Some(spec) = string_literal(ast, source) else {
            return;
        };
        let Some(m) = exports_of(cx, spec) else {
            return;
        };
        if !m.errors.is_empty() {
            report_parse_errors(cx, &m, source, spec);
            return;
        }
        let mc = cx.file.modules();
        if !m.has_default(mc) || !has(mc, &m, name) {
            return;
        }
        if let (Some(named), Some(default)) = (m.reexport(name), m.reexport("default"))
            && let (Some(a), Some(b)) = (
                named.path.as_deref().and_then(|p| mc.map_for(p)),
                default.path.as_deref().and_then(|p| mc.map_for(p)),
            )
            && a.path == b.path
        {
            return;
        }
        let span = cx.span(node);
        cx.report(
            span,
            format!("Using exported name '{name}' as identifier for default import."),
        );
    }
}
