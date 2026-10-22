//! Declared types: classes, interfaces, type aliases, enums, type parameters, and the types
//! written in type annotations.
//!
//! Everything here is silent: errors in type nodes are reported by the per-file walk
//! (`check.rs`), never while computing a (shared, cached) declared type.

use ts_r_ast::{KeywordTypeKind, Node, NodeId, TypeOperatorKind, UnaryOp};
use ts_r_binder::{SymbolFlags, meaning};
use ts_r_types::{
    AliasId, ListId, MapperId, ObjectFlags, ObjectKind, SymId, TupleElement, TupleShape, TypeFlags,
    TypeId, TypeKind, TypeTraits,
};

use crate::checker::{Ck, GlobalType, Loc, Resolving};
use crate::nodes::{fn_parts, skip_parens, type_params_of};

/// Kinds of anonymous object types (the `kind` of `ObjectKind::Anonymous`).
pub mod anon {
    /// `{ ... }` type literal; key: the node.
    pub const TYPE_LITERAL: u8 = 0;
    /// `(x) => y` / `new (x) => y`; key: the node.
    pub const FUNCTION_TYPE: u8 = 1;
    /// `typeof f` for a function or method symbol; key: `(u32::MAX, sym)`.
    pub const SYMBOL_FUNCTION: u8 = 2;
    /// `typeof C` for a class; key: `(u32::MAX, sym)`.
    pub const CLASS_STATIC: u8 = 3;
    /// `typeof N` for a namespace, enum or module; key: `(u32::MAX, sym)`.
    pub const NAMESPACE: u8 = 4;
    /// A function expression or arrow with a declared signature; key: the node.
    pub const FUNCTION_EXPR: u8 = 5;
    /// Reserved: class expressions are keyed by symbol as `CLASS_STATIC`.
    pub const CLASS_EXPR: u8 = 6;
}

/// The anonymous-type key of a symbol-keyed type.
pub fn sym_key(s: SymId) -> (u32, u32) {
    (u32::MAX, s.0)
}

/// The most elements a tuple type may have (TS2799 / TS2800 beyond it).
pub const MAX_TUPLE_LENGTH: usize = 10_000;

/// Longest printed name of an unresolved generic type reference (`Missing<A, B>`); longer
/// names end in `...`.
const MAX_ERROR_NAME_LEN: usize = 4096;

/// A tuple element while building a tuple type.
#[derive(Clone, Copy, Debug)]
pub struct TupleEl {
    pub ty: TypeId,
    pub kind: u8,
    pub label: Option<ts_r_types::StrId>,
}

impl<'c, 'a> Ck<'c, 'a> {
    // ---------------------------------------------------------------------------------
    // Global types
    // ---------------------------------------------------------------------------------

    /// A global type (`Array`, `Promise`, ...) from the lib files.
    pub fn global_type(&mut self, g: GlobalType) -> Option<TypeId> {
        if let Some(&t) = self.c.global_slot(g).get() {
            return t;
        }
        let t = self
            .c
            .global_symbol(g.name(), meaning::TYPE)
            .map(|s| self.declared_type_of_symbol(s))
            .filter(|&t| !self.store().is_error(t));
        let _ = self.c.global_slot(g).set(t);
        *self.c.global_slot(g).get().unwrap_or(&t)
    }

    /// `T[]` / `readonly T[]`.
    pub fn create_array(&mut self, elem: TypeId, readonly: bool) -> TypeId {
        let g = if readonly {
            GlobalType::ReadonlyArray
        } else {
            GlobalType::Array
        };
        match self.global_type(g) {
            Some(d) => self.make_reference(d, &[elem]),
            None => TypeId::UNRESOLVED,
        }
    }

    /// `G<args>` for a global generic type, if the lib declares it.
    pub fn global_reference(&mut self, g: GlobalType, args: &[TypeId]) -> Option<TypeId> {
        let d = self.global_type(g)?;
        Some(self.make_reference(d, args))
    }

    /// A reference to a class/interface declared type; returns the declared type itself when
    /// the arguments are its own type parameters.
    pub fn make_reference(&mut self, d: TypeId, args: &[TypeId]) -> TypeId {
        let s = self.store();
        if let Some(ObjectKind::Declared { type_params, .. }) = s.object_of(d).map(|o| o.kind) {
            let params = s.list_items(type_params);
            if params.is_empty() && args.is_empty() {
                return d;
            }
            if params == args {
                return d;
            }
            let n = params.len();
            if args.len() < n {
                let mut v = args.to_vec();
                v.resize(n, TypeId::UNRESOLVED);
                return s.reference(d, &v);
            }
        }
        s.reference(d, args)
    }

    /// A tuple type. More than [`MAX_TUPLE_LENGTH`] elements gives `ERROR` and sets
    /// `tuple_overflow` (reported as TS2799 / TS2800).
    pub fn create_tuple(&mut self, elems: &[TupleEl], readonly: bool) -> TypeId {
        // Spread fixed tuples and arrays in rest positions.
        let mut out: Vec<TupleEl> = Vec::with_capacity(elems.len());
        for e in elems {
            if e.kind == TupleElement::REST || e.kind == TupleElement::VARIADIC {
                let t = e.ty;
                if let Some(shape) = self.store().tuple_shape_of(t) {
                    if out.len() + shape.elements.len() > MAX_TUPLE_LENGTH {
                        self.tuple_overflow = true;
                        return TypeId::ERROR;
                    }
                    let shape = shape.clone();
                    let args = self.c.type_args_of_tuple(t).to_vec();
                    for (i, el) in shape.elements.iter().enumerate() {
                        out.push(TupleEl {
                            ty: args.get(i).copied().unwrap_or(TypeId::UNRESOLVED),
                            kind: el.kind,
                            label: el.label,
                        });
                    }
                    continue;
                }
                if self.c.is_array_like_type(t) {
                    let el = self
                        .c
                        .type_args_of(t)
                        .first()
                        .copied()
                        .unwrap_or(TypeId::UNRESOLVED);
                    out.push(TupleEl {
                        ty: el,
                        kind: TupleElement::REST,
                        label: e.label,
                    });
                    continue;
                }
                if self.store().flags(t).intersects(TypeFlags::ANY) {
                    let el = t;
                    out.push(TupleEl {
                        ty: el,
                        kind: TupleElement::REST,
                        label: e.label,
                    });
                    continue;
                }
                out.push(TupleEl {
                    ty: t,
                    kind: TupleElement::VARIADIC,
                    label: e.label,
                });
                continue;
            }
            out.push(*e);
        }
        if out.len() > MAX_TUPLE_LENGTH {
            self.tuple_overflow = true;
            return TypeId::ERROR;
        }
        // At most one rest element: later rest/optional elements fold into it.
        let mut fixed: Vec<TupleEl> = Vec::with_capacity(out.len());
        let mut rest_at: Option<usize> = None;
        for e in out {
            if let Some(r) = rest_at
                && fixed[r].kind == TupleElement::REST
                && e.kind != TupleElement::REQUIRED
            {
                let u = self.union2(fixed[r].ty, e.ty);
                fixed[r].ty = u;
                continue;
            }
            if e.kind == TupleElement::REST {
                rest_at = Some(fixed.len());
            }
            fixed.push(e);
        }
        // A lone rest element is the array itself (`[...number[]]` is `number[]`).
        if let [e] = fixed[..]
            && e.kind == TupleElement::REST
        {
            return self.create_array(e.ty, readonly);
        }
        let shape = TupleShape {
            elements: fixed
                .iter()
                .map(|e| TupleElement {
                    kind: e.kind,
                    label: e.label,
                })
                .collect(),
            readonly,
        };
        let target = self.store().tuple_target(shape);
        let args: Vec<TypeId> = fixed.iter().map(|e| e.ty).collect();
        self.store().reference(target, &args)
    }

    // ---------------------------------------------------------------------------------
    // Declared types of symbols
    // ---------------------------------------------------------------------------------

    pub fn declared_type_of_symbol(&mut self, s: SymId) -> TypeId {
        if s.is_none() {
            return TypeId::ERROR;
        }
        let flags = self.c.sym_flags(s);
        if flags.intersects(SymbolFlags::ALIAS) {
            return match self.c.resolve_alias(s) {
                Some(t) if t != s => self.declared_type_of_symbol(t),
                _ => TypeId::ERROR,
            };
        }
        if let Some(x) = self.c.synth_sym(s) {
            return match x.origins.first() {
                Some(&o) if o != s => self.declared_type_of_symbol(o),
                _ => TypeId::ERROR,
            };
        }
        if let Some(l) = self.c.sym_links(s)
            && let Some(&t) = l.declared.get()
        {
            return t;
        }
        if !self.push_resolution(Resolving::DeclaredType(s)) {
            return TypeId::UNRESOLVED;
        }
        let t = if flags.intersects(SymbolFlags::CLASS | SymbolFlags::INTERFACE) {
            self.class_or_interface_type(s)
        } else if flags.intersects(SymbolFlags::TYPE_ALIAS) {
            self.alias_declared_type(s)
        } else if flags.intersects(SymbolFlags::ENUM) {
            self.enum_declared_type(s)
        } else if flags.intersects(SymbolFlags::ENUM_MEMBER) {
            self.enum_member_type(s, false)
        } else if flags.intersects(SymbolFlags::TYPE_PARAMETER) {
            let s = self.canonical_type_param(s);
            self.store().type_param(s, false)
        } else {
            TypeId::ERROR
        };
        let ok = self.pop_resolution();
        let t = if ok { t } else { TypeId::UNRESOLVED };
        if ok
            && flags.intersects(SymbolFlags::TYPE_ALIAS)
            && let Some(f) = self.store().find_forward(s)
        {
            self.store().link_forward(f, t);
        }
        if !self.pending_forwards.is_empty() && !self.in_alias_resolution() {
            self.resolve_pending_forwards();
        }
        if !ok {
            // A cycle: don't publish, a later top-level request may resolve differently.
            return t;
        }
        match self.c.sym_links(s) {
            Some(l) => {
                let _ = l.declared.set(t);
                *l.declared.get().unwrap_or(&t)
            }
            None => t,
        }
    }

    fn class_or_interface_type(&mut self, s: SymId) -> TypeId {
        if let Some(t) = self.store().find_declared(s) {
            return t;
        }
        let decls = self.c.decls(s);
        let is_class = self.c.sym_flags(s).intersects(SymbolFlags::CLASS);
        let mut local: Vec<TypeId> = Vec::new();
        let mut outer: Vec<TypeId> = Vec::new();
        let mut first = None;
        for &d in &decls {
            let ast = self.ast(d.file);
            if !matches!(
                ast.node(d.node),
                Node::ClassDecl { .. } | Node::ClassExpr { .. } | Node::Interface { .. }
            ) {
                continue;
            }
            if first.is_none() {
                first = Some(d);
            }
            if local.is_empty()
                && let Some(tp) = type_params_of(ast, d.node)
            {
                local = self.type_params_from_node(Loc::new(d.file, tp));
            }
        }
        if let Some(d) = first {
            outer = self.outer_type_params(d);
        }
        outer.extend(local);
        let flags = if is_class {
            ObjectFlags::CLASS
        } else {
            ObjectFlags::INTERFACE
        };
        let store = self.store();
        store.declared_object(s, flags, || (store.list(&outer), store.type_param(s, true)))
    }

    /// The local type parameters of a class, interface, type alias or function declaration
    /// symbol (not the outer ones).
    pub fn local_type_params(&mut self, s: SymId) -> Vec<TypeId> {
        // A type merged with a function (`interface Tagged` and `function Tagged<T>`) has
        // the type declarations' parameters only.
        let typed = self
            .c
            .sym_flags(s)
            .intersects(SymbolFlags::CLASS | SymbolFlags::INTERFACE | SymbolFlags::TYPE_ALIAS);
        for d in self.c.decls(s) {
            let ast = self.ast(d.file);
            if typed
                && !matches!(
                    ast.node(d.node),
                    Node::ClassDecl { .. }
                        | Node::ClassExpr { .. }
                        | Node::Interface { .. }
                        | Node::TypeAlias { .. }
                )
            {
                continue;
            }
            if let Some(tp) = type_params_of(ast, d.node) {
                return self.type_params_from_node(Loc::new(d.file, tp));
            }
        }
        Vec::new()
    }

    /// The type parameter declarations at the same position as `tp`'s in the other
    /// declarations of a merged interface or class (`interface F<T>` and `interface
    /// F<T extends X>`).
    fn merged_type_param_decls(&self, tp: SymId) -> Vec<Loc> {
        let mut out = Vec::new();
        let Some(decl) = self.c.decls(tp).first().copied() else {
            return out;
        };
        let Some(list) = self.c.parent(decl) else {
            return out;
        };
        let Some(owner) = self.c.parent(list) else {
            return out;
        };
        let ast = self.ast(decl.file);
        if !matches!(
            ast.node(owner.node),
            Node::Interface { .. } | Node::ClassDecl { .. }
        ) {
            return out;
        }
        let Node::TypeParams { params } = *ast.node(list.node) else {
            return out;
        };
        let Some(index) = ast.list(params).iter().position(|&p| p == decl.node) else {
            return out;
        };
        let Some(owner_sym) = self.c.sym_of_decl(owner) else {
            return out;
        };
        for d in self.c.decls(owner_sym) {
            if d == owner {
                continue;
            }
            let dast = self.ast(d.file);
            if !matches!(
                dast.node(d.node),
                Node::ClassDecl { .. } | Node::Interface { .. }
            ) {
                continue;
            }
            if let Some(tps) = type_params_of(dast, d.node)
                && let Node::TypeParams { params } = *dast.node(tps)
                && let Some(&p) = dast.list(params).get(index)
            {
                out.push(Loc::new(d.file, p));
            }
        }
        out
    }

    /// Merged class/interface declarations each declare their own type parameter symbols;
    /// they all denote the parameter at the same position in the declaration that
    /// `local_type_params` uses (the first one with type parameters).
    pub fn canonical_type_param(&mut self, tp: SymId) -> SymId {
        let Some(decl) = self.c.decls(tp).first().copied() else {
            return tp;
        };
        let Some(list) = self.c.parent(decl) else {
            return tp;
        };
        let Some(owner) = self.c.parent(list) else {
            return tp;
        };
        let ast = self.ast(decl.file);
        if !matches!(
            ast.node(owner.node),
            Node::Interface { .. } | Node::ClassDecl { .. }
        ) {
            return tp;
        }
        let Node::TypeParams { params } = *ast.node(list.node) else {
            return tp;
        };
        let Some(index) = ast.list(params).iter().position(|&p| p == decl.node) else {
            return tp;
        };
        let Some(owner_sym) = self.c.sym_of_decl(owner) else {
            return tp;
        };
        for d in self.c.decls(owner_sym) {
            let dast = self.ast(d.file);
            // (As in `local_type_params`: a merged function's parameters don't count.)
            if !matches!(
                dast.node(d.node),
                Node::ClassDecl { .. }
                    | Node::ClassExpr { .. }
                    | Node::Interface { .. }
                    | Node::TypeAlias { .. }
            ) {
                continue;
            }
            if let Some(tps) = type_params_of(dast, d.node) {
                if d == owner {
                    return tp;
                }
                let Node::TypeParams { params } = *dast.node(tps) else {
                    return tp;
                };
                return match dast.list(params).get(index) {
                    Some(&p) => self.c.sym_of_decl(Loc::new(d.file, p)).unwrap_or(tp),
                    None => tp,
                };
            }
        }
        tp
    }

    /// Type parameters declared by a `TypeParams` node.
    pub fn type_params_from_node(&mut self, tp: Loc) -> Vec<TypeId> {
        let ast = self.ast(tp.file);
        let Node::TypeParams { params } = *ast.node(tp.node) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for &p in ast.list(params) {
            match self.c.sym_of_decl(Loc::new(tp.file, p)) {
                Some(sym) => {
                    let sym = self.canonical_type_param(sym);
                    out.push(self.store().type_param(sym, false))
                }
                None => out.push(TypeId::UNRESOLVED),
            }
        }
        out
    }

    /// Type parameters of enclosing declarations (outermost first), including the `this`
    /// types of enclosing classes and interfaces.
    pub fn outer_type_params(&mut self, l: Loc) -> Vec<TypeId> {
        if let Some(v) = self.outer_cache.get(&l) {
            return v.clone();
        }
        let ast = self.ast(l.file);
        let parents = self.c.file(l.file).parents();
        let mut groups: Vec<Vec<TypeId>> = Vec::new();
        let mut cur = parents.parent(l.node);
        let mut child = l.node;
        let mut guard = 0;
        while let Some(n) = cur {
            guard += 1;
            if guard > 100_000 {
                break;
            }
            let node = ast.node(n);
            match *node {
                // A mapped type's `K` is in scope in its name and template types.
                Node::MappedType { type_param, .. } if child != type_param => {
                    if let Some(k) = self.mapped_type_param_of(Loc::new(l.file, n)) {
                        groups.push(vec![k]);
                    }
                }
                // `infer` parameters are in scope in the extends clause and the true branch.
                Node::ConditionalType {
                    extends, true_ty, ..
                } if child == extends || child == true_ty => {
                    let g = self.infer_params_of(Loc::new(l.file, n));
                    if !g.is_empty() {
                        groups.push(g);
                    }
                }
                _ => {}
            }
            child = n;
            if !matches!(node, Node::MethodDef { .. }) {
                let mut g = Vec::new();
                if let Some(tp) = type_params_of(ast, n) {
                    g = self.type_params_from_node(Loc::new(l.file, tp));
                }
                if matches!(
                    node,
                    Node::ClassDecl { .. } | Node::ClassExpr { .. } | Node::Interface { .. }
                ) && let Some(sym) = self.c.sym_of_decl(Loc::new(l.file, n))
                {
                    g.push(self.store().type_param(sym, true));
                }
                if !g.is_empty() {
                    groups.push(g);
                }
            }
            cur = parents.parent(n);
        }
        let mut out = Vec::new();
        for g in groups.into_iter().rev() {
            for t in g {
                if !out.contains(&t) {
                    out.push(t);
                }
            }
        }
        self.outer_cache.insert(l, out.clone());
        out
    }

    /// The outer type parameters a type literal or function type node may reference: the
    /// ones it names. A literal inside a block (where local types can name them
    /// indirectly) or with a `typeof` query may reference all of them, as may every
    /// parameter not declared in a type parameter list (`this`, mapped and `infer`).
    pub fn referenced_outer_type_params(&mut self, l: Loc) -> Vec<TypeId> {
        if let Some(v) = self.outer_ref_cache.get(&l) {
            return v.clone();
        }
        let outer = self.outer_type_params(l);
        let out = if outer.is_empty() {
            outer
        } else {
            self.filter_referenced_outer(l, outer)
        };
        self.outer_ref_cache.insert(l, out.clone());
        out
    }

    fn filter_referenced_outer(&mut self, l: Loc, outer: Vec<TypeId>) -> Vec<TypeId> {
        let ast = self.ast(l.file);
        let parents = self.c.file(l.file).parents();
        // Names of the enclosing type parameter lists' parameters.
        let mut declared: Vec<(TypeId, &str)> = Vec::new();
        let mut cur = parents.parent(l.node);
        let mut guard = 0;
        while let Some(n) = cur {
            guard += 1;
            if guard > 100_000 {
                return outer;
            }
            match *ast.node(n) {
                Node::Block { .. } | Node::ModuleBlock { .. } | Node::StaticBlock { .. } => {
                    return outer;
                }
                _ => {}
            }
            if let Some(tp) = type_params_of(ast, n)
                && let Node::TypeParams { params } = *ast.node(tp)
            {
                for &p in ast.list(params) {
                    let Node::TypeParam { name, .. } = *ast.node(p) else {
                        continue;
                    };
                    let (Some(sym), Some(text)) = (
                        self.c.sym_of_decl(Loc::new(l.file, p)),
                        crate::nodes::ident_text(ast, name),
                    ) else {
                        continue;
                    };
                    let sym = self.canonical_type_param(sym);
                    declared.push((self.store().type_param(sym, false), text));
                }
            }
            cur = parents.parent(n);
        }
        let mut names: Vec<&str> = Vec::new();
        let mut stack = vec![l.node];
        let mut steps = 0;
        while let Some(n) = stack.pop() {
            steps += 1;
            if steps > 200_000 {
                return outer;
            }
            match *ast.node(n) {
                Node::TypeQuery { .. } => return outer,
                Node::Ident { .. } => {
                    if let Some(t) = crate::nodes::ident_text(ast, n)
                        && !names.contains(&t)
                    {
                        names.push(t);
                    }
                }
                _ => {}
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        outer
            .into_iter()
            .filter(|&p| {
                let mut found = false;
                for &(id, text) in &declared {
                    if id == p {
                        found = true;
                        if names.contains(&text) {
                            return true;
                        }
                    }
                }
                !found
            })
            .collect()
    }

    /// Whether a type alias's declared type is being resolved.
    fn in_alias_resolution(&self) -> bool {
        self.resolving.iter().any(|(r, _)| {
            matches!(*r, Resolving::DeclaredType(s)
                if self.c.sym_flags(s).intersects(SymbolFlags::TYPE_ALIAS))
        })
    }

    /// Resolves the aliases handed out as forward references and links them.
    fn resolve_pending_forwards(&mut self) {
        let mut guard = 0;
        while let Some(s) = self.pending_forwards.pop() {
            guard += 1;
            if guard > 10_000 {
                self.pending_forwards.clear();
                break;
            }
            let t = self.declared_type_of_symbol(s);
            if let Some(f) = self.store().find_forward(s)
                && t != TypeId::UNRESOLVED
            {
                self.store().link_forward(f, t);
            }
        }
    }

    fn alias_declared_type(&mut self, s: SymId) -> TypeId {
        let Some(d) = self
            .c
            .decls(s)
            .into_iter()
            .find(|d| matches!(self.node(*d), Node::TypeAlias { .. }))
        else {
            return TypeId::ERROR;
        };
        let Node::TypeAlias { ty, .. } = *self.node(d) else {
            return TypeId::ERROR;
        };
        let params = self.local_type_params(s);
        let alias = self.store().alias(s, self.store().list(&params));
        // Intrinsic string mappings (`Uppercase<T>` etc.) and `NoInfer<T>`.
        if matches!(
            *self.node(Loc::new(d.file, ty)),
            Node::KeywordType {
                kind: KeywordTypeKind::Intrinsic
            }
        ) {
            let Some(&tp) = params.first() else {
                // `type BuiltinIteratorReturn = intrinsic`: the return type of the lib's
                // built-in iterators.
                if self.c.sym_name(s) == "BuiltinIteratorReturn" {
                    return if self.c.opts.strict_builtin_iterator_return {
                        TypeId::UNDEFINED
                    } else {
                        TypeId::ANY
                    };
                }
                return TypeId::UNRESOLVED;
            };
            return match crate::template::intrinsic_kind(self.c.sym_name(s)) {
                Some(crate::template::Intrinsic::NoInfer) => {
                    self.store().substitution(tp, TypeId::UNKNOWN)
                }
                Some(_) => self.store().string_mapping(s, tp),
                None => TypeId::UNRESOLVED,
            };
        }
        // An overflow while resolving the body is reported at the body (where tsc reports
        // it), not at whatever is checked next.
        let saved = (self.inst_overflow, self.tuple_overflow);
        self.inst_overflow = false;
        self.tuple_overflow = false;
        let body = Loc::new(d.file, ty);
        let t = self.type_from_node_aliased(body, alias);
        if self.inst_overflow || self.tuple_overflow {
            let count = self.inst_count;
            self.report_instantiation_overflow(body, false);
            self.inst_count = count;
        }
        self.inst_overflow |= saved.0;
        self.tuple_overflow |= saved.1;
        t
    }

    /// Default of a type parameter, if declared.
    pub fn type_param_default(&mut self, t: TypeId) -> Option<TypeId> {
        let data = self.store().type_param_data(t)?;
        if let Some(d) = data.default() {
            return d;
        }
        if let Some((orig, m)) = data.origin {
            let d = self
                .type_param_default(orig)
                .map(|d| self.instantiate(d, m));
            return self
                .store()
                .type_param_data(t)
                .and_then(|x| x.set_default(d));
        }
        let sym = data.sym;
        let mut res = None;
        if !data.is_this {
            for d in self.c.decls(sym) {
                if let Node::TypeParam {
                    default: Some(def), ..
                } = *self.node(d)
                {
                    res = Some(self.type_from_node(Loc::new(d.file, def)));
                    break;
                }
            }
        }
        self.store()
            .type_param_data(t)
            .and_then(|x| x.set_default(res))
    }

    /// The declared constraint of a type parameter (`None` when unconstrained).
    pub fn type_param_constraint(&mut self, t: TypeId) -> Option<TypeId> {
        let data = self.store().type_param_data(t)?;
        if let Some(c) = data.constraint() {
            return c;
        }
        if let Some((orig, m)) = data.origin {
            let c = self
                .type_param_constraint(orig)
                .map(|c| self.instantiate(c, m));
            return self
                .store()
                .type_param_data(t)
                .and_then(|x| x.set_constraint(c));
        }
        let sym = data.sym;
        if data.is_this {
            let d = self.declared_type_of_symbol(sym);
            return self
                .store()
                .type_param_data(t)
                .and_then(|x| x.set_constraint(Some(d)));
        }
        if !self.push_resolution(Resolving::Constraint(t)) {
            return None;
        }
        let mut res = None;
        for d in self.c.decls(sym) {
            if let Node::TypeParam {
                constraint: Some(c),
                ..
            } = *self.node(d)
            {
                res = Some(self.type_from_node(Loc::new(d.file, c)));
                break;
            }
        }
        // A merged declaration may declare the constraint (observed: either order).
        if res.is_none() {
            for d in self.merged_type_param_decls(sym) {
                if let Node::TypeParam {
                    constraint: Some(c),
                    ..
                } = *self.node(d)
                {
                    res = Some(self.type_from_node(Loc::new(d.file, c)));
                    break;
                }
            }
        }
        if res.is_none()
            && let Some(d) = self.c.decls(sym).first().copied()
        {
            res = self.implied_infer_constraint(d, false);
        }
        if !self.pop_resolution() {
            return None;
        }
        // A constraint that refers to the parameter itself is circular (TS2313).
        if res == Some(t) {
            res = None;
        }
        // `extends any` constrains like `unknown` (a mapped type's key like `keyof any`);
        // an unresolved constraint stays an error. tsc: `getConstraintFromTypeParameter` in
        // tsc/internal/checker/checker.go.
        if let Some(c) = res
            && self.store().flags(c).contains(TypeFlags::ANY)
            && !self.store().is_error(c)
        {
            let mapped_key = self.c.decls(sym).first().is_some_and(|&d| {
                self.c
                    .parent(d)
                    .is_some_and(|p| matches!(self.node(p), Node::MappedType { .. }))
            });
            res = Some(if mapped_key {
                self.keyof_constraint()
            } else {
                TypeId::UNKNOWN
            });
        }
        self.store()
            .type_param_data(t)
            .and_then(|x| x.set_constraint(res))
    }

    /// TS2313 for the type parameter declared at `tp` when its constraint leads back to it.
    /// The base constraint of a type parameter is found by following constraints through
    /// type parameters, union and intersection members and both sides of indexed accesses;
    /// a parameter is circular when that walk reaches the parameter itself (in
    /// `<U extends T, T extends V, V extends T>` that is `T` and `V`, not `U`). Object,
    /// conditional and mapped constraints end the walk (tsc: getResolvedBaseConstraint in
    /// tsc/internal/checker/checker.go reports at the constraint node).
    pub(crate) fn check_circular_constraint(&mut self, tp: Loc) {
        let Node::TypeParam {
            constraint: Some(c),
            ..
        } = *self.node(tp)
        else {
            return;
        };
        if !self.reporting(tp.file) {
            return;
        }
        let Some(sym) = self.c.sym_of_decl(tp) else {
            return;
        };
        let t = self.declared_type_of_symbol(sym);
        if self.store().type_param_data(t).is_none() {
            return;
        }
        let first = self.type_from_node(Loc::new(tp.file, c));
        let mut todo = vec![first];
        let mut seen: Vec<TypeId> = Vec::new();
        let mut circular = false;
        let mut steps = 0;
        while let Some(x) = todo.pop() {
            steps += 1;
            if steps > 4096 {
                break;
            }
            match self.store().kind(x) {
                TypeKind::TypeParameter(_) => {
                    if x == t {
                        circular = true;
                        break;
                    }
                    if seen.contains(&x) {
                        continue;
                    }
                    seen.push(x);
                    if let Some(c) = self.declared_constraint_of(x) {
                        todo.push(c);
                    }
                }
                TypeKind::Union { .. } | TypeKind::Intersection { .. } => {
                    todo.extend_from_slice(self.store().members_of_union(x));
                }
                TypeKind::IndexedAccess { obj, index } => {
                    todo.push(obj);
                    todo.push(index);
                }
                _ => {}
            }
        }
        if circular {
            let name = self.c.sym_name(sym).to_owned();
            self.error(
                Loc::new(tp.file, c),
                crate::diag::dg::TYPE_PARAMETER_0_HAS_A_CIRCULAR_CONSTRAINT_2313,
                &[&name],
            );
        }
    }

    /// The constraint written on the declaration of the type parameter `t`, even when it
    /// names `t` itself (which [`Self::type_param_constraint`] drops).
    fn declared_constraint_of(&mut self, t: TypeId) -> Option<TypeId> {
        let data = self.store().type_param_data(t)?;
        if data.is_this || data.origin.is_some() {
            return None;
        }
        let sym = data.sym;
        for d in self.c.decls(sym) {
            if let Node::TypeParam {
                constraint: Some(c),
                ..
            } = *self.node(d)
            {
                return Some(self.type_from_node(Loc::new(d.file, c)));
            }
        }
        None
    }

    /// The constraint an unconstrained `infer X` takes from where it appears (observed):
    /// the constraint of the type parameter it is passed to as a type argument
    /// (`Box<infer U>` with `Box<X extends string>`), when that constraint mentions no type
    /// parameters; with `rest`, `unknown[]` as a rest parameter or rest tuple element type
    /// (only what an `infer` with no candidates falls back to: `Parameters<any>` is
    /// `unknown[]`).
    pub(crate) fn implied_infer_constraint(&mut self, d: Loc, rest: bool) -> Option<TypeId> {
        if !matches!(self.node(d), Node::TypeParam { .. }) {
            return None;
        }
        let infer = self.c.parent(d)?;
        if !matches!(self.node(infer), Node::InferType { .. }) {
            return None;
        }
        let ast = self.ast(d.file);
        let mut child = infer;
        let mut parent = self.c.parent(infer)?;
        while let Node::ParenType { .. } = *ast.node(parent.node) {
            child = parent;
            parent = self.c.parent(parent)?;
        }
        match *ast.node(parent.node) {
            Node::Param {
                rest: true,
                type_ann: Some(t),
                ..
            } if rest && t == child.node => Some(self.create_array(TypeId::UNKNOWN, false)),
            Node::RestType { .. } if rest => Some(self.create_array(TypeId::UNKNOWN, false)),
            Node::NamedTupleMember { rest: true, ty, .. } if rest && ty == child.node => {
                Some(self.create_array(TypeId::UNKNOWN, false))
            }
            Node::TypeArgs { args } if !rest => {
                let idx = ast.list(args).iter().position(|&a| a == child.node)?;
                let r = self.c.parent(parent)?;
                let Node::TypeRef { name, .. } = *ast.node(r.node) else {
                    return None;
                };
                let sym = self.resolve_entity(Loc::new(r.file, name), meaning::TYPE)?;
                let sym = self.c.resolve_alias(sym)?;
                let tps = self.local_type_params(sym);
                let tp = *tps.get(idx)?;
                let c = self.type_param_constraint(tp)?;
                if self
                    .store()
                    .traits(c)
                    .contains(TypeTraits::CONTAINS_TYPE_PARAMETER)
                    || c == TypeId::UNRESOLVED
                    || self.store().is_error(c)
                {
                    return None;
                }
                Some(c)
            }
            _ => None,
        }
    }

    /// Whether the type parameter `tp` is declared by an `infer X` that is a template
    /// literal placeholder (`${infer X}...`): what it infers is a string.
    pub(crate) fn is_template_infer_param(&mut self, tp: TypeId) -> bool {
        let Some(data) = self.store().type_param_data(tp) else {
            return false;
        };
        let Some(d) = self.c.decls(data.sym).first().copied() else {
            return false;
        };
        let Some(infer) = self.c.parent(d) else {
            return false;
        };
        if !matches!(self.node(infer), Node::InferType { .. }) {
            return false;
        }
        let mut parent = self.c.parent(infer);
        while let Some(p) = parent
            && matches!(self.node(p), Node::ParenType { .. })
        {
            parent = self.c.parent(p);
        }
        parent.is_some_and(|p| matches!(self.node(p), Node::TemplateLitType { .. }))
    }

    // ---------------------------------------------------------------------------------
    // Enums
    // ---------------------------------------------------------------------------------

    /// `(member, value)` of every member in declaration order; `value` is a regular string or
    /// number literal, or `None` for computed members.
    pub fn enum_members(&mut self, s: SymId) -> Vec<(SymId, Option<TypeId>)> {
        if let Some(l) = self.c.sym_links(s)
            && let Some(v) = l.enum_members.get()
        {
            return v.to_vec();
        }
        if self.resolving.len() >= 50 || !self.push_resolution(Resolving::EnumMembers(s)) {
            return Vec::new();
        }
        let mut out: Vec<(SymId, Option<TypeId>)> = Vec::new();
        for d in self.c.decls(s) {
            let ast = self.ast(d.file);
            let Node::Enum { members, .. } = *ast.node(d.node) else {
                continue;
            };
            let mut prev: Option<Option<TypeId>> = None;
            for &m in ast.list(members) {
                let Node::EnumMember { init, .. } = *ast.node(m) else {
                    continue;
                };
                let Some(ms) = self.c.sym_of_decl(Loc::new(d.file, m)) else {
                    continue;
                };
                let value = match init {
                    Some(e) => self.eval_constant(Loc::new(d.file, e), s, &out, 0),
                    None => match prev {
                        None => Some(self.store().number_literal(0.0, false)),
                        Some(Some(p)) => self
                            .store()
                            .number_value(p)
                            .map(|v| self.store().number_literal(v + 1.0, false)),
                        Some(None) => None,
                    },
                };
                prev = Some(value);
                out.push((ms, value));
            }
        }
        if self.pop_resolution()
            && let Some(l) = self.c.sym_links(s)
        {
            let _ = l.enum_members.set(out.clone().into_boxed_slice());
        }
        out
    }

    /// Evaluates an enum initializer or constant expression (`enum_sym` is `SymId::NONE`
    /// outside enums).
    pub(crate) fn eval_constant(
        &mut self,
        e: Loc,
        enum_sym: SymId,
        prev: &[(SymId, Option<TypeId>)],
        depth: u32,
    ) -> Option<TypeId> {
        if depth > 50 {
            return None;
        }
        let ast = self.ast(e.file);
        let s = self.store();
        match *ast.node(e.node) {
            Node::NumberLit { value } => Some(s.number_literal(value.get(), false)),
            Node::StringLit { value } => Some(s.string_literal(ast.atom(value), false)),
            Node::TemplateLit { parts } => {
                let mut text = String::new();
                for &p in ast.list(parts) {
                    match *ast.node(p) {
                        Node::TemplateElement { cooked, .. } => text.push_str(ast.atom(cooked)),
                        _ => {
                            let v =
                                self.eval_constant(Loc::new(e.file, p), enum_sym, prev, depth + 1)?;
                            text.push_str(&self.literal_text(v)?);
                        }
                    }
                }
                Some(s.string_literal(&text, false))
            }
            Node::Paren { expr } => {
                self.eval_constant(Loc::new(e.file, expr), enum_sym, prev, depth + 1)
            }
            Node::Unary { op, arg } => {
                let v = self.eval_constant(Loc::new(e.file, arg), enum_sym, prev, depth + 1)?;
                let n = s.number_value(v)?;
                let r = match op {
                    UnaryOp::Minus => -n,
                    UnaryOp::Plus => n,
                    UnaryOp::BitNot => !(to_int32(n)) as f64,
                    _ => return None,
                };
                Some(s.number_literal(r, false))
            }
            Node::Binary { left, op, right } => {
                let a = self.eval_constant(Loc::new(e.file, left), enum_sym, prev, depth + 1)?;
                let b = self.eval_constant(Loc::new(e.file, right), enum_sym, prev, depth + 1)?;
                use ts_r_ast::BinaryOp as B;
                if op == B::Add && (s.string_value(a).is_some() || s.string_value(b).is_some()) {
                    let t = format!("{}{}", self.literal_text(a)?, self.literal_text(b)?);
                    return Some(s.string_literal(&t, false));
                }
                let (x, y) = (s.number_value(a)?, s.number_value(b)?);
                let r = match op {
                    B::Add => x + y,
                    B::Sub => x - y,
                    B::Mul => x * y,
                    B::Div => x / y,
                    B::Mod => x % y,
                    B::Exp => x.powf(y),
                    B::BitOr => (to_int32(x) | to_int32(y)) as f64,
                    B::BitAnd => (to_int32(x) & to_int32(y)) as f64,
                    B::BitXor => (to_int32(x) ^ to_int32(y)) as f64,
                    B::Shl => (to_int32(x).wrapping_shl(to_int32(y) as u32 & 31)) as f64,
                    B::Shr => (to_int32(x).wrapping_shr(to_int32(y) as u32 & 31)) as f64,
                    B::UShr => ((to_int32(x) as u32).wrapping_shr(to_int32(y) as u32 & 31)) as f64,
                    _ => return None,
                };
                Some(s.number_literal(r, false))
            }
            Node::Ident { name } => {
                let text = ast.atom(name);
                // A previous member of the same enum.
                for &(m, v) in prev {
                    if self.c.sym_name(m) == text {
                        return v;
                    }
                }
                let sym = self.c.resolve_ident(e, meaning::VALUE)?;
                // An imported constant is its target's value.
                let sym = self.c.resolve_alias(sym)?;
                // The global `NaN` and `Infinity` are constants.
                if matches!(text, "NaN" | "Infinity")
                    && self
                        .c
                        .first_decl(sym)
                        .is_some_and(|d| self.c.file(d.file).is_lib)
                {
                    let v = if text == "NaN" {
                        f64::NAN
                    } else {
                        f64::INFINITY
                    };
                    return Some(s.number_literal(v, false));
                }
                self.constant_of_symbol(sym, enum_sym, e, depth)
            }
            Node::Member { obj, prop, .. } => {
                let o = self
                    .resolve_entity(Loc::new(e.file, obj), meaning::VALUE | meaning::NAMESPACE)?;
                let o = self.c.resolve_alias(o)?;
                let pname = crate::nodes::ident_text(ast, prop)?;
                let m = self.c.export_of(o, pname)?;
                // An earlier member of the enum being evaluated (`E.a`, `A.B.E.a`).
                if let Some(&(_, v)) = prev.iter().find(|x| x.0 == m) {
                    return v;
                }
                self.constant_of_symbol(m, enum_sym, e, depth)
            }
            Node::Index { obj, index, .. } => {
                let o = self
                    .resolve_entity(Loc::new(e.file, obj), meaning::VALUE | meaning::NAMESPACE)?;
                let o = self.c.resolve_alias(o)?;
                // `E["a"]` or `E[`a`]` (a template without substitutions).
                let key = match *ast.node(skip_parens(ast, index)) {
                    Node::StringLit { value } => ast.atom(value),
                    Node::TemplateLit { parts } => match ast.list(parts) {
                        [one] => match *ast.node(*one) {
                            Node::TemplateElement { cooked, .. } => ast.atom(cooked),
                            _ => return None,
                        },
                        _ => return None,
                    },
                    _ => return None,
                };
                let m = self.c.export_of(o, key)?;
                if let Some(&(_, v)) = prev.iter().find(|x| x.0 == m) {
                    return v;
                }
                self.constant_of_symbol(m, enum_sym, e, depth)
            }
            _ => None,
        }
    }

    fn constant_of_symbol(
        &mut self,
        sym: SymId,
        enum_sym: SymId,
        at: Loc,
        depth: u32,
    ) -> Option<TypeId> {
        let flags = self.c.sym_flags(sym);
        if flags.intersects(SymbolFlags::ENUM_MEMBER) {
            let parent = self.c.sym_parent(sym)?;
            if parent == enum_sym {
                // Forward references to members of the enum being evaluated are not constant.
                return None;
            }
            let members = self.enum_members(parent);
            return members.iter().find(|m| m.0 == sym).and_then(|m| m.1);
        }
        if flags.intersects(SymbolFlags::BLOCK_SCOPED_VARIABLE)
            && let Some(v) = self.const_initializer_value(sym, at, depth)
        {
            return Some(v);
        }
        if flags.intersects(SymbolFlags::BLOCK_SCOPED_VARIABLE) {
            // In an expression (no enum), only a variable with an initializer is a constant:
            // `${lit}` of `declare const lit: "a"` is a `string` (observed, tsc 7).
            if enum_sym == SymId::NONE
                && !self
                    .c
                    .value_decl(sym)
                    .and_then(|d| self.declarator_of(d))
                    .is_some_and(|d| {
                        matches!(self.node(d), Node::VarDeclarator { init: Some(_), .. })
                    })
            {
                return None;
            }
            let t = self.type_of_symbol(sym);
            let s = self.store();
            if s.flags(t)
                .intersects(TypeFlags::STRING_LITERAL | TypeFlags::NUMBER_LITERAL)
                && !s.flags(t).contains(TypeFlags::ENUM_LITERAL)
            {
                return Some(s.regular(t));
            }
        }
        None
    }

    /// The constant value of an unannotated `const` declared (in the same file) before `at`:
    /// its initializer evaluated (`const C = 9000 % 2` is `0`).
    fn const_initializer_value(&mut self, sym: SymId, at: Loc, depth: u32) -> Option<TypeId> {
        let d = self.c.value_decl(sym)?;
        let decl = self.declarator_of(d)?;
        let Node::VarDeclarator {
            type_ann: None,
            init: Some(init),
            ..
        } = *self.node(decl)
        else {
            return None;
        };
        if !self.is_const_declaration(d) {
            return None;
        }
        if decl.file == at.file {
            let ast = self.ast(at.file);
            let span = ast.span(decl.node);
            let use_at = ast.span(at.node).start;
            // Inside its own initializer or before the declaration: not a constant.
            if use_at < span.end {
                return None;
            }
        }
        self.eval_constant(Loc::new(decl.file, init), SymId::NONE, &[], depth + 1)
    }

    /// The text of a string or number literal type (template literal concatenation).
    pub fn literal_text(&self, t: TypeId) -> Option<String> {
        let s = self.store();
        if let Some(v) = s.string_value(t) {
            return Some(v.to_owned());
        }
        s.number_value(t).map(ts_r_binder::js_number_to_string)
    }

    fn enum_declared_type(&mut self, s: SymId) -> TypeId {
        let members = self.enum_members(s);
        if members.is_empty() || members.iter().any(|m| m.1.is_none()) {
            return self.store().enum_type(s);
        }
        let mut lits: Vec<TypeId> = Vec::new();
        let mut seen: Vec<TypeId> = Vec::new();
        for &(m, v) in &members {
            let v = v.unwrap_or(TypeId::ERROR);
            if seen.contains(&v) {
                continue;
            }
            seen.push(v);
            lits.push(self.store().enum_literal(m, v, false));
        }
        if lits.len() == 1 {
            return lits[0];
        }
        let c = self.c;
        lits.sort_by(|&a, &b| c.compare_types(a, b, 0));
        let alias = self.store().alias(s, ListId::EMPTY);
        self.store().enum_union(&lits, alias)
    }

    /// The literal type of an enum member (regular, or fresh for expression use).
    pub fn enum_member_type(&mut self, m: SymId, fresh: bool) -> TypeId {
        let Some(e) = self.c.sym_parent(m) else {
            return TypeId::ERROR;
        };
        let members = self.enum_members(e);
        // A computed member is typed as its enum; members with literal values keep
        // their literal types even next to computed ones.
        let Some(&(_, own)) = members.iter().find(|x| x.0 == m) else {
            return TypeId::ERROR;
        };
        let Some(v) = own else {
            return self.store().enum_type(e);
        };
        // Members with equal values share one literal type (keyed by the first member).
        let first = members.iter().find(|x| x.1 == Some(v)).map_or(m, |x| x.0);
        self.store().enum_literal(first, v, fresh)
    }

    // ---------------------------------------------------------------------------------
    // Entity names
    // ---------------------------------------------------------------------------------

    /// Resolves `A`, `A.B.C` (`QualifiedName` or property access) to a symbol, without
    /// resolving the final alias.
    pub fn resolve_entity(&mut self, l: Loc, m: u8) -> Option<SymId> {
        let ast = self.ast(l.file);
        match *ast.node(l.node) {
            Node::Ident { .. } => self.c.resolve_ident(l, m),
            Node::QualifiedName { left, right }
            | Node::Member {
                obj: left,
                prop: right,
                ..
            } => {
                // `globalThis.X` (with no local `globalThis`): the global `X`.
                if matches!(*ast.node(left), Node::Ident { name } if ast.atom(name) == "globalThis")
                    && self
                        .c
                        .resolve_ident(Loc::new(l.file, left), meaning::ALL)
                        .is_none()
                {
                    let name = crate::nodes::ident_text(ast, right)?;
                    return self.c.global_symbol(name, m);
                }
                // The left of a type name is a namespace first (a nearer value of the name
                // does not hide it), then a value.
                let left = Loc::new(l.file, left);
                let ls = match (m & meaning::VALUE == 0)
                    .then(|| self.resolve_entity(left, meaning::NAMESPACE))
                    .flatten()
                {
                    Some(s) => s,
                    None => self.resolve_entity(left, meaning::NAMESPACE | meaning::VALUE)?,
                };
                // A default import of a module without a default export is the module
                // itself (synthetic default): `import net from 'net'; net.Socket`.
                let ls = match self.c.resolve_alias(ls) {
                    Some(r) => r,
                    None => self.c.synthetic_default_module(ls)?,
                };
                let name = crate::nodes::ident_text(ast, right)?;
                let r = self.c.export_of(ls, name)?;
                Some(r)
            }
            Node::Paren { expr } => self.resolve_entity(Loc::new(l.file, expr), m),
            Node::ExprWithTypeArgs { expr, .. } => self.resolve_entity(Loc::new(l.file, expr), m),
            _ => None,
        }
    }

    // ---------------------------------------------------------------------------------
    // Type nodes
    // ---------------------------------------------------------------------------------

    pub fn type_from_node(&mut self, l: Loc) -> TypeId {
        self.type_from_node_aliased(l, AliasId::NONE)
    }

    /// A type node in a deferred position (see `deferred_alias_ref`): only a plain
    /// reference without type arguments can be a forward reference.
    fn type_from_node_deferred(&mut self, l: Loc) -> TypeId {
        let ast = self.ast(l.file);
        let n = crate::nodes::skip_type_parens(ast, l.node);
        if !matches!(
            *ast.node(n),
            Node::TypeRef {
                type_args: None,
                ..
            }
        ) {
            return self.type_from_node(l);
        }
        self.deferred_alias_ref = true;
        let t = self.type_from_node(Loc::new(l.file, n));
        self.deferred_alias_ref = false;
        t
    }

    pub fn type_from_node_aliased(&mut self, l: Loc, alias: AliasId) -> TypeId {
        if alias == AliasId::NONE
            && let Some(&t) = self.type_nodes.get(&l)
        {
            return t;
        }
        if self.depth > 400 {
            return TypeId::UNRESOLVED;
        }
        self.depth += 1;
        let t = self.type_from_node_inner(l, alias);
        self.depth -= 1;
        if alias == AliasId::NONE {
            self.type_nodes.insert(l, t);
        }
        t
    }

    fn type_from_node_inner(&mut self, l: Loc, alias: AliasId) -> TypeId {
        let f = l.file;
        let ast = self.ast(f);
        let at = |n: NodeId| Loc::new(f, n);
        match *ast.node(l.node) {
            Node::KeywordType { kind } => match kind {
                KeywordTypeKind::Any => TypeId::ANY,
                KeywordTypeKind::Unknown => TypeId::UNKNOWN,
                KeywordTypeKind::Number => TypeId::NUMBER,
                KeywordTypeKind::BigInt => TypeId::BIGINT,
                KeywordTypeKind::Boolean => TypeId::BOOLEAN,
                KeywordTypeKind::String => TypeId::STRING,
                KeywordTypeKind::Symbol => TypeId::SYMBOL,
                KeywordTypeKind::Object => TypeId::OBJECT,
                KeywordTypeKind::Never => TypeId::NEVER,
                KeywordTypeKind::Void => TypeId::VOID,
                KeywordTypeKind::Undefined => TypeId::UNDEFINED,
                KeywordTypeKind::Null => TypeId::NULL,
                KeywordTypeKind::Intrinsic | KeywordTypeKind::Const => TypeId::UNRESOLVED,
            },
            Node::ParenType { ty } => self.type_from_node_aliased(at(ty), alias),
            Node::TypeRef { name, type_args } => {
                self.type_reference(at(name), type_args.map(at), alias)
            }
            Node::ExprWithTypeArgs { expr, type_args } => {
                self.type_reference(at(expr), type_args.map(at), AliasId::NONE)
            }
            Node::ArrayType { elem } => {
                let e = self.type_from_node_deferred(at(elem));
                let t = self.create_array(e, false);
                self.with_alias(t, alias)
            }
            Node::TupleType { elems } => {
                let t = self.tuple_from_node(f, elems, false);
                self.tuple_alias(t, f, elems, alias)
            }
            Node::TypeOperator { op, ty } => match op {
                TypeOperatorKind::Readonly => {
                    let inner = crate::nodes::skip_type_parens(ast, ty);
                    match *ast.node(inner) {
                        Node::ArrayType { elem } => {
                            let e = self.type_from_node_deferred(at(elem));
                            let t = self.create_array(e, true);
                            self.with_alias(t, alias)
                        }
                        Node::TupleType { elems } => {
                            let t = self.tuple_from_node(f, elems, true);
                            self.tuple_alias(t, f, elems, alias)
                        }
                        _ => self.type_from_node(at(ty)),
                    }
                }
                TypeOperatorKind::KeyOf => {
                    let t = self.type_from_node(at(ty));
                    self.keyof_type(t)
                }
                TypeOperatorKind::Unique => {
                    // `unique symbol` on a const or readonly static property declaration.
                    let parents = self.c.file(f).parents();
                    let mut cur = parents.parent(l.node);
                    let mut found = None;
                    let mut guard = 0;
                    while let Some(n) = cur {
                        guard += 1;
                        if guard > 8 {
                            break;
                        }
                        match *ast.node(n) {
                            Node::VarDeclarator { name, .. } => {
                                found = self.c.sym_of_decl(at(name));
                                break;
                            }
                            Node::PropertyDef { .. } | Node::PropertySignature { .. } => {
                                found = self.c.sym_of_decl(at(n));
                                break;
                            }
                            _ => {}
                        }
                        cur = parents.parent(n);
                    }
                    match found {
                        Some(s) => self.store().unique_symbol(s),
                        None => TypeId::SYMBOL,
                    }
                }
            },
            Node::UnionType { types } => {
                let parts: Vec<TypeId> = ast
                    .list(types)
                    .iter()
                    .map(|&t| self.type_from_node(at(t)))
                    .collect();
                self.union_with(&parts, crate::unions::Reduction::Literal, alias)
            }
            Node::IntersectionType { types } => {
                let parts: Vec<TypeId> = ast
                    .list(types)
                    .iter()
                    .map(|&t| self.type_from_node(at(t)))
                    .collect();
                match self.written_primitive_and_empty_object(&parts, alias) {
                    Some(t) => t,
                    None => self.intersection_with(&parts, alias),
                }
            }
            Node::FunctionType { .. } | Node::ConstructorType { .. } => {
                let outer = !self.referenced_outer_type_params(l).is_empty();
                self.store().anonymous(
                    (f.0, l.node.raw()),
                    anon::FUNCTION_TYPE,
                    MapperId::IDENTITY,
                    ObjectFlags::ANONYMOUS | ObjectFlags::FUNCTION_LIKE,
                    alias,
                    outer,
                )
            }
            Node::TypeLiteral { .. } => {
                let outer = !self.referenced_outer_type_params(l).is_empty();
                self.store().anonymous(
                    (f.0, l.node.raw()),
                    anon::TYPE_LITERAL,
                    MapperId::IDENTITY,
                    ObjectFlags::ANONYMOUS | ObjectFlags::TYPE_LITERAL,
                    alias,
                    outer,
                )
            }
            Node::TypeQuery {
                expr_name,
                type_args,
            } => {
                // `typeof x` of an unresolved import is the plain error type, which infers
                // itself (`Parameters<typeof x>` is the error type), unlike an unresolved
                // type name (`Parameters<Missing>` is `unknown[]`; `error_type_inference`).
                let t = self.type_of_entity_expression(at(expr_name));
                match type_args {
                    // `typeof f<string>`
                    Some(ta) => {
                        let args = self.type_args_from_node(Some(at(ta)), false);
                        let r = self.instantiation_expression_type(t, &args, None);
                        if r != t {
                            let shown: Vec<String> =
                                args.iter().map(|&a| self.type_to_string(a)).collect();
                            let name = format!(
                                "typeof {}<{}>",
                                self.c.text(at(expr_name)),
                                shown.join(", ")
                            );
                            self.c.set_instantiation_query_name(r, name);
                        }
                        r
                    }
                    None => t,
                }
            }
            Node::IndexedAccessType { obj, index } => {
                let o = self.type_from_node(at(obj));
                let i = self.type_from_node(at(index));
                let t = self
                    .indexed_access_type(o, i, None)
                    .unwrap_or(TypeId::UNRESOLVED);
                // A union of property types read through a union of keys is a fresh union
                // that takes the alias being declared (`type ArrayBufferLike =
                // ArrayBufferTypes[keyof ArrayBufferTypes]` prints as `ArrayBufferLike`).
                if alias != AliasId::NONE
                    && self.store().flags(i).contains(TypeFlags::UNION)
                    && self.store().flags(t).contains(TypeFlags::UNION)
                {
                    self.with_alias(t, alias)
                } else {
                    t
                }
            }
            Node::StringLit { value } => self.store().string_literal(ast.atom(value), false),
            Node::NumberLit { value } => self.store().number_literal(value.get(), false),
            Node::BigIntLit { value } => {
                let text = bigint_text(ast.atom(value));
                self.store().bigint_literal(&text, false)
            }
            Node::BoolLit { value } => self.store().boolean_literal(value, false),
            Node::NullLit {} => TypeId::NULL,
            Node::Unary {
                op: UnaryOp::Minus,
                arg,
            } => match *ast.node(arg) {
                Node::NumberLit { value } => self.store().number_literal(-value.get(), false),
                Node::BigIntLit { value } => {
                    let text = bigint_text(ast.atom(value));
                    let neg = if text == "0" {
                        text
                    } else {
                        format!("-{text}")
                    };
                    self.store().bigint_literal(&neg, false)
                }
                _ => TypeId::ERROR,
            },
            Node::TemplateLit { parts } => {
                let parts = ast.list(parts);
                if let [p] = parts
                    && let Node::TemplateElement { cooked, .. } = *ast.node(*p)
                {
                    return self.store().string_literal(ast.atom(cooked), false);
                }
                TypeId::UNRESOLVED
            }
            Node::TemplateLitType { .. } => self.template_type_from_node(l),
            Node::MappedType { .. } => self.mapped_type_from_node(l, alias),
            Node::ConditionalType { .. } => self.conditional_type_from_node(l, alias),
            Node::InferType { type_param } => match self.c.sym_of_decl(at(type_param)) {
                Some(sym) => self.store().type_param(sym, false),
                None => TypeId::UNRESOLVED,
            },
            Node::ThisType {} => self.this_type_at(l).unwrap_or(TypeId::ERROR),
            Node::TypePredicate { asserts, .. } => {
                if asserts {
                    TypeId::VOID
                } else {
                    TypeId::BOOLEAN
                }
            }
            Node::ImportType {
                is_typeof,
                arg,
                qualifier,
                type_args,
                ..
            } => self.import_type(l, arg, is_typeof, qualifier, type_args),
            // JSDoc `{T=}` outside a tuple or parameter list is `T | undefined`.
            Node::OptionalType { ty } => {
                let t = self.type_from_node(at(ty));
                self.add_undefined(t)
            }
            Node::RestType { ty } => self.type_from_node(at(ty)),
            Node::NamedTupleMember { ty, .. } => self.type_from_node(at(ty)),
            Node::Missing {} => TypeId::ERROR,
            _ => TypeId::UNRESOLVED,
        }
    }

    fn tuple_from_node(
        &mut self,
        f: ts_r_diagnostics::FileId,
        elems: ts_r_ast::NodeList,
        readonly: bool,
    ) -> TypeId {
        let ast = self.ast(f);
        let mut out = Vec::new();
        for &e in ast.list(elems) {
            let (kind, ty, label) = match *ast.node(e) {
                Node::OptionalType { ty } => (TupleElement::OPTIONAL, ty, None),
                Node::RestType { ty } => (TupleElement::REST, ty, None),
                Node::NamedTupleMember {
                    rest,
                    name,
                    optional,
                    ty,
                    ..
                } => {
                    let k = if rest {
                        TupleElement::REST
                    } else if optional {
                        TupleElement::OPTIONAL
                    } else {
                        TupleElement::REQUIRED
                    };
                    let label =
                        crate::nodes::ident_text(ast, name).map(|s| self.store().intern_str(s));
                    (k, ty, label)
                }
                _ => (TupleElement::REQUIRED, e, None),
            };
            let t = if kind == TupleElement::REST {
                self.type_from_node(Loc::new(f, ty))
            } else {
                self.type_from_node_deferred(Loc::new(f, ty))
            };
            out.push(TupleEl { ty: t, kind, label });
        }
        self.create_tuple(&out, readonly)
    }

    /// The type of a type reference `name<args>`. A reference that is the whole body of a
    /// type alias declaration gets that alias (`type O<T> = Pick<T, "a">` prints as `O<X>`).
    fn type_reference(&mut self, name: Loc, type_args: Option<Loc>, alias: AliasId) -> TypeId {
        let deferred = std::mem::take(&mut self.deferred_alias_ref);
        if self.ast(name.file).is_jsdoc(name.node)
            && let Some(t) = self.jsdoc_type_reference(name, type_args)
        {
            return t;
        }
        let Some(sym) = self.resolve_entity(name, meaning::TYPE) else {
            return self.unresolved_reference_type(name, type_args);
        };
        let Some(sym) = self.c.resolve_alias(sym) else {
            return self.unresolved_reference_type(name, type_args);
        };
        // `J[]` inside `type J = string | J[]` (or inside `type A = J[]` with `type J =
        // string | A`): a forward reference to `J`, linked once `J` is resolved.
        if deferred
            && type_args.is_none()
            && self.c.sym_flags(sym).intersects(SymbolFlags::TYPE_ALIAS)
            && self.local_type_params(sym).is_empty()
        {
            if self.is_resolving(Resolving::DeclaredType(sym)) {
                return self.store().forward(sym);
            }
            let known = self
                .c
                .sym_links(sym)
                .is_some_and(|l| l.declared.get().is_some());
            if !known && self.in_alias_resolution() {
                self.pending_forwards.push(sym);
                return self.store().forward(sym);
            }
        }
        // Only `Array<J>` / `ReadonlyArray<J>` (like `J[]`): other generic interfaces (a
        // `Promise<J>`) would make types whose awaited or apparent forms nest forever.
        let defer_args = type_args.is_some()
            && self.c.sym_flags(sym).intersects(SymbolFlags::INTERFACE)
            && (self.c.global_symbol("Array", meaning::TYPE) == Some(sym)
                || self.c.global_symbol("ReadonlyArray", meaning::TYPE) == Some(sym));
        let args = self.type_args_from_node(type_args, defer_args);
        // The wrong number of type arguments (TS2314/TS2707, reported by the walk) makes
        // the reference tsc's error type (TypeScript files only).
        if !self.c.file(name.file).is_js()
            && self
                .c
                .sym_flags(sym)
                .intersects(SymbolFlags::CLASS | SymbolFlags::INTERFACE | SymbolFlags::TYPE_ALIAS)
        {
            let local = self.local_type_params(sym);
            if !local.is_empty() {
                let mut min = local.len();
                while min > 0 && self.type_param_default(local[min - 1]).is_some() {
                    min -= 1;
                }
                if args.len() < min || args.len() > local.len() {
                    return TypeId::ERROR;
                }
            }
        }
        if self
            .c
            .sym_flags(sym)
            .intersects(SymbolFlags::TYPE_PARAMETER)
            && (self.is_static_type_parameter_use(name, sym)
                || self.is_computed_name_type_parameter_use(name, sym))
        {
            return TypeId::ERROR;
        }
        let t = self.type_from_symbol_reference(sym, &args);
        // A generic class or interface reference written as an alias's type is named by the
        // alias (`type Table = TableClass` prints `Table`).
        if alias != AliasId::NONE
            && self
                .c
                .sym_flags(sym)
                .intersects(SymbolFlags::CLASS | SymbolFlags::INTERFACE)
        {
            return self.with_alias(t, alias);
        }
        if alias != AliasId::NONE
            && self.c.sym_flags(sym).intersects(SymbolFlags::TYPE_ALIAS)
            && !self.local_type_params(sym).is_empty()
        {
            let d = self.declared_type_of_symbol(sym);
            if self.alias_carries_over(d, t) {
                return self.with_alias(t, alias);
            }
        }
        t
    }

    pub(crate) fn type_args_from_node(
        &mut self,
        type_args: Option<Loc>,
        deferred: bool,
    ) -> Vec<TypeId> {
        match type_args {
            Some(ta) => match *self.node(ta) {
                Node::TypeArgs { args } => {
                    let ast = self.ast(ta.file);
                    ast.list(args)
                        .iter()
                        .map(|&a| {
                            let l = Loc::new(ta.file, a);
                            if deferred {
                                self.type_from_node_deferred(l)
                            } else {
                                self.type_from_node(l)
                            }
                        })
                        .collect()
                }
                _ => Vec::new(),
            },
            None => Vec::new(),
        }
    }

    /// The error type of a type reference whose name does not resolve: tsc prints it as
    /// written, with its type arguments resolved (`Problem`, `z.infer<any>`, `N.Q2`), and
    /// it behaves as the error type everywhere else.
    fn unresolved_reference_type(&mut self, name: Loc, type_args: Option<Loc>) -> TypeId {
        let text = crate::nodes::entity_text(self.ast(name.file), name.node);
        if text.is_empty() {
            return TypeId::ERROR;
        }
        let args = self.type_args_from_node(type_args, false);
        if args.is_empty() {
            return self.store().named_error(&text);
        }
        let printed: Vec<String> = args.iter().map(|&a| self.type_to_string(a)).collect();
        let mut name = format!("{text}<{}>", printed.join(", "));
        // Arguments that are themselves such names would double it with every level.
        if name.len() > MAX_ERROR_NAME_LEN {
            let mut end = MAX_ERROR_NAME_LEN;
            while !name.is_char_boundary(end) {
                end -= 1;
            }
            name.truncate(end);
            name.push_str("...");
        }
        self.store().named_error(&name)
    }

    /// Whether instantiating the declared type `d` of a generic alias gives a type that takes
    /// the alias of an enclosing alias declaration: unions, intersections, object types and
    /// conditional types do; template literal, `keyof`, indexed access and string mapping
    /// types do not (`type T2 = Tpl<"a" | "b">` prints as the resolved union).
    fn alias_carries_over(&mut self, d: TypeId, t: TypeId) -> bool {
        let s = self.store();
        let f = s.flags(d);
        if f.intersects(TypeFlags::UNION | TypeFlags::INTERSECTION | TypeFlags::OBJECT) {
            return true;
        }
        if let Some(cd) = s.conditional_data(d) {
            // A branch type the conditional resolves to keeps its own name (`type V =
            // Infer<Ob<{ a: 1 }>>` is `{ a: 1 }`); a union distributed over takes the alias.
            return s.flags(t).contains(TypeFlags::CONDITIONAL)
                || (s.conditional_root_data(cd.root).distributive
                    && s.flags(t).contains(TypeFlags::UNION));
        }
        false
    }

    /// A tuple written as an alias's type takes the alias unless it spreads a type that is
    /// not written as an array (`type U = [...Numbers, boolean]` prints expanded).
    fn tuple_alias(
        &mut self,
        t: TypeId,
        f: ts_r_diagnostics::FileId,
        elems: ts_r_ast::NodeList,
        alias: AliasId,
    ) -> TypeId {
        if alias == AliasId::NONE {
            return t;
        }
        let ast = self.ast(f);
        let variadic = ast.list(elems).iter().any(|&e| {
            let spread = match *ast.node(e) {
                Node::RestType { ty } => Some(ty),
                Node::NamedTupleMember { rest: true, ty, .. } => Some(ty),
                _ => None,
            };
            spread.is_some_and(|ty| !written_as_array(ast, ty, 0))
        });
        if variadic {
            t
        } else {
            self.with_alias(t, alias)
        }
    }

    /// A class, interface, array or tuple reference without the alias that names it.
    pub fn without_reference_alias(&mut self, t: TypeId) -> TypeId {
        let s = self.store();
        match s.object_of(t) {
            Some(o) if o.alias != AliasId::NONE => match o.kind {
                ObjectKind::Reference { target, args } => {
                    let args = s.list_items(args).to_vec();
                    s.reference(target, &args)
                }
                _ => t,
            },
            _ => t,
        }
    }

    /// `t` re-interned with another alias (unions, intersections, anonymous, mapped and
    /// conditional types carry one; other types are returned unchanged).
    pub fn with_alias(&mut self, t: TypeId, alias: AliasId) -> TypeId {
        let s = self.store();
        match s.kind(t) {
            TypeKind::Union { list, alias: a } if a != alias => {
                if s.traits(t).contains(TypeTraits::ENUM_UNION) {
                    return t;
                }
                let items = s.list_items(list).to_vec();
                s.union_sorted(&items, Some(alias))
            }
            TypeKind::Intersection { list, alias: a } if a != alias => {
                let items = s.list_items(list).to_vec();
                s.intersection_sorted(&items, Some(alias))
            }
            TypeKind::Conditional(_) => match s.conditional_data(t) {
                Some(d) if d.alias != alias => s.conditional(d.root, d.mapper, alias),
                _ => t,
            },
            TypeKind::Object(o) => {
                let od = s.object(o);
                if od.alias == alias {
                    return t;
                }
                let contains = s.traits(t).contains(TypeTraits::CONTAINS_TYPE_PARAMETER);
                match od.kind {
                    ObjectKind::Anonymous { decl, kind, mapper } => s.anonymous(
                        decl,
                        kind,
                        mapper,
                        od.flags.without(ObjectFlags::INSTANTIATED),
                        alias,
                        contains,
                    ),
                    // An instantiated homomorphic mapped type keeps its declared alias
                    // (`type P = Partial<X>` prints as `Partial<X>`).
                    ObjectKind::Mapped { decl, mapper } => {
                        if mapper != MapperId::IDENTITY && self.is_homomorphic_mapped(t) {
                            return t;
                        }
                        s.mapped_object(decl, mapper, alias, contains)
                    }
                    // A reference named by an alias (`type M = Map<string, number>`).
                    ObjectKind::Reference { target, args } => {
                        let args = s.list_items(args).to_vec();
                        s.reference_aliased(target, &args, alias)
                    }
                    _ => t,
                }
            }
            _ => t,
        }
    }

    /// The type a reference to `sym` with `args` denotes.
    pub fn type_from_symbol_reference(&mut self, sym: SymId, args: &[TypeId]) -> TypeId {
        let flags = self.c.sym_flags(sym);
        if flags.intersects(SymbolFlags::CLASS | SymbolFlags::INTERFACE) {
            let d = self.declared_type_of_symbol(sym);
            let local = self.local_type_params(sym);
            if local.is_empty() {
                return d;
            }
            let filled = self.fill_type_args(&local, args);
            let all = match self.store().object_of(d).map(|o| o.kind) {
                Some(ObjectKind::Declared { type_params, .. }) => {
                    self.store().list_items(type_params).to_vec()
                }
                _ => return d,
            };
            let outer_n = all.len().saturating_sub(local.len());
            let mut full: Vec<TypeId> = all[..outer_n].to_vec();
            full.extend(filled);
            return self.make_reference(d, &full);
        }
        if flags.intersects(SymbolFlags::TYPE_ALIAS) {
            let d = self.declared_type_of_symbol(sym);
            let local = self.local_type_params(sym);
            if local.is_empty() {
                return d;
            }
            let filled = self.fill_type_args(&local, args);
            let m = self.store().mapper(&local, &filled);
            return self.instantiate(d, m);
        }
        if flags
            .intersects(SymbolFlags::ENUM | SymbolFlags::ENUM_MEMBER | SymbolFlags::TYPE_PARAMETER)
        {
            return self.declared_type_of_symbol(sym);
        }
        TypeId::ERROR
    }

    /// Fills missing type arguments with defaults (instantiated by the previous arguments)
    /// or `unknown`-like placeholders.
    pub fn fill_type_args(&mut self, params: &[TypeId], args: &[TypeId]) -> Vec<TypeId> {
        let mut out: Vec<TypeId> = args.iter().take(params.len()).copied().collect();
        while out.len() < params.len() {
            let p = params[out.len()];
            let t = match self.type_param_default(p) {
                Some(d) => {
                    let m = self.store().mapper(&params[..out.len()], &out);
                    self.instantiate(d, m)
                }
                None => TypeId::UNRESOLVED,
            };
            out.push(t);
        }
        out
    }

    /// `typeof a.b.c` in a type position.
    pub(crate) fn type_of_entity_expression(&mut self, l: Loc) -> TypeId {
        let ast = self.ast(l.file);
        match *ast.node(l.node) {
            Node::Ident { name } => {
                // `typeof this` is the `this` expression there (narrowed, and checked as one).
                if ast.atom(name) == "this" {
                    return self.check_this(l);
                }
                match self.c.resolve_ident(l, meaning::VALUE) {
                    Some(s) => {
                        let t = self.type_of_symbol(s);
                        self.flow_type_of_reference_quiet(l, s, t)
                    }
                    None if ast.atom(name) == "globalThis" => self.global_this_type(),
                    // `typeof undefined`: `undefined`, widened to `any` without
                    // strictNullChecks.
                    None if ast.atom(name) == "undefined" => {
                        if self.c.opts.strict_null_checks {
                            TypeId::UNDEFINED
                        } else {
                            TypeId::ANY
                        }
                    }
                    None => TypeId::ERROR,
                }
            }
            Node::QualifiedName { left, right } => {
                // A namespace member, or a property of a value.
                if let Some(s) = self.resolve_entity(l, meaning::VALUE) {
                    let s = self.c.resolve_alias(s).unwrap_or(s);
                    return self.type_of_symbol(s);
                }
                let lt = self.type_of_entity_expression(Loc::new(l.file, left));
                // A property of an `any` value is `any` (`typeof anyValue.p`, observed).
                if matches!(lt, TypeId::ANY | TypeId::IMPLICIT_ANY) {
                    return TypeId::ANY;
                }
                // A possibly nullish value has no members (`typeof v.a`, TS18048).
                let lt = self.check_non_null(Loc::new(l.file, left), lt);
                if self.store().is_error(lt) {
                    return TypeId::ERROR;
                }
                let Some(name) = crate::nodes::ident_text(ast, right) else {
                    return TypeId::ERROR;
                };
                match self.property_of_type(lt, name) {
                    // Narrowed as the reference it names, where the type stands (`typeof x.p`
                    // after `x.p === "A"`, `if (this.a) { let b: typeof this.a.b }`).
                    Some(p) => {
                        let t = self.type_of_symbol(p);
                        self.quietly(|ck| ck.flow_type_of_reference(l, t, t))
                    }
                    None => TypeId::UNRESOLVED,
                }
            }
            Node::ImportType { .. } => self.type_from_node(l),
            // `typeof this` is the `this` expression there (narrowed, and checked as one).
            Node::This {} => self.check_this(l),
            _ => TypeId::UNRESOLVED,
        }
    }

    /// The polymorphic `this` type at a location (enclosing class or interface), if the
    /// `this` type is available there ([`Self::this_type_container`]).
    pub fn this_type_at(&mut self, l: Loc) -> Option<TypeId> {
        let c = self.this_type_container(l)?;
        let s = self.c.sym_of_decl(c)?;
        Some(self.store().type_param(s, true))
    }

    /// The class or interface whose `this` type a `this` type node at `l` names. It is only
    /// available in a non-static member of a class or interface (observed tsc 7): not in
    /// type literals, object literal methods, functions, constructor parameters, static
    /// members, heritage clauses or type parameters. Arrow functions and function types are
    /// transparent.
    pub(crate) fn this_type_container(&self, l: Loc) -> Option<Loc> {
        let ast = self.ast(l.file);
        let parents = self.c.file(l.file).parents();
        let mut prev = l.node;
        let mut cur = parents.parent(l.node);
        let mut in_params = false;
        for _ in 0..1000 {
            let n = cur?;
            match *ast.node(n) {
                Node::TypeLiteral { .. }
                | Node::ObjectLit { .. }
                | Node::FunctionDecl { .. }
                | Node::Module { .. }
                | Node::Program { .. } => return None,
                Node::FunctionExpr { params, .. } => {
                    let is_method = parents
                        .parent(n)
                        .is_some_and(|m| matches!(ast.node(m), Node::MethodDef { .. }));
                    if !is_method {
                        return None;
                    }
                    in_params = ast.list(params).contains(&prev);
                }
                Node::MethodDef {
                    modifiers, kind, ..
                } => {
                    // A constructor's body sees the class's `this`, its parameters do not.
                    if (kind == ts_r_ast::MethodKind::Constructor && in_params)
                        || modifiers.contains(ts_r_ast::Modifiers::STATIC)
                    {
                        return None;
                    }
                }
                Node::PropertyDef { modifiers, .. } => {
                    if modifiers.contains(ts_r_ast::Modifiers::STATIC) {
                        return None;
                    }
                }
                Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => {
                    return ast
                        .list(members)
                        .contains(&prev)
                        .then_some(Loc::new(l.file, n));
                }
                Node::Interface { body, .. } => {
                    return ast
                        .list(body)
                        .contains(&prev)
                        .then_some(Loc::new(l.file, n));
                }
                _ => {}
            }
            prev = n;
            cur = parents.parent(n);
        }
        None
    }

    fn import_type(
        &mut self,
        l: Loc,
        arg: NodeId,
        is_typeof: bool,
        qualifier: Option<NodeId>,
        type_args: Option<NodeId>,
    ) -> TypeId {
        let ast = self.ast(l.file);
        let lit = match *ast.node(arg) {
            Node::StringLit { .. } => arg,
            Node::TypeRef { name, .. } => name,
            _ => arg,
        };
        let bound = self.c.bound(l.file);
        let Some(req) = bound
            .module_requests
            .iter()
            .position(|r| r.node == lit || r.node == arg)
        else {
            return TypeId::UNRESOLVED;
        };
        let target = self.c.link.module_target(l.file, req as u32);
        let Some(&first) = self.c.link.module_symbols(target).first() else {
            return TypeId::UNRESOLVED;
        };
        let msym = self.c.sym_ref(first);
        // `import("m")` as a type: the module's `export =` class or other type (TS1340,
        // reported by the walk, for a module that is no type).
        if qualifier.is_none() && !is_typeof {
            return match self.module_as_type(msym) {
                Ok(Some(s)) => {
                    self.module_type_reference(s, type_args.map(|t| Loc::new(l.file, t)))
                }
                Ok(None) => TypeId::UNRESOLVED,
                Err(()) => TypeId::ERROR,
            };
        }
        // A CommonJS JavaScript module's exports (`module.exports =`) are not modelled.
        if let Some(d) = self.c.first_decl(msym)
            && self.c.file(d.file).is_js()
            && self.c.file(d.file).is_commonjs_module()
        {
            return TypeId::UNRESOLVED;
        }
        let mut cur = msym;
        if let Some(q) = qualifier {
            let mut names = Vec::new();
            let mut n = q;
            let mut guard = 0;
            loop {
                guard += 1;
                if guard > 1000 {
                    return TypeId::UNRESOLVED;
                }
                match *ast.node(n) {
                    Node::QualifiedName { left, right } => {
                        names.push(right);
                        n = left;
                    }
                    _ => {
                        names.push(n);
                        break;
                    }
                }
            }
            names.reverse();
            for nm in names {
                let Some(text) = crate::nodes::ident_text(ast, nm) else {
                    return TypeId::UNRESOLVED;
                };
                let Some(next) = self.c.export_of(cur, text) else {
                    return TypeId::ERROR;
                };
                cur = match self.c.resolve_alias(next) {
                    Some(x) => x,
                    None => return TypeId::ERROR,
                };
            }
        } else if !is_typeof {
            return TypeId::ERROR;
        }
        if is_typeof {
            return self.type_of_symbol(cur);
        }
        let args: Vec<TypeId> = match type_args.map(|t| *ast.node(t)) {
            Some(Node::TypeArgs { args }) => ast
                .list(args)
                .iter()
                .map(|&a| self.type_from_node(Loc::new(l.file, a)))
                .collect(),
            _ => Vec::new(),
        };
        self.type_from_symbol_reference(cur, &args)
    }

    /// The anonymous type of a symbol-keyed declaration (`typeof f`, `typeof C`, `typeof N`).
    pub fn symbol_anonymous(&mut self, s: SymId, kind: u8) -> TypeId {
        let first = self.c.first_decl(s);
        let outer = first
            .map(|d| !self.outer_type_params(d).is_empty())
            .unwrap_or(false);
        let mut flags = ObjectFlags::ANONYMOUS;
        match kind {
            anon::SYMBOL_FUNCTION => flags |= ObjectFlags::FUNCTION_LIKE,
            anon::CLASS_STATIC => flags |= ObjectFlags::CLASS_STATIC,
            anon::NAMESPACE => flags |= ObjectFlags::MODULE,
            _ => {}
        }
        self.store().anonymous(
            sym_key(s),
            kind,
            MapperId::IDENTITY,
            flags,
            AliasId::NONE,
            outer,
        )
    }

    /// Whether a declaration's signature is generic.
    pub fn has_own_type_params(&self, l: Loc) -> bool {
        type_params_of(self.ast(l.file), l.node).is_some()
    }

    /// Return type node of a function-like declaration.
    pub fn return_type_node(&self, l: Loc) -> Option<Loc> {
        fn_parts(self.ast(l.file), l.node)
            .and_then(|p| p.return_type)
            .map(|r| Loc::new(l.file, r))
    }

    /// tsc's `isGenericType`: a generic object type (type variables, generic mapped types,
    /// tuples with variadic type-variable elements) or a generic index type (`keyof T`,
    /// template literal and string mapping types over type variables).
    pub fn is_generic_type(&mut self, t: TypeId) -> bool {
        self.is_generic_object_type(t) || self.is_generic_index_type(t)
    }

    /// tsc's `isGenericObjectType`.
    pub fn is_generic_object_type(&mut self, t: TypeId) -> bool {
        let s = self.store();
        let f = s.flags(t);
        if f.intersects(TypeFlags::INSTANTIABLE_NON_PRIMITIVE) {
            return true;
        }
        if f.intersects(TypeFlags::UNION_OR_INTERSECTION) {
            if !s.traits(t).contains(TypeTraits::CONTAINS_TYPE_PARAMETER) {
                return false;
            }
            let ms = s.members_of_union(t).to_vec();
            return ms.into_iter().any(|m| self.is_generic_object_type(m));
        }
        if !f.contains(TypeFlags::OBJECT)
            || !s.traits(t).contains(TypeTraits::CONTAINS_TYPE_PARAMETER)
        {
            return false;
        }
        if self.is_generic_mapped_type(t) {
            return true;
        }
        // A tuple with a variadic element of a type variable.
        if let Some(shape) = s.tuple_shape_of(t)
            && shape
                .elements
                .iter()
                .any(|e| e.kind == TupleElement::VARIADIC)
        {
            return true;
        }
        false
    }

    /// tsc's `isGenericIndexType`.
    pub fn is_generic_index_type(&mut self, t: TypeId) -> bool {
        let s = self.store();
        let f = s.flags(t);
        if f.intersects(TypeFlags::INSTANTIABLE_NON_PRIMITIVE | TypeFlags::INDEX) {
            return true;
        }
        if f.intersects(TypeFlags::TEMPLATE_LITERAL | TypeFlags::STRING_MAPPING) {
            return s.traits(t).contains(TypeTraits::CONTAINS_TYPE_PARAMETER);
        }
        if f.intersects(TypeFlags::UNION_OR_INTERSECTION) {
            if !s.traits(t).contains(TypeTraits::CONTAINS_TYPE_PARAMETER) {
                return false;
            }
            let ms = s.members_of_union(t).to_vec();
            return ms.into_iter().any(|m| self.is_generic_index_type(m));
        }
        false
    }

    pub fn type_kind(&self, t: TypeId) -> TypeKind {
        self.store().kind(t)
    }
}

/// `123n` text without the suffix, normalised to decimal.
pub fn bigint_text(raw: &str) -> String {
    let r = raw.trim_end_matches('n').replace('_', "");
    let (radix, digits) = if let Some(d) = r.strip_prefix("0x").or_else(|| r.strip_prefix("0X")) {
        (16, d)
    } else if let Some(d) = r.strip_prefix("0o").or_else(|| r.strip_prefix("0O")) {
        (8, d)
    } else if let Some(d) = r.strip_prefix("0b").or_else(|| r.strip_prefix("0B")) {
        (2, d)
    } else {
        (10, r.as_str())
    };
    if radix == 10 {
        let t = digits.trim_start_matches('0');
        return if t.is_empty() {
            "0".to_owned()
        } else {
            t.to_owned()
        };
    }
    // Convert to decimal with simple big arithmetic (vector of base-1e9 limbs).
    let mut limbs: Vec<u64> = vec![0];
    for ch in digits.chars() {
        let Some(d) = ch.to_digit(radix) else {
            continue;
        };
        let mut carry = d as u64;
        for l in limbs.iter_mut() {
            let v = *l * radix as u64 + carry;
            *l = v % 1_000_000_000;
            carry = v / 1_000_000_000;
        }
        while carry > 0 {
            limbs.push(carry % 1_000_000_000);
            carry /= 1_000_000_000;
        }
    }
    let mut s = String::new();
    for (i, l) in limbs.iter().rev().enumerate() {
        if i == 0 {
            s.push_str(&l.to_string());
        } else {
            s.push_str(&format!("{l:09}"));
        }
    }
    s
}

/// ECMAScript ToInt32.
pub fn to_int32(v: f64) -> i32 {
    if !v.is_finite() {
        return 0;
    }
    let m = v.trunc().rem_euclid(4294967296.0);
    (m as u64 as u32) as i32
}

/// Whether a spread tuple element's type is written as an array (`...X[]`, `...(X[])`,
/// `...[...X[]]`): a rest element rather than a variadic one.
fn written_as_array(ast: &ts_r_ast::Ast, n: NodeId, depth: u32) -> bool {
    if depth > 32 {
        return false;
    }
    match *ast.node(n) {
        Node::ArrayType { .. } => true,
        Node::ParenType { ty } => written_as_array(ast, ty, depth + 1),
        Node::TupleType { elems } => match ast.list(elems) {
            [one] => match *ast.node(*one) {
                Node::RestType { ty } | Node::NamedTupleMember { rest: true, ty, .. } => {
                    written_as_array(ast, ty, depth + 1)
                }
                _ => false,
            },
            _ => false,
        },
        _ => false,
    }
}
