//! The interned type store.

use std::hash::{BuildHasher, Hash};
use std::sync::{Arc, OnceLock, RwLock};

use rustc_hash::{FxBuildHasher, FxHashMap};

use crate::chunk::ChunkVec;
use crate::flags::{ObjectFlags, SigFlags, TypeFlags, TypeTraits};
use crate::{SymId, TypeId};

/// An interned string (literal values, property names).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct StrId(pub u32);

/// An interned, immutable list of types. `ListId::EMPTY` is the empty list.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
pub struct ListId(pub u32);

impl ListId {
    pub const EMPTY: ListId = ListId(0);
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ObjId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct SigId(pub u32);

/// An interned type mapper (`sources[i] -> targets[i]`). `MapperId::IDENTITY` maps nothing.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
pub struct MapperId(pub u32);

impl MapperId {
    pub const IDENTITY: MapperId = MapperId(0);
    /// Maps every type parameter to the wildcard type (tsc's permissive instantiation, used
    /// to decide that a conditional type is definitely false).
    pub const PERMISSIVE: MapperId = MapperId(u32::MAX);
    /// Maps every type parameter to an unconstrained copy of itself (restrictive
    /// instantiation, used to decide that a conditional type is definitely true).
    pub const RESTRICTIVE: MapperId = MapperId(u32::MAX - 1);

    /// One of the special mappers that map every type parameter.
    #[inline]
    pub fn is_special(self) -> bool {
        self.0 >= u32::MAX - 1
    }
}

/// The alias a union, intersection or anonymous type was written through
/// (`type U = A | B` prints as `U`). `AliasId::NONE` = no alias.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
pub struct AliasId(pub u32);

impl AliasId {
    pub const NONE: AliasId = AliasId(0);
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct TypeParamId(pub u32);

/// What a type is. 12 bytes; with flags and traits a [`TypeData`] is 20 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TypeKind {
    /// `any`, `string`, `never`, ... (the flags say which); also `true`/`false`.
    Intrinsic,
    StringLiteral(StrId),
    /// The value's `f64` bits, low and high halves.
    NumberLiteral(u32, u32),
    /// Decimal digits with a leading `-` when negative, no `n` suffix.
    BigIntLiteral(StrId),
    /// An enum member type: the enum member symbol and its literal value type.
    EnumLiteral {
        member: SymId,
        value: TypeId,
    },
    /// A non-literal enum (some member is computed): the enum symbol.
    Enum(SymId),
    /// `unique symbol` of a declaration.
    UniqueSymbol(SymId),
    TypeParameter(TypeParamId),
    /// Members are sorted in the canonical order and de-duplicated.
    Union {
        list: ListId,
        alias: AliasId,
    },
    Intersection {
        list: ListId,
        alias: AliasId,
    },
    Object(ObjId),
    /// `keyof T` for a generic `T`.
    Index(TypeId),
    IndexedAccess {
        obj: TypeId,
        index: TypeId,
    },
    /// A deferred conditional type: index of its [`ConditionalData`]
    /// ([`TypeStore::conditional_data`]).
    Conditional(u32),
    Substitution {
        base: TypeId,
        constraint: TypeId,
    },
    /// A template literal type: index of its [`TemplateData`] ([`TypeStore::template_parts`]).
    TemplateLiteral(u32),
    /// `Uppercase<T>` and friends: the intrinsic alias symbol and the operand.
    StringMapping {
        sym: SymId,
        ty: TypeId,
    },
    /// A forward reference to a non-generic type alias whose declared type is still being
    /// resolved (`type J = string | J[]`), or to the type of a type node that names one
    /// (the element of `type V = (string | V)[]`): index into the level's forwards. Once
    /// linked, every accessor sees the target type; until then it behaves as `UNRESOLVED`.
    Forward(u32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TypeData {
    pub flags: TypeFlags,
    pub traits: TypeTraits,
    pub kind: TypeKind,
}

/// What kind of object type an [`ObjectData`] is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ObjectKind {
    /// A class or interface declaration. `type_params` are the outer and local type
    /// parameters; `this_type` is the `this` type parameter.
    Declared {
        sym: SymId,
        type_params: ListId,
        this_type: TypeId,
    },
    /// `target<args>`; `target` is a generic `Declared` type or a tuple target. `args` may
    /// have one extra trailing element: the `this` type argument.
    Reference { target: TypeId, args: ListId },
    /// A tuple shape; instantiated by `Reference`. Index into [`TypeStore::tuple_shape`].
    TupleTarget { shape: u32 },
    /// A type literal, function type, `typeof` of a function/class/namespace/enum, keyed by
    /// its declaring node `(file, node)` (`kind` distinguishes several anonymous types of one
    /// node, e.g. a class's static side), instantiated with `mapper`.
    Anonymous {
        decl: (u32, u32),
        kind: u8,
        mapper: MapperId,
    },
    /// Members supplied when the type is created (object literals, synthesized types).
    Literal { sym: SymId },
    /// A mapped type `{ [K in C as N]: T }` keyed by its declaring `MappedType` node
    /// `(file, node)`, instantiated with `mapper` (which maps the type parameters in scope
    /// at the declaration, never the mapped type's own `K`).
    Mapped { decl: (u32, u32), mapper: MapperId },
}

/// The parts of a conditional type `C extends E ? X : Y` that do not depend on a mapper,
/// created once per `ConditionalType` node (see [`TypeStore::conditional_root`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ConditionalRoot {
    /// The `ConditionalType` node `(file, node)`; the branch types are read from it lazily.
    pub decl: (u32, u32),
    pub check: TypeId,
    pub extends: TypeId,
    /// The `infer` type parameters declared in the `extends` clause, in source order.
    pub infer_params: ListId,
    /// The type parameters in scope at the node (what a conditional's mapper maps).
    pub outer_params: ListId,
    /// The check type is a naked type parameter: the type distributes over unions.
    pub distributive: bool,
}

/// A deferred conditional type: its root and the mapper of the root's outer parameters.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ConditionalData {
    pub root: u32,
    pub mapper: MapperId,
    pub alias: AliasId,
}

/// A template literal type: `texts.len() == types.len() + 1`; texts are string literal types.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TemplateData {
    pub texts: ListId,
    pub types: ListId,
}

/// One element of a tuple shape.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TupleElement {
    /// `0` required, `1` optional (`T?`), `2` rest (`...T[]`), `3` variadic (`...T`).
    pub kind: u8,
    pub label: Option<StrId>,
}

impl TupleElement {
    pub const REQUIRED: u8 = 0;
    pub const OPTIONAL: u8 = 1;
    pub const REST: u8 = 2;
    pub const VARIADIC: u8 = 3;
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TupleShape {
    pub elements: Box<[TupleElement]>,
    pub readonly: bool,
}

impl TupleShape {
    /// Number of required elements (`[...X[], Y]` needs one value).
    pub fn min_length(&self) -> usize {
        self.elements
            .iter()
            .filter(|e| e.kind == TupleElement::REQUIRED)
            .count()
    }

    /// Has a rest or variadic element.
    pub fn has_rest(&self) -> bool {
        self.elements.iter().any(|e| e.kind >= TupleElement::REST)
    }

    pub fn fixed_length(&self) -> usize {
        self.elements
            .iter()
            .take_while(|e| e.kind < TupleElement::REST)
            .count()
    }
}

/// A resolved index signature (`[key: K]: V`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IndexInfo {
    pub key: TypeId,
    pub value: TypeId,
    pub readonly: bool,
    /// Declaring node as `(file, node)`, if any.
    pub decl: Option<(u32, u32)>,
}

/// Members of an object type, resolved once and published into the object's slot.
#[derive(Debug, Default)]
pub struct ResolvedMembers {
    /// Properties in declaration order.
    pub props: Box<[SymId]>,
    /// Names parallel to `props`.
    pub names: Box<[StrId]>,
    lookup: Option<FxHashMap<StrId, u32>>,
    pub call: Box<[SigId]>,
    pub construct: Box<[SigId]>,
    pub index: Box<[IndexInfo]>,
}

impl ResolvedMembers {
    pub fn new(
        props: Vec<(StrId, SymId)>,
        call: Vec<SigId>,
        construct: Vec<SigId>,
        index: Vec<IndexInfo>,
    ) -> ResolvedMembers {
        let names: Box<[StrId]> = props.iter().map(|p| p.0).collect();
        let props: Box<[SymId]> = props.iter().map(|p| p.1).collect();
        let lookup = (names.len() > 12).then(|| {
            let mut m = FxHashMap::default();
            for (i, &n) in names.iter().enumerate() {
                m.entry(n).or_insert(i as u32);
            }
            m
        });
        ResolvedMembers {
            props,
            names,
            lookup,
            call: call.into_boxed_slice(),
            construct: construct.into_boxed_slice(),
            index: index.into_boxed_slice(),
        }
    }

    pub fn empty() -> ResolvedMembers {
        ResolvedMembers::default()
    }

    /// The property named `name`.
    pub fn find(&self, name: StrId) -> Option<SymId> {
        match &self.lookup {
            Some(m) => m.get(&name).map(|&i| self.props[i as usize]),
            None => self
                .names
                .iter()
                .position(|&n| n == name)
                .map(|i| self.props[i]),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.props.is_empty()
            && self.call.is_empty()
            && self.construct.is_empty()
            && self.index.is_empty()
    }
}

#[derive(Debug)]
pub struct ObjectData {
    pub kind: ObjectKind,
    pub flags: ObjectFlags,
    pub alias: AliasId,
    members: OnceLock<Box<ResolvedMembers>>,
    /// For fresh object literal types: the regular (non-fresh) twin.
    regular: OnceLock<TypeId>,
    /// The store level the object lives in (see [`TypeStore::child`]).
    level: u8,
}

impl ObjectData {
    pub fn members(&self) -> Option<&ResolvedMembers> {
        self.members.get().map(|b| &**b)
    }
}

/// A function parameter of a signature.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Param {
    pub name: StrId,
    pub ty: TypeId,
    pub optional: bool,
    /// Declaring parameter symbol (`SymId::NONE` when synthesized).
    pub sym: SymId,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PredicateKind {
    /// `x is T`
    Identifier,
    /// `this is T`
    This,
    /// `asserts x` / `asserts x is T`
    AssertsIdentifier,
    /// `asserts this` / `asserts this is T`
    AssertsThis,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TypePredicate {
    pub kind: PredicateKind,
    pub param_index: u32,
    pub param_name: StrId,
    pub ty: Option<TypeId>,
}

#[derive(Debug)]
pub struct Signature {
    pub flags: SigFlags,
    /// Declaring node `(file, node)`.
    pub decl: Option<(u32, u32)>,
    pub type_params: ListId,
    pub params: Box<[Param]>,
    /// Declared `this` parameter type.
    pub this_type: Option<TypeId>,
    /// Number of required arguments.
    pub min_args: u32,
    /// Set at creation when known, otherwise computed lazily by the checker.
    ret: OnceLock<TypeId>,
    predicate: OnceLock<Option<TypePredicate>>,
    /// `(original signature, mapper)` for instantiated signatures.
    pub target: Option<(SigId, MapperId)>,
    /// The store level the signature lives in (set by [`TypeStore::add_signature`]).
    level: u8,
}

impl Signature {
    pub fn new(
        flags: SigFlags,
        decl: Option<(u32, u32)>,
        type_params: ListId,
        params: Vec<Param>,
        this_type: Option<TypeId>,
        min_args: u32,
        ret: Option<TypeId>,
    ) -> Signature {
        let r = OnceLock::new();
        if let Some(t) = ret {
            let _ = r.set(t);
        }
        Signature {
            flags,
            decl,
            type_params,
            params: params.into_boxed_slice(),
            this_type,
            min_args,
            ret: r,
            predicate: OnceLock::new(),
            target: None,
            level: 0,
        }
    }

    pub fn with_target(mut self, target: SigId, mapper: MapperId) -> Signature {
        self.target = Some((target, mapper));
        self
    }

    pub fn with_predicate(self, p: Option<TypePredicate>) -> Signature {
        let _ = self.predicate.set(p);
        self
    }

    pub fn ret(&self) -> Option<TypeId> {
        self.ret.get().copied()
    }

    /// Publishes the lazily computed return type; returns the published value. A type from
    /// a higher store level than the signature's is returned but not published (a lower
    /// level is shared by programs that cannot see it).
    pub fn set_ret(&self, t: TypeId) -> TypeId {
        if crate::level_of(t.0) > self.level {
            crate::note_miss(0);
            return t;
        }
        let _ = self.ret.set(t);
        *self.ret.get().unwrap_or(&t)
    }

    pub fn predicate(&self) -> Option<Option<TypePredicate>> {
        self.predicate.get().copied()
    }

    pub fn set_predicate(&self, p: Option<TypePredicate>) -> Option<TypePredicate> {
        if p.and_then(|p| p.ty)
            .is_some_and(|t| crate::level_of(t.0) > self.level)
        {
            crate::note_miss(1);
            return p;
        }
        let _ = self.predicate.set(p);
        self.predicate.get().copied().unwrap_or(p)
    }

    pub fn has_rest(&self) -> bool {
        self.flags.contains(SigFlags::HAS_REST)
    }
}

#[derive(Debug)]
pub struct TypeParamData {
    pub sym: SymId,
    pub is_this: bool,
    /// For a type parameter cloned while instantiating a generic signature: the original
    /// type parameter and the outer mapper applied to its constraint.
    pub origin: Option<(TypeId, MapperId)>,
    /// An unconstrained copy made by restrictive instantiation.
    pub restrictive: bool,
    constraint: OnceLock<Option<TypeId>>,
    default: OnceLock<Option<TypeId>>,
    level: u8,
}

impl TypeParamData {
    pub fn constraint(&self) -> Option<Option<TypeId>> {
        self.constraint.get().copied()
    }
    pub fn set_constraint(&self, c: Option<TypeId>) -> Option<TypeId> {
        if c.is_some_and(|t| crate::level_of(t.0) > self.level) {
            crate::note_miss(2);
            return c;
        }
        let _ = self.constraint.set(c);
        self.constraint.get().copied().unwrap_or(c)
    }
    pub fn default(&self) -> Option<Option<TypeId>> {
        self.default.get().copied()
    }
    pub fn set_default(&self, c: Option<TypeId>) -> Option<TypeId> {
        if c.is_some_and(|t| crate::level_of(t.0) > self.level) {
            crate::note_miss(3);
            return c;
        }
        let _ = self.default.set(c);
        self.default.get().copied().unwrap_or(c)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct AliasInfo {
    pub sym: SymId,
    pub args: ListId,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Mapper {
    sources: ListId,
    targets: ListId,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Key {
    Str(StrId, bool),
    Num(u64, bool),
    BigInt(StrId, bool),
    EnumLit(SymId, TypeId, bool),
    Enum(SymId),
    Unique(SymId),
    Union(ListId, AliasId),
    Inter(ListId, AliasId),
    Declared(SymId),
    Reference(TypeId, ListId, AliasId),
    Tuple(u32),
    Anonymous((u32, u32), u8, MapperId, AliasId),
    TypeParam(SymId, bool),
    ClonedParam(TypeId, MapperId),
    Index(TypeId),
    IndexedAccess(TypeId, TypeId),
    Substitution(TypeId, TypeId),
    StringMapping(SymId, TypeId),
    Mapped((u32, u32), MapperId, AliasId),
    Conditional(ConditionalData),
    Template(TemplateData),
    Restrictive(TypeId),
    NamedError(StrId),
    Forward(SymId),
    ForwardNode((u32, u32)),
    ForwardNodeArgs((u32, u32), ListId),
}

const SHARDS: usize = 64;

struct Sharded<K, V> {
    shards: Box<[RwLock<FxHashMap<K, V>>]>,
}

impl<K: Hash + Eq + Clone, V: Copy> Sharded<K, V> {
    fn new() -> Self {
        let mut v = Vec::with_capacity(SHARDS);
        v.resize_with(SHARDS, || RwLock::new(FxHashMap::default()));
        Sharded {
            shards: v.into_boxed_slice(),
        }
    }

    fn shard<Q: Hash + ?Sized>(&self, k: &Q) -> &RwLock<FxHashMap<K, V>> {
        let h = FxBuildHasher.hash_one(k);
        &self.shards[(h >> 32) as usize % SHARDS]
    }

    fn get_or_insert_with(&self, k: K, make: impl FnOnce() -> V) -> V {
        let shard = self.shard(&k);
        if let Some(&v) = shard.read().unwrap_or_else(|e| e.into_inner()).get(&k) {
            return v;
        }
        let mut w = shard.write().unwrap_or_else(|e| e.into_inner());
        if let Some(&v) = w.get(&k) {
            return v;
        }
        let v = make();
        w.insert(k, v);
        v
    }

    fn get(&self, k: &K) -> Option<V> {
        self.shard(k)
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(k)
            .copied()
    }

    fn insert(&self, k: K, v: V) {
        self.shard(&k)
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(k, v);
    }
}

/// Relations with their own caches.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RelationKind {
    Identity,
    Subtype,
    StrictSubtype,
    Assignable,
    Comparable,
}

/// A shared cache of final relation results keyed by `(source, target)`.
pub struct RelationCache {
    map: Sharded<u64, bool>,
}

impl std::fmt::Debug for RelationCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RelationCache")
    }
}

impl RelationCache {
    fn new() -> Self {
        RelationCache {
            map: Sharded::new(),
        }
    }

    #[inline]
    fn key(s: TypeId, t: TypeId) -> u64 {
        (s.0 as u64) << 32 | t.0 as u64
    }

    pub fn get(&self, s: TypeId, t: TypeId) -> Option<bool> {
        self.map.get(&Self::key(s, t))
    }

    /// Records a final result. `Maybe` results of an unfinished comparison must never be
    /// stored (design invariant 4).
    pub fn set(&self, s: TypeId, t: TypeId, related: bool) {
        self.map.insert(Self::key(s, t), related);
    }
}

/// A relation cache seen through a layered store: each result lives in the level of its
/// higher operand, so results about shared (lower-level) types are shared too.
#[derive(Clone, Copy, Debug)]
pub struct RelationCacheRef<'a> {
    store: &'a TypeStore,
    kind: RelationKind,
}

impl RelationCacheRef<'_> {
    #[inline]
    fn cache(&self, s: TypeId, t: TypeId) -> &RelationCache {
        let l = level_of(s.0).max(level_of(t.0));
        &self.store.at(l).relations[self.kind as usize]
    }

    pub fn get(&self, s: TypeId, t: TypeId) -> Option<bool> {
        self.cache(s, t).get(s, t)
    }

    /// Records a final result. `Maybe` results of an unfinished comparison must never be
    /// stored (design invariant 4).
    pub fn set(&self, s: TypeId, t: TypeId, related: bool) {
        self.cache(s, t).set(s, t, related);
    }
}

/// Bit position of the store level in every id the store hands out.
pub const LEVEL_SHIFT: u32 = 30;
/// Number of store levels (see [`TypeStore::child`]).
pub const MAX_LEVELS: usize = 4;
const INDEX_MASK: u32 = (1 << LEVEL_SHIFT) - 1;

/// The store level encoded in a raw id (`u32::MAX`, the `NONE` sentinel, is level 0).
#[inline]
pub fn level_of(raw: u32) -> u8 {
    if raw == u32::MAX {
        0
    } else {
        (raw >> LEVEL_SHIFT) as u8
    }
}

/// The level of a mapper (the special mappers belong to every level: 0).
#[inline]
pub fn mapper_level(m: MapperId) -> u8 {
    if m.is_special() { 0 } else { level_of(m.0) }
}

/// The index within its level of a raw id.
#[inline]
pub fn index_of(raw: u32) -> u32 {
    raw & INDEX_MASK
}

/// A raw id from a level and an index within it.
#[inline]
pub fn make_id(level: u8, index: u32) -> u32 {
    (level as u32) << LEVEL_SHIFT | index
}

/// The shared, append-only type store.
///
/// Stores are layered (history, design record 0005): [`TypeStore::new`] makes a root
/// (level 0); [`TypeStore::child`] makes a store one level up that sees everything of its
/// ancestors. Every id carries its level in its top bits. Whatever is created lands in the
/// level of its highest component (a union of lib types lives in the lib level, shared by
/// every program built on it); strings, number literals and tuple shapes always live in the
/// root; checker-side types ([`TypeStore::opaque`], [`TypeStore::mapped_object`]) live in
/// the store they were created through. Write-once slots of an entity never take a value
/// from a higher level (it is returned but not published; resolved members are kept in a
/// shadow table of the value's level instead), so a lower level never refers to anything
/// of a higher one and can be shared read-mostly by many programs.
pub struct TypeStore {
    level: u8,
    /// Ancestors by level (`chain[k]` has level `k`), for `k < level`.
    chain: Box<[Arc<TypeStore>]>,
    /// First file id of each level `0..=level`: declarations of file `f` belong to the
    /// highest level whose start is `<= f`.
    file_starts: [u32; MAX_LEVELS],
    types: ChunkVec<TypeData>,
    intern: Sharded<Key, TypeId>,
    strings: ChunkVec<Box<str>>,
    string_map: Sharded<Box<str>, StrId>,
    lists: ChunkVec<Box<[TypeId]>>,
    list_map: Sharded<Box<[TypeId]>, ListId>,
    objects: ChunkVec<ObjectData>,
    sigs: ChunkVec<Signature>,
    params: ChunkVec<TypeParamData>,
    aliases: ChunkVec<AliasInfo>,
    alias_map: Sharded<AliasInfo, AliasId>,
    mappers: ChunkVec<Mapper>,
    mapper_map: Sharded<Mapper, MapperId>,
    tuples: ChunkVec<TupleShape>,
    tuple_map: Sharded<TupleShape, u32>,
    cond_roots: ChunkVec<ConditionalRoot>,
    cond_root_map: Sharded<(u32, u32), u32>,
    conds: ChunkVec<ConditionalData>,
    templates: ChunkVec<TemplateData>,
    /// Forward references: the alias (`None` for a type node's) and the linked target.
    forwards: ChunkVec<(Option<SymId>, OnceLock<TypeId>)>,
    /// Forward references to a type node under type arguments: forward index -> (node,
    /// arguments) (see [`TypeStore::forward_node_args`]).
    forward_args: Sharded<u32, ((u32, u32), ListId)>,
    relations: [RelationCache; 5],
    /// Members of lower-level objects whose resolution involves this level's entities:
    /// object id (raw) -> index into `shadow_vals`.
    shadow_members: Sharded<u32, u32>,
    shadow_vals: ChunkVec<ResolvedMembers>,
    /// Names of the named error types (root store only): type -> printed name.
    error_names: Sharded<TypeId, StrId>,
}

impl std::fmt::Debug for TypeStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypeStore")
            .field("level", &self.level)
            .field("types", &self.types.len())
            .field("objects", &self.objects.len())
            .field("sigs", &self.sigs.len())
            .finish()
    }
}

impl Default for TypeStore {
    fn default() -> Self {
        Self::new()
    }
}

fn data(flags: TypeFlags) -> TypeData {
    TypeData {
        flags,
        traits: TypeTraits::NONE,
        kind: TypeKind::Intrinsic,
    }
}

impl TypeStore {
    fn empty(level: u8, chain: Box<[Arc<TypeStore>]>, file_starts: [u32; MAX_LEVELS]) -> Self {
        TypeStore {
            level,
            chain,
            file_starts,
            types: ChunkVec::new(),
            forwards: ChunkVec::new(),
            forward_args: Sharded::new(),
            intern: Sharded::new(),
            strings: ChunkVec::new(),
            string_map: Sharded::new(),
            lists: ChunkVec::new(),
            list_map: Sharded::new(),
            objects: ChunkVec::new(),
            sigs: ChunkVec::new(),
            params: ChunkVec::new(),
            aliases: ChunkVec::new(),
            alias_map: Sharded::new(),
            mappers: ChunkVec::new(),
            mapper_map: Sharded::new(),
            tuples: ChunkVec::new(),
            tuple_map: Sharded::new(),
            cond_roots: ChunkVec::new(),
            cond_root_map: Sharded::new(),
            conds: ChunkVec::new(),
            templates: ChunkVec::new(),
            relations: [
                RelationCache::new(),
                RelationCache::new(),
                RelationCache::new(),
                RelationCache::new(),
                RelationCache::new(),
            ],
            shadow_members: Sharded::new(),
            shadow_vals: ChunkVec::new(),
            error_names: Sharded::new(),
        }
    }

    pub fn new() -> TypeStore {
        let s = TypeStore::empty(0, Box::new([]), [0; MAX_LEVELS]);
        // Index 0 of side tables are sentinels.
        s.lists.push(Box::new([]));
        s.list_map.insert(Box::new([]), ListId::EMPTY);
        s.aliases.push(AliasInfo {
            sym: SymId::NONE,
            args: ListId::EMPTY,
        });
        s.mappers.push(Mapper {
            sources: ListId::EMPTY,
            targets: ListId::EMPTY,
        });
        s.strings.push("".into());
        s.string_map.insert("".into(), StrId(0));

        let push = |d: TypeData, expect: TypeId| {
            let id = s.types.push(d);
            debug_assert_eq!(id, expect.0);
        };
        let lit = |_value: bool, fresh: bool| TypeData {
            flags: TypeFlags::BOOLEAN_LITERAL,
            traits: if fresh {
                TypeTraits::FRESH
            } else {
                TypeTraits::NONE
            },
            kind: TypeKind::Intrinsic,
        };
        push(
            TypeData {
                flags: TypeFlags::ANY,
                traits: TypeTraits::CONTAINS_ERROR,
                kind: TypeKind::Intrinsic,
            },
            TypeId::ERROR,
        );
        push(data(TypeFlags::ANY), TypeId::ANY);
        push(data(TypeFlags::UNKNOWN), TypeId::UNKNOWN);
        push(data(TypeFlags::NEVER), TypeId::NEVER);
        push(data(TypeFlags::VOID), TypeId::VOID);
        push(data(TypeFlags::UNDEFINED), TypeId::UNDEFINED);
        push(data(TypeFlags::NULL), TypeId::NULL);
        push(data(TypeFlags::STRING), TypeId::STRING);
        push(data(TypeFlags::NUMBER), TypeId::NUMBER);
        push(data(TypeFlags::BIGINT), TypeId::BIGINT);
        push(lit(false, false), TypeId::FALSE);
        push(lit(true, false), TypeId::TRUE);
        let bool_list = ListId(s.lists.push(Box::new([TypeId::FALSE, TypeId::TRUE])));
        s.list_map
            .insert(Box::new([TypeId::FALSE, TypeId::TRUE]), bool_list);
        push(
            TypeData {
                flags: TypeFlags::UNION | TypeFlags::BOOLEAN,
                traits: TypeTraits::NONE,
                kind: TypeKind::Union {
                    list: bool_list,
                    alias: AliasId::NONE,
                },
            },
            TypeId::BOOLEAN,
        );
        s.intern
            .insert(Key::Union(bool_list, AliasId::NONE), TypeId::BOOLEAN);
        push(lit(false, true), TypeId::FRESH_FALSE);
        push(lit(true, true), TypeId::FRESH_TRUE);
        push(data(TypeFlags::ES_SYMBOL), TypeId::SYMBOL);
        push(data(TypeFlags::NON_PRIMITIVE), TypeId::OBJECT);
        // `{}`: an empty anonymous object with resolved (empty) members.
        let empty = s.objects.push(ObjectData {
            kind: ObjectKind::Literal { sym: SymId::NONE },
            flags: ObjectFlags::ANONYMOUS,
            alias: AliasId::NONE,
            members: OnceLock::from(Box::new(ResolvedMembers::empty())),
            regular: OnceLock::new(),
            level: 0,
        });
        push(
            TypeData {
                flags: TypeFlags::OBJECT,
                traits: TypeTraits::NONE,
                kind: TypeKind::Object(ObjId(empty)),
            },
            TypeId::EMPTY_OBJECT,
        );
        push(data(TypeFlags::NEVER), TypeId::SILENT_NEVER);
        push(data(TypeFlags::ANY), TypeId::WILDCARD);
        push(data(TypeFlags::UNDEFINED), TypeId::MISSING);
        push(data(TypeFlags::NEVER), TypeId::IMPLICIT_NEVER);
        push(
            TypeData {
                flags: TypeFlags::UNDEFINED,
                traits: TypeTraits::CONTAINS_WIDENING,
                kind: TypeKind::Intrinsic,
            },
            TypeId::UNDEFINED_WIDENING,
        );
        push(
            TypeData {
                flags: TypeFlags::NULL,
                traits: TypeTraits::CONTAINS_WIDENING,
                kind: TypeKind::Intrinsic,
            },
            TypeId::NULL_WIDENING,
        );
        push(data(TypeFlags::ANY), TypeId::AUTO);
        push(
            TypeData {
                flags: TypeFlags::ANY,
                traits: TypeTraits::CONTAINS_ERROR,
                kind: TypeKind::Intrinsic,
            },
            TypeId::UNRESOLVED,
        );
        let empty_fresh = s.objects.push(ObjectData {
            kind: ObjectKind::Literal { sym: SymId::NONE },
            flags: ObjectFlags::ANONYMOUS
                | ObjectFlags::OBJECT_LITERAL
                | ObjectFlags::FRESH_LITERAL,
            alias: AliasId::NONE,
            members: OnceLock::from(Box::new(ResolvedMembers::empty())),
            regular: OnceLock::from(TypeId::EMPTY_OBJECT),
            level: 0,
        });
        push(
            TypeData {
                flags: TypeFlags::OBJECT,
                traits: TypeTraits::CONTAINS_OBJECT_LITERAL,
                kind: TypeKind::Object(ObjId(empty_fresh)),
            },
            TypeId::EMPTY_FRESH_OBJECT,
        );
        push(data(TypeFlags::ANY), TypeId::IMPLICIT_ANY);
        debug_assert_eq!(s.types.len() as u32, TypeId::INTRINSIC_COUNT);
        s
    }

    /// A store one level above `parent`: it sees every type of `parent` and its ancestors,
    /// and holds what is created from its own declarations. Declarations of files with ids
    /// `>= file_start` belong to it (files of lower levels come first in the program).
    ///
    /// Returns `None` when `parent` is already at the highest level.
    pub fn child(parent: &Arc<TypeStore>, file_start: u32) -> Option<TypeStore> {
        let level = parent.level + 1;
        if level as usize >= MAX_LEVELS {
            return None;
        }
        let mut chain: Vec<Arc<TypeStore>> = parent.chain.to_vec();
        chain.push(parent.clone());
        let mut starts = parent.file_starts;
        starts[level as usize] = file_start.max(parent.file_starts[parent.level as usize]);
        Some(TypeStore::empty(level, chain.into_boxed_slice(), starts))
    }

    /// This store's level (0 for a root).
    pub fn level(&self) -> u8 {
        self.level
    }

    /// The first file id whose declarations belong to this level.
    pub fn file_start(&self) -> u32 {
        self.file_starts[self.level as usize]
    }

    /// The store of level `l` (this one or an ancestor).
    #[inline]
    fn at(&self, l: u8) -> &TypeStore {
        if l == self.level {
            self
        } else {
            match self.chain.get(l as usize) {
                Some(s) => s,
                None => panic!(
                    "id of store level {l} used with a level-{} store",
                    self.level
                ),
            }
        }
    }

    #[inline]
    fn root(&self) -> &TypeStore {
        self.at(0)
    }

    /// The level of a declaration key `(file, node)`; `(u32::MAX, id)` keys by a symbol or
    /// type id instead, whose level is encoded in the id.
    #[inline]
    pub fn decl_level(&self, decl: (u32, u32)) -> u8 {
        if decl.0 == u32::MAX {
            level_of(decl.1)
        } else {
            self.file_level(decl.0)
        }
    }

    /// The level declarations of file `f` belong to.
    #[inline]
    pub fn file_level(&self, f: u32) -> u8 {
        let mut l = self.level;
        while l > 0 && f < self.file_starts[l as usize] {
            l -= 1;
        }
        l
    }

    #[inline]
    fn lt(t: TypeId) -> u8 {
        level_of(t.0)
    }

    fn lts(items: &[TypeId]) -> u8 {
        items.iter().map(|t| level_of(t.0)).max().unwrap_or(0)
    }

    /// The level of a key: the highest level among its components.
    fn key_level(&self, k: &Key) -> u8 {
        match *k {
            Key::Str(..) | Key::Num(..) | Key::BigInt(..) | Key::Tuple(_) | Key::NamedError(_) => 0,
            Key::EnumLit(s, t, _) => level_of(s.0).max(Self::lt(t)),
            Key::Enum(s) | Key::Unique(s) | Key::Declared(s) | Key::TypeParam(s, _) => {
                level_of(s.0)
            }
            Key::Union(l, a) | Key::Inter(l, a) => level_of(l.0).max(level_of(a.0)),
            Key::Reference(t, l, a) => Self::lt(t).max(level_of(l.0)).max(level_of(a.0)),
            Key::Anonymous(decl, _, m, a) | Key::Mapped(decl, m, a) => self
                .decl_level(decl)
                .max(mapper_level(m))
                .max(level_of(a.0)),
            Key::ClonedParam(t, m) => Self::lt(t).max(mapper_level(m)),
            Key::Conditional(d) => level_of(d.root)
                .max(mapper_level(d.mapper))
                .max(level_of(d.alias.0)),
            Key::Template(d) => level_of(d.texts.0).max(level_of(d.types.0)),
            Key::Restrictive(t) => Self::lt(t),
            Key::Forward(s) => level_of(s.0),
            Key::ForwardNode(decl) => self.decl_level(decl),
            Key::ForwardNodeArgs(decl, l) => self.decl_level(decl).max(level_of(l.0)),
            Key::Index(t) => Self::lt(t),
            Key::IndexedAccess(a, b) | Key::Substitution(a, b) => Self::lt(a).max(Self::lt(b)),
            Key::StringMapping(s, t) => level_of(s.0).max(Self::lt(t)),
        }
    }

    /// The level of resolved members: the highest level among what they refer to.
    pub fn members_level(&self, m: &ResolvedMembers) -> u8 {
        let mut l = 0u8;
        for &p in m.props.iter() {
            l = l.max(level_of(p.0));
        }
        for &s in m.call.iter().chain(m.construct.iter()) {
            l = l.max(level_of(s.0));
        }
        for i in m.index.iter() {
            l = l.max(Self::lt(i.key)).max(Self::lt(i.value));
            if let Some(d) = i.decl {
                l = l.max(self.decl_level(d));
            }
        }
        l
    }

    fn signature_level(&self, s: &Signature) -> u8 {
        let mut l = level_of(s.type_params.0);
        if let Some(d) = s.decl {
            l = l.max(self.decl_level(d));
        }
        for p in s.params.iter() {
            l = l.max(Self::lt(p.ty)).max(level_of(p.sym.0));
        }
        if let Some(t) = s.this_type {
            l = l.max(Self::lt(t));
        }
        if let Some(t) = s.ret() {
            l = l.max(Self::lt(t));
        }
        if let Some(Some(p)) = s.predicate()
            && let Some(t) = p.ty
        {
            l = l.max(Self::lt(t));
        }
        if let Some((sig, m)) = s.target {
            l = l.max(level_of(sig.0)).max(mapper_level(m));
        }
        l
    }

    /// Number of types created so far in this level.
    pub fn len(&self) -> usize {
        self.types.len()
    }

    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    #[inline]
    pub fn get(&self, t: TypeId) -> &TypeData {
        let s = self.at(level_of(t.0));
        let d = s.types.get(index_of(t.0));
        if let TypeKind::Forward(i) = d.kind
            && let Some(&r) = s.forwards.get(i).1.get()
        {
            return self.get(r);
        }
        d
    }

    /// The forward reference to the non-generic type alias `sym` (see
    /// [`TypeKind::Forward`]).
    pub fn forward(&self, sym: SymId) -> TypeId {
        self.forward_of(Key::Forward(sym), Some(sym))
    }

    /// The forward reference to the type of the type node `node` (file, node), linked once
    /// the alias it names is known (see [`TypeKind::Forward`]).
    pub fn forward_node(&self, node: (u32, u32)) -> TypeId {
        self.forward_of(Key::ForwardNode(node), None)
    }

    /// The forward reference to the type of the type node `node` with the outer type
    /// parameters it references replaced by `args` (in their order): the union element of
    /// `type R<T> = Array<T | R<T>>`, `R<string>`'s being `node`'s with `args = [string]`.
    /// Also whether it was created by this call (the caller links it).
    pub fn forward_node_args(&self, node: (u32, u32), args: ListId) -> (TypeId, bool) {
        let key = Key::ForwardNodeArgs(node, args);
        let l = self.key_level(&key);
        let s = self.at(l);
        if let Some(t) = s.intern.get(&key) {
            return (t, false);
        }
        let mut created = false;
        let t = self.intern(key, || {
            created = true;
            let i = s.forwards.push((None, OnceLock::new()));
            s.forward_args.insert(i, (node, args));
            let u = *self.get(TypeId::UNRESOLVED);
            let items = self.list_items(args);
            let traits = u.traits | self.combined_traits(items);
            TypeData {
                flags: u.flags,
                traits,
                kind: TypeKind::Forward(i),
            }
        });
        (t, created)
    }

    /// The node and type arguments of a [`Self::forward_node_args`] forward reference `t`
    /// (linked or not).
    pub fn forward_node_args_of(&self, t: TypeId) -> Option<((u32, u32), ListId)> {
        let s = self.at(level_of(t.0));
        match s.types.get(index_of(t.0)).kind {
            TypeKind::Forward(i) => s.forward_args.get(&i),
            _ => None,
        }
    }

    /// Whether the forward reference `t` is linked (`false` for other types).
    pub fn is_linked_forward(&self, t: TypeId) -> bool {
        let s = self.at(level_of(t.0));
        match s.types.get(index_of(t.0)).kind {
            TypeKind::Forward(i) => s.forwards.get(i).1.get().is_some(),
            _ => false,
        }
    }

    fn forward_of(&self, key: Key, sym: Option<SymId>) -> TypeId {
        let l = self.key_level(&key);
        let s = self.at(l);
        self.intern(key, || {
            let i = s.forwards.push((sym, OnceLock::new()));
            let u = *self.get(TypeId::UNRESOLVED);
            TypeData {
                flags: u.flags,
                traits: u.traits,
                kind: TypeKind::Forward(i),
            }
        })
    }

    /// The alias symbol of `t` when `t` itself is a forward reference to an alias (linked
    /// or not). Printers stop there: a linked forward closes a cycle.
    pub fn forward_alias(&self, t: TypeId) -> Option<SymId> {
        let s = self.at(level_of(t.0));
        match s.types.get(index_of(t.0)).kind {
            TypeKind::Forward(i) => s.forwards.get(i).0,
            _ => None,
        }
    }

    /// Whether `t` itself is a forward reference (to an alias or a type node, linked or not).
    pub fn is_forward(&self, t: TypeId) -> bool {
        let s = self.at(level_of(t.0));
        matches!(s.types.get(index_of(t.0)).kind, TypeKind::Forward(_))
    }

    /// The type the forward reference `t` is linked to (following chains), or `t` itself
    /// for other types and unlinked forwards. Forwards already read as their target
    /// ([`Self::get`]); this gives the target's identity too.
    pub fn unforward(&self, mut t: TypeId) -> TypeId {
        for _ in 0..16 {
            let s = self.at(level_of(t.0));
            match s.types.get(index_of(t.0)).kind {
                TypeKind::Forward(i) => match s.forwards.get(i).1.get() {
                    Some(&r) if r != t => t = r,
                    _ => break,
                },
                _ => break,
            }
        }
        t
    }

    /// The forward reference to `sym`, if one was handed out.
    pub fn find_forward(&self, sym: SymId) -> Option<TypeId> {
        let key = Key::Forward(sym);
        self.at(self.key_level(&key)).intern.get(&key)
    }

    /// Points the forward reference `f` at `t` (no-op when already linked, or when `t`
    /// lives in a level above `f`'s).
    pub fn link_forward(&self, f: TypeId, t: TypeId) {
        if level_of(t.0) > level_of(f.0) || t == f {
            return;
        }
        let s = self.at(level_of(f.0));
        if let TypeKind::Forward(i) = s.types.get(index_of(f.0)).kind {
            let _ = s.forwards.get(i).1.set(t);
        }
    }

    #[inline]
    pub fn flags(&self, t: TypeId) -> TypeFlags {
        self.get(t).flags
    }

    #[inline]
    pub fn traits(&self, t: TypeId) -> TypeTraits {
        self.get(t).traits
    }

    #[inline]
    pub fn kind(&self, t: TypeId) -> TypeKind {
        self.get(t).kind
    }

    fn intern(&self, key: Key, make: impl FnOnce() -> TypeData) -> TypeId {
        let l = self.key_level(&key);
        let s = self.at(l);
        s.intern
            .get_or_insert_with(key, || TypeId(make_id(l, s.types.push(make()))))
    }

    // ---------------------------------------------------------------------------------
    // Strings and lists
    // ---------------------------------------------------------------------------------

    /// Interns a string. Strings always live in the root store, so every program and project
    /// built on one root shares them.
    pub fn intern_str(&self, s: &str) -> StrId {
        let r = self.root();
        let shard = r.string_map.shard(s);
        if let Some(&id) = shard.read().unwrap_or_else(|e| e.into_inner()).get(s) {
            return id;
        }
        r.string_map
            .get_or_insert_with(s.into(), || StrId(r.strings.push(s.into())))
    }

    /// The id of an already interned string.
    pub fn find_str(&self, s: &str) -> Option<StrId> {
        self.root()
            .string_map
            .shard(s)
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(s)
            .copied()
    }

    #[inline]
    pub fn str(&self, id: StrId) -> &str {
        self.root().strings.get(id.0)
    }

    pub fn list(&self, items: &[TypeId]) -> ListId {
        if items.is_empty() {
            return ListId::EMPTY;
        }
        let l = Self::lts(items);
        let s = self.at(l);
        let shard = s.list_map.shard(items);
        if let Some(&id) = shard.read().unwrap_or_else(|e| e.into_inner()).get(items) {
            return id;
        }
        s.list_map.get_or_insert_with(items.into(), || {
            ListId(make_id(l, s.lists.push(items.into())))
        })
    }

    #[inline]
    pub fn list_items(&self, id: ListId) -> &[TypeId] {
        self.at(level_of(id.0)).lists.get(index_of(id.0))
    }

    // ---------------------------------------------------------------------------------
    // Literals and unit types
    // ---------------------------------------------------------------------------------

    /// The error type of an unresolved type reference, printed as written (`Problem`,
    /// `z.infer<any>`). It behaves exactly like [`TypeId::ERROR`] (`any` flags, the
    /// `CONTAINS_ERROR` and `NAMED_ERROR` traits); test with [`TypeStore::is_error`].
    pub fn named_error(&self, name: &str) -> TypeId {
        let n = self.intern_str(name);
        let t = self.intern(Key::NamedError(n), || TypeData {
            flags: TypeFlags::ANY,
            traits: TypeTraits::CONTAINS_ERROR | TypeTraits::NAMED_ERROR,
            kind: TypeKind::Intrinsic,
        });
        self.root().error_names.insert(t, n);
        t
    }

    /// The error type or a named error type.
    #[inline]
    pub fn is_error(&self, t: TypeId) -> bool {
        if t == TypeId::ERROR {
            return true;
        }
        let d = self.get(t);
        d.traits.contains(TypeTraits::NAMED_ERROR) && matches!(d.kind, TypeKind::Intrinsic)
    }

    /// The printed name of a named error type.
    pub fn error_name(&self, t: TypeId) -> Option<&str> {
        if t == TypeId::ERROR || !self.is_error(t) {
            return None;
        }
        self.root().error_names.get(&t).map(|n| self.str(n))
    }

    pub fn string_literal(&self, value: &str, fresh: bool) -> TypeId {
        let s = self.intern_str(value);
        self.string_literal_id(s, fresh)
    }

    pub fn string_literal_id(&self, s: StrId, fresh: bool) -> TypeId {
        self.intern(Key::Str(s, fresh), || TypeData {
            flags: TypeFlags::STRING_LITERAL,
            traits: if fresh {
                TypeTraits::FRESH
            } else {
                TypeTraits::NONE
            },
            kind: TypeKind::StringLiteral(s),
        })
    }

    pub fn number_literal(&self, value: f64, fresh: bool) -> TypeId {
        // -0 and 0 are the same literal type; all NaNs are one.
        let v = if value == 0.0 {
            0.0
        } else if value.is_nan() {
            f64::NAN
        } else {
            value
        };
        let bits = v.to_bits();
        self.intern(Key::Num(bits, fresh), || TypeData {
            flags: TypeFlags::NUMBER_LITERAL,
            traits: if fresh {
                TypeTraits::FRESH
            } else {
                TypeTraits::NONE
            },
            kind: TypeKind::NumberLiteral(bits as u32, (bits >> 32) as u32),
        })
    }

    /// `text` is decimal digits with an optional leading `-`.
    pub fn bigint_literal(&self, text: &str, fresh: bool) -> TypeId {
        let s = self.intern_str(text);
        self.intern(Key::BigInt(s, fresh), || TypeData {
            flags: TypeFlags::BIGINT_LITERAL,
            traits: if fresh {
                TypeTraits::FRESH
            } else {
                TypeTraits::NONE
            },
            kind: TypeKind::BigIntLiteral(s),
        })
    }

    pub fn boolean_literal(&self, value: bool, fresh: bool) -> TypeId {
        match (value, fresh) {
            (false, false) => TypeId::FALSE,
            (true, false) => TypeId::TRUE,
            (false, true) => TypeId::FRESH_FALSE,
            (true, true) => TypeId::FRESH_TRUE,
        }
    }

    /// Enum member type: `value` is the (regular) number or string literal type of the member.
    pub fn enum_literal(&self, member: SymId, value: TypeId, fresh: bool) -> TypeId {
        let base = self.flags(value) & (TypeFlags::NUMBER_LITERAL | TypeFlags::STRING_LITERAL);
        self.intern(Key::EnumLit(member, value, fresh), || TypeData {
            flags: TypeFlags::ENUM_LITERAL | base,
            traits: if fresh {
                TypeTraits::FRESH
            } else {
                TypeTraits::NONE
            },
            kind: TypeKind::EnumLiteral { member, value },
        })
    }

    pub fn enum_type(&self, sym: SymId) -> TypeId {
        self.intern(Key::Enum(sym), || TypeData {
            flags: TypeFlags::ENUM,
            traits: TypeTraits::NONE,
            kind: TypeKind::Enum(sym),
        })
    }

    pub fn unique_symbol(&self, sym: SymId) -> TypeId {
        self.intern(Key::Unique(sym), || TypeData {
            flags: TypeFlags::UNIQUE_ES_SYMBOL,
            traits: TypeTraits::NONE,
            kind: TypeKind::UniqueSymbol(sym),
        })
    }

    /// The non-fresh twin of a fresh literal or fresh object literal type.
    pub fn regular(&self, t: TypeId) -> TypeId {
        let d = self.get(t);
        if !d.traits.contains(TypeTraits::FRESH) {
            if let TypeKind::Object(o) = d.kind
                && let Some(&r) = self.object(o).regular.get()
                && self.object(o).flags.contains(ObjectFlags::FRESH_LITERAL)
            {
                return r;
            }
            return t;
        }
        match d.kind {
            TypeKind::StringLiteral(s) => self.string_literal_id(s, false),
            TypeKind::NumberLiteral(lo, hi) => {
                self.number_literal(f64::from_bits((hi as u64) << 32 | lo as u64), false)
            }
            TypeKind::BigIntLiteral(s) => {
                let text = self.str(s).to_owned();
                self.bigint_literal(&text, false)
            }
            TypeKind::EnumLiteral { member, value } => self.enum_literal(member, value, false),
            TypeKind::Intrinsic => match t {
                TypeId::FRESH_FALSE => TypeId::FALSE,
                TypeId::FRESH_TRUE => TypeId::TRUE,
                _ => t,
            },
            _ => t,
        }
    }

    /// The fresh twin of a literal type.
    pub fn fresh(&self, t: TypeId) -> TypeId {
        match self.kind(t) {
            TypeKind::StringLiteral(s) => self.string_literal_id(s, true),
            TypeKind::NumberLiteral(lo, hi) => {
                self.number_literal(f64::from_bits((hi as u64) << 32 | lo as u64), true)
            }
            TypeKind::BigIntLiteral(s) => {
                let text = self.str(s).to_owned();
                self.bigint_literal(&text, true)
            }
            TypeKind::EnumLiteral { member, value } => self.enum_literal(member, value, true),
            TypeKind::Intrinsic => match t {
                TypeId::FALSE => TypeId::FRESH_FALSE,
                TypeId::TRUE => TypeId::FRESH_TRUE,
                _ => t,
            },
            _ => t,
        }
    }

    pub fn number_value(&self, t: TypeId) -> Option<f64> {
        match self.kind(t) {
            TypeKind::NumberLiteral(lo, hi) => Some(f64::from_bits((hi as u64) << 32 | lo as u64)),
            TypeKind::EnumLiteral { value, .. } => self.number_value(value),
            _ => None,
        }
    }

    pub fn string_value(&self, t: TypeId) -> Option<&str> {
        match self.kind(t) {
            TypeKind::StringLiteral(s) => Some(self.str(s)),
            TypeKind::EnumLiteral { value, .. } => self.string_value(value),
            _ => None,
        }
    }

    // ---------------------------------------------------------------------------------
    // Unions and intersections
    // ---------------------------------------------------------------------------------

    fn combined_traits(&self, items: &[TypeId]) -> TypeTraits {
        let mut tr = TypeTraits::NONE;
        for &t in items {
            tr |= self.traits(t);
        }
        tr.without(
            TypeTraits::FRESH
                | TypeTraits::ENUM_UNION
                | TypeTraits::THIS_TYPE
                | TypeTraits::SUBTYPE_REDUCED
                | TypeTraits::NAMED_ERROR,
        )
    }

    /// Interns a union whose members are already flattened, sorted in the canonical order and
    /// de-duplicated (at least two members; callers handle 0 and 1).
    pub fn union_sorted(&self, members: &[TypeId], alias: Option<AliasId>) -> TypeId {
        let alias = alias.unwrap_or(AliasId::NONE);
        let list = self.list(members);
        self.intern(Key::Union(list, alias), || {
            let mut flags = TypeFlags::UNION;
            if members
                .iter()
                .all(|&m| self.flags(m).intersects(TypeFlags::BOOLEAN_LITERAL))
                && members.len() == 2
            {
                flags |= TypeFlags::BOOLEAN;
            }
            TypeData {
                flags,
                traits: self.combined_traits(members),
                kind: TypeKind::Union { list, alias },
            }
        })
    }

    /// A union of `members` (sorted, de-duplicated, at least two) that remembers how it was
    /// written: `origin` lists aliased unions as single items next to the other members
    /// (`HeadersInit | undefined`). It is a distinct type from the plain union of the same
    /// members, printed through its origin ([`Self::union_origin`]). The origin is stored as
    /// the union's alias with no symbol, so [`Self::alias_of`] never reports it.
    pub fn union_with_origin(&self, members: &[TypeId], origin: &[TypeId]) -> TypeId {
        let oa = self.alias(SymId::NONE, self.list(origin));
        self.union_sorted(members, Some(oa))
    }

    /// The origin items of a union created by [`Self::union_with_origin`].
    pub fn union_origin(&self, t: TypeId) -> Option<&[TypeId]> {
        match self.kind(t) {
            TypeKind::Union { alias, .. } => {
                let info = self.alias_info(alias)?;
                (info.sym == SymId::NONE).then(|| self.list_items(info.args))
            }
            _ => None,
        }
    }

    /// Like [`Self::union_sorted`], marking the result as an enum's literal union.
    pub fn enum_union(&self, members: &[TypeId], alias: AliasId) -> TypeId {
        let list = self.list(members);
        self.intern(Key::Union(list, alias), || TypeData {
            flags: TypeFlags::UNION,
            traits: self.combined_traits(members) | TypeTraits::ENUM_UNION,
            kind: TypeKind::Union { list, alias },
        })
    }

    pub fn intersection_sorted(&self, members: &[TypeId], alias: Option<AliasId>) -> TypeId {
        let alias = alias.unwrap_or(AliasId::NONE);
        let list = self.list(members);
        self.intern(Key::Inter(list, alias), || TypeData {
            flags: TypeFlags::INTERSECTION,
            traits: self.combined_traits(members),
            kind: TypeKind::Intersection { list, alias },
        })
    }

    /// Members of a union or intersection (empty for other types).
    pub fn members_of_union(&self, t: TypeId) -> &[TypeId] {
        match self.kind(t) {
            TypeKind::Union { list, .. } | TypeKind::Intersection { list, .. } => {
                self.list_items(list)
            }
            _ => &[],
        }
    }

    // ---------------------------------------------------------------------------------
    // Aliases and mappers
    // ---------------------------------------------------------------------------------

    pub fn alias(&self, sym: SymId, args: ListId) -> AliasId {
        let info = AliasInfo { sym, args };
        let l = level_of(sym.0).max(level_of(args.0));
        let s = self.at(l);
        s.alias_map
            .get_or_insert_with(info, || AliasId(make_id(l, s.aliases.push(info))))
    }

    pub fn alias_info(&self, id: AliasId) -> Option<AliasInfo> {
        (id != AliasId::NONE).then(|| *self.at(level_of(id.0)).aliases.get(index_of(id.0)))
    }

    /// The alias of a type, if it has one.
    pub fn alias_of(&self, t: TypeId) -> Option<AliasInfo> {
        match self.kind(t) {
            TypeKind::Union { alias, .. } | TypeKind::Intersection { alias, .. } => {
                self.alias_info(alias).filter(|i| i.sym != SymId::NONE)
            }
            TypeKind::Object(o) => self.alias_info(self.object(o).alias),
            TypeKind::Conditional(i) => {
                self.alias_info(self.at(level_of(i)).conds.get(index_of(i)).alias)
            }
            _ => None,
        }
    }

    pub fn mapper(&self, sources: &[TypeId], targets: &[TypeId]) -> MapperId {
        if sources.is_empty() {
            return MapperId::IDENTITY;
        }
        let m = Mapper {
            sources: self.list(sources),
            targets: self.list(targets),
        };
        let l = level_of(m.sources.0).max(level_of(m.targets.0));
        let s = self.at(l);
        s.mapper_map
            .get_or_insert_with(m, || MapperId(make_id(l, s.mappers.push(m))))
    }

    /// `(sources, targets)` of a mapper (empty for the special mappers).
    pub fn mapper_lists(&self, m: MapperId) -> (&[TypeId], &[TypeId]) {
        if m.is_special() {
            return (&[], &[]);
        }
        let d = self.at(level_of(m.0)).mappers.get(index_of(m.0));
        (self.list_items(d.sources), self.list_items(d.targets))
    }

    /// Looks a type up in a mapper.
    pub fn map_type(&self, m: MapperId, t: TypeId) -> Option<TypeId> {
        if m == MapperId::IDENTITY {
            return None;
        }
        if m.is_special() {
            if !self.flags(t).contains(TypeFlags::TYPE_PARAMETER) {
                return None;
            }
            return Some(if m == MapperId::PERMISSIVE {
                TypeId::WILDCARD
            } else {
                self.restrictive_type_param(t)
            });
        }
        let (s, d) = self.mapper_lists(m);
        s.iter()
            .position(|&x| x == t)
            .and_then(|i| d.get(i).copied())
    }

    // ---------------------------------------------------------------------------------
    // Objects
    // ---------------------------------------------------------------------------------

    #[inline]
    pub fn object(&self, o: ObjId) -> &ObjectData {
        self.at(level_of(o.0)).objects.get(index_of(o.0))
    }

    /// The object data of an object type.
    pub fn object_of(&self, t: TypeId) -> Option<&ObjectData> {
        match self.kind(t) {
            TypeKind::Object(o) => Some(self.object(o)),
            _ => None,
        }
    }

    /// Pushes an object type into level `l` (a type and its object always share a level).
    fn push_object(
        &self,
        l: u8,
        kind: ObjectKind,
        flags: ObjectFlags,
        alias: AliasId,
        traits: TypeTraits,
    ) -> TypeId {
        let s = self.at(l);
        let o = ObjId(make_id(
            l,
            s.objects.push(ObjectData {
                kind,
                flags,
                alias,
                members: OnceLock::new(),
                regular: OnceLock::new(),
                level: l,
            }),
        ));
        TypeId(make_id(
            l,
            s.types.push(TypeData {
                flags: TypeFlags::OBJECT,
                traits,
                kind: TypeKind::Object(o),
            }),
        ))
    }

    /// The declared type of a class or interface (one per symbol).
    pub fn declared_object(
        &self,
        sym: SymId,
        flags: ObjectFlags,
        make: impl FnOnce() -> (ListId, TypeId),
    ) -> TypeId {
        let l = level_of(sym.0);
        let s = self.at(l);
        if let Some(t) = s.intern.get(&Key::Declared(sym)) {
            return t;
        }
        let (type_params, this_type) = make();
        let traits = if type_params == ListId::EMPTY {
            TypeTraits::NONE
        } else {
            TypeTraits::CONTAINS_TYPE_PARAMETER
        };
        s.intern.get_or_insert_with(Key::Declared(sym), || {
            self.push_object(
                l,
                ObjectKind::Declared {
                    sym,
                    type_params,
                    this_type,
                },
                flags,
                AliasId::NONE,
                traits,
            )
        })
    }

    /// The declared class/interface type of `sym`, if already created.
    pub fn find_declared(&self, sym: SymId) -> Option<TypeId> {
        self.at(level_of(sym.0)).intern.get(&Key::Declared(sym))
    }

    /// `target<args>`. `args` includes the optional trailing `this` argument.
    pub fn reference(&self, target: TypeId, args: &[TypeId]) -> TypeId {
        self.reference_aliased(target, args, AliasId::NONE)
    }

    /// `target<args>` named by a type alias (`type M = Map<string, number>` prints `M`): a
    /// type distinct from the plain reference, with the same target and arguments.
    pub fn reference_aliased(&self, target: TypeId, args: &[TypeId], alias: AliasId) -> TypeId {
        let list = self.list(args);
        let tflags = self
            .object_of(target)
            .map_or(ObjectFlags::NONE, |o| o.flags);
        let key = Key::Reference(target, list, alias);
        let l = self.key_level(&key);
        let s = self.at(l);
        s.intern.get_or_insert_with(key, || {
            let traits = self
                .combined_traits(args)
                .without(TypeTraits::CONTAINS_OBJECT_LITERAL);
            self.push_object(
                l,
                ObjectKind::Reference { target, args: list },
                (tflags
                    & (ObjectFlags::CLASS
                        | ObjectFlags::INTERFACE
                        | ObjectFlags::TUPLE
                        | ObjectFlags::READONLY))
                    | ObjectFlags::REFERENCE,
                alias,
                traits,
            )
        })
    }

    /// The tuple target for a shape (interned in the root store).
    pub fn tuple_target(&self, shape: TupleShape) -> TypeId {
        let readonly = shape.readonly;
        let r = self.root();
        let idx = r
            .tuple_map
            .get_or_insert_with(shape.clone(), || r.tuples.push(shape));
        r.intern.get_or_insert_with(Key::Tuple(idx), || {
            let mut flags = ObjectFlags::TUPLE;
            if readonly {
                flags |= ObjectFlags::READONLY;
            }
            self.push_object(
                0,
                ObjectKind::TupleTarget { shape: idx },
                flags,
                AliasId::NONE,
                TypeTraits::NONE,
            )
        })
    }

    pub fn tuple_shape(&self, idx: u32) -> &TupleShape {
        self.root().tuples.get(idx)
    }

    /// The shape of a tuple type (a reference to a tuple target) or tuple target.
    pub fn tuple_shape_of(&self, t: TypeId) -> Option<&TupleShape> {
        match self.object_of(t)?.kind {
            ObjectKind::TupleTarget { shape } => Some(self.tuple_shape(shape)),
            ObjectKind::Reference { target, .. } => match self.object_of(target)?.kind {
                ObjectKind::TupleTarget { shape } => Some(self.tuple_shape(shape)),
                _ => None,
            },
            _ => None,
        }
    }

    /// An anonymous type of a declaration under a mapper (interned).
    pub fn anonymous(
        &self,
        decl: (u32, u32),
        kind: u8,
        mapper: MapperId,
        flags: ObjectFlags,
        alias: AliasId,
        contains_tp: bool,
    ) -> TypeId {
        let key = Key::Anonymous(decl, kind, mapper, alias);
        let l = self.key_level(&key);
        self.at(l).intern.get_or_insert_with(key, || {
            let traits = if contains_tp {
                TypeTraits::CONTAINS_TYPE_PARAMETER
            } else {
                TypeTraits::NONE
            };
            let flags = if mapper == MapperId::IDENTITY {
                flags
            } else {
                flags | ObjectFlags::INSTANTIATED
            };
            self.push_object(
                l,
                ObjectKind::Anonymous { decl, kind, mapper },
                flags,
                alias,
                traits,
            )
        })
    }

    /// A new (not interned) object type whose members are given now. It lives in the level
    /// of the highest of its symbol and members.
    pub fn literal_object(
        &self,
        sym: SymId,
        flags: ObjectFlags,
        members: ResolvedMembers,
        traits: TypeTraits,
    ) -> TypeId {
        let l = level_of(sym.0).max(self.members_level(&members));
        let t = self.push_object(l, ObjectKind::Literal { sym }, flags, AliasId::NONE, traits);
        if let TypeKind::Object(o) = self.kind(t) {
            let _ = self.object(o).members.set(Box::new(members));
        }
        t
    }

    /// The mapped type of a `MappedType` node under a mapper (interned, in the level of the
    /// node's file and the mapper).
    pub fn mapped_object(
        &self,
        decl: (u32, u32),
        mapper: MapperId,
        alias: AliasId,
        contains_tp: bool,
    ) -> TypeId {
        let key = Key::Mapped(decl, mapper, alias);
        let l = self.key_level(&key);
        self.at(l).intern.get_or_insert_with(key, || {
            let traits = if contains_tp {
                TypeTraits::CONTAINS_TYPE_PARAMETER
            } else {
                TypeTraits::NONE
            };
            let flags = if mapper == MapperId::IDENTITY {
                ObjectFlags::MAPPED
            } else {
                ObjectFlags::MAPPED | ObjectFlags::INSTANTIATED
            };
            self.push_object(l, ObjectKind::Mapped { decl, mapper }, flags, alias, traits)
        })
    }

    // ---------------------------------------------------------------------------------
    // Conditional and template literal types
    // ---------------------------------------------------------------------------------

    /// The root of the conditional type node `decl`, created by `make` the first time (the
    /// first writer wins; `make` runs outside any lock and may create other types). A root
    /// lives in the level of its node's file, or higher when its parts do.
    pub fn conditional_root(
        &self,
        decl: (u32, u32),
        make: impl FnOnce() -> ConditionalRoot,
    ) -> u32 {
        let fl = self.decl_level(decl);
        for l in fl..=self.level {
            if let Some(i) = self.at(l).cond_root_map.get(&decl) {
                return i;
            }
        }
        let root = make();
        let l = fl
            .max(Self::lt(root.check))
            .max(Self::lt(root.extends))
            .max(level_of(root.infer_params.0))
            .max(level_of(root.outer_params.0));
        let s = self.at(l);
        s.cond_root_map
            .get_or_insert_with(decl, || make_id(l, s.cond_roots.push(root)))
    }

    pub fn conditional_root_data(&self, root: u32) -> &ConditionalRoot {
        self.at(level_of(root)).cond_roots.get(index_of(root))
    }

    /// A deferred conditional type (interned by root, mapper and alias).
    pub fn conditional(&self, root: u32, mapper: MapperId, alias: AliasId) -> TypeId {
        let data = ConditionalData {
            root,
            mapper,
            alias,
        };
        let key = Key::Conditional(data);
        let l = self.key_level(&key);
        let s = self.at(l);
        self.intern(key, || {
            let idx = make_id(l, s.conds.push(data));
            TypeData {
                flags: TypeFlags::CONDITIONAL,
                traits: TypeTraits::CONTAINS_TYPE_PARAMETER,
                kind: TypeKind::Conditional(idx),
            }
        })
    }

    /// The data of a conditional type.
    pub fn conditional_data(&self, t: TypeId) -> Option<ConditionalData> {
        match self.kind(t) {
            TypeKind::Conditional(i) => Some(*self.at(level_of(i)).conds.get(index_of(i))),
            _ => None,
        }
    }

    /// A template literal type from normalized parts: `texts` (string literal types) has one
    /// more element than `types`, and no type is a literal or union (the checker normalizes).
    pub fn template_literal(&self, texts: &[TypeId], types: &[TypeId]) -> TypeId {
        let data = TemplateData {
            texts: self.list(texts),
            types: self.list(types),
        };
        let key = Key::Template(data);
        let l = self.key_level(&key);
        let s = self.at(l);
        self.intern(key, || {
            let idx = make_id(l, s.templates.push(data));
            TypeData {
                flags: TypeFlags::TEMPLATE_LITERAL,
                traits: self.combined_traits(types),
                kind: TypeKind::TemplateLiteral(idx),
            }
        })
    }

    /// `(texts, types)` of a template literal type.
    pub fn template_parts(&self, t: TypeId) -> Option<(&[TypeId], &[TypeId])> {
        match self.kind(t) {
            TypeKind::TemplateLiteral(i) => {
                let d = self.at(level_of(i)).templates.get(index_of(i));
                Some((self.list_items(d.texts), self.list_items(d.types)))
            }
            _ => None,
        }
    }

    /// The unconstrained copy of a type parameter used by restrictive instantiation.
    pub fn restrictive_type_param(&self, t: TypeId) -> TypeId {
        let Some(d) = self.type_param_data(t) else {
            return t;
        };
        if d.restrictive {
            return t;
        }
        let (sym, is_this) = (d.sym, d.is_this);
        let key = Key::Restrictive(t);
        let l = self.key_level(&key);
        let s = self.at(l);
        self.intern(key, || {
            let p = TypeParamId(make_id(
                l,
                s.params.push(TypeParamData {
                    sym,
                    is_this,
                    origin: None,
                    restrictive: true,
                    constraint: OnceLock::from(None),
                    default: OnceLock::from(None),
                    level: l,
                }),
            ));
            TypeData {
                flags: TypeFlags::TYPE_PARAMETER,
                traits: TypeTraits::CONTAINS_TYPE_PARAMETER,
                kind: TypeKind::TypeParameter(p),
            }
        })
    }

    /// Records the regular twin of a fresh object literal type.
    pub fn set_regular(&self, fresh: TypeId, regular: TypeId) {
        if let TypeKind::Object(o) = self.kind(fresh) {
            let d = self.object(o);
            if Self::lt(regular) <= d.level {
                let _ = d.regular.set(regular);
            }
        }
    }

    /// Resolved members of an object type, if published.
    pub fn members(&self, t: TypeId) -> Option<&ResolvedMembers> {
        let d = self.object_of(t)?;
        if let Some(m) = d.members() {
            return Some(m);
        }
        let TypeKind::Object(o) = self.kind(t) else {
            return None;
        };
        for l in (d.level + 1)..=self.level {
            let s = self.at(l);
            if let Some(i) = s.shadow_members.get(&o.0) {
                return Some(s.shadow_vals.get(i));
            }
        }
        None
    }

    /// Publishes resolved members (first writer wins) and returns the published value.
    /// Members that refer to a higher level than the object's are kept in that level's
    /// shadow table, so the object's own (shared) slot never sees them.
    pub fn set_members(&self, t: TypeId, m: ResolvedMembers) -> &ResolvedMembers {
        match self.kind(t) {
            TypeKind::Object(o) => {
                let d = self.object(o);
                let ml = self.members_level(&m);
                if ml <= d.level {
                    let _ = d.members.set(Box::new(m));
                    return d.members().unwrap_or_else(|| unreachable!());
                }
                if let Some(existing) = self.members(t) {
                    return existing;
                }
                let s = self.at(ml);
                let i = s
                    .shadow_members
                    .get_or_insert_with(o.0, || s.shadow_vals.push(m));
                s.shadow_vals.get(i)
            }
            _ => panic!("set_members on a non-object type"),
        }
    }

    // ---------------------------------------------------------------------------------
    // Type parameters and other generic types
    // ---------------------------------------------------------------------------------

    /// The declared type parameter of `sym` (or the `this` type of a class/interface `sym`).
    pub fn type_param(&self, sym: SymId, is_this: bool) -> TypeId {
        let key = Key::TypeParam(sym, is_this);
        let l = self.key_level(&key);
        let s = self.at(l);
        self.intern(key, || {
            let p = TypeParamId(make_id(
                l,
                s.params.push(TypeParamData {
                    sym,
                    is_this,
                    origin: None,
                    restrictive: false,
                    constraint: OnceLock::new(),
                    default: OnceLock::new(),
                    level: l,
                }),
            ));
            TypeData {
                flags: TypeFlags::TYPE_PARAMETER,
                traits: if is_this {
                    TypeTraits::CONTAINS_TYPE_PARAMETER | TypeTraits::THIS_TYPE
                } else {
                    TypeTraits::CONTAINS_TYPE_PARAMETER
                },
                kind: TypeKind::TypeParameter(p),
            }
        })
    }

    /// A fresh copy of type parameter `orig` whose constraint is `orig`'s under `mapper`.
    pub fn cloned_type_param(&self, orig: TypeId, mapper: MapperId) -> TypeId {
        let sym = match self.kind(orig) {
            TypeKind::TypeParameter(p) => self.param(p).sym,
            _ => SymId::NONE,
        };
        let key = Key::ClonedParam(orig, mapper);
        let l = self.key_level(&key);
        let s = self.at(l);
        self.intern(key, || {
            let p = TypeParamId(make_id(
                l,
                s.params.push(TypeParamData {
                    sym,
                    is_this: false,
                    origin: Some((orig, mapper)),
                    restrictive: false,
                    constraint: OnceLock::new(),
                    default: OnceLock::new(),
                    level: l,
                }),
            ));
            TypeData {
                flags: TypeFlags::TYPE_PARAMETER,
                traits: TypeTraits::CONTAINS_TYPE_PARAMETER,
                kind: TypeKind::TypeParameter(p),
            }
        })
    }

    #[inline]
    fn param(&self, p: TypeParamId) -> &TypeParamData {
        self.at(level_of(p.0)).params.get(index_of(p.0))
    }

    pub fn type_param_data(&self, t: TypeId) -> Option<&TypeParamData> {
        match self.kind(t) {
            TypeKind::TypeParameter(p) => Some(self.param(p)),
            _ => None,
        }
    }

    pub fn index_type(&self, target: TypeId) -> TypeId {
        self.intern(Key::Index(target), || TypeData {
            flags: TypeFlags::INDEX,
            traits: self
                .traits(target)
                .without(TypeTraits::FRESH | TypeTraits::THIS_TYPE),
            kind: TypeKind::Index(target),
        })
    }

    pub fn indexed_access(&self, obj: TypeId, index: TypeId) -> TypeId {
        self.intern(Key::IndexedAccess(obj, index), || TypeData {
            flags: TypeFlags::INDEXED_ACCESS,
            traits: self.combined_traits(&[obj, index]),
            kind: TypeKind::IndexedAccess { obj, index },
        })
    }

    pub fn substitution(&self, base: TypeId, constraint: TypeId) -> TypeId {
        self.intern(Key::Substitution(base, constraint), || TypeData {
            flags: TypeFlags::SUBSTITUTION,
            traits: self.combined_traits(&[base, constraint]),
            kind: TypeKind::Substitution { base, constraint },
        })
    }

    pub fn string_mapping(&self, sym: SymId, ty: TypeId) -> TypeId {
        self.intern(Key::StringMapping(sym, ty), || TypeData {
            flags: TypeFlags::STRING_MAPPING,
            traits: self.traits(ty).without(TypeTraits::FRESH),
            kind: TypeKind::StringMapping { sym, ty },
        })
    }

    // ---------------------------------------------------------------------------------
    // Signatures
    // ---------------------------------------------------------------------------------

    /// Adds a signature in the level of the highest of its parts.
    pub fn add_signature(&self, mut sig: Signature) -> SigId {
        let l = self.signature_level(&sig);
        sig.level = l;
        SigId(make_id(l, self.at(l).sigs.push(sig)))
    }

    #[inline]
    pub fn sig(&self, s: SigId) -> &Signature {
        self.at(level_of(s.0)).sigs.get(index_of(s.0))
    }

    // ---------------------------------------------------------------------------------
    // Relation caches
    // ---------------------------------------------------------------------------------

    pub fn relation_cache(&self, kind: RelationKind) -> RelationCacheRef<'_> {
        RelationCacheRef { store: self, kind }
    }

    /// Sizes for diagnostics (`--extendedDiagnostics`-style output), this level only.
    pub fn stats(&self) -> (usize, usize, usize, usize) {
        (
            self.types.len(),
            self.objects.len(),
            self.sigs.len(),
            self.lists.len(),
        )
    }

    /// Approximate heap bytes of this level's tables (slots, not the data they point to).
    pub fn heap_bytes(&self) -> usize {
        self.types.heap_bytes()
            + self.strings.heap_bytes()
            + self.lists.heap_bytes()
            + self.objects.heap_bytes()
            + self.sigs.heap_bytes()
            + self.params.heap_bytes()
            + self.aliases.heap_bytes()
            + self.mappers.heap_bytes()
            + self.tuples.heap_bytes()
            + self.shadow_vals.heap_bytes()
    }
}
