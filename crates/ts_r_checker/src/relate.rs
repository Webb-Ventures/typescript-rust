//! Type relations (identity, subtype, strict subtype, assignability, comparability) with a
//! shared result cache, plus error elaboration in tsc's message-chain format.
//!
//! The elaboration follows observed tsc 7.0.2 output: the innermost failure is reported
//! first and each enclosing failed comparison wraps it ("Type 'A' is not assignable to type
//! 'B'."); property incompatibilities are collected on a stack and collapsed into
//! "The types of 'a.b' are incompatible between these types." when they nest; a missing
//! property or excess property message replaces the enclosing level's message.

use rustc_hash::FxHashMap;
use ts_r_binder::SymbolFlags;
use ts_r_types::{
    ListId, ObjectFlags, ObjectKind, RelationKind, SigFlags, SigId, StrId, SymId, TupleElement,
    TypeFlags, TypeId, TypeKind, TypeTraits,
};

use crate::checker::{Ck, GlobalType, Loc};
use crate::declared::anon;
use crate::diag::{Chain, dg, msg};
use crate::diagnostics_generated::DiagnosticInfo;
use crate::members::SigKind;
use crate::symbols::check_flags;

const MAX_RELATION_DEPTH: u32 = 80;
/// Structural comparisons one outermost relation may make (see `type_related_entry`).
const MAX_RELATION_WORK: u32 = 100_000;
/// Source × target union member pairs above which two unions are not compared.
const MAX_UNION_PAIRS: usize = 1_000_000;

/// Error-reporting state of one top-level comparison.
#[derive(Default, Debug)]
pub struct Report {
    /// Outermost first.
    pub chain: Vec<(u32, String)>,
    /// Pending levels of a path (property, return type and tuple element
    /// incompatibilities), innermost first; laid out when something else is reported.
    incompatible: Vec<Incompatible>,
    /// Relation heads still to be suppressed, innermost first: `None` for a message that
    /// replaces the head (a missing property), `Some(i)` for the head enclosing
    /// `incompatible[i]` (kept in its `outer`, the path layout decides whether it shows).
    suppress: Vec<Option<usize>>,
    /// Set when an elaboration moved the error to another node (excess properties).
    pub error_node: Option<Loc>,
    /// Set when a property failed only because its `undefined` meets an optional target
    /// property under `exactOptionalPropertyTypes` (TS2375/TS2379 heads).
    exact_mismatch: bool,
    /// The source and target names of the last relation message (the head's, at the end).
    last_names: Option<(String, String)>,
    /// Set when a failed excess property check keeps the relation's head (a property
    /// checked against the union's property types).
    excess_keeps_head: bool,
}

/// One pending level of an elaboration path.
#[derive(Debug)]
struct Incompatible {
    info: DiagnosticInfo,
    args: Vec<String>,
    /// The relation enclosing this level (its head was held back).
    outer: Option<(TypeId, TypeId)>,
}

/// Per-worker relation recursion state.
#[derive(Default, Debug)]
pub struct RelState {
    pub depth: u32,
    pub(crate) stack: Vec<(TypeId, TypeId, RelationKind)>,
    /// Where the innermost top-level query (`is_type_related`) started on `stack`: pairs
    /// above it are its own comparisons in progress, which win over cached results.
    pub(crate) stack_floor: usize,
    pub(crate) maybe_used: bool,
    /// Assumptions of the comparison in progress (see [`Assumed`]).
    pub(crate) assumed: Assumed,
    /// Successes that hold if pairs still in progress further down `stack` do (tsc's
    /// "maybe" results), in the order found, with what they assumed. A repeat of one is
    /// answered from here; they are shared once those pairs succeed for good and dropped
    /// when one fails.
    pub(crate) pending: Vec<PendingKey>,
    /// What each pending success assumed, and whether it skipped a deferred check.
    pub(crate) pending_set: FxHashMap<PendingKey, (Assumed, bool)>,
    /// Successes under an intersection target's member (`in_target_member`), which are
    /// more permissive than the shared results, and whether each skipped a deferred check
    /// (`deferred_skipped`). This worker's own: they are kept apart from the shared cache.
    pub(crate) permissive_cache: FxHashMap<(TypeId, TypeId, RelationKind), bool>,
    /// Set while relating a source to one member of an intersection target: the excess
    /// property check already ran against the whole intersection.
    pub(crate) skip_excess: bool,
    /// Set while relating a fresh object literal to one member of a union target: only the
    /// excess property check is skipped (it ran against the whole union).
    pub(crate) skip_excess_props: bool,
    /// Set while relating a source to the members of an intersection target, through
    /// their properties: excess property and weak type checks are deferred (recorded in
    /// `deferred_skipped`) to the check of the source's properties against the
    /// intersection's combined properties. Results that this more permissive state makes
    /// `true` are not cached.
    pub(crate) in_target_member: bool,
    pub(crate) deferred_skipped: bool,
    /// Target properties the next `properties_related` skips (the discriminants a
    /// discriminated-union relation already compared).
    pub(crate) excluded_props: Vec<StrId>,
    /// Set while a strict function type's callback parameter is explained by its
    /// signatures (one level, so recursive callback types terminate).
    pub(crate) in_strict_callback: bool,
    pub(crate) report: Option<Box<Report>>,
    /// The variance measurement in progress (see `variance.rs`).
    pub(crate) probe: Option<Box<crate::variance::Probe>>,
    /// Structural comparisons left before the relation gives up and assumes "related"
    /// (`None`: unlimited). Declaration checks set it so that pathological recursive
    /// generics can't run away; a spent budget never produces an error.
    pub budget: Option<u32>,
}

/// A pending success: source, target, relation, and whether it held under an
/// intersection target's member (`in_target_member`).
pub(crate) type PendingKey = (TypeId, TypeId, RelationKind, bool);

/// Entries [`RelState::permissive_cache`] keeps before it starts over.
const PERMISSIVE_CACHE_MAX: usize = 1 << 20;

/// What a comparison assumed to be related while it ran.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Assumed {
    /// The lowest `stack` position of an in-progress pair it assumed related
    /// (`usize::MAX`: none). A success that assumed only its own pair is final.
    pub lowest: usize,
    /// It met deeply nested instantiations (assumed related, settled only by the
    /// outermost comparison). Its results are then not shared beyond the outermost one,
    /// since they depend on the stack they were found under.
    pub nested: bool,
    /// It assumed "related" for a reason that is never settled (the depth limit, a spent
    /// budget, a variance measurement in progress): nothing below the outermost result is
    /// kept.
    pub unknown: bool,
}

impl Default for Assumed {
    fn default() -> Assumed {
        Assumed {
            lowest: usize::MAX,
            nested: false,
            unknown: false,
        }
    }
}

/// The top-level message of a relation error.
#[derive(Clone, Copy, Debug)]
pub struct Head {
    pub info: DiagnosticInfo,
}

impl<'c, 'a> Ck<'c, 'a> {
    pub fn is_assignable(&mut self, s: TypeId, t: TypeId) -> bool {
        self.is_type_related(s, t, RelationKind::Assignable)
    }

    pub fn is_comparable(&mut self, s: TypeId, t: TypeId) -> bool {
        self.is_type_related(s, t, RelationKind::Comparable)
    }

    pub fn is_subtype(&mut self, s: TypeId, t: TypeId) -> bool {
        self.is_type_related(s, t, RelationKind::Subtype)
    }

    pub fn is_strict_subtype(&mut self, s: TypeId, t: TypeId) -> bool {
        self.is_type_related(s, t, RelationKind::StrictSubtype)
    }

    pub fn is_identical(&mut self, s: TypeId, t: TypeId) -> bool {
        self.is_type_related(s, t, RelationKind::Identity)
    }

    /// Quiet relation check.
    pub fn is_type_related(&mut self, s: TypeId, t: TypeId, rel: RelationKind) -> bool {
        let saved = self.rel.report.take();
        let floor = std::mem::replace(&mut self.rel.stack_floor, self.rel.stack.len());
        let in_member = std::mem::take(&mut self.rel.in_target_member);
        let skipped = std::mem::take(&mut self.rel.deferred_skipped);
        let r = self.type_related_entry(s, t, rel);
        self.rel.in_target_member = in_member;
        self.rel.deferred_skipped = skipped;
        self.rel.stack_floor = floor;
        self.rel.report = saved;
        r
    }

    fn type_related_entry(&mut self, s: TypeId, t: TypeId, rel: RelationKind) -> bool {
        // An outermost relation gets a budget of structural comparisons. Relating
        // instantiations of big generic interfaces structurally (rxjs's `Observable` with a
        // hundred overloaded operator methods, each returning another `Observable<...>`,
        // whose measured variance is unreliable) grows exponentially; a spent budget means
        // "related", like the declaration checks' budget.
        if self.rel.budget.is_none() && self.rel.stack.is_empty() {
            self.rel.budget = Some(MAX_RELATION_WORK);
            let r = self.type_related_entry(s, t, rel);
            self.rel.budget = None;
            return r;
        }
        let st = self.store();
        let (s, t) = (
            fresh_literal_regular(self, s),
            fresh_literal_regular(self, t),
        );
        if s == t {
            return true;
        }
        if rel != RelationKind::Identity {
            if rel == RelationKind::Comparable
                && !st.flags(t).contains(TypeFlags::NEVER)
                && self.simple_related(t, s, rel)
            {
                return true;
            }
            if self.simple_related(s, t, rel) {
                return true;
            }
        }
        let f = st.flags(s) | st.flags(t);
        if f.intersects(TypeFlags::STRUCTURED_OR_INSTANTIABLE) {
            return self.related_to(s, t, rel, false, None);
        }
        false
    }

    /// Checks the relation and reports an error at `node` when it fails. Returns whether the
    /// types are related.
    pub fn check_type_related(
        &mut self,
        s: TypeId,
        t: TypeId,
        rel: RelationKind,
        node: Option<Loc>,
        head: Option<Head>,
    ) -> bool {
        if self.is_type_related(s, t, rel) {
            return true;
        }
        let Some(node) = node else { return false };
        if !self.reporting(node.file) {
            return false;
        }
        let chain = self.relation_error_chain(s, t, rel, head);
        let (chain, at) = chain;
        let at = at.unwrap_or(node);
        self.error_chain(at, &chain);
        false
    }

    /// Runs a failing comparison with reporting on; returns the chain and the node the
    /// error moved to (if any).
    pub fn relation_error_chain(
        &mut self,
        s: TypeId,
        t: TypeId,
        rel: RelationKind,
        head: Option<Head>,
    ) -> (Chain, Option<Loc>) {
        let saved = self.rel.report.replace(Box::default());
        let (s2, t2) = (
            fresh_literal_regular(self, s),
            fresh_literal_regular(self, t),
        );
        let outermost = self.rel.budget.is_none() && self.rel.stack.is_empty();
        if outermost {
            self.rel.budget = Some(MAX_RELATION_WORK);
        }
        let ok = self.related_to(s2, t2, rel, true, head);
        if outermost {
            self.rel.budget = None;
        }
        let rep = self.rel.report.take();
        self.rel.report = saved;
        let mut rep = rep.unwrap_or_default();

        if !rep.incompatible.is_empty() {
            self.flush_incompatible(&mut rep, rel);
        }
        // An `exactOptionalPropertyTypes` mismatch names the option in the head.
        if !ok
            && rep.exact_mismatch
            && let Some((ss, ts)) = rep.last_names.take()
            && let Some(first) = rep.chain.first_mut()
        {
            let info = if first.0 == dg::TYPE_0_IS_NOT_ASSIGNABLE_TO_TYPE_1_2322.code {
                Some(dg::TYPE_0_IS_NOT_ASSIGNABLE_TO_TYPE_1_WITH_EXACTOPTIONALPROPERTYTYPES_TRUE_CONSIDER_2375)
            } else if first.0
                == dg::ARGUMENT_OF_TYPE_0_IS_NOT_ASSIGNABLE_TO_PARAMETER_OF_TYPE_1_2345.code
            {
                Some(dg::ARGUMENT_OF_TYPE_0_IS_NOT_ASSIGNABLE_TO_PARAMETER_OF_TYPE_1_WITH_EXACTOPTIONALPR_2379)
            } else {
                None
            };
            if let Some(info) = info {
                *first = (info.code, msg(info).format(&[&ss, &ts]));
            }
        }
        if ok || rep.chain.is_empty() {
            // The quiet and reporting runs disagree (assumptions in recursive types): report
            // the plain message.
            let mut c = Chain::new();
            let (ss, ts) = self.type_names_for_error(s, t);
            c.push_back(
                head.map_or(dg::TYPE_0_IS_NOT_ASSIGNABLE_TO_TYPE_1_2322, |h| h.info),
                &[&ss, &ts],
            );
            return (c, rep.error_node);
        }
        (
            Chain {
                items: rep.chain,
                nested: Vec::new(),
            },
            rep.error_node,
        )
    }

    fn report_error(&mut self, info: DiagnosticInfo, args: &[&str]) {
        if let Some(r) = self.rel.report.as_mut() {
            let text = msg(info).format(args);
            r.chain.insert(0, (info.code, text));
        }
    }

    fn reporting_rel(&self) -> bool {
        self.rel.report.is_some()
    }

    fn report_incompatible(&mut self, info: DiagnosticInfo, args: Vec<String>) {
        if let Some(r) = self.rel.report.as_mut() {
            r.suppress.push(Some(r.incompatible.len()));
            r.incompatible.push(Incompatible {
                info,
                args,
                outer: None,
            });
        }
    }

    /// Holds back the head of the enclosing relation: `msg` replaces it.
    fn suppress_next_head(&mut self) {
        if let Some(r) = self.rel.report.as_mut() {
            r.suppress.push(None);
        }
    }

    /// Lays out the pending path levels (observed tsc 7 rules). Outermost first, each
    /// level shows the head of its enclosing relation, then:
    /// - a property starts a path that takes the following properties, and return types
    ///   right after a property (`f()`, `new A()`); the heads in between are dropped. A
    ///   lone property is "Types of property 'x' are incompatible", a longer path "The
    ///   types of 'a.b'" or, when the first property is called, "The types returned by
    ///   'f().x'";
    /// - a return type outside a path prints nothing (its head says it);
    /// - anything else (tuple positions) prints its message.
    fn flush_incompatible(&mut self, rep: &mut Report, rel: RelationKind) {
        let entries = std::mem::take(&mut rep.incompatible);
        rep.suppress.retain(|o| o.is_none());
        enum Item {
            Head(TypeId, TypeId),
            Msg(DiagnosticInfo, Vec<String>),
        }
        let is_return = |code: u32| (2202..=2205).contains(&code);
        let mut plan: Vec<Item> = Vec::new();
        let mut i = entries.len();
        while i > 0 {
            i -= 1;
            let e = &entries[i];
            if let Some((s, t)) = e.outer {
                plan.push(Item::Head(s, t));
            }
            if is_return(e.info.code) {
                continue;
            }
            if e.info.code != dg::TYPES_OF_PROPERTY_0_ARE_INCOMPATIBLE_2326.code {
                plan.push(Item::Msg(e.info, e.args.clone()));
                continue;
            }
            let mut elems = vec![i];
            while i > 0 {
                let next = entries[i - 1].info.code;
                let last = entries[elems[elems.len() - 1]].info.code;
                let joins = next == dg::TYPES_OF_PROPERTY_0_ARE_INCOMPATIBLE_2326.code
                    || (is_return(next) && !is_return(last));
                if !joins {
                    break;
                }
                i -= 1;
                elems.push(i);
            }
            if elems.len() == 1 {
                plan.push(Item::Msg(e.info, e.args.clone()));
                continue;
            }
            let mut path = String::new();
            for &k in &elems {
                let x = &entries[k];
                if is_return(x.info.code) {
                    let prefix = if x.info.code == 2203 || x.info.code == 2205 {
                        "new "
                    } else {
                        ""
                    };
                    let params = if x.info.code == 2204 || x.info.code == 2205 {
                        ""
                    } else {
                        "..."
                    };
                    path = format!("{prefix}{path}({params})");
                    continue;
                }
                if path.starts_with("new ") {
                    path = format!("({path})");
                }
                let s = x.args.first().cloned().unwrap_or_default();
                if path.is_empty() {
                    path = s;
                } else if is_identifier_text(&s) {
                    path = format!("{path}.{s}");
                } else if s.starts_with('[') && s.ends_with(']') {
                    path = format!("{path}{s}");
                } else {
                    path = format!("{path}[{s}]");
                }
            }
            let info = if is_return(entries[elems[1]].info.code) {
                dg::THE_TYPES_RETURNED_BY_0_ARE_INCOMPATIBLE_BETWEEN_THESE_TYPES_2201
            } else {
                dg::THE_TYPES_OF_0_ARE_INCOMPATIBLE_BETWEEN_THESE_TYPES_2200
            };
            plan.push(Item::Msg(info, vec![path]));
        }
        // The chain is built inside out.
        for item in plan.into_iter().rev() {
            match item {
                Item::Msg(info, args) => {
                    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                    rep.chain.insert(0, (info.code, msg(info).format(&a)));
                }
                Item::Head(s, t) => {
                    // Re-enter reporting to add the held-back level's message.
                    let saved = self.rel.report.replace(Box::new(std::mem::take(rep)));
                    self.report_relation_error(None, s, t, rel);
                    if let Some(r) = self.rel.report.take() {
                        *rep = *r;
                    }
                    self.rel.report = saved;
                }
            }
        }
    }

    /// The source and target type strings for an error message.
    /// Source and target names for a relation message: qualified (`M.I`, `N.I`) when they
    /// would otherwise print alike.
    fn type_names_for_error(&mut self, s: TypeId, t: TypeId) -> (String, String) {
        let ss = self.type_to_string(s);
        let ts = self.type_to_string(t);
        if ss == ts && s != t {
            let saved = std::mem::replace(&mut self.print_qualified, true);
            let qs = self.type_to_string(s);
            let qt = self.type_to_string(t);
            self.print_qualified = saved;
            return (qs, qt);
        }
        (ss, ts)
    }

    fn report_error_results(
        &mut self,
        s: TypeId,
        t: TypeId,
        head: Option<Head>,
        rel: RelationKind,
    ) {
        if !self.reporting_rel() {
            return;
        }
        let owner = match self.rel.report.as_mut() {
            Some(r) if !r.suppress.is_empty() => Some(r.suppress.remove(0)),
            _ => None,
        };
        let suppress = owner.is_some();
        // A missing-property error stands in for the default head, and (tsc 7) for an
        // argument's TS2345 head, a type argument's TS2344 head and a `this` argument's
        // TS2684 head too.
        let replaceable = head.is_none_or(|h| {
            (h.info.code
                == dg::ARGUMENT_OF_TYPE_0_IS_NOT_ASSIGNABLE_TO_PARAMETER_OF_TYPE_1_2345.code
                || h.info.code == dg::TYPE_0_DOES_NOT_SATISFY_THE_CONSTRAINT_1_2344.code
                || h.info.code
                    == dg::THE_THIS_CONTEXT_OF_TYPE_0_IS_NOT_ASSIGNABLE_TO_METHOD_S_THIS_OF_TYPE_1_2684
                        .code)
                && self.rel.report.as_ref().is_some_and(|r| {
                    r.incompatible.is_empty()
                        && r.chain
                            .first()
                            .is_some_and(|c| matches!(c.0, 2739..=2741 | 4104))
                })
        });
        // Not under the `Object` hint (TS2696), which needs the head above it.
        let object_hint = self.global_type(GlobalType::Object) == Some(s);
        if replaceable && suppress && !object_hint {
            if let (Some(r), Some(Some(i))) = (self.rel.report.as_mut(), owner)
                && let Some(e) = r.incompatible.get_mut(i)
            {
                e.outer = Some((s, t));
            }
            return;
        }
        self.report_relation_error(head, s, t, rel);
    }

    /// Emits the pending property-incompatibility levels and the head they deferred.
    fn flush_pending_incompatible(&mut self, rel: RelationKind) {
        if let Some(r) = self.rel.report.as_mut()
            && !r.incompatible.is_empty()
        {
            let mut rep = std::mem::take(&mut **r);
            self.flush_incompatible(&mut rep, rel);
            if let Some(r) = self.rel.report.as_mut() {
                **r = rep;
            }
        }
    }

    fn report_relation_error(
        &mut self,
        head: Option<Head>,
        s: TypeId,
        t: TypeId,
        rel: RelationKind,
    ) {
        if let Some(r) = self.rel.report.as_mut()
            && !r.incompatible.is_empty()
        {
            let mut rep = std::mem::take(&mut **r);
            self.flush_incompatible(&mut rep, rel);
            if let Some(r) = self.rel.report.as_mut() {
                **r = rep;
            }
        }
        let st = self.store();
        let generalized = if self.is_literal_type(s) && !self.could_have_top_level_singleton(t) {
            self.base_of_literal(s)
        } else {
            s
        };
        let (mut gs, ts) = self.type_names_for_error(generalized, t);
        // An enum type widened from one of its members prints qualified (`N.E`).
        if generalized != s
            && self.some_member(s, |m| st.flags(m).contains(TypeFlags::ENUM_LITERAL))
        {
            let saved = std::mem::replace(&mut self.print_qualified, true);
            gs = self.type_to_string(generalized);
            self.print_qualified = saved;
        }
        let type_param = st.flags(t).contains(TypeFlags::TYPE_PARAMETER);
        if rel != RelationKind::Comparable && !type_param {
            self.report_wrapper_hints(s, t);
        }
        if type_param {
            // A type parameter target says why the source isn't enough: it fits the
            // constraint but `T` may be narrower, or `T` may be anything at all. Reported
            // before the head because the chain is built inside out.
            let saved = self.rel.report.take();
            let c = self.base_constraint(t);
            let fits = c != TypeId::UNKNOWN
                && c != TypeId::UNRESOLVED
                && self.is_type_related(generalized, c, rel);
            let cs = fits.then(|| self.type_to_string(c));
            self.rel.report = saved;
            if fits {
                self.report_wrapper_hints(s, c);
            }
            match cs {
                Some(cs) => self.report_error(
                    dg::_0_IS_ASSIGNABLE_TO_THE_CONSTRAINT_OF_TYPE_1_BUT_1_COULD_BE_INSTANTIATED_WITH_A_D_5075,
                    &[&gs, &ts, &cs],
                ),
                None => self.report_error(
                    dg::_0_COULD_BE_INSTANTIATED_WITH_AN_ARBITRARY_TYPE_WHICH_COULD_BE_UNRELATED_TO_1_5082,
                    &[&ts, &gs],
                ),
            }
        }
        let info = match head {
            Some(h) => h.info,
            None => {
                if rel == RelationKind::Comparable {
                    dg::TYPE_0_IS_NOT_COMPARABLE_TO_TYPE_1_2678
                } else if gs == ts {
                    // Even qualified, the names are alike (two type parameters `T`).
                    dg::TYPE_0_IS_NOT_ASSIGNABLE_TO_TYPE_1_TWO_DIFFERENT_TYPES_WITH_THIS_NAME_EXIST_BUT_T_2719
                } else {
                    // A string literal against a union of string literals suggests the
                    // closest one.
                    if let Some(sug) = self.string_literal_suggestion(s, t) {
                        self.report_error(
                            dg::TYPE_0_IS_NOT_ASSIGNABLE_TO_TYPE_1_DID_YOU_MEAN_2_2820,
                            &[&gs, &ts, &sug],
                        );
                        if let Some(r) = self.rel.report.as_mut() {
                            r.last_names = Some((gs, ts));
                        }
                        return;
                    }
                    dg::TYPE_0_IS_NOT_ASSIGNABLE_TO_TYPE_1_2322
                }
            }
        };
        self.report_error(info, &[&gs, &ts]);
        if let Some(r) = self.rel.report.as_mut() {
            r.last_names = Some((gs, ts));
        }
    }

    /// The hints tsc adds under a relation message about the global `Object` type (TS2696:
    /// a source of type `Object` against an object target or a union with one; for a type
    /// parameter target, against the constraint the source fits) and wrapper objects (TS2692: `Number` against `number`).
    fn report_wrapper_hints(&mut self, s: TypeId, t: TypeId) {
        let st = self.store();
        let globals = [
            (GlobalType::Number, TypeId::NUMBER, "number"),
            (GlobalType::String, TypeId::STRING, "string"),
            (GlobalType::Boolean, TypeId::BOOLEAN, "boolean"),
            (GlobalType::Symbol, TypeId::SYMBOL, "symbol"),
        ];
        for (g, prim, name) in globals {
            if t == prim && self.global_type(g) == Some(s) {
                let wrapper = self.type_to_string(s);
                self.report_error(
                    dg::_0_IS_A_PRIMITIVE_BUT_1_IS_A_WRAPPER_OBJECT_PREFER_USING_0_WHEN_POSSIBLE_2692,
                    &[name, &wrapper],
                );
                return;
            }
        }
        if !st.flags(s).contains(TypeFlags::OBJECT)
            || self.global_type(GlobalType::Object) != Some(s)
        {
            return;
        }
        let target_object = if st.flags(t).contains(TypeFlags::UNION) {
            st.members_of_union(t)
                .iter()
                .any(|&m| st.flags(m).contains(TypeFlags::OBJECT))
        } else {
            st.flags(t).contains(TypeFlags::OBJECT)
        };
        if target_object {
            self.report_error(
                dg::THE_OBJECT_TYPE_IS_ASSIGNABLE_TO_VERY_FEW_OTHER_TYPES_DID_YOU_MEAN_TO_USE_THE_AN_2696,
                &[],
            );
        }
    }

    /// `isLiteralType`: booleans, unit types and unions of unit types.
    pub fn is_literal_type(&self, t: TypeId) -> bool {
        let s = self.store();
        let f = s.flags(t);
        if f.contains(TypeFlags::BOOLEAN) {
            return true;
        }
        if f.contains(TypeFlags::UNION) {
            return s.traits(t).contains(TypeTraits::ENUM_UNION)
                || s.members_of_union(t).iter().all(|&m| {
                    s.flags(m).intersects(TypeFlags::UNIT) && !s.flags(m).contains(TypeFlags::VOID)
                });
        }
        f.intersects(TypeFlags::UNIT)
    }

    fn could_have_top_level_singleton(&mut self, t: TypeId) -> bool {
        let s = self.store();
        let f = s.flags(t);
        if f.contains(TypeFlags::BOOLEAN) {
            return false;
        }
        if f.intersects(TypeFlags::UNION_OR_INTERSECTION) {
            let ms = s.members_of_union(t).to_vec();
            return ms
                .into_iter()
                .any(|m| self.could_have_top_level_singleton(m));
        }
        if f.intersects(TypeFlags::INSTANTIABLE) {
            let c = self.base_constraint(t);
            if c != t && c != TypeId::UNKNOWN {
                return self.could_have_top_level_singleton(c);
            }
        }
        // `never` keeps the literal too (`Type '"b"' is not assignable to type 'never'`), as
        // does an enum with computed members (its other members are literals). `void` and
        // string mappings (`Uppercase<string>`) do not.
        f.intersects(
            TypeFlags::UNIT | TypeFlags::ENUM | TypeFlags::TEMPLATE_LITERAL | TypeFlags::NEVER,
        ) && !f.contains(TypeFlags::VOID)
    }

    /// tsc's `isSimpleTypeRelatedTo`.
    pub fn simple_related(&mut self, s: TypeId, t: TypeId, rel: RelationKind) -> bool {
        let st = self.store();
        let sf = st.flags(s);
        let tf = st.flags(t);
        let strict = self.c.opts.strict_null_checks;
        if tf.contains(TypeFlags::ANY) || sf.contains(TypeFlags::NEVER) || s == TypeId::WILDCARD {
            return true;
        }
        // Unresolved (later-layer) types are related to everything, both ways.
        if s == TypeId::UNRESOLVED || t == TypeId::UNRESOLVED {
            return true;
        }
        // The error type is an `any`: a subtype of nothing but `any` and `unknown`, so the
        // subtype pass of overload resolution skips `(n: number)` for an error-typed argument.
        if self.store().is_error(s)
            && !matches!(rel, RelationKind::Subtype | RelationKind::StrictSubtype)
        {
            return true;
        }
        if tf.contains(TypeFlags::UNKNOWN)
            && !(rel == RelationKind::StrictSubtype && sf.contains(TypeFlags::ANY))
        {
            return true;
        }
        if tf.contains(TypeFlags::NEVER) {
            return false;
        }
        if sf.intersects(TypeFlags::STRING_LIKE) && tf.contains(TypeFlags::STRING) {
            return true;
        }
        if sf.contains(TypeFlags::STRING_LITERAL)
            && sf.contains(TypeFlags::ENUM_LITERAL)
            && tf.contains(TypeFlags::STRING_LITERAL)
            && !tf.contains(TypeFlags::ENUM_LITERAL)
            && st.string_value(s) == st.string_value(t)
        {
            return true;
        }
        if sf.intersects(TypeFlags::NUMBER_LIKE) && tf.contains(TypeFlags::NUMBER) {
            return true;
        }
        if sf.contains(TypeFlags::NUMBER_LITERAL)
            && sf.contains(TypeFlags::ENUM_LITERAL)
            && tf.contains(TypeFlags::NUMBER_LITERAL)
            && !tf.contains(TypeFlags::ENUM_LITERAL)
            && st.number_value(s) == st.number_value(t)
        {
            return true;
        }
        if sf.intersects(TypeFlags::BIGINT_LIKE) && tf.contains(TypeFlags::BIGINT) {
            return true;
        }
        if sf.intersects(TypeFlags::BOOLEAN_LIKE)
            && tf.contains(TypeFlags::BOOLEAN)
            && !tf.contains(TypeFlags::UNION)
        {
            return true;
        }
        if sf.intersects(TypeFlags::BOOLEAN_LIKE) && t == TypeId::BOOLEAN {
            return true;
        }
        if sf.intersects(TypeFlags::ES_SYMBOL_LIKE) && tf.contains(TypeFlags::ES_SYMBOL) {
            return true;
        }
        if sf.contains(TypeFlags::ENUM)
            && tf.contains(TypeFlags::ENUM)
            && self.c.sym_name(enum_sym(st, s)) == self.c.sym_name(enum_sym(st, t))
        {
            return true;
        }
        // A literal member of an enum with computed members belongs to its enum type.
        if sf.contains(TypeFlags::ENUM_LITERAL)
            && tf.contains(TypeFlags::ENUM)
            && let TypeKind::EnumLiteral { member, .. } = st.kind(s)
            && self.c.sym_parent(member) == Some(enum_sym(st, t))
        {
            return true;
        }
        if rel == RelationKind::Comparable
            && sf.contains(TypeFlags::ENUM)
            && tf.contains(TypeFlags::ENUM_LITERAL)
            && let TypeKind::EnumLiteral { member, .. } = st.kind(t)
            && self.c.sym_parent(member) == Some(enum_sym(st, s))
        {
            return true;
        }
        if sf.contains(TypeFlags::ENUM_LITERAL)
            && tf.contains(TypeFlags::ENUM_LITERAL)
            && let (
                TypeKind::EnumLiteral {
                    member: m1,
                    value: v1,
                },
                TypeKind::EnumLiteral {
                    member: m2,
                    value: v2,
                },
            ) = (st.kind(s), st.kind(t))
            && v1 == v2
        {
            let p1 = self.c.sym_parent(m1);
            let p2 = self.c.sym_parent(m2);
            if p1 == p2
                || (p1.is_some()
                    && p2.is_some()
                    && self.c.sym_name(p1.unwrap_or(SymId::NONE))
                        == self.c.sym_name(p2.unwrap_or(SymId::NONE)))
            {
                return true;
            }
        }
        if sf.contains(TypeFlags::UNDEFINED)
            && ((!strict && !tf.intersects(TypeFlags::UNION_OR_INTERSECTION))
                || tf.intersects(TypeFlags::UNDEFINED | TypeFlags::VOID))
        {
            if s == TypeId::MISSING
                && t != TypeId::MISSING
                && tf.contains(TypeFlags::UNDEFINED)
                && self.c.opts.exact_optional_property_types
            {
                return t == TypeId::UNDEFINED;
            }
            return true;
        }
        if sf.contains(TypeFlags::NULL)
            && ((!strict && !tf.intersects(TypeFlags::UNION_OR_INTERSECTION))
                || tf.contains(TypeFlags::NULL))
        {
            return true;
        }
        if sf.contains(TypeFlags::OBJECT) && tf.contains(TypeFlags::NON_PRIMITIVE) {
            return !(rel == RelationKind::StrictSubtype && self.is_empty_anonymous_object(s));
        }
        if rel == RelationKind::Assignable || rel == RelationKind::Comparable {
            if sf.contains(TypeFlags::ANY) {
                return true;
            }
            if sf.contains(TypeFlags::NUMBER)
                && (tf.contains(TypeFlags::ENUM)
                    || (tf.contains(TypeFlags::NUMBER_LITERAL)
                        && tf.contains(TypeFlags::ENUM_LITERAL)))
            {
                return true;
            }
            if sf.contains(TypeFlags::NUMBER_LITERAL)
                && !sf.contains(TypeFlags::ENUM_LITERAL)
                && (tf.contains(TypeFlags::ENUM)
                    || (tf.contains(TypeFlags::NUMBER_LITERAL)
                        && tf.contains(TypeFlags::ENUM_LITERAL)
                        && st.number_value(s) == st.number_value(t)))
            {
                return true;
            }
        }
        false
    }

    /// tsc's `isRelatedTo`.
    fn related_to(
        &mut self,
        s: TypeId,
        t: TypeId,
        rel: RelationKind,
        report: bool,
        head: Option<Head>,
    ) -> bool {
        let skip_excess = std::mem::take(&mut self.rel.skip_excess);
        let skip_excess_props = std::mem::take(&mut self.rel.skip_excess_props);
        if s == t {
            return true;
        }
        let st = self.store();
        // A linked forward reference to an alias is its target (`object`, not a copy).
        let s = fresh_literal_regular(self, st.unforward(s));
        let t = fresh_literal_regular(self, st.unforward(t));
        // A source `{ [K in C]: X }[C]` relates (and is reported) as `X` with `K` replaced by
        // `C`.
        let s = if rel != RelationKind::Identity
            && st.flags(s).contains(TypeFlags::INDEXED_ACCESS)
            && let Some(sub) = self.indexed_mapped_substitution(s)
        {
            sub
        } else {
            s
        };
        if s == t {
            return true;
        }
        // A reference named by an alias relates as the plain reference: a missing property
        // below it names the plain reference and leaves the head (`Type 'NB' is not
        // assignable to type 'Obj'.`, then `Type 'Box<number>' is missing ...`).
        let (os, ot) = (s, t);
        let (s, t) = if rel == RelationKind::Identity {
            (s, t)
        } else {
            (
                self.without_reference_alias(s),
                self.without_reference_alias(t),
            )
        };
        if s == t {
            return true;
        }
        let keep_head = if s != os || t != ot {
            self.rel
                .report
                .as_ref()
                .map(|r| (r.suppress.len(), r.chain.len()))
        } else {
            None
        };
        self.note_unmeasurable(s, t);
        let st = self.store();
        let sf = st.flags(s);
        let tf = st.flags(t);
        if rel == RelationKind::Identity {
            // Every `any` (implicit, widened, from an error) is the one `any`.
            if sf.contains(TypeFlags::ANY) && tf.contains(TypeFlags::ANY) {
                return true;
            }
            if sf != tf {
                if report {
                    self.report_error_results(s, t, head, rel);
                }
                return false;
            }
            if sf.intersects(TypeFlags::UNIT)
                || !sf.intersects(TypeFlags::STRUCTURED_OR_INSTANTIABLE)
            {
                if report {
                    self.report_error_results(s, t, head, rel);
                }
                return false;
            }
            let r = self.recursive_related(s, t, rel, report);
            if !r && report {
                self.report_error_results(s, t, head, rel);
            }
            return r;
        }
        if sf.contains(TypeFlags::OBJECT) && tf.intersects(TypeFlags::PRIMITIVE) {
            if (rel == RelationKind::Comparable
                && !tf.contains(TypeFlags::NEVER)
                && self.simple_related(t, s, rel))
                || self.simple_related(s, t, rel)
            {
                return true;
            }
            // `{}` (a type parameter's `{}` constraint) is comparable with primitives.
            if rel == RelationKind::Comparable
                && !tf.intersects(TypeFlags::NEVER | TypeFlags::NULLABLE | TypeFlags::VOID)
                && self.is_empty_object_type(s)
            {
                return true;
            }
            if report {
                self.report_error_results(os, ot, head, rel);
            }
            return false;
        }
        if sf.contains(TypeFlags::TYPE_PARAMETER) && self.type_param_constraint(s) == Some(t) {
            return true;
        }
        if (rel == RelationKind::Comparable
            && !tf.contains(TypeFlags::NEVER)
            && self.simple_related(t, s, rel))
            || self.simple_related(s, t, rel)
        {
            return true;
        }
        // Reporting against `X | undefined` (or `| null`) with a source that has neither:
        // tsc reports against `X` when that is a single type. A union source keeps the
        // whole target in its head; its members are reported against `X`. So does
        // `unknown`.
        if report
            && tf.contains(TypeFlags::UNION)
            && s != TypeId::UNKNOWN
            && (!sf.contains(TypeFlags::UNION) || s == TypeId::BOOLEAN)
            && rel != RelationKind::Comparable
            && !self.some_member(s, |m| {
                self.store()
                    .flags(m)
                    .intersects(TypeFlags::NULL | TypeFlags::UNDEFINED | TypeFlags::VOID)
            })
        {
            let nn = self.filter_type(t, |ck, m| {
                !ck.store()
                    .flags(m)
                    .intersects(TypeFlags::NULL | TypeFlags::UNDEFINED)
            });
            if nn != t && nn != TypeId::NEVER && !self.store().flags(nn).contains(TypeFlags::UNION)
            {
                self.rel.skip_excess = skip_excess;
                return self.related_to(s, nn, rel, report, head);
            }
        }
        if sf.intersects(TypeFlags::STRUCTURED_OR_INSTANTIABLE)
            || tf.intersects(TypeFlags::STRUCTURED_OR_INSTANTIABLE)
        {
            // Excess property checks for fresh object literals.
            let fresh = st
                .object_of(s)
                .is_some_and(|o| o.flags.contains(ObjectFlags::FRESH_LITERAL));
            if fresh && !skip_excess && !skip_excess_props {
                if self.rel.in_target_member {
                    if self.has_excess_properties(s, t, rel, false) {
                        self.rel.deferred_skipped = true;
                    }
                } else if self.has_excess_properties(s, t, rel, report) {
                    if report
                        && let Some(r) = self.rel.report.as_mut()
                        && std::mem::take(&mut r.excess_keeps_head)
                    {
                        self.report_error_results(os, ot, head, rel);
                    }
                    return false;
                }
            }
            let weak = !skip_excess && self.weak_type_violation(s, t, rel);
            if weak && self.rel.in_target_member {
                self.rel.deferred_skipped = true;
            } else if weak {
                if report {
                    let (ss, ts) = self.type_names_for_error(os, ot);
                    let calls = self.signatures_of(s, SigKind::Call);
                    let callable = match calls.first() {
                        Some(&sig) => {
                            let r = self.return_type_of(sig);
                            self.is_type_related(r, t, rel)
                        }
                        None => false,
                    };
                    if callable {
                        self.report_error(dg::VALUE_OF_TYPE_0_HAS_NO_PROPERTIES_IN_COMMON_WITH_TYPE_1_DID_YOU_MEAN_TO_CALL_IT_2560, &[&ss, &ts]);
                    } else {
                        self.report_error(
                            dg::TYPE_0_HAS_NO_PROPERTIES_IN_COMMON_WITH_TYPE_1_2559,
                            &[&ss, &ts],
                        );
                    }
                }
                return false;
            }
            let skip_cache = (sf.contains(TypeFlags::UNION)
                && st.members_of_union(s).len() < 4
                && !tf.contains(TypeFlags::UNION))
                || (tf.contains(TypeFlags::UNION)
                    && st.members_of_union(t).len() < 4
                    && !sf.intersects(TypeFlags::STRUCTURED_OR_INSTANTIABLE));
            let r = if skip_cache {
                self.union_or_intersection_related(s, t, rel, report)
            } else {
                self.recursive_related(s, t, rel, report)
            };
            if r {
                return true;
            }
        }
        if report {
            if let Some((v, n)) = keep_head {
                self.renamed_readonly_or_keep_head(os, ot, v, n);
            }
            self.report_error_results(os, ot, head, rel);
        }
        false
    }

    /// After a failed relation of an alias-named reference compared as the plain reference:
    /// a TS4104 it reported names the alias (`The type 'RO' is 'readonly' ...`) and still
    /// stands in for the head; any other message (a missing property) leaves the head.
    fn renamed_readonly_or_keep_head(&mut self, os: TypeId, ot: TypeId, v: usize, n: usize) {
        let readonly = self.rel.report.as_ref().is_some_and(|r| {
            r.chain.len() > n && r.chain.first().is_some_and(|c| {
                c.0 == dg::THE_TYPE_0_IS_READONLY_AND_CANNOT_BE_ASSIGNED_TO_THE_MUTABLE_TYPE_1_4104
                    .code
            })
        });
        if readonly {
            let (ss, ts) = self.type_names_for_error(os, ot);
            let text =
                msg(dg::THE_TYPE_0_IS_READONLY_AND_CANNOT_BE_ASSIGNED_TO_THE_MUTABLE_TYPE_1_4104)
                    .format(&[&ss, &ts]);
            if let Some(r) = self.rel.report.as_mut()
                && let Some(first) = r.chain.first_mut()
            {
                first.1 = text;
            }
        } else if let Some(r) = self.rel.report.as_mut() {
            r.suppress.truncate(v);
        }
    }

    fn recursive_related(&mut self, s: TypeId, t: TypeId, rel: RelationKind, report: bool) -> bool {
        let cache = self.store().relation_cache(rel);
        // A pair this query is already comparing is assumed related, before any cached
        // result: while reporting, the outer pair's failure is cached already, but (as in a
        // single reporting pass) its members still see it as in progress.
        let floor = self.rel.stack_floor.min(self.rel.stack.len());
        if let Some(i) = self.rel.stack[floor..]
            .iter()
            .position(|&(a, b, r)| a == s && b == t && r == rel)
        {
            self.rel.maybe_used = true;
            self.rel.assumed.lowest = self.rel.assumed.lowest.min(floor + i);
            return true;
        }
        // A variance measurement keeps its results to itself. Under an intersection
        // target's member only cached successes hold (a deferred check can't fail), and its
        // own successes are kept apart (they skip checks).
        let permissive = self.rel.in_target_member;
        if let Some(p) = self.rel.probe.as_ref() {
            if let Some(&r) = p.cache.get(&(s, t, rel))
                && (r || !permissive)
            {
                return r;
            }
        } else if !report {
            if let Some(r) = cache.get(s, t)
                && (r || !permissive)
            {
                return r;
            }
            if permissive && let Some(&skipped) = self.rel.permissive_cache.get(&(s, t, rel)) {
                self.rel.deferred_skipped |= skipped;
                return true;
            }
        }
        // A success found earlier in this comparison under assumptions still open:
        // related under the same assumptions.
        if !report && let Some(&(a, skipped)) = self.rel.pending_set.get(&(s, t, rel, permissive)) {
            self.rel.maybe_used = true;
            self.rel.assumed.lowest = self.rel.assumed.lowest.min(a.lowest);
            self.rel.assumed.nested |= a.nested;
            self.rel.deferred_skipped |= skipped;
            return true;
        }
        if let Some(i) = self.rel.stack[..floor]
            .iter()
            .position(|&(a, b, r)| a == s && b == t && r == rel)
        {
            self.rel.maybe_used = true;
            self.rel.assumed.lowest = self.rel.assumed.lowest.min(i);
            return true;
        }
        if self.rel.stack.len() as u32 >= MAX_RELATION_DEPTH {
            self.rel.maybe_used = true;
            self.rel.assumed.unknown = true;
            return true;
        }
        if self.deeply_nested(s, t) {
            // Settled by the outermost comparison (as tsc's "maybe" result).
            self.rel.maybe_used = true;
            self.rel.assumed.lowest = 0;
            self.rel.assumed.nested = true;
            return true;
        }
        if let Some(b) = self.rel.budget.as_mut() {
            if *b == 0 {
                self.rel.maybe_used = true;
                self.rel.assumed.unknown = true;
                return true;
            }
            *b -= 1;
        }
        let at = self.rel.stack.len();
        self.rel.stack.push((s, t, rel));
        let saved = std::mem::take(&mut self.rel.maybe_used);
        let saved_assumed = std::mem::take(&mut self.rel.assumed);
        let saved_skipped = std::mem::take(&mut self.rel.deferred_skipped);
        let pending_start = self.rel.pending.len();
        let r = self.structured_related(s, t, rel, report);
        self.rel.stack.pop();
        let used = self.rel.maybe_used;
        let mine = std::mem::replace(&mut self.rel.assumed, saved_assumed);
        // Whether this comparison skipped a check deferred to an intersection target.
        let skipped = self.rel.deferred_skipped;
        self.rel.deferred_skipped = saved_skipped || skipped;
        // A failure holds whatever was assumed, so its assumptions don't reach the caller.
        // A success passes on what it assumed below this pair (assuming this pair itself
        // is settled here).
        if r {
            self.rel.maybe_used = saved || used;
            let a = &mut self.rel.assumed;
            if mine.lowest < at {
                a.lowest = a.lowest.min(mine.lowest);
            }
            a.nested |= mine.nested;
            a.unknown |= mine.unknown;
        } else {
            self.rel.maybe_used = saved;
        }
        // A result decided by a spent budget is not final (it depends on what the cache
        // held when the budget started), so it is not kept.
        let keep = self.rel.budget != Some(0);
        let settled = !used || (mine.lowest >= at && !mine.unknown);
        let outermost = at == 0;
        if (!r || (settled && !mine.nested) || outermost) && keep {
            self.keep_relation((s, t, rel, permissive), r, skipped);
        }
        if r && settled && !mine.nested && keep {
            // What held under assumptions about this pair holds for good.
            for i in pending_start..self.rel.pending.len() {
                let k = self.rel.pending[i];
                let skipped = self.rel.pending_set.get(&k).is_some_and(|&(_, x)| x);
                self.keep_relation(k, true, skipped);
            }
            self.truncate_pending(pending_start);
        } else if r && !settled && !mine.unknown && keep {
            let k = (s, t, rel, permissive);
            self.rel.pending.push(k);
            self.rel.pending_set.insert(k, (mine, skipped));
        } else if !r || settled || outermost {
            self.truncate_pending(pending_start);
        }
        r
    }

    /// Records a final relation result: in the measurement's cache while measuring
    /// variances, else in the shared cache; successes under an intersection target's
    /// member in this worker's permissive cache (not while measuring).
    fn keep_relation(&mut self, (s, t, rel, permissive): PendingKey, r: bool, skipped: bool) {
        if r && permissive {
            if self.rel.probe.is_none() {
                if self.rel.permissive_cache.len() >= PERMISSIVE_CACHE_MAX {
                    self.rel.permissive_cache.clear();
                }
                self.rel.permissive_cache.insert((s, t, rel), skipped);
            }
        } else if let Some(p) = self.rel.probe.as_mut() {
            p.cache.insert((s, t, rel), r);
        } else {
            self.store().relation_cache(rel).set(s, t, r);
        }
    }

    fn truncate_pending(&mut self, len: usize) {
        if self.rel.pending.len() > len {
            for k in self.rel.pending.drain(len..) {
                self.rel.pending_set.remove(&k);
            }
        }
    }

    /// Three or more nested comparisons of instantiations of the same generic types.
    fn deeply_nested(&self, s: TypeId, t: TypeId) -> bool {
        let si = self.recursion_identity(s);
        let ti = self.recursion_identity(t);
        if si.is_none() && ti.is_none() {
            return false;
        }
        // A side without a recursion identity matches only itself (`X[]` to `StyleArray`
        // below `X[][]` to the union `Style` is not a third nesting).
        let id = |ck: &Self, t: TypeId| ck.recursion_identity(t).unwrap_or((u32::MAX, t.0));
        let (si, ti) = (id(self, s), id(self, t));
        let mut n = 0;
        for &(a, b, _) in &self.rel.stack {
            if id(self, a) == si && id(self, b) == ti {
                n += 1;
                if n >= 3 {
                    return true;
                }
            }
        }
        false
    }

    fn recursion_identity(&self, t: TypeId) -> Option<(u32, u32)> {
        let s = self.store();
        match s.object_of(t).map(|o| o.kind) {
            Some(ObjectKind::Reference { target, .. }) => Some((0, target.0)),
            Some(ObjectKind::Anonymous { decl, kind, .. }) => {
                Some((1 + kind as u32, decl.0 ^ decl.1.rotate_left(7)))
            }
            _ => None,
        }
    }

    fn union_or_intersection_related(
        &mut self,
        s: TypeId,
        t: TypeId,
        rel: RelationKind,
        report: bool,
    ) -> bool {
        let st = self.store();
        let sf = st.flags(s);
        let tf = st.flags(t);
        // Comparing two huge unions member by member is too costly (tsc reports TS2859
        // there); stay silent instead.
        if sf.contains(TypeFlags::UNION)
            && tf.contains(TypeFlags::UNION)
            && st.members_of_union(s).len() * st.members_of_union(t).len() > MAX_UNION_PAIRS
        {
            return true;
        }
        if sf.contains(TypeFlags::UNION) {
            let members = st.members_of_union(s).to_vec();
            if rel == RelationKind::Comparable {
                for &m in &members {
                    if self.related_to(m, t, rel, false, None) {
                        return true;
                    }
                }
                // tsc elaborates against the last member.
                if report && let Some(&m) = members.last() {
                    self.related_to(m, t, rel, true, None);
                }
                return false;
            }
            // `boolean` and an enum type are reported whole, not by member.
            // So is any union against a type parameter (only `'T' could be instantiated ...`).
            let whole = s == TypeId::BOOLEAN
                || st.traits(s).contains(TypeTraits::ENUM_UNION)
                || tf.contains(TypeFlags::TYPE_PARAMETER);
            for &m in &members {
                if !self.related_to(m, t, rel, report && !whole, None) {
                    return false;
                }
            }
            return true;
        }
        if tf.contains(TypeFlags::UNION) {
            return self.type_related_to_some_type(s, t, rel, report);
        }
        if tf.contains(TypeFlags::INTERSECTION) {
            let outer = self.rel.in_target_member;
            let outer_skipped = std::mem::take(&mut self.rel.deferred_skipped);
            for m in st.members_of_union(t).to_vec() {
                self.rel.skip_excess = true;
                self.rel.in_target_member = true;
                let r = self.related_to(s, m, rel, report, None);
                self.rel.in_target_member = outer;
                self.rel.skip_excess = false;
                if !r {
                    self.rel.deferred_skipped |= outer_skipped;
                    return false;
                }
            }
            let skipped = std::mem::replace(&mut self.rel.deferred_skipped, outer_skipped);
            if !skipped {
                return true;
            }
            if outer {
                self.rel.deferred_skipped = true;
                return true;
            }
            // A deferred excess property or weak type check: the source's properties against
            // the intersection's combined properties (`{ x: { c } }` has nothing in common
            // with `{ a? } & { b? }`, reported under the head without a member line).
            if st
                .flags(s)
                .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
                && !self.is_generic_object_type(t)
            {
                return self.properties_related(s, t, rel, report)
                    && self.index_signatures_related(s, t, rel, report);
            }
            return true;
        }
        if sf.contains(TypeFlags::INTERSECTION) {
            let members = st.members_of_union(s).to_vec();
            // A member does not get an implicit index signature on its own (the whole
            // intersection is compared structurally next): `{ a: R } & { [x: string]: true }`
            // is not a `{ [k: string]: R }`. (Only modelled when another member declares
            // that index: tsc relates alias instantiations such as `MatchFn<R, A>` by
            // variance, which reaches the member-wise result this path keeps.)
            let tindex: Vec<TypeId> = if tf.contains(TypeFlags::OBJECT) {
                let tt = self.apparent_type(t);
                self.resolved_members(tt)
                    .index
                    .iter()
                    .filter(|i| !(i.key == TypeId::STRING && i.value == TypeId::ANY))
                    .map(|i| i.key)
                    .filter(|&k| members.iter().any(|&m| self.index_info_of(m, k).is_some()))
                    .collect()
            } else {
                Vec::new()
            };
            for &m in &members {
                if !tindex.is_empty()
                    && self.store().flags(m).contains(TypeFlags::OBJECT)
                    && tindex.iter().any(|&k| self.index_info_of(m, k).is_none())
                {
                    continue;
                }
                // A source member is related with the weak check in force (tsc's source
                // intersection state replaces the target one).
                let in_member = std::mem::take(&mut self.rel.in_target_member);
                let r = self.related_to(m, t, rel, false, None);
                self.rel.in_target_member = in_member;
                if r {
                    return true;
                }
            }
            // `T & {}` (a non-null type parameter) relates through its constraint without
            // `null` and `undefined` (`T extends string | undefined` gives `string`).
            if members.iter().any(|&m| self.is_empty_anonymous_object(m))
                && members
                    .iter()
                    .any(|&m| st.flags(m).intersects(TypeFlags::INSTANTIABLE))
            {
                let mut parts = Vec::with_capacity(members.len());
                for &m in &members {
                    if self.is_empty_anonymous_object(m) {
                        continue;
                    }
                    if st.flags(m).intersects(TypeFlags::INSTANTIABLE) {
                        let c = self.base_constraint(m);
                        if c == TypeId::UNRESOLVED {
                            return false;
                        }
                        let c = if c == TypeId::UNKNOWN {
                            TypeId::EMPTY_OBJECT
                        } else {
                            self.non_nullable(c)
                        };
                        parts.push(c);
                    } else {
                        parts.push(m);
                    }
                }
                let c = self.intersection(&parts);
                if c != s && self.related_to(c, t, rel, false, None) {
                    return true;
                }
            }
            return false;
        }
        false
    }

    fn type_related_to_some_type(
        &mut self,
        s: TypeId,
        t: TypeId,
        rel: RelationKind,
        report: bool,
    ) -> bool {
        let st = self.store();
        let members = st.members_of_union(t).to_vec();
        if members.contains(&s) {
            return true;
        }
        // An intersection with a constituent that is a member of the target union (fast
        // path for large unions such as template literal expansions).
        if st.flags(s).contains(TypeFlags::INTERSECTION)
            && st.members_of_union(s).iter().any(|m| members.contains(m))
        {
            return true;
        }
        // A fresh object literal had its excess property check against the whole union.
        let fresh = st
            .object_of(s)
            .is_some_and(|o| o.flags.contains(ObjectFlags::FRESH_LITERAL));
        for &m in &members {
            self.rel.skip_excess_props = fresh;
            if self.related_to(s, m, rel, false, None) {
                return true;
            }
        }
        self.rel.skip_excess_props = false;
        if self.related_to_discriminated_type(s, &members, rel) {
            return true;
        }
        // An object source against a union of at most three members with `null` or
        // `undefined` is matched against the other members (`X[] | X | undefined` picks `X`).
        let pick_from = if report
            && st.flags(s).contains(TypeFlags::OBJECT)
            && members.len() <= 3
            && members
                .iter()
                .any(|&m| m == TypeId::NULL || m == TypeId::UNDEFINED)
        {
            let rest: Vec<TypeId> = members
                .iter()
                .copied()
                .filter(|&m| m != TypeId::NULL && m != TypeId::UNDEFINED)
                .collect();
            if rest.len() > 1 { self.union(&rest) } else { t }
        } else {
            t
        };
        if report && let Some(best) = self.best_matching_type(s, pick_from, rel) {
            self.related_to(s, best, rel, true, None);
            // A fresh object literal's failure against the chosen member of a union with a
            // generic member (`T | X`) is not restated: `Type '{...}' is not assignable to
            // type 'X'` is dropped (observed tsc 7).
            let reg = fresh_literal_regular(self, s);
            // So is one against `X[] | X | undefined` (matched with `X`).
            let stripped_array =
                pick_from != t && members.iter().any(|&m| self.c.is_array_like_type(m));
            if st
                .object_of(s)
                .is_some_and(|o| o.flags.contains(ObjectFlags::FRESH_LITERAL))
                && (stripped_array
                    || members
                        .iter()
                        .any(|&m| st.flags(m).intersects(TypeFlags::INSTANTIABLE)))
                && let Some(r) = self.rel.report.as_mut()
                && let Some(e) = r.incompatible.last_mut()
                && e.outer
                    .is_some_and(|(ls, lt)| lt == best && (ls == s || ls == reg))
            {
                e.outer = None;
            }
        }
        false
    }

    /// An object source against a discriminated union: each combination of the source's
    /// discriminant values (`type: "a" | "b"` gives `"a"` and `"b"`, at most 25
    /// combinations) selects the members whose discriminants accept it, and the source
    /// relates to every selected member on its other properties (`{ type: "a" | "b"; x }`
    /// to `{ type: "a"; x } | { type: "b"; x }`).
    fn related_to_discriminated_type(
        &mut self,
        s: TypeId,
        members: &[TypeId],
        rel: RelationKind,
    ) -> bool {
        if matches!(rel, RelationKind::Comparable | RelationKind::Identity)
            || !self.store().flags(s).contains(TypeFlags::OBJECT)
        {
            return false;
        }
        // Object members, and intersections of object types (`Base & { kind: "a" }`).
        let objects: Vec<TypeId> = members
            .iter()
            .copied()
            .filter(|&m| {
                let st = self.store();
                st.flags(m).contains(TypeFlags::OBJECT)
                    || (st.flags(m).contains(TypeFlags::INTERSECTION)
                        && st
                            .members_of_union(m)
                            .iter()
                            .all(|&x| st.flags(x).contains(TypeFlags::OBJECT)))
            })
            .collect();
        if objects.len() < 2 {
            return false;
        }
        let mut discs: Vec<(StrId, Vec<TypeId>)> = Vec::new();
        let mut combos = 1usize;
        for (name, sp) in self.properties_of(s) {
            let mut types: Vec<TypeId> = Vec::new();
            let mut unit = false;
            for &m in &objects {
                if let Some(p) = self.property_of_type_id(m, name) {
                    let pt = self.type_of_symbol(p);
                    if self.is_literal_like(pt) || self.is_unit(pt) {
                        unit = true;
                    }
                    if !types.contains(&pt) {
                        types.push(pt);
                    }
                }
            }
            if !unit || types.len() < 2 {
                continue;
            }
            let spt = self.type_of_symbol(sp);
            let spt = self.remove_missing(spt);
            let values = self.union_members(spt);
            combos = combos.saturating_mul(values.len());
            if combos > 25 {
                return false;
            }
            discs.push((name, values));
        }
        if discs.is_empty() {
            return false;
        }
        let mut matching: Vec<TypeId> = Vec::new();
        let mut idx = vec![0usize; discs.len()];
        loop {
            let mut has_match = false;
            for &m in &objects {
                let mut ok = true;
                for (k, (name, values)) in discs.iter().enumerate() {
                    let Some(p) = self.property_of_type_id(m, *name) else {
                        ok = false;
                        break;
                    };
                    let mt = self.type_of_symbol(p);
                    if !self.is_type_related(values[idx[k]], mt, rel) {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    has_match = true;
                    if !matching.contains(&m) {
                        matching.push(m);
                    }
                }
            }
            if !has_match {
                return false;
            }
            // Next combination.
            let mut k = 0;
            loop {
                if k == idx.len() {
                    break;
                }
                idx[k] += 1;
                if idx[k] < discs[k].1.len() {
                    break;
                }
                idx[k] = 0;
                k += 1;
            }
            if k == idx.len() {
                break;
            }
        }
        let names: Vec<StrId> = discs.iter().map(|d| d.0).collect();
        for m in matching {
            let parts = if self.store().flags(m).contains(TypeFlags::INTERSECTION) {
                self.store().members_of_union(m).to_vec()
            } else {
                vec![m]
            };
            for part in parts {
                self.rel.excluded_props = names.clone();
                let ok = self.structural_related(s, part, rel, false);
                self.rel.excluded_props.clear();
                if !ok {
                    return false;
                }
            }
        }
        true
    }

    /// The union member an error should be elaborated against.
    pub(crate) fn best_matching_type(
        &mut self,
        s: TypeId,
        t: TypeId,
        rel: RelationKind,
    ) -> Option<TypeId> {
        // A member, or the members whose discriminants some source value fits (elaborated
        // again as a union).
        if let Some(d) = self.matching_discriminant_type(s, t, rel) {
            return Some(d);
        }
        let st = self.store();
        let members = st.members_of_union(t).to_vec();
        // Same generic target or alias.
        if let Some(sk) = self.reference_target(s) {
            let same: Vec<TypeId> = members
                .iter()
                .copied()
                .filter(|&m| self.reference_target(m) == Some(sk))
                .collect();
            if same.len() == 1 {
                return Some(same[0]);
            }
        }
        if let Some(sa) = st.alias_of(s)
            && let Some(m) = members
                .iter()
                .copied()
                .find(|&m| st.alias_of(m).is_some_and(|a| a.sym == sa.sym))
        {
            return Some(m);
        }
        // Object literal vs a union with array members: the non-array member.
        if st
            .object_of(s)
            .is_some_and(|o| o.flags.contains(ObjectFlags::OBJECT_LITERAL))
            && members.iter().any(|&m| self.c.is_array_like_type(m))
            && let Some(m) = members
                .iter()
                .copied()
                .find(|&m| !self.c.is_array_like_type(m))
        {
            return Some(m);
        }
        // Callable source: the single callable member.
        if !self.signatures_of(s, SigKind::Call).is_empty() {
            let callables: Vec<TypeId> = members
                .iter()
                .copied()
                .filter(|&m| !self.signatures_of(m, SigKind::Call).is_empty())
                .collect();
            if callables.len() == 1 {
                return Some(callables[0]);
            }
            if let Some(&first) = callables.first()
                && self.properties_of(s).is_empty()
            {
                return Some(first);
            }
        }
        // Most overlapping object type.
        if st
            .flags(s)
            .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
        {
            // Overlap: the member's properties the source has, directly or through an
            // index signature (observed: `{ [k: string]: unknown }` against `HeadersInit`
            // elaborates against `[string, string][]`, the member with the most
            // properties; a source sharing no property with any member is not elaborated).
            let sprops: Vec<StrId> = self.properties_of(s).into_iter().map(|p| p.0).collect();
            let s_string_index = self.index_info_of(s, TypeId::STRING).is_some();
            let s_number_index = s_string_index || self.index_info_of(s, TypeId::NUMBER).is_some();
            let mut best: Option<(usize, TypeId)> = None;
            for &m in &members {
                if !self
                    .store()
                    .flags(m)
                    .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
                {
                    continue;
                }
                let mprops: Vec<StrId> = self.properties_of(m).into_iter().map(|p| p.0).collect();
                let numeric = |ck: &Self, n: StrId| ck.store().str(n).parse::<f64>().is_ok();
                // The member's properties the source has (directly or by its index
                // signatures), then the source's other properties the member's index
                // signatures accept.
                let mut overlap = mprops
                    .iter()
                    .filter(|&&n| {
                        sprops.contains(&n) || s_string_index || s_number_index && numeric(self, n)
                    })
                    .count();
                let m_string_index = self.index_info_of(m, TypeId::STRING).is_some();
                let m_number_index =
                    m_string_index || self.index_info_of(m, TypeId::NUMBER).is_some();
                if m_number_index {
                    overlap += sprops
                        .iter()
                        .filter(|&&n| !mprops.contains(&n) && (m_string_index || numeric(self, n)))
                        .count();
                }
                if overlap > 0 && best.is_none_or(|b| overlap >= b.0) {
                    best = Some((overlap, m));
                }
            }
            if let Some((_, m)) = best {
                return Some(m);
            }
            // Only one object-like member.
        }
        None
    }

    fn reference_target(&self, t: TypeId) -> Option<TypeId> {
        match self.store().object_of(t).map(|o| o.kind) {
            Some(ObjectKind::Reference { target, .. }) => Some(target),
            Some(ObjectKind::Declared { .. }) => Some(t),
            _ => None,
        }
    }

    /// Union members selected by the source's discriminant properties.
    pub fn matching_discriminant_type(
        &mut self,
        s: TypeId,
        t: TypeId,
        rel: RelationKind,
    ) -> Option<TypeId> {
        self.discriminant_members(s, t, rel, true)
    }

    /// [`Ck::matching_discriminant_type`] as excess property checks see it: a discriminant
    /// whose value no member accepts selects nothing (`{ a: null }` against
    /// `W[] | { a?: C | boolean }` checks excess properties against the whole union).
    fn excess_discriminant_type(
        &mut self,
        s: TypeId,
        t: TypeId,
        rel: RelationKind,
    ) -> Option<TypeId> {
        self.discriminant_members(s, t, rel, true)
    }

    fn discriminant_members(
        &mut self,
        s: TypeId,
        t: TypeId,
        rel: RelationKind,
        unmatched_keeps_all: bool,
    ) -> Option<TypeId> {
        let st = self.store();
        if !st.flags(t).contains(TypeFlags::UNION)
            || !st
                .flags(s)
                .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
        {
            return None;
        }
        let members = st.members_of_union(t).to_vec();
        let sprops = self.properties_of(s);
        let mut candidates: Vec<bool> = vec![true; members.len()];
        let mut discriminated = false;
        for (name, sp) in sprops {
            // A discriminant: present in some member with a unit type.
            let mut is_disc = false;
            let mut member_types: Vec<Option<TypeId>> = Vec::with_capacity(members.len());
            for &m in &members {
                let mt = match self.property_of_type_id(m, name) {
                    Some(p) => {
                        let pt = self.type_of_symbol(p);
                        if self.is_literal_like(pt) {
                            is_disc = true;
                        }
                        Some(pt)
                    }
                    None => None,
                };
                member_types.push(mt);
            }
            if !is_disc {
                continue;
            }
            let spt = self.type_of_symbol(sp);
            let spt = self.store().regular(spt);
            if !self.is_unit(spt) && !self.store().flags(spt).contains(TypeFlags::UNION) {
                continue;
            }
            discriminated = true;
            let svals = self.union_members(spt);
            // Per member: `Some(fits)` where it has the property, `None` where it lacks it.
            let mut fits: Vec<Option<bool>> = Vec::with_capacity(members.len());
            let mut matched = false;
            for (i, mt) in member_types.into_iter().enumerate() {
                // Some value of the source's discriminant fits the member's.
                let f = mt.map(|mt| svals.iter().any(|&v| self.is_type_related(v, mt, rel)));
                matched |= candidates[i] && f == Some(true);
                fits.push(f);
            }
            for (i, f) in fits.into_iter().enumerate() {
                // A member without the property stays a candidate (observed:
                // `{ mode: "numeric", data: u8 }` against `{ mode: "numeric"; data:
                // number } | { data: string | Uint8Array }` is assignable). For excess
                // checks a discriminant no member accepts selects nothing.
                let keep = f != Some(false) || (unmatched_keeps_all && !matched);
                if !keep {
                    candidates[i] = false;
                }
            }
        }
        if !discriminated {
            return None;
        }
        let picked: Vec<TypeId> = members
            .iter()
            .zip(candidates.iter())
            .filter(|(_, c)| **c)
            .map(|(m, _)| *m)
            .collect();
        if picked.is_empty() || picked.len() == members.len() {
            return None;
        }
        if picked.len() == 1 {
            return Some(picked[0]);
        }
        Some(self.union_with(
            &picked,
            crate::unions::Reduction::None,
            ts_r_types::AliasId::NONE,
        ))
    }

    /// A deferred conditional source against a target without type parameters: related
    /// when its constraint (distributive, else the union of its branches) or the union of
    /// its branches is. `None` when not applicable or the constraint is not trusted (one
    /// with type parameters: branches are not narrowed by the `extends` type here).
    fn conditional_source_related(
        &mut self,
        s: TypeId,
        t: TypeId,
        rel: RelationKind,
        report: bool,
    ) -> Option<bool> {
        let st = self.store();
        if !st.flags(s).contains(TypeFlags::CONDITIONAL)
            || st.flags(t).intersects(TypeFlags::CONDITIONAL)
            || st.traits(t).contains(TypeTraits::CONTAINS_TYPE_PARAMETER)
        {
            return None;
        }
        let opaque = |ck: &mut Self, x: TypeId| {
            x == s
                || x == TypeId::UNRESOLVED
                || x == TypeId::WILDCARD
                || ck.store().is_error(x)
                || ck.is_generic_type(x)
        };
        let c = self.conditional_constraint(s)?;
        if opaque(self, c) {
            return None;
        }
        if self.related_to(c, t, rel, false, None) {
            return Some(true);
        }
        let mut b = self.conditional_branches_union(s)?;
        // Generic branches relate through their constraints (`E extends null ? E : E` with
        // `E extends { a: 1 }` is a `{ a: 1 }`).
        if opaque(self, b) && b != s {
            let bc = self.base_constraint(b);
            if bc != b && !opaque(self, bc) {
                b = bc;
            }
        }
        if opaque(self, b) {
            return None;
        }
        if b != c && self.related_to(b, t, rel, false, None) {
            return Some(true);
        }
        if report {
            self.related_to(c, t, rel, true, None);
        }
        Some(false)
    }

    fn structured_related(
        &mut self,
        s: TypeId,
        t: TypeId,
        rel: RelationKind,
        report: bool,
    ) -> bool {
        let st = self.store();
        let sf = st.flags(s);
        let tf = st.flags(t);
        if rel == RelationKind::Identity {
            return self.identity_structured(s, t);
        }
        if tf.contains(TypeFlags::UNION)
            && let Some(r) = self.conditional_source_related(s, t, rel, report)
        {
            return r;
        }
        // A generic source against a union target: no member matched as is, so its
        // constraint is compared with the whole union (`keyof T` to `string | number`).
        if tf.contains(TypeFlags::UNION)
            && sf.intersects(
                TypeFlags::TYPE_PARAMETER | TypeFlags::INDEX | TypeFlags::INDEXED_ACCESS,
            )
        {
            if self.union_or_intersection_related(s, t, rel, false) {
                return true;
            }
            let c = self.base_constraint(s);
            if c != s
                && c != TypeId::UNRESOLVED
                && c != TypeId::UNKNOWN
                && self.related_to(c, t, rel, false, None)
            {
                return true;
            }
            // The failure is explained by the constraint (`Type 'string' is not assignable
            // to type 'number | boolean'` under `Type 'V' ...`), a type parameter's by its
            // declared constraint (`T | Top | U`).
            let c = if sf.contains(TypeFlags::TYPE_PARAMETER) {
                self.type_param_constraint(s).unwrap_or(c)
            } else {
                c
            };
            let c = self.without_reference_alias(c);
            if report
                && c != s
                && c != TypeId::UNRESOLVED
                && c != TypeId::UNKNOWN
                && !self.store().is_error(c)
                && !self.is_type_related(s, t, rel)
            {
                self.related_to(c, t, rel, true, None);
                return false;
            }
        }
        // An intersection with a deferred conditional member also relates through its
        // constraint (`{ url } & TO<X>` with `TO<X>` constrained to `Yes | No`, against
        // `{ url } & (Yes | No)`).
        if sf.contains(TypeFlags::INTERSECTION)
            && !st.traits(t).contains(TypeTraits::CONTAINS_TYPE_PARAMETER)
            && st
                .members_of_union(s)
                .iter()
                .any(|&m| st.flags(m).contains(TypeFlags::CONDITIONAL))
        {
            let c = self.base_constraint(s);
            if c != s
                && c != TypeId::UNRESOLVED
                && !self.store().is_error(c)
                && !self.is_generic_type(c)
                && self.related_to(c, t, rel, false, None)
            {
                return true;
            }
        }
        if sf.intersects(TypeFlags::UNION_OR_INTERSECTION)
            || tf.intersects(TypeFlags::UNION_OR_INTERSECTION)
        {
            let r = self.union_or_intersection_related(
                s,
                t,
                rel,
                report
                    && !(sf.contains(TypeFlags::INTERSECTION) && tf.intersects(TypeFlags::OBJECT)),
            );
            if r {
                return true;
            }
            // A generic intersection against a union relates through its constraint
            // (`O & N` with both constrained to `A | B`).
            if sf.contains(TypeFlags::INTERSECTION)
                && tf.contains(TypeFlags::UNION)
                && self.is_generic_type(s)
            {
                let c = self.base_constraint(s);
                if c != s
                    && c != TypeId::UNRESOLVED
                    && !self.store().is_error(c)
                    && self.related_to(c, t, rel, false, None)
                {
                    return true;
                }
            }
            // A source intersection may still be structurally related to an object target.
            if !(sf.contains(TypeFlags::INTERSECTION) && tf.contains(TypeFlags::OBJECT)) {
                return false;
            }
        }
        // Non-generic template literal and string mapping targets match by pattern.
        if tf.intersects(TypeFlags::TEMPLATE_LITERAL | TypeFlags::STRING_MAPPING)
            && let Some(r) = self.template_target_related(s, t)
        {
            if !r && report {
                self.report_pattern_source_constraint(s, t, rel);
            }
            return r;
        }
        // Non-generic template literal and string mapping sources relate as `string`.
        if sf.intersects(TypeFlags::TEMPLATE_LITERAL | TypeFlags::STRING_MAPPING)
            && !st.traits(s).contains(TypeTraits::CONTAINS_TYPE_PARAMETER)
            && !tf.intersects(TypeFlags::INSTANTIABLE)
        {
            let r = self.related_to(TypeId::STRING, t, rel, false, None);
            if !r && report {
                self.report_pattern_source_constraint(s, t, rel);
            }
            return r;
        }
        // A deferred conditional target without `infer`: the source must relate to each
        // branch that can be taken.
        if tf.contains(TypeFlags::CONDITIONAL)
            && !sf.intersects(TypeFlags::CONDITIONAL | TypeFlags::ANY)
            && let Some((a, b)) = self.conditional_target_branches(s, t)
        {
            // Excess properties were checked against the conditional type as a whole.
            return a.is_none_or(|a| {
                self.rel.skip_excess = true;
                self.related_to(s, a, rel, false, None)
            }) && b.is_none_or(|b| {
                self.rel.skip_excess = true;
                self.related_to(s, b, rel, false, None)
            });
        }
        // A concrete source is a subtype of no generic `T[K]` (only assignable to it, through
        // `T`'s constraint): type predicates intersect `T["ctx"]` with the candidate.
        if tf.contains(TypeFlags::INDEXED_ACCESS)
            && matches!(rel, RelationKind::Subtype | RelationKind::StrictSubtype)
            && !st.traits(s).contains(TypeTraits::CONTAINS_TYPE_PARAMETER)
            && !sf.intersects(TypeFlags::INSTANTIABLE)
        {
            return false;
        }
        if let Some(r) = self.conditional_source_related(s, t, rel, report) {
            return r;
        }
        // A conditional against a generic `keyof X` relates through its branches
        // (`V extends U ? keyof L<V> : number` is not a key of `L<V>`).
        if sf.contains(TypeFlags::CONDITIONAL)
            && tf.contains(TypeFlags::INDEX)
            && !self.conditional_has_infer(s)
            && let Some(b) = self.conditional_branches_union_narrowed(s)
            && b != s
            && b != TypeId::UNRESOLVED
            && b != TypeId::WILDCARD
            && !self.store().is_error(b)
        {
            return self.related_to(b, t, rel, report, None);
        }
        // Deferred generic forms we do not model: be permissive.
        if tf.intersects(
            TypeFlags::INDEXED_ACCESS
                | TypeFlags::CONDITIONAL
                | TypeFlags::SUBSTITUTION
                | TypeFlags::TEMPLATE_LITERAL
                | TypeFlags::STRING_MAPPING,
        ) || sf.intersects(
            TypeFlags::CONDITIONAL | TypeFlags::SUBSTITUTION | TypeFlags::TEMPLATE_LITERAL,
        ) {
            return true;
        }
        // A generic mapped type over other keys than `keyof T` is not a `T` (`{ [K in keyof T
        // & keyof X]: T[K] }` may lack properties of `T`).
        if tf.contains(TypeFlags::TYPE_PARAMETER)
            && sf.contains(TypeFlags::OBJECT)
            && self.is_generic_mapped_type(s)
            && let Some(info) = self.mapped_info(s)
            && info.name_node.is_none()
        {
            let c = self.mapped_constraint(&info);
            let st = self.store();
            let keyed = |x: TypeId| matches!(st.kind(x), TypeKind::Index(o) if o == t);
            let parts: Vec<TypeId> = if st.flags(c).contains(TypeFlags::UNION) {
                st.members_of_union(c).to_vec()
            } else {
                vec![c]
            };
            if parts.iter().all(|&p| {
                st.flags(p).contains(TypeFlags::INTERSECTION)
                    && st.members_of_union(p).iter().any(|&m| keyed(m))
            }) {
                return false;
            }
        }
        // Generic mapped types are opaque.
        if (sf.contains(TypeFlags::OBJECT) && self.is_generic_mapped_type(s))
            || (tf.contains(TypeFlags::OBJECT) && self.is_generic_mapped_type(t))
        {
            return true;
        }
        // `{ [K in C]: X }[C]` relates through `X` with `K` replaced by `C`.
        if sf.contains(TypeFlags::INDEXED_ACCESS)
            && let Some(sub) = self.indexed_mapped_substitution(s)
            && sub != s
            && self.related_to(sub, t, rel, false, None)
        {
            return true;
        }
        if tf.contains(TypeFlags::INDEX) {
            // A type parameter constrained (through other parameters) to the target:
            // `K in Keys` with `Keys extends keyof T` against `keyof T`, also through a
            // conditional (`A extends Exclude<keyof T, "type">`).
            if sf.contains(TypeFlags::TYPE_PARAMETER)
                && let Some(c) = self.type_param_constraint(s)
                && c != s
                && self.store().flags(c).intersects(
                    TypeFlags::TYPE_PARAMETER
                        | TypeFlags::INDEX
                        | TypeFlags::CONDITIONAL
                        | TypeFlags::INDEXED_ACCESS,
                )
                && self.related_to(c, t, rel, false, None)
            {
                return true;
            }
            // `U[number]` with `U extends readonly (keyof T)[]` against `keyof T`: the element
            // of the object's own constraint.
            if sf.contains(TypeFlags::INDEXED_ACCESS)
                && let TypeKind::IndexedAccess { obj, index } = st.kind(s)
                && self.store().flags(obj).contains(TypeFlags::TYPE_PARAMETER)
                && let Some(oc) = self.type_param_constraint(obj)
                && oc != obj
                && let Some(c) = self.indexed_access_type(oc, index, None)
                && c != s
                && c != TypeId::UNRESOLVED
                && self.related_to(c, t, rel, false, None)
            {
                return true;
            }
            if let TypeKind::Index(x) = st.kind(t) {
                // `keyof T` accepts keys of T's constraint only when T is generic; be permissive
                // for literal keys of the constraint.
                let c = self.base_constraint(x);
                if c == TypeId::UNRESOLVED {
                    // A constraint we can't compute: no error.
                    return true;
                }
                if c == TypeId::UNKNOWN {
                    return sf.contains(TypeFlags::INDEX)
                        && matches!(st.kind(s), TypeKind::Index(y) if y == x);
                }
                if sf.contains(TypeFlags::INDEX) {
                    return true;
                }
                // `S` is assignable to `keyof T` when it is assignable to `keyof C`, `C` being
                // T's constraint (a string index signature contributes `string` only).
                let mut kc = if self.store().flags(c).contains(TypeFlags::ANY) {
                    self.keyof_constraint()
                } else {
                    self.keyof_type(c)
                };
                if self
                    .index_info_of(c, TypeId::STRING)
                    .is_some_and(|i| i.key == TypeId::STRING)
                    && self.store().flags(kc).contains(TypeFlags::UNION)
                {
                    let parts: Vec<TypeId> = self
                        .store()
                        .members_of_union(kc)
                        .iter()
                        .copied()
                        .filter(|&m| m != TypeId::NUMBER)
                        .collect();
                    kc = self.union_with(
                        &parts,
                        crate::unions::Reduction::Literal,
                        ts_r_types::AliasId::NONE,
                    );
                }
                if kc == t || self.store().flags(kc).intersects(TypeFlags::INDEX) {
                    return false;
                }
                return self.related_to(s, kc, rel, report, None);
            }
            return true;
        }
        if tf.contains(TypeFlags::TYPE_PARAMETER) {
            // Only the same type parameter (identity handled earlier) or `never`.
            if sf.contains(TypeFlags::TYPE_PARAMETER) {
                // A type parameter constrained to another type parameter.
                let mut cur = s;
                for _ in 0..20 {
                    match self.type_param_constraint(cur) {
                        Some(c) if c == t => return true,
                        Some(c) if self.store().flags(c).contains(TypeFlags::TYPE_PARAMETER) => {
                            cur = c
                        }
                        _ => break,
                    }
                }
            }
            if sf.contains(TypeFlags::INDEXED_ACCESS) {
                return true;
            }
            return false;
        }
        if sf.intersects(TypeFlags::INSTANTIABLE) {
            let c = self.base_constraint(s);
            if c == TypeId::UNRESOLVED {
                return true;
            }
            if c == TypeId::UNKNOWN {
                // Unconstrained: assignable to `{}` only without strictNullChecks.
                if !self.c.opts.strict_null_checks
                    && tf.contains(TypeFlags::OBJECT)
                    && self.is_empty_object_type(t)
                {
                    return true;
                }
                // Comparable through its `unknown` constraint: anything is assignable to
                // `unknown`, so an unconstrained `T` compares with any non-generic type.
                if rel == RelationKind::Comparable
                    && sf.contains(TypeFlags::TYPE_PARAMETER)
                    && !tf.intersects(TypeFlags::INSTANTIABLE | TypeFlags::NEVER)
                {
                    return true;
                }
                return false;
            }
            if c == s {
                return false;
            }
            // Reporting goes one constraint at a time (`U extends T`, `T extends () => void`:
            // "Type 'T' is not assignable", then the function type).
            if report
                && sf.contains(TypeFlags::TYPE_PARAMETER)
                && let Some(dc) = self.type_param_constraint(s)
                && dc != c
                && dc != s
                && self.store().flags(dc).contains(TypeFlags::TYPE_PARAMETER)
            {
                return self.related_to(dc, t, rel, report, None);
            }
            // A reference constraint relates without its alias (`T extends NB` with
            // `type NB = Box<number>` elaborates `Box<number>`).
            let c = self.without_reference_alias(c);
            return self.related_to(c, t, rel, report, None);
        }
        if sf.contains(TypeFlags::INDEX) {
            let kc = self.keyof_constraint();
            return self.related_to(kc, t, rel, report, None);
        }
        if !tf.contains(TypeFlags::OBJECT) {
            return false;
        }
        // Primitives: compare their apparent (wrapper) types structurally. tsc reports no
        // elaboration below a primitive source.
        // `object` compares as its apparent type `{}` (with `Object`'s members).
        let primitive_source = sf.intersects(TypeFlags::PRIMITIVE | TypeFlags::NON_PRIMITIVE)
            && !sf.contains(TypeFlags::UNION);
        let non_primitive = sf.contains(TypeFlags::NON_PRIMITIVE) && !sf.contains(TypeFlags::UNION);
        let report = report && (!primitive_source || non_primitive);
        // Under the strict subtype relation a primitive is not a subtype of `Object` (it is of
        // `{}`): `((x: number) => void) | ((x: Object) => void)` does not reduce.
        if primitive_source
            && !non_primitive
            && rel == RelationKind::StrictSubtype
            && self.global_type(GlobalType::Object) == Some(t)
        {
            return false;
        }
        // A primitive never matches a target index signature through its wrapper type, and
        // `object` (apparent type `{}`) has no implicit index signature; only a string
        // index of `any` accepts `object`.
        if primitive_source {
            let tt = self.apparent_type(t);
            if self.store().flags(tt).contains(TypeFlags::OBJECT) {
                let tindex = self.resolved_members(tt).index.to_vec();
                for ti in tindex {
                    if non_primitive && ti.key == TypeId::STRING && ti.value == TypeId::ANY {
                        continue;
                    }
                    if non_primitive {
                        if report {
                            let k = self.type_to_string(ti.key);
                            self.report_error(
                                dg::INDEX_SIGNATURE_FOR_TYPE_0_IS_MISSING_IN_TYPE_1_2329,
                                &[&k, "{}"],
                            );
                        }
                        return false;
                    }
                    if ti.key == TypeId::STRING && ti.value == TypeId::ANY {
                        return false;
                    }
                }
            }
        }
        if non_primitive {
            // Compared as `{}`; the enclosing "Type 'object' is not assignable" head stays
            // (a missing property does not replace it, the types differ).
            let saved = self.rel.report.as_ref().map(|r| r.suppress.len());
            let a = self.apparent_type(s);
            let r = a != s && self.structured_related(a, t, rel, report);
            if let (Some(rep), Some(v)) = (self.rel.report.as_mut(), saved) {
                rep.suppress.truncate(v);
            }
            return r;
        }
        let s = if primitive_source {
            let a = self.apparent_type(s);
            if a == s {
                return false;
            }
            a
        } else {
            s
        };
        let sf = self.store().flags(s);
        if !sf.intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION) {
            return false;
        }
        // An interface that only extends one base (`interface A extends Array<Number> {}`)
        // relates as that base to another instantiation of the base's generic type.
        if let Some((tt, _)) = self.generic_args(t)
            && let Some(b) = self.plain_extension_base(s)
            && self.generic_args(b).is_some_and(|x| x.0 == tt)
        {
            return self.structured_related(b, t, rel, report);
        }
        if let Some((st_, _)) = self.generic_args(s)
            && let Some(b) = self.plain_extension_base(t)
            && self.generic_args(b).is_some_and(|x| x.0 == st_)
        {
            return self.structured_related(s, b, rel, report);
        }
        // Same generic target: compare type arguments.
        if let (Some(sa), Some(ta)) = (self.generic_args(s), self.generic_args(t))
            && sa.0 == ta.0
        {
            let key = crate::variance::VarianceKey::Type(sa.0);
            if let Some(r) = self.related_by_variances(s, t, key, &sa.1, &ta.1, rel, report) {
                return r;
            }
            let ok = self.type_args_related(&sa.1, &ta.1, rel, false);
            if ok {
                return true;
            }
            // Variance is not measured: fall back to structure before failing.
            let structural = self.structural_related(s, t, rel, false);
            if structural {
                return true;
            }
            if report {
                self.type_args_related(&sa.1, &ta.1, rel, true);
            }
            return false;
        }
        // Same alias with different arguments.
        if let (Some(a1), Some(a2)) = (self.store().alias_of(s), self.store().alias_of(t))
            && a1.sym == a2.sym
            && a1.args != ListId::EMPTY
        {
            let sa = self.store().list_items(a1.args).to_vec();
            let ta = self.store().list_items(a2.args).to_vec();
            let key = crate::variance::VarianceKey::Alias(a1.sym);
            if let Some(r) = self.related_by_variances(s, t, key, &sa, &ta, rel, report) {
                return r;
            }
            if self.type_args_related(&sa, &ta, rel, false) {
                return true;
            }
            if self.structural_related(s, t, rel, false) {
                return true;
            }
            if report {
                self.type_args_related(&sa, &ta, rel, true);
            }
            return false;
        }
        self.structural_related(s, t, rel, report)
    }

    /// Elaborates a failed non-generic template literal or string mapping source (observed
    /// tsc 7): a string mapping through `string` (against a mapping of its own kind, through
    /// the mapped types; against another kind, not at all); a template literal through the
    /// template with its mapping holes widened to `string`, unless the target is a pattern
    /// too or nothing was widened.
    fn report_pattern_source_constraint(&mut self, s: TypeId, t: TypeId, rel: RelationKind) {
        if !self.is_pattern_literal_type(s) {
            return;
        }
        let tf = self.store().flags(t);
        match self.store().kind(s) {
            TypeKind::StringMapping { sym, ty } => {
                if tf.contains(TypeFlags::STRING_MAPPING) {
                    if let TypeKind::StringMapping { sym: b, ty: y } = self.store().kind(t)
                        && b == sym
                    {
                        self.related_to(ty, y, rel, true, None);
                    }
                    return;
                }
                self.related_to(TypeId::STRING, t, rel, true, None);
            }
            TypeKind::TemplateLiteral(_) => {
                if tf.intersects(TypeFlags::TEMPLATE_LITERAL | TypeFlags::STRING_MAPPING) {
                    return;
                }
                let c = self.template_mapping_constraint(s);
                if c != s {
                    self.related_to(c, t, rel, true, None);
                }
            }
            _ => {}
        }
    }

    /// Two instantiations of one generic type related by its measured variances (`None`
    /// without a measurement: the caller compares the arguments covariantly, then
    /// structure).
    #[allow(clippy::too_many_arguments)]
    fn related_by_variances(
        &mut self,
        s: TypeId,
        t: TypeId,
        key: crate::variance::VarianceKey,
        sa: &[TypeId],
        ta: &[TypeId],
        rel: RelationKind,
        report: bool,
    ) -> Option<bool> {
        // Marker instantiations of a measurement compare structurally; other instantiations
        // of a type whose variances are being measured are "maybe related".
        if self.is_marker_reference(s) || self.is_marker_reference(t) {
            return Some(self.structural_related(s, t, rel, report));
        }
        if rel == RelationKind::Identity {
            return None;
        }
        if self.variance_in_progress(key) {
            self.rel.maybe_used = true;
            self.rel.assumed.unknown = true;
            return Some(true);
        }
        let vs = self.variances_of(key)?;
        if vs.list.len() != sa.len() || sa.len() != ta.len() {
            return None;
        }
        if self.type_args_related_by_variance(sa, ta, &vs.list, rel, false) {
            return Some(true);
        }
        // When the measurement is unreliable (or a covariant argument is `void`), structure
        // decides what the arguments reject.
        if vs.unreliable || self.covariant_void_argument(ta, &vs.list) {
            let saved = self.rel.report.take();
            let structural = self.structural_related(s, t, rel, false);
            self.rel.report = saved;
            if structural {
                return Some(true);
            }
            if report {
                // A reliable measurement still explains the failure by the arguments
                // (`IteratorReturnResult<State>` to `IteratorReturnResult<void>`).
                if vs.unreliable {
                    self.structural_related(s, t, rel, true);
                } else {
                    self.type_args_related_by_variance(sa, ta, &vs.list, rel, true);
                }
            }
            return Some(false);
        }
        if report {
            // An invariant parameter is explained structurally when structure fails too
            // (tsc's elaboration); otherwise by the arguments.
            let invariant = vs.list.contains(&crate::variance::Variance::Invariant);
            let saved = self.rel.report.take();
            let structural = invariant && self.structural_related(s, t, rel, false);
            self.rel.report = saved;
            if invariant && !structural {
                self.structural_related(s, t, rel, true);
            } else {
                self.type_args_related_by_variance(sa, ta, &vs.list, rel, true);
            }
        }
        Some(false)
    }

    /// The single base of a non-generic interface that declares no members of its own.
    fn plain_extension_base(&mut self, t: TypeId) -> Option<TypeId> {
        let ObjectKind::Declared {
            sym, type_params, ..
        } = self.store().object_of(t)?.kind
        else {
            return None;
        };
        if type_params != ListId::EMPTY {
            return None;
        }
        let decls = self.c.decls(sym);
        if decls.is_empty() {
            return None;
        }
        for d in decls {
            let ast = self.ast(d.file);
            match *ast.node(d.node) {
                Node::Interface { body, .. } if ast.list(body).is_empty() => {}
                _ => return None,
            }
        }
        match self.base_types(sym)[..] {
            [b] => Some(b),
            _ => None,
        }
    }

    /// `(target, args)` of a class/interface reference (arguments without `this`).
    pub(crate) fn generic_args(&self, t: TypeId) -> Option<(TypeId, Vec<TypeId>)> {
        let st = self.store();
        match st.object_of(t)?.kind {
            ObjectKind::Reference { target, .. } => {
                if st.tuple_shape_of(target).is_some() {
                    return None;
                }
                Some((target, self.c.type_args_of(t).to_vec()))
            }
            _ => None,
        }
    }

    /// A covariant `void` target argument lets structure decide (`Promise<number>` to
    /// `Promise<void>` through `then`'s callbacks).
    fn covariant_void_argument(&self, ta: &[TypeId], vs: &[crate::variance::Variance]) -> bool {
        ta.iter().zip(vs).any(|(&t, &v)| {
            v == crate::variance::Variance::Covariant
                && self.store().flags(t).contains(TypeFlags::VOID)
        })
    }

    fn type_args_related_by_variance(
        &mut self,
        sa: &[TypeId],
        ta: &[TypeId],
        vs: &[crate::variance::Variance],
        rel: RelationKind,
        report: bool,
    ) -> bool {
        use crate::variance::Variance;
        for ((&a, &b), &v) in sa.iter().zip(ta.iter()).zip(vs) {
            let ok = match v {
                Variance::Independent => true,
                Variance::Covariant => self.related_to(a, b, rel, report, None),
                Variance::Contravariant => self.related_to(b, a, rel, report, None),
                Variance::Bivariant => {
                    self.related_to(b, a, rel, false, None)
                        || self.related_to(a, b, rel, report, None)
                }
                Variance::Invariant => {
                    self.related_to(a, b, rel, report, None)
                        && self.related_to(b, a, rel, report, None)
                }
            };
            if !ok {
                return false;
            }
        }
        true
    }

    fn type_args_related(
        &mut self,
        sa: &[TypeId],
        ta: &[TypeId],
        rel: RelationKind,
        report: bool,
    ) -> bool {
        for (i, (&a, &b)) in sa.iter().zip(ta.iter()).enumerate() {
            let _ = i;
            if !self.related_to(a, b, rel, report, None) {
                return false;
            }
        }
        true
    }

    fn identity_structured(&mut self, s: TypeId, t: TypeId) -> bool {
        let st = self.store();
        let sf = st.flags(s);
        let tf = st.flags(t);
        if sf.intersects(TypeFlags::UNION_OR_INTERSECTION)
            && tf.intersects(TypeFlags::UNION_OR_INTERSECTION)
        {
            // Each member identical to some member of the other side, both ways
            // (`{ a } | { a, b } | { b, a }` is identical to `{ a } | { a, b }`).
            let a = st.members_of_union(s).to_vec();
            let b = st.members_of_union(t).to_vec();
            return a.iter().all(|&x| {
                b.iter()
                    .any(|&y| self.related_to(x, y, RelationKind::Identity, false, None))
            }) && b.iter().all(|&y| {
                a.iter()
                    .any(|&x| self.related_to(y, x, RelationKind::Identity, false, None))
            });
        }
        if sf.contains(TypeFlags::OBJECT) && tf.contains(TypeFlags::OBJECT) {
            return self.structural_related(s, t, RelationKind::Identity, false);
        }
        false
    }

    /// Properties, signatures and index signatures.
    fn structural_related(
        &mut self,
        s: TypeId,
        t: TypeId,
        rel: RelationKind,
        report: bool,
    ) -> bool {
        // Arrays and tuples.
        let st = self.store();
        let t_tuple = st.tuple_shape_of(t).is_some();
        let s_arr = self.c.is_array_like_type(s);
        if t_tuple && s_arr {
            return self.tuple_related(s, t, rel, report);
        }
        if self.c.is_array_type(t) && s_arr && rel != RelationKind::Identity {
            let t_ro = self.is_readonly_array_like(t);
            let s_ro = self.is_readonly_array_like(s);
            if s_ro && !t_ro {
                if report {
                    let (ss, ts) = self.type_names_for_error(s, t);
                    self.report_error(dg::THE_TYPE_0_IS_READONLY_AND_CANNOT_BE_ASSIGNED_TO_THE_MUTABLE_TYPE_1_4104, &[&ss, &ts]);
                    self.suppress_next_head();
                }
                return false;
            }
            if self.c.is_tuple_type(s) || t_ro {
                let se = self.element_type_of_array_like(s).unwrap_or(TypeId::ANY);
                let te = self.element_type_of_array_like(t).unwrap_or(TypeId::ANY);
                return self.related_to(se, te, rel, report, None);
            }
        }
        if !self.properties_related(s, t, rel, report) {
            return false;
        }
        if !self.signatures_related(s, t, SigKind::Call, rel, report) {
            return false;
        }
        if !self.signatures_related(s, t, SigKind::Construct, rel, report) {
            return false;
        }
        self.index_signatures_related(s, t, rel, report)
    }

    fn tuple_related(&mut self, s: TypeId, t: TypeId, rel: RelationKind, report: bool) -> bool {
        let st = self.store();
        let tshape = st
            .tuple_shape_of(t)
            .cloned()
            .unwrap_or_else(|| ts_r_types::TupleShape {
                elements: Box::new([]),
                readonly: false,
            });
        let targs = self.c.type_args_of_tuple(t).to_vec();
        let (sshape, sargs) = match st.tuple_shape_of(s) {
            Some(sh) => (sh.clone(), self.c.type_args_of_tuple(s).to_vec()),
            None => {
                let e = self.element_type_of_array_like(s).unwrap_or(TypeId::ANY);
                (
                    ts_r_types::TupleShape {
                        elements: Box::new([TupleElement {
                            kind: TupleElement::REST,
                            label: None,
                        }]),
                        readonly: self.is_readonly_array_like(s),
                    },
                    vec![e],
                )
            }
        };
        if sshape.readonly && !tshape.readonly {
            if report {
                let (ss, ts) = self.type_names_for_error(s, t);
                self.report_error(
                    dg::THE_TYPE_0_IS_READONLY_AND_CANNOT_BE_ASSIGNED_TO_THE_MUTABLE_TYPE_1_4104,
                    &[&ss, &ts],
                );
                self.suppress_next_head();
            }
            return false;
        }
        if sshape
            .elements
            .iter()
            .any(|e| e.kind == TupleElement::VARIADIC)
            || tshape
                .elements
                .iter()
                .any(|e| e.kind == TupleElement::VARIADIC)
        {
            return true;
        }
        let s_arity = sshape.elements.len();
        let t_arity = tshape.elements.len();
        let s_rest = sshape.has_rest();
        let t_rest = tshape.has_rest();
        let s_min = sshape.min_length();
        let t_min = tshape.min_length();
        if !s_rest && s_arity < t_min {
            if report {
                self.report_error(
                    dg::SOURCE_HAS_0_ELEMENT_S_BUT_TARGET_REQUIRES_1_2618,
                    &[&s_arity.to_string(), &t_min.to_string()],
                );
            }
            return false;
        }
        if !t_rest && t_arity < s_min {
            if report {
                self.report_error(
                    dg::SOURCE_HAS_0_ELEMENT_S_BUT_TARGET_ALLOWS_ONLY_1_2619,
                    &[&s_min.to_string(), &t_arity.to_string()],
                );
            }
            return false;
        }
        if !t_rest && (s_rest || t_arity < s_arity) {
            if report {
                if s_min < t_min {
                    self.report_error(
                        dg::TARGET_REQUIRES_0_ELEMENT_S_BUT_SOURCE_MAY_HAVE_FEWER_2620,
                        &[&t_min.to_string()],
                    );
                } else {
                    self.report_error(
                        dg::TARGET_ALLOWS_ONLY_0_ELEMENT_S_BUT_SOURCE_MAY_HAVE_MORE_2621,
                        &[&t_arity.to_string()],
                    );
                }
            }
            return false;
        }
        // A target rest element followed by fixed elements (`[...X[], Y]`): trailing source
        // elements pair with the trailing target elements.
        let t_rest_at = tshape
            .elements
            .iter()
            .position(|e| e.kind == TupleElement::REST);
        let t_suffix = t_rest_at.map_or(0, |r| t_arity - r - 1);
        // An open-ended source (an array, or a tuple with a rest element) cannot provide the
        // target's required elements past its own minimum length (observed: `string[]` and
        // even `any[]` against `[T, ...T[]]`).
        if s_rest && t_rest && s_min < t_min {
            if report {
                let t_prefix = t_rest_at.map_or(t_arity, |r| r);
                let t_prefix_min = tshape.elements[..t_prefix]
                    .iter()
                    .filter(|e| e.kind == TupleElement::REQUIRED)
                    .count();
                let pos = if t_prefix_min > s_min {
                    s_min
                } else {
                    t_rest_at.map_or(t_arity, |r| r + 1)
                };
                self.report_error(
                    dg::SOURCE_PROVIDES_NO_MATCH_FOR_REQUIRED_ELEMENT_AT_POSITION_0_IN_TARGET_2623,
                    &[&pos.to_string()],
                );
            }
            return false;
        }
        // An open-ended source pairs with a target suffix only when its own suffix covers it
        // and its rest starts no earlier than the target's (`[...string[], "b", 1]` against
        // `[...string[], "a", 1]`); other shapes are not modelled.
        if t_suffix > 0 && s_rest {
            let s_rest_at = sshape
                .elements
                .iter()
                .position(|e| e.kind == TupleElement::REST)
                .unwrap_or(0);
            let s_suffix = s_arity - s_rest_at - 1;
            if s_suffix < t_suffix || t_rest_at.is_some_and(|r| s_rest_at < r) {
                return true;
            }
        }
        for i in 0..s_arity {
            let sk = sshape.elements[i].kind;
            let mut se = sargs.get(i).copied().unwrap_or(TypeId::ANY);
            let tidx = if let (Some(r), true) = (t_rest_at, t_suffix > 0) {
                if i < r {
                    i
                } else if i + t_suffix >= s_arity {
                    t_arity - (s_arity - i)
                } else {
                    r
                }
            } else if i < t_arity {
                i
            } else {
                t_arity.saturating_sub(1)
            };
            let Some(tel) = tshape.elements.get(tidx) else {
                break;
            };
            let mut te = targs.get(tidx).copied().unwrap_or(TypeId::ANY);
            if sk == TupleElement::OPTIONAL
                && tel.kind != TupleElement::OPTIONAL
                && tel.kind != TupleElement::REST
            {
                // Optional source element into a required target element.
                if report {
                    self.report_error(
                        dg::SOURCE_PROVIDES_NO_MATCH_FOR_REQUIRED_ELEMENT_AT_POSITION_0_IN_TARGET_2623,
                        &[&i.to_string()],
                    );
                }
                return false;
            }
            if sk == TupleElement::OPTIONAL {
                se = self.add_optionality(se);
            }
            if tel.kind == TupleElement::OPTIONAL {
                te = self.add_optionality(te);
            }
            if !self.related_to(se, te, rel, report, None) {
                // Several source positions landing in the target's rest are named as a range.
                let into_rest = t_rest_at
                    .filter(|&r| r == tidx)
                    .map(|r| (r, s_arity - t_suffix - 1))
                    .filter(|&(a, b)| b > a);
                if report && let Some((a, b)) = into_rest {
                    self.report_incompatible(
                        dg::TYPE_AT_POSITIONS_0_THROUGH_1_IN_SOURCE_IS_NOT_COMPATIBLE_WITH_TYPE_AT_POSITION_2_2627,
                        vec![a.to_string(), b.to_string(), tidx.to_string()],
                    );
                } else if report && (t_arity > 1 || s_arity > 1) {
                    self.report_incompatible(
                        dg::TYPE_AT_POSITION_0_IN_SOURCE_IS_NOT_COMPATIBLE_WITH_TYPE_AT_POSITION_1_IN_TARGET_2626,
                        vec![i.to_string(), tidx.to_string()],
                    );
                }
                return false;
            }
        }
        true
    }

    pub(crate) fn is_empty_object_type(&mut self, t: TypeId) -> bool {
        if t == TypeId::EMPTY_OBJECT {
            return true;
        }
        let st = self.store();
        if !st.flags(t).contains(TypeFlags::OBJECT) {
            return false;
        }
        let m = self.resolved_members(t);
        m.is_empty()
    }

    fn properties_related(
        &mut self,
        s: TypeId,
        t: TypeId,
        rel: RelationKind,
        report: bool,
    ) -> bool {
        let excluded = std::mem::take(&mut self.rel.excluded_props);
        let mut tprops = self.properties_of(t);
        if !excluded.is_empty() {
            tprops.retain(|(n, _)| !excluded.contains(n));
        }
        // A subtype of an object literal type has no other properties (except
        // `undefined` ones): `User` is not a subtype of `{ name: string }`, so the common
        // supertype of the two is `User`.
        if matches!(rel, RelationKind::Subtype | RelationKind::StrictSubtype)
            && self.object_flags(t).contains(ObjectFlags::OBJECT_LITERAL)
        {
            for (n, sp) in self.properties_of(s) {
                if excluded.contains(&n) || self.property_of_type_id(t, n).is_some() {
                    continue;
                }
                if self.type_of_symbol(sp) != TypeId::UNDEFINED {
                    if report {
                        let name = self.store().str(n).to_owned();
                        let ts = self.type_to_string(t);
                        self.report_error(
                            dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339,
                            &[&name, &ts],
                        );
                    }
                    return false;
                }
            }
        }
        // Identical types have the same properties (`{ x: number }` is not `{}`).
        if rel == RelationKind::Identity && self.properties_of(s).len() != tprops.len() {
            return false;
        }
        if tprops.is_empty() {
            return true;
        }
        let require_optional = matches!(rel, RelationKind::Subtype | RelationKind::StrictSubtype)
            && !self.object_flags(s).contains(ObjectFlags::OBJECT_LITERAL)
            && !self.c.is_tuple_type(s);
        if rel != RelationKind::Identity {
            // Missing properties.
            let mut missing: Vec<(StrId, SymId)> = Vec::new();
            for &(n, tp) in &tprops {
                let opt = self.c.sym_flags(tp).intersects(SymbolFlags::OPTIONAL);
                if opt && !require_optional {
                    continue;
                }
                if self.property_of_type_id(s, n).is_none() {
                    // Index signatures don't satisfy required properties.
                    missing.push((n, tp));
                }
            }
            if !missing.is_empty() {
                // A plain function type (`() => number`) against a type without call
                // signatures is reported whole.
                let plain_fn = report
                    && self.properties_of(s).is_empty()
                    && !self.signatures_of(s, SigKind::Call).is_empty()
                    && self.signatures_of(t, SigKind::Call).is_empty();
                // A tuple target is not elaborated for a source that is not array-like
                // (`IArguments` to `[]` reports only the head message; an array type
                // missing an element is still reported).
                let non_array_to_tuple = report
                    && self.c.is_tuple_type(t)
                    && missing
                        .iter()
                        .any(|&(n, _)| !self.store().str(n).bytes().all(|b| b.is_ascii_digit()));
                if report && !plain_fn && !non_array_to_tuple {
                    let (ss, ts) = self.type_names_for_error(s, t);
                    if missing.len() == 1 {
                        let pn = self.written_name(missing[0].1, missing[0].0);
                        self.report_error(
                            dg::PROPERTY_0_IS_MISSING_IN_TYPE_1_BUT_REQUIRED_IN_TYPE_2_2741,
                            &[&pn, &ss, &ts],
                        );
                    } else {
                        let names: Vec<String> = missing
                            .iter()
                            .map(|m| self.written_name(m.1, m.0))
                            .collect();
                        if names.len() > 5 {
                            let first: Vec<String> = names[..4].to_vec();
                            self.report_error(
                                dg::TYPE_0_IS_MISSING_THE_FOLLOWING_PROPERTIES_FROM_TYPE_1_2_AND_3_MORE_2740,
                                &[&ss, &ts, &first.join(", "), &(names.len() - 4).to_string()],
                            );
                        } else {
                            self.report_error(
                                dg::TYPE_0_IS_MISSING_THE_FOLLOWING_PROPERTIES_FROM_TYPE_1_2_2739,
                                &[&ss, &ts, &names.join(", ")],
                            );
                        }
                    }
                    self.suppress_next_head();
                }
                return false;
            }
        }
        for (n, tp) in tprops {
            let Some(sp) = self.property_of_type_id(s, n) else {
                if rel == RelationKind::Identity {
                    return false;
                }
                continue;
            };
            if sp == tp {
                continue;
            }
            // A class's `prototype` is not compared (its construct signatures say it):
            // `The types returned by 'new A()'`, not `The types of 'A.prototype'`.
            if rel != RelationKind::Identity && self.is_class_prototype(tp) {
                continue;
            }
            if !self.property_related(s, t, sp, tp, n, rel, report) {
                return false;
            }
        }
        true
    }

    /// A property's name as tsc writes a symbol: a string literal name keeps its quotes
    /// as declared (`'a'`, `"b-c"`), a numeric one its spelling (`2.0`).
    pub(crate) fn written_name(&self, sym: SymId, n: StrId) -> String {
        let d = self
            .c
            .synth_sym(sym)
            .and_then(|x| x.decl)
            .or_else(|| self.c.first_decl(sym));
        if let Some(d) = d {
            let ast = self.ast(d.file);
            let key = match *ast.node(d.node) {
                Node::PropertySignature { key, .. }
                | Node::PropertyDef { key, .. }
                | Node::MethodSignature { key, .. }
                | Node::MethodDef { key, .. }
                | Node::Property { key, .. } => Some(key),
                _ => None,
            };
            // A late-bound name is written as declared (`[c0]` with `const c0 = "1"`).
            if let Some(k) = key
                && let Node::ComputedKey { expr } = *ast.node(k)
                && crate::nodes::is_entity_name_expr(ast, expr)
            {
                return format!("[{}]", self.c.text(Loc::new(d.file, expr)));
            }
            if let Some(k) = key
                && matches!(ast.node(k), Node::StringLit { .. } | Node::NumberLit { .. })
            {
                let sp = self.c.span(Loc::new(d.file, k));
                if let Some(text) = self
                    .c
                    .file(d.file)
                    .text
                    .get(sp.start as usize..sp.end as usize)
                {
                    return text.to_owned();
                }
            }
        }
        self.symbol_display_name(n)
    }

    fn symbol_display_name(&self, n: StrId) -> String {
        let name = self.store().str(n);
        self.c
            .symbol_key_display(name)
            .unwrap_or_else(|| name.to_owned())
    }

    #[allow(clippy::too_many_arguments)]
    fn property_related(
        &mut self,
        s: TypeId,
        t: TypeId,
        sp: SymId,
        tp: SymId,
        name: StrId,
        rel: RelationKind,
        report: bool,
    ) -> bool {
        // Private and protected members must come from the same declaration.
        let hash = self.store().str(name).starts_with('#');
        let s_priv = if hash {
            Some(true)
        } else {
            self.is_private_like(sp)
        };
        let t_priv = if hash {
            Some(true)
        } else {
            self.is_private_like(tp)
        };
        if s_priv.is_some() || t_priv.is_some() {
            let same = self.same_declaration(sp, tp);
            let derived = !same && self.declared_in_derived_class(sp, tp);
            if derived && hash {
                // A subclass's own `#x` doesn't replace the base's (each class has its own).
                return true;
            }
            if !same && t_priv == Some(false) && s_priv != Some(true) && !derived {
                // A protected target needs a declaration in a class derived from its class.
                if report {
                    let pn = self.symbol_display_name(name);
                    let ss = self.type_to_string(s);
                    let cn = self.declaring_class_name(tp);
                    self.report_error(
                        dg::PROPERTY_0_IS_PROTECTED_BUT_TYPE_1_IS_NOT_A_CLASS_DERIVED_FROM_2_2443,
                        &[&pn, &ss, &cn],
                    );
                }
                return false;
            }
            let allowed = same || (derived && t_priv == Some(false) && s_priv != Some(true));
            if !allowed {
                if report {
                    let pn = self.symbol_display_name(name);
                    match (s_priv, t_priv) {
                        (Some(true), Some(true)) if hash => {
                            // `#x` names the classes declaring each member.
                            let ss = self.declaring_class_name(sp);
                            let cn = self.declaring_class_name(tp);
                            self.report_error(
                                dg::PROPERTY_0_IN_TYPE_1_REFERS_TO_A_DIFFERENT_MEMBER_THAT_CANNOT_BE_ACCESSED_FROM_W_18015,
                                &[&pn, &ss, &cn],
                            );
                        }
                        (Some(true), Some(true)) => {
                            self.report_error(
                                dg::TYPES_HAVE_SEPARATE_DECLARATIONS_OF_A_PRIVATE_PROPERTY_0_2442,
                                &[&pn],
                            );
                        }
                        _ => {
                            // tsc names the types declaring each property.
                            let (ss, ts) = self.type_names_for_error(s, t);
                            let ss = self.declaring_type_name(sp, s).unwrap_or(ss);
                            let ts = self.declaring_type_name(tp, t).unwrap_or(ts);
                            let s_first =
                                s_priv == Some(true) || (t_priv != Some(true) && s_priv.is_some());
                            let (a, b) = if s_first { (ss, ts) } else { (ts, ss) };
                            let info = if s_priv == Some(true) || t_priv == Some(true) {
                                dg::PROPERTY_0_IS_PRIVATE_IN_TYPE_1_BUT_NOT_IN_TYPE_2_2325
                            } else {
                                dg::PROPERTY_0_IS_PROTECTED_IN_TYPE_1_BUT_PUBLIC_IN_TYPE_2_2444
                            };
                            self.report_error(info, &[&pn, &a, &b]);
                        }
                    }
                }
                return false;
            }
        }
        let st_ = self.type_of_symbol(sp);
        let mut tt_ = self.type_of_symbol(tp);
        let s_opt = self.c.sym_flags(sp).intersects(SymbolFlags::OPTIONAL)
            || self.c.sym_check_flags(sp) & check_flags::OPTIONAL != 0;
        let t_opt = self.c.sym_flags(tp).intersects(SymbolFlags::OPTIONAL)
            || self.c.sym_check_flags(tp) & check_flags::OPTIONAL != 0;
        // Under `exactOptionalPropertyTypes` property types are related without the
        // `missing` of optionality (checked on its own below): an optional target property
        // takes a missing source property but not an explicit `undefined`.
        let exact_on = self.c.opts.exact_optional_property_types && rel != RelationKind::Identity;
        let exact = exact_on && t_opt && rel != RelationKind::Comparable;
        if exact {
            tt_ = self.remove_missing(tt_);
        }
        let st_ = if exact_on {
            self.remove_missing(st_)
        } else {
            st_
        };
        let related = self.related_to(st_, tt_, rel, report, None);
        if !related {
            if report
                && exact
                && self.is_exact_optional_mismatch(st_, tt_)
                && let Some(r) = self.rel.report.as_mut()
            {
                r.exact_mismatch = true;
            }
            if report {
                let pn = self.written_name(tp, name);
                self.report_incompatible(dg::TYPES_OF_PROPERTY_0_ARE_INCOMPATIBLE_2326, vec![pn]);
            }
            return false;
        }
        if rel != RelationKind::Comparable && s_opt && !t_opt {
            if report {
                let pn = self.symbol_display_name(name);
                let (ss, ts) = self.type_names_for_error(s, t);
                self.report_error(
                    dg::PROPERTY_0_IS_OPTIONAL_IN_TYPE_1_BUT_REQUIRED_IN_TYPE_2_2327,
                    &[&pn, &ss, &ts],
                );
            }
            return false;
        }
        true
    }

    /// The `prototype` property synthesized for a class's static side (or an
    /// instantiation of it).
    pub(crate) fn is_class_prototype(&self, p: SymId) -> bool {
        let mut p = p;
        for _ in 0..8 {
            let Some(x) = self.c.synth_sym(p) else {
                return false;
            };
            if x.decl.is_some() || x.mapped.is_some() || self.store().str(x.name) != "prototype" {
                return false;
            }
            match x.origins.first() {
                None => return true,
                Some(&o) => p = o,
            }
        }
        false
    }

    /// `Some(true)` for private, `Some(false)` for protected members.
    pub(crate) fn is_private_like(&self, p: SymId) -> Option<bool> {
        let decl = self.c.value_decl(p).or_else(|| self.c.first_decl(p))?;
        let m = self.node(decl).modifiers();
        if m.contains(ts_r_ast::Modifiers::PRIVATE)
            || self
                .c
                .sym_facts(p)
                .intersects(ts_r_binder::SymbolFacts::PRIVATE_NAME)
        {
            return Some(true);
        }
        if m.contains(ts_r_ast::Modifiers::PROTECTED) {
            return Some(false);
        }
        None
    }

    /// The class declaring property `p`.
    fn declaring_class(&self, p: SymId) -> Option<Loc> {
        let d = self.c.value_decl(p).or_else(|| self.c.first_decl(p))?;
        self.enclosing_class(d)
    }

    /// The printed type declaring property `p`: its class (`typeof C` for statics) or
    /// interface.
    /// (`of`, the related type holding `p`, is printed itself when it instantiates that
    /// declaration: `classWithPrivate<number>`.)
    fn declaring_type_name(&mut self, p: SymId, of: TypeId) -> Option<String> {
        let p = match self.c.synth_sym(p) {
            Some(x) if x.origins.len() == 1 => x.origins[0],
            Some(_) => return None,
            None => p,
        };
        let d = self.c.value_decl(p).or_else(|| self.c.first_decl(p))?;
        let owner = match self.node(d) {
            Node::Param { .. } => self.enclosing_class(d)?,
            _ => self.c.parent(d)?,
        };
        if !matches!(
            self.node(owner),
            Node::ClassDecl { .. } | Node::ClassExpr { .. } | Node::Interface { .. }
        ) {
            return None;
        }
        let sym = self.c.sym_of_decl(owner)?;
        if self
            .node(d)
            .modifiers()
            .contains(ts_r_ast::Modifiers::STATIC)
        {
            let n = self.c.sym_name(sym).to_owned();
            return Some(format!("typeof {n}"));
        }
        // A member an interface with members of its own inherits from a class is named
        // after the interface (`interface I extends Foo { y }`: "private in type 'I'"); a
        // derived class's, or an empty interface's, after the class declaring it.
        if let Some(os) = self.declared_sym_of(of)
            && os != sym
            && self.c.sym_flags(os).contains(SymbolFlags::INTERFACE)
            && !self.c.sym_flags(os).contains(SymbolFlags::CLASS)
            && self
                .c
                .decls(os)
                .iter()
                .any(|&d| matches!(self.node(d), Node::Interface { body, .. } if !body.is_empty()))
        {
            return Some(self.type_to_string(of));
        }
        let t = self.declared_type_of_symbol(sym);
        if of != t && self.reference_target(of) == Some(t) {
            return Some(self.type_to_string(of));
        }
        Some(self.type_to_string(t))
    }

    fn declaring_class_name(&self, p: SymId) -> String {
        self.declaring_class(p)
            .and_then(|c| self.c.sym_of_decl(c))
            .map(|s| self.c.sym_name(s).to_owned())
            .unwrap_or_default()
    }

    /// Whether `sp` is declared in a class derived from the class declaring `tp`.
    fn declared_in_derived_class(&mut self, sp: SymId, tp: SymId) -> bool {
        match (self.declaring_class(sp), self.declaring_class(tp)) {
            (Some(a), Some(b)) => a != b && self.class_derives_from(a, b),
            _ => false,
        }
    }

    fn same_declaration(&self, a: SymId, b: SymId) -> bool {
        let da = self.c.value_decl(a).or_else(|| self.c.first_decl(a));
        let db = self.c.value_decl(b).or_else(|| self.c.first_decl(b));
        da.is_some() && da == db
    }

    fn signatures_related(
        &mut self,
        s: TypeId,
        t: TypeId,
        kind: SigKind,
        rel: RelationKind,
        report: bool,
    ) -> bool {
        let tsigs = self.signatures_of(t, kind);
        if tsigs.is_empty() {
            return true;
        }
        let ssigs = self.signatures_of(s, kind);
        if self.store().flags(s).intersects(TypeFlags::ANY) {
            return true;
        }
        if ssigs.is_empty() {
            if report {
                let ss = self.type_to_string(s);
                let sig = self.signature_to_string(tsigs[0]);
                self.report_error(
                    dg::TYPE_0_PROVIDES_NO_MATCH_FOR_THE_SIGNATURE_1_2658,
                    &[&ss, &sig],
                );
            }
            return false;
        }
        if kind == SigKind::Construct {
            let s_abs = ssigs
                .iter()
                .all(|&x| self.store().sig(x).flags.contains(SigFlags::ABSTRACT));
            let t_abs = tsigs
                .iter()
                .all(|&x| self.store().sig(x).flags.contains(SigFlags::ABSTRACT));
            if s_abs && !t_abs && rel != RelationKind::Identity {
                if report {
                    self.report_error(dg::CANNOT_ASSIGN_AN_ABSTRACT_CONSTRUCTOR_TYPE_TO_A_NON_ABSTRACT_CONSTRUCTOR_TYPE_2517, &[]);
                }
                return false;
            }
        }
        // Two instantiations of one type literal (or one generic interface) relate their
        // signatures pairwise, type parameters erased: overloads can't stand in for each
        // other (`{ (x: T): void; (x: T, o: Opts<T>): void }` is invariant in `T`).
        if ssigs.len() == tsigs.len() && self.same_signature_origin(s, t) {
            let pairs: Vec<(SigId, SigId)> =
                ssigs.iter().copied().zip(tsigs.iter().copied()).collect();
            for (ss, ts) in pairs {
                let se = self.erase_signature(ss);
                let te = self.erase_signature(ts);
                if !self.signature_related(se, te, kind, rel, report) {
                    return false;
                }
            }
            return true;
        }
        if ssigs.len() == 1 && tsigs.len() == 1 {
            return self.signature_related(ssigs[0], tsigs[0], kind, rel, report);
        }
        // With several signatures on either side, type parameters are erased to `any`
        // (observed: `Dispatch` with `<T extends A>(action: T) => T` matches every overload
        // of `ThunkDispatch`, but not a single-signature target with the first overload).
        let ssigs: Vec<SigId> = ssigs.iter().map(|&x| self.erase_signature(x)).collect();
        'outer: for &tsig in &tsigs {
            let erased = self.erase_signature(tsig);
            for &ssig in &ssigs {
                if self.signature_related(ssig, erased, kind, rel, false) {
                    continue 'outer;
                }
            }
            // The failure is explained by the first source signature.
            if report {
                let _ = self.signature_related(ssigs[0], erased, kind, rel, true);
            }
            return false;
        }
        true
    }

    /// Whether `s` and `t` are instantiations of one type literal or one generic type.
    fn same_signature_origin(&self, s: TypeId, t: TypeId) -> bool {
        let st = self.store();
        match (
            st.object_of(s).map(|o| o.kind),
            st.object_of(t).map(|o| o.kind),
        ) {
            (
                Some(ObjectKind::Anonymous {
                    decl: d1,
                    mapper: m1,
                    ..
                }),
                Some(ObjectKind::Anonymous {
                    decl: d2,
                    mapper: m2,
                    ..
                }),
            ) => {
                d1 == d2
                    && m1 != ts_r_types::MapperId::IDENTITY
                    && m2 != ts_r_types::MapperId::IDENTITY
            }
            (
                Some(ObjectKind::Reference { target: a, .. }),
                Some(ObjectKind::Reference { target: b, .. }),
            ) => a == b,
            _ => false,
        }
    }

    /// `sig` with its own type parameters replaced by `any`.
    fn erase_signature(&mut self, sig: SigId) -> SigId {
        let d = self.store().sig(sig);
        if d.type_params == ListId::EMPTY {
            return sig;
        }
        let tps = self.store().list_items(d.type_params).to_vec();
        let anys = vec![TypeId::ANY; tps.len()];
        let m = self.store().mapper(&tps, &anys);
        self.instantiate_signature_full(sig, m)
    }

    /// The type of parameter position `i` (rest elements included), if any.
    pub fn param_type_at(&mut self, sig: SigId, i: usize) -> Option<TypeId> {
        let d = self.store().sig(sig);
        let n = d.params.len();
        if d.has_rest() && i + 1 >= n {
            let rest = d.params.last()?.ty;
            if let Some(shape) = self.store().tuple_shape_of(rest) {
                let shape = shape.clone();
                let args = self.c.type_args_of_tuple(rest).to_vec();
                let k = i + 1 - n;
                if k < shape.elements.len() {
                    let el = shape.elements[k];
                    let a = args.get(k).copied().unwrap_or(TypeId::ANY);
                    return Some(match el.kind {
                        TupleElement::OPTIONAL => self.add_optionality(a),
                        TupleElement::VARIADIC => {
                            self.element_type_of_array_like(a).unwrap_or(TypeId::ANY)
                        }
                        _ => a,
                    });
                }
                if shape.has_rest() {
                    let ri = shape
                        .elements
                        .iter()
                        .position(|e| e.kind == TupleElement::REST)
                        .unwrap_or(0);
                    return args.get(ri).copied();
                }
                return None;
            }
            return Some(self.element_type_of_array_like(rest).unwrap_or(TypeId::ANY));
        }
        d.params.get(i).map(|p| p.ty)
    }

    /// Parameter count ignoring rest, and whether there is a rest parameter.
    pub fn param_count(&self, sig: SigId) -> (usize, bool) {
        let d = self.store().sig(sig);
        if d.has_rest() {
            (d.params.len() - 1, true)
        } else {
            (d.params.len(), false)
        }
    }

    fn signature_related(
        &mut self,
        ssig: SigId,
        tsig: SigId,
        kind: SigKind,
        rel: RelationKind,
        report: bool,
    ) -> bool {
        self.signature_related_mode(ssig, tsig, kind, rel, report, false)
    }

    /// For identity, two signatures need as many type parameters with identical constraints
    /// (`<T extends Date>` is not `<T extends number>`, `<T, U>` is not `<T>`), and the same
    /// parameter count, minimum argument count and rest parameter.
    pub(crate) fn signature_shapes_identical(&mut self, ssig: SigId, tsig: SigId) -> bool {
        let sd = self.store().sig(ssig);
        let td = self.store().sig(tsig);
        if sd.params.len() != td.params.len() || sd.has_rest() != td.has_rest() {
            return false;
        }
        let stps = self.store().list_items(sd.type_params).to_vec();
        let ttps = self.store().list_items(td.type_params).to_vec();
        if stps.len() != ttps.len() {
            return false;
        }
        if self.effective_min_args(ssig) != self.effective_min_args(tsig) {
            return false;
        }
        if stps.is_empty() {
            return true;
        }
        let m = self.store().mapper(&stps, &ttps);
        for (&sp, &tp) in stps.iter().zip(&ttps) {
            let sc = self.type_param_constraint(sp);
            let tc = self.type_param_constraint(tp);
            match (sc, tc) {
                (None, None) => {}
                (Some(a), Some(b)) => {
                    let a = self.instantiate(a, m);
                    if a != b && !self.is_type_related(a, b, RelationKind::Identity) {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        true
    }

    /// `callback`: comparing a method's callback parameters (target callback to source
    /// callback); return types then relate either way.
    fn signature_related_mode(
        &mut self,
        ssig: SigId,
        tsig: SigId,
        kind: SigKind,
        rel: RelationKind,
        report: bool,
        callback: bool,
    ) -> bool {
        if ssig == tsig {
            return true;
        }
        self.note_unmeasurable_rest(ssig);
        self.note_unmeasurable_rest(tsig);
        if rel == RelationKind::Identity && !self.signature_shapes_identical(ssig, tsig) {
            return false;
        }
        // Comparability erases both signatures' type parameters to `any` (observed:
        // `{ fn<T extends string>(x: T): T }` and `{ fn(x: number): number }` compare).
        let (ssig, tsig) = if rel == RelationKind::Comparable {
            (self.erase_signature(ssig), self.erase_signature(tsig))
        } else {
            (ssig, tsig)
        };
        // Two instantiations of one declared generic signature (a method read through
        // `ZodNumber` and through `ZodType<unknown>`) relate with their type parameters
        // identified first, then like any other pair.
        {
            let d = self.store().sig(ssig);
            let td = self.store().sig(tsig);
            if d.type_params != ListId::EMPTY
                && d.decl.is_some()
                && d.decl == td.decl
                && self.store().list_items(d.type_params).len()
                    == self.store().list_items(td.type_params).len()
            {
                let tps = self.store().list_items(d.type_params).to_vec();
                let ttps = self.store().list_items(td.type_params).to_vec();
                let m = self.store().mapper(&tps, &ttps);
                let unified = self.instantiate_signature_full(ssig, m);
                if self.signature_related_mode(unified, tsig, kind, rel, false, callback) {
                    return true;
                }
            }
        }
        // Generic source: instantiate in the context of the target.
        let ssig = {
            let d = self.store().sig(ssig);
            if d.type_params != ListId::EMPTY {
                let tps = self.store().list_items(d.type_params).to_vec();
                let args = self.signature_args_in_context(ssig, tsig, &tps, rel);
                let m = self.store().mapper(&tps, &args);
                self.instantiate_signature_full(ssig, m)
            } else {
                ssig
            }
        };
        let sd = self.store().sig(ssig);
        let td = self.store().sig(tsig);
        let (s_count, s_rest) = (sd.params.len(), sd.has_rest());
        let (t_count, t_rest) = (td.params.len(), td.has_rest());
        let s_min = self.effective_min_args(ssig);
        let target_decl_kind_strict = self.c.opts.strict_function_types
            && rel != RelationKind::Comparable
            && kind == SigKind::Call
            && !td.flags.contains(SigFlags::METHOD)
            && !td.flags.contains(SigFlags::CONSTRUCT);
        // Too many required parameters.
        let t_max = if t_rest { usize::MAX } else { t_count };
        if s_min > t_max && rel != RelationKind::Identity {
            if report {
                self.report_error(dg::TARGET_SIGNATURE_PROVIDES_TOO_FEW_ARGUMENTS_EXPECTED_0_OR_MORE_BUT_GOT_1_2849, &[&s_min.to_string(), &t_count.to_string()]);
            }
            return false;
        }
        // `this` types: contravariant for a strict function type, else either way. A source
        // `this: void` asks nothing of the target's.
        if let (Some(st_), Some(tt_)) = (sd.this_type, td.this_type)
            && st_ != TypeId::VOID
            && !self.related_to(tt_, st_, rel, false, None)
            && (target_decl_kind_strict || !self.related_to(st_, tt_, rel, false, None))
        {
            if report {
                self.related_to(tt_, st_, rel, true, None);
                self.flush_pending_incompatible(rel);
                self.report_error(
                    dg::THE_THIS_TYPES_OF_EACH_SIGNATURE_ARE_INCOMPATIBLE_2685,
                    &[],
                );
            }
            return false;
        }
        let count = if s_rest || t_rest {
            s_count.max(t_count)
        } else {
            s_count.min(t_count)
        };
        for i in 0..count {
            let Some(sp) = self.param_type_at(ssig, i) else {
                continue;
            };
            let Some(tp) = self.param_type_at(tsig, i) else {
                continue;
            };
            if sp == tp {
                continue;
            }
            // Callback parameters are compared covariantly-ish (tsc's callback check). Like
            // tsc, only one level: a callback's own callback parameters go through
            // `related_to`, whose cycle check stops `f(cb: A['f'])` against `f(cb: B['f'])`.
            let callbacks = if !target_decl_kind_strict
                && !callback
                && self.c.opts.strict_function_types
                && rel != RelationKind::Comparable
                && rel != RelationKind::Identity
                && !self.declared_param_is_type_param(ssig, i)
                && !self.declared_param_is_type_param(tsig, i)
            {
                self.callback_pair(sp, tp)
            } else {
                None
            };
            let ok = if let Some((scb, tcb)) = callbacks {
                // A callback parameter of a method is still checked strictly: the
                // target's callback must accept what the source's callback would be given.
                self.signature_related_mode(tcb, scb, SigKind::Call, rel, false, true)
            } else if target_decl_kind_strict {
                self.related_to(tp, sp, rel, false, None)
            } else {
                self.related_to(sp, tp, rel, false, None)
                    || self.related_to(tp, sp, rel, false, None)
            };
            if !ok {
                if report {
                    // A strict function type's callback parameters are explained by their
                    // signatures too, without a head for the callback types.
                    let strict_callbacks = if target_decl_kind_strict
                        && !callback
                        && !self.rel.in_strict_callback
                        && rel != RelationKind::Identity
                        && !self.declared_param_is_type_param(ssig, i)
                        && !self.declared_param_is_type_param(tsig, i)
                    {
                        self.callback_pair(sp, tp).filter(|&(scb, tcb)| {
                            self.rel.in_strict_callback = true;
                            let ok = self.signature_related_mode(
                                tcb,
                                scb,
                                SigKind::Call,
                                rel,
                                false,
                                false,
                            );
                            self.rel.in_strict_callback = false;
                            !ok
                        })
                    } else {
                        None
                    };
                    if let Some((scb, tcb)) = callbacks {
                        self.signature_related_mode(tcb, scb, SigKind::Call, rel, true, true);
                    } else if let Some((scb, tcb)) = strict_callbacks {
                        self.rel.in_strict_callback = true;
                        self.signature_related_mode(tcb, scb, SigKind::Call, rel, true, false);
                        self.rel.in_strict_callback = false;
                    } else {
                        // Bivariant (method) parameters report the contravariant direction too.
                        self.related_to(tp, sp, rel, true, None);
                    }
                    let sn = self.param_name_at(ssig, i);
                    let tn = self.param_name_at(tsig, i);
                    // The parameter types' pending property levels (and the head they
                    // deferred) come below this message.
                    self.flush_pending_incompatible(rel);
                    self.report_error(
                        dg::TYPES_OF_PARAMETERS_0_AND_1_ARE_INCOMPATIBLE_2328,
                        &[&sn, &tn],
                    );
                }
                return false;
            }
        }
        // Return types.
        let tr = self.return_type_of(tsig);
        if tr == TypeId::VOID || tr == TypeId::ANY || tr == TypeId::UNRESOLVED {
            return true;
        }
        let sr = self.return_type_of(ssig);
        // Type predicates.
        let tpred = self.predicate_of(tsig);
        if let Some(tp) = tpred {
            let spred = self.predicate_of(ssig);
            match (spred, tp.ty) {
                (Some(sp), Some(tt)) => {
                    use ts_r_types::PredicateKind as Pk;
                    let s_this = matches!(sp.kind, Pk::This | Pk::AssertsThis);
                    let t_this = matches!(tp.kind, Pk::This | Pk::AssertsThis);
                    let ok = if s_this != t_this {
                        if report {
                            self.report_error(
                                dg::A_THIS_BASED_TYPE_GUARD_IS_NOT_COMPATIBLE_WITH_A_PARAMETER_BASED_TYPE_GUARD_2518,
                                &[],
                            );
                        }
                        false
                    } else if !s_this && sp.param_index != tp.param_index {
                        if report {
                            let a = self.store().str(sp.param_name).to_owned();
                            let b = self.store().str(tp.param_name).to_owned();
                            self.report_error(
                                dg::PARAMETER_0_IS_NOT_IN_THE_SAME_POSITION_AS_PARAMETER_1_1227,
                                &[&a, &b],
                            );
                        }
                        false
                    } else if let Some(sty) = sp.ty {
                        self.related_to(sty, tt, rel, report, None)
                    } else {
                        true
                    };
                    if !ok && report {
                        let a = self.predicate_text(sp);
                        let b = self.predicate_text(tp);
                        self.report_error(
                            dg::TYPE_PREDICATE_0_IS_NOT_ASSIGNABLE_TO_1_1226,
                            &[&a, &b],
                        );
                    }
                    return ok;
                }
                (None, _)
                    if matches!(
                        tp.kind,
                        ts_r_types::PredicateKind::Identifier | ts_r_types::PredicateKind::This
                    ) =>
                {
                    if report {
                        let sigs = self.signature_to_string(ssig);
                        self.report_error(dg::SIGNATURE_0_MUST_BE_A_TYPE_PREDICATE_1224, &[&sigs]);
                    }
                    return false;
                }
                _ => {}
            }
        }
        if callback && self.related_to(tr, sr, rel, false, None) {
            return true;
        }
        if !self.related_to(sr, tr, rel, report, None) {
            if report {
                let ss = self.type_to_string(sr);
                let ts = self.type_to_string(tr);
                let no_args = s_count == 0 && t_count == 0;
                let info = match (kind, no_args) {
                    (SigKind::Call, false) => dg::CALL_SIGNATURE_RETURN_TYPES_0_AND_1_ARE_INCOMPATIBLE_2202,
                    (SigKind::Call, true) => dg::CALL_SIGNATURES_WITH_NO_ARGUMENTS_HAVE_INCOMPATIBLE_RETURN_TYPES_0_AND_1_2204,
                    (SigKind::Construct, false) => dg::CONSTRUCT_SIGNATURE_RETURN_TYPES_0_AND_1_ARE_INCOMPATIBLE_2203,
                    (SigKind::Construct, true) => dg::CONSTRUCT_SIGNATURES_WITH_NO_ARGUMENTS_HAVE_INCOMPATIBLE_RETURN_TYPES_0_AND_1_2205,
                };
                self.report_incompatible(info, vec![ss, ts]);
            }
            return false;
        }
        true
    }

    /// Type arguments for a generic source signature compared with `tsig`: inferred from
    /// the target's parameter types; a type parameter they do not inform takes the target's
    /// type parameter at the same position (when every one meets the source's constraint),
    /// else is inferred from the return types, else is `any`.
    fn signature_args_in_context(
        &mut self,
        ssig: SigId,
        tsig: SigId,
        tps: &[TypeId],
        rel: RelationKind,
    ) -> Vec<TypeId> {
        let d = self.store().sig(ssig);
        let td = self.store().sig(tsig);
        let ttps = self.store().list_items(td.type_params).to_vec();
        let _ = d;
        // Signatures compared for identity share their type parameters.
        if ttps.len() == tps.len() && rel == RelationKind::Identity {
            return ttps;
        }
        let has = |ctx: &crate::infer::InferenceContext, i: usize| {
            !ctx.candidates[i].is_empty() || !ctx.contra[i].is_empty() || !ctx.weak[i].is_empty()
        };
        let mut ctx = crate::infer::InferenceContext::new(tps);
        let n = self
            .store()
            .sig(ssig)
            .params
            .len()
            .max(self.store().sig(tsig).params.len());
        for i in 0..n {
            if let (Some(sp), Some(tp)) = (self.param_type_at(ssig, i), self.param_type_at(tsig, i))
            {
                self.infer_types(&mut ctx, tp, sp, 0);
            }
        }
        let mut rctx = crate::infer::InferenceContext::new(tps);
        rctx.filter_by_constraint = true;
        if (0..tps.len()).any(|i| !has(&ctx, i)) {
            let sr = self.return_type_of(ssig);
            let tr = self.return_type_of(tsig);
            self.infer_types(&mut rctx, tr, sr, 0);
        }
        let mut res = Vec::with_capacity(tps.len());
        let mut directs = Vec::new();
        for i in 0..tps.len() {
            // A target type parameter inferred directly is checked against the source's
            // constraint below, once every inference is known (the constraint may mention
            // the type parameter itself: `<T extends I2<T>>`).
            let direct = match (ctx.candidates[i].as_slice(), ctx.contra[i].as_slice()) {
                ([c], []) | ([], [c]) if ttps.contains(c) => Some(*c),
                _ => None,
            };
            if direct.is_some() {
                directs.push(i);
            }
            let t = if let Some(c) = direct {
                c
            } else if has(&ctx, i) {
                let t = self.inferred_type_at(&mut ctx, i);
                if t == TypeId::UNKNOWN { TypeId::ANY } else { t }
            } else if has(&rctx, i) {
                let t = self.inferred_type_at(&mut rctx, i);
                if t == TypeId::UNKNOWN { TypeId::ANY } else { t }
            } else {
                // Nothing informs it: `unknown` (`<S>() => S[]` against `<T>(x: T) => T`
                // compares `unknown[]` with `T`).
                TypeId::UNKNOWN
            };
            res.push(t);
        }
        let checked = res.clone();
        self.constrain_inferences(tps, &mut res);
        let m = self.store().mapper(tps, &checked);
        for i in directs {
            res[i] = checked[i];
            if let Some(k) = self.type_param_constraint(tps[i]) {
                let ic = self.instantiate(k, m);
                if ic != TypeId::UNRESOLVED
                    && !self.is_type_related(checked[i], ic, RelationKind::Assignable)
                {
                    res[i] = ic;
                }
            }
        }
        res
    }

    /// The parameter at `i` is declared as a naked type parameter (`set(value: T)`) in
    /// the signature `sig` was instantiated from: a function type it was instantiated
    /// with is not a callback for the strict callback check.
    fn declared_param_is_type_param(&mut self, sig: SigId, i: usize) -> bool {
        let mut orig = sig;
        for _ in 0..16 {
            match self.store().sig(orig).target {
                Some((o, _)) => orig = o,
                None => break,
            }
        }
        if orig == sig {
            return false;
        }
        self.param_type_at(orig, i)
            .is_some_and(|t| self.store().flags(t).contains(TypeFlags::TYPE_PARAMETER))
    }

    /// The single call signatures of two callback parameter types (function types with
    /// no other members, alike in nullability, without type predicates).
    fn callback_pair(&mut self, sp: TypeId, tp: TypeId) -> Option<(SigId, SigId)> {
        let nullish = |ck: &mut Self, t: TypeId| {
            ck.some_member(t, |m| {
                ck.store()
                    .flags(m)
                    .intersects(TypeFlags::NULLABLE | TypeFlags::VOID)
            })
        };
        if nullish(self, sp) != nullish(self, tp) {
            return None;
        }
        let a = self.single_callback_signature(sp)?;
        let b = self.single_callback_signature(tp)?;
        Some((a, b))
    }

    fn single_callback_signature(&mut self, t: TypeId) -> Option<SigId> {
        let t = self.non_nullable(t);
        if !self.store().flags(t).contains(TypeFlags::OBJECT) {
            return None;
        }
        let sigs = self.signatures_of(t, SigKind::Call);
        if sigs.len() != 1
            || !self.signatures_of(t, SigKind::Construct).is_empty()
            || !self.properties_of(t).is_empty()
        {
            return None;
        }
        let sig = sigs[0];
        if self.store().sig(sig).type_params != ListId::EMPTY || self.predicate_of(sig).is_some() {
            return None;
        }
        Some(sig)
    }

    fn param_name_at(&self, sig: SigId, i: usize) -> String {
        let d = self.store().sig(sig);
        let p = if d.has_rest() && i + 1 >= d.params.len() {
            d.params.last()
        } else {
            d.params.get(i)
        };
        p.map_or_else(
            || format!("arg{i}"),
            |p| {
                let n = self.store().str(p.name);
                // A binding pattern parameter is named by its position (`__0`).
                if n.starts_with('{') || n.starts_with('[') {
                    format!("__{i}")
                } else {
                    n.to_owned()
                }
            },
        )
    }

    fn index_signatures_related(
        &mut self,
        s: TypeId,
        t: TypeId,
        rel: RelationKind,
        report: bool,
    ) -> bool {
        let tt = self.apparent_type(t);
        let tindex = if self.store().flags(tt).contains(TypeFlags::INTERSECTION) {
            // The members' index signatures combined (`{ [x: string]: A } & { [x: string]: B }`
            // indexes to `A & B`).
            let mut keys: Vec<TypeId> = Vec::new();
            for m in self.store().members_of_union(tt).to_vec() {
                let m = self.apparent_type(m);
                if self.store().flags(m).contains(TypeFlags::OBJECT) {
                    for i in self.resolved_members(m).index.iter() {
                        if !keys.contains(&i.key) {
                            keys.push(i.key);
                        }
                    }
                }
            }
            let mut out = Vec::new();
            for k in keys {
                if let Some(i) = self.index_info_of(tt, k) {
                    out.push(i);
                }
            }
            out
        } else if self.store().flags(tt).contains(TypeFlags::OBJECT) {
            self.resolved_members(tt).index.to_vec()
        } else {
            return true;
        };
        // With a string index on the target, any index of type `any` accepts every
        // non-primitive source (`Omit<any, "_id">`'s `symbol` index takes `Document`).
        let any_ok = rel != RelationKind::StrictSubtype
            && !self.store().flags(s).intersects(TypeFlags::PRIMITIVE)
            && tindex.iter().any(|i| i.key == TypeId::STRING);
        for ti in tindex {
            if self.store().flags(s).intersects(TypeFlags::ANY) {
                continue;
            }
            if any_ok && ti.value == TypeId::ANY {
                continue;
            }
            match self.index_info_of(s, ti.key) {
                Some(si) => {
                    if !self.related_to(si.value, ti.value, rel, report, None) {
                        if report {
                            self.report_index_incompatible(si.key, ti.key);
                        }
                        return false;
                    }
                }
                None => {
                    // A string index of `any` accepts any object type (`Record<string, any>`
                    // takes interfaces and class instances).
                    if ti.key == TypeId::STRING && ti.value == TypeId::ANY {
                        continue;
                    }
                    // Under the strict subtype relation only fresh object literals get
                    // an implicit index signature.
                    let implicit_ok = rel != RelationKind::StrictSubtype
                        || self
                            .store()
                            .object_of(s)
                            .is_some_and(|o| o.flags.contains(ObjectFlags::FRESH_LITERAL));
                    if implicit_ok && self.has_inferable_index(s) {
                        // Every applicable property must match the index signature.
                        for (n, p) in self.properties_of(s) {
                            let name = self.store().str(n).to_owned();
                            let applies = if ti.key == TypeId::STRING {
                                !name.starts_with("__@")
                            } else if ti.key == TypeId::NUMBER {
                                name.parse::<f64>().is_ok()
                            } else {
                                name.starts_with("__@")
                            };
                            // Hyphenated JSX attributes the target lacks are ignored.
                            if !applies || self.is_hyphenated_jsx_attr(p) {
                                continue;
                            }
                            let mut pt = self.type_of_symbol(p);
                            if self.c.sym_flags(p).intersects(SymbolFlags::OPTIONAL) {
                                pt = self.remove_optionality(pt);
                            }
                            if !self.related_to(pt, ti.value, rel, report, None) {
                                if report {
                                    let pn = self.written_name(p, n);
                                    self.report_error(
                                        dg::PROPERTY_0_IS_INCOMPATIBLE_WITH_INDEX_SIGNATURE_2530,
                                        &[&pn],
                                    );
                                }
                                return false;
                            }
                        }
                        // The source's own index signatures that apply to the target's key
                        // (`[x: number]` under a target `[x: string]`) must match too.
                        // (Object literals only: an enum's reverse mapping is not checked.)
                        if ti.key == TypeId::STRING
                            && matches!(
                                self.store().object_of(s).map(|o| o.kind),
                                Some(ObjectKind::Literal { .. })
                            )
                        {
                            let sinfos = self.resolved_members(s).index.to_vec();
                            for si in sinfos {
                                if si.key != TypeId::NUMBER {
                                    continue;
                                }
                                if !self.related_to(si.value, ti.value, rel, report, None) {
                                    if report {
                                        self.report_index_incompatible(si.key, ti.key);
                                    }
                                    return false;
                                }
                            }
                        }
                        // A numeric index on the target also covers string-index sources.
                    } else {
                        if rel == RelationKind::Comparable {
                            continue;
                        }
                        if report {
                            let k = self.type_to_string(ti.key);
                            let ss = self.type_to_string(s);
                            self.report_error(
                                dg::INDEX_SIGNATURE_FOR_TYPE_0_IS_MISSING_IN_TYPE_1_2329,
                                &[&k, &ss],
                            );
                        }
                        return false;
                    }
                }
            }
        }
        true
    }

    /// `'number' index signatures are incompatible.`, or `'string' and 'number' ...` when
    /// the source's index signature has another key than the target's.
    fn report_index_incompatible(&mut self, skey: TypeId, tkey: TypeId) {
        let t = self.type_to_string(tkey);
        if skey != tkey {
            let s = self.type_to_string(skey);
            self.report_error(
                dg::_0_AND_1_INDEX_SIGNATURES_ARE_INCOMPATIBLE_2330,
                &[&s, &t],
            );
        } else {
            self.report_error(dg::_0_INDEX_SIGNATURES_ARE_INCOMPATIBLE_2634, &[&t]);
        }
    }

    /// Object literal types, type literals, namespaces and enums get an implicit index
    /// signature; interfaces and classes do not.
    pub(crate) fn has_inferable_index(&mut self, s: TypeId) -> bool {
        let st = self.store();
        if st.flags(s).contains(TypeFlags::INTERSECTION) {
            return st
                .members_of_union(s)
                .to_vec()
                .into_iter()
                .all(|m| self.has_inferable_index(m));
        }
        // Callable and constructable types never do (`() => void` is not a `Record<string,
        // unknown>`).
        if st.flags(s).contains(TypeFlags::OBJECT)
            && (!self.signatures_of(s, SigKind::Call).is_empty()
                || !self.signatures_of(s, SigKind::Construct).is_empty())
        {
            return false;
        }
        let st = self.store();
        match st.object_of(s).map(|o| (o.kind, o.flags)) {
            Some((ObjectKind::Literal { .. }, _)) => true,
            Some((ObjectKind::Anonymous { kind, .. }, _)) => matches!(
                kind,
                anon::TYPE_LITERAL | anon::NAMESPACE | anon::FUNCTION_TYPE
            ),
            Some((ObjectKind::Mapped { .. }, _)) => true,
            _ => false,
        }
    }

    /// tsc's weak type detection: all-optional targets need a common property.
    pub(crate) fn weak_type_violation(&mut self, s: TypeId, t: TypeId, rel: RelationKind) -> bool {
        if rel == RelationKind::Comparable && !self.is_unit(s) {
            return false;
        }
        let st = self.store();
        let sf = st.flags(s);
        if !sf.intersects(TypeFlags::PRIMITIVE | TypeFlags::OBJECT | TypeFlags::INTERSECTION) {
            return false;
        }
        if !st
            .flags(t)
            .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
        {
            return false;
        }
        if Some(s) == self.global_type(GlobalType::Object) {
            return false;
        }
        if !self.is_weak_type(t) {
            return false;
        }
        let mut sprops = self.properties_of(s);
        // Hyphenated JSX attributes the target lacks are ignored.
        sprops.retain(|&(n, p)| !self.is_hyphenated_jsx_attr(p) || self.is_known_property(t, n));
        let has_sigs = !self.signatures_of(s, SigKind::Call).is_empty()
            || !self.signatures_of(s, SigKind::Construct).is_empty();
        if sprops.is_empty() && !has_sigs {
            return false;
        }
        for (n, _) in sprops {
            if self.is_known_property(t, n) {
                return false;
            }
        }
        true
    }

    fn is_weak_type(&mut self, t: TypeId) -> bool {
        let st = self.store();
        if st.flags(t).contains(TypeFlags::OBJECT) {
            let m = self.resolved_members(t);
            if m.props.is_empty()
                || !m.call.is_empty()
                || !m.construct.is_empty()
                || !m.index.is_empty()
            {
                return false;
            }
            let props = m.props.to_vec();
            return props
                .into_iter()
                .all(|p| self.c.sym_flags(p).intersects(SymbolFlags::OPTIONAL));
        }
        if st.flags(t).contains(TypeFlags::INTERSECTION) {
            return st
                .members_of_union(t)
                .to_vec()
                .into_iter()
                .all(|m| self.is_weak_type(m));
        }
        false
    }

    /// Whether a property name is known in `t` (properties or applicable index signatures).
    pub fn is_known_property(&mut self, t: TypeId, name: StrId) -> bool {
        let st = self.store();
        let f = st.flags(t);
        if f.intersects(TypeFlags::UNION_OR_INTERSECTION) {
            let inter = f.contains(TypeFlags::INTERSECTION);
            // `object` in an intersection knows no properties (`object & { x }`), and neither
            // does an empty member (`Empty & { x }`, `A & ThisType<any>`).
            let members: Vec<TypeId> = st
                .members_of_union(t)
                .iter()
                .copied()
                .filter(|&m| !(inter && st.flags(m).contains(TypeFlags::NON_PRIMITIVE)))
                .collect();
            let members: Vec<TypeId> = if inter {
                let all = members.clone();
                let kept: Vec<TypeId> = members
                    .into_iter()
                    .filter(|&m| {
                        !(self.store().flags(m).contains(TypeFlags::OBJECT)
                            && self.is_empty_object_type(m))
                    })
                    .collect();
                if kept.is_empty() { all } else { kept }
            } else {
                members
            };
            return members.into_iter().any(|m| self.is_known_property(m, name));
        }
        if !f.contains(TypeFlags::OBJECT) || self.is_empty_object_type(t) {
            // `{}` accepts any property (`const x: {} | undefined = { a: 1 }`).
            return true;
        }
        let m = self.resolved_members(t);
        if m.find(name).is_some() {
            return true;
        }
        let text = self.store().str(name);
        if m.index.iter().any(|i| {
            i.key == TypeId::STRING
                || (i.key == TypeId::NUMBER && text.parse::<f64>().is_ok())
                || (i.key == TypeId::SYMBOL && text.starts_with("__@"))
                || self
                    .store()
                    .flags(i.key)
                    .intersects(TypeFlags::TEMPLATE_LITERAL | TypeFlags::STRING_MAPPING)
        }) {
            return true;
        }
        false
    }

    fn is_excess_property_check_target(&mut self, t: TypeId) -> bool {
        let st = self.store();
        let f = st.flags(t);
        if f.contains(TypeFlags::OBJECT) {
            if t == TypeId::EMPTY_OBJECT || Some(t) == self.global_type(GlobalType::Object) {
                return false;
            }
            if st
                .object_of(t)
                .is_some_and(|o| matches!(o.kind, ObjectKind::Mapped { .. }))
            {
                return !self.is_generic_mapped_type(t);
            }
            return true;
        }
        if f.contains(TypeFlags::NON_PRIMITIVE) {
            return false;
        }
        if f.contains(TypeFlags::UNION) {
            return st
                .members_of_union(t)
                .to_vec()
                .into_iter()
                .any(|m| self.is_excess_property_check_target(m));
        }
        if f.contains(TypeFlags::INTERSECTION) {
            // `object & { x: string }` checks like `{ x: string }`.
            let members: Vec<TypeId> = st
                .members_of_union(t)
                .iter()
                .copied()
                .filter(|&m| !st.flags(m).contains(TypeFlags::NON_PRIMITIVE))
                .collect();
            return !members.is_empty()
                && members
                    .into_iter()
                    .all(|m| self.is_excess_property_check_target(m));
        }
        false
    }

    /// Whether the property `p` of a fresh object literal with spreads (`owner`, the
    /// literal) was written in it rather than spread into it; always for other literals.
    fn written_in(&self, p: SymId, owner: Option<Loc>) -> bool {
        let Some(lit) = owner else { return true };
        let decl = self.c.synth_sym(p).and_then(|x| x.decl);
        decl.is_some_and(|d| self.c.parent(d).is_some_and(|pd| pd == lit))
    }

    fn has_excess_properties(
        &mut self,
        s: TypeId,
        t: TypeId,
        rel: RelationKind,
        report: bool,
    ) -> bool {
        if !self.is_excess_property_check_target(t) {
            return false;
        }
        if self.is_type_generic_target(t) {
            return false;
        }
        if self.is_empty_object_type(t) {
            return false;
        }
        let st = self.store();
        let mut reduced = t;
        if st.flags(t).contains(TypeFlags::UNION) {
            reduced = match self.excess_discriminant_type(s, t, rel) {
                Some(d) => d,
                None => {
                    // Primitives are dropped when the union also has object types.
                    self.filter_type(t, |ck, m| {
                        !ck.store().flags(m).intersects(TypeFlags::PRIMITIVE)
                    })
                }
            };
            // Only the non-generic members check (`T | { prop: boolean }`, `T & X | Y`).
            if self.store().flags(reduced).contains(TypeFlags::UNION) {
                let generic = |ck: &Self, m: TypeId| {
                    let st = ck.store();
                    st.flags(m).intersects(TypeFlags::INSTANTIABLE)
                        || (st.flags(m).contains(TypeFlags::INTERSECTION)
                            && st
                                .members_of_union(m)
                                .iter()
                                .any(|&x| st.flags(x).intersects(TypeFlags::INSTANTIABLE)))
                };
                let ms = self.store().members_of_union(reduced).to_vec();
                if ms.iter().any(|&m| generic(self, m)) && !ms.iter().all(|&m| generic(self, m)) {
                    reduced = self.filter_type(reduced, |ck, m| !generic(ck, m));
                }
            }
        }
        let props = self.properties_of(s);
        let owner = self.spread_literals.get(&s).copied();
        for (n, p) in props {
            if self.is_known_property(reduced, n) || self.is_hyphenated_jsx_attr(p) {
                continue;
            }
            // A literal with spreads: only its own written properties are excess.
            if !self.written_in(p, owner) {
                continue;
            }
            if report {
                let error_target =
                    self.filter_type(reduced, |ck, m| ck.is_excess_property_check_target(m));
                let ts = self.type_to_string(error_target);
                let pn = self.symbol_display_name(n);
                // The error moves to the property name in the object literal.
                let decl = self.c.synth_sym(p).and_then(|x| x.decl);
                let name_node = decl.and_then(|d| self.object_member_name(d));
                let suggestion = self.spelling_suggestion_for_property(&pn, error_target);
                if let Some(r) = self.rel.report.as_mut()
                    && let Some(nn) = name_node
                {
                    r.error_node = Some(nn);
                }
                // A JSX attribute: `Type '{...}' is not assignable to type 'P'.` over
                // `Property 'x' does not exist on type 'P'.`
                let is_jsx_attr = decl.is_some_and(|d| {
                    matches!(
                        self.ast(d.file).node(d.node),
                        Node::JsxAttr { .. } | Node::JsxOpening { .. }
                    )
                });
                if is_jsx_attr {
                    match &suggestion {
                        Some(sug) => {
                            // A name that isn't an identifier is quoted (`'"ns:a"'`).
                            let ident = sug.chars().enumerate().all(|(i, c)| {
                                c == '_'
                                    || c == '$'
                                    || c.is_alphabetic()
                                    || (i > 0 && c.is_numeric())
                            });
                            let sug = if ident {
                                sug.clone()
                            } else {
                                format!("\"{sug}\"")
                            };
                            self.report_error(
                                dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_DID_YOU_MEAN_2_2551,
                                &[&pn, &ts, &sug],
                            )
                        }
                        None => self.report_error(
                            dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339,
                            &[&pn, &ts],
                        ),
                    }
                    let ss = self.type_to_string(s);
                    let tt = self.type_to_string(t);
                    self.report_error(dg::TYPE_0_IS_NOT_ASSIGNABLE_TO_TYPE_1_2322, &[&ss, &tt]);
                    self.suppress_next_head();
                    return true;
                }
                match suggestion {
                    Some(sug) if name_node.is_some() => self.report_error(
                        dg::OBJECT_LITERAL_MAY_ONLY_SPECIFY_KNOWN_PROPERTIES_BUT_0_DOES_NOT_EXIST_IN_TYPE_1_D_2561,
                        &[&pn, &ts, &sug],
                    ),
                    _ => self.report_error(dg::OBJECT_LITERAL_MAY_ONLY_SPECIFY_KNOWN_PROPERTIES_AND_0_DOES_NOT_EXIST_IN_TYPE_1_2353, &[&pn, &ts]),
                }
                self.suppress_next_head();
            }
            return true;
        }
        // Against a union, each property also checks against that property's types in the
        // object members (their index signatures, or `undefined` when they lack it), reported
        // at the property (observed:
        // `{ foo: "x" }` against `StringKeys | NumberKeys` is `Type 'string' is not
        // assignable to type 'IValue'`, though `NumberKeys` alone accepts the structure).
        // (Not when the union has `object`, `{}` or primitives: they take any object.)
        let open = self.store().flags(t).contains(TypeFlags::UNION)
            && self
                .store()
                .members_of_union(t)
                .to_vec()
                .into_iter()
                .any(|m| {
                    let f = self.store().flags(m);
                    (f.intersects(TypeFlags::NON_PRIMITIVE | TypeFlags::PRIMITIVE)
                        && !f.intersects(TypeFlags::NULLABLE))
                        || self.is_empty_object_type(m)
                });
        if !open && self.store().flags(reduced).contains(TypeFlags::UNION) {
            let checks: Vec<TypeId> = self
                .store()
                .members_of_union(reduced)
                .iter()
                .copied()
                .filter(|&m| {
                    self.store()
                        .flags(m)
                        .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
                })
                .collect();
            let checks: Vec<(TypeId, FxHashMap<StrId, SymId>)> = checks
                .into_iter()
                .map(|m| (m, self.properties_of(m).into_iter().collect()))
                .collect();
            for (n, p) in self.properties_of(s) {
                // Hyphenated JSX attributes (`data-x`) are never checked against the props.
                if self.is_hyphenated_jsx_attr(p) || !self.written_in(p, owner) {
                    continue;
                }
                let name = self.store().str(n);
                let key = self.property_key_type(name);
                let key = if self
                    .store()
                    .flags(key)
                    .intersects(TypeFlags::ES_SYMBOL_LIKE)
                {
                    TypeId::SYMBOL
                } else if name.parse::<f64>().is_ok() {
                    TypeId::NUMBER
                } else {
                    TypeId::STRING
                };
                let mut types = Vec::new();
                for (m, props) in &checks {
                    // Own properties only (not `Object`'s `toString`).
                    if let Some(&tp) = props.get(&n) {
                        types.push(self.type_of_symbol(tp));
                    } else if let Some(ii) = self.index_info_of(*m, key) {
                        types.push(ii.value);
                    } else {
                        // A member without the property allows `undefined`.
                        types.push(TypeId::UNDEFINED);
                    }
                }
                if types.is_empty() {
                    continue;
                }
                let tpt = self.union(&types);
                let spt = self.type_of_symbol(p);
                if self.is_type_related(spt, tpt, rel) {
                    continue;
                }
                if report {
                    // Nullish array unions keep their whole-object error when no member
                    // accepts the structure (`X[] | X | undefined`).
                    if self
                        .some_member(t, |m| self.store().flags(m).intersects(TypeFlags::NULLABLE))
                        && self.some_member(t, |m| self.c.is_array_like_type(m))
                        && !self.type_related_to_some_type(s, t, rel, false)
                    {
                        return false;
                    }
                    // Reported at the whole object: `Types of property 'p' are
                    // incompatible` (elaboration reports at the property when the union
                    // or its best matching member has it).
                    self.related_to(spt, tpt, rel, true, None);
                    let pn = self.symbol_display_name(n);
                    self.report_incompatible(
                        dg::TYPES_OF_PROPERTY_0_ARE_INCOMPATIBLE_2326,
                        vec![pn],
                    );
                    if let Some(r) = self.rel.report.as_mut() {
                        r.excess_keeps_head = true;
                    }
                }
                return true;
            }
        }
        false
    }

    fn is_type_generic_target(&self, t: TypeId) -> bool {
        self.store().flags(t).intersects(TypeFlags::INSTANTIABLE)
    }

    /// The name node of an object literal member declaration.
    pub fn object_member_name(&self, d: Loc) -> Option<Loc> {
        let ast = self.ast(d.file);
        let n = match *ast.node(d.node) {
            Node::Property { key, .. } | Node::MethodDef { key, .. } => key,
            Node::ShorthandProp { name, .. } => name,
            Node::JsxAttr { name, .. } => name,
            // A JSX element's children, reported at its tag name.
            Node::JsxOpening { name, .. } => name,
            _ => return Some(d),
        };
        Some(Loc::new(d.file, n))
    }

    /// tsc-style spelling suggestion among the properties of `t`.
    /// The string literal member of the union `t` closest in spelling to the string
    /// literal `s` (printed), if any.
    fn string_literal_suggestion(&mut self, s: TypeId, t: TypeId) -> Option<String> {
        let st = self.store();
        if !st.flags(s).contains(TypeFlags::STRING_LITERAL)
            || !st.flags(t).contains(TypeFlags::UNION)
        {
            return None;
        }
        let ts_r_types::TypeKind::StringLiteral(name) = st.kind(s) else {
            return None;
        };
        let name = st.str(name).to_string();
        let mut cands: Vec<(String, TypeId)> = Vec::new();
        for &m in st.members_of_union(t) {
            if let ts_r_types::TypeKind::StringLiteral(v) = st.kind(m) {
                cands.push((st.str(v).to_string(), m));
            }
        }
        let best = spelling_suggestion(&name, cands.iter().map(|(n, _)| n.as_str()))?;
        let m = cands.iter().find(|(n, _)| *n == best)?.1;
        Some(self.type_to_string(m))
    }

    pub fn spelling_suggestion_for_property(&mut self, name: &str, t: TypeId) -> Option<String> {
        self.spelling_suggestion_symbol_for_property(name, t)
            .map(|(s, _)| s)
    }

    /// [`Ck::spelling_suggestion_for_property`] with the suggested property's symbol.
    pub fn spelling_suggestion_symbol_for_property(
        &mut self,
        name: &str,
        t: TypeId,
    ) -> Option<(String, SymId)> {
        let props = self.properties_of(t);
        let candidates: Vec<(String, SymId)> = props
            .into_iter()
            .map(|(n, s)| (self.store().str(n).to_owned(), s))
            .filter(|(s, _)| !s.starts_with("__@"))
            .collect();
        let best = spelling_suggestion(name, candidates.iter().map(|(s, _)| s.as_str()))?;
        candidates.into_iter().find(|(s, _)| *s == best)
    }

    /// Infers a generic source signature's type arguments from a target signature's
    /// parameter types (`any` where nothing is inferred).
    pub(crate) fn infer_signature_in_context(&mut self, ssig: SigId, tsig: SigId) -> Vec<TypeId> {
        let tps = self
            .store()
            .list_items(self.store().sig(ssig).type_params)
            .to_vec();
        let mut ctx = crate::infer::InferenceContext::new(&tps);
        let n = self
            .store()
            .sig(ssig)
            .params
            .len()
            .max(self.store().sig(tsig).params.len());
        for i in 0..n {
            if let (Some(sp), Some(tp)) = (self.param_type_at(ssig, i), self.param_type_at(tsig, i))
            {
                self.infer_types(&mut ctx, tp, sp, 0);
            }
        }
        let mut res: Vec<TypeId> = (0..tps.len())
            .map(|i| {
                let t = self.inferred_type_at(&mut ctx, i);
                if t == TypeId::UNKNOWN { TypeId::ANY } else { t }
            })
            .collect();
        self.constrain_inferences(&tps, &mut res);
        res
    }

    /// A type predicate as written in messages (`x is string`, `asserts this is T`).
    fn predicate_text(&mut self, p: ts_r_types::TypePredicate) -> String {
        use ts_r_types::PredicateKind as Pk;
        let name = match p.kind {
            Pk::This | Pk::AssertsThis => "this".to_owned(),
            _ => self.store().str(p.param_name).to_owned(),
        };
        let asserts = if matches!(p.kind, Pk::AssertsIdentifier | Pk::AssertsThis) {
            "asserts "
        } else {
            ""
        };
        match p.ty {
            Some(t) => format!("{asserts}{name} is {}", self.type_to_string(t)),
            None => format!("{asserts}{name}"),
        }
    }

    /// An inference (other than a type parameter) that breaks its parameter's constraint
    /// becomes the constraint.
    fn constrain_inferences(&mut self, tps: &[TypeId], res: &mut [TypeId]) {
        for i in 0..tps.len() {
            if self
                .store()
                .flags(res[i])
                .contains(TypeFlags::TYPE_PARAMETER)
            {
                continue;
            }
            if let Some(c) = self.type_param_constraint(tps[i]) {
                let m = self.store().mapper(tps, res);
                let c = self.instantiate(c, m);
                if c != TypeId::UNRESOLVED
                    && !self.is_type_related(res[i], c, RelationKind::Assignable)
                {
                    res[i] = c;
                }
            }
        }
    }
}

use ts_r_ast::Node;

fn enum_sym(st: &ts_r_types::TypeStore, t: TypeId) -> SymId {
    match st.kind(t) {
        TypeKind::Enum(s) => s,
        _ => SymId::NONE,
    }
}

/// Fresh literal types relate as their regular twins (object literal freshness is kept for
/// excess property checks).
fn fresh_literal_regular(ck: &Ck<'_, '_>, t: TypeId) -> TypeId {
    let st = ck.store();
    if st.traits(t).contains(TypeTraits::FRESH) && st.flags(t).intersects(TypeFlags::LITERAL) {
        st.regular(t)
    } else {
        t
    }
}

pub fn is_identifier_text(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c == '$' || c.is_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c == '$' || c.is_alphanumeric())
}

/// tsc's `getSpellingSuggestion`: candidates within a third of the name's length by
/// edit distance (case-insensitive exact matches first).
pub fn spelling_suggestion<'x>(
    name: &str,
    candidates: impl Iterator<Item = &'x str>,
) -> Option<String> {
    let name_len = name.chars().count();
    let maximum_length_difference = (name_len as f64 * 0.34).floor().max(2.0) as usize;
    let mut best_distance = (name_len as f64 * 0.4).floor() + 1.0;
    let mut best: Option<String> = None;
    let lower = name.to_lowercase();
    for c in candidates {
        if c == name {
            continue;
        }
        let clen = c.chars().count();
        if clen.abs_diff(name_len) > maximum_length_difference {
            continue;
        }
        if c.to_lowercase() == lower {
            return Some(c.to_owned());
        }
        if clen < 3 && c.to_lowercase() != lower {
            continue;
        }
        let d = levenshtein_with_max(name, c, best_distance - 0.1);
        if let Some(d) = d
            && d < best_distance
        {
            best_distance = d;
            best = Some(c.to_owned());
        }
    }
    best
}

fn levenshtein_with_max(s1: &str, s2: &str, max: f64) -> Option<f64> {
    let a: Vec<char> = s1.chars().collect();
    let b: Vec<char> = s2.chars().collect();
    let mut previous: Vec<f64> = (0..=b.len()).map(|i| i as f64).collect();
    let mut current = vec![0.0; b.len() + 1];
    let big = max + 0.01;
    for i in 1..=a.len() {
        let c1 = a[i - 1];
        let min_j = if (i as f64) > max {
            (i as f64 - max).ceil() as usize
        } else {
            1
        };
        let max_j = if (b.len() as f64) > max + i as f64 {
            (max + i as f64).floor() as usize
        } else {
            b.len()
        };
        current[0] = i as f64;
        let mut col_min = i as f64;
        for c in current.iter_mut().take(min_j.min(b.len() + 1)).skip(1) {
            *c = big;
        }
        for j in min_j..=max_j {
            let substitution_distance = if a[i - 1].to_lowercase().eq(b[j - 1].to_lowercase()) {
                previous[j - 1] + 0.1
            } else {
                previous[j - 1] + 2.0
            };
            let dist = if c1 == b[j - 1] {
                previous[j - 1]
            } else {
                (previous[j] + 1.0)
                    .min(current[j - 1] + 1.0)
                    .min(substitution_distance)
            };
            current[j] = dist;
            col_min = col_min.min(dist);
        }
        for c in current.iter_mut().take(b.len() + 1).skip(max_j + 1) {
            *c = big;
        }
        if col_min > max {
            return None;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    let res = previous[b.len()];
    if res > max { None } else { Some(res) }
}
