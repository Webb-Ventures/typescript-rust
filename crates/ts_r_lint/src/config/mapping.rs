//! Mapping configured rule names (ESLint core, typescript-eslint, oxlint, Biome) to ts-r-lint
//! rules, and classifying the ones we cannot run.

use crate::registry::Registry;

/// Why a configured rule is not run.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum UnsupportedReason {
    /// A typescript-eslint rule that needs type information and is not implemented yet.
    NeedsTypes,
    /// A type-aware rule enabled for files without `parserOptions.project` or
    /// `projectService`.
    NoProject,
    /// A type-aware rule enabled for files no tsconfig covers.
    NotInProject,
    /// Type checking is turned off (`--no-type-check`).
    TypeCheckOff,
    /// The TypeScript lib files were not found (neither ts-r's nor the project's).
    NoLibFiles,
    /// Our checker's diagnostics for the project differ from the user's tsc: type-aware
    /// rules are not run there rather than risk wrong reports.
    ProjectMismatch,
    /// Our checker failed on the project (an internal error, reported as a note): type-aware
    /// rules are not run there.
    InternalError,
    /// A rule from a plugin ts-r-lint does not implement (`react-hooks/...`).
    Plugin,
    /// An ESLint core or typescript-eslint rule not implemented yet.
    NotImplemented,
    /// A name no known tool defines (typo, removed rule).
    Unknown,
}

impl UnsupportedReason {
    pub fn describe(self) -> &'static str {
        match self {
            UnsupportedReason::NeedsTypes => "type-aware rule not implemented yet",
            UnsupportedReason::NoProject => {
                "needs type information; no parserOptions.project or projectService"
            }
            UnsupportedReason::NotInProject => "needs type information; no tsconfig for the files",
            UnsupportedReason::TypeCheckOff => "needs type information; type checking is off",
            UnsupportedReason::NoLibFiles => {
                "needs type information; TypeScript lib files not found"
            }
            UnsupportedReason::ProjectMismatch => {
                "ts-r's type check differs from tsc in this project; type-aware rule not run here"
            }
            UnsupportedReason::InternalError => {
                "ts-r's type checker failed on this project (internal error); type-aware rule not run here"
            }
            UnsupportedReason::Plugin => "plugin rule, plugin not implemented",
            UnsupportedReason::NotImplemented => "not implemented yet",
            UnsupportedReason::Unknown => "unknown rule",
        }
    }
}

/// typescript-eslint rules that require type information (typescript-eslint 8.71).
pub const TS_TYPE_AWARE: &[&str] = &[
    "await-thenable",
    "consistent-return",
    "consistent-type-exports",
    "dot-notation",
    "naming-convention",
    "no-array-delete",
    "no-base-to-string",
    "no-confusing-void-expression",
    "no-deprecated",
    "no-duplicate-type-constituents",
    "no-floating-promises",
    "no-for-in-array",
    "no-generated-empty-object-type",
    "no-implied-eval",
    "no-meaningless-void-operator",
    "no-misused-promises",
    "no-misused-spread",
    "no-mixed-enums",
    "no-redundant-type-constituents",
    "no-unnecessary-boolean-literal-compare",
    "no-unnecessary-condition",
    "no-unnecessary-qualifier",
    "no-unnecessary-template-expression",
    "no-unnecessary-type-arguments",
    "no-unnecessary-type-assertion",
    "no-unnecessary-type-conversion",
    "no-unnecessary-type-parameters",
    "no-unsafe-argument",
    "no-unsafe-assignment",
    "no-unsafe-call",
    "no-unsafe-enum-assignment",
    "no-unsafe-enum-comparison",
    "no-unsafe-member-access",
    "no-unsafe-return",
    "no-unsafe-type-assertion",
    "no-unsafe-unary-minus",
    "no-useless-default-assignment",
    "non-nullable-type-assertion-style",
    "only-throw-error",
    "prefer-destructuring",
    "prefer-find",
    "prefer-includes",
    "prefer-nullish-coalescing",
    "prefer-optional-chain",
    "prefer-promise-reject-errors",
    "prefer-readonly",
    "prefer-readonly-parameter-types",
    "prefer-reduce-type-parameter",
    "prefer-regexp-exec",
    "prefer-return-this-type",
    "prefer-string-starts-ends-with",
    "promise-function-async",
    "related-getter-setter-pairs",
    "require-array-sort-compare",
    "require-await",
    "restrict-plus-operands",
    "restrict-template-expressions",
    "return-await",
    "strict-boolean-expressions",
    "strict-void-return",
    "switch-exhaustiveness-check",
    "unbound-method",
    "use-unknown-in-catch-callback-variable",
];

/// Resolves an ESLint-style name to a built-in rule name, or classifies it.
pub fn map_eslint_name(reg: &Registry, name: &str) -> Result<&'static str, UnsupportedReason> {
    if let Some(d) = reg.find(name) {
        return Ok(d.meta.name);
    }
    if let Some(ts) = name.strip_prefix("@typescript-eslint/") {
        if TS_TYPE_AWARE.contains(&ts) {
            return Err(UnsupportedReason::NeedsTypes);
        }
        return Err(UnsupportedReason::NotImplemented);
    }
    if name.contains('/') {
        return Err(UnsupportedReason::Plugin);
    }
    Err(UnsupportedReason::NotImplemented)
}

/// Resolves an oxlint rule name (`no-debugger`, `eslint/no-debugger`,
/// `typescript/no-explicit-any`, `@typescript-eslint/no-explicit-any`).
pub fn map_oxlint_name(reg: &Registry, name: &str) -> Result<&'static str, UnsupportedReason> {
    let (plugin, rule) = match name.split_once('/') {
        Some((p, r)) if !name.starts_with('@') => (Some(p), r),
        _ => (None, name),
    };
    match plugin {
        Some("eslint") => map_eslint_name(reg, rule),
        Some("typescript") => map_eslint_name(reg, &format!("@typescript-eslint/{rule}")),
        // Plugins whose rules ts-r-lint names as ESLint does.
        Some("jsdoc" | "unicorn") => map_eslint_name(reg, name),
        Some(_) => Err(UnsupportedReason::Plugin),
        None => {
            if let Ok(n) = map_eslint_name(reg, rule) {
                return Ok(n);
            }
            map_eslint_name(reg, &format!("@typescript-eslint/{rule}"))
                .or_else(|_| map_eslint_name(reg, rule))
        }
    }
}

/// Biome rules with an equivalent ts-r-lint rule: (group, Biome name, our name,
/// in Biome's recommended set).
pub const BIOME_RULES: &[(&str, &str, &str, bool)] = &[
    ("suspicious", "noDebugger", "no-debugger", true),
    (
        "suspicious",
        "noExplicitAny",
        "@typescript-eslint/no-explicit-any",
        true,
    ),
    ("suspicious", "noDoubleEquals", "eqeqeq", true),
    ("suspicious", "noDuplicateCase", "no-duplicate-case", true),
    (
        "suspicious",
        "noDuplicateClassMembers",
        "no-dupe-class-members",
        true,
    ),
    ("suspicious", "noDuplicateObjectKeys", "no-dupe-keys", true),
    ("suspicious", "noDuplicateParameters", "no-dupe-args", true),
    ("suspicious", "noDuplicateElseIf", "no-dupe-else-if", true),
    (
        "suspicious",
        "noCompareNegZero",
        "no-compare-neg-zero",
        true,
    ),
    (
        "suspicious",
        "noControlCharactersInRegex",
        "no-control-regex",
        true,
    ),
    ("suspicious", "noEmptyBlockStatements", "no-empty", false),
    (
        "suspicious",
        "noFallthroughSwitchClause",
        "no-fallthrough",
        true,
    ),
    ("suspicious", "noFunctionAssign", "no-func-assign", true),
    ("suspicious", "noGlobalAssign", "no-global-assign", true),
    ("suspicious", "noImportAssign", "no-import-assign", true),
    (
        "suspicious",
        "noMisleadingCharacterClass",
        "no-misleading-character-class",
        true,
    ),
    (
        "suspicious",
        "noPrototypeBuiltins",
        "no-prototype-builtins",
        true,
    ),
    ("suspicious", "noRedeclare", "no-redeclare", true),
    ("suspicious", "noSelfCompare", "no-self-compare", true),
    (
        "suspicious",
        "noShadowRestrictedNames",
        "no-shadow-restricted-names",
        true,
    ),
    ("suspicious", "noSparseArray", "no-sparse-arrays", true),
    ("suspicious", "noUnsafeNegation", "no-unsafe-negation", true),
    (
        "suspicious",
        "noAsyncPromiseExecutor",
        "no-async-promise-executor",
        true,
    ),
    ("suspicious", "noCatchAssign", "no-ex-assign", true),
    ("suspicious", "noClassAssign", "no-class-assign", true),
    (
        "suspicious",
        "noAssignInExpressions",
        "no-cond-assign",
        true,
    ),
    ("suspicious", "noConsole", "no-console", false),
    ("suspicious", "noVar", "no-var", false),
    (
        "suspicious",
        "noUnsafeDeclarationMerging",
        "@typescript-eslint/no-unsafe-declaration-merging",
        true,
    ),
    (
        "suspicious",
        "noExtraNonNullAssertion",
        "@typescript-eslint/no-extra-non-null-assertion",
        true,
    ),
    (
        "suspicious",
        "noMisleadingInstantiator",
        "@typescript-eslint/no-misused-new",
        true,
    ),
    (
        "suspicious",
        "noDuplicateEnumValues",
        "@typescript-eslint/no-duplicate-enum-values",
        false,
    ),
    ("suspicious", "noWith", "no-with", true),
    (
        "suspicious",
        "noIrregularWhitespace",
        "no-irregular-whitespace",
        false,
    ),
    (
        "suspicious",
        "noUselessEscapeInString",
        "no-useless-escape",
        false,
    ),
    ("suspicious", "useGetterReturn", "getter-return", true),
    ("suspicious", "useIsArray", "no-instanceof-array", false),
    ("correctness", "noConstAssign", "no-const-assign", true),
    (
        "correctness",
        "noConstantCondition",
        "no-constant-condition",
        true,
    ),
    (
        "correctness",
        "noConstantMathMinMaxClamp",
        "no-constant-binary-expression",
        false,
    ),
    (
        "correctness",
        "noEmptyCharacterClassInRegex",
        "no-empty-character-class",
        true,
    ),
    ("correctness", "noEmptyPattern", "no-empty-pattern", true),
    ("correctness", "noGlobalObjectCalls", "no-obj-calls", true),
    (
        "correctness",
        "noInnerDeclarations",
        "no-inner-declarations",
        false,
    ),
    (
        "correctness",
        "noInvalidConstructorSuper",
        "constructor-super",
        true,
    ),
    (
        "correctness",
        "noNonoctalDecimalEscape",
        "no-nonoctal-decimal-escape",
        true,
    ),
    (
        "correctness",
        "noPrecisionLoss",
        "no-loss-of-precision",
        true,
    ),
    ("correctness", "noSelfAssign", "no-self-assign", true),
    ("correctness", "noSetterReturn", "no-setter-return", true),
    (
        "correctness",
        "noSwitchDeclarations",
        "no-case-declarations",
        true,
    ),
    ("correctness", "noUndeclaredVariables", "no-undef", false),
    ("correctness", "noUnreachable", "no-unreachable", true),
    (
        "correctness",
        "noUnreachableSuper",
        "no-this-before-super",
        true,
    ),
    ("correctness", "noUnsafeFinally", "no-unsafe-finally", true),
    (
        "correctness",
        "noUnsafeOptionalChaining",
        "no-unsafe-optional-chaining",
        true,
    ),
    ("correctness", "noUnusedLabels", "no-unused-labels", true),
    (
        "correctness",
        "noUnusedPrivateClassMembers",
        "no-unused-private-class-members",
        false,
    ),
    ("correctness", "noUnusedVariables", "no-unused-vars", false),
    (
        "correctness",
        "noInvalidBuiltinInstantiation",
        "no-new-native-nonconstructor",
        true,
    ),
    ("correctness", "useIsNan", "use-isnan", true),
    ("correctness", "useValidForDirection", "for-direction", true),
    ("correctness", "useValidTypeof", "valid-typeof", true),
    ("correctness", "useYield", "require-yield", true),
    ("complexity", "noUselessCatch", "no-useless-catch", true),
    (
        "complexity",
        "noUselessConstructor",
        "no-useless-constructor",
        true,
    ),
    (
        "complexity",
        "noExtraBooleanCast",
        "no-extra-boolean-cast",
        true,
    ),
    (
        "complexity",
        "noUselessTypeConstraint",
        "@typescript-eslint/no-unnecessary-type-constraint",
        true,
    ),
    ("complexity", "noUselessLabel", "no-extra-label", true),
    (
        "complexity",
        "useRegexLiterals",
        "prefer-regex-literals",
        false,
    ),
    ("complexity", "noArguments", "prefer-rest-params", false),
    (
        "complexity",
        "noStaticOnlyClass",
        "@typescript-eslint/no-extraneous-class",
        false,
    ),
    (
        "style",
        "noNamespace",
        "@typescript-eslint/no-namespace",
        false,
    ),
    (
        "style",
        "noNonNullAssertion",
        "@typescript-eslint/no-non-null-assertion",
        false,
    ),
    ("style", "useConst", "prefer-const", false),
    (
        "style",
        "useAsConstAssertion",
        "@typescript-eslint/prefer-as-const",
        true,
    ),
    (
        "style",
        "useImportType",
        "@typescript-eslint/consistent-type-imports",
        false,
    ),
    (
        "style",
        "useNamespaceKeyword",
        "@typescript-eslint/prefer-namespace-keyword",
        true,
    ),
    (
        "style",
        "noInferrableTypes",
        "@typescript-eslint/no-inferrable-types",
        false,
    ),
    (
        "style",
        "useLiteralEnumMembers",
        "@typescript-eslint/prefer-literal-enum-member",
        false,
    ),
    (
        "style",
        "useConsistentArrayType",
        "@typescript-eslint/array-type",
        false,
    ),
    ("style", "noUselessElse", "no-else-return", false),
    ("style", "useSpread", "prefer-spread", false),
    (
        "style",
        "useExponentiationOperator",
        "prefer-exponentiation-operator",
        false,
    ),
    ("style", "useTemplate", "prefer-template", false),
    ("style", "noParameterAssign", "no-param-reassign", false),
    (
        "style",
        "useDefaultParameterLast",
        "default-param-last",
        false,
    ),
    ("security", "noGlobalEval", "no-eval", true),
    (
        "suspicious",
        "noEmptyInterface",
        "@typescript-eslint/no-empty-object-type",
        false,
    ),
    ("suspicious", "noShadow", "no-shadow", false),
    (
        "suspicious",
        "noTsIgnore",
        "@typescript-eslint/ban-ts-comment",
        false,
    ),
    ("suspicious", "noConstEnum", "no-restricted-syntax", false),
    (
        "suspicious",
        "noWrapperObjectTypes",
        "@typescript-eslint/no-wrapper-object-types",
        false,
    ),
    (
        "style",
        "noRestrictedTypes",
        "@typescript-eslint/no-restricted-types",
        false,
    ),
    (
        "complexity",
        "noBannedTypes",
        "@typescript-eslint/no-unsafe-function-type",
        true,
    ),
    (
        "correctness",
        "noNewSymbol",
        "no-new-native-nonconstructor",
        false,
    ),
    (
        "correctness",
        "noInvalidUseBeforeDeclaration",
        "no-use-before-define",
        true,
    ),
    (
        "suspicious",
        "noCommentText",
        "react/jsx-no-comment-textnodes",
        true,
    ),
    (
        "nursery",
        "noUselessBackrefInRegex",
        "no-useless-backreference",
        false,
    ),
    ("style", "noUselessCatchBinding", "no-useless-catch", false),
    (
        "suspicious",
        "noUnassignedVariables",
        "no-unassigned-vars",
        false,
    ),
    (
        "correctness",
        "noUselessAssignment",
        "no-useless-assignment",
        false,
    ),
];

/// Resolves a Biome rule (`group`, `name`) to a ts-r-lint rule.
pub fn map_biome_name(
    reg: &Registry,
    group: &str,
    name: &str,
) -> Result<&'static str, UnsupportedReason> {
    match BIOME_RULES
        .iter()
        .find(|(g, n, _, _)| *g == group && *n == name)
        .or_else(|| BIOME_RULES.iter().find(|(_, n, _, _)| *n == name))
    {
        Some((_, _, ours, _)) => reg
            .find(ours)
            .map(|d| d.meta.name)
            .ok_or(UnsupportedReason::NotImplemented),
        None => Err(UnsupportedReason::NotImplemented),
    }
}
