//! `import/export`: a name exported twice from one module or namespace (`export *` names
//! included), and `export *` of a module with no named exports. Types and values are
//! separate (`export type Foo` beside `export const Foo`), overload signatures don't
//! count, and a namespace may merge with one function, class or enum of its name. Each
//! namespace body (`namespace N {}`, `declare module "m" {}`) is its own scope of names.

use super::import_util::{exports_of, report_parse_errors, string_literal};
use super::prelude::*;
use crate::modules::export_map::{module_name, names};
use rustc_hash::FxHashMap;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/export",
        "Forbid any invalid exports, i.e. re-export of the same name.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<Export>,
};

#[derive(Debug, Default)]
struct Export;

/// What declares an exported name (ESTree's type of the name's parent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decl {
    Function,
    DeclareFunction,
    Class,
    Enum,
    Namespace,
    Other,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    span: Span,
    decl: Decl,
    /// An overload signature (`export function f(): void;`): never a duplicate.
    overload: bool,
}

type Names = Vec<(String, Vec<Entry>)>;

fn add(
    scopes: &mut FxHashMap<NodeId, Names>,
    order: &mut Vec<NodeId>,
    scope: NodeId,
    key: String,
    e: Entry,
) {
    let names = scopes.entry(scope).or_insert_with(|| {
        order.push(scope);
        Vec::new()
    });
    match names.iter_mut().find(|(k, _)| *k == key) {
        Some((_, v)) => v.push(e),
        None => names.push((key, vec![e])),
    }
}

fn decl_of(ast: &ts_r_ast::Ast, d: NodeId) -> Decl {
    match *ast.node(d) {
        Node::FunctionDecl { body: None, .. } => Decl::DeclareFunction,
        Node::FunctionDecl { .. } => Decl::Function,
        Node::ClassDecl { .. } => Decl::Class,
        Node::Enum { .. } => Decl::Enum,
        Node::Module { .. } => Decl::Namespace,
        _ => Decl::Other,
    }
}

/// The namespace merges with the other declarations of its name.
fn is_merging(nodes: &[Entry]) -> bool {
    let mut types: Vec<Decl> = nodes.iter().map(|e| e.decl).collect();
    types.sort_by_key(|d| *d as u8);
    types.dedup();
    let has = |d: Decl| types.contains(&d);
    let non_ns = nodes.iter().filter(|e| e.decl != Decl::Namespace).count();
    has(Decl::Namespace)
        && (types.len() == 1
            || (types.len() == 2 && (has(Decl::Function) || has(Decl::DeclareFunction)))
            || (types.len() == 3 && has(Decl::Function) && has(Decl::DeclareFunction))
            || (types.len() == 2 && (has(Decl::Class) || has(Decl::Enum)) && non_ns == 1))
}

impl Rule for Export {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut scopes: FxHashMap<NodeId, Names> = FxHashMap::default();
        let mut order = Vec::new();
        // (statements, scope key, depth): the program, then namespace bodies.
        let root_stmts: Vec<NodeId> = super::import_util::program_statements(ast).to_vec();
        let mut work: Vec<(Vec<NodeId>, NodeId, u32)> = vec![(root_stmts, ast.root, 0)];
        while let Some((stmts, scope, depth)) = work.pop() {
            for s in stmts {
                self.statement(cx, s, scope, depth, &mut scopes, &mut order, &mut work);
            }
        }
        for scope in order {
            let Some(names) = scopes.get(&scope) else {
                continue;
            };
            for (name, nodes) in names {
                let nodes: Vec<Entry> = nodes.iter().copied().filter(|e| !e.overload).collect();
                if nodes.len() <= 1 || is_merging(&nodes) {
                    continue;
                }
                let has_conflict = nodes.iter().any(|e| {
                    matches!(
                        e.decl,
                        Decl::Enum | Decl::Class | Decl::Function | Decl::DeclareFunction
                    )
                });
                for e in &nodes {
                    if e.decl == Decl::Namespace && has_conflict {
                        continue;
                    }
                    let msg = if name == "default" {
                        "Multiple default exports.".to_owned()
                    } else {
                        let shown = name.strip_prefix("type:").unwrap_or(name);
                        format!("Multiple exports of name '{shown}'.")
                    };
                    cx.report(e.span, msg);
                }
            }
        }
    }
}

impl Export {
    #[allow(clippy::too_many_arguments)]
    fn statement(
        &self,
        cx: &mut Ctx<'_, '_>,
        s: NodeId,
        scope: NodeId,
        depth: u32,
        scopes: &mut FxHashMap<NodeId, Names>,
        order: &mut Vec<NodeId>,
        work: &mut Vec<(Vec<NodeId>, NodeId, u32)>,
    ) {
        let ast = cx.ast();
        let entry = |span, decl| Entry {
            span,
            decl,
            overload: false,
        };
        match *ast.node(s) {
            Node::ExportDefaultExpr { .. } => {
                add(
                    scopes,
                    order,
                    scope,
                    "default".into(),
                    entry(ast.span(s), Decl::Other),
                );
            }
            Node::ExportNamed { specifiers, .. } => {
                for &sp in ast.list(specifiers) {
                    let Node::ExportSpecifier {
                        local, exported, ..
                    } = *ast.node(sp)
                    else {
                        continue;
                    };
                    let at = exported.unwrap_or(local);
                    if let Some(n) = module_name(ast, at) {
                        add(
                            scopes,
                            order,
                            scope,
                            n.to_owned(),
                            entry(cx.span(at), Decl::Other),
                        );
                    }
                }
            }
            Node::ExportAll {
                exported: None,
                source,
                ..
            } => {
                let Some(spec) = string_literal(ast, source) else {
                    return;
                };
                let Some(m) = exports_of(cx, spec) else {
                    return;
                };
                if !m.errors.is_empty() {
                    report_parse_errors(cx, &m, source, spec);
                    return;
                }
                let mut any = false;
                for n in names(cx.file.modules(), &m) {
                    if n != "default" {
                        any = true;
                        add(scopes, order, scope, n, entry(ast.span(s), Decl::Other));
                    }
                }
                if !any {
                    let span = cx.span(source);
                    cx.report(span, format!("No named exports found in module '{spec}'."));
                }
            }
            Node::Module {
                body: Some(body), ..
            } => {
                if ast.node(s).modifiers().contains(Modifiers::EXPORT) {
                    self.declaration(cx, s, scope, scopes, order);
                }
                if depth < 64
                    && let Node::ModuleBlock { stmts } = *ast.node(body)
                {
                    work.push((ast.list(stmts).to_vec(), s, depth + 1));
                }
            }
            _ => {
                if ast.node(s).modifiers().contains(Modifiers::EXPORT) {
                    self.declaration(cx, s, scope, scopes, order);
                }
            }
        }
    }

    /// `export <declaration>` (and `export default <declaration>`).
    fn declaration(
        &self,
        cx: &mut Ctx<'_, '_>,
        d: NodeId,
        scope: NodeId,
        scopes: &mut FxHashMap<NodeId, Names>,
        order: &mut Vec<NodeId>,
    ) {
        let ast = cx.ast();
        let mods = ast.node(d).modifiers();
        let decl = decl_of(ast, d);
        if mods.contains(Modifiers::DEFAULT) {
            add(
                scopes,
                order,
                scope,
                "default".into(),
                Entry {
                    span: ast.span(d),
                    decl: Decl::Other,
                    overload: decl == Decl::DeclareFunction,
                },
            );
            return;
        }
        match *ast.node(d) {
            Node::VarDecl { decls, .. } => {
                for &v in ast.list(decls) {
                    let Node::VarDeclarator { name, .. } = *ast.node(v) else {
                        continue;
                    };
                    let mut ids = Vec::new();
                    binding_idents(ast, name, &mut ids);
                    for id in ids {
                        if let Some(n) = ident_name(ast, id) {
                            let e = Entry {
                                span: cx.span(id),
                                decl: Decl::Other,
                                overload: false,
                            };
                            add(scopes, order, scope, n.to_owned(), e);
                        }
                    }
                }
            }
            Node::TypeAlias { name, .. } | Node::Interface { name, .. } => {
                if let Some(n) = ident_name(ast, name) {
                    let e = Entry {
                        span: cx.span(name),
                        decl: Decl::Other,
                        overload: false,
                    };
                    add(scopes, order, scope, format!("type:{n}"), e);
                }
            }
            Node::FunctionDecl {
                name: Some(name), ..
            }
            | Node::ClassDecl {
                name: Some(name), ..
            }
            | Node::Enum { name, .. }
            | Node::Module { name, .. } => {
                if let Some(n) = ident_name(ast, name) {
                    let e = Entry {
                        span: cx.span(name),
                        decl,
                        overload: decl == Decl::DeclareFunction,
                    };
                    add(scopes, order, scope, n.to_owned(), e);
                }
            }
            _ => {}
        }
    }
}
