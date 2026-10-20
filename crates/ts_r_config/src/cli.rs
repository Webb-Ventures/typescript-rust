//! tsc 7.0.2's command line: every flag `tsc --all` lists ([`FLAGS`], in its order and
//! sections, with its aliases) and the parser ([`parse_command_line`]).
//!
//! Observed behaviour (tsc 7.0.2, probes run under `scripts/limit.sh`; design record 0012):
//!
//! - A flag is `-name` or `--name`, or `-alias` / `--alias`, matched case-insensitively.
//!   `--flag=value` is not a form: it is an unknown option (TS5023). A bare `-` or `--` is
//!   unknown too. Anything else is a file name (projects under `--build`).
//! - Booleans take an optional following `true` or `false` (exact spelling); any other
//!   word is the next argument. Strings and paths always take the next argument, even one
//!   starting with `-`. Numbers take the next argument and must be integers (TS6044
//!   otherwise; TS5002 below the minimum). Enumerations take the next argument (TS6046 when
//!   it is not a value; at the end: TS6044 then TS6046). Lists take the next argument unless
//!   it starts with `-` or has no non-empty `,`-separated item; then the option is left
//!   unset and the argument is parsed on its own. A missing value at the end is TS6044.
//! - `null` as the value of any option clears it. `paths`, `plugins` and `rootDirs` accept
//!   only `null` (TS6064). The last occurrence of an option wins; lists are not merged.
//! - `--build` / `-b` must come first: a later `--build` is TS6369 and a later `-b` is
//!   unknown. In build mode `-v`, `-d` and `-f` mean `--verbose`, `--dry` and `--force`;
//!   compiler options not allowed there give TS5094, unknown flags TS5072, a bad `--builders`
//!   value TS5073. Outside build mode the build flags give TS5093.
//! - `@file` reads more arguments from a response file (TS5083 when it cannot be read).
//! - Diagnostics come in argument order; combination errors (TS6370) follow.

use crate::diagnostic::ConfigDiagnostic;
use crate::enums::{
    JsxEmit, LIBS, ModuleDetection, ModuleKind, ModuleResolution, NewLine, PollingKind,
    ScriptTarget, WatchDirectoryKind, WatchFileKind, lib_file_name,
};
use crate::fs::FileSystem;
use crate::options::{CompilerOptions, OptionId, OptionType, OptionValue};
use crate::path;
use crate::tsconfig::WatchOptions;

/// The tsc version whose command line (and output) ts-r matches.
pub const TSC_VERSION: &str = "7.0.2";

/// The section a flag is listed under by `tsc --all`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    CommandLine,
    Modules,
    JavaScriptSupport,
    InteropConstraints,
    TypeChecking,
    WatchAndBuildModes,
    Projects,
    Emit,
    CompilerDiagnostics,
    EditorSupport,
    LanguageAndEnvironment,
    OutputFormatting,
    Completeness,
    /// `WATCH OPTIONS`.
    Watch,
    /// `BUILD OPTIONS`.
    Build,
    /// Accepted but not listed by `tsc --all` (`--ignoreDeprecations`).
    Unlisted,
}

impl Section {
    /// The heading `tsc --all` prints.
    pub fn title(self) -> &'static str {
        match self {
            Section::CommandLine => "Command-line Options",
            Section::Modules => "Modules",
            Section::JavaScriptSupport => "JavaScript Support",
            Section::InteropConstraints => "Interop Constraints",
            Section::TypeChecking => "Type Checking",
            Section::WatchAndBuildModes => "Watch and Build Modes",
            Section::Projects => "Projects",
            Section::Emit => "Emit",
            Section::CompilerDiagnostics => "Compiler Diagnostics",
            Section::EditorSupport => "Editor Support",
            Section::LanguageAndEnvironment => "Language and Environment",
            Section::OutputFormatting => "Output Formatting",
            Section::Completeness => "Completeness",
            Section::Watch => "WATCH OPTIONS",
            Section::Build => "BUILD OPTIONS",
            Section::Unlisted => "Other",
        }
    }
}

/// A watch option (`watchOptions` in a tsconfig).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WatchFlag {
    WatchInterval,
    WatchFile,
    WatchDirectory,
    FallbackPolling,
    SynchronousWatchDirectory,
    ExcludeDirectories,
    ExcludeFiles,
}

/// A build-mode option.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuildFlag {
    Verbose,
    Dry,
    Force,
    Clean,
    Builders,
    StopBuildOnErrors,
}

/// What a flag sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FlagTarget {
    /// `--build` / `-b`: build mode (first argument only).
    BuildMode,
    /// A compiler option of [`crate::options::OPTIONS`] (command-line-only ones included).
    Compiler(OptionId),
    /// A watch option.
    Watch(WatchFlag),
    /// A build option.
    Build(BuildFlag),
}

/// One command-line flag.
#[derive(Clone, Copy, Debug)]
pub struct Flag {
    /// Name without dashes, as `tsc --all` spells it.
    pub name: &'static str,
    /// Short aliases without the dash (`p` for `-p`). Under `--build` the build flags'
    /// aliases (`v`, `d`, `f`) take precedence.
    pub aliases: &'static [&'static str],
    /// What it sets.
    pub target: FlagTarget,
    /// `tsc --all` section.
    pub section: Section,
    /// Accepted after `--build` (otherwise TS5094; build flags are accepted only there).
    pub in_build: bool,
}

impl Flag {
    /// The [`crate::options::affects`] classes of the option it sets (0 for watch and build
    /// flags, which change neither diagnostics nor outputs).
    pub fn affects(&self) -> u8 {
        match self.target {
            FlagTarget::Compiler(id) => id.decl().affects,
            _ => 0,
        }
    }
}

macro_rules! flags {
    ($( $name:literal [$($alias:literal),*] $target:expr, $section:ident, $build:literal; )*) => {
        /// Every flag tsc 7.0.2 accepts on its command line, in `tsc --all` order (sections
        /// in its order; `--help` once, with both aliases), then `--ignoreDeprecations`,
        /// which tsc accepts without listing it.
        pub static FLAGS: &[Flag] = &[
            $( Flag {
                name: $name,
                aliases: &[$($alias),*],
                target: $target,
                section: Section::$section,
                in_build: $build,
            }, )*
        ];
    };
}

use FlagTarget::{Build as B, BuildMode, Compiler as O, Watch as W};

flags! {
    "all" [] O(OptionId::All), CommandLine, false;
    "build" ["b"] BuildMode, CommandLine, true;
    "checkers" [] O(OptionId::Checkers), CommandLine, true;
    "help" ["h", "?"] O(OptionId::Help), CommandLine, true;
    "ignoreConfig" [] O(OptionId::IgnoreConfig), CommandLine, false;
    "init" [] O(OptionId::Init), CommandLine, false;
    "listFilesOnly" [] O(OptionId::ListFilesOnly), CommandLine, false;
    "locale" [] O(OptionId::Locale), CommandLine, true;
    "pprofDir" [] O(OptionId::PprofDir), CommandLine, true;
    "project" ["p"] O(OptionId::Project), CommandLine, false;
    "quiet" ["q"] O(OptionId::Quiet), CommandLine, true;
    "showConfig" [] O(OptionId::ShowConfig), CommandLine, false;
    "singleThreaded" [] O(OptionId::SingleThreaded), CommandLine, true;
    "version" ["v"] O(OptionId::Version), CommandLine, false;
    "watch" ["w"] O(OptionId::Watch), CommandLine, true;
    "allowArbitraryExtensions" [] O(OptionId::AllowArbitraryExtensions), Modules, false;
    "allowImportingTsExtensions" [] O(OptionId::AllowImportingTsExtensions), Modules, false;
    "allowUmdGlobalAccess" [] O(OptionId::AllowUmdGlobalAccess), Modules, false;
    "baseUrl" [] O(OptionId::BaseUrl), Modules, false;
    "customConditions" [] O(OptionId::CustomConditions), Modules, false;
    "module" ["m"] O(OptionId::Module), Modules, false;
    "moduleResolution" [] O(OptionId::ModuleResolution), Modules, false;
    "moduleSuffixes" [] O(OptionId::ModuleSuffixes), Modules, false;
    "noResolve" [] O(OptionId::NoResolve), Modules, false;
    "noUncheckedSideEffectImports" [] O(OptionId::NoUncheckedSideEffectImports), Modules, false;
    "paths" [] O(OptionId::Paths), Modules, false;
    "resolveJsonModule" [] O(OptionId::ResolveJsonModule), Modules, false;
    "resolvePackageJsonExports" [] O(OptionId::ResolvePackageJsonExports), Modules, false;
    "resolvePackageJsonImports" [] O(OptionId::ResolvePackageJsonImports), Modules, false;
    "rewriteRelativeImportExtensions" [] O(OptionId::RewriteRelativeImportExtensions), Modules, false;
    "rootDir" [] O(OptionId::RootDir), Modules, false;
    "rootDirs" [] O(OptionId::RootDirs), Modules, false;
    "typeRoots" [] O(OptionId::TypeRoots), Modules, false;
    "types" [] O(OptionId::Types), Modules, false;
    "allowJs" [] O(OptionId::AllowJs), JavaScriptSupport, false;
    "checkJs" [] O(OptionId::CheckJs), JavaScriptSupport, false;
    "maxNodeModuleJsDepth" [] O(OptionId::MaxNodeModuleJsDepth), JavaScriptSupport, false;
    "allowSyntheticDefaultImports" [] O(OptionId::AllowSyntheticDefaultImports), InteropConstraints, false;
    "erasableSyntaxOnly" [] O(OptionId::ErasableSyntaxOnly), InteropConstraints, false;
    "esModuleInterop" [] O(OptionId::EsModuleInterop), InteropConstraints, false;
    "forceConsistentCasingInFileNames" [] O(OptionId::ForceConsistentCasingInFileNames), InteropConstraints, false;
    "isolatedDeclarations" [] O(OptionId::IsolatedDeclarations), InteropConstraints, false;
    "isolatedModules" [] O(OptionId::IsolatedModules), InteropConstraints, false;
    "preserveSymlinks" [] O(OptionId::PreserveSymlinks), InteropConstraints, false;
    "verbatimModuleSyntax" [] O(OptionId::VerbatimModuleSyntax), InteropConstraints, false;
    "allowUnreachableCode" [] O(OptionId::AllowUnreachableCode), TypeChecking, false;
    "allowUnusedLabels" [] O(OptionId::AllowUnusedLabels), TypeChecking, false;
    "alwaysStrict" [] O(OptionId::AlwaysStrict), TypeChecking, false;
    "deduplicatePackages" [] O(OptionId::DeduplicatePackages), TypeChecking, true;
    "exactOptionalPropertyTypes" [] O(OptionId::ExactOptionalPropertyTypes), TypeChecking, false;
    "noFallthroughCasesInSwitch" [] O(OptionId::NoFallthroughCasesInSwitch), TypeChecking, false;
    "noImplicitAny" [] O(OptionId::NoImplicitAny), TypeChecking, false;
    "noImplicitOverride" [] O(OptionId::NoImplicitOverride), TypeChecking, false;
    "noImplicitReturns" [] O(OptionId::NoImplicitReturns), TypeChecking, false;
    "noImplicitThis" [] O(OptionId::NoImplicitThis), TypeChecking, false;
    "noPropertyAccessFromIndexSignature" [] O(OptionId::NoPropertyAccessFromIndexSignature), TypeChecking, false;
    "noUncheckedIndexedAccess" [] O(OptionId::NoUncheckedIndexedAccess), TypeChecking, false;
    "noUnusedLocals" [] O(OptionId::NoUnusedLocals), TypeChecking, false;
    "noUnusedParameters" [] O(OptionId::NoUnusedParameters), TypeChecking, false;
    "stableTypeOrdering" [] O(OptionId::StableTypeOrdering), TypeChecking, false;
    "strict" [] O(OptionId::Strict), TypeChecking, false;
    "strictBindCallApply" [] O(OptionId::StrictBindCallApply), TypeChecking, false;
    "strictBuiltinIteratorReturn" [] O(OptionId::StrictBuiltinIteratorReturn), TypeChecking, false;
    "strictFunctionTypes" [] O(OptionId::StrictFunctionTypes), TypeChecking, false;
    "strictNullChecks" [] O(OptionId::StrictNullChecks), TypeChecking, false;
    "strictPropertyInitialization" [] O(OptionId::StrictPropertyInitialization), TypeChecking, false;
    "useUnknownInCatchVariables" [] O(OptionId::UseUnknownInCatchVariables), TypeChecking, false;
    "assumeChangesOnlyAffectDirectDependencies" [] O(OptionId::AssumeChangesOnlyAffectDirectDependencies), WatchAndBuildModes, true;
    "composite" [] O(OptionId::Composite), Projects, false;
    "disableReferencedProjectLoad" [] O(OptionId::DisableReferencedProjectLoad), Projects, false;
    "disableSolutionSearching" [] O(OptionId::DisableSolutionSearching), Projects, false;
    "disableSourceOfProjectReferenceRedirect" [] O(OptionId::DisableSourceOfProjectReferenceRedirect), Projects, false;
    "incremental" ["i"] O(OptionId::Incremental), Projects, true;
    "tsBuildInfoFile" [] O(OptionId::TsBuildInfoFile), Projects, false;
    "declaration" ["d"] O(OptionId::Declaration), Emit, true;
    "declarationDir" [] O(OptionId::DeclarationDir), Emit, false;
    "declarationMap" [] O(OptionId::DeclarationMap), Emit, true;
    "downlevelIteration" [] O(OptionId::DownlevelIteration), Emit, false;
    "emitBOM" [] O(OptionId::EmitBom), Emit, false;
    "emitDeclarationOnly" [] O(OptionId::EmitDeclarationOnly), Emit, true;
    "importHelpers" [] O(OptionId::ImportHelpers), Emit, false;
    "inlineSourceMap" [] O(OptionId::InlineSourceMap), Emit, true;
    "inlineSources" [] O(OptionId::InlineSources), Emit, false;
    "mapRoot" [] O(OptionId::MapRoot), Emit, false;
    "newLine" [] O(OptionId::NewLine), Emit, false;
    "noEmit" [] O(OptionId::NoEmit), Emit, true;
    "noEmitHelpers" [] O(OptionId::NoEmitHelpers), Emit, false;
    "noEmitOnError" [] O(OptionId::NoEmitOnError), Emit, false;
    "outDir" [] O(OptionId::OutDir), Emit, false;
    "outFile" [] O(OptionId::OutFile), Emit, false;
    "preserveConstEnums" [] O(OptionId::PreserveConstEnums), Emit, false;
    "removeComments" [] O(OptionId::RemoveComments), Emit, false;
    "sourceMap" [] O(OptionId::SourceMap), Emit, true;
    "sourceRoot" [] O(OptionId::SourceRoot), Emit, false;
    "stripInternal" [] O(OptionId::StripInternal), Emit, false;
    "diagnostics" [] O(OptionId::Diagnostics), CompilerDiagnostics, true;
    "explainFiles" [] O(OptionId::ExplainFiles), CompilerDiagnostics, true;
    "extendedDiagnostics" [] O(OptionId::ExtendedDiagnostics), CompilerDiagnostics, true;
    "generateCpuProfile" [] O(OptionId::GenerateCpuProfile), CompilerDiagnostics, true;
    "generateTrace" [] O(OptionId::GenerateTrace), CompilerDiagnostics, true;
    "listEmittedFiles" [] O(OptionId::ListEmittedFiles), CompilerDiagnostics, true;
    "listFiles" [] O(OptionId::ListFiles), CompilerDiagnostics, true;
    "noCheck" [] O(OptionId::NoCheck), CompilerDiagnostics, true;
    "traceResolution" [] O(OptionId::TraceResolution), CompilerDiagnostics, true;
    "disableSizeLimit" [] O(OptionId::DisableSizeLimit), EditorSupport, false;
    "plugins" [] O(OptionId::Plugins), EditorSupport, false;
    "emitDecoratorMetadata" [] O(OptionId::EmitDecoratorMetadata), LanguageAndEnvironment, false;
    "experimentalDecorators" [] O(OptionId::ExperimentalDecorators), LanguageAndEnvironment, false;
    "jsx" [] O(OptionId::Jsx), LanguageAndEnvironment, false;
    "jsxFactory" [] O(OptionId::JsxFactory), LanguageAndEnvironment, false;
    "jsxFragmentFactory" [] O(OptionId::JsxFragmentFactory), LanguageAndEnvironment, false;
    "jsxImportSource" [] O(OptionId::JsxImportSource), LanguageAndEnvironment, false;
    "lib" [] O(OptionId::Lib), LanguageAndEnvironment, false;
    "libReplacement" [] O(OptionId::LibReplacement), LanguageAndEnvironment, false;
    "moduleDetection" [] O(OptionId::ModuleDetection), LanguageAndEnvironment, false;
    "noLib" [] O(OptionId::NoLib), LanguageAndEnvironment, false;
    "reactNamespace" [] O(OptionId::ReactNamespace), LanguageAndEnvironment, false;
    "target" ["t"] O(OptionId::Target), LanguageAndEnvironment, false;
    "useDefineForClassFields" [] O(OptionId::UseDefineForClassFields), LanguageAndEnvironment, false;
    "noErrorTruncation" [] O(OptionId::NoErrorTruncation), OutputFormatting, false;
    "preserveWatchOutput" [] O(OptionId::PreserveWatchOutput), OutputFormatting, true;
    "pretty" [] O(OptionId::Pretty), OutputFormatting, true;
    "skipDefaultLibCheck" [] O(OptionId::SkipDefaultLibCheck), Completeness, false;
    "skipLibCheck" [] O(OptionId::SkipLibCheck), Completeness, false;
    "watchInterval" [] W(WatchFlag::WatchInterval), Watch, true;
    "watchFile" [] W(WatchFlag::WatchFile), Watch, true;
    "watchDirectory" [] W(WatchFlag::WatchDirectory), Watch, true;
    "fallbackPolling" [] W(WatchFlag::FallbackPolling), Watch, true;
    "synchronousWatchDirectory" [] W(WatchFlag::SynchronousWatchDirectory), Watch, true;
    "excludeDirectories" [] W(WatchFlag::ExcludeDirectories), Watch, true;
    "excludeFiles" [] W(WatchFlag::ExcludeFiles), Watch, true;
    "verbose" ["v"] B(BuildFlag::Verbose), Build, true;
    "dry" ["d"] B(BuildFlag::Dry), Build, true;
    "force" ["f"] B(BuildFlag::Force), Build, true;
    "clean" [] B(BuildFlag::Clean), Build, true;
    "builders" [] B(BuildFlag::Builders), Build, true;
    "stopBuildOnErrors" [] B(BuildFlag::StopBuildOnErrors), Build, true;
    "ignoreDeprecations" [] O(OptionId::IgnoreDeprecations), Unlisted, false;
}

/// The flag named `name` (case-insensitive, without dashes).
pub fn find_flag(name: &str) -> Option<&'static Flag> {
    FLAGS.iter().find(|f| f.name.eq_ignore_ascii_case(name))
}

/// The flag `arg` names in the given mode: `-x` / `--x` with a name or alias.
fn lookup(word: &str, build: bool) -> Option<&'static Flag> {
    if let Some(f) = find_flag(word) {
        return Some(f);
    }
    let alias = |f: &&Flag| f.aliases.iter().any(|a| a.eq_ignore_ascii_case(word));
    if build {
        // Build flags' aliases shadow compiler ones (`-v`, `-d`); aliases of compiler
        // options not allowed in build mode are unknown there (`-p`, `-t`).
        FLAGS
            .iter()
            .filter(|f| matches!(f.target, FlagTarget::Build(_)))
            .find(alias)
            .or_else(|| FLAGS.iter().filter(|f| f.in_build).find(alias))
    } else {
        FLAGS
            .iter()
            .filter(|f| !matches!(f.target, FlagTarget::Build(_) | FlagTarget::BuildMode))
            .find(alias)
    }
}

/// `--build` options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildOptions {
    pub verbose: bool,
    pub dry: bool,
    pub force: bool,
    pub clean: bool,
    /// `--builders N` (N ≥ 1).
    pub builders: Option<u32>,
    pub stop_build_on_errors: bool,
}

/// A parsed command line.
#[derive(Clone, Debug, Default)]
pub struct CommandLine {
    /// `--build` / `-b` came first.
    pub build: bool,
    /// Compiler options given on the command line (paths absolute against the working
    /// directory). Merge them over a tsconfig's with [`CommandLine::apply_to`].
    pub options: CompilerOptions,
    /// Every compiler option given, in first-appearance order, `null` ones included (those
    /// are unset in [`CommandLine::options`] and clear the tsconfig's value).
    pub given: Vec<OptionId>,
    /// Watch options given on the command line.
    pub watch_options: WatchOptions,
    /// `--watchInterval` (milliseconds; not a `watchOptions` key).
    pub watch_interval: Option<u32>,
    /// Build options (only set under `--build`).
    pub build_options: BuildOptions,
    /// Root file names (projects under `--build`), as written.
    pub file_names: Vec<String>,
    /// Parse errors, in argument order, then combination errors. tsc prints them and exits
    /// with status 1 without doing anything else.
    pub diagnostics: Vec<ConfigDiagnostic>,
}

impl CommandLine {
    /// True when the boolean option `id` was given as `true`.
    pub fn flag(&self, id: OptionId) -> bool {
        matches!(self.options.get(id), Some(OptionValue::Bool(true)))
    }

    /// True when `id` was given (with a value or `null`).
    pub fn is_given(&self, id: OptionId) -> bool {
        self.given.contains(&id)
    }

    /// Merges the command-line options over `options` (a loaded tsconfig's): each given
    /// option replaces the config's value, `null` clears it.
    pub fn apply_to(&self, options: &mut CompilerOptions) {
        for &id in &self.given {
            options.set(id, self.options.get(id).cloned(), None);
        }
    }
}

/// The value kinds the parser distinguishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Bool,
    /// An integer with a minimum (TS5002 below it).
    Number(i64),
    String,
    Path,
    List,
    PathList,
    Lib,
    Enum(&'static [&'static str]),
    /// `paths`, `plugins`, `rootDirs`: tsconfig only (TS6064) unless `null`.
    ConfigOnly,
}

fn kind_of(flag: &Flag) -> Kind {
    match flag.target {
        FlagTarget::BuildMode => Kind::Bool,
        FlagTarget::Build(BuildFlag::Builders) => Kind::Number(1),
        FlagTarget::Build(_) => Kind::Bool,
        FlagTarget::Watch(w) => match w {
            WatchFlag::WatchInterval => Kind::Number(0),
            WatchFlag::WatchFile => Kind::Enum(WatchFileKind::VALID),
            WatchFlag::WatchDirectory => Kind::Enum(WatchDirectoryKind::VALID),
            WatchFlag::FallbackPolling => Kind::Enum(PollingKind::VALID),
            WatchFlag::SynchronousWatchDirectory => Kind::Bool,
            WatchFlag::ExcludeDirectories | WatchFlag::ExcludeFiles => Kind::PathList,
        },
        FlagTarget::Compiler(id) => {
            if matches!(id, OptionId::Paths | OptionId::Plugins | OptionId::RootDirs) {
                return Kind::ConfigOnly;
            }
            match id.decl().ty {
                OptionType::Bool => Kind::Bool,
                OptionType::Number => Kind::Number(if id == OptionId::Checkers { 1 } else { 0 }),
                OptionType::String => Kind::String,
                OptionType::Path => Kind::Path,
                OptionType::StringList => Kind::List,
                OptionType::PathList => Kind::PathList,
                OptionType::Lib => Kind::Lib,
                OptionType::Target => Kind::Enum(ScriptTarget::VALID),
                OptionType::Module => Kind::Enum(ModuleKind::VALID),
                OptionType::ModuleResolution => Kind::Enum(ModuleResolution::VALID),
                OptionType::ModuleDetection => Kind::Enum(ModuleDetection::VALID),
                OptionType::Jsx => Kind::Enum(JsxEmit::VALID),
                OptionType::NewLine => Kind::Enum(NewLine::VALID),
                OptionType::PathMap | OptionType::Plugins => Kind::ConfigOnly,
            }
        }
    }
}

/// A parsed flag value.
enum Value {
    /// `null`: clear the option.
    Null,
    Bool(bool),
    Number(f64),
    Str(String),
    List(Vec<String>),
    /// No value (a list without items); the option is left unset.
    None,
}

fn quoted_list(valid: &[&str]) -> String {
    valid
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn expects_argument(name: &str) -> ConfigDiagnostic {
    ConfigDiagnostic::global(
        6044,
        format!("Compiler option '{name}' expects an argument."),
    )
}

fn enum_error(name: &str, valid: &[&str]) -> ConfigDiagnostic {
    ConfigDiagnostic::global(
        6046,
        format!(
            "Argument for '--{name}' option must be: {}.",
            quoted_list(valid)
        ),
    )
}

/// An integer as tsc's command line reads it: optional sign, decimal digits.
fn parse_integer(s: &str) -> Option<i64> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<i64>().ok().or_else(|| {
        // Out of range: saturate rather than fail.
        Some(if s.starts_with('-') {
            i64::MIN
        } else {
            i64::MAX
        })
    })
}

/// `--locale` values tsc rejects outright (TS6048): not shaped like a BCP 47 tag. tsc also
/// rejects some well-formed tags it has no language for (`xx`); those are accepted here.
fn valid_locale(s: &str) -> bool {
    let mut parts = s.split(['-', '_']);
    let lang = parts.next().unwrap_or("");
    (2..=3).contains(&lang.len())
        && lang.bytes().all(|b| b.is_ascii_alphabetic())
        && parts
            .all(|p| !p.is_empty() && p.len() <= 8 && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Splits a response file into arguments: whitespace-separated, `"..."` groups a quoted
/// argument (quotes removed).
fn response_args(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut has = false;
    for c in text.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

/// Parses tsc's command line. `cwd` (absolute) resolves path values and response files.
/// Never panics; problems are [`CommandLine::diagnostics`].
pub fn parse_command_line(args: &[String], cwd: &str, fs: &dyn FileSystem) -> CommandLine {
    let mut cl = CommandLine::default();
    let mut words = args.to_vec();
    let mut i = 0;
    // Expand one level, only after preceding flags have consumed their values.
    let mut response_end = 0;
    while i < words.len() {
        if i >= response_end
            && let Some(file) = words[i].strip_prefix('@').filter(|s| !s.is_empty())
        {
            let abs = path::resolve(cwd, file);
            let expanded = match fs.read_file(&abs) {
                Some(text) => response_args(&text),
                None => {
                    cl.diagnostics.push(ConfigDiagnostic::global(
                        5083,
                        format!("Cannot read file '{abs}'."),
                    ));
                    Vec::new()
                }
            };
            response_end = i + expanded.len();
            words.splice(i..i + 1, expanded);
            continue;
        }
        let arg = &words[i];
        i += 1;
        let Some(word) = dashed(arg) else {
            cl.file_names.push(arg.clone());
            continue;
        };
        if i == 1 && (word.eq_ignore_ascii_case("build") || word.eq_ignore_ascii_case("b")) {
            cl.build = true;
            continue;
        }
        let Some(flag) = lookup(word, cl.build) else {
            cl.diagnostics.push(unknown(arg, cl.build));
            continue;
        };
        let name = flag.name;
        match flag.target {
            FlagTarget::BuildMode if !cl.build => {
                if word.eq_ignore_ascii_case("build") {
                    cl.diagnostics.push(ConfigDiagnostic::global(
                        6369,
                        "Option '--build' must be the first command line argument.",
                    ));
                } else {
                    cl.diagnostics.push(unknown(arg, false));
                }
                continue;
            }
            FlagTarget::BuildMode => continue,
            _ => {}
        }
        let misplaced = if cl.build {
            (!flag.in_build).then(|| {
                ConfigDiagnostic::global(
                    5094,
                    format!("Compiler option '--{name}' may not be used with '--build'."),
                )
            })
        } else {
            matches!(flag.target, FlagTarget::Build(_)).then(|| {
                ConfigDiagnostic::global(
                    5093,
                    format!("Compiler option '--{name}' may only be used with '--build'."),
                )
            })
        };
        let kind = kind_of(flag);
        if let Some(d) = misplaced {
            // The value is still consumed; only the placement error is reported.
            cl.diagnostics.push(d);
            let mut ignored = Vec::new();
            read_value(flag, kind, &words, &mut i, cwd, &mut ignored);
            continue;
        }
        let Some(value) = read_value(flag, kind, &words, &mut i, cwd, &mut cl.diagnostics) else {
            continue;
        };
        store(&mut cl, flag, value);
    }
    combination_errors(&mut cl);
    cl
}

/// `-x` / `--x` → `x`; `None` for file names. `-`, `--` and `---x` give a word no flag has.
fn dashed(arg: &str) -> Option<&str> {
    let rest = arg.strip_prefix('-')?;
    Some(rest.strip_prefix('-').unwrap_or(rest))
}

fn unknown(arg: &str, build: bool) -> ConfigDiagnostic {
    if build {
        ConfigDiagnostic::global(5072, format!("Unknown build option '{arg}'."))
    } else {
        ConfigDiagnostic::global(5023, format!("Unknown compiler option '{arg}'."))
    }
}

/// Reads the value of `flag` from `words[*i..]`, advancing `*i` past what it consumes.
/// `None` when the value is invalid (its diagnostic pushed) or absent.
fn read_value(
    flag: &Flag,
    kind: Kind,
    words: &[String],
    i: &mut usize,
    cwd: &str,
    diags: &mut Vec<ConfigDiagnostic>,
) -> Option<Value> {
    let name = flag.name;
    let next = words.get(*i).map(String::as_str);
    if next == Some("null") {
        *i += 1;
        return Some(Value::Null);
    }
    let missing = |diags: &mut Vec<ConfigDiagnostic>| match flag.target {
        FlagTarget::Build(_) => diags.push(ConfigDiagnostic::global(
            5073,
            format!("Build option '{name}' requires a value of type number."),
        )),
        FlagTarget::Watch(WatchFlag::WatchInterval) => diags.push(ConfigDiagnostic::global(
            5080,
            format!("Watch option '{name}' requires a value of type number."),
        )),
        _ => diags.push(expects_argument(name)),
    };
    match kind {
        Kind::Bool => match next {
            Some("true") => {
                *i += 1;
                Some(Value::Bool(true))
            }
            Some("false") => {
                *i += 1;
                Some(Value::Bool(false))
            }
            _ => Some(Value::Bool(true)),
        },
        Kind::Number(min) => {
            let Some(v) = next else {
                missing(diags);
                return None;
            };
            *i += 1;
            match parse_integer(v) {
                Some(n) if n < min => {
                    diags.push(ConfigDiagnostic::global(
                        5002,
                        format!("Option '{name}' requires value to be greater than '{min}'."),
                    ));
                    None
                }
                Some(n) => Some(Value::Number(n as f64)),
                None => {
                    missing(diags);
                    None
                }
            }
        }
        Kind::String | Kind::Path => {
            let Some(v) = next else {
                diags.push(expects_argument(name));
                return None;
            };
            *i += 1;
            if flag.target == FlagTarget::Compiler(OptionId::Locale) && !valid_locale(v) {
                diags.push(ConfigDiagnostic::global(
                    6048,
                    "Locale must be an IETF BCP 47 language tag. Examples: 'en', 'ja-jp'.",
                ));
                return None;
            }
            Some(Value::Str(if kind == Kind::Path {
                path::resolve(cwd, v)
            } else {
                v.to_owned()
            }))
        }
        Kind::Enum(valid) => {
            let Some(v) = next else {
                diags.push(expects_argument(name));
                diags.push(enum_error(name, valid));
                return None;
            };
            *i += 1;
            let lower = v.to_ascii_lowercase();
            if parse_enum(flag, &lower).is_none() {
                diags.push(enum_error(name, valid));
                return None;
            }
            Some(Value::Str(lower))
        }
        Kind::List | Kind::PathList | Kind::Lib => {
            let Some(v) = next else {
                diags.push(expects_argument(name));
                return None;
            };
            let items: Vec<&str> = v.split(',').filter(|s| !s.is_empty()).collect();
            if v.starts_with('-') || items.is_empty() {
                return Some(Value::None);
            }
            *i += 1;
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                match kind {
                    Kind::PathList => out.push(path::resolve(cwd, it)),
                    Kind::Lib => {
                        if lib_file_name(it).is_some() {
                            out.push(it.to_ascii_lowercase());
                        } else {
                            // One error for the option; it stays unset.
                            let valid: Vec<&str> = LIBS.iter().map(|(n, _)| *n).collect();
                            diags.push(enum_error(name, &valid));
                            return None;
                        }
                    }
                    _ => out.push(it.to_owned()),
                }
            }
            Some(Value::List(out))
        }
        Kind::ConfigOnly => {
            if next.is_some() {
                *i += 1;
            }
            diags.push(ConfigDiagnostic::global(
                6064,
                format!(
                    "Option '{name}' can only be specified in 'tsconfig.json' file or set to 'null' on command line."
                ),
            ));
            None
        }
    }
}

/// Checks an enumeration value (lower case) for `flag`.
fn parse_enum(flag: &Flag, v: &str) -> Option<OptionValue> {
    match flag.target {
        FlagTarget::Compiler(id) => match id.decl().ty {
            OptionType::Target => ScriptTarget::parse(v).map(OptionValue::Target),
            OptionType::Module => ModuleKind::parse(v).map(OptionValue::Module),
            OptionType::ModuleResolution => {
                ModuleResolution::parse(v).map(OptionValue::ModuleResolution)
            }
            OptionType::ModuleDetection => {
                ModuleDetection::parse(v).map(OptionValue::ModuleDetection)
            }
            OptionType::Jsx => JsxEmit::parse(v).map(OptionValue::Jsx),
            OptionType::NewLine => NewLine::parse(v).map(OptionValue::NewLine),
            _ => None,
        },
        // Watch enumerations are kept in `WatchOptions`; any `Some` means valid.
        FlagTarget::Watch(WatchFlag::WatchFile) => {
            WatchFileKind::parse(v).map(|_| OptionValue::Bool(true))
        }
        FlagTarget::Watch(WatchFlag::WatchDirectory) => {
            WatchDirectoryKind::parse(v).map(|_| OptionValue::Bool(true))
        }
        FlagTarget::Watch(WatchFlag::FallbackPolling) => {
            PollingKind::parse(v).map(|_| OptionValue::Bool(true))
        }
        _ => None,
    }
}

fn store(cl: &mut CommandLine, flag: &Flag, value: Value) {
    match flag.target {
        FlagTarget::Compiler(id) => {
            let v = match value {
                Value::None => return,
                Value::Null => None,
                Value::Bool(b) => Some(OptionValue::Bool(b)),
                Value::Number(n) => Some(OptionValue::Number(n)),
                Value::List(l) => Some(OptionValue::List(l)),
                Value::Str(s) => match id.decl().ty {
                    OptionType::String | OptionType::Path => Some(OptionValue::String(s)),
                    _ => parse_enum(flag, &s),
                },
            };
            cl.options.set(id, v, None);
            if !cl.given.contains(&id) {
                cl.given.push(id);
            }
        }
        FlagTarget::Watch(w) => {
            let wo = &mut cl.watch_options;
            match (w, value) {
                (_, Value::None) => {}
                (WatchFlag::WatchFile, Value::Str(s)) => wo.watch_file = WatchFileKind::parse(&s),
                (WatchFlag::WatchDirectory, Value::Str(s)) => {
                    wo.watch_directory = WatchDirectoryKind::parse(&s)
                }
                (WatchFlag::FallbackPolling, Value::Str(s)) => {
                    wo.fallback_polling = PollingKind::parse(&s)
                }
                (WatchFlag::SynchronousWatchDirectory, Value::Bool(b)) => {
                    wo.synchronous_watch_directory = Some(b)
                }
                (WatchFlag::ExcludeDirectories, Value::List(l)) => wo.exclude_directories = Some(l),
                (WatchFlag::ExcludeFiles, Value::List(l)) => wo.exclude_files = Some(l),
                (WatchFlag::WatchInterval, Value::Number(n)) => {
                    cl.watch_interval = Some(n.min(u32::MAX as f64) as u32)
                }
                (WatchFlag::WatchFile, Value::Null) => wo.watch_file = None,
                (WatchFlag::WatchDirectory, Value::Null) => wo.watch_directory = None,
                (WatchFlag::FallbackPolling, Value::Null) => wo.fallback_polling = None,
                (WatchFlag::SynchronousWatchDirectory, Value::Null) => {
                    wo.synchronous_watch_directory = None
                }
                (WatchFlag::ExcludeDirectories, Value::Null) => wo.exclude_directories = None,
                (WatchFlag::ExcludeFiles, Value::Null) => wo.exclude_files = None,
                (WatchFlag::WatchInterval, Value::Null) => cl.watch_interval = None,
                _ => {}
            }
        }
        FlagTarget::Build(b) => {
            let bo = &mut cl.build_options;
            let on = match value {
                Value::Bool(v) => v,
                Value::Null => false,
                _ => true,
            };
            match b {
                BuildFlag::Verbose => bo.verbose = on,
                BuildFlag::Dry => bo.dry = on,
                BuildFlag::Force => bo.force = on,
                BuildFlag::Clean => bo.clean = on,
                BuildFlag::StopBuildOnErrors => bo.stop_build_on_errors = on,
                BuildFlag::Builders => {
                    bo.builders = match value {
                        Value::Number(n) => Some(n.min(u32::MAX as f64) as u32),
                        _ => None,
                    }
                }
            }
        }
        FlagTarget::BuildMode => {}
    }
}

/// TS6370 for options that cannot be combined, in tsc's order.
fn combination_errors(cl: &mut CommandLine) {
    let pairs: Vec<(&str, bool, &str, bool)> = if cl.build {
        let b = &cl.build_options;
        let watch = cl.flag(OptionId::Watch);
        vec![
            ("clean", b.clean, "force", b.force),
            ("clean", b.clean, "verbose", b.verbose),
            ("clean", b.clean, "watch", watch),
            ("watch", watch, "dry", b.dry),
        ]
    } else {
        vec![(
            "watch",
            cl.flag(OptionId::Watch),
            "listFilesOnly",
            cl.flag(OptionId::ListFilesOnly),
        )]
    };
    for (a, x, b, y) in pairs {
        if x && y {
            cl.diagnostics.push(ConfigDiagnostic::global(
                6370,
                format!("Options '{a}' and '{b}' cannot be combined."),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::MemoryFs;

    /// `tsc --all` as tsc 7.0.2 prints it (captured under `scripts/limit.sh`).
    const TSC_ALL: &str = include_str!("../tests/fixtures/tsc-7.0.2-all.txt");

    fn parse_with(args: &[&str], fs: &MemoryFs) -> CommandLine {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_command_line(&args, "/p", fs)
    }

    fn parse(args: &[&str]) -> CommandLine {
        parse_with(args, &MemoryFs::new())
    }

    fn errors(args: &[&str]) -> Vec<String> {
        parse(args)
            .diagnostics
            .iter()
            .map(|d| format!("TS{}: {}", d.code, d.message))
            .collect()
    }

    /// One flag of the `tsc --all` listing.
    struct Listed {
        name: String,
        aliases: Vec<String>,
        section: String,
        boolean: bool,
        one_of: Vec<String>,
    }

    fn listed() -> Vec<Listed> {
        let mut out: Vec<Listed> = Vec::new();
        let mut section = String::new();
        for line in TSC_ALL.lines() {
            if let Some(h) = line.strip_prefix("### ") {
                section = h.to_owned();
            } else if line == "WATCH OPTIONS" || line == "BUILD OPTIONS" {
                section = line.to_owned();
            } else if let Some(rest) = line.strip_prefix("--") {
                let mut parts = rest.split(", -");
                let name = parts.next().unwrap_or_default().to_owned();
                let aliases: Vec<String> = parts.map(str::to_owned).collect();
                if let Some(prev) = out.iter_mut().find(|l| l.name == name) {
                    prev.aliases.extend(aliases);
                    continue;
                }
                out.push(Listed {
                    name,
                    aliases,
                    section: section.clone(),
                    boolean: false,
                    one_of: Vec::new(),
                });
            } else if line == "type: boolean" {
                out.last_mut().unwrap().boolean = true;
            } else if let Some(v) = line.strip_prefix("one of: ") {
                out.last_mut().unwrap().one_of = v
                    .split(", ")
                    .flat_map(|s| s.split('/'))
                    .map(str::to_owned)
                    .collect();
            }
        }
        out
    }

    #[test]
    fn table_matches_tsc_all() {
        let listed = listed();
        assert_eq!(listed.len(), 137, "tsc --all lists 137 distinct flags");
        let ours: Vec<&Flag> = FLAGS
            .iter()
            .filter(|f| f.section != Section::Unlisted)
            .collect();
        assert_eq!(ours.len(), listed.len());
        for (l, f) in listed.iter().zip(&ours) {
            assert_eq!(l.name, f.name, "order");
            assert_eq!(l.aliases, f.aliases, "aliases of {}", f.name);
            assert_eq!(l.section, f.section.title(), "section of {}", f.name);
            if l.boolean {
                assert_eq!(kind_of(f), Kind::Bool, "{} is boolean", f.name);
            }
            if let FlagTarget::Compiler(id) = f.target {
                assert_eq!(id.decl().name, f.name, "option id of {}", f.name);
            }
        }
    }

    /// A value `tsc` accepts for the flag, from the listing or its kind.
    fn sample(l: &Listed, f: &Flag) -> Option<String> {
        if let Some(v) = l.one_of.first() {
            return Some(v.clone());
        }
        match kind_of(f) {
            Kind::Bool => None,
            Kind::Number(_) => Some("2".into()),
            Kind::List | Kind::PathList => Some("a,b".into()),
            Kind::Lib => Some("es2020,DOM".into()),
            Kind::ConfigOnly => Some("null".into()),
            Kind::String if f.name == "locale" => Some("en".into()),
            Kind::String | Kind::Path => Some("x".into()),
            Kind::Enum(v) => Some(v[0].into()),
        }
    }

    #[test]
    fn every_tsc_all_flag_parses() {
        let listed = listed();
        for (l, f) in listed.iter().zip(FLAGS) {
            let build = f.section == Section::Build;
            let value = sample(l, f);
            let mut spellings = vec![format!("--{}", f.name), format!("-{}", f.name)];
            spellings.push(format!("--{}", f.name.to_ascii_uppercase()));
            for a in f.aliases {
                spellings.push(format!("-{a}"));
            }
            for spelling in spellings {
                let mut args: Vec<&str> = Vec::new();
                if build {
                    args.push("--build");
                }
                args.push(&spelling);
                if let Some(v) = &value {
                    args.push(v);
                }
                let cl = parse(&args);
                assert!(cl.diagnostics.is_empty(), "{args:?}: {:?}", cl.diagnostics);
                assert!(
                    cl.file_names.is_empty(),
                    "{args:?} left {:?}",
                    cl.file_names
                );
                match f.target {
                    FlagTarget::BuildMode => assert!(cl.build),
                    FlagTarget::Compiler(id) => assert!(cl.is_given(id), "{args:?}"),
                    _ => {}
                }
            }
            // Every listed enumeration value parses.
            for v in &l.one_of {
                let mut args = vec![format!("--{}", f.name), v.clone()];
                if build {
                    args.insert(0, "--build".into());
                }
                let cl = parse_command_line(&args, "/p", &MemoryFs::new());
                assert!(cl.diagnostics.is_empty(), "{args:?}: {:?}", cl.diagnostics);
            }
        }
        // The unlisted one.
        let cl = parse(&["--ignoreDeprecations", "6.0"]);
        assert!(cl.diagnostics.is_empty() && cl.is_given(OptionId::IgnoreDeprecations));
    }

    #[test]
    fn values() {
        let cl = parse(&[
            "--strict",
            "false",
            "--noEmit",
            "--target",
            "ES2022",
            "--outDir",
            "dist",
            "--types",
            "node,,jest",
            "--lib",
            "ES2020,dom",
            "--checkers",
            "+3",
            "--mapRoot",
            "m",
            "a.ts",
        ]);
        assert!(cl.diagnostics.is_empty(), "{:?}", cl.diagnostics);
        assert_eq!(
            cl.options.get(OptionId::Strict),
            Some(&OptionValue::Bool(false))
        );
        assert!(cl.flag(OptionId::NoEmit));
        assert_eq!(
            cl.options.get(OptionId::Target),
            Some(&OptionValue::Target(ScriptTarget::ES2022))
        );
        assert_eq!(
            cl.options.get(OptionId::OutDir),
            Some(&OptionValue::String("/p/dist".into()))
        );
        assert_eq!(
            cl.options.get(OptionId::Types),
            Some(&OptionValue::List(vec!["node".into(), "jest".into()]))
        );
        assert_eq!(
            cl.options.get(OptionId::Lib),
            Some(&OptionValue::List(vec!["es2020".into(), "dom".into()]))
        );
        assert_eq!(
            cl.options.get(OptionId::Checkers),
            Some(&OptionValue::Number(3.0))
        );
        assert_eq!(cl.file_names, ["a.ts"]);
        // Booleans take only an exact `true` / `false`.
        let cl = parse(&["--strict", "FALSE"]);
        assert!(cl.flag(OptionId::Strict));
        assert_eq!(cl.file_names, ["FALSE"]);
        // Strings take the next argument whatever it is.
        let cl = parse(&["--jsxFactory", "--pretty"]);
        assert!(cl.diagnostics.is_empty());
        assert!(!cl.is_given(OptionId::Pretty));
        // Lists leave a dashed or empty next argument alone.
        let cl = parse(&["--types", "--strict"]);
        assert!(cl.diagnostics.is_empty() && !cl.is_given(OptionId::Types));
        assert!(cl.flag(OptionId::Strict));
        let cl = parse(&["--types", ","]);
        assert_eq!(cl.file_names, [","]);
        let cl = parse(&["--types", "x,-y"]);
        assert_eq!(
            cl.options.get(OptionId::Types),
            Some(&OptionValue::List(vec!["x".into(), "-y".into()]))
        );
        // The last occurrence wins; `null` clears.
        let cl = parse(&["--types", "a", "--types", "b", "--target", "null"]);
        assert_eq!(
            cl.options.get(OptionId::Types),
            Some(&OptionValue::List(vec!["b".into()]))
        );
        assert!(cl.is_given(OptionId::Target) && cl.options.get(OptionId::Target).is_none());
        let mut merged = CompilerOptions::default();
        merged.set(
            OptionId::Target,
            Some(OptionValue::Target(ScriptTarget::ES2020)),
            None,
        );
        cl.apply_to(&mut merged);
        assert!(merged.get(OptionId::Target).is_none());
        // `es5` parses (verify reports TS5108 later) though TS6046's list leaves it out.
        assert!(parse(&["--target", "es5"]).diagnostics.is_empty());
        // Watch options.
        let cl = parse(&["--watchFile", "UseFsEvents", "--watchInterval", "250"]);
        assert_eq!(
            cl.watch_options.watch_file,
            Some(WatchFileKind::UseFsEvents)
        );
        assert_eq!(cl.watch_interval, Some(250));
    }

    #[test]
    fn unknown_and_misplaced() {
        assert_eq!(
            errors(&["--foo", "--target=es2020", "-", "--", "---strict", "-x"]),
            [
                "TS5023: Unknown compiler option '--foo'.",
                "TS5023: Unknown compiler option '--target=es2020'.",
                "TS5023: Unknown compiler option '-'.",
                "TS5023: Unknown compiler option '--'.",
                "TS5023: Unknown compiler option '---strict'.",
                "TS5023: Unknown compiler option '-x'.",
            ]
        );
        assert_eq!(
            errors(&["--strict", "--build", "-b"]),
            [
                "TS6369: Option '--build' must be the first command line argument.",
                "TS5023: Unknown compiler option '-b'.",
            ]
        );
        assert_eq!(
            errors(&["--builders", "3", "--strict", "--foo"]),
            [
                "TS5093: Compiler option '--builders' may only be used with '--build'.",
                "TS5023: Unknown compiler option '--foo'.",
            ]
        );
        assert_eq!(
            errors(&["-b", "--project", "x", "--foo", "-p", "-t"]),
            [
                "TS5094: Compiler option '--project' may not be used with '--build'.",
                "TS5072: Unknown build option '--foo'.",
                "TS5072: Unknown build option '-p'.",
                "TS5072: Unknown build option '-t'.",
            ]
        );
        assert_eq!(
            errors(&["-b", "--version", "--ignoreDeprecations", "6.0"]),
            [
                "TS5094: Compiler option '--version' may not be used with '--build'.",
                "TS5094: Compiler option '--ignoreDeprecations' may not be used with '--build'.",
            ]
        );
        // Build-mode aliases.
        let cl = parse(&["-b", "-v", "-d", "-f", "-w", "-i", "-q", "app"]);
        assert!(
            cl.build && cl.diagnostics.len() == 1,
            "{:?}",
            cl.diagnostics
        );
        let b = &cl.build_options;
        assert!(b.verbose && b.dry && b.force);
        assert!(cl.flag(OptionId::Watch));
        assert!(cl.flag(OptionId::Incremental) && cl.flag(OptionId::Quiet));
        assert_eq!(cl.file_names, ["app"]);
        assert_eq!(
            cl.diagnostics[0].message,
            "Options 'watch' and 'dry' cannot be combined."
        );
        // Outside build mode `-v` is `--version`, `-d` `--declaration`.
        let cl = parse(&["-v", "-d", "-P", "."]);
        assert!(cl.flag(OptionId::Version) && cl.flag(OptionId::Declaration));
        assert_eq!(
            cl.options.get(OptionId::Project),
            Some(&OptionValue::String(".".into()))
        );
    }

    #[test]
    fn value_errors() {
        assert_eq!(
            errors(&["--outDir"]),
            ["TS6044: Compiler option 'outDir' expects an argument."]
        );
        assert_eq!(
            errors(&[
                "--checkers",
                "0",
                "--checkers",
                "2.5",
                "--maxNodeModuleJsDepth",
                "-1"
            ]),
            [
                "TS5002: Option 'checkers' requires value to be greater than '1'.",
                "TS6044: Compiler option 'checkers' expects an argument.",
                "TS5002: Option 'maxNodeModuleJsDepth' requires value to be greater than '0'.",
            ]
        );
        assert_eq!(
            errors(&["--module", "bogus", "--newLine"]),
            [
                "TS6046: Argument for '--module' option must be: 'commonjs', 'es6', 'es2015', 'es2020', 'es2022', 'esnext', 'node16', 'node18', 'node20', 'nodenext', 'preserve'.",
                "TS6044: Compiler option 'newLine' expects an argument.",
                "TS6046: Argument for '--newLine' option must be: 'crlf', 'lf'.",
            ]
        );
        let lib = errors(&["--lib", "es2020,bogus"]);
        assert_eq!(lib.len(), 1);
        assert!(lib[0].starts_with("TS6046: Argument for '--lib' option must be: 'es5', 'es6',"));
        assert_eq!(
            errors(&["--rootDirs", "a", "--paths"]),
            [
                "TS6064: Option 'rootDirs' can only be specified in 'tsconfig.json' file or set to 'null' on command line.",
                "TS6064: Option 'paths' can only be specified in 'tsconfig.json' file or set to 'null' on command line.",
            ]
        );
        assert!(errors(&["--paths", "null", "--plugins", "null"]).is_empty());
        assert_eq!(
            errors(&[
                "--watchInterval",
                "x",
                "--watchInterval",
                "-1",
                "--watchInterval"
            ]),
            [
                "TS5080: Watch option 'watchInterval' requires a value of type number.",
                "TS5002: Option 'watchInterval' requires value to be greater than '0'.",
                "TS5080: Watch option 'watchInterval' requires a value of type number.",
            ]
        );
        assert_eq!(
            errors(&["-b", "--builders", "x", "--builders", "0", "--builders"]),
            [
                "TS5073: Build option 'builders' requires a value of type number.",
                "TS5002: Option 'builders' requires value to be greater than '1'.",
                "TS5073: Build option 'builders' requires a value of type number.",
            ]
        );
        assert_eq!(
            errors(&["--locale", "xx-"]),
            ["TS6048: Locale must be an IETF BCP 47 language tag. Examples: 'en', 'ja-jp'."]
        );
        for ok in ["de", "en-GB", "pt-br", "EN", "en_US"] {
            assert!(errors(&["--locale", ok]).is_empty(), "{ok}");
        }
        assert_eq!(
            errors(&["-b", "--clean", "--force", "--verbose", "--watch", "--dry"]),
            [
                "TS6370: Options 'clean' and 'force' cannot be combined.",
                "TS6370: Options 'clean' and 'verbose' cannot be combined.",
                "TS6370: Options 'clean' and 'watch' cannot be combined.",
                "TS6370: Options 'watch' and 'dry' cannot be combined.",
            ]
        );
        assert_eq!(
            errors(&["--watch", "--listFilesOnly"]),
            ["TS6370: Options 'watch' and 'listFilesOnly' cannot be combined."]
        );
    }

    #[test]
    fn response_files() {
        let mut fs = MemoryFs::new();
        fs.add_file("/p/args.txt", "--strict\n  \"a b.ts\"  --target es2022\n");
        let cl = parse_with(&["@args.txt", "c.ts"], &fs);
        assert!(cl.diagnostics.is_empty());
        assert!(cl.flag(OptionId::Strict));
        assert_eq!(cl.file_names, ["a b.ts", "c.ts"]);
        let cl = parse_with(&["@nope.txt"], &fs);
        assert_eq!(cl.diagnostics[0].code, 5083);
        assert_eq!(cl.diagnostics[0].message, "Cannot read file '/p/nope.txt'.");
    }

    #[test]
    fn response_files_preserve_option_values() {
        let mut fs = MemoryFs::new();
        fs.add_file(
            "/p/args.txt",
            "--jsxImportSource @emotion/react --types @scope/pkg --outDir @dist",
        );
        for args in [
            &[
                "--jsxImportSource",
                "@emotion/react",
                "--types",
                "@scope/pkg",
                "--outDir",
                "@dist",
            ][..],
            &["@args.txt"],
        ] {
            let cl = parse_with(args, &fs);
            assert!(cl.diagnostics.is_empty(), "{:?}", cl.diagnostics);
            assert_eq!(
                cl.options.get(OptionId::JsxImportSource),
                Some(&OptionValue::String("@emotion/react".to_owned()))
            );
            assert_eq!(
                cl.options.get(OptionId::Types),
                Some(&OptionValue::List(vec!["@scope/pkg".to_owned()]))
            );
            assert_eq!(cl.options.out_dir(), Some("/p/@dist"));
        }
        fs.add_file("/p/build.txt", "--build --verbose");
        let cl = parse_with(&["@build.txt"], &fs);
        assert!(cl.build && cl.diagnostics.is_empty());
    }

    #[test]
    fn affects_reaches_flags() {
        use crate::options::affects::{CHECK, EMIT};
        let strict = find_flag("strict").unwrap();
        assert_eq!(strict.affects() & CHECK, CHECK);
        assert_eq!(strict.affects() & EMIT, 0);
        assert_eq!(find_flag("verbose").unwrap().affects(), 0);
        assert!(find_flag("SOURCEMAP").is_some());
    }
}
