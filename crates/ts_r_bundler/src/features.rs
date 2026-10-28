//! Target syntax features: `--target=esN` and `--supported:<feature>=false`.
//!
//! esbuild describes a target as the set of JavaScript features it lacks (its CLI names, e.g.
//! `class-field`, `optional-chain`). ts-r-build lowers through `ts_r_transform`'s per-year
//! passes (tsc's ES2015..ES2025 downlevel), so an unsupported feature lowers everything
//! newer than the year before the feature arrived: `class-field` gives ES2021 output. The
//! output then runs the same, though it may lower more than esbuild would. Features from
//! ES2015 and earlier are not lowered (tsc 7 emits ES2015 at the lowest); where esbuild
//! refuses to lower one (`const`, `class`, destructuring, ... at `es5`) the build fails with
//! esbuild's message, and the rest (arrows, template literals) stay as written. A few
//! features change what is emitted rather than lowering (`import-meta`, `dynamic-import`,
//! `top-level-await`, `arbitrary-module-namespace-names`, `bigint`): see `prepare.rs`.

/// One esbuild JavaScript feature name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Feature {
    ArbitraryModuleNamespaceNames,
    ArraySpread,
    Arrow,
    AsyncAwait,
    AsyncGenerator,
    Bigint,
    Class,
    ClassField,
    ClassPrivateAccessor,
    ClassPrivateBrandCheck,
    ClassPrivateField,
    ClassPrivateMethod,
    ClassPrivateStaticAccessor,
    ClassPrivateStaticField,
    ClassPrivateStaticMethod,
    ClassStaticBlocks,
    ClassStaticField,
    ConstAndLet,
    Decorators,
    DefaultArgument,
    Destructuring,
    DynamicImport,
    ExponentOperator,
    ExportStarAs,
    ForAwait,
    ForOf,
    FunctionNameConfigurable,
    FunctionOrClassPropertyAccess,
    Generator,
    Hashbang,
    ImportAssertions,
    ImportAttributes,
    ImportMeta,
    InlineScript,
    LogicalAssignment,
    NestedRestBinding,
    /// CSS nesting (`--supported:nesting=false`; no ECMAScript target lacks it).
    Nesting,
    NewTarget,
    NodeColonPrefixImport,
    NodeColonPrefixRequire,
    NullishCoalescing,
    ObjectAccessors,
    ObjectExtensions,
    ObjectRestSpread,
    OptionalCatchBinding,
    OptionalChain,
    RegexpDotAllFlag,
    RegexpLookbehindAssertions,
    RegexpMatchIndices,
    RegexpNamedCaptureGroups,
    RegexpSetNotation,
    RegexpStickyAndUnicodeFlags,
    RegexpUnicodePropertyEscapes,
    RestArgument,
    TemplateLiteral,
    TopLevelAwait,
    TypeofExoticObjectIsObject,
    UnicodeEscapes,
    Using,
}

use Feature::*;

/// `(feature, esbuild name, ECMAScript year it arrived in)`.
const TABLE: &[(Feature, &str, u32)] = &[
    (
        ArbitraryModuleNamespaceNames,
        "arbitrary-module-namespace-names",
        2022,
    ),
    (ArraySpread, "array-spread", 2015),
    (Arrow, "arrow", 2015),
    (AsyncAwait, "async-await", 2017),
    (AsyncGenerator, "async-generator", 2018),
    (Bigint, "bigint", 2020),
    (Class, "class", 2015),
    (ClassField, "class-field", 2022),
    (ClassPrivateAccessor, "class-private-accessor", 2022),
    (ClassPrivateBrandCheck, "class-private-brand-check", 2022),
    (ClassPrivateField, "class-private-field", 2022),
    (ClassPrivateMethod, "class-private-method", 2022),
    (
        ClassPrivateStaticAccessor,
        "class-private-static-accessor",
        2022,
    ),
    (ClassPrivateStaticField, "class-private-static-field", 2022),
    (
        ClassPrivateStaticMethod,
        "class-private-static-method",
        2022,
    ),
    (ClassStaticBlocks, "class-static-blocks", 2022),
    (ClassStaticField, "class-static-field", 2022),
    (ConstAndLet, "const-and-let", 2015),
    (Decorators, "decorators", 2026),
    (DefaultArgument, "default-argument", 2015),
    (Destructuring, "destructuring", 2015),
    (DynamicImport, "dynamic-import", 2020),
    (ExponentOperator, "exponent-operator", 2016),
    (ExportStarAs, "export-star-as", 2020),
    (ForAwait, "for-await", 2018),
    (ForOf, "for-of", 2015),
    (FunctionNameConfigurable, "function-name-configurable", 2015),
    (
        FunctionOrClassPropertyAccess,
        "function-or-class-property-access",
        2015,
    ),
    (Generator, "generator", 2015),
    (Hashbang, "hashbang", 2023),
    (ImportAssertions, "import-assertions", 2026),
    (ImportAttributes, "import-attributes", 2025),
    (ImportMeta, "import-meta", 2020),
    (InlineScript, "inline-script", 0),
    (LogicalAssignment, "logical-assignment", 2021),
    (NestedRestBinding, "nested-rest-binding", 2016),
    (Nesting, "nesting", 0),
    (NewTarget, "new-target", 2015),
    (NodeColonPrefixImport, "node-colon-prefix-import", 0),
    (NodeColonPrefixRequire, "node-colon-prefix-require", 0),
    (NullishCoalescing, "nullish-coalescing", 2020),
    (ObjectAccessors, "object-accessors", 2009),
    (ObjectExtensions, "object-extensions", 2015),
    (ObjectRestSpread, "object-rest-spread", 2018),
    (OptionalCatchBinding, "optional-catch-binding", 2019),
    (OptionalChain, "optional-chain", 2020),
    (RegexpDotAllFlag, "regexp-dot-all-flag", 2018),
    (
        RegexpLookbehindAssertions,
        "regexp-lookbehind-assertions",
        2018,
    ),
    (RegexpMatchIndices, "regexp-match-indices", 2022),
    (
        RegexpNamedCaptureGroups,
        "regexp-named-capture-groups",
        2018,
    ),
    (RegexpSetNotation, "regexp-set-notation", 2024),
    (
        RegexpStickyAndUnicodeFlags,
        "regexp-sticky-and-unicode-flags",
        2015,
    ),
    (
        RegexpUnicodePropertyEscapes,
        "regexp-unicode-property-escapes",
        2018,
    ),
    (RestArgument, "rest-argument", 2015),
    (TemplateLiteral, "template-literal", 2015),
    (TopLevelAwait, "top-level-await", 2022),
    (
        TypeofExoticObjectIsObject,
        "typeof-exotic-object-is-object",
        0,
    ),
    (UnicodeEscapes, "unicode-escapes", 2015),
    (Using, "using", 2026),
];

/// Features that lowering by year handles: the target drops to the year before them.
/// The rest (ES2015-level syntax, module-level features, regexp flags) are handled one by
/// one or not at all.
fn lowers_by_year(f: Feature) -> bool {
    matches!(
        f,
        AsyncAwait
            | AsyncGenerator
            | ClassField
            | ClassPrivateAccessor
            | ClassPrivateBrandCheck
            | ClassPrivateField
            | ClassPrivateMethod
            | ClassPrivateStaticAccessor
            | ClassPrivateStaticField
            | ClassPrivateStaticMethod
            | ClassStaticBlocks
            | ClassStaticField
            | Decorators
            | ExponentOperator
            | ForAwait
            | LogicalAssignment
            | NullishCoalescing
            | ObjectRestSpread
            | OptionalCatchBinding
            | OptionalChain
            | Using
    )
}

impl Feature {
    /// The feature with esbuild's name `name`.
    pub fn parse(name: &str) -> Option<Feature> {
        TABLE
            .iter()
            .find(|(_, n, _)| *n == name)
            .map(|(f, _, _)| *f)
    }

    fn year(self) -> u32 {
        TABLE[self as usize].2
    }
}

/// A set of unsupported features (bit `f as u8`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Features(u64);

impl Features {
    pub const NONE: Features = Features(0);

    pub fn insert(&mut self, f: Feature) {
        self.0 |= 1 << f as u8;
    }

    pub fn remove(&mut self, f: Feature) {
        self.0 &= !(1 << f as u8);
    }

    pub fn has(self, f: Feature) -> bool {
        self.0 & (1 << f as u8) != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Every feature newer than ECMAScript `year` (`--target=es<year>`; ES5 is 2014 here, as
    /// `parse_target` maps it).
    pub fn newer_than(year: u32) -> Features {
        let mut s = Features::NONE;
        for &(f, _, y) in TABLE {
            if y > year && y > 2009 {
                s.insert(f);
            }
        }
        s
    }

    /// The ECMAScript year to lower to: the year before the oldest unsupported feature that
    /// lowering by year handles (`None` = nothing to lower).
    pub fn lower_year(self) -> Option<u32> {
        TABLE
            .iter()
            .filter(|(f, _, _)| self.has(*f) && lowers_by_year(*f))
            .map(|(f, _, _)| f.year() - 1)
            .min()
    }

    /// Stable text for cache keys.
    pub fn key(self) -> String {
        format!("{:x}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_in_enum_order() {
        for (i, (f, _, _)) in TABLE.iter().enumerate() {
            assert_eq!(*f as usize, i);
        }
    }

    #[test]
    fn years() {
        let mut s = Features::NONE;
        assert_eq!(s.lower_year(), None);
        s.insert(Feature::parse("class-field").unwrap());
        assert_eq!(s.lower_year(), Some(2021));
        s.insert(Feature::parse("optional-chain").unwrap());
        assert_eq!(s.lower_year(), Some(2019));
        assert_eq!(Features::newer_than(2019).lower_year(), Some(2019));
        assert_eq!(Features::newer_than(2014).lower_year(), Some(2015));
        assert!(Features::newer_than(2014).has(Feature::ConstAndLet));
        assert!(!Features::newer_than(2015).has(Feature::ConstAndLet));
        assert!(Feature::parse("bogus").is_none());
        let mut u = Features::newer_than(2021);
        u.insert(Feature::Using);
        u.remove(Feature::ClassField);
        assert!(!u.has(Feature::ClassField));
    }
}
