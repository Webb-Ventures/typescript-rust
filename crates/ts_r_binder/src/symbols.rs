//! Symbols, symbol flags, the conflict ("excludes") table and symbol tables.

use ts_r_ast::NodeId;

use crate::names::Name;

macro_rules! flags32 {
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
            fn bitor(self, rhs: $name) -> $name { $name(self.0 | rhs.0) }
        }
        impl std::ops::BitOrAssign for $name {
            fn bitor_assign(&mut self, rhs: $name) { self.0 |= rhs.0 }
        }
        impl std::ops::BitAnd for $name {
            type Output = $name;
            fn bitand(self, rhs: $name) -> $name { $name(self.0 & rhs.0) }
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

flags32! {
    /// What a symbol declares. Meaning groups (`VALUE`, `TYPE`, `NAMESPACE`) are unions.
    pub struct SymbolFlags: u32 {
        const FUNCTION_SCOPED_VARIABLE = 1 << 0;
        const BLOCK_SCOPED_VARIABLE = 1 << 1;
        const PROPERTY = 1 << 2;
        const ENUM_MEMBER = 1 << 3;
        const FUNCTION = 1 << 4;
        const CLASS = 1 << 5;
        const INTERFACE = 1 << 6;
        const CONST_ENUM = 1 << 7;
        const REGULAR_ENUM = 1 << 8;
        const VALUE_MODULE = 1 << 9;
        const NAMESPACE_MODULE = 1 << 10;
        const TYPE_LITERAL = 1 << 11;
        const OBJECT_LITERAL = 1 << 12;
        const METHOD = 1 << 13;
        const CONSTRUCTOR = 1 << 14;
        const GET_ACCESSOR = 1 << 15;
        const SET_ACCESSOR = 1 << 16;
        const SIGNATURE = 1 << 17;
        const TYPE_PARAMETER = 1 << 18;
        const TYPE_ALIAS = 1 << 19;
        const ALIAS = 1 << 20;
        const EXPORT_STAR = 1 << 21;
        const OPTIONAL = 1 << 22;
        /// A function or constructor parameter (also `FUNCTION_SCOPED_VARIABLE`).
        const PARAMETER = 1 << 23;
        /// `export default <expr>` / `export = <expr>` whose expression is not a name.
        const EXPORT_EXPRESSION = 1 << 24;
        /// The module symbol of a file, or an ambient module (`declare module "x"`).
        const MODULE_FILE = 1 << 25;
    }
}

flags32! {
    /// Facts about a symbol that are not about meaning.
    pub struct SymbolFacts: u16 {
        const EXPORTED = 1 << 0;
        const AMBIENT = 1 << 1;
        const ABSTRACT = 1 << 2;
        const TYPE_ONLY = 1 << 3;
        const DEFAULT_EXPORT = 1 << 4;
        /// Written after its declaration (assignment, `++`, destructuring target, `for-in/of`).
        const ASSIGNED = 1 << 5;
        const STATIC = 1 << 6;
        const PRIVATE_NAME = 1 << 7;
        /// Namespace whose only values are const enums.
        const CONST_ENUM_ONLY = 1 << 8;
        /// Declared by `export { x }` / `export { x } from` / `export * as ns`: not visible as a
        /// local name.
        const EXPORT_SPECIFIER = 1 << 9;
        /// A conflicting declaration that was not entered into its table.
        const DETACHED = 1 << 10;
        const READONLY = 1 << 11;
        /// `let`/`const`/`using`/class/enum: subject to temporal dead zone checks.
        const BLOCK_SCOPED = 1 << 12;
        /// A property of a function declared by an assignment (`f.x = 1`): an expando.
        const EXPANDO = 1 << 13;
    }
}

impl SymbolFlags {
    pub const ENUM: SymbolFlags = SymbolFlags(Self::CONST_ENUM.0 | Self::REGULAR_ENUM.0);
    pub const VARIABLE: SymbolFlags =
        SymbolFlags(Self::FUNCTION_SCOPED_VARIABLE.0 | Self::BLOCK_SCOPED_VARIABLE.0);
    pub const ACCESSOR: SymbolFlags = SymbolFlags(Self::GET_ACCESSOR.0 | Self::SET_ACCESSOR.0);
    pub const VALUE: SymbolFlags = SymbolFlags(
        Self::VARIABLE.0
            | Self::PROPERTY.0
            | Self::ENUM_MEMBER.0
            | Self::OBJECT_LITERAL.0
            | Self::FUNCTION.0
            | Self::CLASS.0
            | Self::ENUM.0
            | Self::VALUE_MODULE.0
            | Self::METHOD.0
            | Self::ACCESSOR.0
            | Self::EXPORT_EXPRESSION.0,
    );
    pub const TYPE: SymbolFlags = SymbolFlags(
        Self::CLASS.0
            | Self::INTERFACE.0
            | Self::ENUM.0
            | Self::ENUM_MEMBER.0
            | Self::TYPE_LITERAL.0
            | Self::TYPE_PARAMETER.0
            | Self::TYPE_ALIAS.0,
    );
    pub const NAMESPACE: SymbolFlags =
        SymbolFlags(Self::VALUE_MODULE.0 | Self::NAMESPACE_MODULE.0 | Self::ENUM.0);
    pub const MODULE: SymbolFlags = SymbolFlags(Self::VALUE_MODULE.0 | Self::NAMESPACE_MODULE.0);
}

/// What a declaration may not merge with. Measured on tsc 7.0.2; see
/// `docs/design/0003-binder.md` ("Merging and conflicts"). The check is symmetric.
pub mod excludes {
    use super::SymbolFlags as F;

    const V: u32 = F::FUNCTION_SCOPED_VARIABLE.0;
    const L: u32 = F::BLOCK_SCOPED_VARIABLE.0;
    const FN: u32 = F::FUNCTION.0;
    const C: u32 = F::CLASS.0;
    const I: u32 = F::INTERFACE.0;
    const T: u32 = F::TYPE_ALIAS.0;
    const E: u32 = F::REGULAR_ENUM.0;
    const CE: u32 = F::CONST_ENUM.0;
    const NS: u32 = F::VALUE_MODULE.0;
    /// `export default <non-entity expression>`: value declarations bound after it conflict.
    const XE: u32 = F::EXPORT_EXPRESSION.0;

    pub const FUNCTION_SCOPED_VARIABLE: F = F(L | FN | C | E | CE | NS | XE);
    pub const PARAMETER: F = F::PARAMETER;
    pub const BLOCK_SCOPED_VARIABLE: F = F(V | L | FN | C | E | CE | NS | XE);
    pub const FUNCTION: F = F(V | L | E | CE | XE);
    pub const CLASS: F = F(V | L | C | T | E | CE | XE);
    pub const INTERFACE: F = F(T | E | CE);
    pub const TYPE_ALIAS: F = F(C | I | T | E | CE);
    pub const REGULAR_ENUM: F = F(V | L | FN | C | I | T | CE | XE);
    pub const CONST_ENUM: F = F(V | L | FN | C | I | T | E | NS | XE);
    pub const VALUE_MODULE: F = F(V | L | CE);
    pub const NAMESPACE_MODULE: F = F::NONE;
    pub const ALIAS: F = F::ALIAS;
    /// `export default <expr>` and `export = <expr>` (name or not) conflict with any
    /// default export bound before them; later declarations decide by their own excludes
    /// (a class after `export default foo` is the checker's TS2323).
    pub const EXPORT_EXPRESSION: F = F(F::VALUE.0 | F::TYPE.0 | F::NAMESPACE.0 | F::ALIAS.0);
    /// Type parameters conflict with other types of the same declaration space (a function's
    /// top-level locals); a repeated type parameter is reported on the repeat only.
    pub const TYPE_PARAMETER: F = F(F::TYPE.0 & !F::TYPE_PARAMETER.0);
    /// `infer U` may repeat.
    pub const INFER_TYPE_PARAMETER: F = F::NONE;
    pub const ENUM_MEMBER: F = F::VALUE;

    // Class, interface and type literal members.
    pub const PROPERTY: F = F::VALUE;
    pub const METHOD: F = F(F::VALUE.0 & !F::METHOD.0);
    /// Interface and type literal properties: same-named properties of different declarations
    /// of a merged interface merge (the checker compares their types); a repeat within one
    /// body is reported separately.
    pub const PROPERTY_SIGNATURE: F = F(F::VALUE.0 & !F::PROPERTY.0);
    pub const GET_ACCESSOR: F = F(F::VALUE.0 & !F::SET_ACCESSOR.0);
    pub const SET_ACCESSOR: F = F(F::VALUE.0 & !F::GET_ACCESSOR.0);

    // Object literal members (duplicates there are grammar errors 1117/1118/1119 instead).
    pub const OBJ_PROPERTY: F = F::METHOD;
    pub const OBJ_METHOD: F = F(F::PROPERTY.0 | F::METHOD.0 | F::ACCESSOR.0);
    pub const OBJ_GET_ACCESSOR: F = F(F::GET_ACCESSOR.0 | F::METHOD.0);
    pub const OBJ_SET_ACCESSOR: F = F(F::SET_ACCESSOR.0 | F::METHOD.0);
}

/// Index into `BoundFile::symbols`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
pub struct SymbolId(pub u32);

impl SymbolId {
    #[inline]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A symbol table. `TableId::NONE` (0) is the empty table.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
pub struct TableId(pub u32);

impl TableId {
    pub const NONE: TableId = TableId(0);

    #[inline]
    pub fn is_none(self) -> bool {
        self.0 == 0
    }
}

/// One symbol. See the design record for field meanings.
#[derive(Clone, Copy, Debug)]
pub struct Symbol {
    pub name: Name,
    pub flags: SymbolFlags,
    pub facts: SymbolFacts,
    /// `start..start+len` in `BoundFile::decl_nodes`, in binding order.
    pub decl_start: u32,
    pub decl_len: u32,
    /// First declaration with a value meaning.
    pub value_decl: Option<NodeId>,
    /// Container (class, interface, namespace, enum, module) for members and exports.
    pub parent: Option<SymbolId>,
    pub members: TableId,
    pub exports: TableId,
}

/// A finished table: `start..start+len` in `BoundFile::table_entries`, sorted by name.
#[derive(Clone, Copy, Debug, Default)]
pub struct TableRange {
    pub start: u32,
    pub len: u32,
}
