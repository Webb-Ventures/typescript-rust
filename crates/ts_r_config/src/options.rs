//! Compiler options: the declaration table, typed values and computed defaults.
//!
//! [`OPTIONS`] lists every option tsc 7 accepts in a tsconfig, in the order `--showConfig`
//! prints them (observed from `tsc --showConfig`, TypeScript 7.0.2). [`CompilerOptions`]
//! stores explicitly set values indexed by [`OptionId`] and exposes one typed accessor per
//! option (`opts.strict() -> Option<bool>`). Defaults that depend on other options live in
//! [`Effective`] (`opts.effective().module_resolution()`).

use std::sync::Arc;

use crate::enums::{
    JsxEmit, ModuleDetection, ModuleKind, ModuleResolution, NewLine, ScriptTarget, lib_file_name,
};
use crate::jsonc;

/// Value type of an option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionType {
    /// `true` / `false`.
    Bool,
    /// A number.
    Number,
    /// A string.
    String,
    /// A file or directory path, resolved against the config that sets it.
    Path,
    /// A list of strings.
    StringList,
    /// A list of paths, each resolved against the config that sets it.
    PathList,
    /// `lib`: a list of library names.
    Lib,
    /// `target`.
    Target,
    /// `module`.
    Module,
    /// `moduleResolution`.
    ModuleResolution,
    /// `moduleDetection`.
    ModuleDetection,
    /// `jsx`.
    Jsx,
    /// `newLine`.
    NewLine,
    /// `paths`: pattern → substitutions.
    PathMap,
    /// `plugins`: a list of objects.
    Plugins,
}

impl OptionType {
    /// Type name used in TS5024 "requires a value of type X".
    pub fn type_name(self) -> &'static str {
        match self {
            Self::Bool => "boolean",
            Self::Number => "number",
            Self::String | Self::Path => "string",
            Self::StringList | Self::PathList | Self::Lib | Self::Plugins => "Array",
            Self::Target
            | Self::Module
            | Self::ModuleResolution
            | Self::ModuleDetection
            | Self::Jsx
            | Self::NewLine => "enum",
            Self::PathMap => "object",
        }
    }
}

/// What changing an option can change (design record 0012, "Option classes"). Observed on
/// tsc 7.0.2 by building a project with and without each option, fresh and incremental (the
/// probes are described in `docs/design/0012-tsc-emit.md`, "Implementation notes"). The
/// classes are conservative: an option is in a class when changing it can change that output,
/// also when tsc's own incremental build does not notice the change.
pub mod affects {
    /// Check diagnostics: the checker's behaviour, the program's files, module resolution or
    /// the libs. A check-history key (design record 0005) must cover every option in this
    /// class. Option-check diagnostics (`verify.rs`, TS5xxx at the tsconfig) do not count:
    /// they are recomputed from the config on every run.
    pub const CHECK: u8 = 1;
    /// The bytes of emitted JavaScript or of its source maps.
    pub const EMIT: u8 = 2;
    /// The bytes of emitted `.d.ts` files or of their maps, other than through inferred types
    /// (those depend on `CHECK` options too, so a `.d.ts` cache key needs both classes).
    pub const DECL_EMIT: u8 = 4;
    /// Which files are written, or where.
    pub const OUTPUT_PATHS: u8 = 8;
    /// Stored in tsc's `.tsbuildinfo` `options` (observed in the written file).
    pub const BUILDINFO: u8 = 16;
}

/// Option flags.
pub mod flags {
    /// Not printed by `--showConfig`.
    pub const HIDDEN: u8 = 1;
    /// Only valid on the command line; TS6266 in a tsconfig.
    pub const CMDLINE_ONLY: u8 = 2;
}

/// Declaration of one compiler option.
#[derive(Debug, Clone, Copy)]
pub struct OptionDecl {
    /// Identifier.
    pub id: OptionId,
    /// Name as written in tsconfig (`allowJs`).
    pub name: &'static str,
    /// Value type.
    pub ty: OptionType,
    /// [`flags`] bits.
    pub flags: u8,
    /// [`affects`] bits.
    pub affects: u8,
}

impl OptionDecl {
    /// True when the option is in every class of `mask` ([`affects`] bits).
    pub fn affects(&self, mask: u8) -> bool {
        self.affects & mask == mask
    }
}

/// `paths`: ordered list of (pattern, substitutions).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathsMap(pub Vec<(String, Vec<String>)>);

/// A parsed option value.
#[derive(Debug, Clone, PartialEq)]
pub enum OptionValue {
    /// Boolean.
    Bool(bool),
    /// Number.
    Number(f64),
    /// String or path (paths are absolute and normalised once loaded).
    String(String),
    /// String/path/lib list.
    List(Vec<String>),
    /// `target`.
    Target(ScriptTarget),
    /// `module`.
    Module(ModuleKind),
    /// `moduleResolution`.
    ModuleResolution(ModuleResolution),
    /// `moduleDetection`.
    ModuleDetection(ModuleDetection),
    /// `jsx`.
    Jsx(JsxEmit),
    /// `newLine`.
    NewLine(NewLine),
    /// `paths`.
    Paths(PathsMap),
    /// `plugins` (each entry kept as JSON).
    Plugins(Vec<jsonc::Value>),
}

/// Where an option value was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// Config file path.
    pub file: Arc<str>,
    /// Span of the property name.
    pub key_span: (u32, u32),
    /// Span of the value.
    pub value_span: (u32, u32),
}

macro_rules! accessor {
    ($acc:ident, $id:ident, Bool) => {
        #[doc = concat!("Explicit `", stringify!($acc), "` value.")]
        pub fn $acc(&self) -> Option<bool> {
            match self.get(OptionId::$id) {
                Some(OptionValue::Bool(b)) => Some(*b),
                _ => None,
            }
        }
    };
    ($acc:ident, $id:ident, Number) => {
        #[doc = concat!("Explicit `", stringify!($acc), "` value.")]
        pub fn $acc(&self) -> Option<f64> {
            match self.get(OptionId::$id) {
                Some(OptionValue::Number(n)) => Some(*n),
                _ => None,
            }
        }
    };
    ($acc:ident, $id:ident, String) => {
        #[doc = concat!("Explicit `", stringify!($acc), "` value.")]
        pub fn $acc(&self) -> Option<&str> {
            match self.get(OptionId::$id) {
                Some(OptionValue::String(s)) => Some(s),
                _ => None,
            }
        }
    };
    ($acc:ident, $id:ident, Path) => {
        #[doc = concat!("Explicit `", stringify!($acc), "` value (absolute path).")]
        pub fn $acc(&self) -> Option<&str> {
            match self.get(OptionId::$id) {
                Some(OptionValue::String(s)) => Some(s),
                _ => None,
            }
        }
    };
    ($acc:ident, $id:ident, StringList) => {
        #[doc = concat!("Explicit `", stringify!($acc), "` value.")]
        pub fn $acc(&self) -> Option<&[String]> {
            match self.get(OptionId::$id) {
                Some(OptionValue::List(v)) => Some(v),
                _ => None,
            }
        }
    };
    ($acc:ident, $id:ident, PathList) => {
        #[doc = concat!("Explicit `", stringify!($acc), "` value (absolute paths).")]
        pub fn $acc(&self) -> Option<&[String]> {
            match self.get(OptionId::$id) {
                Some(OptionValue::List(v)) => Some(v),
                _ => None,
            }
        }
    };
    ($acc:ident, $id:ident, Lib) => {
        #[doc = concat!("Explicit `", stringify!($acc), "` value (lower-case names).")]
        pub fn $acc(&self) -> Option<&[String]> {
            match self.get(OptionId::$id) {
                Some(OptionValue::List(v)) => Some(v),
                _ => None,
            }
        }
    };
    ($acc:ident, $id:ident, Target) => {
        #[doc = concat!("Explicit `", stringify!($acc), "` value.")]
        pub fn $acc(&self) -> Option<ScriptTarget> {
            match self.get(OptionId::$id) {
                Some(OptionValue::Target(v)) => Some(*v),
                _ => None,
            }
        }
    };
    ($acc:ident, $id:ident, Module) => {
        #[doc = concat!("Explicit `", stringify!($acc), "` value.")]
        pub fn $acc(&self) -> Option<ModuleKind> {
            match self.get(OptionId::$id) {
                Some(OptionValue::Module(v)) => Some(*v),
                _ => None,
            }
        }
    };
    ($acc:ident, $id:ident, ModuleResolution) => {
        #[doc = concat!("Explicit `", stringify!($acc), "` value.")]
        pub fn $acc(&self) -> Option<ModuleResolution> {
            match self.get(OptionId::$id) {
                Some(OptionValue::ModuleResolution(v)) => Some(*v),
                _ => None,
            }
        }
    };
    ($acc:ident, $id:ident, ModuleDetection) => {
        #[doc = concat!("Explicit `", stringify!($acc), "` value.")]
        pub fn $acc(&self) -> Option<ModuleDetection> {
            match self.get(OptionId::$id) {
                Some(OptionValue::ModuleDetection(v)) => Some(*v),
                _ => None,
            }
        }
    };
    ($acc:ident, $id:ident, Jsx) => {
        #[doc = concat!("Explicit `", stringify!($acc), "` value.")]
        pub fn $acc(&self) -> Option<JsxEmit> {
            match self.get(OptionId::$id) {
                Some(OptionValue::Jsx(v)) => Some(*v),
                _ => None,
            }
        }
    };
    ($acc:ident, $id:ident, NewLine) => {
        #[doc = concat!("Explicit `", stringify!($acc), "` value.")]
        pub fn $acc(&self) -> Option<NewLine> {
            match self.get(OptionId::$id) {
                Some(OptionValue::NewLine(v)) => Some(*v),
                _ => None,
            }
        }
    };
    ($acc:ident, $id:ident, PathMap) => {
        #[doc = concat!("Explicit `", stringify!($acc), "` value.")]
        pub fn $acc(&self) -> Option<&PathsMap> {
            match self.get(OptionId::$id) {
                Some(OptionValue::Paths(v)) => Some(v),
                _ => None,
            }
        }
    };
    ($acc:ident, $id:ident, Plugins) => {
        #[doc = concat!("Explicit `", stringify!($acc), "` value.")]
        pub fn $acc(&self) -> Option<&[jsonc::Value]> {
            match self.get(OptionId::$id) {
                Some(OptionValue::Plugins(v)) => Some(v),
                _ => None,
            }
        }
    };
}

macro_rules! options {
    ($( $id:ident, $acc:ident, $name:literal, $ty:ident, $flags:expr, $affects:expr; )*) => {
        /// Identifies a compiler option. Discriminants follow `--showConfig` order.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[allow(missing_docs)]
        pub enum OptionId { $($id,)* }

        /// Every option, in `--showConfig` order.
        pub static OPTIONS: &[OptionDecl] = &[
            $( OptionDecl {
                id: OptionId::$id, name: $name, ty: OptionType::$ty, flags: $flags, affects: $affects,
            }, )*
        ];

        /// Raw (explicitly set) option values. Use [`CompilerOptions::effective`] for values
        /// with tsc's defaults applied.
        impl CompilerOptions {
            $( accessor!($acc, $id, $ty); )*
        }
    };
}

const H: u8 = flags::HIDDEN;
const C: u8 = flags::CMDLINE_ONLY | flags::HIDDEN;
// `affects` columns.
const CK: u8 = affects::CHECK;
const EM: u8 = affects::EMIT;
const DE: u8 = affects::DECL_EMIT;
const OP: u8 = affects::OUTPUT_PATHS;
const BI: u8 = affects::BUILDINFO;

// Columns: id, accessor, tsconfig name, value type, `flags`, `affects` (CK check, EM emit, DE
// declaration emit, OP output paths, BI recorded in the buildinfo).
options! {
    AllowJs, allow_js, "allowJs", Bool, 0, CK | EM | DE | OP;
    AllowArbitraryExtensions, allow_arbitrary_extensions, "allowArbitraryExtensions", Bool, 0, CK;
    AllowImportingTsExtensions, allow_importing_ts_extensions, "allowImportingTsExtensions", Bool, 0, CK | BI;
    AllowUmdGlobalAccess, allow_umd_global_access, "allowUmdGlobalAccess", Bool, 0, CK | BI;
    AllowUnreachableCode, allow_unreachable_code, "allowUnreachableCode", Bool, 0, CK | BI;
    AllowUnusedLabels, allow_unused_labels, "allowUnusedLabels", Bool, 0, CK | BI;
    AssumeChangesOnlyAffectDirectDependencies, assume_changes_only_affect_direct_dependencies, "assumeChangesOnlyAffectDirectDependencies", Bool, 0, BI;
    CheckJs, check_js, "checkJs", Bool, 0, CK | EM | DE | OP | BI;
    CustomConditions, custom_conditions, "customConditions", StringList, 0, CK;
    Composite, composite, "composite", Bool, 0, CK | DE | OP;
    EmitDeclarationOnly, emit_declaration_only, "emitDeclarationOnly", Bool, 0, EM | OP;
    EmitBom, emit_bom, "emitBOM", Bool, 0, EM | DE | BI;
    EmitDecoratorMetadata, emit_decorator_metadata, "emitDecoratorMetadata", Bool, 0, EM | BI;
    Declaration, declaration, "declaration", Bool, 0, CK | DE | OP | BI;
    DeclarationDir, declaration_dir, "declarationDir", Path, 0, CK | DE | OP | BI;
    DeclarationMap, declaration_map, "declarationMap", Bool, 0, DE | OP | BI;
    DeduplicatePackages, deduplicate_packages, "deduplicatePackages", Bool, 0, CK;
    DisableSizeLimit, disable_size_limit, "disableSizeLimit", Bool, 0, 0;
    DisableSourceOfProjectReferenceRedirect, disable_source_of_project_reference_redirect, "disableSourceOfProjectReferenceRedirect", Bool, 0, 0;
    DisableSolutionSearching, disable_solution_searching, "disableSolutionSearching", Bool, 0, 0;
    DisableReferencedProjectLoad, disable_referenced_project_load, "disableReferencedProjectLoad", Bool, 0, 0;
    ErasableSyntaxOnly, erasable_syntax_only, "erasableSyntaxOnly", Bool, 0, CK | BI;
    ExactOptionalPropertyTypes, exact_optional_property_types, "exactOptionalPropertyTypes", Bool, 0, CK | BI;
    ExperimentalDecorators, experimental_decorators, "experimentalDecorators", Bool, 0, CK | EM | BI;
    ForceConsistentCasingInFileNames, force_consistent_casing_in_file_names, "forceConsistentCasingInFileNames", Bool, 0, CK;
    IsolatedModules, isolated_modules, "isolatedModules", Bool, 0, CK | EM;
    IsolatedDeclarations, isolated_declarations, "isolatedDeclarations", Bool, 0, CK | DE | BI;
    IgnoreDeprecations, ignore_deprecations, "ignoreDeprecations", String, 0, CK;
    ImportHelpers, import_helpers, "importHelpers", Bool, 0, CK | EM | BI;
    InlineSourceMap, inline_source_map, "inlineSourceMap", Bool, 0, EM | OP | BI;
    InlineSources, inline_sources, "inlineSources", Bool, 0, EM | BI;
    Incremental, incremental, "incremental", Bool, 0, OP;
    Jsx, jsx, "jsx", Jsx, 0, CK | EM | OP | BI;
    JsxFactory, jsx_factory, "jsxFactory", String, 0, CK | EM;
    JsxFragmentFactory, jsx_fragment_factory, "jsxFragmentFactory", String, 0, CK | EM;
    JsxImportSource, jsx_import_source, "jsxImportSource", String, 0, CK | EM | BI;
    Lib, lib, "lib", Lib, 0, CK;
    LibReplacement, lib_replacement, "libReplacement", Bool, 0, CK;
    MapRoot, map_root, "mapRoot", String, 0, EM | DE | BI;
    Module, module, "module", Module, 0, CK | EM | BI;
    ModuleResolution, module_resolution, "moduleResolution", ModuleResolution, 0, CK | OP;
    ModuleSuffixes, module_suffixes, "moduleSuffixes", StringList, 0, CK | OP;
    ModuleDetection, module_detection, "moduleDetection", ModuleDetection, 0, CK | EM | DE;
    NewLine, new_line, "newLine", NewLine, 0, EM | DE | BI;
    NoEmit, no_emit, "noEmit", Bool, 0, OP;
    NoCheck, no_check, "noCheck", Bool, 0, CK;
    NoFallthroughCasesInSwitch, no_fallthrough_cases_in_switch, "noFallthroughCasesInSwitch", Bool, 0, CK | BI;
    NoImplicitAny, no_implicit_any, "noImplicitAny", Bool, 0, CK | BI;
    NoImplicitThis, no_implicit_this, "noImplicitThis", Bool, 0, CK | BI;
    NoImplicitReturns, no_implicit_returns, "noImplicitReturns", Bool, 0, CK | BI;
    NoEmitHelpers, no_emit_helpers, "noEmitHelpers", Bool, 0, EM | BI;
    NoLib, no_lib, "noLib", Bool, 0, CK;
    NoPropertyAccessFromIndexSignature, no_property_access_from_index_signature, "noPropertyAccessFromIndexSignature", Bool, 0, CK | BI;
    NoUncheckedIndexedAccess, no_unchecked_indexed_access, "noUncheckedIndexedAccess", Bool, 0, CK | BI;
    NoEmitOnError, no_emit_on_error, "noEmitOnError", Bool, 0, OP | BI;
    NoUnusedLocals, no_unused_locals, "noUnusedLocals", Bool, 0, CK | BI;
    NoUnusedParameters, no_unused_parameters, "noUnusedParameters", Bool, 0, CK | BI;
    NoResolve, no_resolve, "noResolve", Bool, 0, CK | OP;
    NoImplicitOverride, no_implicit_override, "noImplicitOverride", Bool, 0, CK | BI;
    NoUncheckedSideEffectImports, no_unchecked_side_effect_imports, "noUncheckedSideEffectImports", Bool, 0, CK | BI;
    OutDir, out_dir, "outDir", Path, 0, CK | EM | DE | OP | BI;
    Paths, paths, "paths", PathMap, 0, CK;
    PreserveConstEnums, preserve_const_enums, "preserveConstEnums", Bool, 0, EM | BI;
    PreserveSymlinks, preserve_symlinks, "preserveSymlinks", Bool, 0, CK;
    ResolveJsonModule, resolve_json_module, "resolveJsonModule", Bool, 0, CK | OP;
    ResolvePackageJsonExports, resolve_package_json_exports, "resolvePackageJsonExports", Bool, 0, CK;
    ResolvePackageJsonImports, resolve_package_json_imports, "resolvePackageJsonImports", Bool, 0, CK;
    RemoveComments, remove_comments, "removeComments", Bool, 0, EM | DE | BI;
    RewriteRelativeImportExtensions, rewrite_relative_import_extensions, "rewriteRelativeImportExtensions", Bool, 0, CK | EM | BI;
    ReactNamespace, react_namespace, "reactNamespace", String, 0, CK | EM | BI;
    RootDir, root_dir, "rootDir", Path, 0, CK | EM | DE | OP | BI;
    RootDirs, root_dirs, "rootDirs", PathList, 0, CK;
    SkipLibCheck, skip_lib_check, "skipLibCheck", Bool, 0, CK | BI;
    StableTypeOrdering, stable_type_ordering, "stableTypeOrdering", Bool, 0, CK | BI;
    Strict, strict, "strict", Bool, 0, CK | BI;
    StrictBindCallApply, strict_bind_call_apply, "strictBindCallApply", Bool, 0, CK | BI;
    StrictBuiltinIteratorReturn, strict_builtin_iterator_return, "strictBuiltinIteratorReturn", Bool, 0, CK | BI;
    StrictFunctionTypes, strict_function_types, "strictFunctionTypes", Bool, 0, CK | BI;
    StrictNullChecks, strict_null_checks, "strictNullChecks", Bool, 0, CK | BI;
    StrictPropertyInitialization, strict_property_initialization, "strictPropertyInitialization", Bool, 0, CK | BI;
    StripInternal, strip_internal, "stripInternal", Bool, 0, DE | BI;
    SkipDefaultLibCheck, skip_default_lib_check, "skipDefaultLibCheck", Bool, 0, CK | BI;
    SourceMap, source_map, "sourceMap", Bool, 0, EM | OP | BI;
    SourceRoot, source_root, "sourceRoot", String, 0, EM | DE | BI;
    Target, target, "target", Target, 0, CK | EM | BI;
    TraceResolution, trace_resolution, "traceResolution", Bool, 0, 0;
    TsBuildInfoFile, ts_build_info_file, "tsBuildInfoFile", Path, 0, OP | BI;
    TypeRoots, type_roots, "typeRoots", PathList, 0, CK;
    Types, types, "types", StringList, 0, CK;
    UseDefineForClassFields, use_define_for_class_fields, "useDefineForClassFields", Bool, 0, CK | EM | BI;
    UseUnknownInCatchVariables, use_unknown_in_catch_variables, "useUnknownInCatchVariables", Bool, 0, CK | BI;
    VerbatimModuleSyntax, verbatim_module_syntax, "verbatimModuleSyntax", Bool, 0, CK | EM | BI;
    MaxNodeModuleJsDepth, max_node_module_js_depth, "maxNodeModuleJsDepth", Number, 0, CK;
    AllowSyntheticDefaultImports, allow_synthetic_default_imports, "allowSyntheticDefaultImports", Bool, 0, CK | BI;
    AlwaysStrict, always_strict, "alwaysStrict", Bool, 0, CK | EM | BI;
    BaseUrl, base_url, "baseUrl", Path, 0, CK;
    DownlevelIteration, downlevel_iteration, "downlevelIteration", Bool, 0, EM | BI;
    EsModuleInterop, es_module_interop, "esModuleInterop", Bool, 0, CK | EM | BI;
    OutFile, out_file, "outFile", Path, 0, EM | DE | OP | BI;
    Diagnostics, diagnostics, "diagnostics", Bool, 0, 0;
    ExtendedDiagnostics, extended_diagnostics, "extendedDiagnostics", Bool, 0, 0;
    GenerateCpuProfile, generate_cpu_profile, "generateCpuProfile", Path, 0, 0;
    GenerateTrace, generate_trace, "generateTrace", Path, 0, 0;
    ListFiles, list_files, "listFiles", Bool, 0, 0;
    ExplainFiles, explain_files, "explainFiles", Bool, 0, 0;
    // Accepted in tsconfig but never printed by --showConfig.
    ListEmittedFiles, list_emitted_files, "listEmittedFiles", Bool, H, 0;
    Pretty, pretty, "pretty", Bool, H, 0;
    NoErrorTruncation, no_error_truncation, "noErrorTruncation", Bool, H, CK | BI;
    PreserveWatchOutput, preserve_watch_output, "preserveWatchOutput", Bool, H, 0;
    Plugins, plugins, "plugins", Plugins, H, 0;
    Checkers, checkers, "checkers", Number, H, 0;
    SingleThreaded, single_threaded, "singleThreaded", Bool, H, 0;
    Quiet, quiet, "quiet", Bool, H, 0;
    PprofDir, pprof_dir, "pprofDir", Path, H, 0;
    Version, version, "version", Bool, H, 0;
    Init, init, "init", Bool, H, 0;
    All, all, "all", Bool, H, 0;
    Project, project, "project", String, H, 0;
    // Command-line only: TS6266 when written in a tsconfig.
    Watch, watch, "watch", Bool, C, 0;
    Locale, locale, "locale", String, C, 0;
    Help, help, "help", Bool, C, 0;
    ShowConfig, show_config, "showConfig", Bool, C, 0;
    ListFilesOnly, list_files_only, "listFilesOnly", Bool, C, 0;
    IgnoreConfig, ignore_config, "ignoreConfig", Bool, C, 0;
}

/// Options removed before or in TS 7 that tsc 7 now reports as unknown (TS5023).
/// Kept for documentation and migration tooling.
pub const REMOVED_UNKNOWN_OPTIONS: &[&str] = &[
    "out",
    "charset",
    "keyofStringsOnly",
    "noImplicitUseStrict",
    "noStrictGenericChecks",
    "suppressExcessPropertyErrors",
    "suppressImplicitAnyIndexErrors",
    "importsNotUsedAsValues",
    "preserveValueImports",
    "suppressOutputPathCheck",
    "prepend",
];

impl OptionId {
    /// The declaration of this option.
    pub fn decl(self) -> &'static OptionDecl {
        &OPTIONS[self as usize]
    }

    /// The tsconfig name.
    pub fn name(self) -> &'static str {
        self.decl().name
    }
}

/// Finds an option by its exact (case-sensitive) name.
pub fn find_option(name: &str) -> Option<&'static OptionDecl> {
    OPTIONS.iter().find(|d| d.name == name)
}

/// Finds an option whose name matches case-insensitively (for "Did you mean" suggestions,
/// which tsc 7 only offers for case differences).
pub fn find_option_ignore_case(name: &str) -> Option<&'static OptionDecl> {
    OPTIONS
        .iter()
        .find(|d| d.name.eq_ignore_ascii_case(name) && d.flags & flags::CMDLINE_ONLY == 0)
}

/// Compiler options as written in a config chain, after `extends` merging.
#[derive(Debug, Clone, Default)]
pub struct CompilerOptions {
    values: Vec<Option<OptionValue>>,
    origins: Vec<Option<Origin>>,
    /// The tsconfig these options came from.
    pub config_file_path: Option<String>,
    /// Directory `paths` substitutions are relative to (the directory of the config that
    /// set `paths`).
    pub paths_base_path: Option<String>,
}

impl PartialEq for CompilerOptions {
    fn eq(&self, other: &Self) -> bool {
        OPTIONS.iter().all(|d| self.get(d.id) == other.get(d.id))
            && self.paths_base_path == other.paths_base_path
    }
}

impl CompilerOptions {
    /// Empty options (every value at its default).
    pub fn new() -> Self {
        Self::default()
    }

    /// The explicit value of `id`, if set.
    pub fn get(&self, id: OptionId) -> Option<&OptionValue> {
        self.values.get(id as usize).and_then(|v| v.as_ref())
    }

    /// Where `id` was set, if known.
    pub fn origin(&self, id: OptionId) -> Option<&Origin> {
        self.origins.get(id as usize).and_then(|v| v.as_ref())
    }

    /// True when `id` was explicitly set.
    pub fn is_set(&self, id: OptionId) -> bool {
        self.get(id).is_some()
    }

    /// Sets (or with `None`, clears) a value.
    pub fn set(&mut self, id: OptionId, value: Option<OptionValue>, origin: Option<Origin>) {
        let i = id as usize;
        if self.values.len() <= i {
            self.values.resize(OPTIONS.len(), None);
            self.origins.resize(OPTIONS.len(), None);
        }
        self.values[i] = value;
        self.origins[i] = origin;
    }

    /// Iterates explicitly set options in `--showConfig` order.
    pub fn iter(&self) -> impl Iterator<Item = (OptionId, &OptionValue)> {
        OPTIONS
            .iter()
            .filter_map(|d| self.get(d.id).map(|v| (d.id, v)))
    }

    /// The view with tsc's computed defaults.
    pub fn effective(&self) -> Effective<'_> {
        Effective { o: self }
    }
}

/// Compiler options with tsc 7's defaults and option-dependent defaults applied.
#[derive(Debug, Clone, Copy)]
pub struct Effective<'a> {
    o: &'a CompilerOptions,
}

/// A member of the `strict` family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictFlag {
    /// `noImplicitAny`
    NoImplicitAny,
    /// `noImplicitThis`
    NoImplicitThis,
    /// `strictNullChecks`
    StrictNullChecks,
    /// `strictFunctionTypes`
    StrictFunctionTypes,
    /// `strictBindCallApply`
    StrictBindCallApply,
    /// `strictPropertyInitialization`
    StrictPropertyInitialization,
    /// `strictBuiltinIteratorReturn`
    StrictBuiltinIteratorReturn,
    /// `useUnknownInCatchVariables`
    UseUnknownInCatchVariables,
    /// `alwaysStrict` (always on in TS 7; `false` is removed).
    AlwaysStrict,
}

impl StrictFlag {
    fn id(self) -> OptionId {
        match self {
            Self::NoImplicitAny => OptionId::NoImplicitAny,
            Self::NoImplicitThis => OptionId::NoImplicitThis,
            Self::StrictNullChecks => OptionId::StrictNullChecks,
            Self::StrictFunctionTypes => OptionId::StrictFunctionTypes,
            Self::StrictBindCallApply => OptionId::StrictBindCallApply,
            Self::StrictPropertyInitialization => OptionId::StrictPropertyInitialization,
            Self::StrictBuiltinIteratorReturn => OptionId::StrictBuiltinIteratorReturn,
            Self::UseUnknownInCatchVariables => OptionId::UseUnknownInCatchVariables,
            Self::AlwaysStrict => OptionId::AlwaysStrict,
        }
    }
}

impl Effective<'_> {
    fn flag(&self, id: OptionId) -> Option<bool> {
        match self.o.get(id) {
            Some(OptionValue::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    /// `target`, default ES2025.
    pub fn target(&self) -> ScriptTarget {
        self.o.target().unwrap_or(ScriptTarget::ES2025)
    }

    /// `module`; defaults from `target`: CommonJS below ES2015, ES2015 up to ES2019, ES2020
    /// for ES2020/ES2021, ES2022 for ES2022–ES2025 and ESNext for ESNext.
    pub fn module(&self) -> ModuleKind {
        if let Some(m) = self.o.module() {
            return m;
        }
        match self.target() {
            ScriptTarget::ES5 => ModuleKind::CommonJS,
            ScriptTarget::ES2015
            | ScriptTarget::ES2016
            | ScriptTarget::ES2017
            | ScriptTarget::ES2018
            | ScriptTarget::ES2019 => ModuleKind::ES2015,
            ScriptTarget::ES2020 | ScriptTarget::ES2021 => ModuleKind::ES2020,
            ScriptTarget::ES2022
            | ScriptTarget::ES2023
            | ScriptTarget::ES2024
            | ScriptTarget::ES2025 => ModuleKind::ES2022,
            ScriptTarget::ESNext => ModuleKind::ESNext,
        }
    }

    /// `moduleResolution` as written, or derived from `module`: `nodenext` for
    /// `module: nodenext`, `node16` for `node16`/`node18`/`node20`, otherwise `bundler`.
    /// Removed values (`node10`, `classic`) are returned as written; see
    /// [`Self::resolution_algorithm`] for what the resolver actually runs.
    pub fn module_resolution(&self) -> ModuleResolution {
        if let Some(r) = self.o.module_resolution() {
            return r;
        }
        match self.module() {
            ModuleKind::NodeNext => ModuleResolution::NodeNext,
            ModuleKind::Node16 | ModuleKind::Node18 | ModuleKind::Node20 => {
                ModuleResolution::Node16
            }
            _ => ModuleResolution::Bundler,
        }
    }

    /// The resolution algorithm the resolver runs: removed values fall back to `bundler`.
    pub fn resolution_algorithm(&self) -> ModuleResolution {
        match self.module_resolution() {
            ModuleResolution::Node10 | ModuleResolution::Classic => ModuleResolution::Bundler,
            r => r,
        }
    }

    /// `moduleDetection`: `force` for node16–nodenext modules, otherwise `auto`.
    pub fn module_detection(&self) -> ModuleDetection {
        if let Some(d) = self.o.module_detection() {
            return d;
        }
        if self.module().is_node() {
            ModuleDetection::Force
        } else {
            ModuleDetection::Auto
        }
    }

    /// `resolveJsonModule`: on for `module` node20/nodenext, and whenever resolving with
    /// `bundler` (observed with tsc 7.0.2: `module: node16` or `node18` with
    /// `moduleResolution: bundler` looks for JSON too).
    pub fn resolve_json_module(&self) -> bool {
        if let Some(b) = self.flag(OptionId::ResolveJsonModule) {
            return b;
        }
        matches!(self.module(), ModuleKind::Node20 | ModuleKind::NodeNext)
            || self.resolution_algorithm() == ModuleResolution::Bundler
    }

    /// `resolvePackageJsonExports` (on for every TS 7 resolution mode).
    pub fn resolve_package_json_exports(&self) -> bool {
        self.flag(OptionId::ResolvePackageJsonExports)
            .unwrap_or(true)
    }

    /// `resolvePackageJsonImports` (on for every TS 7 resolution mode).
    pub fn resolve_package_json_imports(&self) -> bool {
        self.flag(OptionId::ResolvePackageJsonImports)
            .unwrap_or(true)
    }

    /// `isolatedModules`, implied by `verbatimModuleSyntax`.
    pub fn isolated_modules(&self) -> bool {
        self.flag(OptionId::IsolatedModules)
            .unwrap_or_else(|| self.flag(OptionId::VerbatimModuleSyntax).unwrap_or(false))
    }

    /// `preserveConstEnums`, implied by `isolatedModules`.
    pub fn preserve_const_enums(&self) -> bool {
        self.flag(OptionId::PreserveConstEnums)
            .unwrap_or_else(|| self.isolated_modules())
    }

    /// Declarations are emitted: `declaration` or `composite`. An explicit
    /// `"declaration": false` does not turn them off in a composite project (observed with
    /// tsc 7: TS6304 is reported and the `.d.ts` files are still written).
    pub fn declaration(&self) -> bool {
        self.flag(OptionId::Declaration) == Some(true)
            || self.flag(OptionId::Composite) == Some(true)
    }

    /// `incremental`, implied by `composite`.
    pub fn incremental(&self) -> bool {
        self.flag(OptionId::Incremental)
            .unwrap_or_else(|| self.flag(OptionId::Composite).unwrap_or(false))
    }

    /// `useDefineForClassFields`: on for ES2022 and later.
    pub fn use_define_for_class_fields(&self) -> bool {
        self.flag(OptionId::UseDefineForClassFields)
            .unwrap_or_else(|| self.target() >= ScriptTarget::ES2022)
    }

    /// `allowJs`, implied by `checkJs`.
    pub fn allow_js(&self) -> bool {
        self.flag(OptionId::AllowJs)
            .unwrap_or_else(|| self.flag(OptionId::CheckJs).unwrap_or(false))
    }

    /// `allowImportingTsExtensions`, or `rewriteRelativeImportExtensions`: tsc ORs the two,
    /// so the rewrite option allows `.ts` imports even when `allowImportingTsExtensions` is
    /// explicitly `false` (`tsc/internal/core/compileroptions.go`).
    pub fn allow_importing_ts_extensions(&self) -> bool {
        self.flag(OptionId::AllowImportingTsExtensions) == Some(true)
            || self.flag(OptionId::RewriteRelativeImportExtensions) == Some(true)
    }

    /// `strict`, default `true` in TS 7.
    pub fn strict(&self) -> bool {
        self.flag(OptionId::Strict).unwrap_or(true)
    }

    /// A member of the strict family: explicit value, else `strict`.
    pub fn strict_flag(&self, f: StrictFlag) -> bool {
        if f == StrictFlag::AlwaysStrict {
            return self.flag(OptionId::AlwaysStrict).unwrap_or(true);
        }
        self.flag(f.id()).unwrap_or_else(|| self.strict())
    }

    /// `esModuleInterop`, default `true` (`false` is removed in TS 7).
    pub fn es_module_interop(&self) -> bool {
        self.flag(OptionId::EsModuleInterop).unwrap_or(true)
    }

    /// `allowSyntheticDefaultImports`, default `true`.
    pub fn allow_synthetic_default_imports(&self) -> bool {
        self.flag(OptionId::AllowSyntheticDefaultImports)
            .unwrap_or(true)
    }

    /// `noUncheckedSideEffectImports`, default `true` in TS 7.
    pub fn no_unchecked_side_effect_imports(&self) -> bool {
        self.flag(OptionId::NoUncheckedSideEffectImports)
            .unwrap_or(true)
    }

    /// `forceConsistentCasingInFileNames`, default `true`.
    pub fn force_consistent_casing_in_file_names(&self) -> bool {
        self.flag(OptionId::ForceConsistentCasingInFileNames)
            .unwrap_or(true)
    }

    /// `stableTypeOrdering`, default `true`.
    pub fn stable_type_ordering(&self) -> bool {
        self.flag(OptionId::StableTypeOrdering).unwrap_or(true)
    }

    /// `deduplicatePackages`, default `true`.
    pub fn deduplicate_packages(&self) -> bool {
        self.flag(OptionId::DeduplicatePackages).unwrap_or(true)
    }

    /// `preserveSymlinks`, default `false`.
    pub fn preserve_symlinks(&self) -> bool {
        self.flag(OptionId::PreserveSymlinks).unwrap_or(false)
    }

    /// `allowArbitraryExtensions`, default `false`.
    pub fn allow_arbitrary_extensions(&self) -> bool {
        self.flag(OptionId::AllowArbitraryExtensions)
            .unwrap_or(false)
    }

    /// `types`: TS 7 defaults to `[]` (no automatic `@types` inclusion).
    pub fn types(&self) -> &[String] {
        self.o.types().unwrap_or(&[])
    }

    /// `customConditions`, default empty.
    pub fn custom_conditions(&self) -> &[String] {
        self.o.custom_conditions().unwrap_or(&[])
    }

    /// `moduleSuffixes`, default `[""]`.
    pub fn module_suffixes(&self) -> Vec<String> {
        match self.o.module_suffixes() {
            Some(v) if !v.is_empty() => v.to_vec(),
            _ => vec![String::new()],
        }
    }

    /// `jsx`, `None` when unset.
    pub fn jsx(&self) -> Option<JsxEmit> {
        self.o.jsx()
    }

    /// `jsxImportSource`, default `react`.
    pub fn jsx_import_source(&self) -> &str {
        self.o.jsx_import_source().unwrap_or("react")
    }

    /// `newLine`, default LF.
    pub fn new_line(&self) -> NewLine {
        self.o.new_line().unwrap_or(NewLine::Lf)
    }

    /// `noEmit`, default `false`.
    pub fn no_emit(&self) -> bool {
        self.flag(OptionId::NoEmit).unwrap_or(false)
    }

    /// `maxNodeModuleJsDepth`, default 0.
    pub fn max_node_module_js_depth(&self) -> u32 {
        self.o
            .max_node_module_js_depth()
            .map(|n| n.max(0.0) as u32)
            .unwrap_or(0)
    }

    /// Library file names to load: `lib` mapped to files, or the target's default library.
    /// Empty with `noLib`.
    pub fn lib_files(&self) -> Vec<&'static str> {
        if self.flag(OptionId::NoLib).unwrap_or(false) {
            return Vec::new();
        }
        match self.o.lib() {
            Some(libs) => libs.iter().filter_map(|l| lib_file_name(l)).collect(),
            None => vec![self.target().default_lib_file_name()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(pairs: &[(OptionId, OptionValue)]) -> CompilerOptions {
        let mut o = CompilerOptions::new();
        for (id, v) in pairs {
            o.set(*id, Some(v.clone()), None);
        }
        o
    }

    #[test]
    fn table_is_consistent() {
        for (i, d) in OPTIONS.iter().enumerate() {
            assert_eq!(d.id as usize, i, "{}", d.name);
        }
        assert_eq!(
            find_option("allowJs").map(|d| d.id),
            Some(OptionId::AllowJs)
        );
        assert_eq!(
            find_option_ignore_case("SOURCEMAP").map(|d| d.name),
            Some("sourceMap")
        );
        assert!(find_option_ignore_case("watch").is_none());
    }

    #[test]
    fn rewrite_extensions_allows_ts_imports_even_when_disallowed() {
        let a = |pairs: &[(OptionId, OptionValue)]| {
            with(pairs).effective().allow_importing_ts_extensions()
        };
        let allow = OptionId::AllowImportingTsExtensions;
        let rewrite = OptionId::RewriteRelativeImportExtensions;
        assert!(!a(&[]));
        assert!(a(&[(allow, OptionValue::Bool(true))]));
        assert!(a(&[(rewrite, OptionValue::Bool(true))]));
        assert!(a(&[
            (allow, OptionValue::Bool(false)),
            (rewrite, OptionValue::Bool(true))
        ]));
        assert!(!a(&[
            (allow, OptionValue::Bool(false)),
            (rewrite, OptionValue::Bool(false))
        ]));
    }

    #[test]
    fn module_defaults_follow_target() {
        let e = |t| {
            with(&[(OptionId::Target, OptionValue::Target(t))])
                .effective()
                .module()
        };
        assert_eq!(e(ScriptTarget::ES5), ModuleKind::CommonJS);
        assert_eq!(e(ScriptTarget::ES2017), ModuleKind::ES2015);
        assert_eq!(e(ScriptTarget::ES2021), ModuleKind::ES2020);
        assert_eq!(e(ScriptTarget::ES2024), ModuleKind::ES2022);
        assert_eq!(e(ScriptTarget::ESNext), ModuleKind::ESNext);
        assert_eq!(
            CompilerOptions::new().effective().module(),
            ModuleKind::ES2022
        );
    }

    #[test]
    fn resolution_defaults_follow_module() {
        let r = |m| {
            with(&[(OptionId::Module, OptionValue::Module(m))])
                .effective()
                .module_resolution()
        };
        assert_eq!(r(ModuleKind::NodeNext), ModuleResolution::NodeNext);
        assert_eq!(r(ModuleKind::Node20), ModuleResolution::Node16);
        assert_eq!(r(ModuleKind::CommonJS), ModuleResolution::Bundler);
        assert_eq!(r(ModuleKind::Preserve), ModuleResolution::Bundler);
    }

    #[test]
    fn json_module_defaults() {
        let m = |m| {
            with(&[(OptionId::Module, OptionValue::Module(m))])
                .effective()
                .resolve_json_module()
        };
        assert!(!m(ModuleKind::Node16));
        assert!(!m(ModuleKind::Node18));
        assert!(m(ModuleKind::Node20));
        assert!(m(ModuleKind::NodeNext));
        assert!(m(ModuleKind::ESNext));
        let o = with(&[
            (OptionId::Module, OptionValue::Module(ModuleKind::ESNext)),
            (
                OptionId::ModuleResolution,
                OptionValue::ModuleResolution(ModuleResolution::NodeNext),
            ),
        ]);
        assert!(!o.effective().resolve_json_module());
    }

    #[test]
    fn strict_family() {
        let o = with(&[(OptionId::Strict, OptionValue::Bool(false))]);
        assert!(!o.effective().strict_flag(StrictFlag::NoImplicitAny));
        assert!(o.effective().strict_flag(StrictFlag::AlwaysStrict));
        assert!(
            CompilerOptions::new()
                .effective()
                .strict_flag(StrictFlag::StrictNullChecks)
        );
    }

    #[test]
    fn default_lib() {
        assert_eq!(
            CompilerOptions::new().effective().lib_files(),
            ["lib.es2025.full.d.ts"]
        );
        let o = with(&[(
            OptionId::Lib,
            OptionValue::List(vec!["es2020".into(), "dom".into()]),
        )]);
        assert_eq!(
            o.effective().lib_files(),
            ["lib.es2020.d.ts", "lib.dom.d.ts"]
        );
    }
}
