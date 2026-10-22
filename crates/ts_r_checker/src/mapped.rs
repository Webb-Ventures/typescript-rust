//! Mapped types `{ readonly [K in C as N]?: X }`.
//!
//! A mapped type is an object keyed by its `MappedType` node and a mapper of the type
//! parameters in scope at the node (never its own `K`). Behaviour (observed from tsc 7.0.2):
//! - Homomorphic mapped types (`{ [K in keyof T]: ... }` with `T` a type parameter)
//!   instantiate over each union member of `T`'s argument: arrays give mapped arrays,
//!   tuples give mapped tuples, primitives pass through; the union keeps the alias.
//! - Members: the keys come from the modifiers type's properties and index signatures
//!   (`keyof X` constraints) or from the constraint's union members. Literal keys become
//!   properties; `string`, `number`, `symbol`, `any` and pattern keys become index
//!   signatures. Modifiers are taken from the modifiers type's property (`Pick` too) unless
//!   overridden by `+`/`-` modifiers.
//! - A mapped type whose constraint is generic is opaque here: no members, an `any`-like
//!   apparent type and permissive relations (no false positives while it is generic).

use rustc_hash::FxHashMap;
use ts_r_ast::{MappedModifier, Node, NodeId, TypeOperatorKind};
use ts_r_binder::SymbolFlags;
use ts_r_types::{
    AliasId, IndexInfo, MapperId, ObjectKind, ResolvedMembers, StrId, SymId, TupleElement,
    TypeFlags, TypeId, TypeKind, TypeTraits,
};

use crate::checker::{Ck, Loc};
use crate::declared::TupleEl;
use crate::symbols::{SynthSym, check_flags};

/// The parts of a mapped type.
#[derive(Clone, Copy, Debug)]
pub struct MappedInfo {
    pub decl: Loc,
    pub mapper: MapperId,
    /// The iteration type parameter `K`.
    pub k: TypeId,
    pub readonly: MappedModifier,
    pub optional: MappedModifier,
    pub constraint_node: Option<NodeId>,
    pub name_node: Option<NodeId>,
    pub template_node: Option<NodeId>,
}

fn includes(m: MappedModifier) -> bool {
    matches!(m, MappedModifier::Present | MappedModifier::Add)
}

fn skip_parens(ast: &ts_r_ast::Ast, mut n: NodeId) -> NodeId {
    let mut guard = 0;
    while let Node::ParenType { ty } = *ast.node(n) {
        n = ty;
        guard += 1;
        if guard > 64 {
            break;
        }
    }
    n
}

impl<'c, 'a> Ck<'c, 'a> {
    /// The type of a `MappedType` node.
    pub fn mapped_type_from_node(&mut self, l: Loc, alias: AliasId) -> TypeId {
        let outer = self.outer_type_params(l);
        self.store()
            .mapped_object(l.pack(), MapperId::IDENTITY, alias, !outer.is_empty())
    }

    /// The iteration type parameter `K` of a `MappedType` node.
    pub fn mapped_type_param_of(&mut self, l: Loc) -> Option<TypeId> {
        let Node::MappedType { type_param, .. } = *self.node(l) else {
            return None;
        };
        let sym = self.c.sym_of_decl(Loc::new(l.file, type_param))?;
        Some(self.store().type_param(sym, false))
    }

    /// The parts of a mapped type, `None` for other types.
    pub fn mapped_info(&mut self, t: TypeId) -> Option<MappedInfo> {
        let ObjectKind::Mapped { decl, mapper } = self.store().object_of(t)?.kind else {
            return None;
        };
        let l = Loc::unpack(decl)?;
        let Node::MappedType {
            readonly,
            type_param,
            name_type,
            optional,
            ty,
        } = *self.node(l)
        else {
            return None;
        };
        let constraint_node = match *self.node(Loc::new(l.file, type_param)) {
            Node::TypeParam { constraint, .. } => constraint,
            _ => None,
        };
        let k = self.mapped_type_param_of(l)?;
        Some(MappedInfo {
            decl: l,
            mapper,
            k,
            readonly,
            optional,
            constraint_node,
            name_node: name_type,
            template_node: ty,
        })
    }

    /// The mapper of the outer type parameters plus `K → key`.
    fn mapped_template_mapper(&mut self, info: &MappedInfo, key: TypeId) -> MapperId {
        let s = self.store();
        let (src, dst) = s.mapper_lists(info.mapper);
        let mut src = src.to_vec();
        let mut dst = dst.to_vec();
        src.push(info.k);
        dst.push(key);
        s.mapper(&src, &dst)
    }

    /// For a mapped type over a generic key set (`{ [K in keyof T]: X<K> }`), the template
    /// for one key (`X<"d">`): what an object literal property named `"d"` is contextually
    /// typed by.
    pub fn generic_mapped_template_for(&mut self, t: TypeId, key: TypeId) -> Option<TypeId> {
        let info = self.mapped_info(t)?;
        if info.name_node.is_some() {
            return None;
        }
        let c = self.mapped_constraint(&info);
        if !self.could_contain_type_params(c) {
            return None;
        }
        let m = self.mapped_template_mapper(&info, key);
        let raw = self.mapped_template_raw(&info);
        Some(self.instantiate(raw, m))
    }

    /// The (instantiated) constraint type `C` of `[K in C]`.
    pub fn mapped_constraint(&mut self, info: &MappedInfo) -> TypeId {
        let Some(c) = info.constraint_node else {
            return TypeId::UNRESOLVED;
        };
        let raw = self.type_from_node(Loc::new(info.decl.file, c));
        self.instantiate(raw, info.mapper)
    }

    fn mapped_name_raw(&mut self, info: &MappedInfo) -> Option<TypeId> {
        let n = info.name_node?;
        Some(self.type_from_node(Loc::new(info.decl.file, n)))
    }

    fn mapped_template_raw(&mut self, info: &MappedInfo) -> TypeId {
        match info.template_node {
            Some(n) => self.type_from_node(Loc::new(info.decl.file, n)),
            None => TypeId::ANY,
        }
    }

    /// The operand `X` of a `keyof X` constraint node.
    fn keyof_constraint_operand(&self, info: &MappedInfo) -> Option<Loc> {
        let c = info.constraint_node?;
        let ast = self.ast(info.decl.file);
        let c = skip_parens(ast, c);
        match *ast.node(c) {
            Node::TypeOperator {
                op: TypeOperatorKind::KeyOf,
                ty,
            } => Some(Loc::new(info.decl.file, ty)),
            _ => None,
        }
    }

    /// The type whose property modifiers a mapped type copies (`unknown` when none).
    pub fn mapped_modifiers_type(&mut self, info: &MappedInfo) -> TypeId {
        if let Some(op) = self.keyof_constraint_operand(info) {
            let raw = self.type_from_node(op);
            return self.instantiate(raw, info.mapper);
        }
        // `[P in K]` where `K extends keyof T` (`Pick`).
        let Some(c) = info.constraint_node else {
            return TypeId::UNKNOWN;
        };
        let raw = self.type_from_node(Loc::new(info.decl.file, c));
        if self.store().flags(raw).contains(TypeFlags::TYPE_PARAMETER)
            && let Some(cc) = self.type_param_constraint(raw)
            && let ts_r_types::TypeKind::Index(x) = self.store().kind(cc)
        {
            return self.instantiate(x, info.mapper);
        }
        TypeId::UNKNOWN
    }

    /// The type parameter `T` of a `{ [K in keyof T]: ... }` declaration.
    fn homomorphic_type_variable(&mut self, info: &MappedInfo) -> Option<TypeId> {
        let op = self.keyof_constraint_operand(info)?;
        let t = self.type_from_node(op);
        self.store()
            .flags(t)
            .contains(TypeFlags::TYPE_PARAMETER)
            .then_some(t)
    }

    /// Whether a mapped type is declared `{ [K in keyof T]: ... }` with `T` a type parameter.
    pub fn is_homomorphic_mapped(&mut self, t: TypeId) -> bool {
        match self.mapped_info(t) {
            Some(info) => self.homomorphic_type_variable(&info).is_some(),
            None => false,
        }
    }

    /// tsc's `isGenericMappedType`.
    /// A generic homomorphic mapped type without an `as` clause (`{ [P in keyof T]: X }`):
    /// array literals in its context are tuples.
    pub(crate) fn is_homomorphic_generic_mapped(&mut self, t: TypeId) -> bool {
        if !self.is_generic_mapped_type(t) {
            return false;
        }
        let Some(info) = self.mapped_info(t) else {
            return false;
        };
        if self.mapped_name_raw(&info).is_some() {
            return false;
        }
        let c = self.mapped_constraint(&info);
        let s = self.store();
        matches!(s.kind(c), TypeKind::Index(x) if s.flags(x).contains(TypeFlags::TYPE_PARAMETER))
    }

    pub fn is_generic_mapped_type(&mut self, t: TypeId) -> bool {
        let Some(info) = self.mapped_info(t) else {
            return false;
        };
        let c = self.mapped_constraint(&info);
        if self.is_generic_index_type(c) {
            return true;
        }
        if let Some(n) = self.mapped_name_raw(&info) {
            let m = self.mapped_template_mapper(&info, c);
            let nt = self.instantiate(n, m);
            if self.is_generic_index_type(nt) {
                return true;
            }
        }
        false
    }

    /// The apparent type of a generic homomorphic mapped type: the mapped type applied to
    /// the constraint of its type variable (`Readonly<X & P>` reads as `Readonly<X>`).
    /// `None` when the constraint is not concrete.
    pub fn mapped_apparent_type(&mut self, t: TypeId) -> Option<TypeId> {
        let info = self.mapped_info(t)?;
        let tv = self.homomorphic_type_variable(&info)?;
        let outer = self.outer_type_params(info.decl);
        let i = outer.iter().position(|&p| p == tv)?;
        let args = self.mapped_args(&outer, info.mapper, MapperId::IDENTITY);
        let b = self.base_constraint(args[i]);
        if b == args[i]
            || b.is_any_like()
            || self.store().is_error(b)
            || b == TypeId::UNKNOWN
            || self.is_generic_object_type(b)
        {
            return None;
        }
        let declared_alias = self.declared_alias_of_node(info.decl);
        Some(self.mapped_constituent(&info, &outer, &args, i, b, declared_alias))
    }

    /// `{ [K in C]: X }[C]` for a generic mapped type without a name clause: the template
    /// `X` with `K` replaced by `C` (the constraint a relation uses for the indexed access).
    pub fn indexed_mapped_substitution(&mut self, t: TypeId) -> Option<TypeId> {
        self.indexed_mapped_substitution_ext(t, false)
    }

    /// [`Self::indexed_mapped_substitution`]; with `any_index`, for any key (`{ [W in D]: X }
    /// [LW]` is `X` with `W` replaced by `LW`), as a base constraint reads it.
    pub fn indexed_mapped_substitution_ext(
        &mut self,
        t: TypeId,
        any_index: bool,
    ) -> Option<TypeId> {
        let TypeKind::IndexedAccess { obj, index } = self.store().kind(t) else {
            return None;
        };
        let info = self.mapped_info(obj)?;
        if info.name_node.is_some() || !self.is_generic_mapped_type(obj) {
            return None;
        }
        if !any_index && self.mapped_constraint(&info) != index {
            return None;
        }
        Some(self.mapped_template_for(&info, index, false))
    }

    /// The alias of the type alias whose body is the mapped type node `l`, with its own type
    /// parameters as arguments.
    fn declared_alias_of_node(&mut self, l: Loc) -> AliasId {
        let mut cur = l;
        for _ in 0..64 {
            let Some(p) = self.c.parent(cur) else {
                return AliasId::NONE;
            };
            match *self.node(p) {
                Node::ParenType { .. } => cur = p,
                Node::TypeAlias { .. } => {
                    let Some(sym) = self.c.sym_of_decl(p) else {
                        return AliasId::NONE;
                    };
                    let params = self.local_type_params(sym);
                    let s = self.store();
                    return s.alias(sym, s.list(&params));
                }
                _ => return AliasId::NONE,
            }
        }
        AliasId::NONE
    }

    /// Instantiation of a mapped type `t` (declared at `decl`, current mapper `mapper`) by `m`.
    pub fn instantiate_mapped(
        &mut self,
        t: TypeId,
        decl: (u32, u32),
        mapper: MapperId,
        m: MapperId,
    ) -> TypeId {
        let Some(l) = Loc::unpack(decl) else {
            return t;
        };
        let Some(info) = self.mapped_info(t) else {
            return t;
        };
        let s = self.store();
        let od_alias = s.object_of(t).map_or(AliasId::NONE, |o| o.alias);
        let outer = self.outer_type_params(l);
        let args = self.mapped_args(&outer, mapper, m);
        let alias = self.instantiate_alias(od_alias, m);
        if let Some(tv) = self.homomorphic_type_variable(&info)
            && let Some(i) = outer.iter().position(|&p| p == tv)
            && args[i] != tv
        {
            let mtv = args[i];
            // Over an error type (an unresolved import) the mapping proceeds as over `any`
            // (`Omit<CP, "type">` is an object type in tsc); only our silent any stays silent.
            if mtv == TypeId::UNRESOLVED {
                return TypeId::UNRESOLVED;
            }
            let declared_alias = self.declared_alias_of_node(l);
            if s.flags(mtv).contains(TypeFlags::UNION) {
                let members = s.members_of_union(mtv).to_vec();
                let mut parts = Vec::with_capacity(members.len());
                for c in members {
                    parts.push(self.mapped_constituent(&info, &outer, &args, i, c, declared_alias));
                }
                return self.union_with(&parts, crate::unions::Reduction::Literal, alias);
            }
            // A single constituent gets the declared alias (`Partial<X>`), not a new one.
            return self.mapped_constituent(&info, &outer, &args, i, mtv, declared_alias);
        }
        if m == MapperId::PERMISSIVE {
            let ni = MappedInfo {
                mapper: self.canonical_mapper(&outer, &args),
                ..info
            };
            if self.mapped_constraint(&ni) == TypeId::WILDCARD {
                return TypeId::WILDCARD;
            }
        }
        let nm = self.canonical_mapper(&outer, &args);
        if nm == mapper && alias == od_alias {
            return t;
        }
        // Over an unresolved type (`Pick<Unresolved, K>`) the result stays unresolved.
        let ni = MappedInfo { mapper: nm, ..info };
        if self.mapped_modifiers_type(&ni) == TypeId::UNRESOLVED {
            return TypeId::UNRESOLVED;
        }
        let contains = args
            .iter()
            .any(|&x| s.traits(x).contains(TypeTraits::CONTAINS_TYPE_PARAMETER));
        s.mapped_object(decl, nm, alias, contains)
    }

    /// One constituent of a homomorphic mapped type instantiation (`T` mapped to `c`).
    fn mapped_constituent(
        &mut self,
        info: &MappedInfo,
        outer: &[TypeId],
        args: &[TypeId],
        tv_index: usize,
        c: TypeId,
        declared_alias: AliasId,
    ) -> TypeId {
        let s = self.store();
        let cf = s.flags(c);
        let mut a2 = args.to_vec();
        a2[tv_index] = c;
        let nm = self.canonical_mapper(outer, &a2);
        if !cf.intersects(
            TypeFlags::ANY_OR_UNKNOWN
                | TypeFlags::INSTANTIABLE_NON_PRIMITIVE
                | TypeFlags::OBJECT
                | TypeFlags::INTERSECTION,
        ) || c == TypeId::WILDCARD
            || (c.is_any_like() || self.store().is_error(c)) && c != TypeId::ANY
        {
            return c;
        }
        let ni = MappedInfo {
            mapper: nm,
            ..*info
        };
        if info.name_node.is_none() {
            let tv = outer[tv_index];
            let arrayish_any = cf.contains(TypeFlags::ANY)
                && self
                    .type_param_constraint(tv)
                    .is_some_and(|k| self.every_member(k, |m| self.c.is_array_like_type(m)));
            if (self.c.is_array_type(c) && !self.c.is_tuple_type(c)) || arrayish_any {
                return self.mapped_array(&ni, c);
            }
            if self.c.is_tuple_type(c) {
                return self.mapped_tuple(info, outer, args, tv_index, c);
            }
            if cf.contains(TypeFlags::INTERSECTION)
                && self.every_member(c, |m| self.c.is_array_like_type(m))
            {
                let ms = s.members_of_union(c).to_vec();
                let parts: Vec<TypeId> = ms
                    .into_iter()
                    .map(|m| {
                        self.mapped_constituent(info, outer, args, tv_index, m, declared_alias)
                    })
                    .collect();
                return self.intersection(&parts);
            }
        }
        let alias = self.instantiate_alias(declared_alias, nm);
        let contains = a2
            .iter()
            .any(|&x| s.traits(x).contains(TypeTraits::CONTAINS_TYPE_PARAMETER));
        s.mapped_object(info.decl.pack(), nm, alias, contains)
    }

    /// The template instantiated for `key`, with the mapped type's optionality applied.
    fn mapped_template_for(&mut self, info: &MappedInfo, key: TypeId, is_optional: bool) -> TypeId {
        let tm = self.mapped_template_mapper(info, key);
        let raw = self.mapped_template_raw(info);
        let pt = self.instantiate(raw, tm);
        let strict = self.c.opts.strict_null_checks;
        if strict
            && includes(info.optional)
            && !self.some_member(pt, |m| {
                self.store()
                    .flags(m)
                    .intersects(TypeFlags::UNDEFINED | TypeFlags::VOID)
            })
        {
            return self.add_optionality(pt);
        }
        if strict && info.optional == MappedModifier::Remove && is_optional {
            return self.remove_undefined(pt);
        }
        pt
    }

    fn remove_undefined(&mut self, t: TypeId) -> TypeId {
        self.filter_type(t, |ck, m| {
            !ck.store().flags(m).contains(TypeFlags::UNDEFINED)
        })
    }

    fn modified_readonly(&self, info: &MappedInfo, current: bool) -> bool {
        match info.readonly {
            MappedModifier::Present | MappedModifier::Add => true,
            MappedModifier::Remove => false,
            MappedModifier::None => current,
        }
    }

    fn mapped_array(&mut self, info: &MappedInfo, arr: TypeId) -> TypeId {
        let elem = self.mapped_template_for(info, TypeId::NUMBER, true);
        let ro = self.is_readonly_array_like(arr);
        let ro = self.modified_readonly(info, ro);
        self.create_array(elem, ro)
    }

    fn mapped_tuple(
        &mut self,
        info: &MappedInfo,
        outer: &[TypeId],
        args: &[TypeId],
        tv_index: usize,
        tup: TypeId,
    ) -> TypeId {
        let s = self.store();
        let Some(shape) = s.tuple_shape_of(tup).cloned() else {
            return tup;
        };
        let targs = self.c.type_args_of_tuple(tup).to_vec();
        let fixed = shape
            .elements
            .iter()
            .take_while(|e| e.kind == TupleElement::REQUIRED || e.kind == TupleElement::OPTIONAL)
            .count();
        let mut a_fixed = args.to_vec();
        a_fixed[tv_index] = tup;
        let fixed_info = MappedInfo {
            mapper: self.canonical_mapper(outer, &a_fixed),
            ..*info
        };
        let mut els: Vec<TupleEl> = Vec::with_capacity(shape.elements.len());
        for (i, el) in shape.elements.iter().enumerate() {
            let ety = targs.get(i).copied().unwrap_or(TypeId::UNRESOLVED);
            let ty = if i < fixed {
                let key = s.string_literal(&i.to_string(), false);
                self.mapped_template_for(&fixed_info, key, el.kind == TupleElement::OPTIONAL)
            } else if el.kind == TupleElement::VARIADIC {
                self.mapped_constituent(info, outer, args, tv_index, ety, AliasId::NONE)
            } else {
                // A rest element maps as an array (`create_tuple` takes rest elements as
                // array types).
                let arr = self.create_array(ety, false);
                let mapped =
                    self.mapped_constituent(info, outer, args, tv_index, arr, AliasId::NONE);
                if el.kind == TupleElement::REST {
                    mapped
                } else {
                    self.element_type_of_array_like(mapped)
                        .unwrap_or(TypeId::UNKNOWN)
                }
            };
            let kind = match (el.kind, info.optional) {
                (TupleElement::REQUIRED, MappedModifier::Present | MappedModifier::Add) => {
                    TupleElement::OPTIONAL
                }
                (TupleElement::OPTIONAL, MappedModifier::Remove) => TupleElement::REQUIRED,
                (k, _) => k,
            };
            els.push(TupleEl {
                ty,
                kind,
                label: el.label,
            });
        }
        let ro = self.modified_readonly(info, shape.readonly);
        if els.iter().any(|e| self.store().is_error(e.ty)) {
            return TypeId::ERROR;
        }
        self.create_tuple(&els, ro)
    }

    // ---------------------------------------------------------------------------------
    // Members
    // ---------------------------------------------------------------------------------

    /// Members of a mapped type (empty while its constraint is generic).
    pub fn mapped_members(&mut self, t: TypeId) -> ResolvedMembers {
        let Some(info) = self.mapped_info(t) else {
            return ResolvedMembers::empty();
        };
        let constraint = self.mapped_constraint(&info);
        if constraint == TypeId::UNRESOLVED || self.is_generic_index_type(constraint) {
            return ResolvedMembers::empty();
        }
        let name_raw = self.mapped_name_raw(&info);
        let modifiers = self.mapped_modifiers_type(&info);
        let keys: Vec<TypeId> = if self.keyof_constraint_operand(&info).is_some() {
            self.mapped_keys_of_modifiers(modifiers)
        } else {
            self.union_members(constraint)
        };
        // Properties in key order with the keys that map to each (their union is built once
        // at the end: building it key by key is quadratic).
        let mut props: Vec<(StrId, Vec<TypeId>)> = Vec::new();
        let mut prop_index: FxHashMap<StrId, usize> = FxHashMap::default();
        let mut index: Vec<IndexInfo> = Vec::new();
        let mut index_values: Vec<Vec<TypeId>> = Vec::new();
        for key in keys {
            let pnt = match name_raw {
                Some(n) => {
                    let m = self.mapped_template_mapper(&info, key);
                    self.instantiate(n, m)
                }
                None => key,
            };
            for pn in self.union_members(pnt) {
                // `string & {}` keys an index signature as `string` does.
                let pn = self.primitive_with_empty_object(pn).unwrap_or(pn);
                let pf = self.store().flags(pn);
                if let Some(name) = self.property_name_of_type(pn) {
                    let nid = self.store().intern_str(&name);
                    match prop_index.get(&nid) {
                        Some(&i) => props[i].1.push(key),
                        None => {
                            prop_index.insert(nid, props.len());
                            props.push((nid, vec![key]));
                        }
                    }
                } else if pf.intersects(
                    TypeFlags::ANY
                        | TypeFlags::STRING
                        | TypeFlags::NUMBER
                        | TypeFlags::ENUM
                        | TypeFlags::ES_SYMBOL,
                ) || self.is_pattern_literal_type(pn)
                {
                    let ikey = if pf.intersects(TypeFlags::ANY | TypeFlags::STRING) {
                        TypeId::STRING
                    } else if pf.intersects(TypeFlags::NUMBER | TypeFlags::ENUM) {
                        TypeId::NUMBER
                    } else if pf.intersects(TypeFlags::ES_SYMBOL) {
                        TypeId::SYMBOL
                    } else {
                        pn
                    };
                    let value = self.mapped_template_for(&info, key, false);
                    let mro = if modifiers == TypeId::UNKNOWN {
                        false
                    } else {
                        self.index_info_of(modifiers, ikey)
                            .is_some_and(|i| i.readonly)
                    };
                    let ro = self.modified_readonly(&info, mro);
                    match index.iter_mut().position(|i| i.key == ikey) {
                        Some(p) => {
                            index_values[p].push(value);
                            index[p].readonly &= ro;
                        }
                        None => {
                            index.push(IndexInfo {
                                key: ikey,
                                value,
                                readonly: ro,
                                decl: None,
                            });
                            index_values.push(vec![value]);
                        }
                    }
                }
            }
        }
        for (info, values) in index.iter_mut().zip(index_values) {
            if values.len() > 1 {
                info.value = self.union(&values);
            }
        }
        let strict = self.c.opts.strict_null_checks;
        let mut out: Vec<(StrId, SymId)> = Vec::with_capacity(props.len());
        for (nid, keys) in props {
            let key = match keys.as_slice() {
                [k] => *k,
                _ => self.union(&keys),
            };
            // Inherited modifiers come from the first key's property (as in tsc: later keys
            // mapped to the same name do not change them).
            let mprop = match self.property_name_of_type(keys[0]) {
                Some(name) if modifiers != TypeId::UNKNOWN => {
                    self.property_of_type(modifiers, &name)
                }
                _ => None,
            };
            let mp_optional =
                mprop.is_some_and(|p| self.c.sym_flags(p).intersects(SymbolFlags::OPTIONAL));
            let is_optional =
                includes(info.optional) || (info.optional != MappedModifier::Remove && mp_optional);
            let mp_readonly = match mprop {
                Some(p) => self.is_readonly_symbol(p),
                None => false,
            };
            let is_readonly = self.modified_readonly(&info, mp_readonly);
            let mut flags = SymbolFlags::PROPERTY;
            if is_optional {
                flags |= SymbolFlags::OPTIONAL;
            }
            let mut check = check_flags::MAPPED;
            if is_readonly {
                check |= check_flags::READONLY;
            }
            if strict && !is_optional && mp_optional {
                check |= check_flags::STRIP_OPTIONAL;
            }
            let sym = self.c.new_synth(
                SynthSym::new(nid, flags, check)
                    .with_origins(mprop.into_iter().collect())
                    .with_mapped(t, key)
                    .with_parent(Some(t)),
            );
            out.push((nid, sym));
        }
        ResolvedMembers::new(out, Vec::new(), Vec::new(), index)
    }

    /// The key types a `keyof X` mapped type iterates: `X`'s property names and index keys.
    fn mapped_keys_of_modifiers(&mut self, modifiers: TypeId) -> Vec<TypeId> {
        let at = self.apparent_type(modifiers);
        if self.store().flags(at).contains(TypeFlags::ANY) {
            return vec![TypeId::STRING];
        }
        let mut keys = Vec::new();
        for (n, _) in self.properties_of(at) {
            let text = self.store().str(n).to_owned();
            if text.starts_with('#') {
                continue;
            }
            if text.starts_with("__@") {
                if let Some(id) = text.rsplit('@').next().and_then(|x| x.parse::<u32>().ok()) {
                    keys.push(self.store().unique_symbol(SymId(id)));
                }
                continue;
            }
            keys.push(self.store().string_literal(&text, false));
        }
        let s = self.store();
        let f = s.flags(at);
        let infos: Vec<IndexInfo> = if f.contains(TypeFlags::OBJECT) {
            self.resolved_members(at).index.to_vec()
        } else if f.intersects(TypeFlags::UNION_OR_INTERSECTION) {
            let mut v = Vec::new();
            for k in [TypeId::STRING, TypeId::NUMBER, TypeId::SYMBOL] {
                if let Some(i) = self.index_info_of(at, k)
                    && (k != TypeId::NUMBER || self.index_info_of(at, TypeId::STRING).is_none())
                {
                    v.push(IndexInfo { key: k, ..i });
                }
            }
            v
        } else {
            Vec::new()
        };
        for i in infos {
            keys.push(i.key);
        }
        keys
    }

    /// The type of a property of a mapped type (computed lazily).
    pub fn mapped_property_type(&mut self, p: SymId, mapped: TypeId, key: TypeId) -> TypeId {
        let Some(info) = self.mapped_info(mapped) else {
            return TypeId::UNRESOLVED;
        };
        if !self.push_resolution(crate::checker::Resolving::Type(p)) {
            return TypeId::UNRESOLVED;
        }
        let tm = self.mapped_template_mapper(&info, key);
        let raw = self.mapped_template_raw(&info);
        let pt = self.instantiate(raw, tm);
        if !self.pop_resolution() {
            return TypeId::UNRESOLVED;
        }
        let strict = self.c.opts.strict_null_checks;
        let optional = self.c.sym_flags(p).intersects(SymbolFlags::OPTIONAL);
        if strict
            && optional
            && !self.some_member(pt, |m| {
                self.store()
                    .flags(m)
                    .intersects(TypeFlags::UNDEFINED | TypeFlags::VOID)
            })
        {
            return self.add_optionality(pt);
        }
        if self.c.sym_check_flags(p) & check_flags::STRIP_OPTIONAL != 0 {
            return self.remove_undefined(pt);
        }
        pt
    }

    /// Inference to a generic mapped type target:
    /// - `{ [P in K]: X }` with `K` inferred: `keyof source` to `K`, and the union of the
    ///   source's property and index types to `X`;
    /// - `{ [P in keyof T]: T[P] }` with `T` inferred: the source to `T`;
    /// - other homomorphic forms whose template reads `T[K]` reverse-map the source
    ///   (`reverse_mapped`); what can't be reverse-mapped infers a silent `any` so that no
    ///   wrong inference produces errors.
    pub(crate) fn infer_to_mapped(
        &mut self,
        ctx: &mut crate::infer::InferenceContext,
        source: TypeId,
        target: TypeId,
        contra: bool,
        depth: u32,
    ) {
        let Some(info) = self.mapped_info(target) else {
            return;
        };
        let constraint = self.mapped_constraint(&info);
        let s = self.store();
        if let ts_r_types::TypeKind::Index(tv) = s.kind(constraint)
            && ctx.index_of(tv).is_some()
        {
            // A primitive has no properties to reverse-map (`2` to `{ [K in keyof T]?: T[K] }`
            // infers nothing).
            if s.flags(source).intersects(TypeFlags::PRIMITIVE)
                && !s
                    .flags(source)
                    .intersects(TypeFlags::ANY | TypeFlags::UNKNOWN)
            {
                // Inside a reverse mapping (`T[K]` through a recursive mapped type), a
                // primitive maps through its apparent type (not modelled): a silent `any`.
                if matches!(s.kind(tv), ts_r_types::TypeKind::IndexedAccess { .. }) {
                    let saved = ctx.weak_mode;
                    ctx.weak_mode = true;
                    self.infer_from(ctx, TypeId::UNRESOLVED, tv, contra, depth + 1);
                    ctx.weak_mode = saved;
                }
                return;
            }
            let raw = self.mapped_template_raw(&info);
            let tmpl = self.instantiate(raw, info.mapper);
            let simple = matches!(
                s.kind(tmpl),
                ts_r_types::TypeKind::IndexedAccess { obj, index } if obj == tv && index == info.k
            );
            // Through another template, an array from the call's contextual type still
            // reverse-maps to an array (`Post[]` against `Promise.all`'s `{ -readonly [P in
            // keyof T]: Awaited<T[P]> }` makes `T` an array, not a tuple context); its
            // elements are a silent `any`.
            let src = if simple {
                self.constrained_reverse_source(source, tv, None, true)
                    .unwrap_or(source)
            } else if ctx.from_return_context && self.c.is_array_type(source) {
                self.create_array(TypeId::ANY, false)
            } else if !self.mentions_type_variable(tmpl, tv)
                && let Some(r) = self.reverse_mapped_unknown(source)
            {
                // A template that does not read `T` (`{ [P in keyof T]?: Opts }`) leaves the
                // source's keys with `unknown` values.
                r
            } else if info.name_node.is_none()
                && let Some(r) = self.reverse_mapped(source, tmpl, tv, &info, depth)
            {
                // The template's other inferred type parameters infer from the source
                // against the mapped type with `T` reverse-mapped (`{ [K in keyof B]: { fn:
                // (a: A) => void; thing: B[K] } }` infers `A` from each `fn`).
                let others: Vec<TypeId> = ctx
                    .params
                    .iter()
                    .copied()
                    .filter(|&p| p != tv && self.type_mentions(tmpl, p))
                    .collect();
                // (`any` infers nothing there: `(state: any) => R` leaves `S` unknown.)
                if !others.is_empty() {
                    let m = self.store().mapper(&[tv], &[r]);
                    let inst = self.instantiate(target, m);
                    ctx.non_inferrable.push(TypeId::ANY);
                    self.infer_from(ctx, source, inst, contra, depth + 1);
                    ctx.non_inferrable.pop();
                }
                r
            } else {
                TypeId::UNRESOLVED
            };
            // A lower priority than direct inferences (`foo(o, {b: 9})` with
            // `(object: T, partial: Partial<T>)` infers `T` from `o`).
            let saved = ctx.weak_mode;
            ctx.weak_mode = true;
            self.infer_from(ctx, src, tv, contra, depth + 1);
            ctx.weak_mode = saved;
            return;
        }
        // `[P in keyof (X & T)]` (`keyof X | keyof T`, `Readonly<X & T>`): `T` reverse-maps
        // the source when the template reads the same intersection (`(X & T)[P]`).
        if let Some(tv) = self.keyof_intersection_param(ctx, constraint) {
            let s = self.store();
            if s.flags(source).intersects(TypeFlags::PRIMITIVE)
                && !s
                    .flags(source)
                    .intersects(TypeFlags::ANY | TypeFlags::UNKNOWN)
            {
                return;
            }
            let raw = self.mapped_template_raw(&info);
            let tmpl = self.instantiate(raw, info.mapper);
            let simple = match s.kind(tmpl) {
                ts_r_types::TypeKind::IndexedAccess { obj, index } if index == info.k => {
                    obj == tv
                        || (s.flags(obj).contains(TypeFlags::INTERSECTION)
                            && s.members_of_union(obj).contains(&tv))
                }
                _ => false,
            };
            let src = if simple { source } else { TypeId::UNRESOLVED };
            let saved = ctx.weak_mode;
            ctx.weak_mode = true;
            self.infer_from(ctx, src, tv, contra, depth + 1);
            // Other inferred parameters inside `X` get a silent `any`, as below.
            for i in 0..ctx.params.len() {
                let p = ctx.params[i];
                if p != tv && self.type_mentions(constraint, p) {
                    self.infer_from(ctx, TypeId::UNRESOLVED, p, contra, depth + 1);
                }
            }
            ctx.weak_mode = saved;
            return;
        }
        // `[K in keyof T & keyof X]` (reverse mapping through a combined constraint, not
        // modelled): a silent `any` for each inferred `T`.
        // (`keyof T & L` reverse-maps the source's properties whose keys are in `L`.)
        if let Some((tv, limit)) = self.keyof_param_limit(ctx, constraint)
            && let Some(filtered) = self.constrained_reverse_source(source, tv, Some(limit), false)
        {
            let raw = self.mapped_template_raw(&info);
            let tmpl = self.instantiate(raw, info.mapper);
            let simple = matches!(
                self.store().kind(tmpl),
                TypeKind::IndexedAccess { obj, index } if obj == tv && index == info.k
            );
            let src = if simple {
                filtered
            } else if info.name_node.is_none()
                && let Some(r) = self.reverse_mapped(filtered, tmpl, tv, &info, depth)
            {
                r
            } else {
                TypeId::UNRESOLVED
            };
            let saved = ctx.weak_mode;
            ctx.weak_mode = true;
            self.infer_from(ctx, src, tv, contra, depth + 1);
            ctx.weak_mode = saved;
            return;
        }
        if s.flags(constraint)
            .intersects(TypeFlags::UNION_OR_INTERSECTION)
        {
            for i in 0..ctx.params.len() {
                let tv = ctx.params[i];
                if self.type_mentions(constraint, tv) {
                    let saved = ctx.weak_mode;
                    ctx.weak_mode = true;
                    self.infer_from(ctx, TypeId::UNRESOLVED, tv, contra, depth + 1);
                    ctx.weak_mode = saved;
                }
            }
            return;
        }
        if s.flags(constraint).contains(TypeFlags::TYPE_PARAMETER) {
            let Some(ki) = ctx.index_of(constraint) else {
                return;
            };
            // Key candidates from several sources combine into their union.
            ctx.combine[ki] = true;
            let k = self.keyof_type(source);
            self.infer_from(ctx, k, constraint, contra, depth + 1);
            let mut types: Vec<TypeId> = Vec::new();
            for (_, p) in self.properties_of(source) {
                types.push(self.type_of_symbol(p));
            }
            let at = self.apparent_type(source);
            if self.store().flags(at).contains(TypeFlags::OBJECT) {
                for i in self.resolved_members(at).index.iter().copied() {
                    types.push(i.value);
                }
            }
            let u = self.union(&types);
            let raw = self.mapped_template_raw(&info);
            let tmpl = self.instantiate(raw, info.mapper);
            self.infer_from(ctx, u, tmpl, contra, depth + 1);
        }
    }

    /// `(T, L)` for a mapped type constraint `keyof T & L` (or a union of such
    /// intersections, `keyof T & ('a' | 'b')`) with `T` inferred and `L` free of the
    /// inferred type parameters.
    fn keyof_param_limit(
        &mut self,
        ctx: &crate::infer::InferenceContext,
        constraint: TypeId,
    ) -> Option<(TypeId, TypeId)> {
        let s = self.store();
        let parts: Vec<TypeId> = if s.flags(constraint).contains(TypeFlags::UNION) {
            s.members_of_union(constraint).to_vec()
        } else {
            vec![constraint]
        };
        let mut tv = None;
        let mut limits = Vec::with_capacity(parts.len());
        for part in parts {
            if !s.flags(part).contains(TypeFlags::INTERSECTION) {
                return None;
            }
            let mut rest = Vec::new();
            for &m in s.members_of_union(part) {
                match s.kind(m) {
                    TypeKind::Index(t) if ctx.index_of(t).is_some() => {
                        if tv.is_some_and(|x| x != t) {
                            return None;
                        }
                        tv = Some(t);
                    }
                    _ => rest.push(m),
                }
            }
            if rest.is_empty() {
                return None;
            }
            limits.push(self.intersection(&rest));
        }
        let tv = tv?;
        let limit = self.union(&limits);
        if ctx.params.iter().any(|&p| self.type_mentions(limit, p)) {
            return None;
        }
        Some((tv, limit))
    }

    /// An object source as `T` sees it through `{ [K in keyof T & L]: T[K] }` (or `keyof T`
    /// with `limit` `None`): only the properties whose keys are in `L`, with the literal
    /// types of an object literal kept where `T`'s constraint has a primitive property of
    /// that name and widened elsewhere (observed: `<T extends { x: number }>` infers
    /// `{ x: 1; y: string; }` from `{ x: 1, y: 'foo' }`). With `only_if_kept`, `None`
    /// unless some literal is kept.
    fn constrained_reverse_source(
        &mut self,
        source: TypeId,
        tv: TypeId,
        limit: Option<TypeId>,
        only_if_kept: bool,
    ) -> Option<TypeId> {
        let s = self.store();
        if !s.flags(source).contains(TypeFlags::OBJECT)
            || self.c.is_array_like_type(source)
            || self.is_generic_mapped_type(source)
        {
            return None;
        }
        let literal = s
            .object_of(source)
            .is_some_and(|o| o.flags.contains(ts_r_types::ObjectFlags::OBJECT_LITERAL));
        if only_if_kept && !literal {
            return None;
        }
        let constraint = self.type_param_constraint(tv);
        let mut kept_any = false;
        let mut props = Vec::new();
        for (n, p) in self.properties_of(source) {
            if let Some(l) = limit {
                let k = if crate::expr::is_numeric_name(self.store().str(n)) {
                    self.numeric_key_type(p)
                } else {
                    None
                }
                .unwrap_or_else(|| self.property_key_type(self.store().str(n)));
                if !self.is_assignable(k, l) {
                    continue;
                }
            }
            let mut pt = self.type_of_symbol(p);
            if literal {
                let ct = match constraint {
                    Some(c) => {
                        let name = self.store().str(n).to_owned();
                        self.contextual_property_type(c, &name)
                    }
                    None => None,
                };
                pt = self.keep_constrained_literals(pt, ct, &mut kept_any, 0);
            }
            let flags = self.c.sym_flags(p) & (SymbolFlags::PROPERTY | SymbolFlags::OPTIONAL);
            let cf = if self.is_readonly_symbol(p) {
                check_flags::READONLY
            } else {
                0
            };
            let np = self.c.new_synth(SynthSym::new(n, flags, cf).with_type(pt));
            props.push((n, np));
        }
        if only_if_kept && !kept_any {
            return None;
        }
        let infos = if limit.is_none() {
            self.resolved_members(source).index.to_vec()
        } else {
            Vec::new()
        };
        Some(self.store().literal_object(
            SymId::NONE,
            ts_r_types::ObjectFlags::ANONYMOUS,
            ResolvedMembers::new(props, Vec::new(), Vec::new(), infos),
            TypeTraits::NONE,
        ))
    }

    /// An object literal's property type `t` as inferred for a property whose type in the
    /// type parameter's constraint is `ct`: literals stay where `ct` takes literals of
    /// their kind (`string`, or `A` with `A extends string`), nested object literals
    /// likewise by their properties, and everything else widens. Sets `kept` when a
    /// literal stays.
    fn keep_constrained_literals(
        &mut self,
        t: TypeId,
        ct: Option<TypeId>,
        kept: &mut bool,
        depth: u32,
    ) -> TypeId {
        if self.is_literal_type(t) {
            // At the top the context is `T[K]`, generic, so a literal stays when the
            // constraint's property takes its primitive (`1` under `x: number`); deeper,
            // the constraint's own property types are the context (`'bar'` under `nested:
            // { prop: string }` widens).
            let fits = |ck: &Self, ct: TypeId| {
                let st = ck.store();
                let has = |f: TypeFlags| ck.some_member(ct, |m| st.flags(m).intersects(f));
                let lit = |f: TypeFlags| ck.some_member(t, |m| st.flags(m).intersects(f));
                (has(TypeFlags::STRING) && lit(TypeFlags::STRING_LITERAL))
                    || (has(TypeFlags::NUMBER) && lit(TypeFlags::NUMBER_LITERAL))
                    || (has(TypeFlags::BIGINT) && lit(TypeFlags::BIGINT_LITERAL))
            };
            if let Some(ct) = ct
                && ((depth == 0 && fits(self, ct)) || self.is_literal_of_contextual_type(t, ct))
            {
                *kept = true;
                return self.regular_of_literal(t);
            }
            let w = self.widen_literal(t);
            return self.widened_type(w);
        }
        let s = self.store();
        let object_literal = s
            .object_of(t)
            .is_some_and(|o| o.flags.contains(ts_r_types::ObjectFlags::OBJECT_LITERAL));
        let Some(ct) = ct.filter(|_| object_literal && depth < 8) else {
            let w = self.widen_literal(t);
            return self.widened_type(w);
        };
        let mut props = Vec::new();
        for (n, p) in self.properties_of(t) {
            let name = self.store().str(n).to_owned();
            let pct = self.contextual_property_type(ct, &name);
            let pt = self.type_of_symbol(p);
            let pt = self.keep_constrained_literals(pt, pct, kept, depth + 1);
            let flags = self.c.sym_flags(p) & (SymbolFlags::PROPERTY | SymbolFlags::OPTIONAL);
            let cf = if self.is_readonly_symbol(p) {
                check_flags::READONLY
            } else {
                0
            };
            props.push((
                n,
                self.c.new_synth(SynthSym::new(n, flags, cf).with_type(pt)),
            ));
        }
        let infos = self.resolved_members(t).index.to_vec();
        self.store().literal_object(
            SymId::NONE,
            ts_r_types::ObjectFlags::ANONYMOUS,
            ResolvedMembers::new(props, Vec::new(), Vec::new(), infos),
            TypeTraits::NONE,
        )
    }

    /// What a source reverse-maps to through a homomorphic mapped type `{ [K in keyof T]:
    /// X }` whose template `X` reads `T[K]`: each property (array or tuple element) of the
    /// source infers its `T[K]` from its type against `X` (the union of what it infers,
    /// `unknown` when nothing). Source properties keep their optionality unless the mapped
    /// type adds `?`. `None` for other sources or templates.
    fn reverse_mapped(
        &mut self,
        source: TypeId,
        tmpl: TypeId,
        tv: TypeId,
        info: &MappedInfo,
        depth: u32,
    ) -> Option<TypeId> {
        if depth > 16 {
            return None;
        }
        let target = self.store().indexed_access(tv, info.k);
        if !self.mentions_type_variable(tmpl, tv) {
            return None;
        }
        if self.reverse_mapping.contains(&(source, info.decl)) {
            return None;
        }
        self.reverse_mapping.push((source, info.decl));
        let r = self.reverse_mapped_inner(source, tmpl, target, info, depth);
        self.reverse_mapping.pop();
        r
    }

    fn reverse_mapped_inner(
        &mut self,
        source: TypeId,
        tmpl: TypeId,
        target: TypeId,
        info: &MappedInfo,
        depth: u32,
    ) -> Option<TypeId> {
        if self.c.is_array_type(source) {
            let el = self.c.type_args_of(source).first().copied()?;
            let e = self.reverse_mapped_member(el, tmpl, target, depth);
            let readonly = self.is_readonly_array_like(source);
            return Some(self.create_array(e, readonly));
        }
        if let Some(shape) = self.store().tuple_shape_of(source) {
            let elements = shape.elements.to_vec();
            let readonly = shape.readonly;
            let types = self.c.type_args_of(source);
            let mut els = Vec::with_capacity(elements.len());
            for (i, e) in elements.iter().enumerate() {
                let t = types.get(i).copied().unwrap_or(TypeId::UNKNOWN);
                let ty = self.reverse_mapped_member(t, tmpl, target, depth);
                els.push(TupleEl {
                    ty,
                    kind: e.kind,
                    label: e.label,
                });
            }
            return Some(self.create_tuple(&els, readonly));
        }
        let s = self.store();
        if !s.flags(source).contains(TypeFlags::OBJECT)
            || self.is_generic_mapped_type(source)
            || self.c.is_array_like_type(source)
        {
            return None;
        }
        let adds_optional = includes(info.optional);
        let mut props = Vec::new();
        for (n, p) in self.properties_of(source) {
            let mut pt = self.type_of_symbol(p);
            let optional = self.c.sym_flags(p).intersects(SymbolFlags::OPTIONAL);
            let mut flags = SymbolFlags::PROPERTY;
            if optional && !adds_optional {
                flags |= SymbolFlags::OPTIONAL;
            }
            if optional && adds_optional {
                pt = self.remove_undefined(pt);
            }
            let readonly = !includes(info.readonly) && self.is_readonly_symbol(p);
            let cf = if readonly { check_flags::READONLY } else { 0 };
            let t = self.reverse_mapped_member(pt, tmpl, target, depth);
            let np = self.c.new_synth(SynthSym::new(n, flags, cf).with_type(t));
            props.push((n, np));
        }
        let index = self.resolved_members(source).index.to_vec();
        let mut infos = Vec::with_capacity(index.len());
        for mut i in index {
            i.value = self.reverse_mapped_member(i.value, tmpl, target, depth);
            infos.push(i);
        }
        Some(self.store().literal_object(
            SymId::NONE,
            ts_r_types::ObjectFlags::ANONYMOUS,
            ResolvedMembers::new(props, Vec::new(), Vec::new(), infos),
            TypeTraits::NONE,
        ))
    }

    /// Whether `t` mentions the type variable `tv`: a type parameter, or the type
    /// parameter an indexed access (`T[K]`, from a nested reverse mapping) reads.
    fn mentions_type_variable(&mut self, t: TypeId, mut tv: TypeId) -> bool {
        for _ in 0..32 {
            match self.store().kind(tv) {
                ts_r_types::TypeKind::IndexedAccess { obj, .. } => tv = obj,
                _ => break,
            }
        }
        self.type_mentions(t, tv)
    }

    /// What `T[K]` is when a source member of type `t` matches the template `tmpl`.
    fn reverse_mapped_member(
        &mut self,
        t: TypeId,
        tmpl: TypeId,
        target: TypeId,
        depth: u32,
    ) -> TypeId {
        let mut ctx = crate::infer::InferenceContext::new(&[target]);
        self.infer_from(&mut ctx, t, tmpl, false, depth + 1);

        let mut cands = ctx.candidates[0].clone();
        if cands.is_empty() && ctx.contra[0].is_empty() {
            // (Lower-priority inferences, as through a nested reverse mapping.)
            cands = ctx.weak[0].clone();
        }
        if !cands.is_empty() {
            return self.union(&cands);
        }
        let contra = ctx.contra[0].clone();
        if !contra.is_empty() {
            return self.intersection(&contra);
        }
        TypeId::UNKNOWN
    }

    /// What a source reverse-maps to through a homomorphic mapped type whose template does
    /// not read the type parameter: its keys with `unknown` values (`unknown[]` for an
    /// array). `None` for other sources.
    fn reverse_mapped_unknown(&mut self, source: TypeId) -> Option<TypeId> {
        if self.c.is_array_type(source) {
            return Some(self.create_array(TypeId::UNKNOWN, false));
        }
        if let Some(shape) = self.store().tuple_shape_of(source) {
            let els: Vec<TupleEl> = shape
                .elements
                .iter()
                .map(|e| TupleEl {
                    ty: TypeId::UNKNOWN,
                    kind: e.kind,
                    label: e.label,
                })
                .collect();
            return Some(self.create_tuple(&els, false));
        }
        let s = self.store();
        if !s.flags(source).contains(TypeFlags::OBJECT) || self.is_generic_mapped_type(source) {
            return None;
        }
        let mut props = Vec::new();
        for (n, p) in self.properties_of(source) {
            let optional = self.c.sym_flags(p).intersects(SymbolFlags::OPTIONAL);
            let mut flags = SymbolFlags::PROPERTY;
            if optional {
                flags |= SymbolFlags::OPTIONAL;
            }
            let np = self
                .c
                .new_synth(SynthSym::new(n, flags, 0).with_type(TypeId::UNKNOWN));
            props.push((n, np));
        }
        Some(self.store().literal_object(
            SymId::NONE,
            ts_r_types::ObjectFlags::ANONYMOUS,
            ResolvedMembers::new(props, Vec::new(), Vec::new(), Vec::new()),
            TypeTraits::NONE,
        ))
    }

    /// The one inferred type parameter `T` of a mapped constraint `keyof (X & T)` (or its
    /// normalized form `keyof X | keyof T`).
    fn keyof_intersection_param(
        &mut self,
        ctx: &crate::infer::InferenceContext,
        constraint: TypeId,
    ) -> Option<TypeId> {
        let s = self.store();
        let operands: Vec<TypeId> = match s.kind(constraint) {
            ts_r_types::TypeKind::Index(x) if s.flags(x).contains(TypeFlags::INTERSECTION) => {
                s.members_of_union(x).to_vec()
            }
            _ if s.flags(constraint).contains(TypeFlags::UNION) => {
                let mut out = Vec::new();
                for &m in s.members_of_union(constraint) {
                    match s.kind(m) {
                        ts_r_types::TypeKind::Index(x) => out.push(x),
                        _ if s.flags(m).intersects(TypeFlags::LITERAL) => {}
                        _ => return None,
                    }
                }
                out
            }
            _ => return None,
        };
        let mut found = None;
        for o in operands {
            if ctx.index_of(o).is_some() {
                if found.is_some() {
                    return None;
                }
                found = Some(o);
            }
        }
        found
    }

    /// `keyof M` for a mapped type `M`: its constraint when there is no `as` clause,
    /// otherwise the remapped keys; `None` when it stays deferred.
    pub fn keyof_mapped(&mut self, t: TypeId) -> Option<TypeId> {
        let info = self.mapped_info(t)?;
        let constraint = self.mapped_constraint(&info);
        let Some(n) = self.mapped_name_raw(&info) else {
            return Some(constraint);
        };
        if self.is_generic_index_type(constraint) {
            return None;
        }
        let keys = if self.keyof_constraint_operand(&info).is_some() {
            let modifiers = self.mapped_modifiers_type(&info);
            self.mapped_keys_of_modifiers(modifiers)
        } else {
            self.union_members(constraint)
        };
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            let m = self.mapped_template_mapper(&info, k);
            let nt = self.instantiate(n, m);
            out.push(if nt == TypeId::STRING {
                self.union2(TypeId::STRING, TypeId::NUMBER)
            } else {
                nt
            });
        }
        Some(self.union(&out))
    }
}
