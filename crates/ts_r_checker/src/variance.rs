//! Measured variance of a generic class or interface's type parameters.
//!
//! Two references to the same generic type (`Box<number>` and `Box<unknown>`) relate by
//! their type arguments, each compared in the direction its type parameter's variance
//! allows. The variance of a parameter is measured by relating two instantiations that
//! differ only in that parameter: one with a "sub" marker type parameter and one with a
//! "super" marker (the sub marker's constraint). Sub-to-super related means covariant,
//! super-to-sub related contravariant, both bivariant (independent when an unrelated third
//! marker relates too), neither invariant.
//!
//! While a type's variances are measured, references to it are "maybe related" (so
//! recursive types measure). A measurement is a pure function of the declaration: the
//! variances it needs of other types are measured inside it and kept local, and its
//! relation results go to a cache of its own rather than the shared one, so the result
//! does not depend on which file or thread asked first. Measurements that meet constructs
//! whose relation does not follow from markers (conditional, indexed-access, `keyof`,
//! template literal or mapped types mentioning type parameters, rest parameters typed by
//! unions of tuples that mention them) or that run out of budget
//! are marked unreliable: their references fall back to comparing type arguments
//! covariantly, then structure.

use std::sync::Arc;

use rustc_hash::FxHashMap;
use ts_r_types::{ObjectKind, RelationKind, SymId, TypeFlags, TypeId, TypeTraits};

use crate::checker::Ck;

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
    /// The measurement met constructs it can't account for (see the module docs).
    pub unreliable: bool,
}

/// Structural comparisons one relation of a measurement may make.
const MEASURE_WORK: u32 = 20_000;

/// The state of the outermost measurement in progress.
#[derive(Default, Debug)]
pub(crate) struct Probe {
    /// Relation results of the measurement.
    pub cache: FxHashMap<(TypeId, TypeId, RelationKind), bool>,
    /// Variances measured inside it (`None`: being measured).
    pub variances: FxHashMap<VarianceKey, Option<Arc<Variances>>>,
    /// The innermost running measurement met something markers can't stand for.
    pub unreliable: bool,
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

    /// Notes, while measuring, a relation between `s` and `t` whose outcome markers can't
    /// stand for: a conditional, indexed-access, `keyof`, substitution, template literal or
    /// string-mapping type, or a generic mapped type, that mentions a marker.
    pub(crate) fn note_unmeasurable(&mut self, s: TypeId, t: TypeId) {
        if self.rel.probe.as_ref().is_none_or(|p| p.unreliable) {
            return;
        }
        if (self.unmeasurable_type(s) || self.unmeasurable_type(t))
            && let Some(p) = self.rel.probe.as_mut()
        {
            p.unreliable = true;
        }
    }

    /// Notes, while measuring, a signature whose rest parameter is a union of tuples that
    /// mentions a marker (`next(...[value]: [] | [N])`): tsc's elaboration of such types
    /// falls back to structure.
    pub(crate) fn note_unmeasurable_rest(&mut self, sig: ts_r_types::SigId) {
        if self.rel.probe.as_ref().is_none_or(|p| p.unreliable) {
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
            || !st.traits(rt).contains(TypeTraits::CONTAINS_TYPE_PARAMETER)
            || !st
                .members_of_union(rt)
                .iter()
                .all(|&m| st.tuple_shape_of(m).is_some())
        {
            return;
        }
        let (sub, sup, other) = self.variance_markers();
        if (self.type_mentions(rt, sub)
            || self.type_mentions(rt, sup)
            || self.type_mentions(rt, other))
            && let Some(p) = self.rel.probe.as_mut()
        {
            p.unreliable = true;
        }
    }

    fn unmeasurable_type(&mut self, x: TypeId) -> bool {
        let st = self.store();
        if !st.traits(x).contains(TypeTraits::CONTAINS_TYPE_PARAMETER) {
            return false;
        }
        let kind = st.flags(x).intersects(
            TypeFlags::CONDITIONAL
                | TypeFlags::INDEX
                | TypeFlags::INDEXED_ACCESS
                | TypeFlags::SUBSTITUTION
                | TypeFlags::TEMPLATE_LITERAL
                | TypeFlags::STRING_MAPPING,
        ) || (matches!(
            st.object_of(x).map(|o| o.kind),
            Some(ObjectKind::Mapped { .. })
        ) && self.is_generic_mapped_type(x));
        if !kind {
            return false;
        }
        let (sub, sup, other) = self.variance_markers();
        self.type_mentions(x, sub) || self.type_mentions(x, sup) || self.type_mentions(x, other)
    }

    /// Whether `t` is a marker instantiation of a measurement in progress.
    pub(crate) fn is_marker_reference(&self, t: TypeId) -> bool {
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
        let saved_unreliable = std::mem::take(&mut probe.unreliable);
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
        let mut spent = false;
        for i in 0..tps.len() {
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
            }
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
        let unreliable = spent || std::mem::replace(&mut probe.unreliable, saved_unreliable);
        let v = Arc::new(Variances {
            list: list.into_boxed_slice(),
            unreliable,
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
}
