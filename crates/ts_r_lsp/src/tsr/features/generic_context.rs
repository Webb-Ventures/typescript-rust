//! What an object literal passed to a generic call may hold, apart from what was inferred
//! from the literal itself.
//!
//! A literal argument of `f<T extends Opts>(o: Partial<T>)` has `Partial<T>` instantiated
//! with what the call inferred from it as its contextual type, so the members it already
//! writes are all the context offers. tsc 7 also lists the members of a second,
//! "completions" type: the declared parameter type with the type parameters this argument
//! decided replaced by their constraints (observed on `completionsWithOptionalProperties*`
//! and `completionsObjectLiteralWithPartialConstraint`; rule read in
//! `tsc: tsc/internal/ls/completions.go`, the object-like completion symbols). Here a type
//! parameter keeps its inferred type when another parameter that has an argument mentions
//! it (`createColor<K>(kind: K, values: Colors[K])` with `kind` given), and takes its
//! constraint (`unknown` without one) otherwise; written type arguments leave nothing to
//! add. Nested literals (`f({ a: { | } })`) read
//! the property (or string index) of the enclosing literal's completions type.

use ts_r_ast::Node;
use ts_r_checker::{Ck, Loc};
use ts_r_types::TypeId;

/// Enclosing literals and parentheses followed at most.
const MAX_DEPTH: u32 = 32;

/// The completions type of object literal `obj` (see the module docs), or `None` when it
/// is not (inside) an argument of a call to a generic signature.
pub(super) fn completions_type(ck: &mut Ck<'_, '_>, obj: Loc) -> Option<TypeId> {
    completions_type_in(ck, obj, 0)
}

fn completions_type_in(ck: &mut Ck<'_, '_>, obj: Loc, depth: u32) -> Option<TypeId> {
    if depth > MAX_DEPTH {
        return None;
    }
    let c = ck.c;
    let parent = c.parent(obj)?;
    match *c.node(parent) {
        Node::Paren { .. } => completions_type_in(ck, parent, depth + 1),
        // Written type arguments decide every type parameter.
        Node::Call {
            type_args: Some(_), ..
        }
        | Node::New {
            type_args: Some(_), ..
        } => None,
        Node::Call { args, .. } | Node::New { args, .. } => {
            let list = c.ast(obj.file).list(args);
            let i = list.iter().position(|&a| a == obj.node)?;
            let argc = list.len();
            let sig = ck.resolved_signature(parent)?;
            let (target, mapper) = ck.store().sig(sig).target?;
            let tps = ck
                .store()
                .list_items(ck.store().sig(target).type_params)
                .to_vec();
            if tps.is_empty() {
                return None;
            }
            let params: Vec<TypeId> = ck.store().sig(target).params.iter().map(|p| p.ty).collect();
            let declared = *params.get(i)?;
            let mut targets = Vec::with_capacity(tps.len());
            for &tp in &tps {
                let probe = ck.store().mapper(&[tp], &[TypeId::NEVER]);
                let decided_elsewhere = params
                    .iter()
                    .enumerate()
                    .any(|(j, &p)| j != i && j < argc && ck.instantiate(p, probe) != p);
                let t = if decided_elsewhere {
                    ck.instantiate(tp, mapper)
                } else {
                    ck.type_param_constraint(tp).unwrap_or(TypeId::UNKNOWN)
                };
                targets.push(t);
            }
            let m = ck.store().mapper(&tps, &targets);
            Some(ck.instantiate(declared, m))
        }
        Node::Property { key, value, .. } if value == obj.node => {
            let outer = c.parent(parent)?;
            if !matches!(c.node(outer), Node::ObjectLit { .. }) {
                return None;
            }
            // An optional parameter or property is `T | undefined`: its members are `T`'s.
            let t = completions_type_in(ck, outer, depth + 1)?;
            let t = ck.non_nullable(t);
            let name = super::locate::key_text(ck, Loc::new(obj.file, key))?;
            if let Some(p) = super::locate::property_of(ck, t, &name) {
                return Some(ck.type_of_symbol(p));
            }
            ck.index_info_of(t, TypeId::STRING).map(|ix| ix.value)
        }
        _ => None,
    }
}
