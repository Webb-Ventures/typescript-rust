//! Type argument inference: candidates are collected by walking a source type against a
//! target type mentioning the inferred type parameters, then combined (tsc's covariant
//! "common supertype" with literal widening rules, contravariant "common subtype").

use rustc_hash::FxHashSet;
use ts_r_binder::SymbolFlags;
use ts_r_types::{
    AliasId, ListId, ObjectKind, SigFlags, SigId, TupleElement, TypeFlags, TypeId, TypeKind,
};

use crate::checker::Ck;
use crate::members::SigKind;
use crate::unions::Reduction;

const MAX_INFERENCE_DEPTH: u32 = 10;

/// Candidates for the type parameters of one inference.
#[derive(Debug, Clone)]
pub struct InferenceContext {
    pub params: Vec<TypeId>,
    pub candidates: Vec<Vec<TypeId>>,
    pub contra: Vec<Vec<TypeId>>,
    /// Some candidate was inferred at the top level (directly to the type parameter).
    pub top_level: Vec<bool>,
    /// Some candidate was inferred from inside another type (a type argument, property,
    /// element or signature: `[]` or `Obs<never>` to `T[]` or `Obs<T>`); the literal
    /// candidates then stay unwidened (`g<T>(a: T[], b: T)` with `([], 1)` infers `1`).
    pub nested: Vec<bool>,
    /// Inside a property, index signature or signature of the target.
    structural: u32,
    /// Inferred and fixed types.
    pub fixed: Vec<Option<TypeId>>,
    /// The signature's declared return type (for the literal widening rule).
    pub ret: Option<TypeId>,
    /// Set while fixing type parameters for contextual typing: fixed literal inferences are
    /// widened even when the parameter is at the top level of the return type.
    pub fixing: bool,
    /// Candidates combine into their union instead of a common supertype (keys inferred
    /// from mapped type constraints).
    pub combine: Vec<bool>,
    /// Lower-priority candidates (through homomorphic mapped types such as `Partial<T>`),
    /// used only when there are no others.
    pub weak: Vec<Vec<TypeId>>,
    /// Set while collecting lower-priority candidates.
    pub weak_mode: bool,
    visited: FxHashSet<(TypeId, TypeId, bool)>,
    /// Inferences made to a type parameter so far (duplicates included), not counting
    /// weak ones: a union member that only inferred weakly (through a conditional's
    /// branches or a homomorphic mapped type) still goes to the naked type parameter
    /// (`2` and `{ foo }` to `T | DeepPartial<T>`).
    made: u32,
    /// Type parameters of generic source signatures being inferred from: a source that is
    /// one of them, or a union containing one, is not a candidate.
    pub(crate) non_inferrable: Vec<TypeId>,
    /// Generic source signatures are inferred from with their type parameters replaced by
    /// their constraints (`unknown` without one): conditional types
    /// (`ReturnType<typeof f>` of a generic `f`).
    pub base_signatures: bool,
    /// Inferring from a call's contextual type to its return type: through a non-simple
    /// homomorphic mapped type an array source still implies an array.
    pub from_return_context: bool,
    /// Inside the parameters of a method or construct signature: contravariant inferences
    /// are recorded as ordinary candidates (method parameters are bivariant).
    pub bivariant: bool,
    /// Return type inferences: a union inference that breaks the constraint keeps the
    /// members that meet it (`T | Promise<T>` for `<S extends string>` gives `T`).
    pub filter_by_constraint: bool,
    /// The lowest-priority inferences: string literal keys inferred to `keyof T` give `T`
    /// an object with those keys typed `any` (`{ a: any; }`), combined by intersection,
    /// used only without any other candidate.
    pub literal_keyof: Vec<Vec<TypeId>>,
    /// Candidates inferred through the type arguments of two instantiations of one
    /// generic type (`Box<number>` to `Box<T>`, `number[]` to `T[]`); they win over the
    /// others (observed: `f<T>(a: T, b: Box<T>)` with `(string, Box<number>)` infers
    /// `number`, with `b: { v: T }` it infers `string`).
    pub preferred: Vec<Vec<TypeId>>,
    /// Inside such type arguments.
    via_type_args: u32,
    /// The element type of an empty array literal (`[]`) stays the implicit `never` as a
    /// candidate (a conditional type's `infer` positions); otherwise it is a plain `never`
    /// candidate (`from<T>(v: T[])` with `[]` infers `never`).
    pub implicit_never: bool,
}

impl InferenceContext {
    /// Forgets which (source, target) pairs were inferred from, so a pair can be inferred
    /// from again with what is known now.
    pub(crate) fn clear_visited(&mut self) {
        self.visited.clear();
    }

    pub fn new(params: &[TypeId]) -> InferenceContext {
        InferenceContext {
            params: params.to_vec(),
            candidates: vec![Vec::new(); params.len()],
            contra: vec![Vec::new(); params.len()],
            top_level: vec![false; params.len()],
            nested: vec![false; params.len()],
            structural: 0,
            fixed: vec![None; params.len()],
            ret: None,
            fixing: false,
            combine: vec![false; params.len()],
            weak: vec![Vec::new(); params.len()],
            weak_mode: false,
            visited: FxHashSet::default(),
            made: 0,
            non_inferrable: Vec::new(),
            base_signatures: false,
            from_return_context: false,
            bivariant: false,
            filter_by_constraint: false,
            literal_keyof: vec![Vec::new(); params.len()],
            preferred: vec![Vec::new(); params.len()],
            via_type_args: 0,
            implicit_never: false,
        }
    }

    /// Whether type parameter `j` has inferences (or is fixed).
    pub(crate) fn has_inferences(&self, j: usize) -> bool {
        self.fixed[j].is_some()
            || !self.candidates[j].is_empty()
            || !self.contra[j].is_empty()
            || !self.weak[j].is_empty()
            || !self.preferred[j].is_empty()
            || !self.literal_keyof[j].is_empty()
    }

    /// Takes over the inferences `src` made for type parameter `j` (same parameters).
    pub(crate) fn adopt(&mut self, src: &InferenceContext, j: usize) {
        self.candidates[j] = src.candidates[j].clone();
        self.contra[j] = src.contra[j].clone();
        self.top_level[j] = src.top_level[j];
        self.nested[j] = src.nested[j];
        self.combine[j] = src.combine[j];
        self.weak[j] = src.weak[j].clone();
        self.literal_keyof[j] = src.literal_keyof[j].clone();
        self.preferred[j] = src.preferred[j].clone();
    }

    pub(crate) fn index_of(&self, t: TypeId) -> Option<usize> {
        self.params.iter().position(|&p| p == t)
    }
}

impl<'c, 'a> Ck<'c, 'a> {
    /// Infers from `source` to `target` (covariant position).
    pub fn infer_types(
        &mut self,
        ctx: &mut InferenceContext,
        source: TypeId,
        target: TypeId,
        depth: u32,
    ) {
        self.infer_from(ctx, source, target, false, depth);
    }

    pub(crate) fn infer_from(
        &mut self,
        ctx: &mut InferenceContext,
        source: TypeId,
        target: TypeId,
        contra: bool,
        depth: u32,
    ) {
        if depth > MAX_INFERENCE_DEPTH || !self.could_contain_type_params(target) {
            return;
        }
        if source == TypeId::WILDCARD || source == TypeId::SILENT_NEVER {
            return;
        }
        if let Some(i) = ctx.index_of(target) {
            if ctx.fixed[i].is_some() {
                return;
            }
            let source = if source == TypeId::IMPLICIT_NEVER && !ctx.implicit_never {
                TypeId::NEVER
            } else {
                source
            };
            if !ctx.non_inferrable.is_empty() {
                let ni = std::mem::take(&mut ctx.non_inferrable);
                let skip = self.mentions_at_top(source, &ni, 0);
                ctx.non_inferrable = ni;
                if skip {
                    return;
                }
            }
            if !ctx.weak_mode {
                ctx.made += 1;
            }
            if ctx.weak_mode {
                if !ctx.weak[i].contains(&source) {
                    ctx.weak[i].push(source);
                }
                return;
            }
            if contra && !ctx.bivariant {
                if !ctx.contra[i].contains(&source) {
                    ctx.contra[i].push(source);
                }
            } else {
                if ctx.via_type_args > 0 || ctx.structural > 0 {
                    ctx.nested[i] = true;
                }
                if !ctx.candidates[i].contains(&source) {
                    ctx.candidates[i].push(source);
                    if depth == 0 {
                        ctx.top_level[i] = true;
                    }
                }
                if ctx.via_type_args > 0 && !ctx.preferred[i].contains(&source) {
                    ctx.preferred[i].push(source);
                }
            }
            return;
        }
        // A `NoInfer<X>` source infers as `X` (observed: `Lazy<NoInfer<R>>` still matches
        // `Lazy<Router<any, infer T>>`); only a `NoInfer` target blocks inference.
        if let TypeKind::Substitution { base, constraint } = self.store().kind(source)
            && constraint == TypeId::UNKNOWN
        {
            return self.infer_from(ctx, base, target, contra, depth + 1);
        }
        if !ctx.visited.insert((source, target, contra)) {
            return;
        }
        let s = self.store();
        // Instantiations of the same alias infer from the alias's type arguments
        // (`Output<R>` to `Output<T> = OutputInstance<T> & Lifted<T>`); a contravariant
        // parameter's argument infers contravariantly (`Func<T> = ((x: T) => void) | undefined`).
        // Object type aliases measure their variances; others infer covariantly.
        if let (Some(sa), Some(ta)) = (s.alias_of(source), s.alias_of(target))
            && sa.sym == ta.sym
            && sa.args != ListId::EMPTY
        {
            let sargs = s.list_items(sa.args).to_vec();
            let targs = s.list_items(ta.args).to_vec();
            let measure = s
                .flags(target)
                .intersects(TypeFlags::OBJECT | TypeFlags::UNION);
            let cov_only = s
                .flags(target)
                .intersects(TypeFlags::INTERSECTION | TypeFlags::CONDITIONAL);
            if sargs.len() == targs.len() && (measure || cov_only) {
                let variances = if measure {
                    self.variances_of(crate::variance::VarianceKey::Alias(sa.sym))
                } else {
                    None
                };
                ctx.via_type_args += 1;
                for (i, (a, b)) in sargs.into_iter().zip(targs).enumerate() {
                    let flip = variances.as_ref().is_some_and(|v| {
                        v.list.get(i) == Some(&crate::variance::Variance::Contravariant)
                    });
                    self.infer_from(ctx, a, b, contra ^ flip, depth + 1);
                }
                ctx.via_type_args -= 1;
                return;
            }
        }
        let s = self.store();
        // Type operators of the same kind infer operand to operand: `S[K]` to `T[L]` infers
        // `S` to `T` and `K` to `L`; `keyof S` to `keyof T` infers `S` to `T`
        // contravariantly (tsc: `inferFromTypes` in `inference.go`).
        match (s.kind(source), s.kind(target)) {
            (
                TypeKind::IndexedAccess { obj: so, index: si },
                TypeKind::IndexedAccess { obj: to, index: ti },
            ) => {
                self.infer_from(ctx, so, to, contra, depth + 1);
                self.infer_from(ctx, si, ti, contra, depth + 1);
                return;
            }
            (TypeKind::Index(so), TypeKind::Index(to)) => {
                self.infer_from(ctx, so, to, !contra, depth + 1);
                return;
            }
            _ => {}
        }
        let tf = s.flags(target);
        let sf = s.flags(source);
        if tf.contains(TypeFlags::UNION) {
            let tmembers = s.members_of_union(target).to_vec();
            let (naked, others): (Vec<TypeId>, Vec<TypeId>) =
                tmembers.iter().partition(|&&m| ctx.index_of(m).is_some());
            let smembers = self.union_members(source);
            let mut remaining = Vec::new();
            for sm in smembers {
                let matched = others.iter().find(|&&o| self.closely_matches(sm, o));
                match matched {
                    Some(&o) => self.infer_from(ctx, sm, o, contra, depth + 1),
                    None => remaining.push(sm),
                }
            }
            if remaining.is_empty() {
                // Every source member matched another member: the naked type parameter
                // still gets the source, as a lower-priority candidate (`string` to
                // `K | string | symbol` gives `K = string`; `v(s, 1)` with
                // `(x: T | string, y: T)` gives `T = number`).
                if naked.len() == 1 && !ctx.weak_mode {
                    ctx.weak_mode = true;
                    self.infer_from(ctx, source, naked[0], contra, depth);
                    ctx.weak_mode = false;
                }
                return;
            }
            if naked.len() == 1 {
                // Each source member infers to the other members first; only members that
                // inferred nothing there go to the naked type parameter (`X[] | never[]` to
                // `U | readonly U[]` gives `U = X`, not `X[]`).
                let mut to_naked = Vec::new();
                for &sm in &remaining {
                    let before = ctx.made;
                    for &o in &others {
                        if self.could_contain_type_params(o) {
                            self.infer_from(ctx, sm, o, contra, depth + 1);
                        }
                    }
                    if ctx.made == before {
                        to_naked.push(sm);
                    }
                }
                if !to_naked.is_empty() {
                    let rest = self.union_with(&to_naked, Reduction::None, AliasId::NONE);
                    self.infer_from(ctx, rest, naked[0], contra, depth);
                }
                return;
            }
            let rest = self.union_with(&remaining, Reduction::None, AliasId::NONE);
            for &o in &others {
                if self.could_contain_type_params(o) {
                    self.infer_from(ctx, rest, o, contra, depth + 1);
                }
            }
            // From a call's contextual type, several naked type parameters (`A | B`) each
            // get the source as lower-priority candidates (observed with tsc 7.0.2:
            // `Promise<R>` to `Promise<A | B>` makes `R` the contextual type of the
            // callback returning `A`). Not modelled for a type variable source (an
            // enclosing function's `Promise<E>` keeps `then`'s defaulted parameters open).
            if naked.len() > 1
                && ctx.from_return_context
                && !ctx.weak_mode
                && !remaining
                    .iter()
                    .any(|&m| self.store().flags(m).intersects(TypeFlags::INSTANTIABLE))
            {
                ctx.weak_mode = true;
                for &n in &naked {
                    self.infer_from(ctx, rest, n, contra, depth);
                }
                ctx.weak_mode = false;
            }
            return;
        }
        if tf.contains(TypeFlags::INTERSECTION) {
            // A union source infers member by member, unless a member of the target is a
            // naked type parameter, which takes the whole union (`AB<string>` to
            // `T & { a: U }` gives `T = AB<string>`).
            if sf.contains(TypeFlags::UNION)
                && !s
                    .members_of_union(target)
                    .iter()
                    .any(|&m| ctx.index_of(m).is_some())
            {
                for m in s.members_of_union(source).to_vec() {
                    self.infer_from(ctx, m, target, contra, depth + 1);
                }
                return;
            }
            let mut targets = s.members_of_union(target).to_vec();
            let mut source = source;
            // Members the two intersections share are dropped first (`C<S> & false` to
            // `C<T> & false` infers from `C<S>` to `C<T>`).
            if sf.contains(TypeFlags::INTERSECTION) {
                let mut sources = s.members_of_union(source).to_vec();
                let before = sources.len();
                sources.retain(|m| !targets.contains(m));
                if sources.len() != before {
                    targets.retain(|m| !s.members_of_union(source).contains(m));
                    if sources.is_empty() || targets.is_empty() {
                        return;
                    }
                    source = if sources.len() == 1 {
                        sources[0]
                    } else {
                        self.intersection(&sources)
                    };
                }
            }
            for m in targets {
                self.infer_from(ctx, source, m, contra, depth + 1);
            }
            return;
        }
        // String literal keys to `keyof T`: `T` has (at least) those properties.
        if let ts_r_types::TypeKind::Index(operand) = s.kind(target)
            && let Some(i) = ctx.index_of(operand)
            && !ctx.weak_mode
            && self.every_member(source, |m| s.flags(m).contains(TypeFlags::STRING_LITERAL))
        {
            if ctx.fixed[i].is_some() {
                return;
            }
            let mut members = Vec::new();
            for m in self.union_members(source) {
                let Some(name) = s.string_value(m) else {
                    continue;
                };
                let nid = s.intern_str(name);
                if members.iter().any(|&(n, _)| n == nid) {
                    continue;
                }
                let p = self.c.new_synth(
                    crate::symbols::SynthSym::new(nid, ts_r_binder::SymbolFlags::PROPERTY, 0)
                        .with_type(TypeId::ANY),
                );
                members.push((nid, p));
            }
            let obj = s.literal_object(
                ts_r_types::SymId::NONE,
                ts_r_types::ObjectFlags::ANONYMOUS,
                ts_r_types::ResolvedMembers::new(members, Vec::new(), Vec::new(), Vec::new()),
                ts_r_types::TypeTraits::NONE,
            );
            ctx.literal_keyof[i].push(obj);
            return;
        }
        if sf.contains(TypeFlags::UNION) {
            for m in s.members_of_union(source).to_vec() {
                self.infer_from(ctx, m, target, contra, depth + 1);
            }
            return;
        }
        if tf.contains(TypeFlags::TEMPLATE_LITERAL) {
            self.infer_to_template(ctx, source, target, contra, depth);
            return;
        }
        // A conditional target infers to both branches as weak candidates (used only
        // without others: `Promise<Awaited<T>>` from `Promise<X>` gives `T = X`).
        if tf.contains(TypeFlags::CONDITIONAL) {
            // Conditional to conditional: check, extends and branch types infer pairwise.
            if sf.contains(TypeFlags::CONDITIONAL)
                && let (Some(sd), Some(d)) =
                    (s.conditional_data(source), s.conditional_data(target))
            {
                let sr = *s.conditional_root_data(sd.root);
                let tr = *s.conditional_root_data(d.root);
                let mut pairs = Vec::with_capacity(4);
                pairs.push((
                    self.instantiate(sr.check, sd.mapper),
                    self.instantiate(tr.check, d.mapper),
                ));
                pairs.push((
                    self.instantiate(sr.extends, sd.mapper),
                    self.instantiate(tr.extends, d.mapper),
                ));
                for yes in [true, false] {
                    let a = self.conditional_branch(sr.decl, yes);
                    let b = self.conditional_branch(tr.decl, yes);
                    pairs.push((
                        self.instantiate(a, sd.mapper),
                        self.instantiate(b, d.mapper),
                    ));
                }
                for (a, b) in pairs {
                    self.infer_from(ctx, a, b, contra, depth + 1);
                }
                return;
            }
            let s = self.store();
            if let Some(d) = s.conditional_data(target) {
                let root = *s.conditional_root_data(d.root);
                let a = self.conditional_branch(root.decl, true);
                let b = self.conditional_branch(root.decl, false);
                let a = self.instantiate(a, d.mapper);
                let b = self.instantiate(b, d.mapper);
                let saved = ctx.weak_mode;
                ctx.weak_mode = true;
                self.infer_from(ctx, source, a, contra, depth + 1);
                self.infer_from(ctx, source, b, contra, depth + 1);
                ctx.weak_mode = saved;
            }
            return;
        }
        if tf.intersects(TypeFlags::INDEXED_ACCESS | TypeFlags::INDEX) {
            return;
        }
        if !tf.contains(TypeFlags::OBJECT) {
            return;
        }
        if s.object_of(target)
            .is_some_and(|o| matches!(o.kind, ObjectKind::Mapped { .. }))
            && self.is_generic_mapped_type(target)
        {
            self.infer_to_mapped(ctx, source, target, contra, depth);
            return;
        }
        let sf_original = sf;
        let source = if sf.intersects(TypeFlags::PRIMITIVE) {
            self.apparent_type(source)
        } else if sf.intersects(TypeFlags::TYPE_PARAMETER) {
            // A type parameter (a class's `this`) infers to an object target through its
            // constraint (`new Wrapper(this)` in `Subject<T>` infers `T`).
            match self.base_constraint(source) {
                c if c != TypeId::UNRESOLVED && c != source => c,
                _ => source,
            }
        } else {
            source
        };
        let sf = self.store().flags(source);
        if !sf.intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION) {
            return;
        }
        // Same generic target: infer from type arguments.
        let sk = self.store().object_of(source).map(|o| o.kind);
        let tk = self.store().object_of(target).map(|o| o.kind);
        if let (
            Some(ObjectKind::Reference {
                target: st,
                args: sa,
            }),
            Some(ObjectKind::Reference {
                target: tt,
                args: ta,
            }),
        ) = (sk, tk)
            && st == tt
            && self.store().tuple_shape_of(st).is_none()
        {
            let sa = self.store().list_items(sa).to_vec();
            let ta = self.store().list_items(ta).to_vec();
            ctx.via_type_args += 1;
            for (a, b) in sa.into_iter().zip(ta) {
                self.infer_from(ctx, a, b, contra, depth + 1);
            }
            ctx.via_type_args -= 1;
            return;
        }
        // Arrays and tuples (a primitive's wrapper type infers nothing to an array:
        // `A | A[]` from `"z"` gives `"z"`).
        if self.c.is_array_like_type(target) && sf_original.intersects(TypeFlags::PRIMITIVE) {
            return;
        }
        if self.c.is_array_like_type(target) && self.c.is_array_like_type(source) {
            if let Some(tshape) = self.store().tuple_shape_of(target).cloned() {
                let targs = self.c.type_args_of_tuple(target).to_vec();
                if let Some(sshape) = self.store().tuple_shape_of(source).cloned() {
                    let sargs = self.c.type_args_of_tuple(source).to_vec();
                    ctx.via_type_args += 1;
                    self.infer_tuple_to_tuple(ctx, &sshape, &sargs, &tshape, &targs, contra, depth);
                    ctx.via_type_args -= 1;
                } else if let Some(se) = self.element_type_of_array_like(source) {
                    for (i, el) in tshape.elements.iter().enumerate() {
                        if let Some(&ta) = targs.get(i) {
                            if el.kind == TupleElement::VARIADIC {
                                self.infer_from(ctx, source, ta, contra, depth + 1);
                            } else {
                                self.infer_from(ctx, se, ta, contra, depth + 1);
                            }
                        }
                    }
                }
                return;
            }
            if let (Some(se), Some(te)) = (
                self.element_type_of_array_like(source),
                self.element_type_of_array_like(target),
            ) {
                ctx.via_type_args += 1;
                self.infer_from(ctx, se, te, contra, depth + 1);
                ctx.via_type_args -= 1;
                return;
            }
        }
        // Structural inference, only between types that may be related: each having a
        // required property the other lacks (`Uuid` against `FindOperator<T>`) infers
        // nothing.
        if self.has_unmatched_property(source, target)
            && self.has_unmatched_property(target, source)
        {
            return;
        }
        if let Some(b) = self.rel.budget.as_mut() {
            // A budgeted declaration check (see `RelState::budget`).
            if *b == 0 {
                return;
            }
            *b -= 1;
        }
        let tprops = self.properties_of(target);
        ctx.structural += 1;
        for (n, tp) in tprops {
            let tpt = self.type_of_symbol(tp);
            if !self.could_contain_type_params(tpt) {
                continue;
            }
            if let Some(sp) = self.property_of_type_id(source, n) {
                let spt = self.type_of_symbol(sp);
                let tpt2 = self.remove_missing(tpt);
                self.infer_from(ctx, spt, tpt2, contra, depth + 1);
            }
        }
        for kind in [SigKind::Call, SigKind::Construct] {
            // Signatures pair up from the end; target signatures beyond the source's
            // count pair with its first (a one-signature function fills every slot of
            // `{ (...a: infer A1): infer R1; (...a: infer A2): infer R2 }`).
            let tsigs = self.signatures_of(target, kind).to_vec();
            let ssigs = self.signatures_of(source, kind).to_vec();
            if ssigs.is_empty() {
                continue;
            }
            let (sl, tl) = (ssigs.len(), tsigs.len());
            for (i, &ts) in tsigs.iter().enumerate() {
                let si = if sl >= tl {
                    sl - tl + i
                } else {
                    i.saturating_sub(tl - sl)
                };
                self.infer_from_signatures(ctx, ssigs[si], ts, contra, depth + 1);
            }
        }
        self.infer_from_index_infos(ctx, source, target, contra, depth);
        ctx.structural -= 1;
    }

    fn infer_from_index_infos(
        &mut self,
        ctx: &mut InferenceContext,
        source: TypeId,
        target: TypeId,
        contra: bool,
        depth: u32,
    ) {
        let at = self.apparent_type(target);
        let tm = self.resolved_members(at).index.to_vec();
        for ti in tm {
            if !self.could_contain_type_params(ti.value) {
                continue;
            }
            if let Some(si) = self.index_info_of(source, ti.key) {
                self.infer_from(ctx, si.value, ti.value, contra, depth + 1);
            } else if ti.key == TypeId::STRING || ti.key == TypeId::NUMBER {
                // Implicit index signature of object literals, type literals and mapped
                // types: the union of the applicable property types, optional ones without
                // their `undefined` (`Object.entries(Partial<Record<K, V>>)` infers `V`).
                if !self.has_inferable_index(source) {
                    continue;
                }
                let mut types = Vec::new();
                for (n, p) in self.properties_of(source) {
                    let name = self.store().str(n);
                    if name.starts_with("__@")
                        || (ti.key == TypeId::NUMBER && name.parse::<f64>().is_err())
                    {
                        continue;
                    }
                    let mut pt = self.type_of_symbol(p);
                    if self.c.sym_flags(p).intersects(SymbolFlags::OPTIONAL) {
                        pt = self.remove_optionality(pt);
                    }
                    types.push(pt);
                }
                if !types.is_empty() {
                    let u = self.union(&types);
                    self.infer_from(ctx, u, ti.value, contra, depth + 1);
                }
            }
        }
    }

    /// `b` has a required property `a` lacks.
    fn has_unmatched_property(&mut self, a: TypeId, b: TypeId) -> bool {
        for (n, p) in self.properties_of(b) {
            if self.c.sym_flags(p).intersects(SymbolFlags::OPTIONAL) {
                continue;
            }
            if self.property_of_type_id(a, n).is_none() {
                return true;
            }
        }
        false
    }

    /// Tuple to tuple: leading and trailing fixed elements pair up; a variadic `...T` target
    /// element receives the middle of the source as a tuple, a rest `...X[]` element its
    /// element types.
    #[allow(clippy::too_many_arguments)]
    fn infer_tuple_to_tuple(
        &mut self,
        ctx: &mut InferenceContext,
        sshape: &ts_r_types::TupleShape,
        sargs: &[TypeId],
        tshape: &ts_r_types::TupleShape,
        targs: &[TypeId],
        contra: bool,
        depth: u32,
    ) {
        let fixed = |k: u8| k == TupleElement::REQUIRED || k == TupleElement::OPTIONAL;
        let slen = sshape.elements.len();
        let tlen = tshape.elements.len();
        let Some(vi) = tshape.elements.iter().position(|e| !fixed(e.kind)) else {
            for i in 0..slen.min(tlen) {
                if !fixed(sshape.elements[i].kind) {
                    let et = self.element_type_of_array_like_arg(sshape.elements[i].kind, sargs[i]);
                    self.infer_from(ctx, et, targs[i], contra, depth + 1);
                    break;
                }
                self.infer_from(ctx, sargs[i], targs[i], contra, depth + 1);
            }
            return;
        };
        let s_var = sshape
            .elements
            .iter()
            .position(|e| !fixed(e.kind))
            .unwrap_or(slen);
        // Leading elements.
        for i in 0..vi.min(s_var) {
            self.infer_from(ctx, sargs[i], targs[i], contra, depth + 1);
        }
        // `[...A, ...B]` with `A` already inferred as a fixed-length tuple (from the rest
        // arguments of `f.bind(null, x)`): `A` takes that many source elements, `B` the rest.
        // Without a known arity the split is unknown: nothing is inferred.
        if vi + 2 == tlen
            && tshape.elements[vi].kind == TupleElement::VARIADIC
            && tshape.elements[vi + 1].kind == TupleElement::VARIADIC
        {
            if let Some(n) = self.implied_arity(ctx, targs[vi])
                && vi + n <= s_var
            {
                let a = self.slice_tuple(sshape, sargs, vi, vi + n);
                let b = self.slice_tuple(sshape, sargs, vi + n, slen);
                self.infer_from(ctx, a, targs[vi], contra, depth + 1);
                self.infer_from(ctx, b, targs[vi + 1], contra, depth + 1);
            }
            return;
        }
        // Trailing elements (only when they line up with fixed source elements).
        let suffix = tlen - vi - 1;
        let mut mid_end = slen;
        if suffix > 0
            && slen >= vi + suffix
            && (slen - suffix..slen).all(|j| fixed(sshape.elements[j].kind))
            && tshape.elements[vi + 1..].iter().all(|e| fixed(e.kind))
        {
            for j in 0..suffix {
                let si = slen - suffix + j;
                self.infer_from(ctx, sargs[si], targs[vi + 1 + j], contra, depth + 1);
            }
            mid_end = slen - suffix;
        }
        let start = vi.min(s_var).min(mid_end);
        if tshape.elements[vi].kind == TupleElement::VARIADIC {
            let mut mid: Vec<crate::declared::TupleEl> = Vec::with_capacity(mid_end - start);
            for j in start..mid_end {
                let a = sargs.get(j).copied().unwrap_or(TypeId::UNRESOLVED);
                let kind = sshape.elements[j].kind;
                // Stored rest elements hold the element type; `create_tuple` takes arrays.
                let ty = if kind == TupleElement::REST {
                    self.create_array(a, false)
                } else {
                    a
                };
                mid.push(crate::declared::TupleEl {
                    ty,
                    kind,
                    label: sshape.elements[j].label,
                });
            }
            let tup = self.create_tuple(&mid, sshape.readonly);
            self.infer_from(ctx, tup, targs[vi], contra, depth + 1);
        } else {
            // A rest target element infers once from the union of the middle's element
            // types (`[1, "a", true]` to `[T, ...U[]]` gives `U` the one candidate
            // `string | boolean`).
            let mut ets = Vec::with_capacity(mid_end.saturating_sub(start));
            for (el, &arg) in sshape.elements[start..mid_end]
                .iter()
                .zip(&sargs[start..mid_end])
            {
                ets.push(self.element_type_of_array_like_arg(el.kind, arg));
            }
            if !ets.is_empty() {
                let et = self.union_with(&ets, Reduction::Literal, AliasId::NONE);
                self.infer_from(ctx, et, targs[vi], contra, depth + 1);
            }
        }
    }

    /// The length of the fixed-length tuple inferred so far for the type parameter `t`.
    fn implied_arity(&self, ctx: &InferenceContext, t: TypeId) -> Option<usize> {
        let j = ctx.params.iter().position(|&p| p == t)?;
        let s = self.store();
        ctx.fixed[j]
            .into_iter()
            .chain(ctx.candidates[j].iter().copied())
            .find_map(|c| {
                let sh = s.tuple_shape_of(c)?;
                (!sh.has_rest()).then_some(sh.elements.len())
            })
    }

    /// Elements `from..to` of a tuple as a tuple of their own.
    fn slice_tuple(
        &mut self,
        sshape: &ts_r_types::TupleShape,
        sargs: &[TypeId],
        from: usize,
        to: usize,
    ) -> TypeId {
        let mut els: Vec<crate::declared::TupleEl> = Vec::with_capacity(to.saturating_sub(from));
        for j in from..to {
            let a = sargs.get(j).copied().unwrap_or(TypeId::UNRESOLVED);
            let kind = sshape.elements[j].kind;
            // Stored rest elements hold the element type; `create_tuple` takes arrays.
            let ty = if kind == TupleElement::REST {
                self.create_array(a, false)
            } else {
                a
            };
            els.push(crate::declared::TupleEl {
                ty,
                kind,
                label: sshape.elements[j].label,
            });
        }
        self.create_tuple(&els, sshape.readonly)
    }

    /// The element type a tuple element contributes (`...T[]` and `...T` give `T`'s element).
    fn element_type_of_array_like_arg(&mut self, kind: u8, arg: TypeId) -> TypeId {
        if kind == TupleElement::VARIADIC {
            return self
                .element_type_of_array_like(arg)
                .unwrap_or(TypeId::UNRESOLVED);
        }
        arg
    }

    /// The parameters of `sig` from position `pos` on, as a tuple (or the rest parameter's
    /// own type when `pos` is the rest position).
    pub(crate) fn rest_type_at(&mut self, sig: SigId, pos: usize) -> TypeId {
        let d = self.store().sig(sig);
        let n = d.params.len();
        let has_rest = d.has_rest();
        if has_rest && pos + 1 == n {
            return d.params[n - 1].ty;
        }
        // Past the rest position of a tuple rest (`(...args: [a: A, b: B])` from 1 is
        // `[b: B]`): the tuple from the matching element on (its open elements stay).
        if has_rest && pos >= n {
            let r = d.params[n - 1].ty;
            if let Some(shape) = self.store().tuple_shape_of(r).cloned() {
                let args = self.c.type_args_of_tuple(r).to_vec();
                let fixed = shape
                    .elements
                    .iter()
                    .take_while(|e| {
                        e.kind == TupleElement::REQUIRED || e.kind == TupleElement::OPTIONAL
                    })
                    .count();
                let from = (pos - (n - 1)).min(fixed);
                return self.slice_tuple(&shape, &args, from, shape.elements.len());
            }
            // Past an array or generic rest: an array of its element (`T[number][]`;
            // tsc: relater.go, getRestTypeAtPosition).
            if let Some(el) = self.indexed_access_type(r, TypeId::NUMBER, None) {
                return self.create_array(el, false);
            }
        }
        let params = d.params.to_vec();
        let min = self.effective_min_args(sig);
        let mut els: Vec<crate::declared::TupleEl> = Vec::new();
        let fixed_end = if has_rest { n - 1 } else { n };
        for (i, p) in params.iter().enumerate().take(fixed_end).skip(pos) {
            els.push(crate::declared::TupleEl {
                ty: if p.optional || i >= min {
                    self.remove_missing(p.ty)
                } else {
                    p.ty
                },
                kind: if p.optional || i >= min {
                    TupleElement::OPTIONAL
                } else {
                    TupleElement::REQUIRED
                },
                label: Some(p.name),
            });
        }
        if has_rest {
            let r = params[n - 1].ty;
            let label = Some(params[n - 1].name);
            if self.c.is_array_type(r) && self.store().tuple_shape_of(r).is_none() {
                els.push(crate::declared::TupleEl {
                    ty: r,
                    kind: TupleElement::REST,
                    label,
                });
            } else {
                els.push(crate::declared::TupleEl {
                    ty: r,
                    kind: TupleElement::VARIADIC,
                    label,
                });
            }
        }
        self.create_tuple(&els, false)
    }

    fn infer_from_signatures(
        &mut self,
        ctx: &mut InferenceContext,
        s: SigId,
        t: SigId,
        contra: bool,
        depth: u32,
    ) {
        // A generic source signature is first instantiated in the context of a
        // non-generic target (`xs.flatMap(<P extends X>(p: P) => Box<P>[])` infers
        // `U = Box<X>`, not `Box<P>`), when the target's parameters fix its type
        // arguments without the type parameters being inferred here (`map(identity, "")`
        // with `(x: S) => T` keeps the old path).
        let s_generic = self.store().sig(s).type_params != ListId::EMPTY;
        let t_generic = self.store().sig(t).type_params != ListId::EMPTY;
        // Against a generic target the source's type parameters are their constraints
        // (`f1(x => x)` with `cb: <S>(x: S) => T` infers `T = unknown`, not `S`); so are
        // the target's in a source that took its parameter types from it (a function
        // expression contextually typed by the generic target).
        let s = if s_generic && (ctx.base_signatures || t_generic) {
            self.base_signature(s)
        } else if t_generic {
            let ttps = self
                .store()
                .list_items(self.store().sig(t).type_params)
                .to_vec();
            let args = self.base_constraints_of(&ttps);
            let m = self.store().mapper(&ttps, &args);
            self.instantiate_signature_full(s, m)
        } else if self.store().sig(s).type_params != ListId::EMPTY
            && self.store().sig(t).type_params == ListId::EMPTY
        {
            let tps = self
                .store()
                .list_items(self.store().sig(s).type_params)
                .to_vec();
            // A type parameter no parameter mentions infers nothing: it is its default (or
            // constraint, else `unknown`) whatever the target (`pair: <T, S>(x: T) => (y: S)
            // => ..` against `(x: number) => ..` has `S = unknown`).
            let free = self.unmentioned_in_params(s, &tps);
            let mut args = self.infer_signature_in_context(s, t);
            self.fill_free_type_args(&tps, &free, &mut args);
            let mut concrete = self.inferred_concretely(ctx, &args);
            if !concrete {
                // The target mentions type parameters being inferred: use what is known
                // about them so far (without fixing).
                let cur: Vec<TypeId> = (0..ctx.params.len())
                    .map(|j| {
                        if let Some(f) = ctx.fixed[j] {
                            f
                        } else if ctx.candidates[j].is_empty() && ctx.contra[j].is_empty() {
                            ctx.params[j]
                        } else {
                            let mut c2 = ctx.clone();
                            self.inferred_type_at(&mut c2, j)
                        }
                    })
                    .collect();
                if cur.iter().zip(&ctx.params).any(|(a, b)| a != b) {
                    let m = self.store().mapper(&ctx.params, &cur);
                    let t2 = self.instantiate_signature_full(t, m);
                    args = self.infer_signature_in_context(s, t2);
                    self.fill_free_type_args(&tps, &free, &mut args);
                    concrete = self.inferred_concretely(ctx, &args);
                }
            }
            if concrete {
                let m = self.store().mapper(&tps, &args);
                self.instantiate_signature_full(s, m)
            } else {
                s
            }
        } else {
            s
        };
        // The type parameters of a source signature that stays generic are not inferred
        // from: a union mentioning one gives its naked target nothing (`T | U` from
        // `"a" | U'` leaves `T` alone), though other positions still infer (`[T, U]`).
        // The generic target's own type parameters are erased to `any`, so a union with
        // one infers nothing (`Set<T | U>` gives `T` nothing from `Set<unknown>`).
        let t = if t_generic {
            let ttps = self
                .store()
                .list_items(self.store().sig(t).type_params)
                .to_vec();
            let anys = vec![TypeId::ANY; ttps.len()];
            let m = self.store().mapper(&ttps, &anys);
            self.instantiate_signature_full(t, m)
        } else {
            t
        };
        let mark = ctx.non_inferrable.len();
        let stps = self.store().sig(s).type_params;
        if stps != ListId::EMPTY {
            let tps = self.store().list_items(stps).to_vec();
            ctx.non_inferrable.extend(tps);
        }
        self.infer_from_signature_parts(ctx, s, t, contra, depth);
        ctx.non_inferrable.truncate(mark);
    }

    /// For each of `tps`, whether no parameter type of `s` mentions it.
    fn unmentioned_in_params(&mut self, s: SigId, tps: &[TypeId]) -> Vec<bool> {
        let mut free = vec![true; tps.len()];
        let n = self.store().sig(s).params.len();
        for i in 0..n {
            let Some(pt) = self.param_type_at(s, i) else {
                continue;
            };
            for (k, &tp) in tps.iter().enumerate() {
                if free[k] && self.type_mentions(pt, tp) {
                    free[k] = false;
                }
            }
        }
        if let Some(this) = self.store().sig(s).this_type {
            for (k, &tp) in tps.iter().enumerate() {
                if free[k] && self.type_mentions(this, tp) {
                    free[k] = false;
                }
            }
        }
        free
    }

    /// Sets the type arguments of the `free` type parameters (see
    /// [`Ck::unmentioned_in_params`]) to their default, else constraint, else `unknown`.
    fn fill_free_type_args(&mut self, tps: &[TypeId], free: &[bool], args: &mut [TypeId]) {
        if !free.contains(&true) {
            return;
        }
        let unknowns = vec![TypeId::UNKNOWN; tps.len()];
        let to_unknown = self.store().mapper(tps, &unknowns);
        for (k, &tp) in tps.iter().enumerate() {
            if !free[k] {
                continue;
            }
            let d = self
                .type_param_default(tp)
                .or_else(|| self.type_param_constraint(tp));
            args[k] = match d {
                Some(d) => self.instantiate(d, to_unknown),
                None => TypeId::UNKNOWN,
            };
        }
    }

    /// `s` with its type parameters replaced by their constraints (`unknown` without one).
    /// Constraints mentioning other type parameters are resolved through theirs (one round
    /// per further type parameter); what still mentions one (a circular constraint, `S
    /// extends Array<S>`) has it as `any`.
    fn base_signature(&mut self, s: SigId) -> SigId {
        let tps = self
            .store()
            .list_items(self.store().sig(s).type_params)
            .to_vec();
        let args = self.base_constraints_of(&tps);
        let m = self.store().mapper(&tps, &args);
        self.instantiate_signature_full(s, m)
    }

    /// The base constraints of `tps` (see [`Ck::base_signature`]).
    fn base_constraints_of(&mut self, tps: &[TypeId]) -> Vec<TypeId> {
        let constraints: Vec<TypeId> = tps
            .iter()
            .map(|&tp| self.type_param_constraint(tp).unwrap_or(TypeId::UNKNOWN))
            .collect();
        let step = self.store().mapper(tps, &constraints);
        let mut args = constraints.clone();
        for _ in 1..tps.len() {
            for a in &mut args {
                *a = self.instantiate(*a, step);
            }
        }
        let anys = vec![TypeId::ANY; tps.len()];
        let to_any = self.store().mapper(tps, &anys);
        for a in &mut args {
            *a = self.instantiate(*a, to_any);
        }
        args
    }

    fn infer_from_signature_parts(
        &mut self,
        ctx: &mut InferenceContext,
        s: SigId,
        t: SigId,
        contra: bool,
        depth: u32,
    ) {
        let saved_bivariant = ctx.bivariant;
        ctx.bivariant |= self.store().sig(t).flags.contains(SigFlags::METHOD);
        // Without strictFunctionTypes parameters infer covariantly.
        let pcontra = if self.c.opts.strict_function_types {
            !contra
        } else {
            contra
        };
        self.infer_from_params(ctx, s, t, pcontra, depth);
        ctx.bivariant = saved_bivariant;
        if let Some(tp) = self.predicate_of(t)
            && let Some(tt) = tp.ty
            && let Some(sp) = self.predicate_of(s)
            && let Some(st) = sp.ty
            && sp.kind == tp.kind
        {
            self.infer_from(ctx, st, tt, contra, depth);
            return;
        }
        let sr = self.return_type_of(s);
        let tr = self.return_type_of(t);
        self.infer_from(ctx, sr, tr, contra, depth);
    }

    /// Infers between the parameters of `s` and `t`; `pcontra` is the parameters' variance.
    pub(crate) fn infer_from_params(
        &mut self,
        ctx: &mut InferenceContext,
        s: SigId,
        t: SigId,
        pcontra: bool,
        depth: u32,
    ) {
        // `this` parameters infer like parameters (`F extends (this: infer T) => void`).
        if let (Some(st), Some(tt)) = (self.store().sig(s).this_type, self.store().sig(t).this_type)
        {
            self.infer_from(ctx, st, tt, pcontra, depth);
        }
        let sn = self.store().sig(s).params.len();
        let tn = self.store().sig(t).params.len();
        let t_rest = self.store().sig(t).has_rest();
        let t_rest_type = self.store().sig(t).params.last().map(|p| p.ty);
        // A rest parameter of a type variable (`...args: P`) or of a tuple whose only
        // non-fixed element is variadic (`...args: [A[0], ...infer R]`) receives the source's
        // remaining parameters as a tuple (not from generic sources: their type parameters
        // would leak).
        if t_rest
            && self.store().sig(s).type_params == ListId::EMPTY
            && let Some(rt) = t_rest_type
            && (self.store().flags(rt).contains(TypeFlags::TYPE_PARAMETER)
                || self.store().tuple_shape_of(rt).is_some_and(|sh| {
                    let open: Vec<u8> = sh
                        .elements
                        .iter()
                        .map(|e| e.kind)
                        .filter(|&k| k == TupleElement::VARIADIC || k == TupleElement::REST)
                        .collect();
                    open == [TupleElement::VARIADIC]
                        || (open == [TupleElement::VARIADIC, TupleElement::VARIADIC]
                            && sh.elements.len() >= 2
                            && sh.elements[sh.elements.len() - 2].kind == TupleElement::VARIADIC)
                }))
        {
            let non_rest = tn - 1;
            let count = if self.store().sig(s).has_rest() {
                non_rest
            } else {
                sn.min(non_rest)
            };
            for i in 0..count {
                if let (Some(sp), Some(tp)) = (self.param_type_at(s, i), self.param_type_at(t, i)) {
                    self.infer_from(ctx, sp, tp, pcontra, depth);
                }
            }
            let rest = self.rest_type_at(s, count);
            self.infer_from(ctx, rest, rt, pcontra, depth);
        } else {
            let n = sn.min(tn).max(if self.store().sig(s).has_rest() {
                tn
            } else {
                0
            });
            for i in 0..n {
                if let (Some(sp), Some(tp)) = (self.param_type_at(s, i), self.param_type_at(t, i)) {
                    self.infer_from(ctx, sp, tp, pcontra, depth);
                }
            }
        }
    }

    /// `t` is one of `tps`, or a union or intersection with one among its members.
    fn mentions_at_top(&self, t: TypeId, tps: &[TypeId], depth: u32) -> bool {
        if tps.contains(&t) {
            return true;
        }
        let s = self.store();
        if depth > 4 || !s.flags(t).intersects(TypeFlags::UNION_OR_INTERSECTION) {
            return false;
        }
        s.members_of_union(t)
            .iter()
            .any(|&m| self.mentions_at_top(m, tps, depth + 1))
    }

    /// Type arguments inferred for a generic source signature are all known: not `any`
    /// (nothing inferred) and free of the type parameters being inferred in `ctx`.
    fn inferred_concretely(&mut self, ctx: &InferenceContext, args: &[TypeId]) -> bool {
        args.iter()
            .all(|&a| a != TypeId::ANY && !ctx.params.iter().any(|&p| self.type_mentions(a, p)))
    }

    /// Whether a source union member is matched by a target member for inference purposes
    /// (identical, or both references to the same generic type).
    fn closely_matches(&self, s: TypeId, t: TypeId) -> bool {
        // An optional property's `missing` is matched by `undefined`.
        if s == t || (s == TypeId::MISSING && t == TypeId::UNDEFINED) {
            return true;
        }
        let st = self.store();
        if st.flags(t).intersects(TypeFlags::UNIT) && st.regular(s) == st.regular(t) {
            return true;
        }
        // Instantiations of the same generic alias.
        if let (Some(a), Some(b)) = (st.alias_of(s), st.alias_of(t))
            && a.sym == b.sym
            && a.args != ListId::EMPTY
        {
            return true;
        }
        match (
            st.object_of(s).map(|o| o.kind),
            st.object_of(t).map(|o| o.kind),
        ) {
            (
                Some(ObjectKind::Reference { target: a, .. }),
                Some(ObjectKind::Reference { target: b, .. }),
            ) => a == b,
            _ => false,
        }
    }

    /// Whether `tp` is `t`, a union member of `t`, or (recursively) a branch of a
    /// conditional `t` (`T extends string ? T : number` keeps a literal `T`).
    fn is_type_param_at_top_level(&mut self, t: TypeId, tp: TypeId, depth: u32) -> bool {
        if t == tp || self.some_member(t, |m| m == tp) {
            return true;
        }
        if depth > 8 {
            return false;
        }
        let s = self.store();
        // Members of an intersection are at the top level too (`NonNullable<T>` is `T & {}`).
        if s.flags(t).contains(TypeFlags::INTERSECTION) {
            let ms = s.members_of_union(t).to_vec();
            return ms
                .into_iter()
                .any(|m| self.is_type_param_at_top_level(m, tp, depth + 1));
        }
        let Some(d) = s.conditional_data(t) else {
            return false;
        };
        let root = *s.conditional_root_data(d.root);
        for branch in [true, false] {
            let b = self.conditional_branch(root.decl, branch);
            let b = self.instantiate(b, d.mapper);
            if self.is_type_param_at_top_level(b, tp, depth + 1) {
                return true;
            }
        }
        false
    }

    /// The contravariant inference: the most specific candidate.
    fn contra_inference(&mut self, cs: &[TypeId]) -> TypeId {
        let mut best = cs[0];
        for &c in &cs[1..] {
            if self.is_subtype(c, best) {
                best = c;
            }
        }
        best
    }

    /// Every candidate of parameter `i`, and of the other type parameters whose constraint
    /// is parameter `i` itself, is assignable to `w`, `i`'s covariant inference.
    fn covers_constrained_candidates(
        &mut self,
        ctx: &InferenceContext,
        i: usize,
        w: TypeId,
    ) -> bool {
        let tp = ctx.params[i];
        for j in 0..ctx.params.len() {
            if j != i && self.type_param_constraint(ctx.params[j]) != Some(tp) {
                continue;
            }
            let cands: Vec<TypeId> = ctx.preferred[j]
                .iter()
                .chain(&ctx.candidates[j])
                .copied()
                .collect();
            if !cands.iter().all(|&c| self.is_assignable(c, w)) {
                return false;
            }
        }
        true
    }

    /// The inferred type of type parameter `i`.
    pub fn inferred_type_at(&mut self, ctx: &mut InferenceContext, i: usize) -> TypeId {
        if let Some(t) = ctx.fixed[i] {
            return t;
        }
        // The other variance's inference, tried when the chosen one breaks the constraint.
        let mut fallback = None;
        let tp = ctx.params[i];
        // Preferred candidates go first, so the common supertype falls back to them.
        let mut cands = ctx.preferred[i].clone();
        for &c in &ctx.candidates[i] {
            if !cands.contains(&c) {
                cands.push(c);
            }
        }
        if cands.is_empty() && ctx.contra[i].is_empty() {
            cands = ctx.weak[i].clone();
        }
        let mut inferred = if !cands.is_empty() {
            let constraint = self.type_param_constraint(tp);
            let primitive_constraint = constraint.is_some_and(|c| {
                self.some_member(c, |m| {
                    self.store().flags(m).intersects(
                        TypeFlags::PRIMITIVE | TypeFlags::INDEX | TypeFlags::TEMPLATE_LITERAL,
                    )
                })
            });
            let at_top_in_return = ctx
                .ret
                .is_some_and(|r| self.is_type_param_at_top_level(r, tp, 0));
            let widen = !primitive_constraint
                && ctx.top_level[i]
                && !ctx.nested[i]
                && (ctx.fixing || !at_top_in_return);
            let base: Vec<TypeId> = cands
                .iter()
                .map(|&c| {
                    if primitive_constraint {
                        self.regular_of_literal(c)
                    } else if widen {
                        self.widen_literal(c)
                    } else {
                        c
                    }
                })
                .collect();
            let t = if ctx.combine[i] {
                self.union(&base)
            } else {
                // Object literal candidates combine into one union candidate after the
                // others (`pair({ x: 'a' }, { x: 'c' })` infers `{ x: "a"; } | { x: "c"; }`,
                // `pair({ x: 1 }, oy)` infers `oy`'s type).
                let is_lit = |ck: &Self, t: TypeId| {
                    ck.store()
                        .object_of(t)
                        .is_some_and(|o| matches!(o.kind, ObjectKind::Literal { .. }))
                        || ck.const_array_literals.contains(&t)
                };
                let (lits, mut rest): (Vec<TypeId>, Vec<TypeId>) =
                    base.iter().partition(|&&t| is_lit(self, t));
                if lits.len() > 1 || (!lits.is_empty() && !rest.is_empty()) {
                    let u = self.union(&lits);
                    rest.push(u);
                    self.common_supertype(&rest)
                } else {
                    self.common_supertype(&base)
                }
            };
            let w = self.widened_type(t);
            // A covariant inference that fits none of the contravariant ones gives way to
            // them (`call((x: number) => x, "bad")` infers `A = [x: number]` from the
            // callback, then reports the argument).
            // (Contravariant candidates that mention this call's own type parameters, from
            // callbacks typed before they were known, do not count.)
            let params = ctx.params.clone();
            let mut contra = Vec::new();
            for &c in &ctx.contra[i] {
                if !params.iter().any(|&p| self.type_mentions(c, p)) {
                    contra.push(c);
                }
            }
            // The covariant inference also gives way when one of its own candidates, or
            // one of another type parameter constrained to this one, is not assignable
            // to it (`every<T, U extends T>(a: readonly T[], f: (x: T) => x is U)` with
            // `T`'s candidates `Decorator` and `Modifier`, or `T`'s `Decorator` and
            // `U`'s `Modifier`, infers `T` from the callback), and when it is `any`
            // (tsc: `getInferredType` in `tsc/internal/checker/inference.go`).
            if !ctx.candidates[i].is_empty()
                && !contra.is_empty()
                && (w == TypeId::NEVER
                    || self.store().flags(w).contains(TypeFlags::ANY)
                    || !contra.iter().any(|&c| self.is_assignable(w, c))
                    || !self.covers_constrained_candidates(ctx, i, w))
            {
                fallback = Some(w);
                self.contra_inference(&contra)
            } else {
                if !contra.is_empty() {
                    fallback = Some(self.contra_inference(&contra));
                }
                w
            }
        } else if !ctx.contra[i].is_empty() {
            let cs = ctx.contra[i].clone();
            self.contra_inference(&cs)
        } else if !ctx.literal_keyof[i].is_empty() {
            let ks = ctx.literal_keyof[i].clone();
            self.intersection(&ks)
        } else {
            match self.type_param_default(tp) {
                Some(d) => {
                    let m = self.inference_mapper(ctx, i);
                    self.instantiate(d, m)
                }
                None => TypeId::UNKNOWN,
            }
        };
        if let Some(c) = self.type_param_constraint(tp) {
            // Other parameters' defaults see this inference (`TAction = TConfig["entry"]`
            // in `TConfig extends StateConfig<TAction>`), and so does the constraint's own
            // reference to the parameter (`C extends X & (number extends C["length"] ? A :
            // B)` is checked with `C` as inferred).
            ctx.fixed[i] = Some(inferred);
            let m = self.inference_mapper(ctx, usize::MAX);
            ctx.fixed[i] = None;
            let ic = self.instantiate(c, m);
            if !self.is_assignable(inferred, ic) {
                let mut kept = Vec::new();
                if ctx.filter_by_constraint
                    && self.store().flags(inferred).contains(TypeFlags::UNION)
                {
                    for m in self.store().members_of_union(inferred).to_vec() {
                        if self.is_assignable(m, ic) {
                            kept.push(m);
                        }
                    }
                }
                inferred = if !kept.is_empty() {
                    self.union(&kept)
                } else if let Some(f) = fallback.filter(|&f| self.is_assignable(f, ic)) {
                    f
                } else {
                    ic
                };
            }
        }
        ctx.fixed[i] = Some(inferred);
        inferred
    }

    /// A mapper from the inference's type parameters to the types fixed so far, excluding
    /// parameter `skip`. Unfixed parameters with inferences map to what they would infer
    /// now without fixing (`O extends RO<A>` sees `A`'s candidates); others to `unknown`.
    pub(crate) fn inference_mapper(
        &mut self,
        ctx: &InferenceContext,
        skip: usize,
    ) -> ts_r_types::MapperId {
        let mut src = Vec::new();
        let mut dst = Vec::new();
        for (j, &p) in ctx.params.iter().enumerate() {
            if j == skip {
                continue;
            }
            src.push(p);
            let t = match ctx.fixed[j] {
                Some(t) => t,
                None if !ctx.candidates[j].is_empty() || !ctx.contra[j].is_empty() => {
                    // One level only: everything else is seen as fixed (`unknown`).
                    let mut c2 = ctx.clone();
                    for k in 0..c2.params.len() {
                        if k != j && c2.fixed[k].is_none() {
                            c2.fixed[k] = Some(TypeId::UNKNOWN);
                        }
                    }
                    self.inferred_type_at(&mut c2, j)
                }
                // A parameter with nothing inferred has its default, over what is fixed.
                None => match self.type_param_default(p) {
                    Some(d) if ctx.fixed.iter().any(|f| f.is_some()) => {
                        let fixed: Vec<TypeId> = ctx
                            .fixed
                            .iter()
                            .map(|f| f.unwrap_or(TypeId::UNKNOWN))
                            .collect();
                        let m = self.store().mapper(&ctx.params, &fixed);
                        self.instantiate(d, m)
                    }
                    _ => TypeId::UNKNOWN,
                },
            };
            dst.push(t);
        }
        self.store().mapper(&src, &dst)
    }

    /// tsc's `getCommonSupertype`. Under strictNullChecks each candidate is taken without
    /// its `null` and `undefined` members (a wholly nullable one becomes `never`), which
    /// come back on the result. Literals of one base type (`never` aside) form a union;
    /// otherwise the leftmost candidate no later one is a strict supertype of wins when
    /// every other candidate is its strict subtype, else the leftmost no later one is a
    /// supertype of (`equal(b as B, d as D | undefined)` infers `B | undefined`).
    pub fn common_supertype(&mut self, types: &[TypeId]) -> TypeId {
        if types.len() == 1 {
            return types[0];
        }
        let mut nullable = Vec::new();
        let primary: Vec<TypeId> = if self.c.opts.strict_null_checks {
            types
                .iter()
                .map(|&t| {
                    for m in [TypeId::UNDEFINED, TypeId::NULL] {
                        if !nullable.contains(&m) && self.some_member(t, |x| x == m) {
                            nullable.push(m);
                        }
                    }
                    self.filter_type(t, |ck, u| {
                        !ck.store().flags(u).intersects(TypeFlags::NULLABLE)
                    })
                })
                .collect()
        } else {
            types.to_vec()
        };
        let mut common_base = None;
        let mut same_base_literals = true;
        for &t in &primary {
            if t == TypeId::NEVER {
                continue;
            }
            let b = self.base_of_literal(t);
            let cb = *common_base.get_or_insert(b);
            if b == t || b != cb {
                same_base_literals = false;
                break;
            }
        }
        let sup = if same_base_literals {
            self.union(&primary)
        } else {
            let strict = self.leftmost_supertype(&primary, true);
            if primary
                .iter()
                .all(|&t| t == strict || self.is_strict_subtype(t, strict))
            {
                strict
            } else {
                self.leftmost_supertype(&primary, false)
            }
        };
        if nullable.is_empty() {
            return sup;
        }
        nullable.insert(0, sup);
        self.union(&nullable)
    }

    /// The leftmost of `types` for which no type to the right is a (strict) supertype.
    fn leftmost_supertype(&mut self, types: &[TypeId], strict: bool) -> TypeId {
        let mut best = types[0];
        for &t in &types[1..] {
            let sub = if strict {
                self.is_strict_subtype(best, t)
            } else {
                self.is_subtype(best, t)
            };
            if sub {
                best = t;
            }
        }
        best
    }

    /// Inferred type arguments for `sig` given argument types (tsc-style two passes are
    /// done by the caller; this runs one pass over `(arg type, param index)` pairs).
    pub fn inference_context_for(&mut self, sig: SigId) -> InferenceContext {
        let d = self.store().sig(sig);
        let tps = if d.type_params == ListId::EMPTY {
            Vec::new()
        } else {
            self.store().list_items(d.type_params).to_vec()
        };
        let mut ctx = InferenceContext::new(&tps);
        let mut r = self.declared_return_of(sig);
        // An inferred return type not resolved yet is resolved now, unless it is being
        // resolved (a recursive call): the return context infers through it too.
        if r.is_none()
            && !tps.is_empty()
            && !self.is_resolving(crate::checker::Resolving::Return(sig))
        {
            r = Some(self.return_type_of(sig));
        }
        ctx.ret = r;
        ctx
    }

    /// The declared return type of a signature if it has an annotation.
    pub fn declared_return_of(&mut self, sig: SigId) -> Option<TypeId> {
        let d = self.store().sig(sig);
        if d.flags.contains(ts_r_types::SigFlags::HAS_RETURN_TYPE)
            || d.decl.is_none()
            || d.ret().is_some()
        {
            return Some(self.return_type_of(sig));
        }
        None
    }
}
