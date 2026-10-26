//! `import/no-deprecated`: imports and uses of names whose doc comment in the module has a
//! `@deprecated` tag (or a TomDoc `Deprecated:` line with `import/docstyle`), members of
//! namespace imports included, and imports of a module whose `@module` doc is deprecated.
//! Only references to module-scope bindings count.

use super::import_util::{
    declared_scope, exports_of, program_statements, report_parse_errors, string_literal,
};
use super::prelude::*;
use crate::modules::ModCtx;
use crate::modules::export_map::{Doc, ExportMap, ExportMeta, Lookup, get, size};
use rustc_hash::FxHashMap;
use std::sync::Arc;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-deprecated",
        "Forbid imported names marked with `@deprecated` documentation tag.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoDeprecated>,
};

#[derive(Debug, Default)]
struct NoDeprecated;

fn message(desc: &str) -> String {
    if desc.is_empty() {
        "Deprecated.".to_owned()
    } else {
        format!("Deprecated: {desc}")
    }
}

fn deprecation(meta: &ExportMeta) -> Option<String> {
    meta.doc
        .as_deref()
        .and_then(|d: &Doc| d.tag("deprecated"))
        .map(str::to_owned)
}

fn namespace_of(m: &ModCtx, meta: &ExportMeta) -> Option<Arc<ExportMap>> {
    meta.namespace.as_deref().and_then(|p| m.map_for(p))
}

impl Rule for NoDeprecated {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut deprecated: FxHashMap<String, String> = FxHashMap::default();
        let mut namespaces: FxHashMap<String, Arc<ExportMap>> = FxHashMap::default();
        for &s in program_statements(ast) {
            let Node::ImportDecl {
                specifiers, source, ..
            } = *ast.node(s)
            else {
                continue;
            };
            let Some(spec) = string_literal(ast, source) else {
                continue;
            };
            let Some(m) = exports_of(cx, spec) else {
                continue;
            };
            if let Some(d) = m.doc.as_deref().and_then(|d| d.tag("deprecated")) {
                let span = cx.span(s);
                cx.report(span, message(d));
            }
            if !m.errors.is_empty() {
                report_parse_errors(cx, &m, source, spec);
                continue;
            }
            for &sp in ast.list(specifiers) {
                let (imported, local) = match *ast.node(sp) {
                    Node::ImportNamespaceSpecifier { local } => {
                        if size(cx.file.modules(), &m) == 0 {
                            continue;
                        }
                        if let Some(l) = ident_name(ast, local) {
                            namespaces.insert(l.to_owned(), m.clone());
                        }
                        continue;
                    }
                    Node::ImportDefaultSpecifier { local } => ("default", local),
                    Node::ImportSpecifier {
                        imported, local, ..
                    } => {
                        let Some(i) =
                            crate::modules::export_map::module_name(ast, imported.unwrap_or(local))
                        else {
                            continue;
                        };
                        (i, local)
                    }
                    _ => continue,
                };
                let Some(local) = ident_name(ast, local) else {
                    continue;
                };
                let Lookup::Found(meta) = get(cx.file.modules(), &m, imported) else {
                    continue;
                };
                if let Some(ns) = namespace_of(cx.file.modules(), &meta) {
                    namespaces.insert(local.to_owned(), ns);
                }
                let Some(d) = deprecation(&meta) else {
                    continue;
                };
                let span = cx.span(sp);
                cx.report(span, message(&d));
                deprecated.insert(local.to_owned(), d);
            }
        }
        if deprecated.is_empty() && namespaces.is_empty() {
            return;
        }
        let mut idents = Vec::new();
        let mut members = Vec::new();
        super::import_util::walk(ast, |id| match ast.node(id) {
            Node::Ident { .. } => idents.push(id),
            Node::Member { .. } | Node::Index { .. } => members.push(id),
            _ => {}
        });
        for id in idents {
            let Some(name) = ident_name(ast, id) else {
                continue;
            };
            let Some(d) = deprecated.get(name) else {
                continue;
            };
            if let Some(p) = cx.file.parents.parent(id) {
                match *ast.node(p) {
                    Node::Member { prop, .. } if prop == id => continue,
                    Node::ImportDecl { .. }
                    | Node::ImportSpecifier { .. }
                    | Node::ImportDefaultSpecifier { .. }
                    | Node::ImportNamespaceSpecifier { .. } => continue,
                    _ => {}
                }
            }
            if declared_scope(cx, name, id) != Some(ScopeKind::Module) {
                continue;
            }
            let d = d.clone();
            let span = cx.span(id);
            cx.report(span, message(&d));
        }
        for id in members {
            let (Node::Member { obj, .. } | Node::Index { obj, .. }) = *ast.node(id) else {
                continue;
            };
            let Some(name) = ident_name(ast, obj) else {
                continue;
            };
            let Some(ns) = namespaces.get(name) else {
                continue;
            };
            if declared_scope(cx, name, obj) != Some(ScopeKind::Module) {
                continue;
            }
            let mut namespace = Some(ns.clone());
            let mut cur = id;
            for _ in 0..crate::modules::MAX_DEPTH {
                let Some(m) = namespace.take() else { break };
                let Node::Member { prop, .. } = *ast.node(cur) else {
                    break;
                };
                let Some(p) = ident_name(ast, prop) else {
                    break;
                };
                let Lookup::Found(meta) = get(cx.file.modules(), &m, p) else {
                    break;
                };
                if let Some(d) = deprecation(&meta) {
                    let span = cx.span(prop);
                    cx.report(span, message(&d));
                }
                namespace = namespace_of(cx.file.modules(), &meta);
                match parent_skip_parens(ast, cx.file.parents, cur) {
                    Some(next) => cur = next,
                    None => break,
                }
            }
        }
    }
}
