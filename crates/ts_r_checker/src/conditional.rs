//! Conditional types `C extends E ? X : Y` and `infer` type parameters.
//!
//! Behaviour (observed from tsc 7.0.2):
//! - A conditional type node resolves immediately when its check type is not generic;
//!   otherwise it is deferred and resolved again on instantiation.
//! - A naked type parameter check type distributes over unions (and `never` gives `never`);
//!   the resulting union keeps the alias.
//! - "Definitely false" is decided with every type parameter mapped to the wildcard type;
//!   "definitely true" with every type parameter mapped to an unconstrained copy. Neither
//!   decided means deferred. An `any` check type gives the union of both branches.
//! - `infer` results: the union (subtype-reduced) of covariant candidates, else the
//!   intersection of contravariant ones, else `unknown`; a result that does not satisfy the
//!   `infer X extends C` constraint becomes the constraint.
//! - Nested conditional types in branches resolve in a loop (tail recursion) up to 1000
//!   steps; beyond that TS2589 is reported.

use ts_r_ast::Node;
use ts_r_types::{AliasId, ConditionalRoot, MapperId, TypeFlags, TypeId};

use crate::checker::{Ck, Loc};
use crate::infer::InferenceContext;

/// tsc's tail-recursion limit for conditional types.
const MAX_TAIL_RECURSION: u32 = 1000;

impl<'c, 'a> Ck<'c, 'a> {
    /// The `infer` type parameters declared in a conditional type's extends clause, in
    /// source order.
    pub fn infer_params_of(&mut self, l: Loc) -> Vec<TypeId> {
        let bound = self.c.bound(l.file);
        let Some(scope) = bound.scope_of(l.node) else {
            return Vec::new();
        };
        let Some(sc) = bound.scopes.get(scope.0 as usize) else {
            return Vec::new();
        };
        let ast = self.ast(l.file);
        let mut v: Vec<(u32, TypeId)> = Vec::new();
        for &(_, s) in bound.table(sc.locals) {
            let pos = bound
                .decls(s)
                .first()
                .map_or(u32::MAX, |&d| ast.span(d).start);
            let sym = self.c.sym(l.file, s);
            v.push((pos, self.store().type_param(sym, false)));
        }
        v.sort_by_key(|e| e.0);
        v.into_iter().map(|e| e.1).collect()
    }

    /// The union of a deferred conditional type's two branches (instantiated with its
    /// mapper), or `None` for other types.
    pub fn conditional_branches_union(&mut self, t: TypeId) -> Option<TypeId> {
        let s = self.store();
        let d = s.conditional_data(t)?;
        let root = *s.conditional_root_data(d.root);
        let a = self.conditional_branch(root.decl, true);
        let b = self.conditional_branch(root.decl, false);
        let a = self.instantiate(a, d.mapper);
        let b = self.instantiate(b, d.mapper);
        Some(self.union2(a, b))
    }

    /// [`Self::conditional_branches_union`] with a true branch that is the check type
    /// narrowed by the `extends` type (`Extract<K, keyof O>` is `(K & keyof O) | never`).
    pub fn conditional_branches_union_narrowed(&mut self, t: TypeId) -> Option<TypeId> {
        let (a, b) = self.conditional_branches_narrowed(t)?;
        Some(self.union2(a, b))
    }

    /// The two branches of the deferred conditional `t`, instantiated, with a true branch
    /// that is the check type narrowed by the `extends` type (tsc's implied constraint).
    fn conditional_branches_narrowed(&mut self, t: TypeId) -> Option<(TypeId, TypeId)> {
        let s = self.store();
        let d = s.conditional_data(t)?;
        let root = *s.conditional_root_data(d.root);
        let a = self.conditional_branch(root.decl, true);
        let b = self.conditional_branch(root.decl, false);
        let mut a = self.instantiate(a, d.mapper);
        let b = self.instantiate(b, d.mapper);
        let check = self.instantiate(root.check, d.mapper);
        if a == check {
            let ext = self.instantiate(root.extends, d.mapper);
            if ext != TypeId::UNRESOLVED && !self.store().is_error(ext) {
                a = self.intersection(&[check, ext]);
            }
        }
        Some((a, b))
    }

    /// Whether the deferred conditional `t` declares `infer` type parameters.
    pub(crate) fn conditional_has_infer(&self, t: TypeId) -> bool {
        let s = self.store();
        s.conditional_data(t).is_some_and(|d| {
            let root = s.conditional_root_data(d.root);
            !s.list_items(root.infer_params).is_empty()
        })
    }

    /// The constraint of a deferred conditional type (what its members are): for a
    /// distributive one whose check type is a constrained type parameter, the conditional
    /// over that constraint unless it is `never`; otherwise the union of its branches (the
    /// other one when a branch is `any`). `None` inside a cycle.
    pub(crate) fn conditional_constraint(&mut self, t: TypeId) -> Option<TypeId> {
        let d = self.store().conditional_data(t)?;
        let key = crate::checker::Resolving::ConditionalConstraint(t);
        // Recursive conditional types make a new type at every level: bounded nesting.
        if self.cond_constraint_depth >= 4 || self.is_resolving(key) {
            return None;
        }
        self.cond_constraint_depth += 1;
        self.push_resolution(key);
        let root = *self.store().conditional_root_data(d.root);
        let mut out = None;
        let outer = self.store().list_items(root.outer_params).to_vec();
        if root.distributive
            && let Some(i) = outer.iter().position(|&p| p == root.check)
        {
            let args = self.mapped_args(&outer, d.mapper, MapperId::IDENTITY);
            let check = args[i];
            let cf = self.store().flags(check);
            let c = if cf.contains(TypeFlags::TYPE_PARAMETER) {
                self.type_param_constraint(check)
            } else if cf.intersects(TypeFlags::INSTANTIABLE) {
                Some(self.base_constraint(check))
            } else {
                None
            };
            if let Some(c) = c
                && c != check
                && c != TypeId::UNRESOLVED
                && c != TypeId::UNKNOWN
                && !self.store().is_error(c)
            {
                let members = self.union_members(c);
                let mut parts = Vec::with_capacity(members.len());
                let mut a2 = args.clone();
                for mem in members {
                    a2[i] = mem;
                    let mm = self.canonical_mapper(&outer, &a2);
                    parts.push(self.get_conditional_type(d.root, mm, AliasId::NONE));
                }
                let inst = self.union(&parts);
                if inst != t && inst != TypeId::NEVER && !self.store().is_error(inst) {
                    out = Some(inst);
                }
            }
        }
        if out.is_none()
            && let Some((a, b)) = self.conditional_branches_narrowed(t)
        {
            // `infer X` template placeholders are strings here.
            let infers = self.store().list_items(root.infer_params).to_vec();
            let strs: Vec<TypeId> = infers
                .iter()
                .filter(|&&p| self.is_template_infer_param(p))
                .copied()
                .collect();
            let (a, b) = if strs.is_empty() {
                (a, b)
            } else {
                let to = vec![TypeId::STRING; strs.len()];
                let m = self.store().mapper(&strs, &to);
                (self.instantiate(a, m), self.instantiate(b, m))
            };
            out = Some(if self.store().flags(a).contains(TypeFlags::ANY) {
                b
            } else if self.store().flags(b).contains(TypeFlags::ANY) {
                a
            } else {
                self.union2(a, b)
            });
        }
        self.pop_resolution();
        self.cond_constraint_depth -= 1;
        out.filter(|&x| x != TypeId::UNRESOLVED)
    }

    /// What a source must relate to for a deferred conditional target without `infer`
    /// parameters: the branches that can still be taken (`None` for a branch that cannot:
    /// definitely false skips the true branch, definitely true the false one). A trivial
    /// conditional (`T extends never ? never : T`, `T extends T ? T : never`) stands for its
    /// check type.
    pub fn conditional_target_branches(
        &mut self,
        source: TypeId,
        t: TypeId,
    ) -> Option<(Option<TypeId>, Option<TypeId>)> {
        let s = self.store();
        let d = s.conditional_data(t)?;
        let root = *s.conditional_root_data(d.root);
        if root.infer_params != ts_r_types::ListId::EMPTY {
            return None;
        }
        let a = self.conditional_branch(root.decl, true);
        let b = self.conditional_branch(root.decl, false);
        let a = self.instantiate(a, d.mapper);
        let b = self.instantiate(b, d.mapper);
        let check = self.instantiate(root.check, d.mapper);
        let ext = self.instantiate(root.extends, d.mapper);
        if [a, b, check, ext]
            .iter()
            .any(|&x| x == TypeId::UNRESOLVED || self.store().is_error(x) || x == TypeId::WILDCARD)
        {
            return None;
        }
        if (a == TypeId::NEVER && b == check) || (b == TypeId::NEVER && a == check) {
            return Some((Some(check), None));
        }
        // The check type itself as the source is not modelled (permissive).
        if source == check {
            return None;
        }
        let ef = self.store().flags(ext);
        let skip_true = !ef.intersects(TypeFlags::ANY_OR_UNKNOWN) && {
            let pc = self.instantiate(check, MapperId::PERMISSIVE);
            let pe = self.instantiate(ext, MapperId::PERMISSIVE);
            !self.is_assignable(pc, pe)
        };
        let skip_false = !skip_true
            && (ef.intersects(TypeFlags::ANY_OR_UNKNOWN) || {
                let rc = self.instantiate(check, MapperId::RESTRICTIVE);
                let re = self.instantiate(ext, MapperId::RESTRICTIVE);
                self.is_assignable(rc, re)
            });
        Some((
            if skip_true { None } else { Some(a) },
            if skip_false { None } else { Some(b) },
        ))
    }

    /// Whether `t` is (an instantiation of) a type parameter declared by `infer X`.
    pub fn is_infer_type_param(&mut self, t: TypeId) -> bool {
        let Some(data) = self.store().type_param_data(t) else {
            return false;
        };
        let mut sym = data.sym;
        let mut origin = data.origin;
        let mut guard = 0;
        while let Some((o, _)) = origin {
            guard += 1;
            if guard > 100 {
                break;
            }
            let Some(d) = self.store().type_param_data(o) else {
                break;
            };
            sym = d.sym;
            origin = d.origin;
        }
        if sym.is_none() {
            return false;
        }
        for d in self.c.decls(sym) {
            if let Some(p) = self.c.parent(d)
                && matches!(self.node(p), Node::InferType { .. })
            {
                return true;
            }
        }
        false
    }

    /// The type of a `ConditionalType` node (resolved now when its check type is not
    /// generic).
    pub fn conditional_type_from_node(&mut self, l: Loc, alias: AliasId) -> TypeId {
        let root = self.conditional_root_of(l);
        self.get_conditional_type(root, MapperId::IDENTITY, alias)
    }

    fn conditional_root_of(&mut self, l: Loc) -> u32 {
        let s = self.store();
        s.conditional_root(l.pack(), || {
            let (check, extends) = match *self.node(l) {
                Node::ConditionalType { check, extends, .. } => (check, extends),
                _ => {
                    return ConditionalRoot {
                        decl: l.pack(),
                        check: TypeId::UNRESOLVED,
                        extends: TypeId::UNRESOLVED,
                        infer_params: ts_r_types::ListId::EMPTY,
                        outer_params: ts_r_types::ListId::EMPTY,
                        distributive: false,
                    };
                }
            };
            let ct = self.type_from_node(Loc::new(l.file, check));
            let et = self.type_from_node(Loc::new(l.file, extends));
            let infer = self.infer_params_of(l);
            let outer = self.outer_type_params(l);
            ConditionalRoot {
                decl: l.pack(),
                check: ct,
                extends: et,
                infer_params: s.list(&infer),
                outer_params: s.list(&outer),
                distributive: s.flags(ct).contains(TypeFlags::TYPE_PARAMETER),
            }
        })
    }

    /// The true or false branch type of a conditional root (uninstantiated).
    pub(crate) fn conditional_branch(&mut self, decl: (u32, u32), true_branch: bool) -> TypeId {
        let Some(l) = Loc::unpack(decl) else {
            return TypeId::UNRESOLVED;
        };
        match *self.node(l) {
            Node::ConditionalType {
                true_ty, false_ty, ..
            } => {
                let n = if true_branch { true_ty } else { false_ty };
                self.type_from_node(Loc::new(l.file, n))
            }
            _ => TypeId::UNRESOLVED,
        }
    }

    /// A mapper from `params` to `args` (identity when nothing changes).
    pub fn canonical_mapper(&self, params: &[TypeId], args: &[TypeId]) -> MapperId {
        if params == args {
            return MapperId::IDENTITY;
        }
        self.store().mapper(params, args)
    }

    /// The outer type parameter arguments of a type with mapper `m1`, instantiated by `m2`.
    pub fn mapped_args(&mut self, params: &[TypeId], m1: MapperId, m2: MapperId) -> Vec<TypeId> {
        let s = self.store();
        params
            .iter()
            .map(|&p| {
                let cur = s.map_type(m1, p).unwrap_or(p);
                self.instantiate(cur, m2)
            })
            .collect()
    }

    /// Instantiation of a (deferred) conditional type.
    pub fn instantiate_conditional(&mut self, t: TypeId, m: MapperId) -> TypeId {
        let s = self.store();
        let Some(d) = s.conditional_data(t) else {
            return t;
        };
        let root = *s.conditional_root_data(d.root);
        let outer = s.list_items(root.outer_params).to_vec();
        if outer.is_empty() {
            return t;
        }
        let args = self.mapped_args(&outer, d.mapper, m);
        let alias = self.instantiate_alias(d.alias, m);
        let nm = self.canonical_mapper(&outer, &args);
        if root.distributive
            && let Some(i) = outer.iter().position(|&p| p == root.check)
        {
            let dist = args[i];
            let df = s.flags(dist);
            if dist != root.check && df.intersects(TypeFlags::UNION | TypeFlags::NEVER) {
                if df.contains(TypeFlags::NEVER) {
                    return TypeId::NEVER;
                }
                let members = s.members_of_union(dist).to_vec();
                let mut parts = Vec::with_capacity(members.len());
                let mut a2 = args.clone();
                for mem in members {
                    a2[i] = mem;
                    let mm = self.canonical_mapper(&outer, &a2);
                    parts.push(self.get_conditional_type(d.root, mm, AliasId::NONE));
                }
                // The union of the distributed results carries no alias (tsc prints
                // `"b" | "c"` for `Exclude<"a" | "b" | "c", "a">`).
                return self.union_with(&parts, crate::unions::Reduction::Literal, AliasId::NONE);
            }
        }
        self.get_conditional_type(d.root, nm, alias)
    }

    /// Whether a type stays deferred as a check or extends type (generic, or a simple tuple
    /// with a generic element when both sides are simple tuples).
    fn is_deferred_conditional_operand(&mut self, t: TypeId, check_tuples: bool) -> bool {
        if self.is_generic_type(t) {
            return true;
        }
        if check_tuples && self.c.is_tuple_type(t) {
            let args = self.c.type_args_of_tuple(t).to_vec();
            return args.into_iter().any(|a| self.is_generic_type(a));
        }
        false
    }

    /// `[A, B] extends [X, Y]` with tuples of the same length and plain elements.
    fn is_simple_tuple_pair(&self, decl: (u32, u32)) -> bool {
        let Some(l) = Loc::unpack(decl) else {
            return false;
        };
        let ast = self.ast(l.file);
        let Node::ConditionalType { check, extends, .. } = *ast.node(l.node) else {
            return false;
        };
        let simple = |n| -> Option<usize> {
            let mut n = n;
            while let Node::ParenType { ty } = *ast.node(n) {
                n = ty;
            }
            let Node::TupleType { elems } = *ast.node(n) else {
                return None;
            };
            let els = ast.list(elems);
            if els.is_empty() {
                return None;
            }
            for &e in els {
                match *ast.node(e) {
                    Node::OptionalType { .. } | Node::RestType { .. } => return None,
                    Node::NamedTupleMember { rest, optional, .. } if rest || optional => {
                        return None;
                    }
                    _ => {}
                }
            }
            Some(els.len())
        };
        match (simple(check), simple(extends)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    /// Resolves a conditional root under a mapper of its outer type parameters.
    pub fn get_conditional_type(
        &mut self,
        root_idx: u32,
        mapper: MapperId,
        alias: AliasId,
    ) -> TypeId {
        let s = self.store();
        let mut root_idx = root_idx;
        let mut mapper = mapper;
        let mut alias = alias;
        let mut extra: Vec<TypeId> = Vec::new();
        let mut tail = 0u32;
        let error_roots_len = self.error_infer_roots.len();
        let result = loop {
            if tail >= MAX_TAIL_RECURSION {
                self.inst_overflow = true;
                break TypeId::UNRESOLVED;
            }
            let root = *s.conditional_root_data(root_idx);
            let check = self.instantiate(root.check, mapper);
            let ext = self.instantiate(root.extends, mapper);
            // The plain error type (not an unresolved name) infers itself for every `infer`
            // type parameter, as tsc does (`error_type_inference`).
            let plain_error = check == TypeId::ERROR
                && root.infer_params != ts_r_types::ListId::EMPTY
                && !self.store().is_error(ext)
                && ext != TypeId::UNRESOLVED;
            if plain_error {
                if self.error_infer_roots.contains(&root_idx) {
                    break TypeId::ERROR;
                }
                self.error_infer_roots.push(root_idx);
            }
            // An error check type (a reported unresolved name) acts as `any`: both branches.
            let check = if self.store().is_error(check)
                && !self.store().is_error(ext)
                && ext != TypeId::UNRESOLVED
            {
                TypeId::ANY
            } else {
                check
            };
            if check == TypeId::UNRESOLVED
                || self.store().is_error(check)
                || ext == TypeId::UNRESOLVED
                || self.store().is_error(ext)
            {
                break TypeId::UNRESOLVED;
            }
            if check == TypeId::WILDCARD || ext == TypeId::WILDCARD {
                break TypeId::WILDCARD;
            }
            let tuples = self.is_simple_tuple_pair(root.decl);
            let check_deferred = self.is_deferred_conditional_operand(check, tuples);
            let infer = s.list_items(root.infer_params).to_vec();
            let mut combined: Option<MapperId> = None;
            if !infer.is_empty() {
                let mut ctx = InferenceContext::new(&infer);
                ctx.base_signatures = true;
                ctx.implicit_never = true;
                if !check_deferred {
                    self.infer_types(&mut ctx, check, ext, 0);
                }
                // The rest-position fallback only for a check type that is fully known.
                let rest_fallback = !self
                    .store()
                    .traits(check)
                    .contains(ts_r_types::TypeTraits::CONTAINS_TYPE_PARAMETER);
                let mut results = self.infer_results(&mut ctx, mapper, rest_fallback);
                if plain_error {
                    results.iter_mut().for_each(|r| *r = TypeId::ERROR);
                }
                let (src, dst) = s.mapper_lists(mapper);
                let mut src = src.to_vec();
                let mut dst = dst.to_vec();
                src.extend(infer.iter().copied());
                dst.extend(results);
                combined = Some(s.mapper(&src, &dst));
            }
            let inferred_ext = match combined {
                Some(cm) => self.instantiate(root.extends, cm),
                None => ext,
            };
            if !check_deferred && !self.is_deferred_conditional_operand(inferred_ext, tuples) {
                let ef = s.flags(inferred_ext);
                let check_any = s.flags(check).contains(TypeFlags::ANY);
                // A string literal that does not match a template extends type (`"c"` against
                // `${infer H}.${infer R}`) takes the false branch.
                let no_match = !infer.is_empty()
                    && s.flags(ext).contains(TypeFlags::TEMPLATE_LITERAL)
                    && s.flags(check).contains(TypeFlags::STRING_LITERAL)
                    && self.infer_from_template(check, ext).is_none();
                if no_match
                    || !ef.intersects(TypeFlags::ANY_OR_UNKNOWN) && {
                        check_any || {
                            let pc = self.instantiate(check, MapperId::PERMISSIVE);
                            let pe = self.instantiate(inferred_ext, MapperId::PERMISSIVE);
                            !self.is_assignable(pc, pe)
                        }
                    }
                {
                    if check_any {
                        let tt = self.conditional_branch(root.decl, true);
                        let r = self.instantiate(tt, combined.unwrap_or(mapper));
                        extra.push(r);
                    }
                    let ft = self.conditional_branch(root.decl, false);
                    if let Some(fd) = s.conditional_data(ft) {
                        let nr = *s.conditional_root_data(fd.root);
                        let nested = Loc::unpack(nr.decl)
                            .and_then(|l| self.c.parent(l))
                            .is_some_and(|p| {
                                let mut p = p;
                                while let Node::ParenType { .. } = *self.node(p) {
                                    match self.c.parent(p) {
                                        Some(q) => p = q,
                                        None => break,
                                    }
                                }
                                p.pack() == root.decl
                            });
                        if nested
                            && fd.mapper == MapperId::IDENTITY
                            && (!nr.distributive || nr.check == root.check)
                        {
                            root_idx = fd.root;
                            tail += 1;
                            continue;
                        }
                        if let Some((r2, m2)) = self.tail_recursion_target(ft, mapper) {
                            root_idx = r2;
                            mapper = m2;
                            alias = AliasId::NONE;
                            tail += 1;
                            continue;
                        }
                    }
                    break self.instantiate(ft, mapper);
                }
                let definitely_true = ef.intersects(TypeFlags::ANY_OR_UNKNOWN) || {
                    let rc = self.instantiate(check, MapperId::RESTRICTIVE);
                    let re = self.instantiate(inferred_ext, MapperId::RESTRICTIVE);
                    self.is_assignable(rc, re)
                };
                if definitely_true {
                    let tt = self.conditional_branch(root.decl, true);
                    let tm = combined.unwrap_or(mapper);
                    if let Some((r2, m2)) = self.tail_recursion_target(tt, tm) {
                        root_idx = r2;
                        mapper = m2;
                        alias = AliasId::NONE;
                        tail += 1;
                        continue;
                    }
                    break self.instantiate(tt, tm);
                }
            }
            // Deferred: keep only the mapping of the outer type parameters.
            let outer = s.list_items(root.outer_params).to_vec();
            let args: Vec<TypeId> = outer
                .iter()
                .map(|&p| s.map_type(mapper, p).unwrap_or(p))
                .collect();
            let cm = self.canonical_mapper(&outer, &args);
            break s.conditional(root_idx, cm, alias);
        };
        self.error_infer_roots.truncate(error_roots_len);
        if extra.is_empty() {
            return result;
        }
        extra.push(result);
        self.union(&extra)
    }

    /// When a branch is itself a conditional type that can be resolved in the same loop:
    /// its root and the mapper of its outer type parameters.
    fn tail_recursion_target(&mut self, t: TypeId, m: MapperId) -> Option<(u32, MapperId)> {
        let s = self.store();
        let d = s.conditional_data(t)?;
        let nr = *s.conditional_root_data(d.root);
        let outer = s.list_items(nr.outer_params).to_vec();
        if outer.is_empty() {
            return None;
        }
        let args = self.mapped_args(&outer, d.mapper, m);
        if nr.distributive
            && let Some(i) = outer.iter().position(|&p| p == nr.check)
        {
            let c = args[i];
            if c != nr.check && s.flags(c).intersects(TypeFlags::UNION | TypeFlags::NEVER) {
                return None;
            }
        }
        Some((d.root, self.canonical_mapper(&outer, &args)))
    }

    /// The inferred types of a conditional type's `infer` parameters.
    fn infer_results(
        &mut self,
        ctx: &mut InferenceContext,
        outer: MapperId,
        rest_fallback: bool,
    ) -> Vec<TypeId> {
        let n = ctx.params.len();
        let mut out: Vec<TypeId> = Vec::with_capacity(n);
        for i in 0..n {
            let mut cands = ctx.candidates[i].clone();
            if cands.is_empty() && ctx.contra[i].is_empty() {
                cands = ctx.weak[i].clone();
            }
            let contra = ctx.contra[i].clone();
            let inferred = if !cands.is_empty() {
                Some(self.union_subtype(&cands))
            } else if !contra.is_empty() {
                Some(self.intersection(&contra))
            } else {
                None
            };
            let tp = ctx.params[i];
            // Nothing inferred for an `infer` in a rest position: `unknown[]`; for a template
            // placeholder, `string`.
            let fallback = if inferred.is_none() && self.is_template_infer_param(tp) {
                TypeId::STRING
            } else if inferred.is_none() && rest_fallback {
                self.store()
                    .type_param_data(tp)
                    .map(|d| d.sym)
                    .and_then(|sym| self.c.decls(sym).first().copied())
                    .and_then(|d| self.implied_infer_constraint(d, true))
                    .unwrap_or(TypeId::UNKNOWN)
            } else {
                TypeId::UNKNOWN
            };
            out.push(inferred.unwrap_or(fallback));
            ctx.fixed[i] = Some(out[i]);
            if let Some(c) = self.type_param_constraint(tp) {
                let s = self.store();
                let (src, dst) = s.mapper_lists(outer);
                let mut src = src.to_vec();
                let mut dst = dst.to_vec();
                for (j, &p) in ctx.params.iter().enumerate() {
                    src.push(p);
                    dst.push(if j == i {
                        inferred.unwrap_or(TypeId::UNKNOWN)
                    } else {
                        ctx.fixed[j].unwrap_or(TypeId::UNKNOWN)
                    });
                }
                let m = s.mapper(&src, &dst);
                let ic = self.instantiate(c, m);
                let ok = match inferred {
                    Some(t) => self.is_assignable(t, ic),
                    None => false,
                };
                if !ok {
                    out[i] = ic;
                    ctx.fixed[i] = Some(ic);
                }
            }
        }
        out
    }
}
