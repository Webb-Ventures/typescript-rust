//! `import/namespace`: names read from a namespace import (`ns.a`, `const { a } = ns`,
//! `<ns.A />`) must be exported by the module, followed into re-exported namespaces
//! (`ns.inner.a`); also no assignment to a member, no computed member (unless
//! `allowComputed`) and an export at all ("No exported names found"). Only bindings of the
//! module scope count.

use super::import_util::{
    declared_scope, exports_of, program_statements, report_parse_errors, specifier_imported,
    specifier_local, string_literal,
};
use super::prelude::*;
use crate::modules::ModCtx;
use crate::modules::export_map::{ExportMap, Lookup, get, has, size};
use rustc_hash::FxHashMap;
use std::sync::Arc;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/namespace",
        "Ensure imported namespaces contain dereferenced properties as they are dereferenced.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create::<Namespace>,
};

#[derive(Debug)]
struct Namespace {
    allow_computed: bool,
}

impl FromOptions for Namespace {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(Namespace {
            allow_computed: bool_field(obj_opt(opts, 0), "allowComputed", false),
        })
    }
}

fn message(prop: &str, path: &[String]) -> String {
    format!(
        "'{prop}' not found in {}imported namespace '{}'.",
        if path.len() > 1 { "deeply " } else { "" },
        path.join(".")
    )
}

/// The export map a re-exported namespace stands for.
fn namespace_of(m: &ModCtx, meta: &crate::modules::ExportMeta) -> Option<Arc<ExportMap>> {
    meta.namespace.as_deref().and_then(|p| m.map_for(p))
}

/// The namespaces the top-level imports bind: local name, export map.
fn collect(cx: &mut Ctx<'_, '_>) -> FxHashMap<String, Arc<ExportMap>> {
    let ast = cx.ast();
    let mut out = FxHashMap::default();
    for &s in program_statements(ast) {
        let Node::ImportDecl {
            specifiers, source, ..
        } = *ast.node(s)
        else {
            continue;
        };
        let specs = ast.list(specifiers);
        if specs.is_empty() {
            continue;
        }
        let Some(spec) = string_literal(ast, source) else {
            continue;
        };
        let Some(m) = exports_of(cx, spec) else {
            continue;
        };
        if !m.errors.is_empty() {
            report_parse_errors(cx, &m, source, spec);
            continue;
        }
        for &sp in specs {
            let Some(local) = specifier_local(ast, sp) else {
                continue;
            };
            if matches!(ast.node(sp), Node::ImportNamespaceSpecifier { .. }) {
                if size(cx.file.modules(), &m) == 0 {
                    let span = cx.span(sp);
                    cx.report(span, format!("No exported names found in module '{spec}'."));
                }
                out.insert(local.to_owned(), m.clone());
            } else if let Some(imported) = specifier_imported(ast, sp)
                && let Lookup::Found(meta) = get(cx.file.modules(), &m, imported)
                && let Some(ns) = namespace_of(cx.file.modules(), &meta)
            {
                out.insert(local.to_owned(), ns);
            }
        }
    }
    out
}

impl Namespace {
    /// A member chain starting at `node` (`ns.a.b`), whose object is the namespace `name`.
    fn member(&self, cx: &mut Ctx<'_, '_>, node: NodeId, name: &str, ns: &Arc<ExportMap>) {
        let ast = cx.ast();
        if let Some(p) = parent_skip_parens(ast, cx.file.parents, node)
            && let Node::Assign { left, .. } = *ast.node(p)
            && skip_parens(ast, left) == node
        {
            let span = cx.span(p);
            cx.report(span, format!("Assignment to member of namespace '{name}'."));
        }
        let mut namespace = Some(ns.clone());
        let mut path = vec![name.to_owned()];
        let mut cur = node;
        for _ in 0..crate::modules::MAX_DEPTH {
            let Some(m) = namespace.take() else { return };
            match *ast.node(cur) {
                Node::Index { index, .. } => {
                    if !self.allow_computed {
                        let span = cx.span(index);
                        cx.report(
                            span,
                            format!(
                                "Unable to validate computed reference to imported namespace '{name}'."
                            ),
                        );
                    }
                    return;
                }
                Node::Member { prop, .. } => {
                    let Some(p) = ident_name(ast, prop) else {
                        return;
                    };
                    let mc = cx.file.modules();
                    if !has(mc, &m, p) {
                        let span = cx.span(prop);
                        cx.report(span, message(p, &path));
                        return;
                    }
                    let Lookup::Found(meta) = get(mc, &m, p) else {
                        return;
                    };
                    path.push(p.to_owned());
                    namespace = namespace_of(mc, &meta);
                    match parent_skip_parens(ast, cx.file.parents, cur) {
                        Some(next) => cur = next,
                        None => return,
                    }
                }
                _ => return,
            }
        }
    }

    /// `const { a, b: { c } } = ns`.
    fn destructure(
        &self,
        cx: &mut Ctx<'_, '_>,
        pattern: NodeId,
        ns: &Arc<ExportMap>,
        path: &mut Vec<String>,
    ) {
        let ast = cx.ast();
        let Node::ObjectPattern { props } = *ast.node(pattern) else {
            return;
        };
        if path.len() > crate::modules::MAX_DEPTH {
            return;
        }
        for &p in ast.list(props) {
            let Node::BindingProperty { key, value, .. } = *ast.node(p) else {
                continue;
            };
            let key = match *ast.node(key) {
                Node::ComputedKey { expr } => expr,
                _ => key,
            };
            let Some(k) = ident_name(ast, key) else {
                let span = cx.span(p);
                cx.report(span, "Only destructure top-level names.");
                continue;
            };
            let mc = cx.file.modules();
            if !has(mc, ns, k) {
                let span = cx.span(p);
                cx.report(span, message(k, path));
                continue;
            }
            let Lookup::Found(meta) = get(mc, ns, k) else {
                continue;
            };
            if let (Some(inner), Some(value)) = (namespace_of(mc, &meta), value) {
                path.push(k.to_owned());
                self.destructure(cx, value, &inner, path);
                path.pop();
            }
        }
    }
}

impl Rule for Namespace {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let namespaces = collect(cx);
        if namespaces.is_empty() {
            return;
        }
        let ast = cx.ast();
        let mut nodes = Vec::new();
        super::import_util::walk(ast, |id| {
            if matches!(
                ast.node(id),
                Node::Member { .. }
                    | Node::Index { .. }
                    | Node::VarDeclarator { .. }
                    | Node::JsxMemberExpr { .. }
            ) {
                nodes.push(id);
            }
        });
        for id in nodes {
            match *ast.node(id) {
                Node::Member { obj, .. } | Node::Index { obj, .. } => {
                    let Some(name) = ident_name(ast, obj) else {
                        continue;
                    };
                    let Some(ns) = namespaces.get(name) else {
                        continue;
                    };
                    if declared_scope(cx, name, obj) != Some(ScopeKind::Module) {
                        continue;
                    }
                    self.member(cx, id, name, ns);
                }
                Node::VarDeclarator {
                    name: pattern,
                    init: Some(init),
                    ..
                } => {
                    let Some(name) = ident_name(ast, init) else {
                        continue;
                    };
                    let Some(ns) = namespaces.get(name) else {
                        continue;
                    };
                    if declared_scope(cx, name, init) != Some(ScopeKind::Module) {
                        continue;
                    }
                    let mut path = vec![name.to_owned()];
                    self.destructure(cx, pattern, ns, &mut path);
                }
                Node::JsxMemberExpr { obj, prop } => {
                    let Some(name) = jsx_name(ast, obj) else {
                        continue;
                    };
                    let Some(ns) = namespaces.get(name) else {
                        continue;
                    };
                    let Some(p) = jsx_name(ast, prop) else {
                        continue;
                    };
                    if !has(cx.file.modules(), ns, p) {
                        let span = cx.span(prop);
                        cx.report(span, message(p, &[name.to_owned()]));
                    }
                }
                _ => {}
            }
        }
    }
}

fn jsx_name(ast: &ts_r_ast::Ast, id: NodeId) -> Option<&str> {
    match *ast.node(id) {
        Node::Ident { name } => Some(ast.atom(name)),
        _ => None,
    }
}
