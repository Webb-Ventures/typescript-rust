//! `import/max-dependencies`: more distinct module specifiers (imports, re-exports,
//! `import()`, `require()`) than `max` (default 10). The last specifier is reported once
//! per file; `ignoreTypeImports` leaves `import type` declarations out of the count.

use super::import_util::{
    ModuleSystems, is_type_only_declaration, module_refs, string_literal, walk,
};
use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/max-dependencies",
        "Enforce the maximum number of dependencies a module can have.",
        NO_PRESET,
        Category::Pedantic,
    ),
    create: create::<MaxDependencies>,
};

#[derive(Debug)]
struct MaxDependencies {
    /// `None` when an options object leaves `max` out: the plugin then never reports.
    max: Option<f64>,
    ignore_type_imports: bool,
}

impl FromOptions for MaxDependencies {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let max = match opts.first() {
            Some(Value::Object(_)) => o.get("max").and_then(Value::as_f64),
            _ => Some(10.0),
        };
        Ok(MaxDependencies {
            max,
            ignore_type_imports: bool_field(o, "ignoreTypeImports", false),
        })
    }
}

impl Rule for MaxDependencies {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let Some(max) = self.max else { return };
        let ast = cx.ast();
        let systems = ModuleSystems {
            esmodule: true,
            commonjs: true,
            amd: false,
        };
        let mut deps = std::collections::BTreeSet::new();
        let mut last = None;
        walk(ast, |node| {
            for r in module_refs(ast, node, systems) {
                let type_import =
                    ast.kind(node) == NodeKind::ImportDecl && is_type_only_declaration(ast, node);
                if !(type_import && self.ignore_type_imports) {
                    deps.insert(string_literal(ast, r.source).unwrap_or(""));
                }
                last = Some(r.source);
            }
        });
        if let Some(last) = last
            && deps.len() as f64 > max
        {
            let shown = number_to_string(max);
            cx.report_node(
                last,
                format!("Maximum number of dependencies ({shown}) exceeded."),
            );
        }
    }
}
