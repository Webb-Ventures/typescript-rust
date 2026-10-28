//! `no-duplicate-imports`: one import (and, with `includeExports`, one re-export) per module,
//! unless the declarations could not be merged into one.

use rustc_hash::FxHashMap;
use ts_r_ast::visit::{Visitor, Walk, walk};

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-duplicate-imports",
        "Disallow duplicate module imports",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create::<NoDuplicateImports>,
};

#[derive(Debug)]
struct NoDuplicateImports {
    include_exports: bool,
    allow_separate_type_imports: bool,
}

impl FromOptions for NoDuplicateImports {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoDuplicateImports {
            include_exports: bool_field(o, "includeExports", false),
            allow_separate_type_imports: bool_field(o, "allowSeparateTypeImports", false),
        })
    }
}

/// What a declaration brings in, as far as merging goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Default,
    Named,
    Namespace,
    ExportAll,
    SideEffect,
}

#[derive(Clone, Copy)]
struct Decl {
    export: bool,
    type_only: bool,
    shape: Shape,
}

fn can_merge(a: &Decl, b: &Decl) -> bool {
    use Shape::*;
    if a.type_only
        && b.type_only
        && matches!((a.shape, b.shape), (Default, Named) | (Named, Default))
    {
        return false;
    }
    let exclusive = |x: Shape, y: Shape| x == ExportAll && !matches!(y, ExportAll | SideEffect);
    if exclusive(a.shape, b.shape) || exclusive(b.shape, a.shape) {
        return false;
    }
    !matches!((a.shape, b.shape), (Namespace, Named) | (Named, Namespace))
}

#[derive(Default)]
struct Collect {
    decls: Vec<NodeId>,
}

impl Visitor for Collect {
    fn enter(&mut self, ast: &ts_r_ast::Ast, id: NodeId) -> Walk {
        match ast.node(id) {
            Node::ImportDecl { .. } | Node::ExportNamed { .. } | Node::ExportAll { .. } => {
                self.decls.push(id);
                Walk::SkipChildren
            }
            // Imports only appear at the top level and in `declare module` blocks.
            Node::Program { .. } | Node::Module { .. } | Node::ModuleBlock { .. } => Walk::Continue,
            _ => Walk::SkipChildren,
        }
    }
}

fn shape(ast: &ts_r_ast::Ast, specifiers: &[NodeId]) -> Shape {
    let of = |s: NodeId| match ast.node(s) {
        Node::ImportDefaultSpecifier { .. } => Shape::Default,
        Node::ImportNamespaceSpecifier { .. } => Shape::Namespace,
        _ => Shape::Named,
    };
    match specifiers
        .iter()
        .map(|&s| of(s))
        .find(|s| *s != Shape::Default)
    {
        Some(s) => s,
        None if specifiers.is_empty() => Shape::SideEffect,
        None => Shape::Default,
    }
}

impl NoDuplicateImports {
    fn should_report(&self, d: &Decl, previous: &[Decl], export: bool) -> bool {
        previous.iter().filter(|p| p.export == export).any(|p| {
            (!self.allow_separate_type_imports || p.type_only == d.type_only) && can_merge(d, p)
        })
    }
}

impl Rule for NoDuplicateImports {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut c = Collect::default();
        walk(ast, ast.root, &mut c);
        let mut modules: FxHashMap<String, Vec<Decl>> = FxHashMap::default();
        for node in c.decls {
            let (source, d) = match *ast.node(node) {
                Node::ImportDecl {
                    type_only,
                    specifiers,
                    source,
                    ..
                } => (
                    source,
                    Decl {
                        export: false,
                        type_only,
                        shape: shape(ast, ast.list(specifiers)),
                    },
                ),
                Node::ExportNamed {
                    type_only,
                    specifiers,
                    source: Some(source),
                    ..
                } if self.include_exports => (
                    source,
                    Decl {
                        export: true,
                        type_only,
                        shape: shape(ast, ast.list(specifiers)),
                    },
                ),
                Node::ExportAll {
                    type_only,
                    exported,
                    source,
                    ..
                } if self.include_exports => (
                    source,
                    Decl {
                        export: true,
                        type_only,
                        shape: if exported.is_some() {
                            Shape::Namespace
                        } else {
                            Shape::ExportAll
                        },
                    },
                ),
                _ => continue,
            };
            let Node::StringLit { value } = *ast.node(source) else {
                continue;
            };
            let module = ast.atom(value).trim();
            if module.is_empty() {
                continue;
            }
            let previous = modules.entry(module.to_owned()).or_default();
            let mut ids = Vec::new();
            if d.export {
                if self.should_report(&d, previous, true) {
                    ids.push(("export", format!("'{module}' export is duplicated.")));
                }
                if self.should_report(&d, previous, false) {
                    ids.push((
                        "exportAs",
                        format!("'{module}' export is duplicated as import."),
                    ));
                }
            } else {
                if self.should_report(&d, previous, false) {
                    ids.push(("import", format!("'{module}' import is duplicated.")));
                }
                if self.include_exports && self.should_report(&d, previous, true) {
                    ids.push((
                        "importAs",
                        format!("'{module}' import is duplicated as export."),
                    ));
                }
            }
            previous.push(d);
            for (id, msg) in ids {
                cx.report_node(node, msg).id(id);
            }
        }
    }
}
