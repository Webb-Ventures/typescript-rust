//! `import/no-self-import`: a module that imports or `require()`s itself (the specifier
//! resolves to the linted file).

use super::import_util::{MODULE_KINDS, VisitorOptions, resolve};
use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-self-import",
        "Forbid a module from importing itself.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoSelfImport>,
};

#[derive(Debug, Default)]
struct NoSelfImport;

impl Rule for NoSelfImport {
    fn kinds(&self) -> &'static [NodeKind] {
        MODULE_KINDS
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let visitor = VisitorOptions::from_options(&[], true, true);
        for (r, value) in visitor.refs(cx.ast(), node) {
            let Some(resolved) = resolve(cx, value) else {
                continue;
            };
            if resolved.path() == Some(cx.file.modules().file.as_str()) {
                let span = cx.span(r.importer);
                cx.report(span, "Module imports itself.");
            }
        }
    }
}
