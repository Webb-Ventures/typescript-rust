//! Merged declarations that mix exported and local parts (TS2395), and named default
//! exports merged with other declarations (TS2652, see `check_default_merges`).
//!
//! Rules observed from tsc 7.0.2, per container (a module file, a namespace body, an
//! ambient module body):
//! - Each top-level declaration has a name and the meanings it declares: interfaces and
//!   type aliases a type; functions and variables a value; classes and enums a type and a
//!   value; a namespace a namespace (and a value when instantiated); an `import x =` alias
//!   all three.
//! - When a name has both `export`ed and local declarations, every declaration whose
//!   meanings overlap those of the other group gets TS2395 at its name (`interface b {}`
//!   with `export const b = 1` is fine; `var f` with `export function f()` is not).
//! - Ambient bodies (`declare namespace`, `declare module "x"`, declaration files) export
//!   everything implicitly, so they are only checked when they contain an `export =`,
//!   `export default` or `export { }` statement.
//! - `export default` declarations and `import` bindings do not take part. An `import x =`
//!   alias means what its target means. All-function groups are overloads (TS2383); a type
//!   alias with a class, interface or enum, and an enum with anything but enums and
//!   namespaces, are duplicates (TS2300/TS2567) and not reported here.

use rustc_hash::FxHashMap;
use ts_r_ast::{Modifiers, ModuleKind, Node, NodeId, NodeList};
use ts_r_binder::SymbolFlags;
use ts_r_diagnostics::FileId;

use crate::checker::{Ck, Loc};
use crate::diag::dg;

const TYPE: u8 = 1;
const VALUE: u8 = 2;
const NAMESPACE: u8 = 4;

/// Namespace nesting followed.
const MAX_DEPTH: u32 = 64;

impl<'c, 'a> Ck<'c, 'a> {
    /// Checks every container of `file` for TS2395.
    pub(crate) fn check_export_local_merges(&mut self, file: FileId) {
        let f = self.c.file(file);
        if !self.reporting(file) || f.is_js() {
            return;
        }
        let ast = self.ast(file);
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return;
        };
        let ambient = f.kind.is_declaration();
        let is_module = f.bound.is_module;
        self.check_container(file, stmts, ambient, is_module, 0);
    }

    fn check_container(
        &mut self,
        file: FileId,
        stmts: NodeList,
        ambient: bool,
        check_here: bool,
        depth: u32,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        let ast = self.ast(file);
        let explicit = ast.list(stmts).iter().any(|&s| {
            matches!(
                ast.node(s),
                Node::ExportAssignment { .. }
                    | Node::ExportDefaultExpr { .. }
                    | Node::ExportNamed { source: None, .. }
            ) || ast.node(s).modifiers().contains(Modifiers::DEFAULT)
        });
        if check_here && (!ambient || explicit) {
            self.check_merged_exports(file, stmts);
        }
        for &s in ast.list(stmts) {
            let Node::Module {
                modifiers,
                kind,
                name,
                body: Some(b),
            } = *ast.node(s)
            else {
                continue;
            };
            if kind == ModuleKind::Global {
                continue;
            }
            // `namespace A.B {}`: the block belongs to the innermost name.
            let mut body = b;
            for _ in 0..MAX_DEPTH {
                match *ast.node(body) {
                    Node::Module {
                        body: Some(inner), ..
                    } => body = inner,
                    _ => break,
                }
            }
            let Node::ModuleBlock { stmts: inner } = *ast.node(body) else {
                continue;
            };
            let inner_ambient = ambient
                || modifiers.contains(Modifiers::DECLARE)
                || matches!(ast.node(name), Node::StringLit { .. });
            self.check_container(file, inner, inner_ambient, true, depth + 1);
        }
    }

    fn check_merged_exports(&mut self, file: FileId, stmts: NodeList) {
        let ast = self.ast(file);
        // (name text, name node, exported, meanings, kind)
        let mut decls: Vec<(&str, NodeId, bool, u8, Kind)> = Vec::new();
        // Named `export default` declarations, the same way.
        let mut defaults: Vec<(&str, NodeId, bool, u8, Kind)> = Vec::new();
        for &s in ast.list(stmts) {
            let node = *ast.node(s);
            let m = node.modifiers();
            let is_default = m.contains(Modifiers::DEFAULT);
            let exported = m.contains(Modifiers::EXPORT);
            let mut push = |n: NodeId, meanings: u8, kind: Kind| {
                if let Node::Ident { name } = *ast.node(n) {
                    let list = if is_default {
                        &mut defaults
                    } else {
                        &mut decls
                    };
                    list.push((ast.atom(name), n, exported, meanings, kind));
                }
            };
            match node {
                Node::VarDecl { decls: list, .. } => {
                    for &d in ast.list(list) {
                        if let Node::VarDeclarator { name, .. } = *ast.node(d) {
                            push(name, VALUE, Kind::Other);
                        }
                    }
                }
                Node::FunctionDecl { name: Some(n), .. } => push(n, VALUE, Kind::Function),
                Node::ClassDecl { name: Some(n), .. } => push(n, TYPE | VALUE, Kind::TypeDecl),
                Node::Interface { name, .. } => push(name, TYPE, Kind::TypeDecl),
                Node::TypeAlias { name, .. } => push(name, TYPE, Kind::TypeAlias),
                Node::Enum { name, .. } => push(name, TYPE | VALUE, Kind::Enum),
                Node::ImportEquals { name, .. } => {
                    // An alias means what its target means (nothing when unresolved).
                    let meanings = self
                        .c
                        .sym_of_decl(Loc::new(file, s))
                        .and_then(|a| self.c.resolve_alias(a))
                        .map_or(0, |t| {
                            let f = self.c.sym_flags(t);
                            let mut m = 0;
                            if f.intersects(SymbolFlags::TYPE) {
                                m |= TYPE;
                            }
                            if f.intersects(SymbolFlags::VALUE) {
                                m |= VALUE;
                            }
                            if f.intersects(SymbolFlags::NAMESPACE) {
                                m |= NAMESPACE;
                            }
                            m
                        });
                    push(name, meanings, Kind::Other)
                }
                Node::Module { kind, name, .. } if kind != ModuleKind::Global => {
                    let n = match *ast.node(name) {
                        Node::QualifiedName { .. } => leftmost(ast, name),
                        _ => name,
                    };
                    let instantiated = self.c.sym_of_decl(Loc::new(file, s)).is_some_and(|sym| {
                        self.c.sym_flags(sym).contains(SymbolFlags::VALUE_MODULE)
                    });
                    let meanings = if instantiated {
                        NAMESPACE | VALUE
                    } else {
                        NAMESPACE
                    };
                    push(n, meanings, Kind::Namespace);
                }
                _ => {}
            }
        }
        let with_default = self.check_default_merges(file, &defaults, &decls);
        if !decls.iter().any(|d| d.2) || !decls.iter().any(|d| !d.2) {
            return;
        }
        // Per name: meanings (exported, local) and the kinds seen.
        let mut groups: FxHashMap<&str, Group> = FxHashMap::default();
        for &(name, _, exported, meanings, kind) in &decls {
            let g = groups.entry(name).or_default();
            if exported {
                g.exported |= meanings;
            } else {
                g.local |= meanings;
            }
            g.kinds |= kind as u8;
        }
        let mut found: Vec<(NodeId, String)> = Vec::new();
        for &(name, n, exported, meanings, _) in &decls {
            let g = groups.get(name).copied().unwrap_or_default();
            // Overloads (all functions) are TS2383; a type alias with a class, interface
            // or enum, and an enum with anything but enums and namespaces, are duplicates
            // (TS2300/TS2567) instead.
            if g.kinds == Kind::Function as u8
                || (g.kinds & Kind::TypeAlias as u8 != 0
                    && g.kinds & (Kind::TypeDecl as u8 | Kind::Enum as u8) != 0)
                || (g.kinds & Kind::Enum as u8 != 0
                    && g.kinds & !(Kind::Enum as u8 | Kind::Namespace as u8) != 0)
            {
                continue;
            }
            let other = if exported { g.local } else { g.exported };
            if other & meanings != 0 && !with_default.contains(&n) {
                found.push((n, name.to_owned()));
            }
        }
        for (n, name) in found {
            self.error(
                Loc::new(file, n),
                dg::INDIVIDUAL_DECLARATIONS_IN_MERGED_DECLARATION_0_MUST_BE_ALL_EXPORTED_OR_ALL_LOCA_2395,
                &[&name],
            );
        }
    }
}

impl<'c, 'a> Ck<'c, 'a> {
    /// TS2652: a named `export default` declaration merged with other declarations of its
    /// name in the same body, at the default one and at each other one sharing a meaning
    /// with it (an `export default function f` with an instantiated `namespace f`, an
    /// `export default class C` with `interface C`). Function overloads (TS2393) and enums
    /// (TS2567) are left out. Returns the declarations reported, which TS2395 skips.
    fn check_default_merges(
        &mut self,
        file: FileId,
        defaults: &[(&str, NodeId, bool, u8, Kind)],
        decls: &[(&str, NodeId, bool, u8, Kind)],
    ) -> Vec<NodeId> {
        let mut found: Vec<(NodeId, String)> = Vec::new();
        for &(name, n, _, meanings, kind) in defaults {
            let others: Vec<NodeId> = decls
                .iter()
                .filter(|d| {
                    d.0 == name
                        && self.own_meanings(file, d.1, d.3, d.4) & meanings != 0
                        && !matches!(d.4, Kind::Enum)
                        && !(matches!(kind, Kind::Function) && matches!(d.4, Kind::Function))
                })
                .map(|d| d.1)
                .collect();
            if others.is_empty() {
                continue;
            }
            for at in std::iter::once(n).chain(others) {
                if !found.iter().any(|f| f.0 == at) {
                    found.push((at, name.to_owned()));
                }
            }
        }
        for (n, name) in &found {
            self.error(
                Loc::new(file, *n),
                dg::MERGED_DECLARATION_0_CANNOT_INCLUDE_A_DEFAULT_EXPORT_DECLARATION_CONSIDER_ADDING_2652,
                &[name],
            );
        }
        found.into_iter().map(|f| f.0).collect()
    }
}

impl<'c, 'a> Ck<'c, 'a> {
    /// The meanings of one declaration: a namespace declares a value only when its own body
    /// does (the merged symbol may be instantiated by another part).
    fn own_meanings(&self, file: FileId, name: NodeId, meanings: u8, kind: Kind) -> u8 {
        if !matches!(kind, Kind::Namespace) {
            return meanings;
        }
        let ast = self.ast(file);
        let mut cur = Loc::new(file, name);
        for _ in 0..MAX_DEPTH {
            let Some(p) = self.c.parent(cur) else {
                break;
            };
            if let Node::Module { .. } = ast.node(p.node) {
                return if crate::merge_order::namespace_instantiated(ast, p.node, 0) {
                    NAMESPACE | VALUE
                } else {
                    NAMESPACE
                };
            }
            cur = p;
        }
        meanings
    }
}

/// Declaration kinds that change the rule (bit flags).
#[derive(Clone, Copy)]
#[repr(u8)]
enum Kind {
    Other = 1,
    TypeDecl = 32,
    Function = 2,
    TypeAlias = 4,
    Enum = 8,
    Namespace = 16,
}

#[derive(Default, Clone, Copy)]
struct Group {
    exported: u8,
    local: u8,
    kinds: u8,
}

/// The first identifier of a qualified name.
fn leftmost(ast: &ts_r_ast::Ast, mut n: NodeId) -> NodeId {
    for _ in 0..MAX_DEPTH {
        match *ast.node(n) {
            Node::QualifiedName { left, .. } => n = left,
            _ => break,
        }
    }
    n
}
