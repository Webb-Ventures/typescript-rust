//! Flow narrowing queries for sound mode (`ts_r_sound`, design record 0007: S-07 proves
//! user-defined type guards by narrowing the parameter with the guard's own conditions).
//! Additive and read-only: these run the same condition narrowing the backwards flow walk
//! uses, for one parameter, without a flow node.

use std::hash::{Hash, Hasher};

use rustc_hash::FxHasher;
use ts_r_types::{SymId, TypeId};

use super::{RefKey, RefRoot, Walk};
use crate::checker::{Ck, Loc};

impl Ck<'_, '_> {
    /// The type of parameter (or local) `sym` named `name`, currently `t` and declared
    /// `declared`, after condition `cond` (an expression of `sym`'s function) is assumed
    /// true or false. Recognises what the flow walk recognises: `typeof`, equality,
    /// truthiness, discriminants, `instanceof`, `in`, type predicate calls, `!`, `&&`, `||`.
    pub fn narrow_symbol_by_condition(
        &mut self,
        sym: SymId,
        name: &str,
        declared: TypeId,
        t: TypeId,
        cond: Loc,
        assume_true: bool,
    ) -> TypeId {
        let key = RefKey {
            root: RefRoot::Sym(sym),
            path: Vec::new(),
        };
        let mut h = FxHasher::default();
        key.hash(&mut h);
        let w = Walk {
            key,
            hash: h.finish(),
            root_name: name.into(),
            declared,
            initial: declared,
            cross_functions: false,
            file: cond.file,
            at: None,
        };
        self.narrow(&w, t, cond, assume_true, 0)
    }

    /// The type a caller gets in the true or false branch of a type predicate `x is pt`
    /// applied to a value of type `t`.
    pub fn narrow_by_type_predicate(&mut self, t: TypeId, pt: TypeId, assume_true: bool) -> TypeId {
        self.narrow_by_predicate_type(t, pt, assume_true)
    }
}
