//! Union and intersection construction, the canonical member order, literal widening and
//! type filtering helpers.
//!
//! Member order is computed from content (observed from tsc 7.0.2, see
//! `docs/design/0004-types-and-checker.md`): kind rank first, then literal value, symbol name,
//! declaration position and type arguments. It never depends on `TypeId` numbers.

use std::cmp::Ordering;

use ts_r_types::{
    AliasId, ObjectFlags, ObjectKind, StrId, SymId, TupleElement, TypeFlags, TypeId, TypeKind,
    TypeTraits,
};

use crate::checker::{Checker, Ck, Loc};
use crate::declared::anon;

/// How much a union is reduced.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reduction {
    /// Duplicates and fresh/regular literal twins only.
    None,
    /// Literals absorbed by their base primitive (`"a" | string` is `string`).
    Literal,
    /// Also remove members that are strict subtypes of other members.
    Subtype,
}

const MAX_UNION_SIZE: usize = 100_000;
const MAX_COMPARE_DEPTH: u32 = 12;

/// Unions of at least this many written members go through [`UnionCache`].
const CACHED_UNION_MIN: usize = 4;
/// Entries kept before [`UnionCache`] starts over.
const UNION_CACHE_MAX: usize = 1 << 16;

/// Results of [`Ck::union_with`] without subtype reduction, by the written member list,
/// reduction and alias. Building such a union is a pure function of those (it reads only the
/// interned store and the program's options), so entries stay valid for the whole program.
#[derive(Default)]
pub(crate) struct UnionCache {
    /// Key: the written members, then the reduction (0 none, 1 literal) and the alias.
    map: rustc_hash::FxHashMap<Box<[TypeId]>, TypeId>,
    key: Vec<TypeId>,
}

impl UnionCache {
    fn make_key(&mut self, types: &[TypeId], reduction: Reduction, alias: AliasId) {
        self.key.clear();
        self.key.extend_from_slice(types);
        self.key
            .push(TypeId(u32::from(reduction == Reduction::Literal)));
        self.key.push(TypeId(alias.0));
    }

    fn get(&mut self, types: &[TypeId], reduction: Reduction, alias: AliasId) -> Option<TypeId> {
        self.make_key(types, reduction, alias);
        self.map.get(self.key.as_slice()).copied()
    }

    fn put(&mut self, types: &[TypeId], reduction: Reduction, alias: AliasId, result: TypeId) {
        if self.map.len() >= UNION_CACHE_MAX {
            self.map.clear();
        }
        self.make_key(types, reduction, alias);
        self.map.insert(self.key.as_slice().into(), result);
    }
}

impl<'a> Checker<'a> {
    /// Kind rank of a union member (lower sorts first).
    pub fn type_rank(&self, t: TypeId) -> u8 {
        let s = &self.store;
        let f = s.flags(t);
        if t == TypeId::MISSING || f.contains(TypeFlags::UNDEFINED) {
            return 0;
        }
        if f.contains(TypeFlags::NULL) {
            return 1;
        }
        if f.contains(TypeFlags::VOID) {
            return 2;
        }
        if f.contains(TypeFlags::STRING) {
            return 3;
        }
        if f.contains(TypeFlags::NUMBER) {
            return 4;
        }
        if f.contains(TypeFlags::BIGINT) {
            return 5;
        }
        if f.contains(TypeFlags::ES_SYMBOL) {
            return 6;
        }
        if f.intersects(TypeFlags::ENUM_LITERAL | TypeFlags::ENUM) {
            return 12;
        }
        if f.contains(TypeFlags::STRING_LITERAL) {
            return 7;
        }
        if f.contains(TypeFlags::NUMBER_LITERAL) {
            return 8;
        }
        if f.contains(TypeFlags::BIGINT_LITERAL) {
            return 9;
        }
        if f.contains(TypeFlags::BOOLEAN_LITERAL) {
            return 10;
        }
        if f.contains(TypeFlags::UNIQUE_ES_SYMBOL) {
            return 11;
        }
        if f.contains(TypeFlags::NON_PRIMITIVE) {
            return 13;
        }
        if f.contains(TypeFlags::TYPE_PARAMETER) {
            return 14;
        }
        if f.contains(TypeFlags::OBJECT) {
            return 15;
        }
        if f.contains(TypeFlags::INTERSECTION) {
            return 16;
        }
        if f.contains(TypeFlags::INDEX) {
            return 17;
        }
        if f.contains(TypeFlags::INDEXED_ACCESS) {
            return 18;
        }
        if f.intersects(TypeFlags::CONDITIONAL | TypeFlags::SUBSTITUTION) {
            return 19;
        }
        if f.contains(TypeFlags::TEMPLATE_LITERAL) {
            return 20;
        }
        if f.contains(TypeFlags::STRING_MAPPING) {
            return 21;
        }
        if f.contains(TypeFlags::UNION) {
            return 22;
        }
        // any/unknown/never only appear transiently.
        23
    }

    /// Program-order position of a symbol's first declaration.
    pub fn decl_position(&self, s: SymId) -> (u32, u32) {
        match self.first_decl(s) {
            Some(l) => (l.file.0, self.span(l).start),
            None => (u32::MAX, s.0),
        }
    }

    /// Tuple shapes by content (shorter first), not by their ids, which depend on which
    /// thread made a shape first.
    fn compare_tuple_shapes(&self, a: TypeId, b: TypeId) -> Ordering {
        let s = &self.store;
        let (Some(x), Some(y)) = (s.tuple_shape_of(a), s.tuple_shape_of(b)) else {
            return Ordering::Equal;
        };
        let label = |e: &TupleElement| e.label.map(|l| s.str(l));
        (x.elements.len(), x.readonly)
            .cmp(&(y.elements.len(), y.readonly))
            .then_with(|| {
                x.elements
                    .iter()
                    .map(|e| e.kind)
                    .cmp(y.elements.iter().map(|e| e.kind))
            })
            .then_with(|| {
                x.elements
                    .iter()
                    .map(label)
                    .cmp(y.elements.iter().map(label))
            })
    }

    /// The group, name and position an object type sorts by: named types (declared,
    /// referenced or aliased) by name; then unnamed ones by declaration; then those with no
    /// declaration (the empty type literal, mapped types); tuples last.
    fn object_sort_key(&self, t: TypeId) -> (u8, &str, (u32, u32)) {
        let s = &self.store;
        let Some(o) = s.object_of(t) else {
            return (0, "", (0, 0));
        };
        // An aliased anonymous or mapped type sorts by its alias (`Abc | Router` for
        // `type Abc = { b: 1 }`).
        if matches!(
            o.kind,
            ObjectKind::Anonymous { .. } | ObjectKind::Mapped { .. }
        ) && let Some(a) = s.alias_of(t)
        {
            return (0, self.sym_name(a.sym), self.decl_position(a.sym));
        }
        match o.kind {
            ObjectKind::Declared { sym, .. } => (0, self.sym_name(sym), self.decl_position(sym)),
            ObjectKind::Reference { target, .. } => match s.object_of(target).map(|o| o.kind) {
                Some(ObjectKind::Declared { sym, .. }) => {
                    (0, self.sym_name(sym), self.decl_position(sym))
                }
                Some(ObjectKind::TupleTarget { .. }) => (3, "", (0, 0)),
                _ => (0, "", (0, 0)),
            },
            ObjectKind::TupleTarget { .. } => (3, "", (0, 0)),
            ObjectKind::Anonymous { decl, .. } => {
                if decl.0 == u32::MAX {
                    (1, "", self.decl_position(SymId(decl.1)))
                } else {
                    let file = ts_r_diagnostics::FileId(decl.0);
                    let Some(n) = ts_r_ast::NodeId::from_raw(decl.1) else {
                        return (1, "", (decl.0, 0));
                    };
                    let l = crate::checker::Loc::new(file, n);
                    // A written `{}` is one shared type with no declaration.
                    if let ts_r_ast::Node::TypeLiteral { members } = *self.node(l)
                        && members.is_empty()
                    {
                        return (2, "", (u32::MAX, t.0));
                    }
                    (1, "", (decl.0, self.span(l).start))
                }
            }
            ObjectKind::Literal { sym } if !sym.is_none() => (1, "", self.decl_position(sym)),
            ObjectKind::Literal { .. } | ObjectKind::Mapped { .. } => (2, "", (u32::MAX, t.0)),
        }
    }

    /// Canonical union order.
    pub fn compare_types(&self, a: TypeId, b: TypeId, depth: u32) -> Ordering {
        if a == b {
            return Ordering::Equal;
        }
        let (ra, rb) = (self.type_rank(a), self.type_rank(b));
        if ra != rb {
            return ra.cmp(&rb);
        }
        if depth > MAX_COMPARE_DEPTH {
            return a.0.cmp(&b.0);
        }
        let s = &self.store;
        let ord = match (s.kind(a), s.kind(b)) {
            (TypeKind::StringLiteral(x), TypeKind::StringLiteral(y)) => s.str(x).cmp(s.str(y)),
            (TypeKind::NumberLiteral(..), TypeKind::NumberLiteral(..)) => {
                let (x, y) = (
                    s.number_value(a).unwrap_or(0.0),
                    s.number_value(b).unwrap_or(0.0),
                );
                x.partial_cmp(&y)
                    .unwrap_or_else(|| x.is_nan().cmp(&y.is_nan()))
            }
            (TypeKind::BigIntLiteral(x), TypeKind::BigIntLiteral(y)) => {
                let (x, y) = (s.str(x), s.str(y));
                let (nx, ny) = (x.starts_with('-'), y.starts_with('-'));
                nx.cmp(&ny).then_with(|| x.cmp(y))
            }
            (
                TypeKind::EnumLiteral {
                    member: m1,
                    value: v1,
                },
                TypeKind::EnumLiteral {
                    member: m2,
                    value: v2,
                },
            ) => self
                .decl_position(self.sym_parent(m1).unwrap_or(m1))
                .cmp(&self.decl_position(self.sym_parent(m2).unwrap_or(m2)))
                .then_with(|| self.decl_position(m1).cmp(&self.decl_position(m2)))
                .then_with(|| self.compare_types(v1, v2, depth + 1)),
            (TypeKind::Intrinsic, TypeKind::Intrinsic) => {
                // Boolean literals: false before true; fresh/regular twins by id.
                let fa = self.store.regular(a) == TypeId::TRUE;
                let fb = self.store.regular(b) == TypeId::TRUE;
                fa.cmp(&fb)
            }
            // Unique symbols by their symbol's first declaration, then name (`typeof zebra |
            // typeof apple` when `zebra` is declared first).
            (TypeKind::UniqueSymbol(x), TypeKind::UniqueSymbol(y)) => self
                .decl_position(x)
                .cmp(&self.decl_position(y))
                .then_with(|| self.sym_name(x).cmp(self.sym_name(y))),
            (TypeKind::TypeParameter(_), TypeKind::TypeParameter(_)) => {
                let pa = s.type_param_data(a);
                let pb = s.type_param_data(b);
                match (pa, pb) {
                    (Some(pa), Some(pb)) => self
                        .sym_name(pa.sym)
                        .cmp(self.sym_name(pb.sym))
                        .then_with(|| self.decl_position(pa.sym).cmp(&self.decl_position(pb.sym)))
                        .then_with(|| pa.is_this.cmp(&pb.is_this)),
                    _ => Ordering::Equal,
                }
            }
            (TypeKind::Object(_), TypeKind::Object(_)) => {
                let ka = self.object_sort_key(a);
                let kb = self.object_sort_key(b);
                ka.cmp(&kb)
                    .then_with(|| self.compare_tuple_shapes(a, b))
                    .then_with(|| {
                        let la = self.type_args_of(a);
                        let lb = self.type_args_of(b);
                        self.compare_lists(la, lb, depth + 1)
                    })
            }
            // Aliased intersections sort first, by alias name (then alias arguments), and
            // the rest by their members (`OneToOne | Str` for `type Str = B<string> & {..}`).
            (TypeKind::Intersection { list: x, .. }, TypeKind::Intersection { list: y, .. }) => {
                match (s.alias_of(a), s.alias_of(b)) {
                    (Some(p), Some(q)) if p.sym != q.sym => {
                        self.sym_name(p.sym).cmp(self.sym_name(q.sym))
                    }
                    (Some(p), Some(q)) => {
                        self.compare_lists(s.list_items(p.args), s.list_items(q.args), depth + 1)
                    }
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => Ordering::Equal,
                }
                .then_with(|| self.compare_lists(s.list_items(x), s.list_items(y), depth + 1))
            }
            (TypeKind::Union { list: x, .. }, TypeKind::Union { list: y, .. }) => {
                self.compare_lists(s.list_items(x), s.list_items(y), depth + 1)
            }
            (TypeKind::Index(x), TypeKind::Index(y)) => self.compare_types(x, y, depth + 1),
            (
                TypeKind::IndexedAccess { obj: o1, index: i1 },
                TypeKind::IndexedAccess { obj: o2, index: i2 },
            ) => self
                .compare_types(o1, o2, depth + 1)
                .then_with(|| self.compare_types(i1, i2, depth + 1)),
            (TypeKind::TemplateLiteral(_), TypeKind::TemplateLiteral(_)) => {
                match (s.template_parts(a), s.template_parts(b)) {
                    (Some((xt, xs)), Some((yt, ys))) => {
                        let text = |t: &TypeId| s.string_value(*t).unwrap_or("");
                        xt.iter()
                            .map(text)
                            .cmp(yt.iter().map(text))
                            .then_with(|| self.compare_lists(xs, ys, depth + 1))
                    }
                    _ => Ordering::Equal,
                }
            }
            (
                TypeKind::StringMapping { sym: s1, ty: t1 },
                TypeKind::StringMapping { sym: s2, ty: t2 },
            ) => self
                .sym_name(s1)
                .cmp(self.sym_name(s2))
                .then_with(|| self.compare_types(t1, t2, depth + 1)),
            _ => Ordering::Equal,
        };
        // Distinct types that compare equal by content: fall back to a stable but
        // content-independent tie-breaker (creation order) so sorting is total.
        ord.then_with(|| a.0.cmp(&b.0))
    }

    fn compare_lists(&self, a: &[TypeId], b: &[TypeId], depth: u32) -> Ordering {
        for (x, y) in a.iter().zip(b.iter()) {
            let o = self.compare_types(*x, *y, depth);
            if o != Ordering::Equal {
                return o;
            }
        }
        a.len().cmp(&b.len())
    }

    /// Type arguments of a reference (without the trailing `this` argument).
    pub fn type_args_of(&self, t: TypeId) -> &[TypeId] {
        let s = &self.store;
        match s.object_of(t).map(|o| o.kind) {
            Some(ObjectKind::Reference { target, args }) => {
                let items = s.list_items(args);
                let n = match s.object_of(target).map(|o| o.kind) {
                    Some(ObjectKind::Declared { type_params, .. }) => {
                        s.list_items(type_params).len()
                    }
                    _ => items.len(),
                };
                &items[..n.min(items.len())]
            }
            _ => &[],
        }
    }
}

impl<'c, 'a> Ck<'c, 'a> {
    /// `T1 | T2 | ...` with literal reduction.
    pub fn union(&mut self, types: &[TypeId]) -> TypeId {
        self.union_with(types, Reduction::Literal, AliasId::NONE)
    }

    pub fn union2(&mut self, a: TypeId, b: TypeId) -> TypeId {
        self.union_with(&[a, b], Reduction::Literal, AliasId::NONE)
    }

    /// Union with subtype reduction (array literal elements, inferred return types).
    pub fn union_subtype(&mut self, types: &[TypeId]) -> TypeId {
        self.union_with(types, Reduction::Subtype, AliasId::NONE)
    }

    pub fn union_with(&mut self, types: &[TypeId], reduction: Reduction, alias: AliasId) -> TypeId {
        // Forward references to type aliases read differently once linked: not cached.
        if types.len() >= CACHED_UNION_MIN
            && reduction != Reduction::Subtype
            && !types.iter().any(|&t| self.store().is_forward(t))
        {
            if let Some(t) = self.union_cache.get(types, reduction, alias) {
                return t;
            }
            let t = self.union_uncached(types, reduction, alias);
            self.union_cache.put(types, reduction, alias, t);
            return t;
        }
        self.union_uncached(types, reduction, alias)
    }

    fn union_uncached(&mut self, types: &[TypeId], reduction: Reduction, alias: AliasId) -> TypeId {
        let s = self.store();
        let strict = self.c.opts.strict_null_checks;
        let mut set: Vec<TypeId> = Vec::with_capacity(types.len());
        let mut includes = TypeFlags::NONE;
        let mut has_error = false;
        let mut has_unresolved = false;
        // The one `any`-like member, while no other member has been seen (a union of one
        // named error type is that type).
        let mut only_any: Option<TypeId> = None;
        let mut mixed = false;
        let mut widening_nullable: Option<TypeId> = None;
        let mut stack: Vec<TypeId> = types.iter().rev().copied().collect();
        let mut guard = 0usize;
        while let Some(t) = stack.pop() {
            guard += 1;
            if guard > MAX_UNION_SIZE {
                return TypeId::UNRESOLVED;
            }
            let f = s.flags(t);
            if f.contains(TypeFlags::UNION) {
                // Enum unions keep their alias only when used as a whole; members are flattened.
                for &m in s.members_of_union(t).iter().rev() {
                    stack.push(m);
                }
                continue;
            }
            if f.contains(TypeFlags::NEVER) {
                continue;
            }
            includes |= f;
            if f.contains(TypeFlags::ANY) {
                if self.store().is_error(t) {
                    has_error = true;
                }
                if t == TypeId::UNRESOLVED {
                    has_unresolved = true;
                }
                match only_any {
                    None => only_any = Some(t),
                    Some(a) if a != t => mixed = true,
                    _ => {}
                }
                continue;
            }
            mixed = true;
            if !strict && f.intersects(TypeFlags::NULLABLE) {
                if s.traits(t).contains(TypeTraits::CONTAINS_WIDENING) {
                    widening_nullable.get_or_insert(t);
                } else {
                    widening_nullable = Some(if f.contains(TypeFlags::NULL) {
                        TypeId::NULL
                    } else {
                        TypeId::UNDEFINED
                    });
                }
                continue;
            }
            if !set.contains(&t) {
                set.push(t);
            }
        }
        if includes.contains(TypeFlags::ANY) {
            return if has_unresolved {
                TypeId::UNRESOLVED
            } else if let Some(e) = only_any
                && !mixed
                && s.error_name(e).is_some()
            {
                e
            } else if has_error {
                TypeId::ERROR
            } else {
                TypeId::ANY
            };
        }
        if includes.contains(TypeFlags::UNKNOWN) {
            return TypeId::UNKNOWN;
        }
        if set.is_empty() {
            if let Some(n) = widening_nullable {
                return n;
            }
            return TypeId::NEVER;
        }
        // Fresh literals whose regular twin is present, and `undefined` next to `void`.
        let has_void = set.iter().any(|&t| s.flags(t).contains(TypeFlags::VOID));
        let snapshot = set.clone();
        set.retain(|&t| {
            let f = s.flags(t);
            if s.traits(t).contains(TypeTraits::FRESH) && snapshot.contains(&s.regular(t)) {
                return false;
            }
            // (Only subtype reduction: a written `void | undefined` keeps both.)
            if has_void
                && reduction == Reduction::Subtype
                && f.contains(TypeFlags::UNDEFINED)
                && t != TypeId::MISSING
            {
                return false;
            }
            if reduction != Reduction::None {
                // Enum members are absorbed too (`e | number` is `number`).
                if f.contains(TypeFlags::STRING_LITERAL) && includes.contains(TypeFlags::STRING) {
                    return false;
                }
                if f.contains(TypeFlags::NUMBER_LITERAL) && includes.contains(TypeFlags::NUMBER) {
                    return false;
                }
                if f.contains(TypeFlags::BIGINT_LITERAL) && includes.contains(TypeFlags::BIGINT) {
                    return false;
                }
                if f.contains(TypeFlags::UNIQUE_ES_SYMBOL)
                    && includes.contains(TypeFlags::ES_SYMBOL)
                {
                    return false;
                }
                if f.contains(TypeFlags::TEMPLATE_LITERAL) && includes.contains(TypeFlags::STRING) {
                    return false;
                }
            }
            true
        });
        // Duplicated fresh literals (two fresh twins of the same regular literal).
        let mut seen_regular: Vec<TypeId> = Vec::new();
        set.retain(|&t| {
            if s.traits(t).contains(TypeTraits::FRESH) {
                let r = s.regular(t);
                if seen_regular.contains(&r) {
                    return false;
                }
                seen_regular.push(r);
            }
            true
        });
        if includes.contains(TypeFlags::INTERSECTION) && set.len() > 1 {
            self.remove_constrained_type_variables(&mut set);
        }
        if reduction == Reduction::Subtype && set.len() > 1 {
            set = self.remove_subtypes(set);
        }
        if set.len() == 1 {
            return set[0];
        }
        let c = self.c;
        set.sort_by(|&a, &b| c.compare_types(a, b, 0));
        if alias == AliasId::NONE
            && let Some(items) = self.union_origin_items(types, &set)
        {
            if let [only] = items[..] {
                return only;
            }
            return s.union_with_origin(&set, &items);
        }
        // Enum unions: when every member of an enum is present, the printer collapses them.
        s.union_sorted(&set, Some(alias))
    }

    /// How a union of `types` (whose members are `set`) was written, when that is shorter:
    /// aliased unions stay single items (`A | undefined`, `B | null`), other unions are
    /// flattened, unions with an origin contribute their origin. `None` when no aliased
    /// union takes part or reduction removed some of their members. Items are sorted with
    /// the other members first, then the aliased unions (observed: `A | string` prints as
    /// `string | A`).
    fn union_origin_items(&self, types: &[TypeId], set: &[TypeId]) -> Option<Vec<TypeId>> {
        let s = self.store();
        // An aliased union, or one printed as a single named type (`keyof A`).
        let is_alias_union = |t: TypeId| {
            s.flags(t).contains(TypeFlags::UNION)
                && !s.traits(t).contains(TypeTraits::ENUM_UNION)
                && (s.alias_of(t).is_some()
                    || s.union_origin(t).is_some_and(
                        |o| matches!(o, [x] if !s.flags(*x).contains(TypeFlags::UNION)),
                    ))
        };
        if !types
            .iter()
            .any(|&t| is_alias_union(t) || s.union_origin(t).is_some())
        {
            return None;
        }
        let mut items: Vec<TypeId> = Vec::new();
        let push = |x: TypeId, items: &mut Vec<TypeId>| {
            if !items.contains(&x) {
                items.push(x);
            }
        };
        for &t in types {
            let f = s.flags(t);
            if f.contains(TypeFlags::NEVER) {
                continue;
            }
            if f.contains(TypeFlags::UNION) {
                if is_alias_union(t) {
                    push(t, &mut items);
                } else if let Some(o) = s.union_origin(t) {
                    for &x in o {
                        push(x, &mut items);
                    }
                } else {
                    for &x in s.members_of_union(t) {
                        push(x, &mut items);
                    }
                }
            } else {
                push(t, &mut items);
            }
        }
        // The items must cover exactly the union's members.
        let norm = |x: TypeId| {
            if s.traits(x).contains(TypeTraits::FRESH) {
                s.regular(x)
            } else {
                x
            }
        };
        // A member already inside one of the aliased unions is not written again
        // (`C | undefined` with `type C = undefined | ...` is `C`).
        let named: Vec<TypeId> = items
            .iter()
            .copied()
            .filter(|&x| is_alias_union(x))
            .collect();
        if !named.is_empty() {
            items.retain(|&x| {
                is_alias_union(x)
                    || !named
                        .iter()
                        .any(|&u| s.members_of_union(u).iter().any(|&m| norm(m) == norm(x)))
            });
        }
        let set_norm: Vec<TypeId> = set.iter().map(|&x| norm(x)).collect();
        let mut covered = 0usize;
        let mut flat: Vec<TypeId> = Vec::new();
        for &x in &items {
            if is_alias_union(x) {
                for &m in s.members_of_union(x) {
                    flat.push(norm(m));
                }
            } else {
                flat.push(norm(x));
            }
        }
        for &x in &flat {
            if !set_norm.contains(&x) {
                return None;
            }
        }
        for &x in &set_norm {
            if flat.contains(&x) {
                covered += 1;
            }
        }
        if covered != set_norm.len() || items.len() >= set.len() && items.len() != 1 {
            return None;
        }
        let (mut unions, mut others): (Vec<TypeId>, Vec<TypeId>) =
            items.into_iter().partition(|&x| is_alias_union(x));
        let c = self.c;
        others.sort_by(|&a, &b| c.compare_types(a, b, 0));
        // Aliased unions in declaration order of their aliases (tsc: creation order).
        unions.sort_by(|&a, &b| {
            let pa = s.alias_of(a).map(|i| c.decl_position(i.sym));
            let pb = s.alias_of(b).map(|i| c.decl_position(i.sym));
            pa.cmp(&pb).then_with(|| c.compare_types(a, b, 0))
        });
        others.extend(unions);
        Some(others)
    }

    fn remove_subtypes(&mut self, mut set: Vec<TypeId>) -> Vec<TypeId> {
        if set.len() > 200 {
            return set;
        }
        let s = self.store();
        let mut i = set.len();
        while i > 0 {
            i -= 1;
            let src = set[i];
            let sf = s.flags(src);
            // Primitive literals are handled by literal reduction already; object types and
            // literals against object types are what subtype reduction is for.
            let mut remove = false;
            for (j, &tgt) in set.iter().enumerate() {
                if j == i {
                    continue;
                }
                let tf = s.flags(tgt);
                if sf.intersects(TypeFlags::PRIMITIVE)
                    && tf.contains(TypeFlags::OBJECT)
                    && !self.is_empty_anonymous_object(tgt)
                {
                    continue;
                }
                if sf.contains(TypeFlags::OBJECT) && tf.intersects(TypeFlags::PRIMITIVE) {
                    continue;
                }
                if self.is_strict_subtype(src, tgt) {
                    remove = true;
                    break;
                }
            }
            if remove {
                set.remove(i);
            }
        }
        set
    }

    /// The type variable and the primitive of a constraining intersection `T & P` (one that
    /// `reduce_by_constraint` kept: `T`'s constraint is made of primitives, `object` or
    /// `{}`, and `P` narrows it).
    fn constraining_intersection(&mut self, t: TypeId) -> Option<(TypeId, TypeId)> {
        let s = self.store();
        if !s.flags(t).contains(TypeFlags::INTERSECTION) {
            return None;
        }
        let &[a, b] = s.members_of_union(t) else {
            return None;
        };
        let var = |t: TypeId| {
            s.flags(t)
                .intersects(TypeFlags::TYPE_PARAMETER | TypeFlags::INDEXED_ACCESS)
        };
        let (tv, p) = if var(a) { (a, b) } else { (b, a) };
        if !var(tv) {
            return None;
        }
        let pf = s.flags(p);
        let generic_string_like = pf
            .intersects(TypeFlags::TEMPLATE_LITERAL | TypeFlags::STRING_MAPPING)
            && !self.is_pattern_literal_type(p);
        let primitive = pf.intersects(TypeFlags::PRIMITIVE | TypeFlags::NON_PRIMITIVE)
            && !pf.contains(TypeFlags::UNION)
            && !generic_string_like;
        if !primitive && !self.is_empty_anonymous_object(p) {
            return None;
        }
        let c = self.base_constraint(tv);
        if matches!(c, TypeId::UNKNOWN | TypeId::UNRESOLVED) || self.store().is_error(c) {
            return None;
        }
        let members = self.union_members(c);
        let ok = members.iter().all(|&m| {
            self.store()
                .flags(m)
                .intersects(TypeFlags::PRIMITIVE | TypeFlags::NON_PRIMITIVE)
                || self.is_empty_anonymous_object(m)
        });
        ok.then_some((tv, p))
    }

    /// Constraining intersections `T & P1 | T & P2 | ...` whose primitives cover every
    /// member of `T`'s constraint are replaced by `T` (`K extends "a" | "b"`:
    /// `("a" & K) | ("b" & K)` is `K`; tsc: checker.go, removeConstrainedTypeVariables).
    fn remove_constrained_type_variables(&mut self, set: &mut Vec<TypeId>) {
        let mut pairs: Vec<(usize, TypeId, TypeId)> = Vec::new();
        for (i, &t) in set.iter().enumerate() {
            if self.store().flags(t).contains(TypeFlags::INTERSECTION)
                && let Some((tv, p)) = self.constraining_intersection(t)
            {
                pairs.push((i, tv, p));
            }
        }
        if pairs.len() < 2 {
            return;
        }
        let mut vars: Vec<TypeId> = Vec::new();
        for &(_, tv, _) in &pairs {
            if !vars.contains(&tv) {
                vars.push(tv);
            }
        }
        let mut remove: Vec<usize> = Vec::new();
        let mut add: Vec<TypeId> = Vec::new();
        for tv in vars {
            let c = self.base_constraint(tv);
            let members = self.union_members(c);
            let covered = members
                .iter()
                .all(|m| pairs.iter().any(|&(_, v, p)| v == tv && p == *m));
            if covered {
                for &(i, v, p) in &pairs {
                    if v == tv && members.contains(&p) {
                        remove.push(i);
                    }
                }
                add.push(tv);
            }
        }
        if add.is_empty() {
            return;
        }
        let mut i = 0;
        set.retain(|_| {
            let keep = !remove.contains(&i);
            i += 1;
            keep
        });
        for tv in add {
            if !set.contains(&tv) {
                set.push(tv);
            }
        }
    }

    /// `T1 & T2 & ...`, normalised (distributes over unions, removes `unknown`, detects
    /// empty intersections of disjoint primitives).
    pub fn intersection(&mut self, types: &[TypeId]) -> TypeId {
        self.intersection_with(types, AliasId::NONE)
    }

    /// `T & P` of a type variable and a primitive, `object` or `{}`, reduced by `T`'s
    /// constraint when that is made of the same kinds of types: the constraint inside `P`
    /// gives `T` (`T extends "a" | "b"`: `T & string` is `T`), a constraint with no member
    /// inside `P` and not containing `P` gives `never` (`T & undefined`).
    fn reduce_by_constraint(&mut self, set: &[TypeId]) -> Option<TypeId> {
        if set.len() != 2 {
            return None;
        }
        let s = self.store();
        let var = |t: TypeId| {
            s.flags(t)
                .intersects(TypeFlags::TYPE_PARAMETER | TypeFlags::INDEXED_ACCESS)
        };
        let (tv, p) = if var(set[0]) {
            (set[0], set[1])
        } else {
            (set[1], set[0])
        };
        if !var(tv) {
            return None;
        }
        let pf = s.flags(p);
        let generic_string_like = pf
            .intersects(TypeFlags::TEMPLATE_LITERAL | TypeFlags::STRING_MAPPING)
            && !self.is_pattern_literal_type(p);
        let empty_object = set.iter().any(|&t| self.is_empty_anonymous_object(t));
        let primitive = pf.intersects(TypeFlags::PRIMITIVE | TypeFlags::NON_PRIMITIVE)
            && !pf.contains(TypeFlags::UNION)
            && !generic_string_like;
        if !primitive && !empty_object {
            return None;
        }
        let c = self.base_constraint(tv);
        if matches!(c, TypeId::UNKNOWN | TypeId::UNRESOLVED) || self.store().is_error(c) {
            return None;
        }
        let members = self.union_members(c);
        let ok = |ck: &Self, m: TypeId| {
            ck.store()
                .flags(m)
                .intersects(TypeFlags::PRIMITIVE | TypeFlags::NON_PRIMITIVE)
                || ck.is_empty_anonymous_object(m)
        };
        if !members.iter().all(|&m| ok(self, m)) {
            return None;
        }
        if self.is_strict_subtype(c, p) {
            return Some(tv);
        }
        let is_union = self.store().flags(c).contains(TypeFlags::UNION);
        let member_inside = is_union && members.iter().any(|&m| self.is_strict_subtype(m, p));
        if !member_inside && !self.is_strict_subtype(p, c) {
            return Some(TypeId::NEVER);
        }
        None
    }

    pub fn intersection_with(&mut self, types: &[TypeId], alias: AliasId) -> TypeId {
        let s = self.store();
        let mut set: Vec<TypeId> = Vec::new();
        let mut includes = TypeFlags::NONE;
        let mut stack: Vec<TypeId> = types.iter().rev().copied().collect();
        let mut guard = 0;
        while let Some(t) = stack.pop() {
            guard += 1;
            if guard > 10_000 {
                return TypeId::UNRESOLVED;
            }
            let f = s.flags(t);
            if f.contains(TypeFlags::INTERSECTION) {
                for &m in s.members_of_union(t).iter().rev() {
                    stack.push(m);
                }
                continue;
            }
            includes |= f;
            if f.contains(TypeFlags::ANY) {
                if t == TypeId::UNRESOLVED {
                    return t;
                }
                // An intersection with an error type is the (unnamed) error type: tsc
                // prints `Problem & X` as `any`.
                if self.store().is_error(t) {
                    return TypeId::ERROR;
                }
                continue;
            }
            if f.contains(TypeFlags::UNKNOWN) {
                continue;
            }
            let t = if s.traits(t).contains(TypeTraits::FRESH) && f.intersects(TypeFlags::LITERAL) {
                s.regular(t)
            } else {
                t
            };
            if !set.contains(&t) {
                set.push(t);
            }
        }
        if includes.contains(TypeFlags::NEVER) {
            return TypeId::NEVER;
        }
        if includes.contains(TypeFlags::ANY) {
            return TypeId::ANY;
        }
        if set.is_empty() {
            return TypeId::UNKNOWN;
        }
        // Disjoint primitives.
        let prim_groups = [
            TypeFlags::STRING_LIKE,
            TypeFlags::NUMBER_LIKE,
            TypeFlags::BIGINT_LIKE,
            TypeFlags::BOOLEAN_LIKE,
            TypeFlags::ES_SYMBOL_LIKE,
            TypeFlags::VOID_LIKE,
            TypeFlags::NULL,
        ];
        let mut groups = 0;
        for g in prim_groups {
            if set
                .iter()
                .any(|&t| s.flags(t).intersects(g) && !s.flags(t).contains(TypeFlags::UNION))
            {
                groups += 1;
            }
        }
        // `object & string` is empty too (`object` is only absorbed nowhere, see below).
        if groups > 0
            && set
                .iter()
                .any(|&t| s.flags(t).contains(TypeFlags::NON_PRIMITIVE))
        {
            return TypeId::NEVER;
        }
        if groups > 1 {
            return TypeId::NEVER;
        }
        // Two different unit types of the same kind.
        let units: Vec<TypeId> = set
            .iter()
            .copied()
            .filter(|&t| s.flags(t).intersects(TypeFlags::UNIT))
            .collect();
        if units.len() > 1 {
            return TypeId::NEVER;
        }
        if units.len() == 1 {
            // A literal intersected with its base primitive is the literal.
            set.retain(|&t| {
                t == units[0]
                    || !s.flags(t).intersects(
                        TypeFlags::STRING
                            | TypeFlags::NUMBER
                            | TypeFlags::BIGINT
                            | TypeFlags::ES_SYMBOL,
                    )
            });
        }
        let has_object = set.iter().any(|&t| {
            s.flags(t)
                .intersects(TypeFlags::OBJECT | TypeFlags::NON_PRIMITIVE)
        });
        if self.c.opts.strict_null_checks
            && set
                .iter()
                .any(|&t| s.flags(t).intersects(TypeFlags::NULLABLE))
            && (has_object
                || set.iter().any(|&t| {
                    s.flags(t).intersects(
                        TypeFlags::STRING_LIKE
                            | TypeFlags::NUMBER_LIKE
                            | TypeFlags::BIGINT_LIKE
                            | TypeFlags::BOOLEAN_LIKE
                            | TypeFlags::ES_SYMBOL_LIKE,
                    )
                }))
        {
            return TypeId::NEVER;
        }
        // `{}` (also written as an empty type literal, as in `T & {}`) is absorbed by
        // another primitive or object type (not by a type variable: `T & {}` stays; see
        // `written_primitive_and_empty_object` for `string & {}` as written).
        if set.len() > 1
            && set.iter().any(|&t| self.is_empty_anonymous_object(t))
            && set.iter().any(|&t| {
                !self.is_empty_anonymous_object(t)
                    && s.flags(t).intersects(
                        TypeFlags::PRIMITIVE | TypeFlags::OBJECT | TypeFlags::NON_PRIMITIVE,
                    )
            })
        {
            let mut kept = Vec::with_capacity(set.len());
            for &t in &set {
                if !self.is_empty_anonymous_object(t) {
                    kept.push(t);
                }
            }
            if kept.is_empty() {
                kept.push(set[0]);
            }
            set = kept;
            if set.len() == 1 {
                return set[0];
            }
        }
        // `object & { a: string }` stays as written (tsc 7 prints both).
        if set.len() == 1 {
            return set[0];
        }
        if let Some(r) = self.reduce_by_constraint(&set) {
            return r;
        }
        // Unions of primitives intersect as sets first (`keyof` of a union of tuples and
        // records: `(number | "0" | "length" | ...) & ("am" | "pm")`), so the cross product
        // below stays small.
        let prim_union = |t: TypeId| {
            s.flags(t).contains(TypeFlags::UNION)
                && s.members_of_union(t)
                    .iter()
                    .all(|&m| s.flags(m).intersects(TypeFlags::PRIMITIVE))
        };
        if set.iter().filter(|&&t| prim_union(t)).count() > 1 {
            let unions: Vec<TypeId> = set.iter().copied().filter(|&t| prim_union(t)).collect();
            let covered = |x: TypeId, u: TypeId| {
                let ms = s.members_of_union(u);
                if ms.contains(&x) {
                    return true;
                }
                let xf = s.flags(x);
                let has = |f: TypeFlags| ms.iter().any(|&m| s.flags(m).contains(f));
                (xf.intersects(TypeFlags::STRING_LITERAL | TypeFlags::TEMPLATE_LITERAL)
                    && has(TypeFlags::STRING))
                    || (xf.contains(TypeFlags::NUMBER_LITERAL) && has(TypeFlags::NUMBER))
                    || (xf.contains(TypeFlags::BIGINT_LITERAL) && has(TypeFlags::BIGINT))
                    || (xf.contains(TypeFlags::UNIQUE_ES_SYMBOL) && has(TypeFlags::ES_SYMBOL))
                    || (xf.contains(TypeFlags::BOOLEAN_LITERAL) && has(TypeFlags::BOOLEAN))
            };
            let mut keep: Vec<TypeId> = Vec::new();
            for &u in &unions {
                for &x in s.members_of_union(u) {
                    if !keep.contains(&x) && unions.iter().all(|&v| covered(x, v)) {
                        keep.push(x);
                    }
                }
            }
            let merged = self.union_with(&keep, Reduction::Literal, AliasId::NONE);
            let mut rest: Vec<TypeId> = set.iter().copied().filter(|&t| !prim_union(t)).collect();
            rest.push(merged);
            return self.intersection_with(&rest, alias);
        }
        // Distribute over unions.
        if let Some(ui) = set
            .iter()
            .position(|&t| s.flags(t).contains(TypeFlags::UNION))
        {
            let members = s.members_of_union(set[ui]).to_vec();
            let total: usize = set
                .iter()
                .filter(|&&t| s.flags(t).contains(TypeFlags::UNION))
                .map(|&t| s.members_of_union(t).len())
                .product();
            if total > 10_000 {
                return TypeId::UNRESOLVED;
            }
            let unions = set
                .iter()
                .filter(|&&t| s.flags(t).contains(TypeFlags::UNION))
                .count();
            let mut parts = Vec::with_capacity(members.len());
            for m in members {
                let mut v = set.clone();
                v[ui] = m;
                parts.push(self.intersection(&v));
            }
            let u = self.union_with(&parts, Reduction::Literal, alias);
            // A distribution that leaves intersections and more pieces than were written
            // prints as written: `T & (0 | 1 | 2)`, not `(T & 0) | (T & 1) | (T & 2)`.
            if unions == 1 && alias == AliasId::NONE && s.flags(u).contains(TypeFlags::UNION) {
                let ms = s.members_of_union(u);
                if ms
                    .iter()
                    .any(|&m| s.flags(m).contains(TypeFlags::INTERSECTION))
                    && self.constituent_count_of(ms, 0) > self.constituent_count_of(&set, 0)
                {
                    let ms = ms.to_vec();
                    let written = s.intersection_sorted(&set, None);
                    return s.union_with_origin(&ms, &[written]);
                }
            }
            return u;
        }
        s.intersection_sorted(&set, Some(alias))
    }

    /// How many pieces `types` spell out: a union or intersection counts its members (a
    /// union with an origin, the origin's), anything else or an aliased type one.
    fn constituent_count_of(&self, types: &[TypeId], depth: u32) -> usize {
        let s = self.store();
        types
            .iter()
            .map(|&t| {
                if depth > 16
                    || !s.flags(t).intersects(TypeFlags::UNION_OR_INTERSECTION)
                    || s.alias_of(t).is_some()
                {
                    return 1;
                }
                match s.union_origin(t) {
                    Some(o) => self.constituent_count_of(o, depth + 1),
                    None => self.constituent_count_of(s.members_of_union(t), depth + 1),
                }
            })
            .sum()
    }

    /// `t` without what is empty because of conflicting discriminants: an intersection
    /// of object types where a property declared with literal types in two members has
    /// no common value is `never` (`{ kind: "two" } & { kind: "one" }`), and a union
    /// drops such members (`(A | B) & { kind: "one" }` is `A & { kind: "one" }` when `B`
    /// has `kind: "two"`). Two non-literal property types (`string & number`) do not
    /// empty the intersection (observed).
    pub(crate) fn reduced_type(&mut self, t: TypeId) -> TypeId {
        let f = self.store().flags(t);
        if !f.intersects(TypeFlags::UNION_OR_INTERSECTION) {
            return t;
        }
        if let Some(&r) = self.reduced.get(&t) {
            return r;
        }
        self.reduced.insert(t, t);
        let r = if f.contains(TypeFlags::UNION) {
            let ms = self.store().members_of_union(t).to_vec();
            if ms
                .iter()
                .any(|&m| self.store().flags(m).contains(TypeFlags::INTERSECTION))
            {
                let kept: Vec<TypeId> = ms
                    .iter()
                    .copied()
                    .filter(|&m| self.reduced_type(m) != TypeId::NEVER)
                    .collect();
                if kept.len() == ms.len() {
                    t
                } else {
                    self.union(&kept)
                }
            } else {
                t
            }
        } else if self.has_conflicting_discriminant(t) {
            TypeId::NEVER
        } else {
            t
        };
        self.reduced.insert(t, r);
        r
    }

    fn has_conflicting_discriminant(&mut self, t: TypeId) -> bool {
        self.conflicting_discriminant(t).is_some() || self.conflicting_private(t).is_some()
    }

    /// Why the intersection `t` was reduced to `never` (TS18031 or TS18032), with its
    /// arguments.
    pub(crate) fn never_reduction_reason(
        &mut self,
        t: TypeId,
    ) -> Option<(crate::diagnostics_generated::DiagnosticInfo, String, String)> {
        if !self.store().flags(t).contains(TypeFlags::INTERSECTION) {
            return None;
        }
        let (info, pn) = if let Some(pn) = self.conflicting_discriminant(t) {
            (crate::diagnostics_generated::THE_INTERSECTION_0_WAS_REDUCED_TO_NEVER_BECAUSE_PROPERTY_1_HAS_CONFLICTING_TYPES_18031, pn)
        } else {
            let pn = self.conflicting_private(t)?;
            (crate::diagnostics_generated::THE_INTERSECTION_0_WAS_REDUCED_TO_NEVER_BECAUSE_PROPERTY_1_EXISTS_IN_MULTIPLE_CO_18032, pn)
        };
        self.print_no_reduction = true;
        let ts = self.type_to_string(t);
        self.print_no_reduction = false;
        let pname = self.store().str(pn).to_owned();
        Some((info, ts, pname))
    }

    /// The property of intersection `t` declared separately by two of its members, private
    /// in one of them (`A & B` with a `private x` in each class).
    pub(crate) fn conflicting_private(&mut self, t: TypeId) -> Option<StrId> {
        let ms = self.store().members_of_union(t).to_vec();
        if ms.len() < 2
            || !ms
                .iter()
                .all(|&m| self.store().flags(m).contains(TypeFlags::OBJECT))
            || self.is_generic_type(t)
        {
            return None;
        }
        let mut seen: Vec<(StrId, SymId)> = Vec::new();
        for &m in &ms {
            for (name, p) in self.properties_of(m) {
                if let Some(&(_, q)) = seen.iter().find(|e| e.0 == name) {
                    if q != p
                        && !self.same_declaration(p, q)
                        && (self.is_private_like(p) == Some(true)
                            || self.is_private_like(q) == Some(true))
                    {
                        return Some(name);
                    }
                } else {
                    seen.push((name, p));
                }
            }
        }
        None
    }

    /// The property of intersection `t` whose types in two members, one of them literal,
    /// have no common value (`{ a: "" } & { a: number }`; tsc: `isDiscriminantWithNeverType`). Only properties declared by two members are typed (`window` is
    /// `Window & typeof globalThis`, whose globals are being declared).
    pub(crate) fn conflicting_discriminant(&mut self, t: TypeId) -> Option<StrId> {
        let ms = self.store().members_of_union(t).to_vec();
        if ms.len() < 2
            || !ms
                .iter()
                .all(|&m| self.store().flags(m).contains(TypeFlags::OBJECT))
            || self.is_generic_type(t)
        {
            return None;
        }
        let per_member: Vec<Vec<(StrId, SymId)>> =
            ms.iter().map(|&m| self.properties_of(m)).collect();
        let mut counts: rustc_hash::FxHashMap<StrId, u32> = rustc_hash::FxHashMap::default();
        for props in &per_member {
            for &(n, _) in props {
                *counts.entry(n).or_insert(0) += 1;
            }
        }
        // Per property: the members' types, whether one of them is literal, and whether
        // one is `never` already (then the property is not a discriminant).
        let mut seen: Vec<(StrId, Vec<TypeId>, bool, bool)> = Vec::new();
        for props in &per_member {
            for &(name, p) in props {
                if counts.get(&name).copied().unwrap_or(0) < 2 {
                    continue;
                }
                let pt = self.type_of_symbol(p);
                let literal = self.every_member(pt, |m| self.is_unit(m));
                let never = pt == TypeId::NEVER;
                if let Some(e) = seen.iter_mut().find(|e| e.0 == name) {
                    if !e.1.contains(&pt) {
                        e.1.push(pt);
                    }
                    e.2 |= literal;
                    e.3 |= never;
                } else {
                    seen.push((name, vec![pt], literal, never));
                }
            }
        }
        for (name, types, literal, never) in seen {
            if literal && !never && types.len() > 1 && self.intersection(&types) == TypeId::NEVER {
                return Some(name);
            }
        }
        None
    }

    /// The primitive of an intersection of a primitive with `{}` (`string & {}` is
    /// `string`), which behaves as the primitive except that literals stay next to it in
    /// a union.
    pub fn primitive_with_empty_object(&self, t: TypeId) -> Option<TypeId> {
        let s = self.store();
        if !s.flags(t).contains(TypeFlags::INTERSECTION) {
            return None;
        }
        let mut prim = None;
        for &m in s.members_of_union(t) {
            if self.is_empty_anonymous_object(m) {
                continue;
            }
            if prim.is_some() || !s.flags(m).intersects(TypeFlags::PRIMITIVE) {
                return None;
            }
            prim = Some(m);
        }
        prim
    }

    pub fn is_primitive_with_empty_object(&self, t: TypeId) -> bool {
        self.primitive_with_empty_object(t).is_some()
    }

    /// An intersection type written as a non-unit string, number or bigint with `{}`
    /// (`string & {}`, `` `a${string}` & {} ``) keeps the `{}` (observed: it prints as
    /// written and a union keeps its literals next to it, `"left" | (string & {})`), unlike
    /// one made by instantiation or distribution (`NonNullable<string>` and `(string |
    /// undefined) & {}` are `string`).
    pub fn written_primitive_and_empty_object(
        &mut self,
        parts: &[TypeId],
        alias: AliasId,
    ) -> Option<TypeId> {
        let s = self.store();
        let keeps = TypeFlags::STRING
            | TypeFlags::NUMBER
            | TypeFlags::BIGINT
            | TypeFlags::TEMPLATE_LITERAL
            | TypeFlags::STRING_MAPPING;
        let mut prim = None;
        let mut empty = None;
        for &p in parts {
            if self.is_empty_anonymous_object(p) {
                empty.get_or_insert(p);
            } else if prim.is_none() && s.flags(p).intersects(keeps) {
                prim = Some(p);
            } else {
                return None;
            }
        }
        let (p, e) = (prim?, empty?);
        Some(s.intersection_sorted(&[p, e], Some(alias)))
    }

    /// `{}`: the empty object type or an empty type literal.
    pub fn is_empty_anonymous_object(&self, t: TypeId) -> bool {
        if t == TypeId::EMPTY_OBJECT {
            return true;
        }
        match self.store().object_of(t).map(|o| o.kind) {
            Some(ObjectKind::Anonymous {
                decl,
                kind: anon::TYPE_LITERAL,
                ..
            }) => Loc::unpack(decl).is_some_and(|l| {
                matches!(*self.node(l), ts_r_ast::Node::TypeLiteral { members } if self.ast(l.file).list(members).is_empty())
            }),
            _ => false,
        }
    }

    /// The base primitive of a literal (`1` → `number`), mapped over unions; enum literals
    /// widen to their enum.
    pub fn base_of_literal(&mut self, t: TypeId) -> TypeId {
        let s = self.store();
        let f = s.flags(t);
        if f.contains(TypeFlags::ENUM_LITERAL) {
            return self.enum_type_of_literal(t);
        }
        if f.contains(TypeFlags::STRING_LITERAL)
            || f.contains(TypeFlags::TEMPLATE_LITERAL)
            || f.contains(TypeFlags::STRING_MAPPING)
        {
            return TypeId::STRING;
        }
        if f.contains(TypeFlags::NUMBER_LITERAL) {
            return TypeId::NUMBER;
        }
        if f.contains(TypeFlags::BIGINT_LITERAL) {
            return TypeId::BIGINT;
        }
        if f.contains(TypeFlags::BOOLEAN_LITERAL) {
            return TypeId::BOOLEAN;
        }
        if f.contains(TypeFlags::UNION) {
            // Members of one enum give the enum itself (`E.A | E.B` is `E`).
            let ms = s.members_of_union(t).to_vec();
            if ms
                .iter()
                .all(|&m| self.store().flags(m).contains(TypeFlags::ENUM_LITERAL))
            {
                let e = self.enum_type_of_literal(ms[0]);
                if ms[1..].iter().all(|&m| self.enum_type_of_literal(m) == e) {
                    return e;
                }
            }
            return self.map_type(t, |ck, m| ck.base_of_literal(m));
        }
        t
    }

    /// The base type a relational comparison (`<`, `>=`...) sees: enum members compare
    /// as their primitive (`string` for string members, `number` otherwise).
    pub fn base_for_comparison(&mut self, t: TypeId) -> TypeId {
        let f = self.store().flags(t);
        if f.contains(TypeFlags::ENUM_LITERAL) {
            return if f.contains(TypeFlags::STRING_LITERAL) {
                TypeId::STRING
            } else {
                TypeId::NUMBER
            };
        }
        if f.contains(TypeFlags::ENUM) {
            return TypeId::NUMBER;
        }
        if f.contains(TypeFlags::UNION) {
            return self.map_type(t, |ck, m| ck.base_for_comparison(m));
        }
        self.base_of_literal(t)
    }

    /// Widens fresh literal types (`let x = 1` is `number`).
    pub fn widen_literal(&mut self, t: TypeId) -> TypeId {
        let s = self.store();
        let f = s.flags(t);
        if f.intersects(TypeFlags::LITERAL) && s.traits(t).contains(TypeTraits::FRESH) {
            return self.base_of_literal(t);
        }
        if f.contains(TypeFlags::UNION) {
            if !s
                .members_of_union(t)
                .iter()
                .any(|&m| s.traits(m).contains(TypeTraits::FRESH))
            {
                return t;
            }
            return self.map_type(t, |ck, m| ck.widen_literal(m));
        }
        t
    }

    /// Widening for declarations without annotations: fresh object literal types become
    /// regular; `null`/`undefined` widen to `any` without `strictNullChecks`.
    pub fn widened_type(&mut self, t: TypeId) -> TypeId {
        self.widened_type_in(t, None)
    }

    /// [`Self::widened_type`] with the sibling types of a union being widened: object
    /// literals in a union are normalized, each getting the properties only its siblings
    /// have as optional `undefined` ones (`[{ a: 1 }, { b: "" }]` widens to
    /// `({ a: number; b?: undefined; } | { b: string; a?: undefined; })[]`), recursively
    /// for object literals in the same property of each sibling.
    pub(crate) fn widened_type_in(&mut self, t: TypeId, siblings: Option<&[TypeId]>) -> TypeId {
        let s = self.store();
        let tr = s.traits(t);
        if !self.c.opts.strict_null_checks
            && (t == TypeId::UNDEFINED_WIDENING || t == TypeId::NULL_WIDENING)
        {
            return TypeId::ANY;
        }
        if s.flags(t).contains(TypeFlags::UNION)
            && tr.intersects(TypeTraits::CONTAINS_WIDENING | TypeTraits::CONTAINS_OBJECT_LITERAL)
        {
            let sibs: Vec<TypeId> = match siblings {
                Some(x) => x.to_vec(),
                None => s.members_of_union(t).to_vec(),
            };
            return self.map_type(t, |ck, m| {
                if ck.store().flags(m).intersects(TypeFlags::NULLABLE) {
                    ck.widened_type_in(m, None)
                } else {
                    ck.widened_type_in(m, Some(&sibs))
                }
            });
        }
        if let Some(o) = s.object_of(t)
            && o.flags.contains(ObjectFlags::OBJECT_LITERAL)
        {
            // An already widened literal is not normalized again with its siblings.
            let siblings = siblings.filter(|_| self.is_normalizable_literal(t));
            return self.widen_object_literal_in(t, siblings);
        }
        if s.object_of(t).is_some() && self.c.is_array_type(t) {
            let el = self
                .c
                .type_args_of(t)
                .first()
                .copied()
                .unwrap_or(TypeId::ANY);
            // Object literal elements become regular too (`[{ assertId: 1 }]` inferred for
            // `T` is not checked for excess properties against `T`'s constraint).
            if tr.contains(TypeTraits::CONTAINS_WIDENING)
                || self
                    .store()
                    .traits(el)
                    .contains(TypeTraits::CONTAINS_OBJECT_LITERAL)
            {
                let w = self.widened_type(el);
                if w != el {
                    let s = self.store();
                    let ro = match s.object_of(t).map(|o| o.kind) {
                        Some(ts_r_types::ObjectKind::Reference { target, .. }) => {
                            match s.object_of(target).map(|o| o.kind) {
                                Some(ts_r_types::ObjectKind::Declared { sym, .. }) => {
                                    self.c.is_readonly_array_symbol(sym)
                                }
                                _ => false,
                            }
                        }
                        _ => false,
                    };
                    return self.create_array(w, ro);
                }
            }
        }
        // Tuples widen element-wise (`var [a, b] = [undefined, null]` gives `any`s).
        if tr.contains(TypeTraits::CONTAINS_WIDENING)
            && let Some(o) = s.object_of(t)
            && let ts_r_types::ObjectKind::Reference { target, args } = o.kind
            && s.tuple_shape_of(target).is_some_and(|sh| {
                !sh.elements
                    .iter()
                    .any(|e| e.kind == ts_r_types::TupleElement::VARIADIC)
            })
        {
            let items = s.list_items(args).to_vec();
            let mut changed = false;
            let mut out = Vec::with_capacity(items.len());
            for x in items {
                let w = self.widened_type(x);
                changed |= w != x;
                out.push(w);
            }
            if changed {
                return self.store().reference(target, &out);
            }
        }
        t
    }

    /// The regular (non-fresh) version of a type, recursively for unions.
    pub fn regular_of(&mut self, t: TypeId) -> TypeId {
        let s = self.store();
        if s.flags(t).contains(TypeFlags::UNION)
            && s.traits(t).intersects(TypeTraits::CONTAINS_OBJECT_LITERAL)
        {
            return self.map_type(t, |ck, m| ck.regular_of(m));
        }
        s.regular(t)
    }

    /// Maps each union member (or the type itself) through `f`, re-forming a union.
    pub fn map_type(
        &mut self,
        t: TypeId,
        mut f: impl FnMut(&mut Self, TypeId) -> TypeId,
    ) -> TypeId {
        let s = self.store();
        if !s.flags(t).contains(TypeFlags::UNION) {
            return f(self, t);
        }
        let members = s.members_of_union(t).to_vec();
        let mut out = Vec::with_capacity(members.len());
        let mut changed = false;
        for m in members.iter() {
            let r = f(self, *m);
            changed |= r != *m;
            out.push(r);
        }
        if !changed {
            return t;
        }
        // A union with an origin keeps the aliased unions that map to themselves (not an
        // origin that names no member, such as `keyof A`).
        if let Some(origin) = self.store().union_origin(t)
            && origin_items_are_members(self.store(), origin, &members)
        {
            let origin = origin.to_vec();
            let mut items = Vec::with_capacity(origin.len());
            for x in origin {
                let ms = self.union_members(x);
                if ms.len() > 1 {
                    let mapped: Vec<TypeId> = ms
                        .iter()
                        .map(|m| {
                            let i = members.iter().position(|y| y == m).unwrap_or(0);
                            out.get(i).copied().unwrap_or(*m)
                        })
                        .collect();
                    if mapped == ms {
                        items.push(x);
                    } else {
                        items.extend(mapped);
                    }
                } else {
                    let i = members.iter().position(|y| *y == x);
                    items.push(i.and_then(|i| out.get(i).copied()).unwrap_or(x));
                }
            }
            return self.union(&items);
        }
        self.union(&out)
    }

    /// Keeps the union members for which `keep` is true.
    pub fn filter_type(
        &mut self,
        t: TypeId,
        mut keep: impl FnMut(&mut Self, TypeId) -> bool,
    ) -> TypeId {
        let s = self.store();
        if !s.flags(t).contains(TypeFlags::UNION) {
            return if keep(self, t) { t } else { TypeId::NEVER };
        }
        let members = s.members_of_union(t).to_vec();
        let mut out = Vec::with_capacity(members.len());
        for &m in &members {
            if keep(self, m) {
                out.push(m);
            }
        }
        if out.len() == members.len() {
            return t;
        }
        // A union with an origin keeps the aliased unions that survive whole
        // (`A | undefined` without `undefined` is `A`).
        if let Some(origin) = s.union_origin(t)
            && origin_items_are_members(s, origin, &members)
        {
            let mut items = Vec::with_capacity(origin.len());
            for &x in origin {
                if s.flags(x).contains(TypeFlags::UNION) {
                    let ms = s.members_of_union(x);
                    if ms.iter().all(|m| out.contains(m)) {
                        items.push(x);
                    } else {
                        items.extend(ms.iter().copied().filter(|m| out.contains(m)));
                    }
                } else if out.contains(&x) {
                    items.push(x);
                }
            }
            return self.union_with(&items, Reduction::None, AliasId::NONE);
        }
        self.union_with(&out, Reduction::None, AliasId::NONE)
    }

    /// Members of a union, or the type itself.
    pub fn union_members(&self, t: TypeId) -> Vec<TypeId> {
        let s = self.store();
        if s.flags(t).contains(TypeFlags::UNION) {
            s.members_of_union(t).to_vec()
        } else {
            vec![t]
        }
    }

    pub fn some_member(&self, t: TypeId, mut f: impl FnMut(TypeId) -> bool) -> bool {
        let s = self.store();
        if s.flags(t).contains(TypeFlags::UNION) {
            s.members_of_union(t).iter().any(|&m| f(m))
        } else {
            f(t)
        }
    }

    pub fn every_member(&self, t: TypeId, mut f: impl FnMut(TypeId) -> bool) -> bool {
        let s = self.store();
        if s.flags(t).contains(TypeFlags::UNION) {
            s.members_of_union(t).iter().all(|&m| f(m))
        } else {
            f(t)
        }
    }

    /// `T | undefined` (or `T` without `strictNullChecks`).
    pub fn add_undefined(&mut self, t: TypeId) -> TypeId {
        if !self.c.opts.strict_null_checks {
            return t;
        }
        self.union2(t, TypeId::UNDEFINED)
    }

    /// `T | undefined` for optional properties and parameters (the `missing` type under
    /// `exactOptionalPropertyTypes`).
    pub fn add_optionality(&mut self, t: TypeId) -> TypeId {
        if !self.c.opts.strict_null_checks {
            return t;
        }
        let u = if self.c.opts.exact_optional_property_types {
            TypeId::MISSING
        } else {
            TypeId::UNDEFINED
        };
        self.union2(t, u)
    }

    /// Removes `null` and `undefined`.
    pub fn non_nullable(&mut self, t: TypeId) -> TypeId {
        if !self.c.opts.strict_null_checks {
            return t;
        }
        // `unknown` without `null` and `undefined` is `{}` (`u!`, `u ?? x`).
        if t == TypeId::UNKNOWN {
            return TypeId::EMPTY_OBJECT;
        }
        let r = self.filter_type(t, |ck, m| !ck.is_nullish_member(m));
        self.non_nullable_generic_members(r)
    }

    /// `t` without its `null`, `undefined` and `void` members (generic members unchanged).
    pub fn remove_nullish(&mut self, t: TypeId) -> TypeId {
        if !self.c.opts.strict_null_checks {
            return t;
        }
        self.filter_type(t, |ck, m| !ck.is_nullish_member(m))
    }

    /// Generic members of `t` that may still be `null` or `undefined` (an unconstrained `T`,
    /// or one constrained to a nullable type) become `NonNullable<T>`: tsc 7 prints
    /// `if (a) a` and `a!` for `a: T` as `NonNullable<T>` (`T & {}`).
    pub fn non_nullable_generic_members(&mut self, t: TypeId) -> TypeId {
        if !self.c.opts.strict_null_checks
            || !self
                .store()
                .traits(t)
                .contains(TypeTraits::CONTAINS_TYPE_PARAMETER)
                && !self.store().flags(t).intersects(TypeFlags::INSTANTIABLE)
        {
            return t;
        }
        self.map_type(t, |ck, m| {
            if ck.store().flags(m).intersects(TypeFlags::INSTANTIABLE)
                && ck.generic_may_be_nullish(m, 0)
            {
                ck.non_nullable_of_generic(m)
            } else {
                m
            }
        })
    }

    /// Whether a value of the instantiable type `m` may be `null` or `undefined`: its
    /// constraint (none, `unknown` or `any` count as yes) includes a nullish type.
    fn generic_may_be_nullish(&mut self, m: TypeId, depth: u32) -> bool {
        if depth > 8 {
            return false;
        }
        let f = self.store().flags(m);
        if f.intersects(TypeFlags::NULLABLE | TypeFlags::VOID | TypeFlags::ANY | TypeFlags::UNKNOWN)
        {
            return true;
        }
        if f.contains(TypeFlags::UNION) {
            let ms = self.store().members_of_union(m).to_vec();
            return ms
                .into_iter()
                .any(|x| self.generic_may_be_nullish(x, depth + 1));
        }
        if f.intersects(TypeFlags::INSTANTIABLE) {
            let c = self.base_constraint(m);
            if c == m || c == TypeId::UNRESOLVED {
                return true;
            }
            return self.generic_may_be_nullish(c, depth + 1);
        }
        false
    }

    /// `NonNullable<m>` (the lib alias instantiated, else `m & {}`).
    pub(crate) fn non_nullable_of_generic(&mut self, m: TypeId) -> TypeId {
        if let Some(sym) = self
            .c
            .global_symbol("NonNullable", ts_r_binder::meaning::TYPE)
            && self
                .c
                .sym_flags(sym)
                .intersects(ts_r_binder::SymbolFlags::TYPE_ALIAS)
        {
            let local = self.local_type_params(sym);
            if local.len() == 1 {
                let d = self.declared_type_of_symbol(sym);
                let mp = self.store().mapper(&local, &[m]);
                return self.instantiate(d, mp);
            }
        }
        self.intersection(&[m, TypeId::EMPTY_OBJECT])
    }

    /// `null`, `undefined`, `void`, or a deferred conditional type whose branches are only
    /// those (or `never`): what removing nullability drops from a union.
    pub fn is_nullish_member(&mut self, m: TypeId) -> bool {
        let f = self.store().flags(m);
        if f.intersects(TypeFlags::NULLABLE | TypeFlags::VOID) {
            return true;
        }
        // `T & null` (from `typeof t === "object"` on a generic `t`).
        if f.contains(TypeFlags::INTERSECTION) {
            let s = self.store();
            return s
                .members_of_union(m)
                .iter()
                .any(|&x| s.flags(x).intersects(TypeFlags::NULLABLE | TypeFlags::VOID));
        }
        if f.contains(TypeFlags::CONDITIONAL)
            && let Some(u) = self.conditional_branches_union(m)
        {
            return self.every_member(u, |b| {
                self.store()
                    .flags(b)
                    .intersects(TypeFlags::NULLABLE | TypeFlags::VOID | TypeFlags::NEVER)
            });
        }
        false
    }

    /// Removes `undefined` (optional property reads under `exactOptionalPropertyTypes`).
    pub fn remove_missing(&mut self, t: TypeId) -> TypeId {
        self.filter_type(t, |_, m| m != TypeId::MISSING)
    }

    /// The type of an optional property without the `undefined` its optionality added:
    /// what an implicit index signature sees (`{ a?: number }` relates to and infers to
    /// `{ [k: string]: number }`). Under `exactOptionalPropertyTypes` only `missing` goes.
    pub fn remove_optionality(&mut self, t: TypeId) -> TypeId {
        if self.c.opts.exact_optional_property_types {
            return self.remove_missing(t);
        }
        if !self.c.opts.strict_null_checks {
            return t;
        }
        self.filter_type(t, |_, m| m != TypeId::MISSING && m != TypeId::UNDEFINED)
    }

    /// The enum a literal of it belongs to.
    pub fn enum_type_of_literal(&mut self, t: TypeId) -> TypeId {
        let s = self.store();
        match s.kind(t) {
            TypeKind::EnumLiteral { member, .. } => match self.c.sym_parent(member) {
                Some(e) => self.declared_type_of_symbol(e),
                None => t,
            },
            _ => t,
        }
    }

    pub fn is_unit(&self, t: TypeId) -> bool {
        let s = self.store();
        s.flags(t).intersects(TypeFlags::UNIT)
    }

    /// `true` when the type is a union containing a literal type, or a literal type.
    pub fn is_literal_like(&self, t: TypeId) -> bool {
        self.some_member(t, |m| {
            self.store()
                .flags(m)
                .intersects(TypeFlags::LITERAL | TypeFlags::UNIQUE_ES_SYMBOL)
        })
    }

    pub fn anon_kind_function(&self) -> u8 {
        anon::FUNCTION_EXPR
    }
}

/// Whether every origin item of a union is one of its `members` or a union (not a type
/// that only names them, like the `keyof A` origin of `"a" | "b"`).
fn origin_items_are_members(
    s: &ts_r_types::TypeStore,
    origin: &[TypeId],
    members: &[TypeId],
) -> bool {
    origin
        .iter()
        .all(|&x| s.flags(x).contains(TypeFlags::UNION) || members.contains(&x))
}
