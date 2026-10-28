//! `import/default`: a default import needs a default export in the module (explicit, or
//! re-exported as `default`; `export *` never provides one). Modules without an export
//! map are skipped.

use super::import_util::{exports_of, report_parse_errors, string_literal};
use super::prelude::*;
use crate::modules::export_map::{Lookup, get};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/default",
        "Ensure a default export is present, given a default import.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<Default>,
};

#[derive(Debug, Default)]
struct Default;

impl Rule for Default {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ImportDecl]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::ImportDecl {
            specifiers, source, ..
        } = *ast.node(node)
        else {
            return;
        };
        let Some(&default) = ast
            .list(specifiers)
            .iter()
            .find(|&&s| matches!(ast.node(s), Node::ImportDefaultSpecifier { .. }))
        else {
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
        } else if matches!(get(cx.file.modules(), &m, "default"), Lookup::Undefined) {
            let span = cx.span(default);
            cx.report(
                span,
                format!("No default export found in imported module \"{spec}\"."),
            );
        }
    }
}
