//! Flag sets: what kind a type is ([`TypeFlags`]), derived summary facts ([`TypeTraits`]),
//! object type facts ([`ObjectFlags`]) and signature facts ([`SigFlags`]).

macro_rules! bitset {
    ($(#[$m:meta])* pub struct $name:ident: $ty:ty { $( $(#[$fm:meta])* const $flag:ident = $val:expr; )* }) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
        pub struct $name(pub $ty);

        impl $name {
            $( $(#[$fm])* pub const $flag: $name = $name($val); )*
            pub const NONE: $name = $name(0);
            const NAMES: &'static [(&'static str, $ty)] = &[$( (stringify!($flag), $val) ),*];

            #[inline] pub const fn contains(self, other: $name) -> bool { self.0 & other.0 == other.0 }
            #[inline] pub const fn intersects(self, other: $name) -> bool { self.0 & other.0 != 0 }
            #[inline] pub const fn is_empty(self) -> bool { self.0 == 0 }
            #[inline] pub fn insert(&mut self, other: $name) { self.0 |= other.0 }
            #[inline] pub fn remove(&mut self, other: $name) { self.0 &= !other.0 }
            #[inline] pub const fn union(self, other: $name) -> $name { $name(self.0 | other.0) }
            #[inline] pub const fn without(self, other: $name) -> $name { $name(self.0 & !other.0) }
        }

        impl std::ops::BitOr for $name {
            type Output = $name;
            #[inline] fn bitor(self, rhs: $name) -> $name { $name(self.0 | rhs.0) }
        }
        impl std::ops::BitOrAssign for $name {
            #[inline] fn bitor_assign(&mut self, rhs: $name) { self.0 |= rhs.0 }
        }
        impl std::ops::BitAnd for $name {
            type Output = $name;
            #[inline] fn bitand(self, rhs: $name) -> $name { $name(self.0 & rhs.0) }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let mut first = true;
                for (name, bit) in Self::NAMES {
                    if bit.count_ones() == 1 && self.0 & bit != 0 {
                        if !first { write!(f, "|")?; }
                        first = false;
                        write!(f, "{}", name)?;
                    }
                }
                if first { write!(f, "NONE")?; }
                Ok(())
            }
        }
    };
}

bitset! {
    /// The kind of a type. Exactly one kind bit is set, except that boolean/enum literal
    /// types also carry their base (`BOOLEAN_LITERAL`, `ENUM_LITERAL | NUMBER_LITERAL`).
    pub struct TypeFlags: u32 {
        const ANY = 1 << 0;
        const UNKNOWN = 1 << 1;
        const STRING = 1 << 2;
        const NUMBER = 1 << 3;
        const BOOLEAN = 1 << 4;
        /// A non-literal enum type (an enum with computed members).
        const ENUM = 1 << 5;
        const BIGINT = 1 << 6;
        const STRING_LITERAL = 1 << 7;
        const NUMBER_LITERAL = 1 << 8;
        const BOOLEAN_LITERAL = 1 << 9;
        /// Enum member type (with `NUMBER_LITERAL` or `STRING_LITERAL`).
        const ENUM_LITERAL = 1 << 10;
        const BIGINT_LITERAL = 1 << 11;
        const ES_SYMBOL = 1 << 12;
        const UNIQUE_ES_SYMBOL = 1 << 13;
        const VOID = 1 << 14;
        const UNDEFINED = 1 << 15;
        const NULL = 1 << 16;
        const NEVER = 1 << 17;
        const TYPE_PARAMETER = 1 << 18;
        const OBJECT = 1 << 19;
        const UNION = 1 << 20;
        const INTERSECTION = 1 << 21;
        /// `keyof T` for a generic `T`.
        const INDEX = 1 << 22;
        const INDEXED_ACCESS = 1 << 23;
        const CONDITIONAL = 1 << 24;
        const SUBSTITUTION = 1 << 25;
        /// The `object` type.
        const NON_PRIMITIVE = 1 << 26;
        const TEMPLATE_LITERAL = 1 << 27;
        const STRING_MAPPING = 1 << 28;
    }
}

impl TypeFlags {
    pub const LITERAL: TypeFlags = TypeFlags(
        Self::STRING_LITERAL.0
            | Self::NUMBER_LITERAL.0
            | Self::BIGINT_LITERAL.0
            | Self::BOOLEAN_LITERAL.0,
    );
    pub const UNIT: TypeFlags = TypeFlags(
        Self::LITERAL.0
            | Self::UNIQUE_ES_SYMBOL.0
            | Self::UNDEFINED.0
            | Self::NULL.0
            | Self::VOID.0,
    );
    pub const STRING_LIKE: TypeFlags = TypeFlags(
        Self::STRING.0 | Self::STRING_LITERAL.0 | Self::TEMPLATE_LITERAL.0 | Self::STRING_MAPPING.0,
    );
    pub const NUMBER_LIKE: TypeFlags =
        TypeFlags(Self::NUMBER.0 | Self::NUMBER_LITERAL.0 | Self::ENUM.0);
    pub const BIGINT_LIKE: TypeFlags = TypeFlags(Self::BIGINT.0 | Self::BIGINT_LITERAL.0);
    pub const BOOLEAN_LIKE: TypeFlags = TypeFlags(Self::BOOLEAN.0 | Self::BOOLEAN_LITERAL.0);
    pub const ENUM_LIKE: TypeFlags = TypeFlags(Self::ENUM.0 | Self::ENUM_LITERAL.0);
    pub const ES_SYMBOL_LIKE: TypeFlags = TypeFlags(Self::ES_SYMBOL.0 | Self::UNIQUE_ES_SYMBOL.0);
    pub const VOID_LIKE: TypeFlags = TypeFlags(Self::VOID.0 | Self::UNDEFINED.0);
    pub const NULLABLE: TypeFlags = TypeFlags(Self::UNDEFINED.0 | Self::NULL.0);
    pub const PRIMITIVE: TypeFlags = TypeFlags(
        Self::STRING_LIKE.0
            | Self::NUMBER_LIKE.0
            | Self::BIGINT_LIKE.0
            | Self::BOOLEAN_LIKE.0
            | Self::ENUM_LIKE.0
            | Self::ES_SYMBOL_LIKE.0
            | Self::VOID_LIKE.0
            | Self::NULL.0,
    );
    pub const UNION_OR_INTERSECTION: TypeFlags = TypeFlags(Self::UNION.0 | Self::INTERSECTION.0);
    pub const STRUCTURED: TypeFlags =
        TypeFlags(Self::OBJECT.0 | Self::UNION.0 | Self::INTERSECTION.0);
    pub const ANY_OR_UNKNOWN: TypeFlags = TypeFlags(Self::ANY.0 | Self::UNKNOWN.0);
    pub const INSTANTIABLE_NON_PRIMITIVE: TypeFlags = TypeFlags(
        Self::TYPE_PARAMETER.0
            | Self::INDEXED_ACCESS.0
            | Self::CONDITIONAL.0
            | Self::SUBSTITUTION.0,
    );
    pub const INSTANTIABLE_PRIMITIVE: TypeFlags =
        TypeFlags(Self::INDEX.0 | Self::TEMPLATE_LITERAL.0 | Self::STRING_MAPPING.0);
    pub const INSTANTIABLE: TypeFlags =
        TypeFlags(Self::INSTANTIABLE_NON_PRIMITIVE.0 | Self::INSTANTIABLE_PRIMITIVE.0);
    pub const STRUCTURED_OR_INSTANTIABLE: TypeFlags =
        TypeFlags(Self::STRUCTURED.0 | Self::INSTANTIABLE.0);
    /// Types that may be the operand of a narrowing by `typeof` and similar.
    pub const DEFINITELY_FALSY_CANDIDATES: TypeFlags =
        TypeFlags(Self::LITERAL.0 | Self::VOID.0 | Self::UNDEFINED.0 | Self::NULL.0);
}

bitset! {
    /// Facts computed once when a type is interned.
    pub struct TypeTraits: u32 {
        /// A fresh literal (from a literal expression): widens in mutable locations.
        const FRESH = 1 << 0;
        /// Mentions a type parameter somewhere (needs instantiation).
        const CONTAINS_TYPE_PARAMETER = 1 << 1;
        /// An `any`-like error type is a member.
        const CONTAINS_ERROR = 1 << 2;
        /// A fresh object literal type is a member.
        const CONTAINS_OBJECT_LITERAL = 1 << 3;
        /// Contains a widening `null`/`undefined` (non-strict mode).
        const CONTAINS_WIDENING = 1 << 4;
        /// A union produced from an enum declaration (all members of one enum).
        const ENUM_UNION = 1 << 5;
        /// The type is `this` (a class or interface's this-type parameter).
        const THIS_TYPE = 1 << 6;
        /// A union that already had subtype reduction applied.
        const SUBTYPE_REDUCED = 1 << 7;
        /// Contains the wildcard type.
        const CONTAINS_WILDCARD = 1 << 8;
        /// Union containing a `{}`-like or object-like member that makes it non-primitive.
        const PRIMITIVE_UNION = 1 << 9;
        /// A named error type (`TypeStore::named_error`): the error type, printed by name.
        const NAMED_ERROR = 1 << 10;
    }
}

bitset! {
    /// Facts about object types.
    pub struct ObjectFlags: u32 {
        const CLASS = 1 << 0;
        const INTERFACE = 1 << 1;
        const REFERENCE = 1 << 2;
        const TUPLE = 1 << 3;
        const ANONYMOUS = 1 << 4;
        const MAPPED = 1 << 5;
        const INSTANTIATED = 1 << 6;
        /// An object literal expression's type.
        const OBJECT_LITERAL = 1 << 7;
        /// A fresh object literal type (excess property checks apply).
        const FRESH_LITERAL = 1 << 8;
        /// `[...]` array literal.
        const ARRAY_LITERAL = 1 << 9;
        /// The literal contains a computed or spread member: no excess property check.
        const OBJECT_REST_OR_SPREAD = 1 << 10;
        /// Members known only after instantiation of a generic signature.
        const CONTAINS_SPREAD = 1 << 11;
        /// A JS literal (`any`-like members).
        const JS_LITERAL = 1 << 12;
        /// Type of a function or method declaration/expression (`typeof f`).
        const FUNCTION_LIKE = 1 << 13;
        /// The static side of a class (`typeof C`).
        const CLASS_STATIC = 1 << 14;
        /// Namespace or enum object (`typeof NS`).
        const MODULE = 1 << 15;
        /// The tuple is `readonly`.
        const READONLY = 1 << 16;
        /// A type literal written in source (`{ x: T }`).
        const TYPE_LITERAL = 1 << 17;
        /// Created by spreading.
        const SPREAD = 1 << 18;
        /// Object literal had a non-inferrable (context-sensitive) member.
        const NON_INFERRABLE = 1 << 19;
        /// Synthesized intersection/union member resolution.
        const SYNTHETIC = 1 << 20;
        /// Evolving array (`let x = []`).
        const EVOLVING_ARRAY = 1 << 21;
    }
}

bitset! {
    /// Facts about signatures.
    pub struct SigFlags: u16 {
        const HAS_REST = 1 << 0;
        const ABSTRACT = 1 << 1;
        const CONSTRUCT = 1 << 2;
        /// From a declaration with a body (an implementation).
        const IMPLEMENTATION = 1 << 3;
        /// Return type was written.
        const HAS_RETURN_TYPE = 1 << 4;
        /// The function is a method (bivariant parameters under strictFunctionTypes).
        const METHOD = 1 << 5;
        /// Contains context-sensitive parameters (unannotated function expression params).
        const CONTEXT_SENSITIVE = 1 << 6;
        /// A union/intersection of signatures was synthesized.
        const SYNTHETIC = 1 << 7;
        /// The return type is a literal that should be kept.
        const HAS_LITERAL_TYPES = 1 << 8;
        const ASYNC = 1 << 9;
        const GENERATOR = 1 << 10;
        /// Signature created for an arrow/function expression.
        const EXPRESSION = 1 << 11;
        /// A union's signature that stands for several members' signatures, one of them
        /// abstract (`new` of `typeof Concrete | typeof Abstract` is TS2511).
        const UNION_OF_ABSTRACT = 1 << 12;
    }
}
