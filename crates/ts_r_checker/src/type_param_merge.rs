//! Merged classes and interfaces must declare the same type parameters (TS2428).
//!
//! Rules observed from tsc 7.0.2: every class and interface declaration of a merged symbol
//! is reported at its name when any two of them differ in the names of their type
//! parameters, or both write a constraint (or both a default) for one parameter and those
//! are not the same type, or one leaves out a parameter that no declaration gives a
//! default (`interface I {}` next to `interface I<T = number> {}` is fine). A missing
//! constraint or default, and variance modifiers, do not count.

use ts_r_ast::visit::{Visitor, Walk, walk};
use ts_r_ast::{Node, NodeId};
use ts_r_binder::meaning;
use ts_r_diagnostics::FileId;
use ts_r_types::TypeId;

use crate::checker::{Ck, Loc};
use crate::diag::dg;

/// One declaration's type parameters: name, constraint and default nodes.
type Params = Vec<(String, Option<NodeId>, Option<NodeId>)>;

impl<'c, 'a> Ck<'c, 'a> {
    /// Checks class or interface declaration `d` against the other declarations of its
    /// symbol.
    pub(crate) fn check_type_parameter_merge(&mut self, d: Loc) {
        if !self.reporting(d.file) || self.c.file(d.file).is_js() {
            return;
        }
        let Some(name) = declaration_name(self.ast(d.file).node(d.node)) else {
            return;
        };
        let Some(sym) = self.c.sym_of_decl(d) else {
            return;
        };
        let decls: Vec<Loc> = self
            .c
            .decls(sym)
            .into_iter()
            .filter(|l| declaration_name(self.node(*l)).is_some())
            .collect();
        if decls.len() < 2 {
            return;
        }
        let all: Vec<Params> = decls.iter().map(|&l| self.written_type_params(l)).collect();
        // A declaration may leave out trailing parameters that have a default in some
        // declaration.
        let max = all.iter().map(Vec::len).max().unwrap_or(0);
        let longest = all
            .iter()
            .find(|p| p.len() == max)
            .cloned()
            .unwrap_or_default();
        let min = (0..max)
            .rev()
            .find(|&i| {
                !all.iter()
                    .any(|p| p.get(i).is_some_and(|x| x.2.is_some()) && p[i].0 == longest[i].0)
            })
            .map_or(0, |i| i + 1);
        let mut same = all.iter().all(|p| p.len() >= min);
        for (i, ps) in all.iter().enumerate().skip(1) {
            if !same {
                break;
            }
            same = self.same_type_params(decls[0], &all[0], decls[i], ps);
        }
        if !same {
            let text = self.c.sym_name(sym).to_owned();
            self.error(
                Loc::new(d.file, name),
                dg::ALL_DECLARATIONS_OF_0_MUST_HAVE_IDENTICAL_TYPE_PARAMETERS_2428,
                &[&text],
            );
        }
    }

    fn written_type_params(&self, d: Loc) -> Params {
        let ast = self.ast(d.file);
        let tps = match *ast.node(d.node) {
            Node::ClassDecl { type_params, .. }
            | Node::ClassExpr { type_params, .. }
            | Node::Interface { type_params, .. } => type_params,
            _ => None,
        };
        let Some(tps) = tps else {
            return Vec::new();
        };
        let Node::TypeParams { params } = *ast.node(tps) else {
            return Vec::new();
        };
        ast.list(params)
            .iter()
            .filter_map(|&p| match *ast.node(p) {
                Node::TypeParam {
                    name,
                    constraint,
                    default,
                    ..
                } => Some((
                    self.c.text(Loc::new(d.file, name)).to_owned(),
                    constraint,
                    default,
                )),
                _ => None,
            })
            .collect()
    }

    fn same_type_params(&mut self, a: Loc, pa: &Params, b: Loc, pb: &Params) -> bool {
        for (x, y) in pa.iter().zip(pb) {
            if x.0 != y.0 {
                return false;
            }
            for (na, nb) in [(x.1, y.1), (x.2, y.2)] {
                if let (Some(na), Some(nb)) = (na, nb)
                    && !self.same_type_node(Loc::new(a.file, na), Loc::new(b.file, nb))
                {
                    return false;
                }
            }
        }
        true
    }

    /// Whether two type nodes denote the same type (or are written the same).
    fn same_type_node(&mut self, a: Loc, b: Loc) -> bool {
        let squash = |s: &str| s.split_whitespace().collect::<String>();
        if squash(self.c.text(a)) == squash(self.c.text(b)) {
            return true;
        }
        let ta = self.quietly(|ck| ck.type_from_node(a));
        let tb = self.quietly(|ck| ck.type_from_node(b));
        if ta == TypeId::UNRESOLVED || tb == TypeId::UNRESOLVED {
            return true;
        }
        ta == tb
    }
}

impl<'c, 'a> Ck<'c, 'a> {
    /// A type parameter list's defaults: a parameter without a default after one with a
    /// default is TS2706 (at the parameter), and a default naming its own parameter or a
    /// later one of the list is TS2744 (at the reference). Names are resolved, so a
    /// parameter of a nested signature that shadows one is fine (tsc:
    /// tsc/internal/checker/checker.go, checkTypeParameters).
    pub(crate) fn check_type_param_defaults(&mut self, file: FileId, list: NodeId) {
        let ast = self.ast(file);
        let Node::TypeParams { params } = *ast.node(list) else {
            return;
        };
        let params = ast.list(params).to_vec();
        let syms: Vec<_> = params
            .iter()
            .map(|&p| self.c.sym_of_decl(Loc::new(file, p)))
            .collect();
        let mut seen_default = false;
        for (i, &p) in params.iter().enumerate() {
            let Node::TypeParam { default, .. } = *ast.node(p) else {
                continue;
            };
            let Some(d) = default else {
                if seen_default {
                    self.error(
                        Loc::new(file, p),
                        dg::REQUIRED_TYPE_PARAMETERS_MAY_NOT_FOLLOW_OPTIONAL_TYPE_PARAMETERS_2706,
                        &[],
                    );
                }
                continue;
            };
            seen_default = true;
            let mut refs = TypeRefs(Vec::new());
            walk(ast, d, &mut refs);
            for (r, name) in refs.0 {
                let Some(text) = crate::nodes::ident_text(ast, name) else {
                    continue;
                };
                let Some(s) = self.resolve_name_at(Loc::new(file, name), text, meaning::TYPE)
                else {
                    continue;
                };
                if syms[i..].contains(&Some(s)) {
                    self.error(
                        Loc::new(file, r),
                        dg::TYPE_PARAMETER_DEFAULTS_CAN_ONLY_REFERENCE_PREVIOUSLY_DECLARED_TYPE_PARAMETERS_2744,
                        &[],
                    );
                }
            }
        }
    }
}

/// The type references under a node whose name is a plain identifier: `(reference, name)`.
struct TypeRefs(Vec<(NodeId, NodeId)>);

impl Visitor for TypeRefs {
    fn enter(&mut self, ast: &ts_r_ast::Ast, id: NodeId) -> Walk {
        if let Node::TypeRef { name, .. } = *ast.node(id)
            && matches!(ast.node(name), Node::Ident { .. })
        {
            self.0.push((id, name));
        }
        Walk::Continue
    }
}

/// The name of a class or interface declaration.
fn declaration_name(n: &Node) -> Option<NodeId> {
    match *n {
        Node::ClassDecl { name, .. } | Node::ClassExpr { name, .. } => name,
        Node::Interface { name, .. } => Some(name),
        _ => None,
    }
}
