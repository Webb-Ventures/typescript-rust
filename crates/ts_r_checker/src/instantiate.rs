//! Instantiation of types and signatures through mappers.

use ts_r_types::{
    AliasId, IndexInfo, ListId, MapperId, ObjectKind, Param, ResolvedMembers, SigId, Signature,
    SymId, TupleElement, TypeFlags, TypeId, TypeKind, TypeTraits,
};

use crate::checker::{Ck, Loc};
use crate::declared::TupleEl;
use crate::symbols::SynthSym;

const MAX_INSTANTIATION_DEPTH: u32 = 100;
/// Instantiations per checked statement or type reference before giving up (TS2589).
const MAX_INSTANTIATION_COUNT: u32 = 5_000_000;

impl<'c, 'a> Ck<'c, 'a> {
    /// `t` with the mapper's type parameters replaced.
    pub fn instantiate(&mut self, t: TypeId, m: MapperId) -> TypeId {
        if m == MapperId::IDENTITY {
            return t;
        }
        let s = self.store();
        if !s.traits(t).contains(TypeTraits::CONTAINS_TYPE_PARAMETER) {
            return t;
        }
        // A forward reference under type arguments (`type R<T> = Array<T | R<T>>`) maps its
        // arguments; its target would lead back to it.
        if let Some((node, args)) = s.forward_node_args_of(t) {
            return self.instantiate_arg_forward(t, node, args, m);
        }
        if let Some(&r) = self.inst_cache.get(&(t, m)) {
            return r;
        }
        if self.inst_depth >= MAX_INSTANTIATION_DEPTH || self.inst_count >= MAX_INSTANTIATION_COUNT
        {
            self.inst_overflow = true;
            return TypeId::UNRESOLVED;
        }
        self.inst_depth += 1;
        self.inst_count += 1;
        let r = self.instantiate_inner(t, m);
        self.inst_depth -= 1;
        self.inst_cache.insert((t, m), r);
        r
    }

    /// Reports TS2589 at `at` when an instantiation overflowed since the last report, and
    /// starts a new instantiation count.
    /// A tuple over the length limit reports TS2800 at an expression (`is_expr`) and TS2799
    /// at a type node instead.
    pub fn report_instantiation_overflow(&mut self, at: Loc, is_expr: bool) {
        self.inst_count = 0;
        if self.tuple_overflow {
            self.tuple_overflow = false;
            self.inst_overflow = false;
            let d = if is_expr {
                crate::diag::dg::EXPRESSION_PRODUCES_A_TUPLE_TYPE_THAT_IS_TOO_LARGE_TO_REPRESENT_2800
            } else {
                crate::diag::dg::TYPE_PRODUCES_A_TUPLE_TYPE_THAT_IS_TOO_LARGE_TO_REPRESENT_2799
            };
            self.error(at, d, &[]);
            return;
        }
        if !self.inst_overflow {
            return;
        }
        self.inst_overflow = false;
        self.error(
            at,
            crate::diag::dg::TYPE_INSTANTIATION_IS_EXCESSIVELY_DEEP_AND_POSSIBLY_INFINITE_2589,
            &[],
        );
    }

    pub(crate) fn instantiate_alias(&mut self, alias: AliasId, m: MapperId) -> AliasId {
        let s = self.store();
        let Some(info) = s.alias_info(alias) else {
            return AliasId::NONE;
        };
        if info.args == ListId::EMPTY {
            return alias;
        }
        let args = s.list_items(info.args).to_vec();
        let mapped: Vec<TypeId> = args.iter().map(|&a| self.instantiate(a, m)).collect();
        s.alias(info.sym, s.list(&mapped))
    }

    /// An instantiation of `C & T`, `T` read under the constraint `C` implied by the true
    /// branch of a conditional type: it is `T` alone once `T` is known to satisfy `C`, as the
    /// conditional would then pick its true branch, and stays an implied intersection while
    /// `T` is a type variable (tsc: checker.go, instantiateTypeWorker, substitution types).
    fn instantiate_implied(&mut self, t: TypeId, m: MapperId) -> TypeId {
        let Some((base, constraint)) = self.implied_parts(t) else {
            return t;
        };
        let nb = self.instantiate(base, m);
        let nc = self.instantiate(constraint, m);
        let var = self
            .store()
            .flags(nb)
            .intersects(TypeFlags::TYPE_PARAMETER | TypeFlags::INDEXED_ACCESS);
        // (tsc compares restrictive instantiations, where type parameters have no
        // constraint; a still-generic `T` is kept here instead.)
        if !(var && self.is_generic_type(nc))
            && (self.store().flags(nc).intersects(TypeFlags::ANY_OR_UNKNOWN)
                || !self.is_generic_type(nb) && self.quietly(|ck| ck.is_assignable(nb, nc)))
        {
            return nb;
        }
        let r = self.intersection(&[nc, nb]);
        if var {
            self.implied_intersection(r, nb, nc)
        } else {
            r
        }
    }

    fn instantiate_inner(&mut self, t: TypeId, m: MapperId) -> TypeId {
        let s = self.store();
        match s.kind(t) {
            TypeKind::TypeParameter(_) => s.map_type(m, t).unwrap_or(t),
            TypeKind::Union { list, alias } => {
                let items = s.list_items(list).to_vec();
                let mapped: Vec<TypeId> = items.iter().map(|&x| self.instantiate(x, m)).collect();
                // A union's origin (a symbol-less alias) is not carried over.
                let a = if s.union_origin(t).is_some() {
                    AliasId::NONE
                } else {
                    self.instantiate_alias(alias, m)
                };
                self.union_with(&mapped, crate::unions::Reduction::Literal, a)
            }
            TypeKind::Intersection { .. } if self.implied_parts(t).is_some() => {
                self.instantiate_implied(t, m)
            }
            TypeKind::Intersection { list, alias } => {
                let items = s.list_items(list).to_vec();
                let mapped: Vec<TypeId> = items.iter().map(|&x| self.instantiate(x, m)).collect();
                let a = self.instantiate_alias(alias, m);
                self.intersection_with(&mapped, a)
            }
            TypeKind::Index(x) => {
                let i = self.instantiate(x, m);
                self.keyof_type(i)
            }
            TypeKind::IndexedAccess { obj, index } => {
                let o = self.instantiate(obj, m);
                let i = self.instantiate(index, m);
                match self.indexed_access_type(o, i, None) {
                    Some(t) => t,
                    // An instantiation that indexes an object type by a key it lacks is
                    // `unknown` in tsc (`ParamMap[R]` with `R` given an argument that breaks
                    // its constraint, `TOptions['transformer']` with `TOptions = {}`).
                    None if self.missing_key_of_plain_object(o, i) => TypeId::UNKNOWN,
                    None => TypeId::UNRESOLVED,
                }
            }
            TypeKind::Substitution { base, constraint } => {
                let b = self.instantiate(base, m);
                // `NoInfer<T>` stays a marker while its type is generic.
                if constraint == TypeId::UNKNOWN && self.is_no_infer_target(b, 0) {
                    return self.store().substitution(b, TypeId::UNKNOWN);
                }
                b
            }
            TypeKind::StringMapping { sym, ty } => {
                let i = self.instantiate(ty, m);
                self.string_mapping_type(sym, i)
            }
            TypeKind::TemplateLiteral(_) => {
                let Some((texts, types)) = s.template_parts(t) else {
                    return t;
                };
                let texts: Vec<String> = texts
                    .iter()
                    .map(|&x| s.string_value(x).unwrap_or("").to_owned())
                    .collect();
                let types = types.to_vec();
                let mapped: Vec<TypeId> = types.iter().map(|&x| self.instantiate(x, m)).collect();
                self.template_literal_type(&texts, &mapped)
            }
            TypeKind::Conditional(_) => self.instantiate_conditional(t, m),
            TypeKind::Object(o) => {
                let od = s.object(o);
                match od.kind {
                    ObjectKind::Declared { type_params, .. } => {
                        let params = s.list_items(type_params).to_vec();
                        if params.is_empty() {
                            return t;
                        }
                        let args: Vec<TypeId> =
                            params.iter().map(|&p| self.instantiate(p, m)).collect();
                        self.make_reference(t, &args)
                    }
                    ObjectKind::Reference { target, args } => {
                        let items = s.list_items(args).to_vec();
                        let mapped: Vec<TypeId> =
                            items.iter().map(|&x| self.instantiate(x, m)).collect();
                        if let Some(shape) = s.tuple_shape_of(target)
                            && shape
                                .elements
                                .iter()
                                .any(|e| e.kind == TupleElement::VARIADIC)
                        {
                            let shape = shape.clone();
                            let els: Vec<TupleEl> = shape
                                .elements
                                .iter()
                                .zip(mapped.iter())
                                .map(|(e, &ty)| TupleEl {
                                    ty,
                                    kind: if e.kind == TupleElement::VARIADIC {
                                        TupleElement::REST
                                    } else {
                                        e.kind
                                    },
                                    label: e.label,
                                })
                                .collect();
                            return self.create_tuple(&els, shape.readonly);
                        }
                        // A reference that is a generic alias's type keeps the alias
                        // (`type A<T> = T[]`: `A<string>` prints as such).
                        if od.alias != AliasId::NONE {
                            let alias = self.instantiate_alias(od.alias, m);
                            return self.store().reference_aliased(target, &mapped, alias);
                        }
                        s.reference(target, &mapped)
                    }
                    ObjectKind::Anonymous { decl, kind, mapper } => {
                        let alias = self.instantiate_alias(od.alias, m);
                        let outer = if decl.0 == u32::MAX {
                            match self.c.first_decl(SymId(decl.1)) {
                                Some(d) => self.outer_type_params(d),
                                None => Vec::new(),
                            }
                        } else {
                            match Loc::unpack(decl) {
                                Some(l)
                                    if kind == crate::declared::anon::TYPE_LITERAL
                                        || kind == crate::declared::anon::FUNCTION_TYPE =>
                                {
                                    self.referenced_outer_type_params(l)
                                }
                                Some(l) => self.outer_type_params(l),
                                None => Vec::new(),
                            }
                        };
                        let mut targets = Vec::with_capacity(outer.len());
                        let mut changed = false;
                        for &p in &outer {
                            let cur = s.map_type(mapper, p).unwrap_or(p);
                            let nt = self.instantiate(cur, m);
                            changed |= nt != p;
                            targets.push(nt);
                        }
                        let nm = if changed {
                            s.mapper(&outer, &targets)
                        } else {
                            MapperId::IDENTITY
                        };
                        if nm == mapper && alias == od.alias {
                            return t;
                        }
                        let contains = targets
                            .iter()
                            .any(|&x| s.traits(x).contains(TypeTraits::CONTAINS_TYPE_PARAMETER));
                        let flags = od.flags.without(ts_r_types::ObjectFlags::INSTANTIATED);
                        s.anonymous(decl, kind, nm, flags, alias, contains)
                    }
                    ObjectKind::Literal { sym } => {
                        let Some(members) = od.members() else {
                            return t;
                        };
                        let props: Vec<(ts_r_types::StrId, SymId)> = members
                            .names
                            .iter()
                            .copied()
                            .zip(members.props.iter().copied())
                            .collect();
                        let call = members.call.to_vec();
                        let construct = members.construct.to_vec();
                        let index = members.index.to_vec();
                        let mut nprops = Vec::with_capacity(props.len());
                        let mut contains = false;
                        for (n, p) in props {
                            let pt = self.type_of_symbol(p);
                            let it = self.instantiate(pt, m);
                            contains |= s.traits(it).contains(TypeTraits::CONTAINS_TYPE_PARAMETER);
                            let flags = self.c.sym_flags(p);
                            let check = self.c.sym_check_flags(p);
                            let decl = self.c.synth_sym(p).and_then(|x| x.decl);
                            let np = self.c.new_synth(
                                SynthSym::new(n, flags, check)
                                    .with_type(it)
                                    .with_origins(vec![p])
                                    .with_decl(decl),
                            );
                            nprops.push((n, np));
                        }
                        let ncall: Vec<SigId> = call
                            .iter()
                            .map(|&x| self.instantiate_signature(x, m))
                            .collect();
                        let ncons: Vec<SigId> = construct
                            .iter()
                            .map(|&x| self.instantiate_signature(x, m))
                            .collect();
                        let nindex: Vec<IndexInfo> = index
                            .iter()
                            .map(|i| IndexInfo {
                                value: self.instantiate(i.value, m),
                                ..*i
                            })
                            .collect();
                        let mut traits = s
                            .traits(t)
                            .without(TypeTraits::CONTAINS_TYPE_PARAMETER | TypeTraits::FRESH);
                        if contains || !ncall.is_empty() || !ncons.is_empty() {
                            traits |= TypeTraits::CONTAINS_TYPE_PARAMETER;
                        }

                        // Keep the fresh/regular relation for object literals. The
                        // INSTANTIATED mark only tells the declaration writer that this is an
                        // instantiation (its nesting bound).
                        s.literal_object(
                            sym,
                            od.flags | ts_r_types::ObjectFlags::INSTANTIATED,
                            ResolvedMembers::new(nprops, ncall, ncons, nindex),
                            traits,
                        )
                    }
                    ObjectKind::Mapped { decl, mapper } => {
                        self.instantiate_mapped(t, decl, mapper, m)
                    }
                    ObjectKind::TupleTarget { .. } => t,
                }
            }
            _ => t,
        }
    }

    /// `m1` then `m2`: each source of `m1` maps to its target instantiated by `m2`; sources
    /// only in `m2` keep their `m2` targets.
    pub fn compose_mappers(&mut self, m1: MapperId, m2: MapperId) -> MapperId {
        if m1 == MapperId::IDENTITY {
            return m2;
        }
        if m2 == MapperId::IDENTITY || m1.is_special() {
            return m1;
        }
        let s = self.store();
        let (s1, t1) = s.mapper_lists(m1);
        let (s1, t1) = (s1.to_vec(), t1.to_vec());
        let (s2, t2) = s.mapper_lists(m2);
        let (s2, t2) = (s2.to_vec(), t2.to_vec());
        let mut src = s1.clone();
        let mut dst: Vec<TypeId> = t1.iter().map(|&x| self.instantiate(x, m2)).collect();
        for (a, b) in s2.into_iter().zip(t2) {
            if !src.contains(&a) {
                src.push(a);
                dst.push(b);
            }
        }
        s.mapper(&src, &dst)
    }

    /// A signature instantiated by an outer mapper (its own type parameters stay).
    pub fn instantiate_signature(&mut self, sig: SigId, m: MapperId) -> SigId {
        if m == MapperId::IDENTITY {
            return sig;
        }
        if let Some(&r) = self.sig_inst_cache.get(&(sig, m)) {
            return r;
        }
        let outer = m;
        let s = self.store();
        let d = s.sig(sig);
        // A generic signature instantiated by an outer mapper (a method of `Array<X>`) gets
        // fresh copies of its own type parameters whose constraints see the outer mapping
        // (`find<S extends T>` becomes `find<S' extends X>`).
        let own = s.list_items(d.type_params);
        let (m, type_params) = if !own.is_empty()
            && own.iter().all(|&tp| s.map_type(m, tp).is_none())
        {
            let own = own.to_vec();
            // The copies are keyed by the mapper with aliases dropped from the references it
            // maps to: the members of `type A = B<any>` (whose `this` is `A`) and of `B<any>`
            // share them, so a method read through either relates to the other.
            let key = self.mapper_without_reference_aliases(m);
            let s = self.store();
            let clones: Vec<TypeId> = own.iter().map(|&tp| s.cloned_type_param(tp, key)).collect();
            let (src, dst) = s.mapper_lists(m);
            let mut src = src.to_vec();
            let mut dst = dst.to_vec();
            src.extend(own.iter().copied());
            dst.extend(clones.iter().copied());
            let full = s.mapper(&src, &dst);
            // A constraint or default that mentions the signature's own type parameters
            // (`E extends Ext<A, E> extends true ? ...`, `<A = unknown, B = A>`) refers to
            // the copies.
            for (&tp, &clone) in own.iter().zip(&clones) {
                if self
                    .store()
                    .type_param_data(clone)
                    .is_none_or(|x| x.constraint().is_none())
                    && let Some(c) = self.type_param_constraint(tp)
                    && own.iter().any(|&o| self.type_mentions(c, o))
                {
                    let ci = self.instantiate(c, full);
                    if let Some(x) = self.store().type_param_data(clone) {
                        x.set_constraint(Some(ci));
                    }
                }
                if self
                    .store()
                    .type_param_data(clone)
                    .is_none_or(|x| x.default().is_none())
                    && let Some(d) = self.type_param_default(tp)
                    && own.iter().any(|&o| self.type_mentions(d, o))
                {
                    let di = self.instantiate(d, full);
                    if let Some(x) = self.store().type_param_data(clone) {
                        x.set_default(Some(di));
                    }
                }
            }
            (full, s.list(&clones))
        } else {
            (m, d.type_params)
        };
        let params: Vec<Param> = d
            .params
            .iter()
            .cloned()
            .map(|p| Param {
                ty: self.instantiate(p.ty, m),
                ..p
            })
            .collect();
        let d = s.sig(sig);
        let this_type = d.this_type.map(|t| self.instantiate(t, m));
        let d = s.sig(sig);
        let ns = Signature::new(
            d.flags,
            d.decl,
            type_params,
            params,
            this_type,
            d.min_args,
            None,
        )
        .with_target(sig, m);
        let id = s.add_signature(ns);
        self.sig_inst_cache.insert((sig, outer), id);
        id
    }

    /// The type of an instantiation expression (`f<string>`, `typeof f<string>`): each
    /// generic call and construct signature taking that many type arguments, instantiated
    /// with them; the others are dropped. Properties and index signatures stay; a type
    /// without signatures is unchanged. Unions and intersections map over their members, a
    /// type parameter over its constraint.
    ///
    /// With `report` (the type argument list): type arguments that do not satisfy an
    /// applicable signature's constraints get TS2344 (that signature stays generic), and
    /// TS2635 is reported at the first argument, naming the whole type when no member has
    /// an applicable signature, else the first member with signatures but none applicable.
    pub(crate) fn instantiation_expression_type(
        &mut self,
        t: TypeId,
        args: &[TypeId],
        report: Option<Loc>,
    ) -> TypeId {
        let mut st = InstExprState::default();
        let r = self.instantiation_expression_part(t, args, 0, report, &mut st);
        let Some(type_args) = report else {
            return r;
        };
        // An empty list (`f<>`) is the parser's TS1099 only.
        if args.is_empty()
            || self
                .store()
                .flags(t)
                .intersects(TypeFlags::ANY | TypeFlags::NEVER)
            || self.store().is_error(t)
        {
            return r;
        }
        let bad = if st.any_applicable {
            st.non_applicable
        } else {
            Some(t)
        };
        if let Some(bad) = bad {
            let at = match *self.node(type_args) {
                ts_r_ast::Node::TypeArgs { args } => self
                    .ast(type_args.file)
                    .list(args)
                    .first()
                    .map_or(type_args, |&a| Loc::new(type_args.file, a)),
                _ => type_args,
            };
            let shown = self.type_to_string(bad);
            self.error(
                at,
                crate::diag::dg::TYPE_0_HAS_NO_SIGNATURES_FOR_WHICH_THE_TYPE_ARGUMENT_LIST_IS_APPLICABLE_2635,
                &[&shown],
            );
        }
        r
    }

    fn instantiation_expression_part(
        &mut self,
        t: TypeId,
        args: &[TypeId],
        depth: u32,
        report: Option<Loc>,
        st: &mut InstExprState,
    ) -> TypeId {
        if depth > 32 {
            return t;
        }
        let s = self.store();
        let f = s.flags(t);
        if f.contains(TypeFlags::UNION) {
            let ms = s.members_of_union(t).to_vec();
            let mut parts = Vec::with_capacity(ms.len());
            for &m in &ms {
                // Each member is judged on its own (tsc's `getInstantiatedType`).
                let mut inner = InstExprState::default();
                parts.push(self.instantiation_expression_part(
                    m,
                    args,
                    depth + 1,
                    report,
                    &mut inner,
                ));
                st.any_applicable |= inner.any_applicable;
                st.has_signatures |= inner.has_signatures;
                if inner.has_signatures && !inner.any_applicable && st.non_applicable.is_none() {
                    st.non_applicable = Some(m);
                }
                if st.non_applicable.is_none() {
                    st.non_applicable = inner.non_applicable;
                }
            }
            if parts == ms {
                return t;
            }
            return self.union(&parts);
        }
        if f.contains(TypeFlags::INTERSECTION) {
            let ms = s.members_of_union(t).to_vec();
            let parts: Vec<TypeId> = ms
                .iter()
                .map(|&m| self.instantiation_expression_part(m, args, depth + 1, report, st))
                .collect();
            if parts == ms {
                return t;
            }
            return self.intersection(&parts);
        }
        if f.contains(TypeFlags::TYPE_PARAMETER) {
            let c = self.base_constraint(t);
            if c != t && c != TypeId::UNKNOWN {
                let i = self.instantiation_expression_part(c, args, depth + 1, report, st);
                if i != c {
                    return i;
                }
            }
            return t;
        }
        if !f.contains(TypeFlags::OBJECT) {
            return t;
        }
        let m = self.resolved_members(t);
        let props: Vec<(ts_r_types::StrId, SymId)> = m
            .names
            .iter()
            .copied()
            .zip(m.props.iter().copied())
            .collect();
        let (call, construct, index) = (m.call.to_vec(), m.construct.to_vec(), m.index.to_vec());
        if call.is_empty() && construct.is_empty() {
            return t;
        }
        let contains = s.traits(t).contains(TypeTraits::CONTAINS_TYPE_PARAMETER)
            || args
                .iter()
                .any(|&a| s.traits(a).contains(TypeTraits::CONTAINS_TYPE_PARAMETER));
        st.has_signatures = true;
        let mut changed = false;
        let mut applicable = false;
        let mut inst = |ck: &mut Self, sigs: &[SigId]| -> Vec<SigId> {
            let mut out = Vec::new();
            for &sig in sigs {
                let tps = ck
                    .store()
                    .list_items(ck.store().sig(sig).type_params)
                    .to_vec();
                if tps.is_empty() || !ck.type_arg_count_fits(&tps, args.len()) {
                    changed = true;
                    continue;
                }
                applicable = true;
                changed = true;
                if ck.type_args_satisfy_constraints(sig, args, report) == Some(false) {
                    out.push(sig);
                    continue;
                }
                let filled = ck.fill_type_args(&tps, args);
                let mapper = ck.store().mapper(&tps, &filled);
                out.push(ck.instantiate_signature_full(sig, mapper));
            }
            out
        };
        let ncall = inst(self, &call);
        let ncons = inst(self, &construct);
        st.any_applicable |= applicable;
        if !changed {
            return t;
        }
        let traits = if contains {
            TypeTraits::CONTAINS_TYPE_PARAMETER
        } else {
            TypeTraits::NONE
        };
        self.store().literal_object(
            SymId::NONE,
            ts_r_types::ObjectFlags::ANONYMOUS,
            ResolvedMembers::new(props, ncall, ncons, index),
            traits,
        )
    }

    /// `m` with each target that is a reference named by an alias replaced by the plain
    /// reference.
    fn mapper_without_reference_aliases(&mut self, m: MapperId) -> MapperId {
        if m.is_special() {
            return m;
        }
        let s = self.store();
        let (src, dst) = s.mapper_lists(m);
        if !dst
            .iter()
            .any(|&t| s.object_of(t).is_some_and(|o| o.alias != AliasId::NONE))
        {
            return m;
        }
        let src = src.to_vec();
        let dst: Vec<TypeId> = dst
            .iter()
            .map(|&t| self.without_reference_alias(t))
            .collect();
        self.store().mapper(&src, &dst)
    }

    /// A generic signature with its type parameters replaced (the result is not generic).
    pub fn instantiate_signature_full(&mut self, sig: SigId, m: MapperId) -> SigId {
        let r = self.instantiate_signature(sig, m);
        let s = self.store();
        let d = s.sig(r);
        if d.type_params == ListId::EMPTY {
            return r;
        }
        let ns = Signature::new(
            d.flags,
            d.decl,
            ListId::EMPTY,
            d.params.to_vec(),
            d.this_type,
            d.min_args,
            None,
        )
        .with_target(sig, m);
        s.add_signature(ns)
    }

    /// Whether `t` mentions the type parameter `tp`.
    pub fn type_mentions(&mut self, t: TypeId, tp: TypeId) -> bool {
        if t == tp {
            return true;
        }
        if !self.could_contain_type_params(t) {
            return false;
        }
        // A probe, not a real instantiation: it must not count towards (or trip) the
        // TS2589 limits of the statement being checked.
        let (count, overflow) = (self.inst_count, self.inst_overflow);
        let m = self.store().mapper(&[tp], &[TypeId::NEVER]);
        let r = self.instantiate(t, m) != t;
        self.inst_count = count;
        self.inst_overflow = overflow;
        r
    }

    /// Whether a type contains type parameters (by its interned summary bit).
    /// Whether `NoInfer<t>` stays a marker: `t` is generic, or an object type (or a union
    /// or intersection member) that could contain type parameters (`NoInfer<T[]>`).
    fn is_no_infer_target(&mut self, t: TypeId, depth: u32) -> bool {
        let f = self.store().flags(t);
        if depth < 16 && f.intersects(TypeFlags::UNION | TypeFlags::INTERSECTION) {
            let members = self.store().members_of_union(t).to_vec();
            return members
                .iter()
                .any(|&m| self.is_no_infer_target(m, depth + 1));
        }
        if f.contains(TypeFlags::OBJECT) {
            return self.could_contain_type_params(t);
        }
        self.is_generic_type(t)
    }

    pub fn could_contain_type_params(&self, t: TypeId) -> bool {
        self.store()
            .traits(t)
            .contains(TypeTraits::CONTAINS_TYPE_PARAMETER)
    }

    /// Whether `t` is a type parameter (or union containing one).
    pub fn is_type_param(&self, t: TypeId) -> bool {
        self.store().flags(t).contains(TypeFlags::TYPE_PARAMETER)
    }
}

impl<'c, 'a> Ck<'c, 'a> {
    /// Whether `index` is a string or number literal naming no property or index signature
    /// of the object type `obj` (`{}['transformer']` is `unknown` once instantiated).
    fn missing_key_of_plain_object(&mut self, obj: TypeId, index: TypeId) -> bool {
        let s = self.store();
        if !s
            .flags(index)
            .intersects(TypeFlags::STRING_LITERAL | TypeFlags::NUMBER_LITERAL)
            || s.flags(index).contains(TypeFlags::UNION)
            || s.object_of(obj).is_none()
        {
            return false;
        }
        let Some(name) = self.property_name_of_type(index) else {
            return false;
        };
        self.property_of_type(obj, &name).is_none()
            && self.index_info_of(obj, TypeId::STRING).is_none()
            && self.index_info_of(obj, TypeId::NUMBER).is_none()
    }
}

/// What an instantiation expression found while mapping a type.
#[derive(Default)]
struct InstExprState {
    /// Some (sub)type had call or construct signatures.
    has_signatures: bool,
    /// Some signature took the type arguments.
    any_applicable: bool,
    /// The first union member with signatures but none applicable.
    non_applicable: Option<TypeId>,
}
