//! Index signature key types written as type references, unions and other non-keyword
//! types (TS1268/TS1337). Keyword and template literal key types are the parser's
//! (`ts_r_parser` grammar), as are the other index signature parameter errors.
//!
//! Rules observed from tsc 7.0.2 (reported at the parameter name):
//! - Valid keys: `string`, `number`, `symbol`, template literal types, string mapping types
//!   (`Uppercase<string>`), intersections with a valid member (`string & { __b: 1 }`) and
//!   unions of those, through aliases (`type U = string | number`).
//! - A generic key (`T`, `T extends string`) or one with a string, number or enum literal
//!   member (`'a' | 'b'`, an enum, `'a' | RegExp`) is TS1337. Boolean and bigint literals
//!   don't count (`true`, `boolean`, `1n` are TS1268).
//! - Anything else (an interface, a class, `RegExp`, `string | boolean`, `never`, an
//!   unresolved name) is TS1268.
//! - `unique symbol` is TS1335 (not checked here).
//! - A valid key on a signature without a type annotation is TS1021 at the signature.
//!
//! Duplicate index signatures (TS2374), at the signature (modifiers included):
//! - Among the instance index signatures of one symbol (merged interfaces, a class merged
//!   with an interface, a type literal), each key type member (`string | number` has two)
//!   declared by more than one signature is reported on every such signature, once per
//!   type. Valid or not (`'a'`, `Date`, `T`), as long as the type is the same.
//! - Static index signatures, signatures without exactly one typed parameter and
//!   intersections with object types (a fresh type per occurrence) never collide.

use ts_r_ast::{Modifiers, Node, TypeOperatorKind};
use ts_r_binder::Name;
use ts_r_types::{TypeFlags, TypeId};

use crate::checker::{Ck, Loc};
use crate::diag::dg;

impl<'c, 'a> Ck<'c, 'a> {
    /// Checks the key type of the index signature `l`.
    pub(crate) fn check_index_key_type(&mut self, l: Loc) {
        let ast = self.ast(l.file);
        let Node::IndexSignature {
            params,
            type_ann: sig_type,
            ..
        } = *ast.node(l.node)
        else {
            return;
        };
        let &[p] = ast.list(params) else {
            return;
        };
        let Node::Param {
            modifiers,
            rest,
            optional,
            init,
            type_ann: Some(kt),
            pattern,
            ..
        } = *ast.node(p)
        else {
            return;
        };
        if rest
            || optional
            || init.is_some()
            || modifiers.intersects(Modifiers::PUBLIC | Modifiers::PRIVATE | Modifiers::PROTECTED)
            || matches!(
                ast.node(kt),
                Node::KeywordType { .. }
                    | Node::TemplateLitType { .. }
                    | Node::TypeOperator {
                        op: TypeOperatorKind::Unique,
                        ..
                    }
            )
        {
            return;
        }
        let t = self.type_from_node(Loc::new(l.file, kt));
        if self.is_valid_index_key(t) {
            // A valid key written as a reference and no annotation: TS1021 at the
            // signature (the parser's for keyword keys).
            if sig_type.is_none() {
                self.error(
                    l,
                    dg::AN_INDEX_SIGNATURE_MUST_HAVE_A_TYPE_ANNOTATION_1021,
                    &[],
                );
            }
            return;
        }
        let info = if self.is_generic_type(t) || self.has_literal_member(t) {
            dg::AN_INDEX_SIGNATURE_PARAMETER_TYPE_CANNOT_BE_A_LITERAL_TYPE_OR_GENERIC_TYPE_CONSI_1337
        } else {
            dg::AN_INDEX_SIGNATURE_PARAMETER_TYPE_MUST_BE_STRING_NUMBER_SYMBOL_OR_A_TEMPLATE_LIT_1268
        };
        self.error(Loc::new(l.file, pattern), info, &[]);
    }

    /// Reports the key types index signature `l` shares with another index signature of
    /// the same symbol (TS2374).
    pub(crate) fn check_duplicate_index_signatures(&mut self, l: Loc) {
        if !self.reporting(l.file) {
            return;
        }
        let Some(mine) = self.index_key_members(l) else {
            return;
        };
        let Some(sym) = self.c.sym_of_decl(l) else {
            return;
        };
        // Every part of the owner (a class merged with interfaces has a table per part).
        let decls = match self.c.sym_parent(sym) {
            Some(owner) => {
                let mut out = Vec::new();
                for (file, table) in self.c.member_tables(owner) {
                    let ff = self.c.file(file);
                    for &(name, s) in ff.bound.table(table) {
                        if name == Name::INDEX {
                            out.extend(ff.bound.decls(s).iter().map(|&d| Loc::new(file, d)));
                        }
                    }
                }
                out
            }
            None => self.c.decls(sym),
        };
        if decls.len() < 2 {
            return;
        }
        let mut others: Vec<TypeId> = Vec::new();
        for d in decls {
            if d == l || !matches!(self.ast(d.file).node(d.node), Node::IndexSignature { .. }) {
                continue;
            }
            if let Some(ms) = self.index_key_members(d) {
                others.extend(ms);
            }
        }
        let mut reported: Vec<TypeId> = Vec::new();
        for m in mine {
            if others.contains(&m) && !reported.contains(&m) {
                reported.push(m);
                let name = self.type_to_string(m);
                self.error(l, dg::DUPLICATE_INDEX_SIGNATURE_FOR_TYPE_0_2374, &[&name]);
            }
        }
    }

    /// The key type members of instance index signature `l` with exactly one typed
    /// parameter.
    fn index_key_members(&mut self, l: Loc) -> Option<Vec<TypeId>> {
        let ast = self.ast(l.file);
        let Node::IndexSignature {
            modifiers, params, ..
        } = *ast.node(l.node)
        else {
            return None;
        };
        if modifiers.contains(Modifiers::STATIC) {
            return None;
        }
        let &[p] = ast.list(params) else {
            return None;
        };
        let Node::Param {
            type_ann: Some(kt), ..
        } = *ast.node(p)
        else {
            return None;
        };
        let t = self.type_from_node(Loc::new(l.file, kt));
        if t == TypeId::UNRESOLVED || self.store().is_error(t) {
            return None;
        }
        let members = self.union_members(t);
        Some(
            members
                .into_iter()
                .filter(|&m| {
                    !self.store().flags(m).contains(TypeFlags::INTERSECTION)
                        || !self
                            .store()
                            .members_of_union(m)
                            .iter()
                            .any(|&i| self.store().flags(i).intersects(TypeFlags::OBJECT))
                })
                .collect(),
        )
    }

    fn is_valid_index_key(&mut self, t: TypeId) -> bool {
        if t == TypeId::UNRESOLVED || self.store().is_error(t) {
            return false;
        }
        let members = self.union_members(t);
        members.iter().all(|&m| {
            let f = self.store().flags(m);
            if f.intersects(
                TypeFlags::STRING
                    | TypeFlags::NUMBER
                    | TypeFlags::ES_SYMBOL
                    | TypeFlags::TEMPLATE_LITERAL
                    | TypeFlags::STRING_MAPPING,
            ) {
                return true;
            }
            f.contains(TypeFlags::INTERSECTION)
                && self.store().members_of_union(m).iter().any(|&i| {
                    self.store().flags(i).intersects(
                        TypeFlags::STRING
                            | TypeFlags::NUMBER
                            | TypeFlags::ES_SYMBOL
                            | TypeFlags::TEMPLATE_LITERAL
                            | TypeFlags::STRING_MAPPING,
                    )
                })
        })
    }

    fn has_literal_member(&mut self, t: TypeId) -> bool {
        let members = self.union_members(t);
        members.iter().any(|&m| {
            self.store().flags(m).intersects(
                TypeFlags::STRING_LITERAL | TypeFlags::NUMBER_LITERAL | TypeFlags::ENUM_LITERAL,
            )
        })
    }
}
