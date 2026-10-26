//! `import/no-relative-parent-imports`: a module specifier that resolves to a file in a
//! parent directory of the linted file. Specifiers of type `external` are skipped, as are
//! ones that don't resolve to a file; the path from the file's directory to the resolved
//! file is then typed again (`../x` is `parent` unless `import/internal-regex` claims it).
//! Module-visitor options: ES modules on, CommonJS and AMD off by default.

use super::import_util::{MODULE_KINDS, VisitorOptions, resolve};
use super::prelude::*;
use crate::modules::import_type::{ImportType, import_type};
use ts_r_config::path;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-relative-parent-imports",
        "Forbid importing modules from parent directories.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoRelativeParentImports>,
};

#[derive(Debug)]
struct NoRelativeParentImports {
    visitor: VisitorOptions,
}

impl FromOptions for NoRelativeParentImports {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoRelativeParentImports {
            visitor: VisitorOptions::from_options(opts, true, false),
        })
    }
}

impl Rule for NoRelativeParentImports {
    fn kinds(&self) -> &'static [NodeKind] {
        MODULE_KINDS
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        for (r, value) in self.visitor.refs(cx.ast(), node) {
            let Some(resolved) = resolve(cx, value) else {
                continue;
            };
            let m = cx.file.modules();
            if import_type(m, value, resolved.path()) == ImportType::External {
                continue;
            }
            let Some(target) = resolved.path() else {
                continue;
            };
            let rel = crate::modules::relative(m.dir(), target);
            let rel_resolved = m.resolve(&rel).ok();
            let rel_path = rel_resolved.as_ref().and_then(|r| r.path());
            if import_type(m, &rel, rel_path) != ImportType::Parent {
                continue;
            }
            let base = path::file_name(&m.file).to_owned();
            cx.report_node(
                r.source,
                format!(
                    "Relative imports from parent directories are not allowed. Please either pass what you're importing through at runtime (dependency injection), move `{base}` to same directory as `{value}` or consider making `{value}` a package."
                ),
            );
        }
    }
}
