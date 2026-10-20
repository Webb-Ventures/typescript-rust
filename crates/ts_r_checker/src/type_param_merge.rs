//! Merged classes and interfaces must declare the same type parameters (TS2428).
//!
//! Rules observed from tsc 7.0.2: every class and interface declaration of a merged symbol
//! is reported at its name when any two of them differ in the names of their type
//! parameters, or both write a constraint (or both a default) for one parameter and those
//! are not the same type, or one leaves out a parameter that no declaration gives a
//! default (`interface I {}` next to `interface I<T = number> {}` is fine). A missing
//! constraint or default, and variance modifiers, do not count.

use ts_r_ast::{Node, NodeId};
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

/// The name of a class or interface declaration.
fn declaration_name(n: &Node) -> Option<NodeId> {
    match *n {
        Node::ClassDecl { name, .. } | Node::ClassExpr { name, .. } => name,
        Node::Interface { name, .. } => Some(name),
        _ => None,
    }
}
