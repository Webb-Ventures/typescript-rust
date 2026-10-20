//! The type store shared by every checker worker (design: `docs/design/0004-types-and-checker.md`).
//!
//! Types are `u32` ids ([`TypeId`]) into one append-only, thread-safe store. Structural types
//! (literals, unions, intersections, references, tuples, indexed access, `keyof`, unique
//! symbols) are hash-consed: equal content gives an equal id. Types created from declarations
//! (classes, interfaces, type literals, function types, object literals) are keyed by their
//! declaring symbol and mapper, so two equal type literals in different places are different
//! types, as in TypeScript.
//!
//! The store knows nothing about the AST or the binder: symbols are opaque [`SymId`]s owned by
//! the checker, and anything that needs declarations (member resolution, printing, relations)
//! lives in `ts_r_checker`, which publishes its lazily computed results into the store's
//! write-once slots.
//!
//! ```
//! use ts_r_types::{TypeStore, TypeId};
//! let store = TypeStore::new();
//! let a = store.string_literal("a", false);
//! assert_eq!(a, store.string_literal("a", false));
//! let u = store.union_sorted(&[TypeId::NULL, TypeId::STRING], None);
//! assert_eq!(store.members_of_union(u), &[TypeId::NULL, TypeId::STRING]);
//! ```

mod chunk;
mod flags;
mod store;

pub use chunk::ChunkVec;
pub use flags::{ObjectFlags, SigFlags, TypeFlags, TypeTraits};
pub use store::{
    AliasId, AliasInfo, ConditionalData, ConditionalRoot, IndexInfo, LEVEL_SHIFT, ListId,
    MAX_LEVELS, MapperId, ObjId, ObjectData, ObjectKind, Param, PredicateKind, RelationCache,
    RelationCacheRef, RelationKind, ResolvedMembers, SigId, Signature, StrId, TemplateData,
    TupleElement, TupleShape, TypeData, TypeKind, TypeParamData, TypeParamId, TypePredicate,
    TypeStore, index_of, level_of, make_id, mapper_level,
};

/// A type: index into the [`TypeStore`]. Ids are never reused and their numeric order means
/// nothing observable (printing and union order are content-based).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct TypeId(pub u32);

/// A symbol as the checker numbers them (binder symbols first, synthetic ones above). The
/// store treats it as an opaque key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct SymId(pub u32);

impl SymId {
    pub const NONE: SymId = SymId(u32::MAX);

    #[inline]
    pub fn is_none(self) -> bool {
        self.0 == u32::MAX
    }
}

macro_rules! intrinsics {
    ($($(#[$m:meta])* $name:ident = $n:literal;)*) => {
        impl TypeId {
            $( $(#[$m])* pub const $name: TypeId = TypeId($n); )*
            /// Number of fixed-id types allocated by [`TypeStore::new`].
            pub const INTRINSIC_COUNT: u32 = [$($n),*].len() as u32;
        }
    };
}

intrinsics! {
    ERROR = 0;
    ANY = 1;
    UNKNOWN = 2;
    NEVER = 3;
    VOID = 4;
    UNDEFINED = 5;
    NULL = 6;
    STRING = 7;
    NUMBER = 8;
    BIGINT = 9;
    FALSE = 10;
    TRUE = 11;
    BOOLEAN = 12;
    FRESH_FALSE = 13;
    FRESH_TRUE = 14;
    SYMBOL = 15;
    /// The non-primitive `object` type.
    OBJECT = 16;
    /// `{}` with no members.
    EMPTY_OBJECT = 17;
    /// `never` that does not produce errors (inference placeholder).
    SILENT_NEVER = 18;
    /// Wildcard used while relating generic types.
    WILDCARD = 19;
    /// `undefined` from a missing optional property (`exactOptionalPropertyTypes`).
    MISSING = 20;
    /// The `never` of an implicit-never context (empty array literal element type).
    IMPLICIT_NEVER = 21;
    /// `undefined` that widens to `any` (non-strict mode `let x = undefined`).
    UNDEFINED_WIDENING = 22;
    /// `null` that widens to `any` (non-strict mode).
    NULL_WIDENING = 23;
    /// The `any` of an evolving declaration (`let x;` with noImplicitAny control flow).
    AUTO = 24;
    /// An unresolved or not yet supported type construct. Behaves like `any` without
    /// producing errors; prints as `any`.
    UNRESOLVED = 25;
    /// `{}` used as the type of an empty object literal before widening; printed `{}`.
    EMPTY_FRESH_OBJECT = 26;
    /// `any` produced by `noImplicitAny` violations (printed `any`).
    IMPLICIT_ANY = 27;
}

impl TypeId {
    #[inline]
    pub fn index(self) -> usize {
        self.0 as usize
    }

    /// `any`-like intrinsic (`any`, the error type, `UNRESOLVED`, `AUTO`, implicit any).
    #[inline]
    pub fn is_any_like(self) -> bool {
        matches!(
            self,
            TypeId::ANY | TypeId::ERROR | TypeId::UNRESOLVED | TypeId::AUTO | TypeId::IMPLICIT_ANY
        )
    }
}

#[cfg(test)]
mod tests;

/// Counters of values a lower store level could not keep because they refer to a higher
/// level (`[signature return, predicate, constraint, default, checker slots, other]`); they are
/// recomputed on each use. Diagnostics for tuning the level split.
pub static PUBLISH_MISSES: [std::sync::atomic::AtomicU64; 6] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 6];

/// Counts a value a level could not keep (see [`PUBLISH_MISSES`]).
#[inline]
pub fn note_miss(kind: usize) {
    if let Some(c) = PUBLISH_MISSES.get(kind) {
        c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}
