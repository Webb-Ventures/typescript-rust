//! `import/named`: named imports (`import { a } from "./x"`) and re-exports
//! (`export { a } from "./x"`) must name an export of the module, followed through its
//! re-exports. Modules without an export map (unresolved, CommonJS, ignored) and
//! ambiguous ones are skipped, as are type-only imports. With `commonjs`, destructured
//! `require("./x")` keys are checked too.

use super::import_util::{exports_of, string_literal};
use super::prelude::*;
use crate::modules::export_map::{has_deep, module_name};
use crate::modules::{ExportMap, relative};
use std::sync::Arc;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/named",
        "Ensure named imports correspond to a named export in the remote file.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create::<Named>,
};

#[derive(Debug)]
struct Named {
    commonjs: bool,
}

impl FromOptions for Named {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(Named {
            commonjs: bool_field(obj_opt(opts, 0), "commonjs", false),
        })
    }
}

/// Reports `name` (at `at`) unless `m` exports it.
fn check_name(cx: &mut Ctx<'_, '_>, m: &ExportMap, name: &str, at: NodeId, spec: &str) {
    let (found, chain) = has_deep(cx.file.modules(), m, name);
    if found {
        return;
    }
    let msg = if chain.len() > 1 {
        let dir = cx.file.modules().dir().to_owned();
        let path: Vec<String> = chain.iter().map(|p| relative(&dir, p)).collect();
        format!("{name} not found via {}", path.join(" -> "))
    } else {
        format!("{name} not found in '{spec}'")
    };
    let span = cx.span(at);
    cx.report(span, msg);
}

/// The module's map when it is worth checking (resolved and unambiguously an ES module).
fn usable(cx: &mut Ctx<'_, '_>, spec: &str) -> Option<Arc<ExportMap>> {
    // A map with parse errors is "ambiguous" too: `named` never reports them.
    let m = exports_of(cx, spec)?;
    m.is_module.then_some(m)
}

impl Rule for Named {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::ImportDecl,
            NodeKind::ExportNamed,
            NodeKind::VarDeclarator,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *ast.node(node) {
            Node::ImportDecl {
                type_only: false,
                specifiers,
                source,
                ..
            } => {
                let specs: Vec<NodeId> = ast
                    .list(specifiers)
                    .iter()
                    .copied()
                    .filter(|&s| matches!(ast.node(s), Node::ImportSpecifier { .. }))
                    .collect();
                if specs.is_empty() {
                    return;
                }
                let Some(spec) = string_literal(ast, source) else {
                    return;
                };
                let Some(m) = usable(cx, spec) else {
                    return;
                };
                for s in specs {
                    let Node::ImportSpecifier {
                        type_only: false,
                        imported,
                        local,
                    } = *ast.node(s)
                    else {
                        continue;
                    };
                    let at = imported.unwrap_or(local);
                    if let Some(name) = module_name(ast, at) {
                        check_name(cx, &m, name, at, spec);
                    }
                }
            }
            Node::ExportNamed {
                type_only: false,
                specifiers,
                source: Some(source),
                ..
            } => {
                let specs = ast.list(specifiers);
                if specs.is_empty() {
                    return;
                }
                let Some(spec) = string_literal(ast, source) else {
                    return;
                };
                let Some(m) = usable(cx, spec) else {
                    return;
                };
                for &s in specs {
                    let Node::ExportSpecifier {
                        type_only: false,
                        local,
                        ..
                    } = *ast.node(s)
                    else {
                        continue;
                    };
                    if let Some(name) = module_name(ast, local) {
                        check_name(cx, &m, name, local, spec);
                    }
                }
            }
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } if self.commonjs => {
                let Node::ObjectPattern { props } = *ast.node(name) else {
                    return;
                };
                let props = ast.list(props);
                if props.is_empty() {
                    return;
                }
                let Node::Call { callee, args, .. } = *ast.node(init) else {
                    return;
                };
                if ident_name(ast, callee) != Some("require") {
                    return;
                }
                let &[arg] = ast.list(args) else { return };
                let Some(spec) = string_literal(ast, arg) else {
                    return;
                };
                let Some(m) = usable(cx, spec) else {
                    return;
                };
                for &p in props {
                    if let Node::BindingProperty { key, .. } = *ast.node(p)
                        && let Some(k) = ident_name(ast, key)
                    {
                        check_name(cx, &m, k, key, spec);
                    }
                }
            }
            _ => {}
        }
    }
}
