//! `import/no-webpack-loader-syntax`: a module specifier with a `!` (webpack loader
//! syntax), in imports, re-exports, `import()` and `require()`.

use super::import_util::{MODULE_KINDS, ModuleSystems, module_refs, string_literal};
use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-webpack-loader-syntax",
        "Forbid webpack loader syntax in imports.",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create_default::<NoWebpackLoaderSyntax>,
};

#[derive(Debug, Default)]
struct NoWebpackLoaderSyntax;

impl Rule for NoWebpackLoaderSyntax {
    fn kinds(&self) -> &'static [NodeKind] {
        MODULE_KINDS
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let systems = ModuleSystems {
            esmodule: true,
            commonjs: true,
            amd: false,
        };
        for r in module_refs(ast, node, systems) {
            let value = string_literal(ast, r.source).unwrap_or("");
            if value.contains('!') {
                cx.report_node(
                    r.importer,
                    format!(
                        "Unexpected '!' in '{value}'. Do not use import syntax to configure webpack loaders."
                    ),
                );
            }
        }
    }
}
