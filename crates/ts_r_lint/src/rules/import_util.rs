//! Helpers shared by the eslint-plugin-import rules: the module specifiers a file names
//! (`eslint-module-utils`' module visitor) and the visitor's options.

use super::prelude::*;
use ts_r_ast::Ast;

/// Which module systems the visitor looks at (`esmodule`, `commonjs`, `amd`), as the
/// plugin's shared options schema spells them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ModuleSystems {
    pub esmodule: bool,
    pub commonjs: bool,
    pub amd: bool,
}

impl ModuleSystems {
    /// The options object at `opts[0]`, with the given defaults for absent keys.
    pub(crate) fn from_options(opts: &[Value], esmodule: bool, commonjs: bool) -> ModuleSystems {
        let o = obj_opt(opts, 0);
        ModuleSystems {
            esmodule: bool_field(o, "esmodule", esmodule),
            commonjs: bool_field(o, "commonjs", commonjs),
            amd: bool_field(o, "amd", false),
        }
    }
}

/// One module specifier: the string literal and the node that names it (the declaration,
/// `import()` or `require()` call; the array element for AMD).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ModuleRef {
    pub source: NodeId,
    pub importer: NodeId,
}

/// The node kinds [`module_refs`] answers for.
pub(crate) const MODULE_KINDS: &[NodeKind] = &[
    NodeKind::ImportDecl,
    NodeKind::ExportNamed,
    NodeKind::ExportAll,
    NodeKind::ImportCall,
    NodeKind::Call,
];

/// A string literal (not a template), through parentheses.
pub(crate) fn string_literal(ast: &Ast, id: NodeId) -> Option<&str> {
    match *ast.node(skip_parens(ast, id)) {
        Node::StringLit { value } => Some(ast.atom(value)),
        _ => None,
    }
}

/// The module specifiers `node` names under `systems` (the plugin's module visitor):
/// import and export-from declarations and `import("x")` for ES modules, `require("x")`
/// with one string argument for CommonJS, and the string elements (other than the magic
/// `require` and `exports`) of `require([...], f)` / `define([...], f)` for AMD.
pub(crate) fn module_refs(ast: &Ast, node: NodeId, systems: ModuleSystems) -> Vec<ModuleRef> {
    let mut out = Vec::new();
    let import = |source| ModuleRef {
        source,
        importer: node,
    };
    match *ast.node(node) {
        Node::ImportDecl { source, .. } | Node::ExportAll { source, .. } if systems.esmodule => {
            if string_literal(ast, source).is_some() {
                out.push(import(skip_parens(ast, source)));
            }
        }
        Node::ExportNamed {
            source: Some(source),
            ..
        } if systems.esmodule => {
            if string_literal(ast, source).is_some() {
                out.push(import(source));
            }
        }
        Node::ImportCall { arg, .. } if systems.esmodule => {
            if string_literal(ast, arg).is_some() {
                out.push(import(skip_parens(ast, arg)));
            }
        }
        Node::Call { callee, args, .. } => {
            let callee_name = ident_name(ast, skip_parens(ast, callee));
            let args = ast.list(args);
            if systems.commonjs
                && callee_name == Some("require")
                && let &[arg] = args
                && string_literal(ast, arg).is_some()
            {
                out.push(ModuleRef {
                    source: skip_parens(ast, arg),
                    importer: node,
                });
            }
            if systems.amd
                && matches!(callee_name, Some("require" | "define"))
                && let &[deps, _] = args
                && let Node::ArrayLit { elements } = *ast.node(skip_parens(ast, deps))
            {
                for &e in ast.list(elements) {
                    let e = skip_parens(ast, e);
                    if let Some(v) = string_literal(ast, e)
                        && v != "require"
                        && v != "exports"
                    {
                        out.push(ModuleRef {
                            source: e,
                            importer: e,
                        });
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// A declaration-level `import type` / `export type ... from` (ESTree's `importKind` or
/// `exportKind` of `type`).
pub(crate) fn is_type_only_declaration(ast: &Ast, node: NodeId) -> bool {
    matches!(
        *ast.node(node),
        Node::ImportDecl {
            type_only: true,
            ..
        } | Node::ExportNamed {
            type_only: true,
            ..
        } | Node::ExportAll {
            type_only: true,
            ..
        }
    )
}

/// The statement is an ESTree export declaration: `export ...` declarations, export
/// lists, `export *` and `export default`.
pub(crate) fn is_export_statement(ast: &Ast, stmt: NodeId) -> bool {
    match ast.node(stmt) {
        Node::ExportNamed { .. } | Node::ExportAll { .. } | Node::ExportDefaultExpr { .. } => true,
        n => n.modifiers().contains(Modifiers::EXPORT),
    }
}

/// Every node of the file in source order (depth first, parents before children).
pub(crate) fn walk(ast: &Ast, mut f: impl FnMut(NodeId)) {
    let mut stack = vec![ast.root];
    let mut kids = Vec::new();
    while let Some(id) = stack.pop() {
        f(id);
        kids.clear();
        ast.for_each_child(id, |c| kids.push(c));
        stack.extend(kids.iter().rev());
    }
}

/// The kind of the innermost scope around `node` (ESLint's `getScope`).
pub(crate) fn scope_kind(cx: &Ctx<'_, '_>, node: NodeId) -> ScopeKind {
    let sem = cx.semantic();
    sem.scope(sem.enclosing_scope(node)).kind
}

/// The kind of the scope `var` declarations at `node` land in (ESLint's
/// `scope.variableScope`).
pub(crate) fn var_scope_kind(cx: &Ctx<'_, '_>, node: NodeId) -> ScopeKind {
    let sem = cx.semantic();
    let mut s = sem.enclosing_scope(node);
    loop {
        let sc = sem.scope(s);
        if sc.kind.is_var_scope() {
            return sc.kind;
        }
        match sc.parent {
            Some(p) => s = p,
            None => return sc.kind,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};

    /// Every `import/*` rule on `src` (module code): the rule names reported.
    fn lint_all_import_rules(src: &str) -> Vec<&'static str> {
        let reg = crate::Registry::global();
        let rules: serde_json::Map<String, serde_json::Value> = super::super::ALL
            .iter()
            .filter(|d| d.meta.name.starts_with("import/"))
            .map(|d| (d.meta.name.to_owned(), "error".into()))
            .collect();
        let json = serde_json::json!([{ "files": ["**/*.js"], "rules": rules }]);
        let objs = objects_from_eslint_json(&json, reg).unwrap();
        let file = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let fc = file.file_config("a.js", &ResolveOptions::new()).unwrap();
        let r = crate::lint_source(
            "/p/a.js",
            src,
            &fc,
            &crate::NoTypeInfo,
            crate::types::FileKey(0),
        );
        r.diagnostics.iter().filter_map(|d| d.rule).collect()
    }

    #[test]
    fn deep_nesting_does_not_overflow() {
        let depth = 20_000;
        let src = format!(
            "export const {}a{} = 1; var x = {}require('a'){};",
            "[".repeat(depth),
            "]".repeat(depth),
            "(".repeat(depth),
            ")".repeat(depth)
        );
        let _ = lint_all_import_rules(&src);
    }

    #[test]
    fn malformed_imports_do_not_panic() {
        for src in [
            "import {",
            "import { , } from",
            "import {} from 'x' with {",
            "import(/* webpackChunkName: \"a\" */",
            "require(",
            "import a from 'x'; import a from 'x'; import {",
            "export { a as default, b }; import * as ns from 'x'; ns[",
            "#",
        ] {
            let _ = lint_all_import_rules(src);
        }
    }

    #[test]
    fn reports_on_a_mixed_module() {
        let names = lint_all_import_rules(
            "import fs from 'fs';\nfoo();\nimport {} from 'x';\nimport { y } from 'x';\nexport let z = 1;\n",
        );
        for rule in [
            "import/first",
            "import/no-nodejs-modules",
            "import/no-empty-named-blocks",
            "import/no-duplicates",
            "import/no-mutable-exports",
            "import/prefer-default-export",
        ] {
            assert!(names.contains(&rule), "{rule} not in {names:?}");
        }
    }
}
