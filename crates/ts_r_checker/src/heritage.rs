//! Heritage compatibility of classes and interfaces (TS2415, TS2416, TS2417, TS2420, TS2422,
//! TS2430, TS2720, TS2320).
//!
//! Rules observed from tsc 7.0.2:
//! - A class instance type (with `this` bound to itself) must be assignable to its base
//!   class type and to each implemented type. When it is not, every member written in the
//!   class body (not static, not a parameter property) whose type is not assignable to the
//!   same property of the base gets TS2416 at its name; when no member is to blame, the
//!   class gets TS2415 (extends), TS2420 (implements an interface or type) or TS2720
//!   (implements a class) at its name with the relation chain.
//! - The static side is compared (TS2417, properties only) only when the instance side
//!   passed.
//! - A class whose `extends` chain of class names leads back to itself: TS2506 at its
//!   name (each class in the cycle, not those extending into it).
//! - An interface is checked once, at the name of its first declaration: TS2320 when two
//!   bases bring non-identical properties it does not redeclare, otherwise TS2430 for each
//!   base it is not assignable to.

use ts_r_ast::{HeritageKind, MethodKind, Modifiers, Node};
use ts_r_binder::SymbolFlags;
use ts_r_types::{ObjectFlags, RelationKind, ResolvedMembers, StrId, SymId, TypeFlags, TypeId};

use crate::checker::{Ck, Loc};
use crate::diag::{Chain, dg};
use crate::members::SigKind;
use crate::nodes::key_text;
use crate::relate::Head;

/// Structural comparisons one heritage check may spend (see `RelState::budget`).
const RELATION_BUDGET: u32 = 20_000;

impl<'c, 'a> Ck<'c, 'a> {
    /// TS2506 at the name of class `c` when its `extends` chain of class names leads back
    /// to it (classes extending into a cycle from outside are not reported).
    pub(crate) fn check_base_cycle(&mut self, c: Loc) {
        if !self.reporting(c.file) {
            return;
        }
        let Some(start) = self.c.sym_of_decl(c) else {
            return;
        };
        let mut cur = start;
        let mut seen: Vec<SymId> = Vec::new();
        for _ in 0..64 {
            let Some(next) = self.base_class_symbol(cur) else {
                return;
            };
            if next == start {
                let ast = self.ast(c.file);
                let (Node::ClassDecl { name: Some(n), .. } | Node::ClassExpr { name: Some(n), .. }) =
                    *ast.node(c.node)
                else {
                    return;
                };
                let name = self.c.sym_name(start).to_owned();
                self.error(
                    Loc::new(c.file, n),
                    dg::_0_IS_REFERENCED_DIRECTLY_OR_INDIRECTLY_IN_ITS_OWN_BASE_EXPRESSION_2506,
                    &[&name],
                );
                return;
            }
            if seen.contains(&next) {
                return;
            }
            seen.push(next);
            cur = next;
        }
    }

    /// The class an `extends` clause of class `s` names (an entity name resolving to a
    /// class), if any.
    fn base_class_symbol(&mut self, s: SymId) -> Option<SymId> {
        let d = self.c.decls(s).into_iter().find(|d| {
            matches!(
                self.node(*d),
                Node::ClassDecl { .. } | Node::ClassExpr { .. }
            )
        })?;
        let ast = self.ast(d.file);
        let (Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. }) =
            *ast.node(d.node)
        else {
            return None;
        };
        for &h in ast.list(heritage) {
            if let Node::HeritageClause {
                kind: HeritageKind::Extends,
                types,
            } = *ast.node(h)
                && let Some(&first) = ast.list(types).first()
                && let Node::ExprWithTypeArgs { expr, .. } = *ast.node(first)
            {
                if !crate::nodes::is_entity_name_expr(ast, expr) {
                    return None;
                }
                let b = self.resolve_entity(Loc::new(d.file, expr), ts_r_binder::meaning::VALUE)?;
                let b = self.c.resolve_alias(b)?;
                return self
                    .c
                    .sym_flags(b)
                    .intersects(SymbolFlags::CLASS)
                    .then_some(b);
            }
        }
        None
    }

    /// Runs `f` with a relation budget; returns its result and whether the budget ran out.
    fn budgeted<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> (R, bool) {
        let saved = self.rel.budget.replace(RELATION_BUDGET);
        let r = f(self);
        let spent = self.rel.budget == Some(0);
        self.rel.budget = saved;
        (r, spent)
    }

    /// Budgeted assignability; a spent budget counts as "related".
    fn heritage_assignable(&mut self, s: TypeId, t: TypeId) -> bool {
        let (r, spent) = self.budgeted(|ck| ck.is_assignable(s, t));
        r || spent
    }

    /// Budgeted error chain of a failed comparison (`None` when the budget ran out).
    fn heritage_chain(&mut self, s: TypeId, t: TypeId, head: Option<Head>) -> Option<Chain> {
        let (r, spent) = self.budgeted(|ck| {
            ck.relation_error_chain(s, t, RelationKind::Assignable, head)
                .0
        });
        (!spent).then_some(r)
    }

    /// The class instance type with `this` bound to the class.
    fn type_with_this(&mut self, sym: SymId) -> TypeId {
        let d = self.declared_type_of_symbol(sym);
        let this_t = self.store().type_param(sym, true);
        self.with_this_arg(d, this_t)
    }

    /// The class or interface symbol of a (reference to a) declared type.
    pub(crate) fn declared_sym_of(&self, t: TypeId) -> Option<SymId> {
        let st = self.store();
        let o = st.object_of(t)?;
        let o = match o.kind {
            ts_r_types::ObjectKind::Reference { target, .. } => st.object_of(target)?,
            _ => o,
        };
        match o.kind {
            ts_r_types::ObjectKind::Declared { sym, .. } => Some(sym),
            _ => None,
        }
    }

    /// Whether the base types of `sym` lead back to `sym` (tsc reports TS2506/TS2310 and
    /// skips the heritage checks). `None` means the traversal budget ran out.
    fn has_circular_bases(&mut self, sym: SymId) -> Option<bool> {
        let mut stack: Vec<SymId> = vec![sym];
        let mut seen: Vec<SymId> = Vec::new();
        while let Some(s) = stack.pop() {
            if seen.len() > 256 {
                return None;
            }
            for b in self.base_types(s).to_vec() {
                let parts: Vec<TypeId> = if self.store().flags(b).contains(TypeFlags::INTERSECTION)
                {
                    self.store().members_of_union(b).to_vec()
                } else {
                    vec![b]
                };
                for p in parts {
                    let Some(bs) = self.declared_sym_of(p) else {
                        continue;
                    };
                    if bs == sym {
                        return Some(true);
                    }
                    if !seen.contains(&bs) {
                        seen.push(bs);
                        stack.push(bs);
                    }
                }
            }
        }
        Some(false)
    }

    /// TS2310 for interface declaration `l` with an `extends` clause whose symbol's base
    /// types lead back to it (every such declaration of the symbol is reported).
    pub(crate) fn check_circular_interface_bases(&mut self, l: Loc) {
        if !self.reporting(l.file) {
            return;
        }
        let Node::Interface { name, extends, .. } = *self.node(l) else {
            return;
        };
        if extends.is_empty() {
            return;
        }
        let Some(sym) = self.c.sym_of_decl(l) else {
            return;
        };
        if self.has_circular_bases(sym) != Some(true) {
            return;
        }
        let t = self.declared_type_of_symbol(sym);
        let shown = self.type_to_string(t);
        self.error(
            Loc::new(l.file, name),
            dg::TYPE_0_RECURSIVELY_REFERENCES_ITSELF_AS_A_BASE_TYPE_2310,
            &[&shown],
        );
    }

    /// TS2312 at each `extends` entry of interface declaration `l` whose type is not an
    /// object type (or intersection of them) with statically known members: a union, a
    /// primitive, a type parameter without an object constraint, a generic mapped type.
    pub(crate) fn check_interface_base_kinds(&mut self, l: Loc) {
        if !self.reporting(l.file) {
            return;
        }
        let Node::Interface { extends, .. } = *self.node(l) else {
            return;
        };
        let entries: Vec<_> = self.ast(l.file).list(extends).to_vec();
        for e in entries {
            let at = Loc::new(l.file, e);
            let t = self.type_from_node(at);
            if self.store().is_error(t) || t == TypeId::UNRESOLVED {
                continue;
            }
            let t = self.reduced_type(t);
            if !self.is_valid_base_type(t, 0) {
                self.error(
                    at,
                    dg::AN_INTERFACE_CAN_ONLY_EXTEND_AN_OBJECT_TYPE_OR_INTERSECTION_OF_OBJECT_TYPES_WITH_2312,
                    &[],
                );
            }
        }
    }

    /// Whether `t` may be an interface's base type (tsc: checker/checker.go,
    /// isValidBaseType): an object type that is not a generic mapped type, `object`, `any`,
    /// an intersection of valid types, or a type parameter whose base constraint is one.
    pub(crate) fn is_valid_base_type(&mut self, t: TypeId, depth: u32) -> bool {
        if depth > 32 {
            return true;
        }
        let f = self.store().flags(t);
        if f.contains(TypeFlags::TYPE_PARAMETER) {
            let c = self.base_constraint(t);
            if c != t && c != TypeId::UNRESOLVED && c != TypeId::UNKNOWN {
                return self.is_valid_base_type(c, depth + 1);
            }
            return false;
        }
        if f.intersects(TypeFlags::OBJECT | TypeFlags::NON_PRIMITIVE | TypeFlags::ANY) {
            return !self.is_generic_mapped_type(t);
        }
        if f.contains(TypeFlags::INTERSECTION) {
            let members = self.store().members_of_union(t).to_vec();
            return members
                .into_iter()
                .all(|m| self.is_valid_base_type(m, depth + 1));
        }
        false
    }

    /// Whether `t` can't take part in a heritage comparison (`any`, errors, unresolved).
    fn heritage_skip(&self, t: TypeId) -> bool {
        self.store().is_error(t)
            || t == TypeId::UNRESOLVED
            || self
                .store()
                .flags(t)
                .intersects(TypeFlags::ANY | TypeFlags::UNKNOWN | TypeFlags::NEVER)
    }

    /// TS2415/TS2416/TS2417/TS2420/TS2422/TS2720 for class `l`.
    pub(crate) fn check_class_heritage(&mut self, l: Loc) {
        let saved = self.print_scope.replace(l);
        self.check_class_heritage_in(l);
        self.print_scope = saved;
    }

    fn check_class_heritage_in(&mut self, l: Loc) {
        let ast = self.ast(l.file);
        let (name, heritage, members) = match *ast.node(l.node) {
            Node::ClassDecl {
                name,
                heritage,
                members,
                ..
            }
            | Node::ClassExpr {
                name,
                heritage,
                members,
                ..
            } => (name, heritage, members),
            _ => return,
        };
        if ast.list(heritage).is_empty() {
            return;
        }
        let Some(sym) = self.c.sym_of_decl(l) else {
            return;
        };
        let at = name.map_or(l, |n| Loc::new(l.file, n));
        if self.has_circular_bases(sym) != Some(false) {
            return;
        }
        let derived = self.type_with_this(sym);
        if self.heritage_skip(derived) {
            return;
        }
        let this_t = self.store().type_param(sym, true);
        let member_list: Vec<_> = ast.list(members).to_vec();
        // `extends`.
        let has_extends = ast.list(heritage).iter().any(|&h| {
            matches!(
                ast.node(h),
                Node::HeritageClause {
                    kind: HeritageKind::Extends,
                    ..
                }
            )
        });
        if has_extends && let Some(&base) = self.base_types(sym).first() {
            let base = self.with_this_arg(base, this_t);
            // A base intersection reduced to `never` (conflicting privates): TS2509 at the
            // `extends` expression, saying why.
            if self.reduced_type(base) == TypeId::NEVER
                && let Some((info, ts, pname)) = self.never_reduction_reason(base)
            {
                let expr = ast.list(heritage).iter().find_map(|&h| match *ast.node(h) {
                    Node::HeritageClause {
                        kind: HeritageKind::Extends,
                        types,
                    } => ast.list(types).first().copied(),
                    _ => None,
                });
                if let Some(x) = expr {
                    let mut chain = Chain::new();
                    chain.push_back(
                        dg::BASE_CONSTRUCTOR_RETURN_TYPE_0_IS_NOT_AN_OBJECT_TYPE_OR_INTERSECTION_OF_OBJECT_T_2509,
                        &["never"],
                    );
                    chain.push_back(info, &[&ts, &pname]);
                    self.error_chain(Loc::new(l.file, x), &chain);
                }
                return;
            }
            // Extending a class whose constructor is `private` outside that class (TS2675).
            let extends_node = ast.list(heritage).iter().find_map(|&h| match *ast.node(h) {
                Node::HeritageClause {
                    kind: HeritageKind::Extends,
                    types,
                } => ast.list(types).first().copied(),
                _ => None,
            });
            if let Some(x) = extends_node
                && let Node::ExprWithTypeArgs { expr, .. } = *ast.node(x)
            {
                let ct = self.quietly(|ck| ck.check_expr(Loc::new(l.file, expr)));
                if let Some((dc, true)) = self.restricted_constructor(ct) {
                    let mut inside = false;
                    let mut cur = self.enclosing_class(l);
                    let mut guard = 0;
                    while let Some(c) = cur {
                        guard += 1;
                        if guard > 64 {
                            break;
                        }
                        if c == dc {
                            inside = true;
                            break;
                        }
                        cur = self.enclosing_class(c);
                    }
                    if !inside && let Some(bs) = self.c.sym_of_decl(dc) {
                        let bt = self.declared_type_of_symbol(bs);
                        let saved = std::mem::replace(&mut self.print_qualified, true);
                        let bn = self.type_to_string(bt);
                        self.print_qualified = saved;
                        self.error(
                            Loc::new(l.file, x),
                            dg::CANNOT_EXTEND_A_CLASS_0_CLASS_CONSTRUCTOR_IS_MARKED_AS_PRIVATE_2675,
                            &[&bn],
                        );
                    }
                }
            }
            if !self.heritage_skip(base) {
                let ok = self.check_heritage_pair(
                    sym,
                    derived,
                    base,
                    at,
                    &member_list,
                    l.file,
                    dg::CLASS_0_INCORRECTLY_EXTENDS_BASE_CLASS_1_2415,
                );
                if ok {
                    self.check_static_side(sym, at);
                }
            }
        }
        // (A base whose instances are `any` has no base type here, but tsc's checks still
        // run for it.)
        if has_extends {
            self.check_mixin_class(l, sym, at);
        }
        // `implements`.
        for &h in ast.list(heritage) {
            let Node::HeritageClause {
                kind: HeritageKind::Implements,
                types,
            } = *ast.node(h)
            else {
                continue;
            };
            for &t in ast.list(types) {
                let tl = Loc::new(l.file, t);
                let it = self.type_from_node(tl);
                if self.heritage_skip(it) {
                    continue;
                }
                if !self.is_valid_implements_type(it) {
                    self.error(tl, dg::A_CLASS_CAN_ONLY_IMPLEMENT_AN_OBJECT_TYPE_OR_INTERSECTION_OF_OBJECT_TYPES_WITH_S_2422, &[]);
                    continue;
                }
                let it = self.with_this_arg(it, this_t);
                let head = if self.is_class_reference(it) {
                    dg::CLASS_0_INCORRECTLY_IMPLEMENTS_CLASS_1_DID_YOU_MEAN_TO_EXTEND_1_AND_INHERIT_ITS_M_2720
                } else {
                    dg::CLASS_0_INCORRECTLY_IMPLEMENTS_INTERFACE_1_2420
                };
                self.check_heritage_pair(sym, derived, it, at, &member_list, l.file, head);
            }
        }
    }

    /// Object types and intersections of object types with known members.
    fn is_valid_implements_type(&mut self, t: TypeId) -> bool {
        let st = self.store();
        let f = st.flags(t);
        if f.contains(TypeFlags::OBJECT) {
            return !self.object_flags(t).contains(ObjectFlags::MAPPED)
                || self.resolved_members(t).call.is_empty();
        }
        if f.contains(TypeFlags::INTERSECTION) {
            let parts = st.members_of_union(t).to_vec();
            return parts.into_iter().all(|p| self.is_valid_implements_type(p));
        }
        false
    }

    /// Whether `t` is a reference to a class instance type.
    fn is_class_reference(&self, t: TypeId) -> bool {
        self.object_flags(t).contains(ObjectFlags::CLASS)
            || self
                .store()
                .object_of(t)
                .and_then(|o| match o.kind {
                    ts_r_types::ObjectKind::Reference { target, .. } => Some(target),
                    _ => None,
                })
                .is_some_and(|tg| self.object_flags(tg).contains(ObjectFlags::CLASS))
    }

    /// Compares `derived` with one base; reports member errors (TS2416) or the class error
    /// (`head`). Returns whether they are related.
    #[allow(clippy::too_many_arguments)]
    fn check_heritage_pair(
        &mut self,
        sym: SymId,
        derived: TypeId,
        base: TypeId,
        at: Loc,
        members: &[ts_r_ast::NodeId],
        file: ts_r_diagnostics::FileId,
        head: dg::DiagnosticInfo,
    ) -> bool {
        if self.heritage_assignable(derived, base) {
            return true;
        }
        if !self.reporting(file) {
            return false;
        }
        let _ = sym;
        let extends = head.code == 2415;
        if extends && !self.inherits_all(derived, base) {
            // The base didn't resolve while the class was built (circular bases and
            // constraints, which tsc reports as TS2310): nothing reliable to compare.
            return false;
        }
        let mut reported = false;
        let ast = self.ast(file);
        for &m in members {
            let (key, modifiers) = match *ast.node(m) {
                Node::PropertyDef { key, modifiers, .. } => (key, modifiers),
                Node::MethodDef {
                    key,
                    modifiers,
                    kind,
                    ..
                } if kind != MethodKind::Constructor => (key, modifiers),
                _ => continue,
            };
            if modifiers.contains(Modifiers::STATIC) {
                continue;
            }
            let name = match *ast.node(key) {
                Node::Ident { .. } | Node::StringLit { .. } | Node::NumberLit { .. } => {
                    key_text(ast, key)
                }
                Node::ComputedKey { expr } => {
                    key_text(ast, key).or_else(|| self.late_bound_name(Loc::new(file, expr)))
                }
                _ => None,
            };
            let Some(name) = name else {
                continue;
            };
            let display = self
                .c
                .symbol_key_display(&name)
                .unwrap_or_else(|| name.clone());
            let (Some(dp), Some(bp)) = (
                self.property_of_type(derived, &name),
                self.property_of_type(base, &name),
            ) else {
                continue;
            };
            let dt = self.type_of_symbol(dp);
            let bt = self.type_of_symbol(bp);
            if self.heritage_assignable(dt, bt) {
                continue;
            }
            let Some(mut chain) = self.heritage_chain(dt, bt, None) else {
                continue;
            };
            let ds = self.type_to_string(derived);
            let bs = self.type_to_string(base);
            let display = self.written_decl_name(dp).unwrap_or(display);
            chain.push_front(
                dg::PROPERTY_0_IN_TYPE_1_IS_NOT_ASSIGNABLE_TO_THE_SAME_PROPERTY_IN_BASE_TYPE_2_2416,
                &[&display, &ds, &bs],
            );
            self.error_chain(Loc::new(file, key), &chain);
            reported = true;
        }
        // Class instance types don't carry the signatures of an interface-typed base
        // constructor's instance type; tsc doesn't report those.
        let base_has_sigs = !self.signatures_of(base, SigKind::Call).is_empty()
            || !self.signatures_of(base, SigKind::Construct).is_empty();
        if !reported
            && !base_has_sigs
            && let Some(chain) = self.heritage_chain(derived, base, Some(Head { info: head }))
        {
            self.error_chain(at, &chain);
        }
        false
    }

    /// Whether `derived` has every property of `base`.
    fn inherits_all(&mut self, derived: TypeId, base: TypeId) -> bool {
        for (n, _) in self.properties_of(base) {
            if self.property_of_type_id(derived, n).is_none() {
                return false;
            }
        }
        true
    }

    /// A class extending a value typed by a type variable is a mixin: its constructor must
    /// take a single `...args: any[]` (TS2545), and it must be `abstract` when the base
    /// can construct abstractly (TS2797) (tsc: checker.go, checkClassLikeDeclarationWorker).
    fn check_mixin_class(&mut self, l: Loc, sym: SymId, at: Loc) {
        let Some(bc) = self.extends_expression_type(sym) else {
            return;
        };
        if !self
            .store()
            .flags(bc)
            .intersects(TypeFlags::TYPE_PARAMETER | TypeFlags::INDEXED_ACCESS)
            || !self.reporting(at.file)
        {
            return;
        }
        // A type variable without construct signatures is no base constructor at all (TS2507
        // instead), and tsc has no base type to check then.
        let apparent_bc = self.apparent_type(bc);
        let base_sigs = self.signatures_of(apparent_bc, SigKind::Construct);
        if base_sigs.is_empty() {
            return;
        }
        let static_t = self.type_of_symbol(sym);
        let sigs = self.signatures_of(static_t, SigKind::Construct);
        // An optional rest (`...args?: any[]`) is `any[] | undefined` under
        // strictNullChecks, which is no array of `any`.
        let optional_rest = |ck: &Self, s: ts_r_types::SigId| {
            let Some(p) = ck.store().sig(s).params.first() else {
                return false;
            };
            ck.c.opts.strict_null_checks
                && p.ty != TypeId::ANY
                && ck
                    .c
                    .decls(p.sym)
                    .into_iter()
                    .any(|d| matches!(ck.node(d), Node::Param { optional: true, .. }))
        };
        let mixin = matches!(sigs[..], [s] if self.is_mixin_construct_signature(s) && !optional_rest(self, s));
        if !mixin {
            self.error(
                at,
                dg::A_MIXIN_CLASS_MUST_HAVE_A_CONSTRUCTOR_WITH_A_SINGLE_REST_PARAMETER_OF_TYPE_ANY_2545,
                &[],
            );
            return;
        }
        let abstract_base = base_sigs.into_iter().any(|s| {
            self.store()
                .sig(s)
                .flags
                .contains(ts_r_types::SigFlags::ABSTRACT)
        });
        if abstract_base && !self.node(l).modifiers().contains(Modifiers::ABSTRACT) {
            self.error(
                at,
                dg::A_MIXIN_CLASS_THAT_EXTENDS_FROM_A_TYPE_VARIABLE_CONTAINING_AN_ABSTRACT_CONSTRUCT_2797,
                &[],
            );
        }
    }

    /// TS2417: the static side against the base constructor's properties.
    fn check_static_side(&mut self, sym: SymId, at: Loc) {
        let Some(base_ctor) = self.base_constructor_type(sym) else {
            // `class C extends null` that still has a base type (from a merged `interface C
            // extends Base`): tsc compares the static side with `null` itself, which never
            // fits (tsc 7.0.2, classExtendsNull2).
            if let Some(t) = self.extends_expression_type(sym)
                && self.store().flags(t).contains(TypeFlags::NULL)
                && self.reporting(at.file)
            {
                let static_t = self.type_of_symbol(sym);
                let ss = self.type_to_string(static_t);
                self.error(
                    at,
                    dg::CLASS_STATIC_SIDE_0_INCORRECTLY_EXTENDS_BASE_CLASS_STATIC_SIDE_1_2417,
                    &[&ss, "null"],
                );
            }
            return;
        };
        if self.heritage_skip(base_ctor) {
            return;
        }
        let static_t = self.type_of_symbol(sym);
        if self.heritage_skip(static_t) {
            return;
        }
        // Only plain class bases: the static side of mixin and intersection bases isn't
        // modelled precisely enough to report on.
        let base_sym = match self.store().object_of(base_ctor).map(|o| o.kind) {
            Some(ts_r_types::ObjectKind::Anonymous { decl, kind, .. })
                if decl.0 == u32::MAX && kind == crate::declared::anon::CLASS_STATIC =>
            {
                SymId(decl.1)
            }
            _ => return,
        };
        // `prototype` repeats the instance side, which is checked on its own (TS2415).
        let prototype = self.store().intern_str("prototype");
        let mut props: Vec<(StrId, SymId)> = self.properties_of(base_ctor);
        props.retain(|(n, _)| *n != prototype);
        // Written `static [k: K]: V` signatures aren't compared (tsc 7.0.2 accepts a derived
        // class whose static index type differs).
        let mut index = self.resolved_members(base_ctor).index.to_vec();
        index.retain(|i| {
            !i.decl
                .and_then(Loc::unpack)
                .is_some_and(|l| matches!(self.node(l), Node::IndexSignature { .. }))
        });
        let traits = self.traits_of(base_ctor);
        let target = self.store().literal_object(
            base_sym,
            ObjectFlags::ANONYMOUS | ObjectFlags::CLASS_STATIC,
            ResolvedMembers::new(props, Vec::new(), Vec::new(), index),
            traits,
        );
        if self.heritage_assignable(static_t, target) {
            return;
        }
        if !self.reporting(at.file) {
            return;
        }
        let Some(mut chain) = self.heritage_chain(
            static_t,
            target,
            Some(Head {
                info: dg::CLASS_STATIC_SIDE_0_INCORRECTLY_EXTENDS_BASE_CLASS_STATIC_SIDE_1_2417,
            }),
        ) else {
            return;
        };
        let ss = self.type_to_string(static_t);
        let bs = self.type_to_string(base_ctor);
        let header = crate::diag::msg(
            dg::CLASS_STATIC_SIDE_0_INCORRECTLY_EXTENDS_BASE_CLASS_STATIC_SIDE_1_2417,
        )
        .format(&[&ss, &bs]);
        match chain.items.first_mut() {
            Some(first) if first.0 == 2417 => first.1 = header,
            _ => chain.push_front(
                dg::CLASS_STATIC_SIDE_0_INCORRECTLY_EXTENDS_BASE_CLASS_STATIC_SIDE_1_2417,
                &[&ss, &bs],
            ),
        }
        self.error_chain(at, &chain);
    }

    /// TS2320/TS2430 for interface `l` (only at the first declaration of its symbol).
    pub(crate) fn check_interface_heritage(&mut self, l: Loc) {
        let saved = self.print_scope.replace(l);
        self.check_interface_heritage_in(l);
        self.print_scope = saved;
    }

    fn check_interface_heritage_in(&mut self, l: Loc) {
        let Some(sym) = self.c.sym_of_decl(l) else {
            return;
        };
        let decls = self.c.decls(sym);
        let first = decls
            .iter()
            .copied()
            .find(|d| matches!(self.node(*d), Node::Interface { .. }));
        if first != Some(l) {
            return;
        }
        let any_extends = decls.iter().any(
            |d| matches!(self.node(*d), Node::Interface { extends, .. } if !extends.is_empty()),
        );
        if !any_extends {
            return;
        }
        let Node::Interface { name, .. } = *self.node(l) else {
            return;
        };
        let at = Loc::new(l.file, name);
        if self.has_circular_bases(sym) != Some(false) {
            return;
        }
        let derived = self.type_with_this(sym);
        if self.heritage_skip(derived) {
            return;
        }
        let bases: Vec<TypeId> = self.base_types(sym).to_vec();
        if bases.len() > 1 && !self.check_inherited_identical(sym, derived, &bases, at) {
            return;
        }
        for b in bases {
            if self.heritage_skip(b) || self.heritage_assignable(derived, b) {
                continue;
            }
            if !self.reporting(at.file) {
                return;
            }
            if !self.inherits_all(derived, b) {
                // Unresolved inheritance (circular bases): see `check_heritage_pair`.
                continue;
            }
            if let Some(chain) = self.heritage_chain(
                derived,
                b,
                Some(Head {
                    info: dg::INTERFACE_0_INCORRECTLY_EXTENDS_INTERFACE_1_2430,
                }),
            ) {
                self.error_chain(at, &chain);
            }
        }
    }

    /// Whether two inherited properties are identical: same optionality, same visibility
    /// (private and protected ones from the same declaration) and identical types.
    fn inherited_identical(&mut self, a: SymId, at: TypeId, b: SymId, bt: TypeId) -> bool {
        let opt = |ck: &Self, p: SymId| {
            ck.c.sym_flags(p).intersects(SymbolFlags::OPTIONAL)
                || ck.c.sym_check_flags(p) & crate::symbols::check_flags::OPTIONAL != 0
        };
        if opt(self, a) != opt(self, b) {
            return false;
        }
        let vis = |ck: &Self, p: SymId| {
            let d = ck.c.value_decl(p).or_else(|| ck.c.first_decl(p));
            let m = d.map_or(Modifiers::NONE, |d| ck.node(d).modifiers());
            (
                d,
                (
                    m.contains(Modifiers::PRIVATE),
                    m.contains(Modifiers::PROTECTED),
                ),
            )
        };
        let (da, ma) = vis(self, a);
        let (db, mb) = vis(self, b);
        if ma != mb || ((ma.0 || ma.1) && da != db) {
            return false;
        }
        let (r, spent) = self.budgeted(|ck| ck.is_identical(at, bt));
        r || spent
    }

    /// TS2320: properties inherited from several bases must be identical unless the
    /// interface redeclares them. Returns whether no error was found.
    fn check_inherited_identical(
        &mut self,
        sym: SymId,
        derived: TypeId,
        bases: &[TypeId],
        at: Loc,
    ) -> bool {
        let _ = derived;
        // Names the interface declares itself.
        let decls = self.c.decls(sym);
        let raw: Vec<(StrId, SymId)> = {
            let m = self.raw_members(sym);
            m.names
                .iter()
                .copied()
                .zip(m.props.iter().copied())
                .collect()
        };
        let own: Vec<StrId> = raw
            .into_iter()
            .filter(|&(_, p)| {
                self.c
                    .first_decl(p)
                    .and_then(|d| self.c.parent(d))
                    .is_some_and(|pd| decls.contains(&pd))
            })
            .map(|(n, _)| n)
            .collect();
        let mut seen: Vec<(StrId, TypeId, TypeId, SymId)> = Vec::new();
        for &b in bases {
            if self.heritage_skip(b) {
                continue;
            }
            for (n, p) in self.properties_of(b) {
                if own.contains(&n) {
                    continue;
                }
                let pt = self.type_of_symbol(p);
                if let Some(&(_, prev_base, prev_t, prev_p)) = seen.iter().find(|x| x.0 == n) {
                    if prev_base == b
                        || prev_p == p
                        || self.inherited_identical(prev_p, prev_t, p, pt)
                    {
                        continue;
                    }
                    if !self.reporting(at.file) {
                        return false;
                    }
                    let plain = self.declared_type_of_symbol(sym);
                    let iname = self.type_to_string(plain);
                    let s1 = self.type_to_string(prev_base);
                    let s2 = self.type_to_string(b);
                    let pn = self.store().str(n).to_owned();
                    let pn = self.property_name_for_print(&pn);
                    let mut chain = Chain::new();
                    chain.push_back(
                        dg::INTERFACE_0_CANNOT_SIMULTANEOUSLY_EXTEND_TYPES_1_AND_2_2320,
                        &[&iname, &s1, &s2],
                    );
                    chain.push_back(
                        dg::NAMED_PROPERTY_0_OF_TYPES_1_AND_2_ARE_NOT_IDENTICAL_2319,
                        &[&pn, &s1, &s2],
                    );
                    self.error_chain(at, &chain);
                    return false;
                } else {
                    seen.push((n, b, pt, p));
                }
            }
        }
        true
    }
}
