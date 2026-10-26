//! Members related through inheritance, for references, rename and highlights.
//!
//! A member of a class or interface is one with the members of the same name its
//! container inherits from (base classes, implemented interfaces and extended interfaces,
//! at any depth). Two members are related when the sets of declarations reached that way
//! from each of them share one: an override and the member it overrides, two overrides of
//! one base member, and a property implementing an interface's property. Static members
//! are related to nothing (bases are looked up on instance types, as tsc 7 does).
//! Rule from `tsc: tsc/internal/ls/findallreferences.go` (`forEachRelatedSymbol`,
//! `getPropertySymbolsFromBaseTypes`), v7.0.2.

use std::collections::HashSet;
use ts_r_ast::{HeritageKind, Modifiers, Node};
use ts_r_checker::{Ck, Loc};
use ts_r_types::SymId;

/// At most this many containers are visited from one member (cyclic or huge hierarchies).
const MAX_CONTAINERS: usize = 256;

/// The class or interface declaring the member declaration `d`, unless `d` is static or not
/// a class or interface member.
fn container(ck: &Ck<'_, '_>, d: Loc) -> Option<Loc> {
    let c = ck.c;
    let (member, is_param) = match *c.node(d) {
        Node::PropertyDef { modifiers, .. } | Node::MethodDef { modifiers, .. } => {
            if modifiers.contains(Modifiers::STATIC) {
                return None;
            }
            (d, false)
        }
        Node::PropertySignature { .. } | Node::MethodSignature { .. } => (d, false),
        Node::Param { modifiers, .. } if !modifiers.is_empty() => (d, true),
        _ => return None,
    };
    let mut p = c.parent(member)?;
    if is_param {
        // `constructor(public x)`: Param -> function -> constructor -> class.
        for _ in 0..3 {
            if matches!(c.node(p), Node::ClassDecl { .. } | Node::ClassExpr { .. }) {
                break;
            }
            p = c.parent(p)?;
        }
    }
    matches!(
        c.node(p),
        Node::ClassDecl { .. } | Node::ClassExpr { .. } | Node::Interface { .. }
    )
    .then_some(p)
}

/// The declarations of `s` and of every member named like it that its containers inherit.
/// Empty when `s` is not a non-static class or interface member.
pub fn ancestor_decls(ck: &mut Ck<'_, '_>, s: SymId) -> HashSet<Loc> {
    let c = ck.c;
    let name = c.sym_name(s).to_string();
    let own = c.decls(s);
    let mut queue: Vec<Loc> = own.iter().filter_map(|&d| container(ck, d)).collect();
    if queue.is_empty() {
        return HashSet::new();
    }
    let mut out: HashSet<Loc> = own.into_iter().collect();
    let mut seen: HashSet<Loc> = HashSet::new();
    while let Some(k) = queue.pop() {
        if !seen.insert(k) || seen.len() > MAX_CONTAINERS {
            continue;
        }
        let Some(ksym) = c.sym_of_decl(k) else {
            continue;
        };
        // A class in its own base chain has no base (TS2506); interfaces still name
        // theirs (tsc looks bases up from the heritage type nodes).
        let ast = c.ast(k.file);
        let is_class = matches!(
            ast.node(k.node),
            Node::ClassDecl { .. } | Node::ClassExpr { .. }
        );
        if is_class && in_base_cycle(ck, ksym) {
            continue;
        }
        let mut bases: Vec<_> = ck.base_types(ksym).into_vec();
        // `implements I` of a class.
        if let Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. } =
            *ast.node(k.node)
        {
            for &h in ast.list(heritage) {
                if let Node::HeritageClause {
                    kind: HeritageKind::Implements,
                    types,
                } = *ast.node(h)
                {
                    for &t in ast.list(types) {
                        bases.push(ck.type_from_node(Loc::new(k.file, t)));
                    }
                }
            }
        }
        for b in bases {
            let Some(p) = ck.property_of_type(b, &name) else {
                continue;
            };
            for d in c.decls(p) {
                if out.insert(d)
                    && let Some(k2) = container(ck, d)
                {
                    queue.push(k2);
                }
            }
        }
    }
    out
}

/// Whether `s` is reached again by following its base types.
fn in_base_cycle(ck: &mut Ck<'_, '_>, s: SymId) -> bool {
    let mut stack = vec![s];
    let mut seen: HashSet<SymId> = HashSet::new();
    while let Some(k) = stack.pop() {
        if seen.len() > MAX_CONTAINERS {
            return false;
        }
        for b in ck.base_types(k).iter() {
            let Some(bs) = super::nav::type_symbol(ck, *b) else {
                continue;
            };
            if bs == s {
                return true;
            }
            if seen.insert(bs) {
                stack.push(bs);
            }
        }
    }
    false
}
