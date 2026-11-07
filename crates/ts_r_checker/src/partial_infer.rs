//! Partial inference from context-sensitive literal arguments to reverse mapped types.
//!
//! Before its context-sensitive functions are typed, an object or array literal argument
//! whose parameter type is a generic homomorphic mapped type (`{ [K in keyof T]: X }`)
//! still infers `T` from what it has: its other members, with each context-sensitive
//! function standing for a type that infers nothing. The keys of such functions reverse-map
//! to `unknown`. A literal with nothing inferable at any depth infers nothing at all. Then,
//! when its functions are typed, `T` is fixed if their contextual parameter types read it,
//! and the partial candidates give way to the whole literal's otherwise (tsc:
//! `createReverseMappedType` and `isPartiallyInferableType` in
//! `tsc/internal/checker/inference.go`; the checker's AGENTS.md, "Partial reverse mapped
//! inference").

use ts_r_ast::{MethodKind, Node};
use ts_r_binder::SymbolFlags;
use ts_r_types::{ObjectFlags, ObjectKind, ResolvedMembers, SymId, TypeFlags, TypeId, TypeTraits};

use crate::checker::{Ck, Loc};
use crate::declared::TupleEl;
use crate::infer::InferenceContext;
use crate::symbols::{SynthSym, check_flags};

/// What a partial inference of one argument added to the inference context: per type
/// parameter, the candidates (covariant, contravariant, weak) it contributed.
#[derive(Clone)]
pub(crate) struct PartialInference {
    /// The reverse-mapped type parameter `T`.
    pub tv: TypeId,
    /// The literal's partial type.
    pub partial: TypeId,
    pub added: Vec<(usize, TypeId)>,
}

impl<'c, 'a> Ck<'c, 'a> {
    /// The type parameter `T` (one of `params`) of a parameter type `pt` that is, without
    /// `null` and `undefined`, a generic `{ [K in keyof T]: X }` (no `as` clause).
    pub(crate) fn reverse_mapped_param(&mut self, pt: TypeId, params: &[TypeId]) -> Option<TypeId> {
        let ptn = self.non_nullable(pt);
        let info = self.mapped_info(ptn)?;
        if info.name_node.is_some() {
            return None;
        }
        let c = self.mapped_constraint(&info);
        match self.store().kind(c) {
            ts_r_types::TypeKind::Index(tv) if params.contains(&tv) => Some(tv),
            _ => None,
        }
    }

    /// The type standing for a context-sensitive function in a partial literal type.
    fn non_inferrable_marker(&mut self) -> TypeId {
        if let Some(t) = self.non_inferrable_fn {
            return t;
        }
        let t = self.store().literal_object(
            SymId::NONE,
            ObjectFlags::ANONYMOUS | ObjectFlags::NON_INFERRABLE,
            ResolvedMembers::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()),
            TypeTraits::NONE,
        );
        self.non_inferrable_fn = Some(t);
        t
    }

    /// First pass over a context-sensitive literal argument `l` whose parameter type `pt`
    /// reverse-maps to `T`: infers from its partial type and returns what was added.
    /// `None` when the literal has members this does not model (spreads, accessors,
    /// computed keys that are not literals): nothing is inferred then.
    pub(crate) fn infer_partial_literal(
        &mut self,
        ctx: &mut InferenceContext,
        l: Loc,
        pt: TypeId,
    ) -> Option<PartialInference> {
        let tv = self.reverse_mapped_param(pt, &ctx.params)?;
        let ptn = self.non_nullable(pt);
        let (partial, inferable) = self.partial_literal_type(l, Some(ptn), 0)?;
        let mut out = PartialInference {
            tv,
            partial,
            added: Vec::new(),
        };
        // (tsc reverse-maps only a source with an inferable member somewhere.)
        if !inferable || self.properties_of(partial).is_empty() {
            return Some(out);
        }
        let n = ctx.params.len();
        let before: Vec<(usize, usize, usize)> = (0..n)
            .map(|j| {
                (
                    ctx.candidates[j].len(),
                    ctx.contra[j].len(),
                    ctx.weak[j].len(),
                )
            })
            .collect();
        self.infer_types(ctx, partial, ptn, 0);
        let marker = self.non_inferrable_marker();
        let jt = ctx.params.iter().position(|&p| p == tv);
        for (j, &(c0, x0, w0)) in before.iter().enumerate() {
            for (list, from) in [
                (&mut ctx.candidates[j], c0),
                (&mut ctx.contra[j], x0),
                (&mut ctx.weak[j], w0),
            ] {
                let added: Vec<TypeId> = list.drain(from..).collect();
                for t in added {
                    let mut budget = 256;
                    let t = if !self.mentions_marker(t, marker, 0, &mut budget) {
                        Some(t)
                    } else if Some(j) == jt {
                        self.unknown_for_marker(t, marker)
                    } else {
                        None
                    };
                    if let Some(t) = t
                        && !list.contains(&t)
                    {
                        list.push(t);
                        out.added.push((j, t));
                    }
                }
            }
        }
        Some(out)
    }

    /// A reverse-mapped candidate for `T` with the members that hold a context-sensitive
    /// function (at any depth) typed `unknown`: tsc infers nothing from a non-inferrable
    /// source to `T[K]` (`{ [K in keyof T]: T[K] }` with `{ a: 1, b: (v) => v }` gives `{
    /// a: number; b: unknown }`). `None` for other shapes.
    fn unknown_for_marker(&mut self, t: TypeId, marker: TypeId) -> Option<TypeId> {
        let clean = |ck: &mut Self, x: TypeId| {
            let mut budget = 256;
            if ck.mentions_marker(x, marker, 0, &mut budget) {
                TypeId::UNKNOWN
            } else {
                x
            }
        };
        if self.c.is_array_type(t) {
            let el = *self.c.type_args_of(t).first()?;
            let el = clean(self, el);
            let readonly = self.is_readonly_array_like(t);
            return Some(self.create_array(el, readonly));
        }
        if let Some(shape) = self.store().tuple_shape_of(t) {
            let elements = shape.elements.to_vec();
            let readonly = shape.readonly;
            let types = self.c.type_args_of(t).to_vec();
            let mut els = Vec::with_capacity(elements.len());
            for (i, e) in elements.iter().enumerate() {
                let ty = types.get(i).copied().unwrap_or(TypeId::UNKNOWN);
                els.push(TupleEl {
                    ty: clean(self, ty),
                    kind: e.kind,
                    label: e.label,
                });
            }
            return Some(self.create_tuple(&els, readonly));
        }
        let oflags = match self.store().object_of(t) {
            Some(o) if matches!(o.kind, ObjectKind::Literal { .. }) => o.flags,
            _ => return None,
        };
        let traits = self.store().traits(t) & TypeTraits::CONTAINS_OBJECT_LITERAL;
        let mut props = Vec::new();
        for (n, p) in self.properties_of(t) {
            let pt = self.type_of_symbol(p);
            let pt = clean(self, pt);
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
        Some(self.store().literal_object(
            SymId::NONE,
            oflags,
            ResolvedMembers::new(props, Vec::new(), Vec::new(), infos),
            traits,
        ))
    }

    /// Removes what a partial inference added for the type parameters not fixed since.
    pub(crate) fn undo_partial_inference(
        &mut self,
        ctx: &mut InferenceContext,
        p: &PartialInference,
    ) {
        for &(j, t) in &p.added {
            if ctx.fixed[j].is_some() {
                continue;
            }
            ctx.candidates[j].retain(|&c| c != t);
            ctx.contra[j].retain(|&c| c != t);
            ctx.weak[j].retain(|&c| c != t);
        }
    }

    /// What `T` stands for in the contextual type of a literal whose partial inference
    /// fixed `T` to `unknown`: the literal's keys typed `unknown`, so each member's
    /// contextual type reads `unknown` (tsc indexes the fixed `unknown`).
    pub(crate) fn unknown_keys_of_partial(&mut self, p: &PartialInference) -> Option<TypeId> {
        self.reverse_mapped_unknown(p.partial)
    }

    /// The contextual call signatures of the context-sensitive functions in the literal
    /// `l` typed by `ct` (other members are skipped).
    pub(crate) fn sensitive_fn_sigs(
        &mut self,
        l: Loc,
        ct: TypeId,
        depth: u32,
        out: &mut Vec<ts_r_types::SigId>,
    ) {
        if depth > 8 || out.len() > 64 {
            return;
        }
        let ast = self.ast(l.file);
        let n = crate::nodes::skip_parens(ast, l.node);
        match *ast.node(n) {
            Node::Arrow { .. } | Node::FunctionExpr { .. } => {
                if self.is_context_sensitive_function(Loc::new(l.file, n)) {
                    let t = self.non_nullable(ct);
                    out.extend(self.signatures_of(t, crate::members::SigKind::Call));
                }
            }
            Node::ArrayLit { elements } => {
                for (i, &x) in ast.list(elements).to_vec().iter().enumerate() {
                    let xl = Loc::new(l.file, x);
                    if !self.is_context_sensitive_expr(xl) {
                        continue;
                    }
                    if let Some(t) = self.contextual_element_type(ct, i, false, None) {
                        self.sensitive_fn_sigs(xl, t, depth + 1, out);
                    }
                }
            }
            Node::ObjectLit { props } => {
                for &p in ast.list(props).to_vec().iter() {
                    let (key, value) = match *self.ast(l.file).node(p) {
                        Node::Property { key, value, .. } | Node::MethodDef { key, value, .. } => {
                            (key, value)
                        }
                        _ => continue,
                    };
                    let vl = Loc::new(l.file, value);
                    if !self.is_context_sensitive_expr(vl) {
                        continue;
                    }
                    if let Some(t) = self.contextual_member_type(ct, Loc::new(l.file, key)) {
                        self.sensitive_fn_sigs(vl, t, depth + 1, out);
                    }
                }
            }
            _ => {}
        }
    }

    /// The type of the literal `l` (contextually typed by `ct`) with its context-sensitive
    /// functions replaced by the non-inferrable marker, and whether anything in it is
    /// inferable. `None` for what is not modelled.
    fn partial_literal_type(
        &mut self,
        l: Loc,
        ct: Option<TypeId>,
        depth: u32,
    ) -> Option<(TypeId, bool)> {
        if depth > 8 {
            return None;
        }
        if !self.is_context_sensitive_expr(l) {
            let t = match ct {
                Some(c) => self.speculative_arg_type(l, c),
                None => self.quietly(|ck| ck.check_expr(l)),
            };
            // As a member of the literal: its literal types widen unless in a const
            // context or the contextual type takes them (tsc's
            // `checkExpressionForMutableLocation`).
            let t = if depth == 0 {
                t
            } else if self.in_const_context(l) {
                self.regular_of_literal(t)
            } else if let Some(c) = ct
                && self.is_literal_of_contextual_type(t, c)
            {
                self.regular_of_literal(t)
            } else {
                self.widen_literal(t)
            };
            return Some((t, true));
        }
        let ast = self.ast(l.file);
        let n = crate::nodes::skip_parens(ast, l.node);
        match *ast.node(n) {
            Node::Arrow { .. } | Node::FunctionExpr { .. } => {
                Some((self.non_inferrable_marker(), false))
            }
            Node::ObjectLit { props } => {
                let mut members = Vec::new();
                let mut inferable = false;
                for &p in ast.list(props).to_vec().iter() {
                    let (key, value) = match *self.ast(l.file).node(p) {
                        Node::Property { key, value, .. } => (key, value),
                        Node::MethodDef {
                            kind: MethodKind::Method,
                            optional: false,
                            key,
                            value,
                            ..
                        } => (key, value),
                        _ => return None,
                    };
                    let kl = Loc::new(l.file, key);
                    let name = self.member_key_name(kl)?;
                    let pct = match ct {
                        Some(c) => self.contextual_member_type(c, kl),
                        None => None,
                    };
                    let (vt, inf) =
                        self.partial_literal_type(Loc::new(l.file, value), pct, depth + 1)?;
                    inferable |= inf;
                    let nid = self.store().intern_str(&name);
                    if members.iter().any(|&(m, _)| m == nid) {
                        return None;
                    }
                    let sym = self.c.new_synth(
                        SynthSym::new(nid, SymbolFlags::PROPERTY, check_flags::OBJECT_LITERAL)
                            .with_type(vt),
                    );
                    members.push((nid, sym));
                }
                let t = self.store().literal_object(
                    SymId::NONE,
                    ObjectFlags::OBJECT_LITERAL,
                    ResolvedMembers::new(members, Vec::new(), Vec::new(), Vec::new()),
                    TypeTraits::CONTAINS_OBJECT_LITERAL,
                );
                Some((t, inferable))
            }
            Node::ArrayLit { elements } => {
                let tuple = match ct {
                    Some(c) => self.is_generic_mapped_type(c) || self.is_tuple_like_context(c),
                    None => false,
                };
                let mut els = Vec::new();
                let mut inferable = false;
                for (i, &x) in ast.list(elements).to_vec().iter().enumerate() {
                    if matches!(
                        self.ast(l.file).node(x),
                        Node::Spread { .. } | Node::Hole {}
                    ) {
                        return None;
                    }
                    let ect = match ct {
                        Some(c) => self.contextual_element_type(c, i, false, None),
                        None => None,
                    };
                    let (et, inf) =
                        self.partial_literal_type(Loc::new(l.file, x), ect, depth + 1)?;
                    inferable |= inf;
                    els.push(et);
                }
                let t = if tuple {
                    let els: Vec<TupleEl> = els
                        .into_iter()
                        .map(|ty| TupleEl {
                            ty,
                            kind: ts_r_types::TupleElement::REQUIRED,
                            label: None,
                        })
                        .collect();
                    self.create_tuple(&els, false)
                } else {
                    let u = self.union(&els);
                    self.create_array(u, false)
                };
                Some((t, inferable))
            }
            _ => None,
        }
    }

    /// Whether `t` contains the marker: through unions, intersections, array and tuple
    /// elements and the properties of object literal types (what partial and reverse
    /// mapped types are built from).
    fn mentions_marker(&mut self, t: TypeId, marker: TypeId, depth: u32, budget: &mut u32) -> bool {
        if t == marker {
            return true;
        }
        if depth > 10 || *budget == 0 {
            return false;
        }
        *budget -= 1;
        let s = self.store();
        let f = s.flags(t);
        if f.intersects(TypeFlags::UNION | TypeFlags::INTERSECTION) {
            let ms = s.members_of_union(t).to_vec();
            return ms
                .into_iter()
                .any(|m| self.mentions_marker(m, marker, depth + 1, budget));
        }
        if !f.contains(TypeFlags::OBJECT) {
            return false;
        }
        if self.c.is_array_type(t) || self.store().tuple_shape_of(t).is_some() {
            let args = self.c.type_args_of(t).to_vec();
            return args
                .into_iter()
                .any(|a| self.mentions_marker(a, marker, depth + 1, budget));
        }
        let literal = self
            .store()
            .object_of(t)
            .is_some_and(|o| matches!(o.kind, ObjectKind::Literal { .. }));
        if !literal {
            return false;
        }
        for (_, p) in self.properties_of(t) {
            let pt = self.type_of_symbol(p);
            if self.mentions_marker(pt, marker, depth + 1, budget) {
                return true;
            }
        }
        false
    }
}
