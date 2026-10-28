//! `import/no-import-module-exports`: `import` declarations in a module that also uses
//! `module.*` or `exports.*` (CommonJS). At the first such member expression every import
//! seen so far is reported, once per file. A `module` or `exports` declared in any scope
//! other than the module scope (the last such scope is the one looked at) turns the check
//! off, and so does importing the name. Files matching an `exceptions` glob are skipped.
//!
//! The plugin also skips the package's entry point (`main` of the nearest package.json);
//! the linter does not resolve packages, so that exception is not made.

use super::import_util::walk;
use super::prelude::*;
use crate::config::glob::Glob;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-import-module-exports",
        "Forbid import statements with CommonJS module.exports.",
        NO_PRESET,
        Category::Suspicious,
    )
    .fixable(),
    create: create::<NoImportModuleExports>,
};

#[derive(Debug)]
struct NoImportModuleExports {
    exceptions: Vec<Glob>,
}

impl FromOptions for NoImportModuleExports {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoImportModuleExports {
            exceptions: str_list(obj_opt(opts, 0), "exceptions")
                .iter()
                .map(|g| Glob::new(g))
                .collect(),
        })
    }
}

/// True when `name` counts as the CommonJS binding: no scope declares it, or the last
/// scope that does is the module scope and the declaration is not an import.
fn is_cjs_reference(cx: &Ctx<'_, '_>, name: &str) -> bool {
    let sem = cx.semantic();
    let scope = sem.scopes().iter().rev().find(|s| {
        s.symbols.iter().any(|&sym| {
            sem.name(sym) == name
                && sem
                    .symbol(sym)
                    .decls
                    .iter()
                    .any(|d| d.kind != DeclKind::CommentGlobal)
        })
    });
    let Some(scope) = scope else { return true };
    if scope.kind != ScopeKind::Module {
        return false;
    }
    let import = scope.symbols.iter().any(|&sym| {
        sem.name(sym) == name
            && sem.symbol(sym).decls.first().is_some_and(|d| {
                matches!(
                    d.kind,
                    DeclKind::ImportDefault | DeclKind::ImportNamed | DeclKind::ImportNamespace
                )
            })
    });
    !import
}

impl Rule for NoImportModuleExports {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let path = cx.file.path.replace('\\', "/");
        if self.exceptions.iter().any(|g| g.matches(&path)) {
            return;
        }
        let ast = cx.ast();
        let mut imports = Vec::new();
        let mut found = None;
        walk(ast, |n| {
            if found.is_some() {
                return;
            }
            match *ast.node(n) {
                Node::ImportDecl { .. } => imports.push(n),
                Node::Member { obj, .. } | Node::Index { obj, .. } => {
                    if let Some(name @ ("module" | "exports")) = ident_name(ast, obj)
                        && is_cjs_reference(cx, name)
                    {
                        found = Some(imports.len());
                    }
                }
                _ => {}
            }
        });
        let Some(n) = found else { return };
        for &i in &imports[..n] {
            cx.report_node(
                i,
                "Cannot use import declarations in modules that export using CommonJS (module.exports = 'foo' or exports.bar = 'hi')",
            );
        }
    }
}
