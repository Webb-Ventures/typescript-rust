//! What a type argument is expected to be: the constraint of the type parameter it fills
//! (`Bar<{ | }>` with `interface Bar<T extends Foo>`), and below it, the type a part of that
//! argument is expected to have (`Bar<{ two: { | } }>` expects `Foo["two"]`).
//!
//! Rules learned from tsc 7 (`tsc: tsc/internal/checker/services.go`, the type argument
//! constraint query, and `tsc: tsc/internal/ls/completions.go`, the constraint of a type
//! argument's property), written here on our AST:
//!
//! - A type argument of a call, `new`, tagged template, JSX tag or decorator expects the union
//!   of the constraints the parameter at its position has across the callee's signatures
//!   (uninstantiated: the call is not resolved for this).
//! - A type argument of an instantiation expression (`f<{ | }>;`) expects the call
//!   signatures' constraint, the construct signatures' one, or both intersected.
//! - A type argument of a type reference or heritage clause expects its type parameter's
//!   constraint, instantiated with the arguments written (missing ones keep the parameter).
//! - Below a type argument, a property signature expects the property of that name in what
//!   its type literal expects; type literals, unions and intersections pass their
//!   expectation down unchanged.

use ts_r_ast::{Node, NodeId};
use ts_r_binder::meaning;
use ts_r_checker::members::SigKind;
use ts_r_checker::{Ck, Loc};
use ts_r_types::{SigId, TypeFlags, TypeId};

/// Levels walked up from a type node to the type argument it sits in.
const MAX_DEPTH: u32 = 64;

/// The constraint the type argument `arg` must meet, when `arg` is one (see the module docs).
pub fn argument_constraint(ck: &mut Ck<'_, '_>, arg: Loc) -> Option<TypeId> {
    let c = ck.c;
    let ast = c.ast(arg.file);
    let list = c.parent(arg)?;
    let Node::TypeArgs { args } = *ast.node(list.node) else {
        return None;
    };
    let index = ast.list(args).iter().position(|&a| a == arg.node)?;
    let owner = c.parent(list)?;
    let at = |n: NodeId| Loc::new(arg.file, n);
    match *ast.node(owner.node) {
        Node::Call { callee, .. } => {
            let sigs = callee_signatures(ck, at(callee), SigKind::Call);
            across_signatures(ck, &sigs, index)
        }
        Node::New { callee, .. } => {
            let sigs = callee_signatures(ck, at(callee), SigKind::Construct);
            across_signatures(ck, &sigs, index)
        }
        Node::TaggedTemplate { tag, .. } => {
            let sigs = callee_signatures(ck, at(tag), SigKind::Call);
            across_signatures(ck, &sigs, index)
        }
        Node::JsxOpening { name, .. } => {
            // Intrinsic tags (`<div<T>>`) have no signatures.
            if matches!(ast.node(name), Node::Ident { name } if ast.atom(*name).starts_with(|ch: char| ch.is_ascii_lowercase()))
            {
                return None;
            }
            let sigs = callee_signatures(ck, at(name), SigKind::Call);
            across_signatures(ck, &sigs, index)
        }
        Node::Instantiation { expr, .. } => {
            let t = ck.quietly(|ck| ck.check_expr(at(expr)));
            let a = ck.apparent_type(t);
            let calls = ck.signatures_of(a, SigKind::Call);
            let news = ck.signatures_of(a, SigKind::Construct);
            let call = across_signatures(ck, &calls, index);
            let new = across_signatures(ck, &news, index);
            match (call, new) {
                (Some(x), Some(y)) => Some(ck.intersection(&[x, y])),
                (x, y) => x.or(y),
            }
        }
        Node::TypeRef { name, .. } | Node::ExprWithTypeArgs { expr: name, .. } => {
            let s = match *ast.node(name) {
                Node::Ident { .. } => c
                    .resolve_ident(at(name), meaning::TYPE)
                    .or_else(|| super::locate::entity_symbol(ck, at(name))),
                _ => super::locate::entity_symbol(ck, at(name)),
            }?;
            let s = c.resolve_alias(s).unwrap_or(s);
            let params = ck.local_type_params(s);
            let tp = *params.get(index)?;
            let k = ck.type_param_constraint(tp)?;
            // Instantiate with the arguments written (`Pick<I, "|">`: `keyof I`).
            let written: Vec<NodeId> = ast.list(args).to_vec();
            let mut sources = Vec::new();
            let mut targets = Vec::new();
            for (i, &p) in params.iter().enumerate() {
                let Some(&w) = written.get(i) else { break };
                if i == index {
                    continue;
                }
                sources.push(p);
                targets.push(ck.type_from_node(at(w)));
            }
            if sources.is_empty() {
                return Some(k);
            }
            let m = ck.store().mapper(&sources, &targets);
            Some(ck.instantiate(k, m))
        }
        _ => None,
    }
}

/// Signatures of the callee's (apparent) type.
fn callee_signatures(ck: &mut Ck<'_, '_>, callee: Loc, kind: SigKind) -> Vec<SigId> {
    let t = ck.quietly(|ck| ck.check_expr(callee));
    let a = ck.apparent_type(t);
    ck.signatures_of(a, kind)
}

/// The union of the constraints of the type parameter at `index` across `sigs` (`None` when
/// no signature has one there).
fn across_signatures(ck: &mut Ck<'_, '_>, sigs: &[SigId], index: usize) -> Option<TypeId> {
    let mut found = Vec::new();
    for &s in sigs {
        let tps = ck
            .store()
            .list_items(ck.store().sig(s).type_params)
            .to_vec();
        if let Some(&tp) = tps.get(index)
            && let Some(k) = ck.type_param_constraint(tp)
            && !found.contains(&k)
        {
            found.push(k);
        }
    }
    match found.len() {
        0 => None,
        1 => Some(found[0]),
        _ => Some(ck.union(&found)),
    }
}

/// The type `node` (a type node or a property signature) is expected to have because it sits
/// in a type argument: the argument's constraint, or the part of it that `node` describes.
pub fn expected_type(ck: &mut Ck<'_, '_>, node: Loc) -> Option<TypeId> {
    expected_at(ck, node, 0)
}

fn expected_at(ck: &mut Ck<'_, '_>, node: Loc, depth: u32) -> Option<TypeId> {
    if depth > MAX_DEPTH {
        return None;
    }
    let c = ck.c;
    let ast = c.ast(node.file);
    if let Some(k) = argument_constraint(ck, node) {
        return Some(k);
    }
    match *ast.node(node.node) {
        Node::PropertySignature { key, .. } => {
            let outer = c.parent(node)?;
            let t = expected_at(ck, outer, depth + 1)?;
            let name = ts_r_checker::nodes::key_text(ast, key)?;
            property_type(ck, t, &name)
        }
        Node::TypeLiteral { .. }
        | Node::UnionType { .. }
        | Node::IntersectionType { .. }
        | Node::TupleType { .. } => {
            let outer = c.parent(node)?;
            match *ast.node(outer.node) {
                // A property's type: what the property is expected to be.
                Node::PropertySignature { type_ann, .. } if type_ann == Some(node.node) => {
                    expected_at(ck, outer, depth + 1)
                }
                Node::UnionType { .. } | Node::IntersectionType { .. } => {
                    expected_at(ck, outer, depth + 1)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// The type of property `name` of `t` (of each union member that has it).
fn property_type(ck: &mut Ck<'_, '_>, t: TypeId, name: &str) -> Option<TypeId> {
    let mut found = Vec::new();
    for m in ck.union_members(t) {
        if ck.store().flags(m).intersects(TypeFlags::ANY_OR_UNKNOWN) {
            continue;
        }
        let a = ck.apparent_type(m);
        if let Some(p) = ck.property_of_type(a, name) {
            let pt = ck.type_of_symbol(p);
            if !found.contains(&pt) {
                found.push(pt);
            }
        }
    }
    match found.len() {
        0 => None,
        1 => Some(found[0]),
        _ => Some(ck.union(&found)),
    }
}
