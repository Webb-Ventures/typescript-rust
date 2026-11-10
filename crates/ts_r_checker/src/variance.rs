//! Variance of the type parameters of generic classes, interfaces and type aliases.
//!
//! Two references to the same generic type (`Box<number>` and `Box<unknown>`) relate by
//! their type arguments, each compared in the direction its type parameter's variance
//! allows. A written `in`, `out` or `in out` sets the variance. Otherwise it is measured
//! by relating two instantiations that differ only in that parameter: one with a "sub"
//! marker type parameter and one with a "super" marker (the sub marker's constraint).
//! Sub-to-super related means covariant, super-to-sub related contravariant, both
//! bivariant (independent when an unrelated third marker relates too), neither invariant.
//!
//! While a type's variances are measured, references to it are "maybe related" (so
//! recursive types measure). A measurement is a pure function of the declaration: the
//! variances it needs of other types are measured inside it and kept local, and its
//! relation results go to a cache of its own rather than the shared one, so the result
//! does not depend on which file or thread asked first.
//!
//! Each parameter's measurement also records what it met that markers can't stand for
//! (tsc 7's reliability flags, `tsc/internal/checker/relater.go`): a source mapped type
//! that removes `?` with a marker in its keys is *unmeasurable* (arguments relate only
//! when identical); other mapped type keys, template literal sources, non-array rest
//! parameters and arguments of unreliable parameters mentioning a marker, and a spent
//! budget, are *unreliable*. The flags of a comparison are cached with its result, so a
//! cache hit notes them again. When any parameter has a flag, arguments the variances
//! reject fall back to structure.

use std::sync::Arc;

use rustc_hash::FxHashMap;
use ts_r_ast::{Modifiers, Node, NodeId, NodeKind};
use ts_r_diagnostics::FileId;
use ts_r_types::{ObjectKind, RelationKind, SymId, TypeFlags, TypeId, TypeTraits};

use crate::checker::{Ck, Loc};
use crate::diag::dg;

/// The variance of one type parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Variance {
    /// The argument does not matter.
    Independent,
    Covariant,
    Contravariant,
    Bivariant,
    Invariant,
}

/// What variances are measured for: a generic class or interface (its declared type) or a
/// generic type alias.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum VarianceKey {
    Type(TypeId),
    Alias(SymId),
}

/// The measured variances of a generic type's type parameters.
#[derive(Debug)]
pub(crate) struct Variances {
    pub list: Box<[Variance]>,
    /// Per type parameter: [`UNMEASURABLE`] and [`UNRELIABLE`], what the measurement met
    /// that markers can't stand for (see the module docs). Written variances have none.
    pub flags: Box<[u8]>,
}

/// A relation whose outcome doesn't follow from the arguments' (a source mapped type
/// that removes `?`): arguments relate only when identical, else by structure.
pub(crate) const UNMEASURABLE: u8 = 1;
/// A relation that may hold where the measured variance says it doesn't (a mapped
/// type's keys, template literals, generic rest parameters): arguments the variances
/// reject are compared by structure.
pub(crate) const UNRELIABLE: u8 = 2;

impl Variances {
    /// Some parameter's measurement was unmeasurable or unreliable: arguments the
    /// variances reject fall back to structure.
    pub(crate) fn allows_structural_fallback(&self) -> bool {
        self.flags.iter().any(|&f| f != 0)
    }
}

/// Structural comparisons one relation of a measurement may make.
const MEASURE_WORK: u32 = 20_000;

/// A variance annotation check in progress (`check_variance_annotation`).
#[derive(Debug)]
pub(crate) struct VarianceCheck {
    pub sub: TypeId,
    pub sup: TypeId,
    /// The annotated type parameter (the markers print `sub-T` and `super-T`).
    pub param: SymId,
    /// The two instantiations compared (structurally, like measurement markers).
    pub markers: [TypeId; 2],
}

/// The state of the outermost measurement in progress.
#[derive(Default, Debug)]
pub(crate) struct Probe {
    /// Relation results of the measurement, with the [`UNMEASURABLE`] / [`UNRELIABLE`]
    /// flags their comparison noted (a later hit notes them again).
    pub cache: FxHashMap<(TypeId, TypeId, RelationKind), (bool, u8)>,
    /// Variances measured inside it (`None`: being measured).
    pub variances: FxHashMap<VarianceKey, Option<Arc<Variances>>>,
    /// What the running comparison of the innermost measurement noted.
    pub flags: u8,
    /// The marker instantiations being compared (compared structurally).
    pub markers: rustc_hash::FxHashSet<TypeId>,
}

impl<'c, 'a> Ck<'c, 'a> {
    /// The marker type parameters: `(sub, super, unrelated)`.
    fn variance_markers(&mut self) -> (TypeId, TypeId, TypeId) {
        let st = self.store();
        let m_super = st.mapper(&[TypeId::UNKNOWN], &[TypeId::NEVER]);
        let m_sub = st.mapper(&[TypeId::UNKNOWN], &[TypeId::UNDEFINED]);
        let m_other = st.mapper(&[TypeId::UNKNOWN], &[TypeId::NULL]);
        let sup = st.cloned_type_param(TypeId::UNKNOWN, m_super);
        let sub = st.cloned_type_param(TypeId::UNKNOWN, m_sub);
        let other = st.cloned_type_param(TypeId::UNKNOWN, m_other);
        if let Some(d) = st.type_param_data(sup) {
            d.set_constraint(None);
        }
        if let Some(d) = st.type_param_data(other) {
            d.set_constraint(None);
        }
        if let Some(d) = st.type_param_data(sub) {
            d.set_constraint(Some(sup));
        }
        (sub, sup, other)
    }

    /// Notes, while measuring, `flag` ([`UNMEASURABLE`] or [`UNRELIABLE`]) when `x`
    /// mentions a marker (tsc: instantiating with `reportUnreliableMapper` /
    /// `reportUnmeasurableMapper`).
    pub(crate) fn note_markers(&mut self, x: TypeId, flag: u8) {
        if self.rel.probe.as_ref().is_none_or(|p| p.flags & flag != 0) {
            return;
        }
        if !self
            .store()
            .traits(x)
            .contains(TypeTraits::CONTAINS_TYPE_PARAMETER)
        {
            return;
        }
        let (sub, sup, other) = self.variance_markers();
        if (self.type_mentions(x, sub) || self.type_mentions(x, sup) || self.type_mentions(x, other))
            && let Some(p) = self.rel.probe.as_mut()
        {
            p.flags |= flag;
        }
    }

    /// Notes, while measuring, a signature whose rest parameter is a union of tuples that
    /// mentions a marker (`next(...[value]: [] | [N])`): such a rest type is not an array,
    /// so its comparison is unreliable (tsc: `getNonArrayRestType` in
    /// `compareSignaturesRelated`).
    pub(crate) fn note_unmeasurable_rest(&mut self, sig: ts_r_types::SigId) {
        if self.rel.probe.as_ref().is_none_or(|p| p.flags & UNRELIABLE != 0) {
            return;
        }
        let st = self.store();
        let d = st.sig(sig);
        if !d.has_rest() {
            return;
        }
        let Some(rt) = d.params.last().map(|p| p.ty) else {
            return;
        };
        if !st.flags(rt).contains(TypeFlags::UNION)
            || !st
                .members_of_union(rt)
                .iter()
                .all(|&m| st.tuple_shape_of(m).is_some())
        {
            return;
        }
        self.note_markers(rt, UNRELIABLE);
    }

    /// Notes, while measuring, a generic rest parameter type (`...args: T`) related as a
    /// whole that mentions a marker: such a comparison is unreliable, references fall back
    /// to structure (tsc: `reportUnreliableMarkers` on `getNonArrayRestType`).
    pub(crate) fn note_unreliable_rest_type(&mut self, rt: TypeId) {
        self.note_markers(rt, UNRELIABLE);
    }

    /// Whether `t` is a marker instantiation of a measurement in progress.
    pub(crate) fn is_marker_reference(&self, t: TypeId) -> bool {
        if self
            .variance_check
            .as_ref()
            .is_some_and(|v| v.markers.contains(&t))
        {
            return true;
        }
        self.rel
            .probe
            .as_ref()
            .is_some_and(|p| p.markers.contains(&t))
    }

    /// Whether `target`'s variances are being measured (an enclosing measurement relates
    /// its references as "maybe related").
    pub(crate) fn variance_in_progress(&self, target: VarianceKey) -> bool {
        self.rel
            .probe
            .as_ref()
            .is_some_and(|p| matches!(p.variances.get(&target), Some(None)))
    }

    /// The variances of a generic class, interface or alias's type parameters (`None` for
    /// a type without type parameters or while its own measurement runs).
    pub(crate) fn variances_of(&mut self, target: VarianceKey) -> Option<Arc<Variances>> {
        if let Some(p) = self.rel.probe.as_ref() {
            if let Some(v) = p.variances.get(&target) {
                return v.clone();
            }
        } else if let Some(v) = self.c.cached.variances.lock().ok()?.get(&target) {
            return Some(v.clone());
        }
        let (tps, alias_type) = match target {
            VarianceKey::Type(t) => match self.store().object_of(t).map(|o| o.kind) {
                Some(ObjectKind::Declared { type_params, .. }) => {
                    (self.store().list_items(type_params).to_vec(), None)
                }
                _ => return None,
            },
            VarianceKey::Alias(sym) => {
                let d = self.declared_type_of_symbol(sym);
                (self.local_type_params(sym), Some(d))
            }
        };
        if tps.is_empty() {
            return None;
        }
        let outermost = self.rel.probe.is_none();
        let probe = self.rel.probe.get_or_insert_with(Box::default);
        probe.variances.insert(target, None);
        let saved_flags = std::mem::take(&mut probe.flags);
        let (sub, sup, other) = self.variance_markers();
        // A clean relation state for the measurement.
        let saved_stack = std::mem::take(&mut self.rel.stack);
        let saved_maybe = std::mem::take(&mut self.rel.maybe_used);
        let saved_assumed = std::mem::take(&mut self.rel.assumed);
        let saved_pending = std::mem::take(&mut self.rel.pending);
        let saved_pending_set = std::mem::take(&mut self.rel.pending_set);
        let saved_report = self.rel.report.take();
        let saved_budget = self.rel.budget.take();
        let saved_excluded = std::mem::take(&mut self.rel.excluded_props);
        let saved_skip = std::mem::take(&mut self.rel.skip_excess);
        let mut list = Vec::with_capacity(tps.len());
        let mut flags = Vec::with_capacity(tps.len());
        for i in 0..tps.len() {
            // `out`, `in` and `in out` annotations replace the measurement.
            let written = self.variance_modifiers(tps[i]);
            if !written.is_empty() {
                list.push(
                    match (
                        written.contains(Modifiers::IN),
                        written.contains(Modifiers::OUT),
                    ) {
                        (true, true) => Variance::Invariant,
                        (true, false) => Variance::Contravariant,
                        _ => Variance::Covariant,
                    },
                );
                flags.push(0);
                continue;
            }
            let with = |ck: &mut Self, m: TypeId| {
                let mut args = tps.clone();
                args[i] = m;
                match (target, alias_type) {
                    (_, Some(d)) => {
                        let mapper = ck.store().mapper(&tps, &args);
                        ck.instantiate(d, mapper)
                    }
                    (VarianceKey::Type(t), None) => ck.store().reference(t, &args),
                    (VarianceKey::Alias(_), None) => TypeId::UNKNOWN,
                }
            };
            let t_sub = with(self, sub);
            let t_sup = with(self, sup);
            let t_other = with(self, other);
            if let Some(p) = self.rel.probe.as_mut() {
                p.markers.extend([t_sub, t_sup, t_other]);
                p.flags = 0;
            }
            let mut spent = false;
            let mut related = |ck: &mut Self, a: TypeId, b: TypeId| {
                ck.rel.budget = Some(MEASURE_WORK);
                let r = ck.is_type_related(a, b, RelationKind::Assignable);
                if ck.rel.budget == Some(0) {
                    spent = true;
                }
                ck.rel.budget = None;
                r
            };
            let co = related(self, t_sub, t_sup);
            let contra = related(self, t_sup, t_sub);
            let v = match (co, contra) {
                (true, true) => {
                    if related(self, t_other, t_sup) {
                        Variance::Independent
                    } else {
                        Variance::Bivariant
                    }
                }
                (true, false) => Variance::Covariant,
                (false, true) => Variance::Contravariant,
                (false, false) => Variance::Invariant,
            };
            list.push(v);
            let noted = self.rel.probe.as_ref().map_or(0, |p| p.flags);
            flags.push(noted | if spent { UNRELIABLE } else { 0 });
        }
        self.rel.stack = saved_stack;
        self.rel.maybe_used = saved_maybe;
        self.rel.assumed = saved_assumed;
        self.rel.pending = saved_pending;
        self.rel.pending_set = saved_pending_set;
        self.rel.report = saved_report;
        self.rel.budget = saved_budget;
        self.rel.excluded_props = saved_excluded;
        self.rel.skip_excess = saved_skip;
        let probe = self.rel.probe.as_mut()?;
        probe.flags = saved_flags;
        let v = Arc::new(Variances {
            list: list.into_boxed_slice(),
            flags: flags.into_boxed_slice(),
        });
        probe.variances.insert(target, Some(v.clone()));
        if outermost {
            self.rel.probe = None;
            if let Ok(mut m) = self.c.cached.variances.lock() {
                m.entry(target).or_insert_with(|| v.clone());
            }
        }
        Some(v)
    }

    /// The `in` and `out` modifiers written on a type parameter's declarations.
    pub(crate) fn variance_modifiers(&mut self, tp: TypeId) -> Modifiers {
        let Some(d) = self.store().type_param_data(tp) else {
            return Modifiers::NONE;
        };
        if d.is_this || d.origin.is_some() || d.sym.is_none() {
            return Modifiers::NONE;
        }
        let sym = d.sym;
        let mut m = Modifiers::NONE;
        let add = |m: &mut Modifiers, modifiers: Modifiers| {
            for f in [Modifiers::IN, Modifiers::OUT] {
                if modifiers.contains(f) {
                    m.insert(f);
                }
            }
        };
        let decls = self.c.decls(sym);
        for l in &decls {
            if let Node::TypeParam { modifiers, .. } = *self.ast(l.file).node(l.node) {
                add(&mut m, modifiers);
            }
        }
        // The same-named parameter of the other declarations of a merged interface or class
        // (`interface Baz<out T> {}` with `interface Baz<in T> {}` is invariant).
        let Some(&first) = decls.first() else {
            return m;
        };
        let parents = self.c.file(first.file).parents();
        let Some(owner) = parents
            .parent(first.node)
            .and_then(|list| parents.parent(list))
        else {
            return m;
        };
        let Some(owner_sym) = self.c.sym_of_decl(Loc::new(first.file, owner)) else {
            return m;
        };
        let name = self.c.sym_name(sym).to_owned();
        for o in self.c.decls(owner_sym) {
            let ast = self.ast(o.file);
            let tparams = match *ast.node(o.node) {
                Node::Interface { type_params, .. }
                | Node::ClassDecl { type_params, .. }
                | Node::ClassExpr { type_params, .. } => type_params,
                _ => None,
            };
            let Some(Node::TypeParams { params }) = tparams.map(|t| *ast.node(t)) else {
                continue;
            };
            for &p in ast.list(params) {
                if let Node::TypeParam {
                    modifiers,
                    name: pn,
                    ..
                } = *ast.node(p)
                    && crate::nodes::ident_text(ast, pn) == Some(name.as_str())
                {
                    add(&mut m, modifiers);
                }
            }
        }
        m
    }

    /// The marker type parameters of a variance annotation check: `(sub, super)`, printed
    /// `sub-T` and `super-T` while the check runs.
    fn variance_check_markers(&mut self) -> (TypeId, TypeId) {
        let st = self.store();
        let m_super = st.mapper(&[TypeId::UNKNOWN], &[TypeId::VOID]);
        let m_sub = st.mapper(&[TypeId::UNKNOWN], &[TypeId::ANY]);
        let sup = st.cloned_type_param(TypeId::UNKNOWN, m_super);
        let sub = st.cloned_type_param(TypeId::UNKNOWN, m_sub);
        if let Some(d) = st.type_param_data(sup) {
            d.set_constraint(None);
        }
        if let Some(d) = st.type_param_data(sub) {
            d.set_constraint(Some(sup));
        }
        (sub, sup)
    }

    /// The name a variance annotation check marker prints as (`sub-T`, `super-T`).
    pub(crate) fn variance_check_marker_name(&self, t: TypeId) -> Option<String> {
        let vc = self.variance_check.as_ref()?;
        let prefix = if t == vc.sub {
            "sub-"
        } else if t == vc.sup {
            "super-"
        } else {
            return None;
        };
        Some(format!("{prefix}{}", self.c.sym_name(vc.param)))
    }

    /// `declared` (a generic class or interface, or an alias's type) with the type
    /// parameter `tp` of `tps` replaced by `m`.
    fn instantiate_one_param(
        &mut self,
        declared: TypeId,
        alias: bool,
        tps: &[TypeId],
        tp: TypeId,
        m: TypeId,
    ) -> TypeId {
        let args: Vec<TypeId> = tps.iter().map(|&x| if x == tp { m } else { x }).collect();
        if alias {
            let mapper = self.store().mapper(tps, &args);
            self.instantiate(declared, mapper)
        } else {
            self.store().reference(declared, &args)
        }
    }

    /// Variance annotations on a type parameter of a class, interface or type alias. Only
    /// aliases of object, function, constructor and mapped types take them (TS2637); a lone
    /// `out` (`in`) must agree with the type's structure: the instantiation with a sub
    /// marker must be assignable to the one with a super marker (the reverse for `in`),
    /// else TS2636, elaborated structurally with the markers printed `sub-T` / `super-T`.
    pub(crate) fn check_variance_annotation(&mut self, file: FileId, tp: NodeId) {
        if !self.reporting(file) {
            return;
        }
        let ast = self.ast(file);
        let Node::TypeParam { modifiers, .. } = *ast.node(tp) else {
            return;
        };
        if !modifiers.intersects(Modifiers::IN.union(Modifiers::OUT)) {
            return;
        }
        let parents = self.c.file(file).parents();
        let Some(owner) = parents.parent(tp).and_then(|list| {
            if ast.kind(list) == NodeKind::TypeParams {
                parents.parent(list)
            } else {
                Some(list)
            }
        }) else {
            return;
        };
        let is_alias = match ast.node(owner) {
            Node::TypeAlias { .. } => true,
            Node::Interface { .. } | Node::ClassDecl { .. } | Node::ClassExpr { .. } => false,
            _ => return,
        };
        let l = Loc::new(file, tp);
        let Some(tp_sym) = self.c.sym_of_decl(l) else {
            return;
        };
        let tpt = self.declared_type_of_symbol(tp_sym);
        if self.store().type_param_data(tpt).is_none() {
            return;
        }
        let written = self.variance_modifiers(tpt);
        if written.is_empty() {
            return;
        }
        let Some(owner_sym) = self.c.sym_of_decl(Loc::new(file, owner)) else {
            return;
        };
        let declared = self.declared_type_of_symbol(owner_sym);
        let tps = if is_alias {
            let ok = matches!(
                self.store().object_of(declared).map(|o| o.kind),
                Some(ObjectKind::Anonymous { .. } | ObjectKind::Mapped { .. })
            );
            if !ok {
                self.error(
                    l,
                    dg::VARIANCE_ANNOTATIONS_ARE_ONLY_SUPPORTED_IN_TYPE_ALIASES_FOR_OBJECT_FUNCTION_CONS_2637,
                    &[],
                );
                return;
            }
            self.local_type_params(owner_sym)
        } else {
            match self.store().object_of(declared).map(|o| o.kind) {
                Some(ObjectKind::Declared { type_params, .. }) => {
                    self.store().list_items(type_params).to_vec()
                }
                _ => return,
            }
        };
        if !tps.contains(&tpt) || written.contains(Modifiers::IN.union(Modifiers::OUT)) {
            return;
        }
        let (sub, sup) = self.variance_check_markers();
        let with_sub = self.instantiate_one_param(declared, is_alias, &tps, tpt, sub);
        let with_sup = self.instantiate_one_param(declared, is_alias, &tps, tpt, sup);
        let (s, t) = if written.contains(Modifiers::OUT) {
            (with_sub, with_sup)
        } else {
            (with_sup, with_sub)
        };
        let saved = self.variance_check.replace(VarianceCheck {
            sub,
            sup,
            param: tp_sym,
            markers: [s, t],
        });
        self.check_type_related(
            s,
            t,
            RelationKind::Assignable,
            Some(l),
            Some(crate::relate::Head {
                info: dg::TYPE_0_IS_NOT_ASSIGNABLE_TO_TYPE_1_AS_IMPLIED_BY_VARIANCE_ANNOTATION_2636,
            }),
        );
        self.variance_check = saved;
    }
}
