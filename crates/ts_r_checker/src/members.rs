//! Members of object types (properties, call/construct signatures, index signatures),
//! apparent types, union/intersection properties and signatures from declarations.

use std::sync::LazyLock;

use rustc_hash::FxHashMap;
use ts_r_ast::{MethodKind, Modifiers, Node, NodeId, TypeOperatorKind};
use ts_r_binder::{Name, SymbolFacts, SymbolFlags, meaning};
use ts_r_types::{
    IndexInfo, ListId, MapperId, ObjectFlags, ObjectKind, Param, PredicateKind, ResolvedMembers,
    SigFlags, SigId, Signature, StrId, SymId, TupleElement, TypeFlags, TypeId, TypeKind,
    TypePredicate, TypeTraits,
};

use crate::checker::{Ck, GlobalType, Loc, Resolving};
use crate::declared::anon;
use crate::nodes::{fn_parts, is_this_param};
use crate::symbols::{SynthSym, check_flags};

static EMPTY_MEMBERS: LazyLock<ResolvedMembers> = LazyLock::new(ResolvedMembers::empty);

/// Nesting limit of `Ck::base_constraint` (distinct types, each constrained by the next).
const MAX_BASE_CONSTRAINT_DEPTH: usize = 100;

/// Call or construct signatures.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SigKind {
    Call,
    Construct,
}

/// One side of a union signature fold: fixed positions, a rest element, the minimum count.
struct UnionPositions {
    fixed: Vec<(StrId, TypeId)>,
    rest: Option<(StrId, TypeId)>,
    min: u32,
}

impl UnionPositions {
    fn count(&self) -> usize {
        self.fixed.len() + usize::from(self.rest.is_some())
    }

    fn type_at(&self, i: usize) -> Option<TypeId> {
        match self.fixed.get(i) {
            Some(&(_, t)) => Some(t),
            None => self.rest.map(|(_, t)| t),
        }
    }

    fn name_at(&self, i: usize) -> Option<StrId> {
        match self.fixed.get(i) {
            Some(&(n, _)) => Some(n),
            None if i < self.count() => self.rest.map(|(n, _)| n),
            None => None,
        }
    }
}

impl<'c, 'a> Ck<'c, 'a> {
    // ---------------------------------------------------------------------------------
    // Resolved members
    // ---------------------------------------------------------------------------------

    /// The members of an object type (empty for other types).
    pub fn resolved_members(&mut self, t: TypeId) -> &'c ResolvedMembers {
        let s = self.store();
        let Some(o) = s.object_of(t) else {
            return &EMPTY_MEMBERS;
        };
        if let Some(m) = o.members() {
            return m;
        }
        if !self.push_resolution(Resolving::Members(t)) {
            return &EMPTY_MEMBERS;
        }
        let m = match o.kind {
            ObjectKind::Declared {
                sym,
                type_params,
                this_type,
            } => {
                let mut src: Vec<TypeId> = s.list_items(type_params).to_vec();
                let mut dst = src.clone();
                src.push(this_type);
                dst.push(t);
                let m = s.mapper(&src, &dst);
                self.class_members_mapped(sym, m)
            }
            ObjectKind::Reference { target, args } => match s.object_of(target).map(|o| o.kind) {
                Some(ObjectKind::Declared {
                    sym,
                    type_params,
                    this_type,
                }) => {
                    let mut src: Vec<TypeId> = s.list_items(type_params).to_vec();
                    let mut dst: Vec<TypeId> = s.list_items(args).to_vec();
                    dst.resize(src.len(), TypeId::UNRESOLVED);
                    let this_arg = s.list_items(args).get(src.len()).copied().unwrap_or(t);
                    src.push(this_type);
                    dst.push(this_arg);
                    let m = s.mapper(&src, &dst);
                    self.class_members_mapped(sym, m)
                }
                Some(ObjectKind::TupleTarget { shape }) => self.tuple_members(t, shape, args),
                _ => ResolvedMembers::empty(),
            },
            ObjectKind::Anonymous { decl, kind, mapper } => {
                self.anonymous_members(t, decl, kind, mapper)
            }
            ObjectKind::Mapped { .. } => self.mapped_members(t),
            ObjectKind::TupleTarget { .. } | ObjectKind::Literal { .. } => ResolvedMembers::empty(),
        };
        self.pop_resolution();
        s.set_members(t, m)
    }

    /// Members of a class or interface symbol (own plus inherited), with `mapper` applied.
    fn class_members_mapped(&mut self, sym: SymId, mapper: MapperId) -> ResolvedMembers {
        let raw = self.raw_members(sym);
        self.map_members(raw, mapper)
    }

    /// Wraps the members of `raw` with `mapper`.
    pub fn map_members(&mut self, raw: &ResolvedMembers, mapper: MapperId) -> ResolvedMembers {
        if mapper == MapperId::IDENTITY {
            return ResolvedMembers::new(
                raw.names
                    .iter()
                    .copied()
                    .zip(raw.props.iter().copied())
                    .collect(),
                raw.call.to_vec(),
                raw.construct.to_vec(),
                raw.index.to_vec(),
            );
        }
        let mut props = Vec::with_capacity(raw.props.len());
        for (&n, &p) in raw.names.iter().zip(raw.props.iter()) {
            props.push((n, self.instantiate_symbol(p, mapper)));
        }
        let call = raw
            .call
            .iter()
            .map(|&s| self.instantiate_signature(s, mapper))
            .collect();
        let construct = raw
            .construct
            .iter()
            .map(|&s| self.instantiate_signature(s, mapper))
            .collect();
        let index = raw
            .index
            .iter()
            .map(|i| IndexInfo {
                key: i.key,
                value: self.instantiate(i.value, mapper),
                readonly: i.readonly,
                decl: i.decl,
            })
            .collect();
        ResolvedMembers::new(props, call, construct, index)
    }

    /// A property symbol instantiated by `mapper` (a synthetic symbol with the mapper).
    pub fn instantiate_symbol(&mut self, p: SymId, mapper: MapperId) -> SymId {
        if mapper == MapperId::IDENTITY {
            return p;
        }
        let key = (p, mapper);
        if let Some(&s) = self.inst_syms.get(&key) {
            return s;
        }
        let (origin, m) = match self.c.synth_sym(p) {
            // Compose with an existing instantiation.
            Some(x)
                if x.mapper != MapperId::IDENTITY
                    && !mapper.is_special()
                    && x.origins.len() == 1
                    && x.known_type().is_none() =>
            {
                let composed = self.compose_mappers(x.mapper, mapper);
                (x.origins[0], composed)
            }
            _ => (p, mapper),
        };
        let name = self.c.sym_name_id(p);
        let flags = self.c.sym_flags(p);
        let check = self.c.sym_check_flags(p) | check_flags::INSTANTIATED;
        let parent = self.c.synth_sym(p).and_then(|x| x.parent_type);
        let s = self.c.new_synth(
            SynthSym::new(name, flags, check)
                .with_origins(vec![origin])
                .with_mapper(m)
                .with_parent(parent),
        );
        self.inst_syms.insert(key, s);
        s
    }

    /// Own and inherited members of a class or interface, uninstantiated.
    pub fn raw_members(&mut self, sym: SymId) -> &'c ResolvedMembers {
        let c = self.c;
        if let Some(l) = c.sym_links(sym)
            && let Some(m) = l.raw_members.get()
        {
            return m;
        }
        let m = self.compute_raw_members(sym);
        match c.sym_links(sym) {
            Some(l) => {
                let _ = l.raw_members.set(Box::new(m));
                l.raw_members.get().map(|b| &**b).unwrap_or(&EMPTY_MEMBERS)
            }
            None => &EMPTY_MEMBERS,
        }
    }

    fn compute_raw_members(&mut self, sym: SymId) -> ResolvedMembers {
        let tables = self.c.member_tables(sym);
        let mut own: Vec<(Loc, String, SymId)> = Vec::new();
        let mut calls: Vec<Loc> = Vec::new();
        let mut news: Vec<Loc> = Vec::new();
        let mut indexes: Vec<Loc> = Vec::new();
        for (file, table) in tables {
            let f = self.c.file(file);
            for &(name, s) in f.bound.table(table) {
                let decls = f.bound.decls(s);
                if name == Name::CALL {
                    calls.extend(decls.iter().map(|&d| Loc::new(file, d)));
                    continue;
                }
                if name == Name::NEW {
                    news.extend(decls.iter().map(|&d| Loc::new(file, d)));
                    continue;
                }
                if name == Name::INDEX {
                    indexes.extend(decls.iter().map(|&d| Loc::new(file, d)));
                    continue;
                }
                if name == Name::CONSTRUCTOR || name == Name::COMPUTED || name == Name::MISSING {
                    continue;
                }
                let first = decls.first().copied().unwrap_or(f.ast.root);
                own.push((
                    Loc::new(file, first),
                    f.bound.name_text(&f.ast, name).to_owned(),
                    self.c.sym(file, s),
                ));
            }
        }
        // Late-bound (computed) members of each declaration.
        for d in self.c.decls(sym) {
            self.collect_computed_members(d, false, &mut own);
        }
        own.sort_by_key(|e| e.0);
        let mut props: Vec<(StrId, SymId)> = Vec::new();
        let mut index_of: FxHashMap<StrId, usize> = FxHashMap::default();
        // Overloads of same-named methods, grouped by the declaration they are in.
        type Groups = Vec<(Option<Loc>, Vec<SymId>)>;
        let mut merged: FxHashMap<usize, Groups> = FxHashMap::default();
        let mut first_block: FxHashMap<usize, Option<Loc>> = FxHashMap::default();
        for (at, name, s) in own {
            let n = self.store().intern_str(&name);
            let block = self.c.parent(at);
            match index_of.get(&n) {
                Some(&i) => {
                    // Same-named members of several interface declarations: methods merge
                    // their overloads (later declarations first, each declaration's in
                    // order).
                    if self.c.sym_flags(s).intersects(SymbolFlags::METHOD) {
                        let first = first_block.get(&i).copied().flatten();
                        let groups = merged
                            .entry(i)
                            .or_insert_with(|| vec![(first, vec![props[i].1])]);
                        match groups.last_mut() {
                            Some((b, list)) if *b == block && block.is_some() => list.push(s),
                            _ => groups.push((block, vec![s])),
                        }
                    }
                }
                None => {
                    index_of.insert(n, props.len());
                    first_block.insert(props.len(), block);
                    props.push((n, s));
                }
            }
        }
        let mut merged_keys: Vec<usize> = merged.keys().copied().collect();
        merged_keys.sort_unstable();
        for i in merged_keys {
            let groups = merged.remove(&i).unwrap_or_default();
            let origins: Vec<SymId> = groups.into_iter().rev().flat_map(|g| g.1).collect();
            let flags = self.c.sym_flags(props[i].1);
            let synth =
                SynthSym::new(props[i].0, flags, check_flags::MERGED_MEMBER).with_origins(origins);
            props[i].1 = self.c.new_synth(synth);
        }
        let calls = self.later_interfaces_first(calls);
        let news = self.later_interfaces_first(news);
        let mut call: Vec<SigId> = calls.iter().map(|&l| self.signature_from_decl(l)).collect();
        let mut construct: Vec<SigId> = news.iter().map(|&l| self.signature_from_decl(l)).collect();
        let mut index: Vec<IndexInfo> = Vec::new();
        for l in indexes {
            index.extend(self.index_infos_from_decl(l));
        }
        for d in self.c.decls(sym) {
            self.computed_index_infos(d, false, &mut index);
        }
        // Inherited members.
        let bases = self.base_types(sym);
        for b in bases.iter() {
            if self.store().flags(*b).contains(TypeFlags::INTERSECTION) {
                // `interface I extends A & B`: the intersection's properties and signatures.
                for (n, p) in self.properties_of(*b) {
                    if let std::collections::hash_map::Entry::Vacant(e) = index_of.entry(n) {
                        e.insert(props.len());
                        props.push((n, p));
                    }
                }
                if self.c.sym_flags(sym).intersects(SymbolFlags::INTERFACE) {
                    call.extend(self.signatures_of(*b, SigKind::Call));
                    construct.extend(self.signatures_of(*b, SigKind::Construct));
                }
                for key in [TypeId::STRING, TypeId::NUMBER, TypeId::SYMBOL] {
                    if !index.iter().any(|x| x.key == key)
                        && let Some(i) = self.index_info_of(*b, key)
                        && i.key == key
                    {
                        index.push(i);
                    }
                }
                continue;
            }
            let bm = self.resolved_members(*b);
            for (&n, &p) in bm.names.iter().zip(bm.props.iter()) {
                if let std::collections::hash_map::Entry::Vacant(e) = index_of.entry(n) {
                    e.insert(props.len());
                    props.push((n, p));
                }
            }
            if self.c.sym_flags(sym).intersects(SymbolFlags::INTERFACE) {
                call.extend(bm.call.iter().copied());
                construct.extend(bm.construct.iter().copied());
            }
            for i in bm.index.iter() {
                if !index.iter().any(|x| x.key == i.key) {
                    index.push(*i);
                }
            }
        }
        // A class extending an `any` value inherits `[x: string]: any`.
        if bases.is_empty()
            && !index.iter().any(|x| x.key == TypeId::STRING)
            && self.c.sym_flags(sym).intersects(SymbolFlags::CLASS)
            && self.extends_any(sym)
        {
            index.push(IndexInfo {
                key: TypeId::STRING,
                value: TypeId::ANY,
                readonly: false,
                decl: None,
            });
        }
        ResolvedMembers::new(props, call, construct, index)
    }

    /// Whether a class's `extends` expression has type `any` (`class extends anyValue`).
    fn extends_any(&mut self, sym: SymId) -> bool {
        let Some(d) = self.c.decls(sym).into_iter().find(|d| {
            matches!(
                self.node(*d),
                Node::ClassDecl { .. } | Node::ClassExpr { .. }
            )
        }) else {
            return false;
        };
        let ast = self.ast(d.file);
        let (Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. }) =
            *ast.node(d.node)
        else {
            return false;
        };
        for &h in ast.list(heritage) {
            if let Node::HeritageClause {
                kind: ts_r_ast::HeritageKind::Extends,
                types,
            } = *ast.node(h)
                && let Some(&first) = ast.list(types).first()
                && let Node::ExprWithTypeArgs { expr, .. } = *ast.node(first)
            {
                if !self.push_resolution(Resolving::BaseTypes(sym)) {
                    return false;
                }
                let t = self.quietly(|ck| ck.check_expr(Loc::new(d.file, expr)));
                self.pop_resolution();
                return t == TypeId::ANY;
            }
        }
        false
    }

    /// Index signatures implied by members whose computed key has a type that is not a
    /// literal or unique symbol (`[s]: number` with `s: string` gives `[x: string]: number`):
    /// a `string` index (when some key is string-like) over the string- and number-keyed
    /// ones, a `number` index over the number-keyed ones and a `symbol` index over the
    /// symbol-keyed ones. Each also covers the named members it applies to. Keys of type `any`
    /// count as number keys only (`a["k"]` is still TS7015). Only entity-name keys count
    /// (`[k]`, `[a.b]`, not `[s + s]` or `[+s]`). An index is readonly when all its members
    /// are. Each info's `decl` is its first member (not an `IndexSignature`). Explicit index
    /// signatures win.
    fn computed_index_infos(&mut self, d: Loc, statics: bool, out: &mut Vec<IndexInfo>) {
        let ast = self.ast(d.file);
        // A generic class or interface's implied instance index is `any` (observed:
        // `class C<T> { [s]() {} }` reads `c["x"]` as `any`; a type literal's is the union).
        let (members, generic) = match *ast.node(d.node) {
            Node::ClassDecl {
                members,
                type_params,
                ..
            }
            | Node::ClassExpr {
                members,
                type_params,
                ..
            } => (members, type_params.is_some() && !statics),
            Node::Interface {
                body, type_params, ..
            } => (body, type_params.is_some()),
            Node::TypeLiteral { members } => (members, false),
            _ => return,
        };
        // Per key kind (string, number, symbol): values, all readonly, first member.
        let mut acc: [(Vec<TypeId>, bool, Option<NodeId>); 3] =
            std::array::from_fn(|_| (Vec::new(), true, None));
        for &m in ast.list(members) {
            let (key, is_static, readonly) = match *ast.node(m) {
                Node::PropertyDef { key, modifiers, .. } => (
                    key,
                    modifiers.contains(Modifiers::STATIC),
                    modifiers.contains(Modifiers::READONLY),
                ),
                Node::MethodDef {
                    key,
                    modifiers,
                    kind: MethodKind::Method | MethodKind::Get | MethodKind::Set,
                    ..
                } => (key, modifiers.contains(Modifiers::STATIC), false),
                Node::PropertySignature { key, modifiers, .. } => {
                    (key, false, modifiers.contains(Modifiers::READONLY))
                }
                Node::MethodSignature { key, .. } => (key, false, false),
                _ => continue,
            };
            if is_static != statics {
                continue;
            }
            let Node::ComputedKey { expr } = *ast.node(key) else {
                continue;
            };
            // Only entity names (`[k]`, `[a.b]`) imply indexes; `[s + s]` and `[+s]` don't.
            if crate::nodes::is_literal_computed_key(ast, key)
                || !crate::nodes::is_entity_name_expr(ast, expr)
            {
                continue;
            }
            let kt = self.quietly(|ck| ck.check_expr(Loc::new(d.file, expr)));
            if self.property_name_of_type(kt).is_some() {
                continue;
            }
            let mut kinds = [false; 3];
            // An `any` key (also an unresolved import's error type) implies a `number` index.
            if kt != TypeId::UNRESOLVED && self.store().flags(kt).contains(TypeFlags::ANY) {
                kinds[1] = true;
            }
            for k in self.union_members(kt) {
                // A branded key (`'k' & { __brand: 'k' }`) counts as its primitive parts.
                let mut f = self.store().flags(k);
                if f.contains(TypeFlags::INTERSECTION) {
                    for p in self.store().members_of_union(k).to_vec() {
                        f |= self.store().flags(p);
                    }
                }
                if f.intersects(
                    TypeFlags::STRING
                        | TypeFlags::STRING_LITERAL
                        | TypeFlags::TEMPLATE_LITERAL
                        | TypeFlags::STRING_MAPPING,
                ) {
                    kinds[0] = true;
                } else if f.intersects(TypeFlags::NUMBER | TypeFlags::NUMBER_LITERAL) {
                    kinds[1] = true;
                } else if f.intersects(TypeFlags::ES_SYMBOL | TypeFlags::UNIQUE_ES_SYMBOL) {
                    kinds[2] = true;
                }
            }
            if !kinds.iter().any(|&k| k) {
                continue;
            }
            let Some(s) = self.c.sym_of_decl(Loc::new(d.file, m)) else {
                continue;
            };
            let v = self.type_of_symbol(s);
            // A number key implies only a `number` index but also counts towards `string`.
            let contributes = [kinds[0] || kinds[1], kinds[1], kinds[2]];
            for i in 0..3 {
                if contributes[i] {
                    acc[i].0.push(v);
                    acc[i].1 &= readonly;
                }
                if kinds[i] {
                    acc[i].2.get_or_insert(m);
                }
            }
        }
        // An implied index also covers the named members it applies to (`x` and `[k]` in
        // `{ [s]: number; x: string }` give `[x: string]: string | number`).
        if acc.iter().any(|a| a.2.is_some()) {
            for &m in ast.list(members) {
                let (key, is_static) = match *ast.node(m) {
                    Node::PropertyDef { key, modifiers, .. }
                    | Node::MethodDef { key, modifiers, .. } => {
                        (key, modifiers.contains(Modifiers::STATIC))
                    }
                    Node::PropertySignature { key, .. } | Node::MethodSignature { key, .. } => {
                        (key, false)
                    }
                    _ => continue,
                };
                if is_static != statics
                    || matches!(
                        *ast.node(m),
                        Node::MethodDef {
                            kind: MethodKind::Constructor,
                            ..
                        }
                    )
                    || matches!(ast.node(key), Node::PrivateName { .. })
                {
                    continue;
                }
                let name = match crate::nodes::key_text(ast, key) {
                    Some(n) => n,
                    None => match *ast.node(key) {
                        Node::ComputedKey { expr } => {
                            match self.late_bound_name(Loc::new(d.file, expr)) {
                                Some(n) => n,
                                None => continue,
                            }
                        }
                        _ => continue,
                    },
                };
                let symbol_keyed = name.starts_with("__@")
                    && self
                        .store()
                        .flags(self.property_key_type(&name))
                        .intersects(TypeFlags::UNIQUE_ES_SYMBOL);
                let applies = if symbol_keyed {
                    [false, false, true]
                } else {
                    [true, crate::expr::is_numeric_name(&name), false]
                };
                if !(0..3).any(|i| applies[i] && acc[i].2.is_some()) {
                    continue;
                }
                let Some(s) = self.c.sym_of_decl(Loc::new(d.file, m)) else {
                    continue;
                };
                let v = self.type_of_symbol(s);
                for i in 0..3 {
                    if applies[i] && acc[i].2.is_some() {
                        acc[i].0.push(v);
                    }
                }
            }
        }
        for (i, key) in [TypeId::STRING, TypeId::NUMBER, TypeId::SYMBOL]
            .into_iter()
            .enumerate()
        {
            let (vals, readonly, first) = std::mem::take(&mut acc[i]);
            let Some(first) = first else {
                continue;
            };
            if out.iter().any(|x| x.key == key) {
                continue;
            }
            let value = if generic {
                TypeId::ANY
            } else {
                self.union(&vals)
            };
            out.push(IndexInfo {
                key,
                value,
                readonly,
                decl: Some(Loc::new(d.file, first).pack()),
            });
        }
    }

    /// Members with computed names whose type is a literal or unique symbol.
    fn collect_computed_members(
        &mut self,
        d: Loc,
        statics: bool,
        out: &mut Vec<(Loc, String, SymId)>,
    ) {
        let ast = self.ast(d.file);
        let members = match *ast.node(d.node) {
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
            Node::Interface { body, .. } => body,
            Node::TypeLiteral { members } => members,
            _ => return,
        };
        for &m in ast.list(members) {
            let (key, is_static) = match *ast.node(m) {
                Node::PropertyDef { key, modifiers, .. }
                | Node::MethodDef { key, modifiers, .. } => {
                    (key, modifiers.contains(Modifiers::STATIC))
                }
                Node::PropertySignature { key, .. } | Node::MethodSignature { key, .. } => {
                    (key, false)
                }
                _ => continue,
            };
            if is_static != statics {
                continue;
            }
            let Node::ComputedKey { expr } = *ast.node(key) else {
                continue;
            };
            // Only entity names late-bind (`[f()]` and `[(k)]` do not name a member).
            if crate::nodes::key_text(ast, key).is_some()
                || !crate::nodes::is_entity_name_expr(ast, expr)
            {
                continue;
            }
            let Some(name) = self.late_bound_name(Loc::new(d.file, expr)) else {
                continue;
            };
            let Some(s) = self.c.sym_of_decl(Loc::new(d.file, m)) else {
                continue;
            };
            out.push((Loc::new(d.file, m), name, s));
        }
    }

    /// The property name a computed key denotes, when its type is a literal or unique symbol.
    pub fn late_bound_name(&mut self, e: Loc) -> Option<String> {
        let t = self.quietly(|ck| ck.check_expr(e));
        self.property_name_of_type(t)
    }

    /// The property name for a literal or unique symbol type.
    /// The key type of a property name: the unique symbol of a symbol-keyed name
    /// (`__@A@12`), else the string literal.
    pub fn property_key_type(&self, name: &str) -> TypeId {
        let sym = name
            .strip_prefix("__@")
            .and_then(|r| r.rsplit_once('@'))
            .and_then(|(_, id)| id.parse::<u32>().ok())
            .map(SymId)
            .filter(|&s| self.c.sym_exists(s));
        match sym {
            Some(s) => self.store().unique_symbol(s),
            None => self.store().string_literal(name, false),
        }
    }

    pub fn property_name_of_type(&self, t: TypeId) -> Option<String> {
        let s = self.store();
        match s.kind(t) {
            TypeKind::StringLiteral(x) => Some(s.str(x).to_owned()),
            TypeKind::NumberLiteral(..) => s.number_value(t).map(ts_r_binder::js_number_to_string),
            TypeKind::EnumLiteral { value, .. } => self.property_name_of_type(value),
            TypeKind::UniqueSymbol(sym) => Some(format!("__@{}@{}", self.c.sym_name(sym), sym.0)),
            _ => None,
        }
    }

    /// Base types of a class or interface (instantiated, with `this` bound to the symbol's
    /// own `this` type).
    pub fn base_types(&mut self, sym: SymId) -> Box<[TypeId]> {
        if let Some(l) = self.c.sym_links(sym)
            && let Some(b) = l.bases.get()
        {
            return b.clone();
        }
        if !self.push_resolution(Resolving::BaseTypes(sym)) {
            return Box::new([]);
        }
        let mut out: Vec<TypeId> = Vec::new();
        let this_type = self.store().type_param(sym, true);
        for d in self.c.decls(sym) {
            let ast = self.ast(d.file);
            match *ast.node(d.node) {
                Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. } => {
                    for &h in ast.list(heritage) {
                        if let Node::HeritageClause {
                            kind: ts_r_ast::HeritageKind::Extends,
                            types,
                        } = *ast.node(h)
                            && let Some(&first) = ast.list(types).first()
                            && let Some(b) =
                                self.class_base_type(Loc::new(d.file, first), this_type)
                        {
                            out.push(b);
                        }
                    }
                }
                Node::Interface { extends, .. } => {
                    for &e in ast.list(extends) {
                        let t = self.type_from_node(Loc::new(d.file, e));
                        let t = self.with_this_arg(t, this_type);
                        if self.store().flags(t).contains(TypeFlags::OBJECT)
                            || self.store().flags(t).contains(TypeFlags::INTERSECTION)
                        {
                            out.push(t);
                        }
                    }
                }
                _ => {}
            }
        }
        let ok = self.pop_resolution();
        if !ok {
            out.clear();
        }
        let b: Box<[TypeId]> = out.into_boxed_slice();
        if ok && let Some(l) = self.c.sym_links(sym) {
            let _ = l.bases.set(b.clone());
        }
        b
    }

    /// `extends Expr<Args>` of a class: the instance type of the base constructor.
    fn class_base_type(&mut self, h: Loc, this_type: TypeId) -> Option<TypeId> {
        let ast = self.ast(h.file);
        let Node::ExprWithTypeArgs { expr, type_args } = *ast.node(h.node) else {
            return None;
        };
        let args: Vec<TypeId> = match type_args.map(|t| *ast.node(t)) {
            Some(Node::TypeArgs { args }) => ast
                .list(args)
                .iter()
                .map(|&a| self.type_from_node(Loc::new(h.file, a)))
                .collect(),
            _ => Vec::new(),
        };
        let js = self.c.file(h.file).is_js();
        // A class name: its instance type (none when the type arguments don't fit: TS2314).
        if let Some(s) = self.resolve_entity(Loc::new(h.file, expr), meaning::VALUE)
            && let Some(s) = self.c.resolve_alias(s)
            && self.c.sym_flags(s).intersects(SymbolFlags::CLASS)
        {
            let tps = self.local_type_params(s);
            if !js && !self.type_arg_count_fits(&tps, args.len()) {
                return None;
            }
            // In JavaScript, defaults of `unknown` or `{}` fill in as `any`.
            let args = if js && args.len() < tps.len() {
                self.fill_type_args_in(&tps, &args, true)
            } else {
                args
            };
            let t = self.type_from_symbol_reference(s, &args);
            return Some(self.with_this_arg(t, this_type));
        }
        // Any expression with construct signatures (mixins, `class extends f()`).
        let ct = self.quietly(|ck| ck.check_expr(Loc::new(h.file, expr)));
        if self.store().flags(ct).intersects(TypeFlags::ANY) {
            return None;
        }
        // The first construct signature taking that many type arguments (none: TS2508).
        let sigs = self.signatures_of(ct, SigKind::Construct);
        let sig = sigs.iter().copied().find(|&s| {
            let tps = self
                .store()
                .list_items(self.store().sig(s).type_params)
                .to_vec();
            if js {
                tps.is_empty() || !args.is_empty()
            } else {
                self.type_arg_count_fits(&tps, args.len())
            }
        })?;
        let sig = if self.store().sig(sig).type_params == ListId::EMPTY {
            sig
        } else {
            let tps = self
                .store()
                .list_items(self.store().sig(sig).type_params)
                .to_vec();
            let filled = self.fill_type_args_in(&tps, &args, js);
            let m = self.store().mapper(&tps, &filled);
            self.instantiate_signature_full(sig, m)
        };
        let r = self.return_type_of(sig);
        if !self
            .store()
            .flags(r)
            .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
        {
            return None;
        }
        Some(self.with_this_arg(r, this_type))
    }

    /// Whether `n` type arguments fit these type parameters (trailing defaults optional).
    pub(crate) fn type_arg_count_fits(&mut self, tps: &[TypeId], n: usize) -> bool {
        let mut min = tps.len();
        while min > 0 && self.has_type_param_default(tps[min - 1]) {
            min -= 1;
        }
        n >= min && n <= tps.len()
    }

    /// A class/interface reference with an explicit `this` argument.
    pub fn with_this_arg(&mut self, t: TypeId, this_arg: TypeId) -> TypeId {
        let s = self.store();
        match s.object_of(t).map(|o| o.kind) {
            Some(ObjectKind::Declared { type_params, .. }) => {
                let mut args = s.list_items(type_params).to_vec();
                args.push(this_arg);
                s.reference(t, &args)
            }
            Some(ObjectKind::Reference { target, args }) => {
                if let Some(ObjectKind::Declared { type_params, .. }) =
                    s.object_of(target).map(|o| o.kind)
                {
                    let n = s.list_items(type_params).len();
                    let mut a = s.list_items(args).to_vec();
                    a.truncate(n);
                    a.resize(n, TypeId::UNRESOLVED);
                    a.push(this_arg);
                    s.reference(target, &a)
                } else {
                    // A tuple: the plain reference (without an alias that names it).
                    self.without_reference_alias(t)
                }
            }
            _ => {
                if s.flags(t).contains(TypeFlags::INTERSECTION) {
                    let parts: Vec<TypeId> = s.members_of_union(t).to_vec();
                    let mapped: Vec<TypeId> = parts
                        .iter()
                        .map(|&p| self.with_this_arg(p, this_arg))
                        .collect();
                    return self.intersection(&mapped);
                }
                t
            }
        }
    }

    /// The class/interface reference without an explicit `this` argument.
    pub fn without_this_arg(&mut self, t: TypeId) -> TypeId {
        let s = self.store();
        if let Some(ObjectKind::Reference { target, args }) = s.object_of(t).map(|o| o.kind)
            && let Some(ObjectKind::Declared { type_params, .. }) =
                s.object_of(target).map(|o| o.kind)
        {
            let n = s.list_items(type_params).len();
            if s.list_items(args).len() > n {
                let a = s.list_items(args)[..n].to_vec();
                return self.make_reference(target, &a);
            }
        }
        t
    }

    fn tuple_members(&mut self, t: TypeId, shape: u32, args: ListId) -> ResolvedMembers {
        let s = self.store();
        let shape = s.tuple_shape(shape).clone();
        let args = s.list_items(args).to_vec();
        let mut props: Vec<(StrId, SymId)> = Vec::new();
        let mut elem_types: Vec<TypeId> = Vec::new();
        let ro = if shape.readonly {
            check_flags::READONLY
        } else {
            0
        };
        // Only the elements before a rest element are properties: the position of one
        // after it varies (`[...number[], number]` has no `"1"`).
        let mut past_rest = false;
        for (i, el) in shape.elements.iter().enumerate() {
            let ty = args.get(i).copied().unwrap_or(TypeId::UNRESOLVED);
            match el.kind {
                TupleElement::REQUIRED if past_rest => {
                    elem_types.push(ty);
                }
                TupleElement::OPTIONAL if past_rest => {
                    elem_types.push(self.add_optionality(ty));
                }
                TupleElement::REQUIRED | TupleElement::OPTIONAL => {
                    let opt = el.kind == TupleElement::OPTIONAL;
                    let pt = if opt { self.add_optionality(ty) } else { ty };
                    let name = s.intern_str(&i.to_string());
                    let mut flags = SymbolFlags::PROPERTY;
                    if opt {
                        flags |= SymbolFlags::OPTIONAL;
                    }
                    let p = self.c.new_synth(
                        SynthSym::new(name, flags, check_flags::TUPLE | ro)
                            .with_type(pt)
                            .with_parent(Some(t)),
                    );
                    props.push((name, p));
                    // The array base of `[number, string?]` is `(string | number |
                    // undefined)[]`, like its element type (tsc getTupleBaseType).
                    elem_types.push(pt);
                }
                _ => {
                    past_rest = true;
                    if el.kind == TupleElement::VARIADIC {
                        let et = self
                            .element_type_of_array_like(ty)
                            .unwrap_or(TypeId::UNRESOLVED);
                        elem_types.push(et);
                    } else {
                        elem_types.push(ty);
                    }
                }
            }
        }
        let len_ty = if shape.has_rest() {
            TypeId::NUMBER
        } else {
            let min = shape.min_length();
            let max = shape.elements.len();
            let lits: Vec<TypeId> = (min..=max)
                .map(|n| s.number_literal(n as f64, false))
                .collect();
            self.union(&lits)
        };
        let len_name = s.intern_str("length");
        let lp = self.c.new_synth(
            SynthSym::new(
                len_name,
                SymbolFlags::PROPERTY,
                check_flags::TUPLE | check_flags::READONLY,
            )
            .with_type(len_ty)
            .with_parent(Some(t)),
        );
        props.push((len_name, lp));
        let elem = self.union(&elem_types);
        // The array members see the tuple as `this` (`sort(): this` returns the tuple).
        let arr = self.create_array(elem, shape.readonly);
        let arr = self.with_this_arg(arr, t);
        let am = self.resolved_members(arr);
        let mut have: rustc_hash::FxHashSet<StrId> = props.iter().map(|p| p.0).collect();
        for (&n, &p) in am.names.iter().zip(am.props.iter()) {
            if have.insert(n) {
                props.push((n, p));
            }
        }
        ResolvedMembers::new(
            props,
            am.call.to_vec(),
            am.construct.to_vec(),
            am.index.to_vec(),
        )
    }

    fn anonymous_members(
        &mut self,
        t: TypeId,
        decl: (u32, u32),
        kind: u8,
        mapper: MapperId,
    ) -> ResolvedMembers {
        let raw = if decl.0 == u32::MAX {
            let sym = SymId(decl.1);
            match kind {
                anon::SYMBOL_FUNCTION => self.function_symbol_members(sym),
                anon::CLASS_STATIC => self.class_static_members(sym),
                anon::NAMESPACE => self.namespace_members(sym, t),
                _ => ResolvedMembers::empty(),
            }
        } else {
            let Some(l) = Loc::unpack(decl) else {
                return ResolvedMembers::empty();
            };
            match kind {
                anon::TYPE_LITERAL => self.type_literal_members(l),
                anon::FUNCTION_TYPE | anon::FUNCTION_EXPR => {
                    let sig = self.signature_from_decl(l);
                    if self.store().sig(sig).flags.contains(SigFlags::CONSTRUCT) {
                        ResolvedMembers::new(Vec::new(), Vec::new(), vec![sig], Vec::new())
                    } else {
                        ResolvedMembers::new(Vec::new(), vec![sig], Vec::new(), Vec::new())
                    }
                }
                _ => ResolvedMembers::empty(),
            }
        };
        if mapper == MapperId::IDENTITY {
            return raw;
        }
        self.map_members(&raw, mapper)
    }

    fn type_literal_members(&mut self, l: Loc) -> ResolvedMembers {
        let Some(sym) = self.c.sym_of_decl(l) else {
            return ResolvedMembers::empty();
        };
        let f = self.c.file(l.file);
        if self.c.binder_name(sym).is_none() {
            return ResolvedMembers::empty();
        }
        let mut own: Vec<(Loc, String, SymId)> = Vec::new();
        let mut calls = Vec::new();
        let mut news = Vec::new();
        let mut indexes = Vec::new();
        for (file, table) in self.c.member_tables(sym) {
            let ff = self.c.file(file);
            for &(name, s) in ff.bound.table(table) {
                let decls = ff.bound.decls(s);
                match name {
                    Name::CALL => calls.extend(decls.iter().map(|&d| Loc::new(file, d))),
                    Name::NEW => news.extend(decls.iter().map(|&d| Loc::new(file, d))),
                    Name::INDEX => indexes.extend(decls.iter().map(|&d| Loc::new(file, d))),
                    Name::COMPUTED | Name::MISSING | Name::CONSTRUCTOR => {}
                    _ => {
                        let first = decls.first().copied().unwrap_or(f.ast.root);
                        own.push((
                            Loc::new(file, first),
                            ff.bound.name_text(&ff.ast, name).to_owned(),
                            self.c.sym(file, s),
                        ));
                    }
                }
            }
        }
        self.collect_computed_members(l, false, &mut own);
        own.sort_by_key(|e| e.0);
        calls.sort();
        news.sort();
        indexes.sort();
        let props: Vec<(StrId, SymId)> = own
            .into_iter()
            .map(|(_, n, s)| (self.store().intern_str(&n), s))
            .collect();
        let call = calls
            .into_iter()
            .map(|l| self.signature_from_decl(l))
            .collect();
        let construct = news
            .into_iter()
            .map(|l| self.signature_from_decl(l))
            .collect();
        let mut index = Vec::new();
        for l in indexes {
            index.extend(self.index_infos_from_decl(l));
        }
        self.computed_index_infos(l, false, &mut index);
        ResolvedMembers::new(props, call, construct, index)
    }

    /// `[k: K]: V` → one index info per key type.
    fn index_infos_from_decl(&mut self, l: Loc) -> Vec<IndexInfo> {
        let ast = self.ast(l.file);
        let Node::IndexSignature {
            modifiers,
            params,
            type_ann,
        } = *ast.node(l.node)
        else {
            return Vec::new();
        };
        let value = match type_ann {
            Some(t) => self.type_from_node(Loc::new(l.file, t)),
            None => TypeId::ANY,
        };
        let Some(&p) = ast.list(params).first() else {
            return Vec::new();
        };
        let Node::Param {
            type_ann: Some(kt), ..
        } = *ast.node(p)
        else {
            return Vec::new();
        };
        let key = self.type_from_node(Loc::new(l.file, kt));
        let mut out = Vec::new();
        for k in self.union_members(key) {
            let f = self.store().flags(k);
            let k = if f.intersects(TypeFlags::STRING) {
                TypeId::STRING
            } else if f.intersects(TypeFlags::NUMBER) {
                TypeId::NUMBER
            } else if f.intersects(TypeFlags::ES_SYMBOL) {
                TypeId::SYMBOL
            } else if f.intersects(TypeFlags::TEMPLATE_LITERAL | TypeFlags::STRING_MAPPING) {
                k
            } else {
                continue;
            };
            out.push(IndexInfo {
                key: k,
                value,
                readonly: modifiers.contains(Modifiers::READONLY),
                decl: Some(l.pack()),
            });
        }
        out
    }

    /// The function-like declarations that provide a function symbol's signatures.
    pub fn signature_decls(&mut self, sym: SymId) -> Vec<Loc> {
        let decls = self.c.decls(sym);
        let mut fns: Vec<Loc> = decls
            .into_iter()
            .filter(|d| {
                let ast = self.ast(d.file);
                match *ast.node(d.node) {
                    Node::FunctionDecl { .. }
                    | Node::FunctionExpr { .. }
                    | Node::Arrow { .. }
                    | Node::MethodSignature { .. } => true,
                    Node::MethodDef { kind, .. } => {
                        kind == MethodKind::Method || kind == MethodKind::Constructor
                    }
                    _ => false,
                }
            })
            .collect();
        // With overloads, the implementation signature is not visible.
        let has_body =
            |ck: &Self, d: &Loc| fn_parts(ck.ast(d.file), d.node).is_some_and(|p| p.body.is_some());
        if fns.len() > 1 && fns.iter().any(|d| !has_body(self, d)) {
            fns.retain(|d| !has_body(self, d));
        } else if fns.len() > 1
            && fns.iter().all(|d| {
                matches!(
                    self.node(*d),
                    Node::FunctionDecl { .. } | Node::MethodDef { .. }
                )
            })
        {
            // Duplicate implementations (TS2393): only the first one is the signature.
            fns.truncate(1);
        }
        fns
    }

    /// Signature declarations merged from several interface declarations: tsc lists the
    /// later declarations' signatures first (each declaration keeps its own order).
    fn later_interfaces_first(&self, decls: Vec<Loc>) -> Vec<Loc> {
        if decls.len() < 2 {
            return decls;
        }
        let mut groups: Vec<(Loc, Vec<Loc>)> = Vec::new();
        for d in decls.iter().copied() {
            let Some(p) = self.c.parent(d) else {
                return decls;
            };
            if !matches!(self.node(p), Node::Interface { .. }) {
                return decls;
            }
            match groups.iter_mut().find(|g| g.0 == p) {
                Some(g) => g.1.push(d),
                None => groups.push((p, vec![d])),
            }
        }
        if groups.len() < 2 {
            return decls;
        }
        groups.sort_by_key(|g| std::cmp::Reverse(g.0));
        groups.into_iter().flat_map(|g| g.1).collect()
    }

    fn function_symbol_members(&mut self, sym: SymId) -> ResolvedMembers {
        let mut decls: Vec<Loc> = Vec::new();
        match self.c.synth_sym(sym) {
            Some(x) => {
                for o in x.origins.clone() {
                    decls.extend(self.signature_decls(o));
                }
            }
            None => decls = self.signature_decls(sym),
        }
        let decls = self.later_interfaces_first(decls);
        let call: Vec<SigId> = decls
            .into_iter()
            .map(|l| self.signature_from_decl(l))
            .collect();
        let props = if self.c.sym_flags(sym).intersects(SymbolFlags::NAMESPACE) {
            self.value_exports(sym)
        } else {
            self.expando_props(sym)
        };
        ResolvedMembers::new(props, call, Vec::new(), Vec::new())
    }

    /// Properties assigned to a function or a variable holding one (`f.x = 1`), in
    /// declaration order.
    pub fn expando_props(&mut self, sym: SymId) -> Vec<(StrId, SymId)> {
        let mut out = Vec::new();
        for (name, s) in self.c.exports_of(sym) {
            if self.c.sym_facts(s).intersects(SymbolFacts::EXPANDO) {
                out.push((self.store().intern_str(&name), s));
            }
        }
        self.late_expando_props(sym, &mut out);
        out
    }

    /// Late-bound expando properties (`f[k] = v`, `k` a unique symbol or a literal): named
    /// by the key's type, typed by the union of the widened values. Keys of other types
    /// declare nothing.
    fn late_expando_props(&mut self, sym: SymId, out: &mut Vec<(StrId, SymId)>) {
        let mut lefts: Vec<Loc> = Vec::new();
        for r in self.c.parts(sym) {
            let late = &self.c.bound(r.file).late_expandos;
            let lo = late.partition_point(|&(s, _)| s < r.symbol);
            lefts.extend(
                late[lo..]
                    .iter()
                    .take_while(|&&(s, _)| s == r.symbol)
                    .map(|&(_, l)| Loc::new(r.file, l)),
            );
        }
        if lefts.is_empty() {
            return;
        }
        let mut groups: Vec<(StrId, Vec<Loc>)> = Vec::new();
        for left in lefts {
            let ast = self.ast(left.file);
            let Node::Index { index, .. } = *ast.node(left.node) else {
                continue;
            };
            let kt = self.quietly(|ck| ck.check_expr(Loc::new(left.file, index)));
            let kt = self.store().regular(kt);
            let Some(name) = self.property_name_of_type(kt) else {
                continue;
            };
            let n = self.store().intern_str(&name);
            if out.iter().any(|&(o, _)| o == n) {
                continue;
            }
            match groups.iter_mut().find(|g| g.0 == n) {
                Some(g) => g.1.push(left),
                None => groups.push((n, vec![left])),
            }
        }
        for (n, decls) in groups {
            // `const f: F = () => {}; f[k] = v` with `F` declaring `[k]`: the declared type.
            let name = self.store().str(n).to_owned();
            if self.annotated_expando_member(sym, &name).is_some() {
                let t = self.annotated_expando_values(&decls);
                let s = self.c.new_synth(
                    crate::symbols::SynthSym::new(n, SymbolFlags::PROPERTY, 0)
                        .with_decl(decls.first().copied())
                        .with_type(t),
                );
                out.push((n, s));
                continue;
            }
            let mut types = Vec::new();
            for &left in &decls {
                let Some(assign) = self.c.parent(left) else {
                    continue;
                };
                let Node::Assign { right, .. } = *self.node(assign) else {
                    continue;
                };
                if !self.push_resolution(crate::checker::Resolving::Node(left)) {
                    types.push(TypeId::ANY);
                    continue;
                }
                let t = self.quietly(|ck| ck.check_expr(Loc::new(left.file, right)));
                self.pop_resolution();
                let t = self.widen_literal(t);
                types.push(self.widened_type(t));
            }
            let t = if types.is_empty() {
                TypeId::ANY
            } else {
                self.union(&types)
            };
            let s = self.c.new_synth(
                crate::symbols::SynthSym::new(n, SymbolFlags::PROPERTY, 0)
                    .with_decl(decls.first().copied())
                    .with_type(t),
            );
            out.push((n, s));
        }
    }

    /// Value exports of a namespace-like symbol as `(name, symbol)` in declaration order.
    fn value_exports(&mut self, sym: SymId) -> Vec<(StrId, SymId)> {
        let mut out = Vec::new();
        let star_module = self
            .c
            .module_target_of(sym)
            .filter(|_| self.c.sym_flags(sym).intersects(SymbolFlags::MODULE_FILE));
        for (name, s) in self.c.exports_of(sym) {
            if name == "export="
                || name == "__export"
                || name.starts_with("__")
                    && name != "__proto__"
                    && self.c.sym_flags(s).intersects(SymbolFlags::SIGNATURE)
            {
                continue;
            }
            let target = if self.c.sym_flags(s).intersects(SymbolFlags::ALIAS) {
                self.c.resolve_alias(s)
            } else {
                Some(s)
            };
            let mut s = s;
            if !target.is_some_and(|t| self.c.has_meaning(t, meaning::VALUE)) {
                // An alias on the way merged with a local value exports that value.
                match self.alias_merged_value(s) {
                    Some(v) if self.c.sym_flags(s).intersects(SymbolFlags::ALIAS) => s = v,
                    _ => continue,
                }
            }
            if self.c.sym_facts(s).intersects(SymbolFacts::TYPE_ONLY)
                && !self.c.sym_flags(s).intersects(SymbolFlags::VALUE)
            {
                continue;
            }
            // Seen through a type-only import or export on the way (`export type { A }`
            // re-exported plainly), or only through `export type *`: no value property
            // (tsc: checker.go getPropertyOfTypeEx skips members that are not values when
            // type-only meanings are excluded).
            if self.c.sym_flags(s).intersects(SymbolFlags::ALIAS)
                && !self.c.sym_flags(s).intersects(SymbolFlags::VALUE)
                && self.type_only_alias_file(s).is_some()
            {
                continue;
            }
            if star_module.is_some_and(|t| self.c.link.star_export_is_type_only(t, &name)) {
                continue;
            }
            out.push((self.store().intern_str(&name), s));
        }
        out
    }

    fn namespace_members(&mut self, sym: SymId, t: TypeId) -> ResolvedMembers {
        let props = self.value_exports(sym);
        let mut index = Vec::new();
        if self.c.sym_flags(sym).intersects(SymbolFlags::REGULAR_ENUM) {
            let members = self.enum_members(sym);
            // An empty enum is numeric too.
            if members.is_empty()
                || members.iter().any(|m| {
                    m.1.is_none_or(|v| self.store().flags(v).contains(TypeFlags::NUMBER_LITERAL))
                })
            {
                index.push(IndexInfo {
                    key: TypeId::NUMBER,
                    value: TypeId::STRING,
                    readonly: true,
                    decl: None,
                });
            }
        }
        let _ = t;
        ResolvedMembers::new(props, Vec::new(), Vec::new(), index)
    }

    fn class_static_members(&mut self, sym: SymId) -> ResolvedMembers {
        let mut props: Vec<(StrId, SymId)> = Vec::new();
        let mut computed: Vec<(Loc, String, SymId)> = Vec::new();
        for d in self.c.decls(sym) {
            self.collect_computed_members(d, true, &mut computed);
        }
        let mut have: rustc_hash::FxHashSet<StrId> = rustc_hash::FxHashSet::default();
        for (name, s) in self.c.exports_of(sym) {
            if name == "prototype" {
                continue;
            }
            let target = if self.c.sym_flags(s).intersects(SymbolFlags::ALIAS) {
                match self.c.resolve_alias(s) {
                    Some(t) => t,
                    None => continue,
                }
            } else {
                s
            };
            if !self.c.has_meaning(target, meaning::VALUE)
                || self.c.sym_flags(target).intersects(SymbolFlags::SIGNATURE)
            {
                continue;
            }
            let n = self.store().intern_str(&name);
            have.insert(n);
            props.push((n, s));
        }
        for (_, name, s) in computed {
            let n = self.store().intern_str(&name);
            if have.insert(n) {
                props.push((n, s));
            }
        }
        // `prototype`: the instance type with `any` type arguments.
        let instance = self.declared_type_of_symbol(sym);
        let proto_ty = {
            let local = self.local_type_params(sym);
            if local.is_empty() {
                instance
            } else {
                let anys = vec![TypeId::ANY; local.len()];
                self.type_from_symbol_reference(sym, &anys)
            }
        };
        let pn = self.store().intern_str("prototype");
        // Not read-only: tsc 7 accepts `C.prototype = ...` for classes too.
        let proto = self
            .c
            .new_synth(SynthSym::new(pn, SymbolFlags::PROPERTY, 0).with_type(proto_ty));
        props.push((pn, proto));
        have.insert(pn);
        // Base constructor statics.
        let base_ctor = self.base_constructor_type(sym);
        if let Some(bc) = base_ctor {
            // (Each member of an intersection base: `extends Mixin(Base)`.)
            let parts = if self.store().flags(bc).contains(TypeFlags::INTERSECTION) {
                self.store().members_of_union(bc).to_vec()
            } else {
                vec![bc]
            };
            for part in parts {
                let bm = self.resolved_members(part);
                for (&n, &p) in bm.names.iter().zip(bm.props.iter()) {
                    if have.insert(n) {
                        props.push((n, p));
                    }
                }
            }
        }
        let construct = self.class_construct_signatures(sym, base_ctor);
        // A class merged with a function is also callable with the function's signatures.
        let call: Vec<SigId> = if self.c.sym_flags(sym).intersects(SymbolFlags::FUNCTION) {
            let decls: Vec<Loc> = self
                .signature_decls(sym)
                .into_iter()
                .filter(|d| matches!(self.node(*d), Node::FunctionDecl { .. }))
                .collect();
            decls
                .into_iter()
                .map(|l| self.signature_from_decl(l))
                .collect()
        } else {
            Vec::new()
        };
        // Static computed members of non-literal key types imply index signatures; a `string`
        // one may also read `prototype`, so it includes the instance type.
        // Explicit `static [k: K]: V` signatures, then the implied ones.
        let mut index = Vec::new();
        for d in self.c.decls(sym) {
            let ast = self.ast(d.file);
            let members = match *ast.node(d.node) {
                Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
                _ => continue,
            };
            let statics: Vec<NodeId> = ast
                .list(members)
                .iter()
                .copied()
                .filter(|&m| {
                    matches!(*ast.node(m), Node::IndexSignature { modifiers, .. }
                        if modifiers.contains(Modifiers::STATIC))
                })
                .collect();
            for m in statics {
                index.extend(self.index_infos_from_decl(Loc::new(d.file, m)));
            }
        }
        for d in self.c.decls(sym) {
            self.computed_index_infos(d, true, &mut index);
        }
        for i in index.iter_mut() {
            if i.decl.and_then(Loc::unpack).is_some_and(|l| {
                matches!(*self.ast(l.file).node(l.node), Node::IndexSignature { .. })
            }) {
                continue;
            }
            if i.key == TypeId::STRING {
                i.value = self.union(&[i.value, proto_ty]);
            }
        }
        // A class extending an `any` value has `any` statics: `C.unknown` is `any`
        // (observed: tsc 7 reports nothing), as `new C().unknown` is.
        if base_ctor.is_none()
            && !index.iter().any(|i| i.key == TypeId::STRING)
            && self.extends_expression_type(sym) == Some(TypeId::ANY)
        {
            index.push(IndexInfo {
                key: TypeId::STRING,
                value: TypeId::ANY,
                readonly: false,
                decl: None,
            });
        }
        ResolvedMembers::new(props, call, construct, index)
    }

    /// The number of type arguments in a class's `extends` clause.
    fn base_type_arg_count(&mut self, sym: SymId) -> usize {
        let Some(d) = self.c.decls(sym).into_iter().find(|d| {
            matches!(
                self.node(*d),
                Node::ClassDecl { .. } | Node::ClassExpr { .. }
            )
        }) else {
            return 0;
        };
        let ast = self.ast(d.file);
        let (Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. }) =
            *ast.node(d.node)
        else {
            return 0;
        };
        for &h in ast.list(heritage) {
            if let Node::HeritageClause {
                kind: ts_r_ast::HeritageKind::Extends,
                types,
            } = *ast.node(h)
                && let Some(&first) = ast.list(types).first()
                && let Node::ExprWithTypeArgs {
                    type_args: Some(ta),
                    ..
                } = *ast.node(first)
                && let Node::TypeArgs { args } = *ast.node(ta)
            {
                return ast.list(args).len();
            }
        }
        0
    }

    /// The type arguments of a class's `extends` clause (`extends Repo<E>`), empty without.
    fn base_type_args(&mut self, sym: SymId) -> Vec<TypeId> {
        let Some(d) = self.c.decls(sym).into_iter().find(|d| {
            matches!(
                self.node(*d),
                Node::ClassDecl { .. } | Node::ClassExpr { .. }
            )
        }) else {
            return Vec::new();
        };
        let ast = self.ast(d.file);
        let (Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. }) =
            *ast.node(d.node)
        else {
            return Vec::new();
        };
        for &h in ast.list(heritage) {
            if let Node::HeritageClause {
                kind: ts_r_ast::HeritageKind::Extends,
                types,
            } = *ast.node(h)
                && let Some(&first) = ast.list(types).first()
                && let Node::ExprWithTypeArgs {
                    type_args: Some(ta),
                    ..
                } = *ast.node(first)
                && let Node::TypeArgs { args } = *ast.node(ta)
            {
                let nodes = ast.list(args).to_vec();
                return nodes
                    .into_iter()
                    .map(|a| self.type_from_node(Loc::new(d.file, a)))
                    .collect();
            }
        }
        Vec::new()
    }

    /// The type of the `extends` expression of a class (the base constructor type).
    pub fn base_constructor_type(&mut self, sym: SymId) -> Option<TypeId> {
        let t = self.extends_expression_type(sym)?;
        if self
            .store()
            .flags(t)
            .intersects(TypeFlags::ANY | TypeFlags::NULL)
        {
            return None;
        }
        Some(t)
    }

    /// The type of a class's `extends` expression, `any` and `null` included.
    pub(crate) fn extends_expression_type(&mut self, sym: SymId) -> Option<TypeId> {
        let d = self.c.decls(sym).into_iter().find(|d| {
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
                kind: ts_r_ast::HeritageKind::Extends,
                types,
            } = *ast.node(h)
                && let Some(&first) = ast.list(types).first()
                && let Node::ExprWithTypeArgs { expr, .. } = *ast.node(first)
            {
                if !self.push_resolution(Resolving::BaseTypes(sym)) {
                    return None;
                }
                let t = self.quietly(|ck| ck.check_expr(Loc::new(d.file, expr)));
                self.pop_resolution();
                return Some(t);
            }
        }
        None
    }

    /// The type variable a mixin class extends (`class extends base` with `base: T`, or
    /// `T & X`), if any.
    pub fn base_type_variable(&mut self, sym: SymId) -> Option<TypeId> {
        let bc = self.base_constructor_type(sym)?;
        let s = self.store();
        let tv = TypeFlags::TYPE_PARAMETER | TypeFlags::INDEXED_ACCESS;
        if s.flags(bc).intersects(tv) {
            return Some(bc);
        }
        if s.flags(bc).contains(TypeFlags::INTERSECTION) {
            return s
                .members_of_union(bc)
                .iter()
                .copied()
                .find(|&m| s.flags(m).intersects(tv));
        }
        None
    }

    /// Whether `sig` is a mixin constructor's signature: one rest parameter of type
    /// `any[]` (or `any`) and no type parameters.
    pub(crate) fn is_mixin_construct_signature(&mut self, sig: SigId) -> bool {
        let s = self.store();
        let g = s.sig(sig);
        if g.type_params != ListId::EMPTY || !g.flags.contains(SigFlags::HAS_REST) {
            return false;
        }
        let [p] = &g.params[..] else {
            return false;
        };
        let pt = p.ty;
        pt == TypeId::ANY
            || self.c.is_array_type(pt) && self.c.type_args_of(pt).first() == Some(&TypeId::ANY)
    }

    /// Construct signatures of an intersection: members that are mixin constructors (one
    /// `new (...args: any[]) => X` signature) give up their signatures and mix their
    /// instance types into the others' (`typeof Mixin & typeof Base` constructs
    /// `Mixin & Base`); when every member is a mixin, the first signature constructs them
    /// all.
    fn intersection_construct_signatures(&mut self, members: &[TypeId]) -> Vec<SigId> {
        let mut lists = Vec::with_capacity(members.len());
        for &m in members {
            lists.push(self.signatures_of(m, SigKind::Construct));
        }
        let mixin: Vec<bool> = lists
            .iter()
            .map(|l| l.len() == 1 && self.is_mixin_construct_signature(l[0]))
            .collect();
        if !mixin.iter().any(|&m| m) || lists.iter().filter(|l| !l.is_empty()).count() < 2 {
            return lists.into_iter().flatten().collect();
        }
        let mixed: Vec<Option<TypeId>> = (0..lists.len())
            .map(|j| mixin[j].then(|| self.return_type_of(lists[j][0])))
            .collect();
        // (Instance types intersect in member order.)
        let mut targets: Vec<(usize, SigId)> = Vec::new();
        for (i, l) in lists.iter().enumerate() {
            if !mixin[i] {
                targets.extend(l.iter().map(|&s| (i, s)));
            }
        }
        if targets.is_empty() {
            let first = mixin.iter().position(|&m| m).unwrap_or(0);
            targets.push((first, lists[first][0]));
        }
        targets
            .into_iter()
            .map(|(i, sig)| {
                let own = self.return_type_of(sig);
                let parts: Vec<TypeId> = (0..lists.len())
                    .filter_map(|j| if j == i { Some(own) } else { mixed[j] })
                    .collect();
                let ret = self.intersection(&parts);
                let g = self.store().sig(sig);
                let new = Signature::new(
                    g.flags,
                    g.decl,
                    g.type_params,
                    g.params.to_vec(),
                    g.this_type,
                    g.min_args,
                    Some(ret),
                );
                self.store().add_signature(new)
            })
            .collect()
    }

    fn class_construct_signatures(&mut self, sym: SymId, base_ctor: Option<TypeId>) -> Vec<SigId> {
        let instance = self.declared_type_of_symbol(sym);
        let local = self.local_type_params(sym);
        let ret = if local.is_empty() {
            instance
        } else {
            self.type_from_symbol_reference(sym, &local)
        };
        let abstract_ = self
            .c
            .decls(sym)
            .iter()
            .any(|d| self.node(*d).modifiers().contains(Modifiers::ABSTRACT));
        // Explicit constructors.
        let mut ctor_decls: Vec<Loc> = Vec::new();
        if let Some((file, _)) = self
            .c
            .binder_name(sym)
            .or_else(|| self.c.first_decl(sym).map(|d| (d.file, Name(0))))
        {
            let _ = file;
        }
        for (file, table) in self.c.member_tables(sym) {
            let f = self.c.file(file);
            for &(name, s) in f.bound.table(table) {
                if name == Name::CONSTRUCTOR {
                    ctor_decls.extend(f.bound.decls(s).iter().map(|&d| Loc::new(file, d)));
                }
            }
        }
        // Constructors may be in the statics table depending on binding; search members too.
        if ctor_decls.is_empty() {
            for d in self.c.decls(sym) {
                let ast = self.ast(d.file);
                if let Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } =
                    *ast.node(d.node)
                {
                    for &m in ast.list(members) {
                        if let Node::MethodDef {
                            kind: MethodKind::Constructor,
                            ..
                        } = *ast.node(m)
                        {
                            ctor_decls.push(Loc::new(d.file, m));
                        }
                    }
                }
            }
        }
        let has_body =
            |ck: &Self, d: &Loc| fn_parts(ck.ast(d.file), d.node).is_some_and(|p| p.body.is_some());
        if ctor_decls.len() > 1 && ctor_decls.iter().any(|d| !has_body(self, d)) {
            ctor_decls.retain(|d| !has_body(self, d));
        }
        let tps = self.store().list(&local);
        let mut flags = SigFlags::CONSTRUCT;
        if abstract_ {
            flags |= SigFlags::ABSTRACT;
        }
        if !ctor_decls.is_empty() {
            return ctor_decls
                .into_iter()
                .map(|l| {
                    let base = self.signature_from_decl(l);
                    let b = self.store().sig(base);
                    let sig = Signature::new(
                        flags | (b.flags & SigFlags::HAS_REST),
                        b.decl,
                        tps,
                        b.params.to_vec(),
                        None,
                        b.min_args,
                        Some(ret),
                    );
                    self.store().add_signature(sig)
                })
                .collect();
        }
        if let Some(bc) = base_ctor {
            let bsigs = self.signatures_of(bc, SigKind::Construct);
            let base_args = if bsigs
                .iter()
                .any(|&bs| self.store().sig(bs).type_params != ListId::EMPTY)
            {
                self.base_type_args(sym)
            } else {
                Vec::new()
            };
            // Only base signatures taking the `extends` clause's type argument count are
            // inherited; when none does (TS2508/TS2314), the class has no construct
            // signatures.
            let js = self
                .c
                .first_decl(sym)
                .is_some_and(|d| self.c.file(d.file).is_js());
            let had_sigs = !bsigs.is_empty();
            let bsigs: Vec<SigId> = if js {
                bsigs.to_vec()
            } else {
                let n = self.base_type_arg_count(sym);
                bsigs
                    .iter()
                    .copied()
                    .filter(|&bs| {
                        let tps = self
                            .store()
                            .list_items(self.store().sig(bs).type_params)
                            .to_vec();
                        self.type_arg_count_fits(&tps, n)
                    })
                    .collect()
            };
            if had_sigs && bsigs.is_empty() {
                return Vec::new();
            }
            if !bsigs.is_empty() {
                return bsigs
                    .into_iter()
                    .map(|bs| {
                        let b = self.store().sig(bs);
                        let mut params = b.params.to_vec();
                        let mut min = b.min_args;
                        let mut fl = flags | (b.flags & SigFlags::HAS_REST);
                        // A generic base constructor takes the `extends` clause's type
                        // arguments (`extends Repo<E>`); without them, `any` arguments.
                        if b.type_params != ListId::EMPTY {
                            let tp = self.store().list_items(b.type_params).to_vec();
                            let fill =
                                if !js || (!base_args.is_empty() && base_args.len() <= tp.len()) {
                                    self.fill_type_args(&tp, &base_args)
                                } else {
                                    vec![TypeId::ANY; tp.len()]
                                };
                            let m = self.store().mapper(&tp, &fill);
                            params = params
                                .into_iter()
                                .map(|p| Param {
                                    ty: self.instantiate(p.ty, m),
                                    ..p
                                })
                                .collect();
                        }
                        if params.is_empty() {
                            min = 0;
                            fl = flags;
                        }
                        let sig = Signature::new(fl, b.decl, tps, params, None, min, Some(ret));
                        self.store().add_signature(sig)
                    })
                    .collect();
            }
        }
        let sig = Signature::new(flags, None, tps, Vec::new(), None, 0, Some(ret));
        vec![self.store().add_signature(sig)]
    }

    // ---------------------------------------------------------------------------------
    // Signatures from declarations
    // ---------------------------------------------------------------------------------

    /// The signature of a function-like declaration (parameter types resolved now, return
    /// type lazily).
    pub fn signature_from_decl(&mut self, l: Loc) -> SigId {
        let ast = self.ast(l.file);
        // Function expressions checked speculatively get fresh signatures (their parameter
        // and return types depend on the speculative contextual types).
        let cache = !(matches!(
            ast.node(l.node),
            Node::FunctionExpr { .. } | Node::Arrow { .. }
        ) && (self.speculative > 0 || !self.context_overrides.is_empty()));
        if cache && let Some(&s) = self.decl_sigs.get(&l) {
            return s;
        }
        let Some(parts) = fn_parts(ast, l.node) else {
            let s = self.store().add_signature(Signature::new(
                SigFlags::NONE,
                Some(l.pack()),
                ListId::EMPTY,
                Vec::new(),
                None,
                0,
                Some(TypeId::UNRESOLVED),
            ));
            return s;
        };
        let tps = match parts.type_params {
            Some(tp) => self.type_params_from_node(Loc::new(l.file, tp)),
            None => Vec::new(),
        };
        let mut flags = SigFlags::NONE;
        if parts.is_construct {
            flags |= SigFlags::CONSTRUCT;
        }
        if matches!(
            ast.node(l.node),
            Node::MethodDef { .. } | Node::MethodSignature { .. }
        ) {
            flags |= SigFlags::METHOD;
        }
        if parts.flags.contains(ts_r_ast::FnFlags::ASYNC) {
            flags |= SigFlags::ASYNC;
        }
        if parts.flags.contains(ts_r_ast::FnFlags::GENERATOR) {
            flags |= SigFlags::GENERATOR;
        }
        if matches!(
            ast.node(l.node),
            Node::ConstructorType {
                is_abstract: true,
                ..
            }
        ) {
            flags |= SigFlags::ABSTRACT;
        }
        let (params, this_type, mut min_args, has_rest) = self.params_of(l.file, parts.params);
        // An immediately invoked function: unannotated parameters past the call's written
        // arguments (a spread counts as one) are optional (`((a, b) => {})(1)`).
        if let Some(call) = self.iife_call(l)
            && let Node::Call { args, .. } = *ast.node(call.node)
        {
            let n_args = ast.list(args).len();
            let mut last_required = 0;
            let mut i = 0;
            for &p in ast.list(parts.params) {
                if is_this_param(ast, p) {
                    continue;
                }
                if let Node::Param {
                    rest,
                    optional,
                    type_ann,
                    init,
                    ..
                } = *ast.node(p)
                    && !rest
                    && !optional
                    && init.is_none()
                    && (type_ann.is_some() || i < n_args)
                {
                    last_required = i + 1;
                }
                i += 1;
            }
            min_args = min_args.min(last_required as u32);
        }
        // A JavaScript function without parameter types takes any number of arguments.
        if self.is_untyped_js_signature(l) {
            min_args = 0;
        }
        if has_rest {
            flags |= SigFlags::HAS_REST;
        }
        let mut ret = None;
        let mut pred = None;
        if let Some(rt) = parts.return_type {
            flags |= SigFlags::HAS_RETURN_TYPE;
            let rl = Loc::new(l.file, rt);
            if let Node::TypePredicate { asserts, param, ty } = *ast.node(rt) {
                let is_this = matches!(ast.node(param), Node::ThisType {});
                let pname = crate::nodes::ident_text(ast, param).unwrap_or("this");
                let idx = params
                    .iter()
                    .position(|p| self.store().str(p.name) == pname)
                    .unwrap_or(0) as u32;
                let kind = match (asserts, is_this) {
                    (false, false) => PredicateKind::Identifier,
                    (false, true) => PredicateKind::This,
                    (true, false) => PredicateKind::AssertsIdentifier,
                    (true, true) => PredicateKind::AssertsThis,
                };
                let pt = ty.map(|t| self.type_from_node(Loc::new(l.file, t)));
                pred = Some(TypePredicate {
                    kind,
                    param_index: idx,
                    param_name: self.store().intern_str(pname),
                    ty: pt,
                });
                ret = Some(if asserts {
                    TypeId::VOID
                } else {
                    TypeId::BOOLEAN
                });
            } else {
                ret = Some(self.type_from_node(rl));
            }
        } else if matches!(
            ast.node(l.node),
            Node::MethodDef {
                kind: MethodKind::Constructor,
                ..
            }
        ) {
            ret = Some(TypeId::VOID);
        } else if matches!(
            ast.node(l.node),
            Node::MethodSignature { .. }
                | Node::CallSignature { .. }
                | Node::ConstructSignature { .. }
        ) || parts.body.is_none_or(|b| {
            // (So does a body the parser made up after a missing `{`.)
            let sp = ast.span(b);
            sp.start == sp.end && matches!(ast.node(b), Node::Block { .. })
        }) {
            // Body-less declarations without an annotation return `any`.
            ret = Some(TypeId::ANY);
        } else if self.c.file(l.file).is_js()
            && let Some(js) = self.jsdoc_declared_signature(l)
        {
            // `/** @type {(x: number) => string} */ function f(x) {}` returns `string`.
            flags |= SigFlags::HAS_RETURN_TYPE;
            ret = Some(self.return_type_of(js));
            pred = self.predicate_of(js);
        }
        let tlist = self.store().list(&tps);
        let mut sig = Signature::new(
            flags,
            Some(l.pack()),
            tlist,
            params,
            this_type,
            min_args,
            ret,
        );
        if ret.is_some() || pred.is_some() {
            sig = sig.with_predicate(pred);
        }
        let id = self.store().add_signature(sig);
        if cache {
            self.decl_sigs.insert(l, id);
        }
        id
    }

    /// Parameters of a parameter list: `(params, this type, min args, has rest)`.
    pub fn params_of(
        &mut self,
        file: ts_r_diagnostics::FileId,
        list: ts_r_ast::NodeList,
    ) -> (Vec<Param>, Option<TypeId>, u32, bool) {
        let ast = self.ast(file);
        let mut params = Vec::new();
        let mut this_type = None;
        let mut last_required: i64 = -1;
        let mut has_rest = false;
        for &p in ast.list(list) {
            let Node::Param {
                rest,
                pattern,
                optional,
                type_ann,
                init,
                ..
            } = *ast.node(p)
            else {
                continue;
            };
            if is_this_param(ast, p) {
                this_type = Some(match type_ann {
                    Some(t) => self.type_from_node(Loc::new(file, t)),
                    None => TypeId::ANY,
                });
                continue;
            }
            let name = match *ast.node(pattern) {
                Node::Ident { name } => self.store().intern_str(ast.atom(name)),
                _ => {
                    let text = pattern_display(ast, pattern, self.c.text(Loc::new(file, pattern)));
                    self.store().intern_str(&text)
                }
            };
            let mut ty = self.param_type(Loc::new(file, p));
            // Callers may pass `undefined` for a parameter with an initializer (inside the
            // body the initializer removes it).
            if init.is_some() && !optional && !rest {
                ty = self.add_optionality(ty);
            }
            let sym = match *ast.node(pattern) {
                Node::Ident { .. } => self.c.sym_of_decl(Loc::new(file, p)).unwrap_or(SymId::NONE),
                _ => SymId::NONE,
            };
            let is_opt = optional || init.is_some();
            if rest {
                has_rest = true;
            } else if !is_opt {
                last_required = params.len() as i64;
            }
            params.push(Param {
                name,
                ty,
                optional: is_opt || rest,
                sym,
            });
        }
        let mut min = (last_required + 1) as u32;
        // Trailing `void` parameters may be omitted.
        while min > 0 {
            let t = params[(min - 1) as usize].ty;
            let s = self.store();
            if s.flags(t).contains(TypeFlags::VOID)
                || (s.flags(t).contains(TypeFlags::UNION)
                    && s.members_of_union(t)
                        .iter()
                        .any(|&m| s.flags(m).contains(TypeFlags::VOID)))
            {
                min -= 1;
            } else {
                break;
            }
        }
        (params, this_type, min, has_rest)
    }

    /// Whether the parameter `p` belongs to a signature written as a type (a function or
    /// constructor type, or a call, construct or method signature).
    fn in_type_position(&mut self, p: Loc) -> bool {
        self.c.parent(p).is_some_and(|s| {
            matches!(
                self.node(s),
                Node::FunctionType { .. }
                    | Node::ConstructorType { .. }
                    | Node::MethodSignature { .. }
                    | Node::CallSignature { .. }
                    | Node::ConstructSignature { .. }
            )
        })
    }

    /// The declared type of a parameter node (annotation, initializer, contextual type or
    /// `any`), including `undefined` for optional parameters.
    pub fn param_type(&mut self, p: Loc) -> TypeId {
        if let Some(&t) = self.param_types.get(&p) {
            return t;
        }
        let ast = self.ast(p.file);
        let Node::Param {
            rest,
            pattern,
            optional,
            type_ann,
            init,
            ..
        } = *ast.node(p.node)
        else {
            return TypeId::ERROR;
        };
        if let Some(t) = type_ann {
            let ty = self.type_from_node(Loc::new(p.file, t));
            // In the true branch of a conditional type a parameter's type can carry the
            // implied constraint (`number extends T ? (cb: (n: number) => void) => void`
            // types `n` as `number & T`).
            let ty = if self.in_type_position(p) {
                self.with_implied_constraints(Loc::new(p.file, t), ty)
            } else {
                ty
            };
            if optional && !rest {
                return self.add_optionality(ty);
            }
            return ty;
        }
        // Contextually typed parameters are assigned by the expression checker.
        if let Some(t) = self.contextual_param_type(p) {
            return t;
        }
        if let Some(i) = init {
            let t = self.quietly(|ck| ck.check_expr(Loc::new(p.file, i)));
            let t = self.widen_literal(t);
            let t = self.widened_type(t);
            if matches!(ast.node(pattern), Node::ObjectPattern { .. }) {
                // Pattern properties the default lacks are added from the pattern
                // (`({ a = 1 } = {})` takes `{ a?: number }`).
                let pt = self.type_from_binding_pattern(Loc::new(p.file, pattern));
                return self.pad_with_pattern(t, pt);
            }
            if matches!(ast.node(pattern), Node::ArrayPattern { .. }) {
                return self.pad_tuple_with_pattern(t, Loc::new(p.file, pattern));
            }
            return t;
        }
        // A binding pattern without annotation: the type implied by the pattern (a rest
        // `...[a, b]` takes the tuple `[any, any]`, so two arguments).
        if !matches!(ast.node(pattern), Node::Ident { .. }) {
            return self.type_from_binding_pattern(Loc::new(p.file, pattern));
        }
        if rest {
            return self.create_array(TypeId::IMPLICIT_ANY, false);
        }
        if let Some(t) = self.setter_param_type_from_getter(p) {
            return t;
        }
        TypeId::IMPLICIT_ANY
    }

    /// The property name a binding pattern key binds: as written, or a computed key's
    /// string or numeric literal type (`{ [named]: x }` with `const named = "prop"`).
    pub(crate) fn binding_key_name(&mut self, key: Loc) -> Option<String> {
        let ast = self.ast(key.file);
        if let Node::ComputedKey { expr } = *ast.node(key.node) {
            return self
                .late_bound_name(Loc::new(key.file, expr))
                .filter(|n| !n.starts_with("__@"));
        }
        crate::nodes::key_text(ast, key.node)
    }

    /// The type implied by a binding pattern (`{ a, b }` → `{ a: any; b: any }`).
    /// Elements with a default are optional and typed by it (`{ a = 1 }` → `{ a?: number }`),
    /// nested patterns recurse, an object rest adds `[x: string]: any`, and an array pattern
    /// is a tuple (`[a, b = 1]` → `[any, number?]`). Plain names are implicitly `any`.
    pub fn type_from_binding_pattern(&mut self, l: Loc) -> TypeId {
        if self.depth > 100 {
            return TypeId::IMPLICIT_ANY;
        }
        self.depth += 1;
        let r = self.type_from_binding_pattern_inner(l);
        self.depth -= 1;
        r
    }

    fn type_from_binding_pattern_inner(&mut self, l: Loc) -> TypeId {
        let ast = self.ast(l.file);
        match *ast.node(l.node) {
            Node::ObjectPattern { props } => {
                let mut members: Vec<(ts_r_types::StrId, SymId)> = Vec::new();
                let mut index = Vec::new();
                for &p in ast.list(props) {
                    match *ast.node(p) {
                        Node::BindingProperty { key, value, init } => {
                            let Some(name) = self.binding_key_name(Loc::new(l.file, key)) else {
                                continue;
                            };
                            let mut ty = match value {
                                Some(v) if !matches!(ast.node(v), Node::Ident { .. }) => {
                                    self.type_from_binding_pattern(Loc::new(l.file, v))
                                }
                                _ => TypeId::IMPLICIT_ANY,
                            };
                            let mut flags = SymbolFlags::PROPERTY;
                            if let Some(i) = init {
                                ty = self.binding_default_type(Loc::new(l.file, i));
                                ty = self.add_optionality(ty);
                                flags |= SymbolFlags::OPTIONAL;
                            }
                            let nid = self.store().intern_str(&name);
                            if members.iter().any(|m| m.0 == nid) {
                                continue;
                            }
                            let s = self.c.new_synth(SynthSym::new(nid, flags, 0).with_type(ty));
                            members.push((nid, s));
                        }
                        Node::Rest { .. } => index.push(ts_r_types::IndexInfo {
                            key: TypeId::STRING,
                            value: TypeId::ANY,
                            readonly: false,
                            decl: None,
                        }),
                        _ => {}
                    }
                }
                self.store().literal_object(
                    SymId::NONE,
                    ObjectFlags::ANONYMOUS,
                    ResolvedMembers::new(members, Vec::new(), Vec::new(), index),
                    ts_r_types::TypeTraits::NONE,
                )
            }
            Node::ArrayPattern { elements } if ast.list(elements).is_empty() => {
                self.empty_array_pattern_type()
            }
            Node::ArrayPattern { elements } => {
                let mut els = Vec::new();
                for &e in ast.list(elements) {
                    let el = match *ast.node(e) {
                        Node::Hole {} => crate::declared::TupleEl {
                            ty: TypeId::ANY,
                            kind: TupleElement::REQUIRED,
                            label: None,
                        },
                        Node::AssignPattern { right, .. } => crate::declared::TupleEl {
                            ty: {
                                let t = self.binding_default_type(Loc::new(l.file, right));
                                self.add_optionality(t)
                            },
                            kind: TupleElement::OPTIONAL,
                            label: None,
                        },
                        Node::Rest { .. } => crate::declared::TupleEl {
                            ty: self.create_array(TypeId::ANY, false),
                            kind: TupleElement::REST,
                            label: None,
                        },
                        Node::ObjectPattern { .. } | Node::ArrayPattern { .. } => {
                            crate::declared::TupleEl {
                                ty: self.type_from_binding_pattern(Loc::new(l.file, e)),
                                kind: TupleElement::REQUIRED,
                                label: None,
                            }
                        }
                        _ => crate::declared::TupleEl {
                            ty: TypeId::IMPLICIT_ANY,
                            kind: TupleElement::REQUIRED,
                            label: None,
                        },
                    };
                    els.push(el);
                }
                // A lone rest element implies an array, not a tuple (`[...a] = [1, 2]`
                // takes `number[]`, observed).
                if let [only] = els.as_slice()
                    && only.kind == TupleElement::REST
                {
                    return only.ty;
                }
                self.create_tuple(&els, false)
            }
            _ => TypeId::IMPLICIT_ANY,
        }
    }

    /// The type an empty array pattern implies (`function f([]) {}`): any iterable,
    /// `Iterable<any, void, undefined>` from ES2015 on, else `any[]` (observed).
    fn empty_array_pattern_type(&mut self) -> TypeId {
        if self.c.opts.target >= ts_r_config::ScriptTarget::ES2015
            && let Some(d) = self.global_type(GlobalType::Iterable)
        {
            let s = self.store();
            if let Some(ObjectKind::Declared { type_params, .. }) = s.object_of(d).map(|o| o.kind)
                && s.list_items(type_params).len() == 3
            {
                return self.make_reference(d, &[TypeId::ANY, TypeId::VOID, TypeId::UNDEFINED]);
            }
        }
        self.create_array(TypeId::ANY, false)
    }

    /// A tuple default shorter than its array pattern gains the pattern's remaining elements
    /// as optional ones (`([x, y = 0] = [0])` takes `[number, (number | undefined)?]`,
    /// `([x, y] = [])` takes `[any?, any?]`). Patterns with a rest element and defaults that
    /// are not fixed tuples are unchanged.
    fn pad_tuple_with_pattern(&mut self, t: TypeId, pattern: Loc) -> TypeId {
        let Some(shape) = self.store().tuple_shape_of(t) else {
            return t;
        };
        let shape = shape.clone();
        if shape
            .elements
            .iter()
            .any(|e| e.kind == TupleElement::REST || e.kind == TupleElement::VARIADIC)
        {
            return t;
        }
        let ast = self.ast(pattern.file);
        let Node::ArrayPattern { elements } = *ast.node(pattern.node) else {
            return t;
        };
        let pels = ast.list(elements);
        if pels.len() <= shape.elements.len()
            || pels
                .iter()
                .any(|&e| matches!(ast.node(e), Node::Rest { .. }))
        {
            return t;
        }
        let pt = self.type_from_binding_pattern(pattern);
        let pargs = self.c.type_args_of_tuple(pt).to_vec();
        let args = self.c.type_args_of_tuple(t).to_vec();
        let mut els: Vec<crate::declared::TupleEl> = shape
            .elements
            .iter()
            .enumerate()
            .map(|(i, el)| crate::declared::TupleEl {
                ty: args.get(i).copied().unwrap_or(TypeId::UNRESOLVED),
                kind: el.kind,
                label: el.label,
            })
            .collect();
        for i in shape.elements.len()..pels.len() {
            els.push(crate::declared::TupleEl {
                ty: pargs.get(i).copied().unwrap_or(TypeId::ANY),
                kind: TupleElement::OPTIONAL,
                label: None,
            });
        }
        self.create_tuple(&els, shape.readonly)
    }

    /// `t` with the properties of the pattern type `pt` it lacks.
    fn pad_with_pattern(&mut self, t: TypeId, pt: TypeId) -> TypeId {
        let st = self.store();
        if !st.flags(t).contains(TypeFlags::OBJECT) || st.flags(t).contains(TypeFlags::UNION) {
            return t;
        }
        let mut props = self.properties_of(t);
        let extra: Vec<(StrId, SymId)> = self
            .properties_of(pt)
            .into_iter()
            .filter(|(n, _)| !props.iter().any(|(m, _)| m == n))
            .collect();
        if extra.is_empty() {
            return t;
        }
        props.extend(extra);
        let m = self.resolved_members(t);
        let index = m.index.to_vec();
        self.store().literal_object(
            SymId::NONE,
            ObjectFlags::ANONYMOUS,
            ResolvedMembers::new(props, Vec::new(), Vec::new(), index),
            TypeTraits::NONE,
        )
    }

    /// The type a binding element's default gives it in an implied pattern type.
    fn binding_default_type(&mut self, init: Loc) -> TypeId {
        let t = self.quietly(|ck| ck.check_expr(init));
        let t = self.widen_literal(t);
        self.widened_type(t)
    }

    /// The minimum argument count of `sig` after instantiation: trailing parameters whose
    /// type accepts `void` (e.g. `resolve(value: void | PromiseLike<void>)`) are optional.
    pub fn effective_min_args(&self, sig: SigId) -> usize {
        let s = self.store();
        let d = s.sig(sig);
        let fixed = d.params.len() - usize::from(d.has_rest());
        let mut min = (d.min_args as usize).min(fixed);
        while min > 0 {
            let t = d.params[min - 1].ty;
            let accepts_void = s.flags(t).contains(TypeFlags::VOID)
                || (s.flags(t).contains(TypeFlags::UNION)
                    && s.members_of_union(t)
                        .iter()
                        .any(|&m| s.flags(m).contains(TypeFlags::VOID)));
            if !accepts_void {
                break;
            }
            min -= 1;
        }
        min
    }

    /// The return type of a signature (inferring it from the body when not annotated).
    pub fn return_type_of(&mut self, sig: SigId) -> TypeId {
        let s = self.store();
        let d = s.sig(sig);
        if let Some(r) = d.ret() {
            return r;
        }
        if let Some((orig, m)) = d.target {
            let r = self.return_type_of(orig);
            let r = self.instantiate(r, m);
            return self.store().sig(sig).set_ret(r);
        }
        let Some(decl) = d.decl.and_then(Loc::unpack) else {
            return s.sig(sig).set_ret(TypeId::ANY);
        };
        if !self.push_resolution(Resolving::Return(sig)) {
            return TypeId::ANY;
        }
        let r = self.quietly(|ck| ck.infer_return_type(decl, sig));
        if !self.pop_resolution() {
            // Circular inference: `any` (TS7023/7024 are reported by the walk).
            return self.store().sig(sig).set_ret(TypeId::ANY);
        }
        self.store().sig(sig).set_ret(r)
    }

    /// The type predicate of a signature.
    pub fn predicate_of(&mut self, sig: SigId) -> Option<TypePredicate> {
        let s = self.store();
        let d = s.sig(sig);
        if let Some(p) = d.predicate() {
            return p;
        }
        if let Some((orig, m)) = d.target {
            let p = self.predicate_of(orig).map(|p| TypePredicate {
                ty: p.ty.map(|t| self.instantiate(t, m)),
                ..p
            });
            return self.store().sig(sig).set_predicate(p);
        }
        let p = self.inferred_type_predicate(sig);
        self.store().sig(sig).set_predicate(p)
    }

    // ---------------------------------------------------------------------------------
    // Apparent types and property lookup
    // ---------------------------------------------------------------------------------

    /// The base constraint of an instantiable type (`unknown` when unconstrained).
    ///
    /// Constraints can reach the type again through a template, string mapping, indexed
    /// access or union (`T extends \`x${T}\``): a type already being resolved counts as
    /// unconstrained, and the nesting is capped.
    pub fn base_constraint(&mut self, t: TypeId) -> TypeId {
        // A type parameter whose constraint reaches it again through a union, intersection or
        // indexed access is circular: give up rather than recurse.
        let key = Resolving::BaseConstraint(t);
        let param = self.store().flags(t).contains(TypeFlags::TYPE_PARAMETER);
        // Checked first so the entries in between are not marked circular.
        if param && self.is_resolving(key) {
            return TypeId::UNRESOLVED;
        }
        if self.base_constraints.contains(&t) {
            return TypeId::UNKNOWN;
        }
        if self.base_constraints.len() >= MAX_BASE_CONSTRAINT_DEPTH {
            return TypeId::UNRESOLVED;
        }
        if param {
            self.push_resolution(key);
        }
        self.base_constraints.push(t);
        let r = self.base_constraint_of(t);
        self.base_constraints.pop();
        if param {
            self.pop_resolution();
        }
        r
    }

    fn base_constraint_of(&mut self, t: TypeId) -> TypeId {
        let mut cur = t;
        for _ in 0..50 {
            let s = self.store();
            let f = s.flags(cur);
            if f.contains(TypeFlags::TYPE_PARAMETER) {
                match self.type_param_constraint(cur) {
                    Some(c) => cur = c,
                    None => return TypeId::UNKNOWN,
                }
                continue;
            }
            if f.contains(TypeFlags::INDEX) {
                return self.keyof_constraint();
            }
            if f.contains(TypeFlags::INDEXED_ACCESS) {
                if let TypeKind::IndexedAccess { obj, index } = s.kind(cur) {
                    // `{ [K in C]: X }[C]` is `X` with `K` replaced by `C`.
                    if let Some(sub) = self.indexed_mapped_substitution_ext(cur, true)
                        && sub != cur
                    {
                        cur = sub;
                        continue;
                    }
                    // `(A & B)[K]` is constrained by `A[K]`'s and `B[K]`'s constraints.
                    if self.store().flags(obj).contains(TypeFlags::INTERSECTION) {
                        let ms = self.store().members_of_union(obj).to_vec();
                        let mut parts = Vec::with_capacity(ms.len());
                        for m in ms {
                            let a = self.store().indexed_access(m, index);
                            let c = self.base_constraint(a);
                            if c != TypeId::UNRESOLVED && c != TypeId::UNKNOWN {
                                parts.push(c);
                            }
                        }
                        if parts.is_empty() {
                            return TypeId::UNRESOLVED;
                        }
                        return self.intersect_constraints(parts);
                    }
                    let o = self.base_constraint(obj);
                    let i = self.base_constraint(index);
                    if o == obj && i == index {
                        return TypeId::UNRESOLVED;
                    }
                    // Without an applicable index signature, a symbol key reads the string
                    // index signature (`Record<string, X>[keyof T]` is `X`; tsc: checker.go,
                    // getPropertyTypeForIndexType).
                    let r = self
                        .indexed_access_type(o, i, None)
                        .or_else(|| self.indexed_access_symbol_as_string(o, i))
                        .unwrap_or(TypeId::UNRESOLVED);
                    // Circular (`F extends M<F>`, `M<F>[string]` reading `F[string]`): no
                    // constraint.
                    if r == t {
                        return TypeId::UNRESOLVED;
                    }
                    // A generic result (`C<V> | undefined`) has a base constraint too.
                    if r != cur
                        && r != TypeId::UNRESOLVED
                        && self.store().flags(r).contains(TypeFlags::UNION)
                        && self.is_generic_type(r)
                    {
                        cur = r;
                        continue;
                    }
                    return r;
                }
                return TypeId::UNRESOLVED;
            }
            if f.contains(TypeFlags::SUBSTITUTION)
                && let TypeKind::Substitution { base, .. } = s.kind(cur)
            {
                cur = base;
                continue;
            }
            if f.contains(TypeFlags::CONDITIONAL) {
                // The base constraint of the constraint, with this conditional marked as
                // being resolved (recursive aliases such as `Awaited<T>` nest new types).
                let Some(c) = self.conditional_constraint(cur).filter(|&c| c != cur) else {
                    return TypeId::UNRESOLVED;
                };
                let key = crate::checker::Resolving::ConditionalConstraint(cur);
                if self.cond_constraint_depth >= 4 || self.is_resolving(key) {
                    return TypeId::UNRESOLVED;
                }
                self.cond_constraint_depth += 1;
                self.push_resolution(key);
                let r = self.base_constraint(c);
                self.pop_resolution();
                self.cond_constraint_depth -= 1;
                return r;
            }
            // Template literal and string mapping types: the same type over the base
            // constraints of their spans (a non-generic one is its own constraint).
            if f.contains(TypeFlags::TEMPLATE_LITERAL) {
                if !s.traits(cur).contains(TypeTraits::CONTAINS_TYPE_PARAMETER) {
                    return cur;
                }
                let Some((texts, types)) = s.template_parts(cur) else {
                    return TypeId::STRING;
                };
                let texts: Vec<String> = texts
                    .iter()
                    .map(|&x| s.string_value(x).unwrap_or("").to_owned())
                    .collect();
                let types = types.to_vec();
                let mut cs = Vec::with_capacity(types.len());
                for ty in types {
                    let c = self.base_constraint(ty);
                    if c == TypeId::UNKNOWN || c == TypeId::UNRESOLVED || c == ty {
                        return TypeId::STRING;
                    }
                    cs.push(c);
                }
                return self.template_literal_type(&texts, &cs);
            }
            if f.contains(TypeFlags::STRING_MAPPING) {
                if !s.traits(cur).contains(TypeTraits::CONTAINS_TYPE_PARAMETER) {
                    return cur;
                }
                if let TypeKind::StringMapping { sym, ty } = s.kind(cur) {
                    let c = self.base_constraint(ty);
                    if c != TypeId::UNKNOWN && c != TypeId::UNRESOLVED && c != ty {
                        return self.string_mapping_type(sym, c);
                    }
                }
                return TypeId::STRING;
            }
            if f.contains(TypeFlags::UNION) && self.is_generic_type(cur) {
                let ms = s.members_of_union(cur).to_vec();
                let parts: Vec<TypeId> = ms.iter().map(|&m| self.base_constraint(m)).collect();
                return self.union(&parts);
            }
            if f.contains(TypeFlags::INTERSECTION) && self.is_generic_type(cur) {
                let ms = s.members_of_union(cur).to_vec();
                let parts: Vec<TypeId> = ms.iter().map(|&m| self.base_constraint(m)).collect();
                return self.intersect_constraints(parts);
            }
            return cur;
        }
        TypeId::UNRESOLVED
    }

    /// The intersection of constraints, without a union that contains every member of
    /// another part (`(A | B | undefined) & (A | B)` is `A | B`).
    fn intersect_constraints(&mut self, mut parts: Vec<TypeId>) -> TypeId {
        let mut i = 0;
        while i < parts.len() {
            let pi = parts[i];
            let superset = self.store().flags(pi).contains(TypeFlags::UNION)
                && parts.iter().enumerate().any(|(j, &pj)| {
                    j != i && pj != pi && {
                        let big = self.store().members_of_union(pi);
                        self.union_members(pj).iter().all(|m| big.contains(m))
                    }
                });
            if superset {
                parts.remove(i);
            } else {
                i += 1;
            }
        }
        self.intersection(&parts)
    }

    /// `string | number | symbol`.
    pub fn keyof_constraint(&mut self) -> TypeId {
        if let Some(&t) = self.c.cached.string_number_symbol.get() {
            return t;
        }
        let t = self.union(&[TypeId::STRING, TypeId::NUMBER, TypeId::SYMBOL]);
        let _ = self.c.cached.string_number_symbol.set(t);
        t
    }

    /// The type whose members a value of type `t` has (primitives → their wrapper
    /// interfaces, type parameters → their constraints).
    pub fn apparent_type(&mut self, t: TypeId) -> TypeId {
        // A generic intersection (`T[K] & {}` from narrowing) is the intersection of its
        // members' constraints.
        let t =
            if self.store().flags(t).contains(TypeFlags::INTERSECTION) && self.is_generic_type(t) {
                let b = self.base_constraint(t);
                // (`string & {}`, the constraint of `NonNullable<T>` with `T extends string |
                // undefined`, is the primitive's.)
                if b == TypeId::UNRESOLVED
                    || (self.store().flags(b).contains(TypeFlags::INTERSECTION)
                        && !self.is_primitive_with_empty_object(b))
                {
                    t
                } else {
                    b
                }
            } else {
                t
            };
        let t = self.primitive_with_empty_object(t).unwrap_or(t);
        let t = if self.store().flags(t).intersects(TypeFlags::INSTANTIABLE) {
            let b = self.base_constraint(t);
            if b == TypeId::UNKNOWN {
                // Under strictNullChecks an unconstrained type parameter has no members
                // (not even `Object`'s: `t.toString()` is TS2339 on `T`).
                if self.c.opts.strict_null_checks {
                    return TypeId::UNKNOWN;
                } else {
                    TypeId::EMPTY_OBJECT
                }
            } else if self.store().type_param_data(t).is_some_and(|d| d.is_this) {
                // The members of a `this` type are its class's with `this` still the type
                // parameter: `this.items[0]` of `items: this[]` is `this`, not the class.
                self.with_this_arg(b, t)
            } else if self.store().flags(t).contains(TypeFlags::TYPE_PARAMETER)
                && self.has_this_receiver_members(b)
            {
                // So are a type parameter's, with `this` the type parameter: `x.self()` with
                // `x: T extends A` and `self(): this` is `T`.
                return self.with_this_arg(b, t);
            } else {
                b
            }
        } else {
            t
        };
        let s = self.store();
        // Generic mapped types are opaque (members unknown until instantiated).
        if s.object_of(t)
            .is_some_and(|o| matches!(o.kind, ObjectKind::Mapped { .. }))
            && self.is_generic_mapped_type(t)
        {
            return match self.mapped_apparent_type(t) {
                Some(a) if a != t => self.apparent_type(a),
                // Its members come from the lower bound of its key type.
                _ if self.mapped_lower_bound_keys(t).is_some() => t,
                _ => TypeId::UNRESOLVED,
            };
        }
        let f = s.flags(t);
        let g = if f.intersects(TypeFlags::STRING_LIKE) {
            Some(GlobalType::String)
        } else if f.intersects(TypeFlags::NUMBER_LIKE) {
            Some(GlobalType::Number)
        } else if f.intersects(TypeFlags::BIGINT_LIKE) {
            Some(GlobalType::BigInt)
        } else if f.intersects(TypeFlags::BOOLEAN_LIKE) && !f.contains(TypeFlags::UNION) {
            Some(GlobalType::Boolean)
        } else if f.intersects(TypeFlags::ES_SYMBOL_LIKE) {
            Some(GlobalType::Symbol)
        } else {
            None
        };
        if let Some(g) = g {
            return self.global_type(g).unwrap_or(TypeId::EMPTY_OBJECT);
        }
        if f.contains(TypeFlags::NON_PRIMITIVE) {
            return TypeId::EMPTY_OBJECT;
        }
        if f.contains(TypeFlags::UNKNOWN) {
            return TypeId::EMPTY_OBJECT;
        }
        // The class and interface members of an intersection see the whole intersection as
        // `this` (`mockImplementation(): this` on `MockInstance<T> & (() => R)`).
        if f.contains(TypeFlags::INTERSECTION)
            && s.members_of_union(t).iter().any(|&m| {
                matches!(
                    s.object_of(m).map(|o| o.kind),
                    Some(ObjectKind::Declared { .. } | ObjectKind::Reference { .. })
                ) && s.tuple_shape_of(m).is_none()
            })
        {
            // A member that already has its `this` (the base intersection of `interface
            // Thing5 extends Thing1 & Thing2` has `Thing5`'s) keeps it.
            let parts: Vec<TypeId> = s.members_of_union(t).to_vec();
            let mapped: Vec<TypeId> = parts
                .iter()
                .map(|&p| {
                    if self.has_explicit_this_arg(p) {
                        p
                    } else {
                        self.with_this_arg(p, t)
                    }
                })
                .collect();
            return self.intersection(&mapped);
        }
        t
    }

    /// Whether `t` is a class or interface reference with an explicit `this` argument.
    fn has_explicit_this_arg(&self, t: TypeId) -> bool {
        let s = self.store();
        if let Some(ObjectKind::Reference { target, args }) = s.object_of(t).map(|o| o.kind)
            && let Some(ObjectKind::Declared { type_params, .. }) =
                s.object_of(target).map(|o| o.kind)
        {
            return s.list_items(args).len() > s.list_items(type_params).len();
        }
        false
    }

    /// A class or interface type (or an intersection with one), whose members may mention
    /// `this`.
    fn has_this_receiver_members(&self, t: TypeId) -> bool {
        let s = self.store();
        let is_decl = |m: TypeId| {
            matches!(
                s.object_of(m).map(|o| o.kind),
                Some(ObjectKind::Declared { .. } | ObjectKind::Reference { .. })
            ) && s.tuple_shape_of(m).is_none()
        };
        if s.flags(t).contains(TypeFlags::INTERSECTION) {
            return s.members_of_union(t).iter().any(|&m| is_decl(m));
        }
        is_decl(t)
    }

    /// The property `name` of `t` (after apparent type), including `Object`/`Function`
    /// members.
    pub fn property_of_type(&mut self, t: TypeId, name: &str) -> Option<SymId> {
        let n = self.store().intern_str(name);
        self.property_of_type_id(t, n)
    }

    pub fn property_of_type_id(&mut self, t: TypeId, name: StrId) -> Option<SymId> {
        let t = self.apparent_type(t);
        let s = self.store();
        let f = s.flags(t);
        if f.contains(TypeFlags::UNION) {
            return self.union_property(t, name);
        }
        if f.contains(TypeFlags::INTERSECTION) {
            return self.intersection_property(t, name);
        }
        if !f.contains(TypeFlags::OBJECT) {
            return None;
        }
        let m = self.resolved_members(t);
        if let Some(p) = m.find(name) {
            return Some(p);
        }
        // Function-like types get `Function`'s members (`CallableFunction`'s or
        // `NewableFunction`'s under `strictBindCallApply`), everything gets `Object`'s.
        if !m.call.is_empty() || !m.construct.is_empty() {
            let strict = if !self.c.opts.strict_bind_call_apply {
                None
            } else if !m.call.is_empty() {
                self.global_type(GlobalType::CallableFunction)
            } else {
                self.global_type(GlobalType::NewableFunction)
            };
            for ft in [strict, self.global_type(GlobalType::Function)]
                .into_iter()
                .flatten()
            {
                if ft != t
                    && let Some(p) = self.resolved_members(ft).find(name)
                {
                    return Some(p);
                }
            }
        }
        // A const enum's object has no `Object` members (`Bebra.toString`).
        if let Some(ObjectKind::Anonymous { decl, kind, .. }) = s.object_of(t).map(|o| o.kind)
            && decl.0 == u32::MAX
            && kind == crate::declared::anon::NAMESPACE
            && self
                .c
                .sym_flags(SymId(decl.1))
                .intersects(SymbolFlags::CONST_ENUM)
        {
            return None;
        }
        if let Some(ot) = self.global_type(GlobalType::Object)
            && ot != t
            && let Some(p) = self.resolved_members(ot).find(name)
        {
            return Some(p);
        }
        None
    }

    /// Own properties of an object type (no `Object` members), in declaration order.
    pub fn properties_of(&mut self, t: TypeId) -> Vec<(StrId, SymId)> {
        let t = self.apparent_type(t);
        let s = self.store();
        let f = s.flags(t);
        if f.contains(TypeFlags::OBJECT) {
            let m = self.resolved_members(t);
            return m
                .names
                .iter()
                .copied()
                .zip(m.props.iter().copied())
                .collect();
        }
        if f.contains(TypeFlags::INTERSECTION) {
            let mut out: Vec<(StrId, SymId)> = Vec::new();
            let mut seen = rustc_hash::FxHashSet::default();
            for m in s.members_of_union(t).to_vec() {
                for (n, _) in self.properties_of(m) {
                    if seen.insert(n)
                        && let Some(p) = self.intersection_property(t, n)
                    {
                        out.push((n, p));
                    }
                }
            }
            return out;
        }
        if f.contains(TypeFlags::UNION) {
            let members = s.members_of_union(t).to_vec();
            let Some(&first) = members.first() else {
                return Vec::new();
            };
            let mut out = Vec::new();
            for (n, _) in self.properties_of(first) {
                if let Some(p) = self.union_property(t, n) {
                    out.push((n, p));
                }
            }
            return out;
        }
        Vec::new()
    }

    /// A property present in every member of a union.
    fn union_property(&mut self, t: TypeId, name: StrId) -> Option<SymId> {
        if let Some(&r) = self.union_props.get(&(t, name)) {
            return r;
        }
        let members = self.store().members_of_union(t).to_vec();
        let mut origins = Vec::new();
        let mut types = Vec::new();
        let mut optional = false;
        let mut readonly = false;
        let mut partial = false;
        let mut literal_part = false;
        let mut write_partial = false;
        let mut flags = SymbolFlags::NONE;
        for m in members {
            let am = self.apparent_type(m);
            let fm = self.store().flags(am);
            if fm.intersects(TypeFlags::ANY) {
                types.push(am);
                continue;
            }
            match self.property_of_type_id(m, name) {
                Some(p) => {
                    let pf = self.c.sym_flags(p);
                    flags |=
                        pf & (SymbolFlags::PROPERTY | SymbolFlags::METHOD | SymbolFlags::ACCESSOR);
                    optional |= pf.intersects(SymbolFlags::OPTIONAL)
                        || self.c.sym_check_flags(p) & check_flags::OPTIONAL != 0;
                    readonly |= self.is_readonly_symbol(p);
                    let pt = self.type_of_symbol(p);
                    literal_part |= self.store().flags(pt).intersects(TypeFlags::UNIT);
                    types.push(pt);
                    origins.push(p);
                }
                None => {
                    let text = self.store().str(name).to_owned();
                    let idx = if text.parse::<f64>().is_ok() {
                        self.index_info_of(m, TypeId::NUMBER)
                    } else {
                        None
                    };
                    let idx = idx.or_else(|| self.index_info_of(m, TypeId::STRING));
                    match idx {
                        Some(i) if !text.starts_with("__@") => {
                            types.push(i.value);
                            readonly |= i.readonly;
                        }
                        // An object literal's type without the property (and no spread)
                        // reads it as `undefined`: `(o || {}).color` (tsc: checker.go,
                        // createUnionOrIntersectionProperty).
                        _ if self.store().object_of(m).is_some_and(|o| {
                            o.flags.contains(ObjectFlags::OBJECT_LITERAL)
                                && !o.flags.contains(ObjectFlags::CONTAINS_SPREAD)
                        }) =>
                        {
                            types.push(TypeId::UNDEFINED);
                            write_partial = true;
                        }
                        _ => {
                            partial = true;
                            break;
                        }
                    }
                }
            }
        }
        let r = if partial || origins.is_empty() || !self.union_origins_accessible(&origins) {
            None
        } else if origins.len() == 1 && types.len() == 1 {
            Some(origins[0])
        } else {
            let ty = self.union(&types);
            if optional {
                flags |= SymbolFlags::OPTIONAL;
            }
            let mut check = check_flags::SYNTHETIC_UNION;
            if readonly {
                check |= check_flags::READONLY;
            }
            if literal_part {
                check |= check_flags::HAS_LITERAL_PART;
            }
            if write_partial {
                check |= check_flags::WRITE_PARTIAL;
            }
            if types.windows(2).any(|w| w[0] != w[1]) {
                check |= check_flags::HAS_NON_UNIFORM_TYPE;
            }
            if optional {
                check |= check_flags::OPTIONAL;
            }
            let flags = if flags.is_empty() {
                SymbolFlags::PROPERTY
            } else {
                flags
            };
            Some(
                self.c.new_synth(
                    SynthSym::new(name, flags, check)
                        .with_origins(origins)
                        .with_type(ty),
                ),
            )
        };
        self.union_props.insert((t, name), r);
        r
    }

    /// Whether a union's property can be read through these member properties: when one is
    /// private or protected, all must share a declaration (`(Foo | (Foo & Bar))['foo']`),
    /// otherwise the union has no such property (`Public | Protected`).
    fn union_origins_accessible(&mut self, origins: &[SymId]) -> bool {
        if origins.len() < 2 {
            return true;
        }
        let mut sets: Vec<Vec<Loc>> = Vec::with_capacity(origins.len());
        let mut nonpublic = false;
        for &o in origins {
            let mut decls = Vec::new();
            self.property_decls(o, &mut decls, 0);
            for &d in &decls {
                nonpublic |= self
                    .node(d)
                    .modifiers()
                    .intersects(ts_r_ast::Modifiers::PRIVATE | ts_r_ast::Modifiers::PROTECTED);
            }
            sets.push(decls);
        }
        if !nonpublic {
            return true;
        }
        let (first, rest) = sets.split_first().expect("two or more origins");
        first.iter().any(|d| rest.iter().all(|s| s.contains(d)))
    }

    /// The declarations behind a property symbol (through synthetic properties' origins).
    fn property_decls(&self, p: SymId, out: &mut Vec<Loc>, depth: u32) {
        if depth > 16 {
            return;
        }
        match self.c.synth_sym(p) {
            Some(x) if !x.origins.is_empty() => {
                for &o in x.origins.iter() {
                    self.property_decls(o, out, depth + 1);
                }
            }
            Some(_) => {}
            None => {
                for d in self.c.decls(p) {
                    if !out.contains(&d) {
                        out.push(d);
                    }
                }
            }
        }
    }

    /// A property of any member of an intersection (types intersected).
    fn intersection_property(&mut self, t: TypeId, name: StrId) -> Option<SymId> {
        if let Some(&r) = self.union_props.get(&(t, name)) {
            return r;
        }
        let members = self.store().members_of_union(t).to_vec();
        let mut origins = Vec::new();
        let mut types = Vec::new();
        let mut all_optional = true;
        let mut readonly = true;
        let mut flags = SymbolFlags::NONE;
        for m in members {
            if let Some(p) = self.property_of_type_id(m, name) {
                let pf = self.c.sym_flags(p);
                let member = SymbolFlags::PROPERTY | SymbolFlags::METHOD | SymbolFlags::ACCESSOR;
                flags |= pf & member;
                // Only members vote on optionality: a global `var` of `typeof globalThis`
                // leaves `Window`'s optional `ontouchstart` optional.
                if pf.intersects(member) {
                    all_optional &= pf.intersects(SymbolFlags::OPTIONAL);
                }
                readonly &= self.is_readonly_symbol(p);
                types.push(self.type_of_symbol(p));
                origins.push(p);
            }
        }
        let r = match origins.len() {
            0 => None,
            1 => Some(origins[0]),
            _ => {
                let ty = self.intersection(&types);
                let mut f = if flags.is_empty() {
                    SymbolFlags::PROPERTY
                } else {
                    flags
                };
                if all_optional {
                    f |= SymbolFlags::OPTIONAL;
                }
                let mut check = check_flags::SYNTHETIC_INTERSECTION;
                if readonly {
                    check |= check_flags::READONLY;
                }
                Some(
                    self.c.new_synth(
                        SynthSym::new(name, f, check)
                            .with_origins(origins)
                            .with_type(ty),
                    ),
                )
            }
        };
        self.union_props.insert((t, name), r);
        r
    }

    /// Whether writes to a property are disallowed.
    pub fn is_readonly_symbol(&mut self, p: SymId) -> bool {
        if self.c.sym_check_flags(p) & check_flags::READONLY != 0 {
            return true;
        }
        if let Some(x) = self.c.synth_sym(p) {
            // Mapped type properties carry their final modifiers; a spread's copy of a
            // property is not readonly.
            if x.check & (check_flags::MAPPED | check_flags::OBJECT_LITERAL) != 0 {
                return false;
            }
            if x.origins.len() == 1 && x.origins[0] != p {
                return self.is_readonly_symbol(x.origins[0]);
            }
            return false;
        }
        let f = self.c.sym_flags(p);
        if self.c.sym_facts(p).intersects(SymbolFacts::READONLY) {
            return true;
        }
        if f.contains(SymbolFlags::GET_ACCESSOR) && !f.contains(SymbolFlags::SET_ACCESSOR) {
            return true;
        }
        if f.intersects(SymbolFlags::ENUM_MEMBER) {
            return true;
        }
        if f.intersects(SymbolFlags::BLOCK_SCOPED_VARIABLE) {
            // `const` exports of namespaces.
            if let Some(d) = self.c.first_decl(p) {
                return self.is_const_declaration(d);
            }
        }
        false
    }

    /// The `const`ness of a variable declaration (binding identifier).
    pub fn is_const_declaration(&self, d: Loc) -> bool {
        let parents = self.c.file(d.file).parents();
        let ast = self.ast(d.file);
        let mut cur = parents.parent(d.node);
        let mut guard = 0;
        while let Some(n) = cur {
            guard += 1;
            if guard > 64 {
                break;
            }
            match *ast.node(n) {
                Node::VarDecl { kind, .. } => {
                    return matches!(
                        kind,
                        ts_r_ast::VarKind::Const
                            | ts_r_ast::VarKind::Using
                            | ts_r_ast::VarKind::AwaitUsing
                    );
                }
                Node::VarDeclarator { .. }
                | Node::ObjectPattern { .. }
                | Node::ArrayPattern { .. }
                | Node::BindingProperty { .. }
                | Node::AssignPattern { .. }
                | Node::Rest { .. } => {}
                _ => return false,
            }
            cur = parents.parent(n);
        }
        false
    }

    /// The index info of `t` applicable to keys of type `key` (`string`, `number`,
    /// `symbol`); numeric keys fall back to the string index.
    pub fn index_info_of(&mut self, t: TypeId, key: TypeId) -> Option<IndexInfo> {
        let t = self.apparent_type(t);
        let s = self.store();
        let f = s.flags(t);
        if f.contains(TypeFlags::UNION) {
            let members = s.members_of_union(t).to_vec();
            let mut types = Vec::new();
            let mut readonly = false;
            for m in members {
                let i = self.index_info_of(m, key)?;
                types.push(i.value);
                readonly |= i.readonly;
            }
            let v = self.union(&types);
            return Some(IndexInfo {
                key,
                value: v,
                readonly,
                decl: None,
            });
        }
        if f.contains(TypeFlags::INTERSECTION) {
            let members = s.members_of_union(t).to_vec();
            let mut types = Vec::new();
            let mut readonly = true;
            for m in members {
                if let Some(i) = self.index_info_of(m, key) {
                    types.push(i.value);
                    readonly &= i.readonly;
                }
            }
            if types.is_empty() {
                return None;
            }
            let v = self.intersection(&types);
            return Some(IndexInfo {
                key,
                value: v,
                readonly,
                decl: None,
            });
        }
        if !f.contains(TypeFlags::OBJECT) {
            return None;
        }
        let m = self.resolved_members(t);
        if let Some(i) = m.index.iter().find(|i| i.key == key) {
            return Some(*i);
        }
        if key == TypeId::NUMBER
            && let Some(i) = m.index.iter().find(|i| i.key == TypeId::STRING)
        {
            return Some(*i);
        }
        // Template literal / pattern keys accept string keys loosely.
        None
    }

    /// The index signatures of `t` a pattern key (`` `foo-${string}` ``) is assignable to,
    /// their value types intersected (every member of a union, some member of an
    /// intersection); `string` signatures included.
    pub(crate) fn pattern_key_index_info(&mut self, t: TypeId, key: TypeId) -> Option<IndexInfo> {
        let t = self.apparent_type(t);
        let f = self.store().flags(t);
        let mut types = Vec::new();
        let mut readonly = f.contains(TypeFlags::INTERSECTION);
        if f.intersects(TypeFlags::UNION | TypeFlags::INTERSECTION) {
            for m in self.store().members_of_union(t).to_vec() {
                match self.pattern_key_index_info(m, key) {
                    Some(i) => {
                        types.push(i.value);
                        if f.contains(TypeFlags::UNION) {
                            readonly |= i.readonly;
                        } else {
                            readonly &= i.readonly;
                        }
                    }
                    None if f.contains(TypeFlags::UNION) => return None,
                    None => {}
                }
            }
        } else if f.contains(TypeFlags::OBJECT) {
            readonly = true;
            let m = self.resolved_members(t);
            for &i in m.index.iter() {
                if i.key != TypeId::NUMBER
                    && i.key != TypeId::SYMBOL
                    && self.is_assignable(key, i.key)
                {
                    types.push(i.value);
                    readonly &= i.readonly;
                }
            }
        }
        if types.is_empty() {
            return None;
        }
        let value = if f.contains(TypeFlags::UNION) {
            self.union(&types)
        } else {
            self.intersection(&types)
        };
        Some(IndexInfo {
            key,
            value,
            readonly,
            decl: None,
        })
    }

    /// Call or construct signatures of a type.
    pub fn signatures_of(&mut self, t: TypeId, kind: SigKind) -> Vec<SigId> {
        let t = self.apparent_type(t);
        let s = self.store();
        let f = s.flags(t);
        if f.contains(TypeFlags::OBJECT) {
            let m = self.resolved_members(t);
            return match kind {
                SigKind::Call => m.call.to_vec(),
                SigKind::Construct => m.construct.to_vec(),
            };
        }
        if f.contains(TypeFlags::INTERSECTION) {
            let members = s.members_of_union(t).to_vec();
            if kind == SigKind::Construct {
                return self.intersection_construct_signatures(&members);
            }
            let mut out = Vec::new();
            for m in members {
                out.extend(self.signatures_of(m, kind));
            }
            return out;
        }
        if f.contains(TypeFlags::UNION) {
            return self.union_signatures(t, kind);
        }
        Vec::new()
    }

    /// Signatures of a union: when every member has exactly one signature (or they share
    /// identical signature lists), a combined signature with intersected parameters.
    fn union_signatures(&mut self, t: TypeId, kind: SigKind) -> Vec<SigId> {
        let members = self.store().members_of_union(t).to_vec();
        let mut lists: Vec<Vec<SigId>> = Vec::new();
        for m in &members {
            let sigs = self.signatures_of(*m, kind);
            if sigs.is_empty() {
                return Vec::new();
            }
            lists.push(sigs);
        }
        self.union_signatures_of_lists(lists, kind)
    }

    /// [`Self::union_signatures`] over the members' signature lists, each non-empty (a JSX
    /// tag combines each member's construct or else call signatures).
    pub(crate) fn union_signatures_of_lists(
        &mut self,
        lists: Vec<Vec<SigId>>,
        kind: SigKind,
    ) -> Vec<SigId> {
        if lists.is_empty() || lists.iter().any(Vec::is_empty) {
            return Vec::new();
        }
        if lists.windows(2).all(|w| w[0] == w[1]) {
            return lists[0].clone();
        }
        let generic = lists
            .iter()
            .flatten()
            .any(|&s| self.store().sig(s).type_params != ListId::EMPTY);
        // A member signature whose parameters every other member accepts (exactly, else
        // partially) stands for the union (`F1 | F5` with `F5 = (a, b)` takes `(a, b)`).
        let matched = self.matching_union_signatures(&lists, kind);
        if !matched.is_empty() {
            return matched;
        }
        if !lists.iter().all(|l| l.len() == 1) || generic {
            if generic {
                return self.combined_generic_union_signature(&lists, kind);
            }
            // No signature is in every member, and only one member has overloads: each
            // overload combines with the other members' single signatures
            // (`((a: string) => void) | { (a: number): 1; (a: boolean): 2 }`).
            let multi: Vec<usize> = (0..lists.len()).filter(|&i| lists[i].len() > 1).collect();
            let [mi] = multi[..] else {
                return Vec::new();
            };
            let mut out = Vec::new();
            for k in 0..lists[mi].len() {
                let sigs: Vec<SigId> = lists
                    .iter()
                    .enumerate()
                    .map(|(i, l)| if i == mi { l[k] } else { l[0] })
                    .collect();
                out.push(self.combined_union_signature(&sigs, kind, ListId::EMPTY));
            }
            return out;
        }
        let sigs: Vec<SigId> = lists.into_iter().map(|l| l[0]).collect();
        vec![self.combined_union_signature(&sigs, kind, ListId::EMPTY)]
    }

    /// One signature per union member combined: intersected parameters, the union of the
    /// return types.
    fn combined_union_signature(
        &mut self,
        sigs: &[SigId],
        kind: SigKind,
        type_params: ListId,
    ) -> SigId {
        let max_params = sigs
            .iter()
            .map(|&s| self.store().sig(s).params.len())
            .max()
            .unwrap_or(0);
        // Every member has a rest parameter at the same position: the rest types intersect
        // (`(...a: [string]) => R | (...a: [number]) => R` takes `...a: never`).
        let all_rest = max_params > 0
            && sigs.iter().all(|&s| {
                let d = self.store().sig(s);
                d.has_rest() && d.params.len() == max_params
            });
        // Some member has a rest parameter, not all at one position: fold the members pair
        // by pair the way tsc does.
        if !all_rest
            && sigs.iter().any(|&s| self.store().sig(s).has_rest())
            && let Some(folded) = self.fold_union_parameters(sigs)
        {
            return self.finish_combined_union_signature(sigs, kind, type_params, folded);
        }
        let mut params = Vec::with_capacity(max_params);
        for i in 0..max_params {
            let mut types = Vec::new();
            let mut name = None;
            for &s in sigs {
                let sd = self.store().sig(s);
                if all_rest && i + 1 == max_params {
                    let p = &sd.params[i];
                    name.get_or_insert(p.name);
                    types.push(p.ty);
                } else if let Some(p) = sd.params.get(i) {
                    name.get_or_insert(p.name);
                    // A rest parameter combines with the others' parameters at its
                    // position (tuple rests keep each position's type).
                    if sd.has_rest() && i + 1 == sd.params.len() {
                        if let Some(ty) = self.param_type_at(s, i) {
                            types.push(ty);
                        }
                    } else {
                        types.push(p.ty);
                    }
                } else if sd.has_rest()
                    && let Some(ty) = self.param_type_at(s, i)
                {
                    types.push(ty);
                }
            }
            // The same type in every member stays itself (and keeps its alias).
            types.dedup();
            let ty = match types[..] {
                [one] => one,
                _ => self.intersection(&types),
            };
            params.push(Param {
                name: name.unwrap_or(StrId(0)),
                ty,
                optional: false,
                sym: SymId::NONE,
            });
        }
        let min = sigs
            .iter()
            .map(|&s| self.store().sig(s).min_args)
            .max()
            .unwrap_or(0);
        for (i, p) in params.iter_mut().enumerate() {
            p.optional = i as u32 >= min;
        }
        if all_rest && let Some(last) = params.last_mut() {
            last.optional = false;
        }
        self.finish_combined_union_signature(sigs, kind, type_params, (params, min, all_rest))
    }

    /// A member signature's parameter positions for combining: names and types of the
    /// fixed positions (a rest of a fixed-length tuple spread), the name and element type of
    /// a remaining rest parameter, and the minimum argument count. `None` for rests this
    /// does not spread (variadic tuples, generic rests).
    fn union_member_positions(&mut self, s: SigId) -> Option<UnionPositions> {
        let d = self.store().sig(s);
        let min = d.min_args;
        let has_rest = d.has_rest();
        let params: Vec<(StrId, TypeId)> = d.params.iter().map(|p| (p.name, p.ty)).collect();
        if !has_rest {
            return Some(UnionPositions {
                fixed: params,
                rest: None,
                min,
            });
        }
        let (&(rest_name, rest_ty), head) = params.split_last()?;
        let mut fixed = head.to_vec();
        let mut rest = None;
        if let Some(shape) = self.store().tuple_shape_of(rest_ty) {
            let shape = shape.clone();
            if shape
                .elements
                .iter()
                .any(|e| e.kind == TupleElement::VARIADIC)
            {
                return None;
            }
            for (k, e) in shape.elements.iter().enumerate() {
                let ty = self.param_type_at(s, head.len() + k)?;
                if e.kind == TupleElement::REST {
                    rest = Some((rest_name, ty));
                } else {
                    fixed.push((rest_name, ty));
                }
            }
        } else {
            let el = self.element_type_of_array_like(rest_ty)?;
            rest = Some((rest_name, el));
        }
        Some(UnionPositions { fixed, rest, min })
    }

    /// The parameters of a union of signatures some of which have rest parameters (tsc:
    /// `combineUnionOrIntersectionParameters` in `checker.go`), folded member by member.
    /// Each position intersects both sides' types (a missing one is ignored); the longer
    /// side's rest stays a rest, and a rest only on the shorter side becomes an extra
    /// `...args` after the longer side's positions. A position is optional past both
    /// minimum counts; differing names become `arg<i>`.
    fn fold_union_parameters(&mut self, sigs: &[SigId]) -> Option<(Vec<Param>, u32, bool)> {
        let (&first, rest) = sigs.split_first()?;
        let mut acc = self.union_member_positions(first)?;
        for &s in rest {
            let right = self.union_member_positions(s)?;
            let (lc, rc) = (acc.count(), right.count());
            let (long, short) = if lc >= rc {
                (&acc, &right)
            } else {
                (&right, &acc)
            };
            let long_count = long.count();
            let either_rest = acc.rest.is_some() || right.rest.is_some();
            let extra = either_rest && long.rest.is_none();
            let mut fixed = Vec::with_capacity(long_count);
            let mut new_rest = None;
            for i in 0..long_count {
                let lt = long.type_at(i)?;
                let ty = match short.type_at(i) {
                    Some(st) if st != lt => self.intersection(&[lt, st]),
                    _ => lt,
                };
                let name = match (acc.name_at(i), right.name_at(i)) {
                    (Some(a), Some(b)) if a == b => a,
                    (Some(a), None) | (None, Some(a)) => a,
                    _ => self.store().intern_str(&format!("arg{i}")),
                };
                if either_rest && !extra && i + 1 == long_count {
                    new_rest = Some((name, ty));
                } else {
                    fixed.push((name, ty));
                }
            }
            if extra {
                let ty = short.type_at(long_count)?;
                new_rest = Some((self.store().intern_str("args"), ty));
            }
            acc = UnionPositions {
                min: acc.min.max(right.min),
                fixed,
                rest: new_rest,
            };
        }
        let min = acc.min;
        let mut params: Vec<Param> = acc
            .fixed
            .iter()
            .enumerate()
            .map(|(i, &(name, ty))| Param {
                name,
                ty,
                optional: i as u32 >= min,
                sym: SymId::NONE,
            })
            .collect();
        let has_rest = acc.rest.is_some();
        if let Some((name, el)) = acc.rest {
            let ty = self.create_array(el, false);
            params.push(Param {
                name,
                ty,
                optional: false,
                sym: SymId::NONE,
            });
        }
        Some((params, min, has_rest))
    }

    /// The combined signature of a union's members from its parameters: the union of the
    /// return types, the members' predicates and their intersected `this` types.
    fn finish_combined_union_signature(
        &mut self,
        sigs: &[SigId],
        kind: SigKind,
        type_params: ListId,
        (params, min, has_rest): (Vec<Param>, u32, bool),
    ) -> SigId {
        let rets: Vec<TypeId> = sigs.iter().map(|&s| self.return_type_of(s)).collect();
        let ret = self.union(&rets);
        let mut flags = if kind == SigKind::Construct {
            SigFlags::CONSTRUCT
        } else {
            SigFlags::NONE
        } | SigFlags::SYNTHETIC;
        if has_rest {
            flags |= SigFlags::HAS_REST;
        }
        // Combined member signatures keep an abstract member's flag (tsc:
        // combineUnionOrIntersectionMemberSignatures propagates it).
        if sigs
            .iter()
            .any(|&s| self.store().sig(s).flags.contains(SigFlags::ABSTRACT))
        {
            flags |= SigFlags::ABSTRACT;
        }
        let predicate = if kind == SigKind::Call {
            self.union_type_predicate(sigs)
        } else {
            None
        };
        // The members' declared `this` types intersect (`(this: A) => void | (this: B) =>
        // void` needs `A & B`; `this: void` with `this: number` needs `never`).
        let mut this_types: Vec<TypeId> = Vec::new();
        for &s in sigs {
            if let Some(t) = self.store().sig(s).this_type
                && !this_types.contains(&t)
            {
                this_types.push(t);
            }
        }
        let this_type = match this_types[..] {
            [] => None,
            [one] => Some(one),
            _ => Some(self.intersection(&this_types)),
        };
        let sig = Signature::new(
            flags,
            self.store().sig(sigs[0]).decl,
            type_params,
            params,
            this_type,
            min,
            Some(ret),
        )
        .with_predicate(predicate);
        self.store().add_signature(sig)
    }

    /// The type predicate of a union's combined signature: every member's predicate of the
    /// same kind and parameter (a member returning `false` contributes nothing), narrowing
    /// to the union of their types.
    fn union_type_predicate(&mut self, sigs: &[SigId]) -> Option<TypePredicate> {
        let mut first: Option<TypePredicate> = None;
        let mut types = Vec::new();
        for &s in sigs {
            match self.predicate_of(s) {
                Some(p) => {
                    if let Some(f) = first
                        && (f.kind != p.kind || f.param_index != p.param_index)
                    {
                        return None;
                    }
                    first.get_or_insert(p);
                    types.push(p.ty?);
                }
                None if self.return_type_of(s) == TypeId::FALSE => {}
                None => return None,
            }
        }
        let p = first?;
        Some(TypePredicate {
            ty: Some(self.union(&types)),
            ..p
        })
    }

    /// A union whose members each have one signature, some generic: when the generic ones
    /// have the same type parameters (count and constraints, matched by position), they are
    /// renamed to the first one's and everything combines into one generic signature
    /// (`((v: any) => any) | (<T>(it: T) => T)`, `(A[] | B[]).map`).
    fn combined_generic_union_signature(
        &mut self,
        lists: &[Vec<SigId>],
        kind: SigKind,
    ) -> Vec<SigId> {
        if lists.iter().any(|l| l.len() != 1) {
            return Vec::new();
        }
        let mut sigs: Vec<SigId> = lists.iter().map(|l| l[0]).collect();
        let Some(first) = sigs
            .iter()
            .copied()
            .find(|&s| self.store().sig(s).type_params != ListId::EMPTY)
        else {
            return Vec::new();
        };
        let tp_list = self.store().sig(first).type_params;
        let tpa = self.store().list_items(tp_list).to_vec();
        for s in sigs.iter_mut() {
            let l = self.store().sig(*s).type_params;
            if l == ListId::EMPTY || *s == first {
                continue;
            }
            let tpb = self.store().list_items(l).to_vec();
            if tpb.len() != tpa.len() {
                return Vec::new();
            }
            let m = self.store().mapper(&tpb, &tpa);
            for (&x, &y) in tpa.iter().zip(&tpb) {
                let cx = self.type_param_constraint(x);
                let cy = self
                    .type_param_constraint(y)
                    .map(|c| self.instantiate(c, m));
                match (cx, cy) {
                    (None, None) => {}
                    (Some(cx), Some(cy)) if self.is_identical(cx, cy) => {}
                    _ => return Vec::new(),
                }
            }
            *s = self.instantiate_signature(*s, m);
        }
        vec![self.combined_union_signature(&sigs, kind, tp_list)]
    }

    /// Union signatures from overload lists or generic signatures: the signatures of the first
    /// member that are in every other member with identical parameters (and identical type
    /// parameters, matched by position), each returning the subtype-reduced union of the
    /// matches' return types
    /// (`Callable<"A"> | Callable<"B">`, `(<K extends "t">(k: K) => A[K]) | (<K ...> => B[K])`).
    fn matching_union_signatures(&mut self, lists: &[Vec<SigId>], kind: SigKind) -> Vec<SigId> {
        if lists.iter().any(|l| l.len() > 16) {
            return Vec::new();
        }
        let mut out: Vec<SigId> = Vec::new();
        for (i, list) in lists.iter().enumerate() {
            for &a in list {
                // A parameter list already in the result is not added again.
                if out.iter().any(|&o| self.signature_matching(o, a).is_some()) {
                    continue;
                }
                let mut rets = Vec::with_capacity(lists.len());
                let mut matched = Vec::with_capacity(lists.len());
                let mut all = true;
                for (j, other) in lists.iter().enumerate() {
                    if j == i {
                        rets.push(self.return_type_of(a));
                        matched.push(a);
                        continue;
                    }
                    // An identical signature, else one that takes no more required
                    // arguments and whose parameters `a`'s are subtypes of
                    // (`end(cb?)` matches `end(chunk, cb?)`'s missing positions).
                    let found = match other
                        .iter()
                        .find_map(|&b| self.signature_matching(a, b).map(|r| (r, b)))
                    {
                        Some(r) => Some(r),
                        None => other
                            .iter()
                            .find_map(|&b| self.signature_partially_matching(a, b).map(|r| (r, b))),
                    };
                    match found {
                        Some((r, b)) => {
                            rets.push(r);
                            if !matched.contains(&b) {
                                matched.push(b);
                            }
                        }
                        None => {
                            all = false;
                            break;
                        }
                    }
                }
                // Signatures missing from some member drop out (`on("data", ...)` overloads
                // that differ between `IncomingMessage` and `Http2ServerRequest` leave the
                // shared `(event: string | symbol, ...)` one).
                if !all {
                    continue;
                }
                // (Generic returns stay a plain union: `A[K] | B[K]`.)
                let ret = if self.store().sig(a).type_params == ListId::EMPTY {
                    self.union_subtype(&rets)
                } else {
                    self.union(&rets)
                };
                // Several matches need every one's declared `this` (`(this: void) | (this:
                // number) | (i: number)` called through `(i: number)` needs `never`).
                // (A generic signature only matches identical ones and stands alone.)
                let generic = self.store().sig(a).type_params != ListId::EMPTY;
                let mut this_type = self.store().sig(a).this_type;
                let mut predicate = None;
                let combined = matched.len() > 1 && !generic;
                if combined {
                    if kind == SigKind::Call {
                        predicate = Some(self.union_type_predicate(&matched));
                    }
                    let mut thises: Vec<TypeId> = Vec::new();
                    for &m in &matched {
                        if let Some(t) = self.store().sig(m).this_type
                            && !thises.contains(&t)
                        {
                            thises.push(t);
                        }
                    }
                    this_type = match thises[..] {
                        [] => None,
                        [one] => Some(one),
                        _ => Some(self.intersection(&thises)),
                    };
                }
                let d = self.store().sig(a);
                let mut flags = d.flags;
                // A `new` through the union is abstract when any signature it stands for is
                // (tsc: `someSignature` looks into a union signature's members).
                if combined
                    && matched
                        .iter()
                        .any(|&m| self.store().sig(m).flags.contains(SigFlags::ABSTRACT))
                {
                    flags |= SigFlags::UNION_OF_ABSTRACT;
                }
                let d = self.store().sig(a);
                let sig = Signature::new(
                    flags,
                    d.decl,
                    d.type_params,
                    d.params.to_vec(),
                    this_type,
                    d.min_args,
                    Some(ret),
                );
                let sig = match predicate {
                    Some(p) => sig.with_predicate(p),
                    None => sig,
                };
                out.push(self.store().add_signature(sig));
            }
        }
        out
    }

    /// The return type of non-generic `b` when it takes no more required arguments than
    /// non-generic `a` and each of `a`'s parameter types is a subtype of `b`'s at that
    /// position (positions `b` lacks are ignored).
    fn signature_partially_matching(&mut self, a: SigId, b: SigId) -> Option<TypeId> {
        let s = self.store();
        let (da, db) = (s.sig(a), s.sig(b));
        if da.type_params != ListId::EMPTY || db.type_params != ListId::EMPTY {
            return None;
        }
        let (n, a_min) = self.arity_for_matching(a);
        let (_, b_min) = self.arity_for_matching(b);
        if b_min > a_min {
            return None;
        }
        for i in 0..n {
            let Some(x) = self.param_type_at(a, i) else {
                break;
            };
            let Some(y) = self.param_type_at(b, i) else {
                continue;
            };
            if !self.is_subtype(x, y) {
                return None;
            }
        }
        if let (Some(x), Some(y)) = (self.store().sig(a).this_type, self.store().sig(b).this_type)
            && !self.is_subtype(y, x)
        {
            return None;
        }
        Some(self.return_type_of(b))
    }

    /// Parameter count and minimum argument count with a tuple rest parameter spread out
    /// (`(...args: [string])` takes one argument, `(...args: [])` none).
    fn arity_for_matching(&mut self, sig: SigId) -> (usize, usize) {
        let d = self.store().sig(sig);
        let n = d.params.len();
        let min = self.effective_min_args(sig);
        if !d.has_rest() {
            return (n, min);
        }
        let rest = d.params[n - 1].ty;
        let Some(shape) = self.store().tuple_shape_of(rest) else {
            return (n, min);
        };
        let fixed = n - 1;
        let count = fixed + shape.fixed_length() + usize::from(shape.has_rest());
        let required = shape
            .elements
            .iter()
            .take_while(|e| e.kind == TupleElement::REQUIRED)
            .count();
        let min = if min >= fixed { fixed + required } else { min };
        (count, min)
    }

    /// The return type of `b` (with its type parameters renamed to `a`'s) when `b` has the
    /// same type parameters and parameters as `a`.
    fn signature_matching(&mut self, a: SigId, b: SigId) -> Option<TypeId> {
        let s = self.store();
        let (da, db) = (s.sig(a), s.sig(b));
        if da.params.len() != db.params.len()
            || da.min_args != db.min_args
            || da.has_rest() != db.has_rest()
            || da.this_type.is_some() != db.this_type.is_some()
        {
            return None;
        }
        let tpa = s.list_items(da.type_params).to_vec();
        let tpb = s.list_items(db.type_params).to_vec();
        if tpa.len() != tpb.len() {
            return None;
        }
        let b = if tpa.is_empty() {
            b
        } else {
            let m = s.mapper(&tpb, &tpa);
            for (&x, &y) in tpa.iter().zip(&tpb) {
                let cx = self.type_param_constraint(x);
                let cy = self
                    .type_param_constraint(y)
                    .map(|c| self.instantiate(c, m));
                match (cx, cy) {
                    (None, None) => {}
                    (Some(cx), Some(cy)) if self.is_identical(cx, cy) => {}
                    _ => return None,
                }
            }
            self.instantiate_signature(b, m)
        };
        let pa: Vec<TypeId> = self.store().sig(a).params.iter().map(|p| p.ty).collect();
        let pb: Vec<TypeId> = self.store().sig(b).params.iter().map(|p| p.ty).collect();
        for (x, y) in pa.into_iter().zip(pb) {
            if !self.is_identical(x, y) {
                return None;
            }
        }
        if let (Some(x), Some(y)) = (self.store().sig(a).this_type, self.store().sig(b).this_type)
            && !self.is_identical(x, y)
        {
            return None;
        }
        Some(self.return_type_of(b))
    }

    /// The element type of an array or tuple type (union of element types for tuples).
    pub fn element_type_of_array_like(&mut self, t: TypeId) -> Option<TypeId> {
        // An intersection of arrays: the intersection of their element types (observed:
        // iterating `A[] & B[]` gives `A & B`).
        if self.store().flags(t).contains(TypeFlags::INTERSECTION) {
            let ms = self.store().members_of_union(t).to_vec();
            let mut els = Vec::new();
            for m in ms {
                if self.c.is_array_like_type(m)
                    && let Some(e) = self.element_type_of_array_like(m)
                {
                    els.push(e);
                }
            }
            if els.len() > 1 {
                return Some(self.intersection(&els));
            }
        }
        if self.c.is_array_like_type(t) && self.store().tuple_shape_of(t).is_none() {
            return self.c.type_args_of(t).first().copied();
        }
        if let Some(shape) = self.store().tuple_shape_of(t) {
            let shape = shape.clone();
            let args = self.c.type_args_of_tuple(t).to_vec();
            let mut types = Vec::new();
            for (i, el) in shape.elements.iter().enumerate() {
                let a = args.get(i).copied().unwrap_or(TypeId::UNRESOLVED);
                if el.kind == TupleElement::VARIADIC {
                    types.push(
                        self.element_type_of_array_like(a)
                            .unwrap_or(TypeId::UNRESOLVED),
                    );
                } else if el.kind == TupleElement::OPTIONAL && a != TypeId::UNRESOLVED {
                    // An optional element adds `undefined` (`[number, string?]` is an
                    // array of `string | number | undefined`).
                    types.push(self.add_optionality(a));
                } else {
                    types.push(a);
                }
            }
            return Some(self.union(&types));
        }
        let s = self.store();
        if s.flags(t).intersects(TypeFlags::ANY) {
            return Some(t);
        }
        None
    }

    /// `keyof T`.
    pub fn keyof_type(&mut self, t: TypeId) -> TypeId {
        let s = self.store();
        let f = s.flags(t);
        if f.intersects(TypeFlags::ANY) {
            return if t == TypeId::UNRESOLVED {
                t
            } else {
                self.keyof_constraint()
            };
        }
        if f.contains(TypeFlags::NEVER) {
            return self.keyof_constraint();
        }
        if f.contains(TypeFlags::UNKNOWN) {
            return TypeId::NEVER;
        }
        // `keyof` a mapped type without `as` is its constraint (also while generic).
        if s.object_of(t)
            .is_some_and(|o| matches!(o.kind, ObjectKind::Mapped { .. }))
        {
            return match self.keyof_mapped(t) {
                Some(k) => k,
                None => s.index_type(t),
            };
        }
        // `keyof (A & B)` is `keyof A | keyof B`, generic or not, except an intersection of
        // a generic type with `{}` (tsc: checker.go, getIndexTypeEx, shouldDeferIndexType).
        if f.contains(TypeFlags::INTERSECTION)
            && self.is_generic_type(t)
            && !s
                .members_of_union(t)
                .iter()
                .any(|&m| self.is_empty_anonymous_object(m))
        {
            let ms = s.members_of_union(t).to_vec();
            let parts: Vec<TypeId> = ms.iter().map(|&m| self.keyof_type(m)).collect();
            return self.union(&parts);
        }
        if self.is_generic_type(t) {
            return s.index_type(t);
        }
        if f.contains(TypeFlags::UNION) {
            let ms = s.members_of_union(t).to_vec();
            let parts: Vec<TypeId> = ms.iter().map(|&m| self.keyof_type(m)).collect();
            return self.intersection(&parts);
        }
        if f.contains(TypeFlags::INTERSECTION) {
            let ms = s.members_of_union(t).to_vec();
            let parts: Vec<TypeId> = ms.iter().map(|&m| self.keyof_type(m)).collect();
            return self.union(&parts);
        }
        let at = self.apparent_type(t);
        let mut keys: Vec<TypeId> = Vec::new();
        let m = self.resolved_members(at);
        let index = m.index.to_vec();
        let names: Vec<StrId> = m.names.to_vec();
        let props: Vec<SymId> = m.props.to_vec();
        for i in &index {
            if i.key == TypeId::STRING {
                keys.push(TypeId::STRING);
                keys.push(TypeId::NUMBER);
            } else {
                keys.push(i.key);
            }
        }
        for (n, p) in names.into_iter().zip(props) {
            // Private and protected members are not keys (tsc: `getLiteralTypeFromProperty`).
            if self.is_non_public_property(p) {
                continue;
            }
            let text = self.store().str(n);
            if text.starts_with('#') {
                continue;
            }
            if let Some(rest) = text.strip_prefix("__@") {
                let _ = rest;
                // Unique symbol keys.
                if let Some(id) = text.rsplit('@').next().and_then(|x| x.parse::<u32>().ok()) {
                    keys.push(self.store().unique_symbol(SymId(id)));
                }
                continue;
            }
            // A numeric name declared as a number (`1: x`, `[2]: x`, `[E.A]: x`) is that
            // number (or enum member) type.
            if crate::expr::is_numeric_name(text)
                && let Some(k) = self.numeric_key_type(p)
            {
                keys.push(k);
                continue;
            }
            keys.push(self.store().string_literal(text, false));
        }
        let u = self.union(&keys);
        // The keys of a class, an interface, a reference or an aliased type print as
        // `keyof A` (a union whose origin is the index type).
        let st = self.store();
        let named = st.alias_of(t).is_some()
            || st.object_of(t).is_some_and(|o| {
                matches!(o.kind, ObjectKind::Reference { .. })
                    || o.flags
                        .intersects(ObjectFlags::CLASS | ObjectFlags::INTERFACE)
            });
        if named
            && st.flags(u).contains(TypeFlags::UNION)
            && st.alias_of(u).is_none()
            && st.union_origin(u).is_none()
            && !st.traits(u).contains(TypeTraits::ENUM_UNION)
        {
            let ms = st.members_of_union(u).to_vec();
            let ix = st.index_type(t);
            return st.union_with_origin(&ms, &[ix]);
        }
        u
    }

    /// Whether a property is declared `private` or `protected` (also a synthesized one
    /// carrying those check flags).
    pub(crate) fn is_non_public_property(&mut self, p: SymId) -> bool {
        self.c.sym_check_flags(p) & (check_flags::PRIVATE | check_flags::PROTECTED) != 0
            || self.c.decls(p).into_iter().any(|d| {
                self.node(d)
                    .modifiers()
                    .intersects(ts_r_ast::Modifiers::PRIVATE | ts_r_ast::Modifiers::PROTECTED)
            })
    }

    /// The key type of a property declared with a numeric literal or a computed numeric
    /// (literal or enum member) name.
    pub(crate) fn numeric_key_type(&mut self, p: SymId) -> Option<TypeId> {
        let d = self.c.decls(p).first().copied()?;
        let key = match *self.node(d) {
            Node::PropertySignature { key, .. }
            | Node::MethodSignature { key, .. }
            | Node::PropertyDef { key, .. }
            | Node::MethodDef { key, .. }
            | Node::Property { key, .. } => key,
            _ => return None,
        };
        let ast = self.ast(d.file);
        match *ast.node(key) {
            Node::NumberLit { value } => Some(self.store().number_literal(value.get(), false)),
            Node::ComputedKey { expr } => {
                let t = self.quietly(|ck| ck.check_expr(Loc::new(d.file, expr)));
                let st = self.store();
                if st.flags(t).contains(TypeFlags::NUMBER_LITERAL) {
                    Some(st.regular(t))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// `obj[index]` where symbol keys without a symbol index signature read `obj`'s string
    /// index signature; `None` when `obj` has no string index signature or another key fails.
    fn indexed_access_symbol_as_string(&mut self, obj: TypeId, index: TypeId) -> Option<TypeId> {
        let si = self
            .index_info_of(obj, TypeId::STRING)
            .filter(|i| i.key == TypeId::STRING)?;
        let keys: Vec<TypeId> = if self.store().flags(index).contains(TypeFlags::UNION) {
            self.store().members_of_union(index).to_vec()
        } else {
            vec![index]
        };
        let mut parts = Vec::with_capacity(keys.len());
        for k in keys {
            if self.store().flags(k).intersects(TypeFlags::ES_SYMBOL_LIKE)
                && self.index_info_of(obj, TypeId::SYMBOL).is_none()
            {
                parts.push(si.value);
            } else {
                parts.push(self.indexed_access_type(obj, k, None)?);
            }
        }
        Some(self.union(&parts))
    }

    /// `T[K]` for a non-generic object and key; `None` when the access is invalid.
    pub fn indexed_access_type(
        &mut self,
        obj: TypeId,
        index: TypeId,
        _at: Option<Loc>,
    ) -> Option<TypeId> {
        let s = self.store();
        if s.flags(obj).intersects(TypeFlags::ANY) || s.flags(index).intersects(TypeFlags::ANY) {
            // A named error type indexed is itself (`Problem['x']` prints as `Problem`).
            if index != TypeId::UNRESOLVED && s.error_name(obj).is_some() {
                return Some(obj);
            }
            // So is the plain error type (`Parameters<E>[1]` of an error `E`,
            // `error_type_inference`).
            if obj == TypeId::ERROR && index != TypeId::UNRESOLVED {
                return Some(TypeId::ERROR);
            }
            return Some(
                if obj == TypeId::UNRESOLVED || index == TypeId::UNRESOLVED {
                    TypeId::UNRESOLVED
                } else {
                    TypeId::ANY
                },
            );
        }
        if self.is_generic_type(obj) || self.is_generic_type(index) {
            // Indexing a tuple/array type parameter by `number` etc. stays deferred.
            return Some(s.indexed_access(obj, index));
        }
        if s.flags(index).contains(TypeFlags::UNION) {
            let ms = s.members_of_union(index).to_vec();
            let mut parts = Vec::new();
            for m in ms {
                parts.push(self.indexed_access_type(obj, m, None)?);
            }
            return Some(self.union(&parts));
        }
        if let Some(name) = self.property_name_of_type(index) {
            if let Some(p) = self.property_of_type(obj, &name) {
                return Some(self.type_of_symbol(p));
            }
            // A tuple read past its fixed elements: the rest elements, or `undefined`
            // (`Parameters<F>[3]` of a two-parameter `F`).
            if let Some(shape) = s.tuple_shape_of(obj)
                && let Some(v) = s.number_value(index)
                && v >= 0.0
                && v.fract() == 0.0
            {
                if !shape.has_rest() {
                    return Some(TypeId::UNDEFINED);
                }
                let fixed = shape.fixed_length();
                let n = shape.elements.len();
                let args = self.c.type_args_of(obj);
                if args.len() >= n && fixed < n {
                    let rest = args[fixed..n].to_vec();
                    return Some(self.union(&rest));
                }
            }
            // A numeric name (`1`, `"1"`) reads a number index signature.
            if (s.flags(index).intersects(TypeFlags::NUMBER_LITERAL)
                || crate::expr::is_numeric_name(&name))
                && let Some(i) = self.index_info_of(obj, TypeId::NUMBER)
            {
                return Some(i.value);
            }
            if s.flags(index)
                .intersects(TypeFlags::STRING_LITERAL | TypeFlags::NUMBER_LITERAL)
                && let Some(i) = self.index_info_of(obj, TypeId::STRING)
            {
                return Some(i.value);
            }
            return None;
        }
        let f = s.flags(index);
        if f.intersects(TypeFlags::NUMBER_LIKE) {
            if let Some(i) = self.index_info_of(obj, TypeId::NUMBER) {
                return Some(i.value);
            }
            if self.store().tuple_shape_of(obj).is_some() {
                return self.element_type_of_array_like(obj);
            }
            return None;
        }
        if f.intersects(TypeFlags::STRING_LIKE) {
            return self.index_info_of(obj, TypeId::STRING).map(|i| i.value);
        }
        if f.intersects(TypeFlags::ES_SYMBOL_LIKE) {
            return self.index_info_of(obj, TypeId::SYMBOL).map(|i| i.value);
        }
        None
    }

    /// Whether `t` is a readonly array or tuple.
    pub fn is_readonly_array_like(&mut self, t: TypeId) -> bool {
        if let Some(shape) = self.store().tuple_shape_of(t) {
            return shape.readonly;
        }
        let ro = self.global_type(GlobalType::ReadonlyArray);
        match self.store().object_of(t).map(|o| o.kind) {
            Some(ObjectKind::Reference { target, .. }) => Some(target) == ro,
            _ => false,
        }
    }

    /// Whether a type is or contains the `this` type marker of readonly modifiers
    /// (helper for `TypeOperatorKind`).
    pub fn type_operator_is_readonly(op: TypeOperatorKind) -> bool {
        op == TypeOperatorKind::Readonly
    }

    /// Whether a node is a parameter property declaration.
    pub fn is_parameter_property(&self, l: Loc) -> bool {
        matches!(*self.node(l), Node::Param { modifiers, .. } if modifiers.intersects(Modifiers::PUBLIC | Modifiers::PRIVATE | Modifiers::PROTECTED | Modifiers::READONLY | Modifiers::OVERRIDE))
    }

    pub fn traits_of(&self, t: TypeId) -> TypeTraits {
        self.store().traits(t)
    }

    pub fn object_flags(&self, t: TypeId) -> ObjectFlags {
        self.store()
            .object_of(t)
            .map_or(ObjectFlags::NONE, |o| o.flags)
    }

    pub fn node_id(&self, raw: u32) -> Option<NodeId> {
        NodeId::from_raw(raw)
    }
}

/// A binding pattern as tsc prints it in a signature: names only, no defaults
/// (`{ a = 1, b: [x] }` prints `{ a, b: [x] }`). Falls back to the source text.
fn pattern_display(ast: &ts_r_ast::Ast, n: NodeId, fallback: &str) -> String {
    let mut out = String::new();
    if write_pattern(ast, n, &mut out, 0) {
        out
    } else {
        fallback.to_owned()
    }
}

fn write_pattern(ast: &ts_r_ast::Ast, n: NodeId, out: &mut String, depth: u32) -> bool {
    if depth > 50 {
        return false;
    }
    match *ast.node(n) {
        Node::Ident { name } => out.push_str(ast.atom(name)),
        Node::ObjectPattern { props } => {
            let list = ast.list(props);
            if list.is_empty() {
                out.push_str("{}");
                return true;
            }
            out.push_str("{ ");
            for (i, &p) in list.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                match *ast.node(p) {
                    Node::BindingProperty { key, value, .. } => {
                        let k = match crate::nodes::key_text(ast, key) {
                            Some(k) => k,
                            // A computed entity name prints as written (`[foo.bar]: c`).
                            None => match *ast.node(key) {
                                Node::ComputedKey { expr }
                                    if crate::expr::is_entity_name_expression(ast, expr) =>
                                {
                                    format!("[{}]", crate::nodes::entity_text(ast, expr))
                                }
                                _ => return false,
                            },
                        };
                        let shorthand = match value {
                            None => true,
                            Some(v) => {
                                matches!(*ast.node(v), Node::Ident { name } if ast.atom(name) == k)
                            }
                        };
                        out.push_str(&k);
                        if !shorthand && let Some(v) = value {
                            out.push_str(": ");
                            if !write_pattern(ast, v, out, depth + 1) {
                                return false;
                            }
                        }
                    }
                    Node::Rest { arg } => {
                        out.push_str("...");
                        if !write_pattern(ast, arg, out, depth + 1) {
                            return false;
                        }
                    }
                    _ => return false,
                }
            }
            out.push_str(" }");
        }
        Node::ArrayPattern { elements } => {
            out.push('[');
            for (i, &e) in ast.list(elements).iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                if !write_pattern(ast, e, out, depth + 1) {
                    return false;
                }
            }
            out.push(']');
        }
        Node::Hole {} => {}
        Node::AssignPattern { left, .. } => return write_pattern(ast, left, out, depth + 1),
        Node::Rest { arg } => {
            out.push_str("...");
            return write_pattern(ast, arg, out, depth + 1);
        }
        _ => return false,
    }
    true
}
