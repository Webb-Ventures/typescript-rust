//! Per-file language environment: source type, globals and settings resolved from config.

use std::sync::Arc;

use rustc_hash::FxHashMap;

use crate::lib_globals::{LibGlobals, ProjectLib};

/// How top-level code is treated (ESLint's `languageOptions.sourceType`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum SourceType {
    #[default]
    Module,
    Script,
    /// CommonJS: top level is a function scope; `require`, `module`, `exports` are globals.
    CommonJs,
}

impl SourceType {
    pub fn parse(s: &str) -> Option<SourceType> {
        match s {
            "module" => Some(SourceType::Module),
            "script" => Some(SourceType::Script),
            "commonjs" => Some(SourceType::CommonJs),
            _ => None,
        }
    }
}

/// Whether a global may be assigned.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum GlobalAccess {
    Readonly,
    Writable,
    /// Explicitly disabled (`"off"`): treated as not defined.
    Off,
}

impl GlobalAccess {
    /// Parses ESLint's global values: `"readonly"`/`"readable"`/`false`, `"writable"`/
    /// `"writeable"`/`true`, `"off"`.
    pub fn from_json(v: &serde_json::Value) -> Option<GlobalAccess> {
        match v {
            serde_json::Value::Bool(true) => Some(GlobalAccess::Writable),
            serde_json::Value::Bool(false) | serde_json::Value::Null => {
                Some(GlobalAccess::Readonly)
            }
            serde_json::Value::String(s) => match s.as_str() {
                "readonly" | "readable" | "false" => Some(GlobalAccess::Readonly),
                "writable" | "writeable" | "true" => Some(GlobalAccess::Writable),
                "off" => Some(GlobalAccess::Off),
                _ => None,
            },
            _ => None,
        }
    }
}

/// A set of global variables.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Globals {
    map: FxHashMap<String, GlobalAccess>,
}

impl Globals {
    pub fn new() -> Globals {
        Globals::default()
    }

    pub fn insert(&mut self, name: &str, access: GlobalAccess) {
        self.map.insert(name.to_owned(), access);
    }

    pub fn get(&self, name: &str) -> Option<GlobalAccess> {
        match self.map.get(name) {
            Some(GlobalAccess::Off) | None => None,
            Some(a) => Some(*a),
        }
    }

    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Names in sorted order (for hashing and output).
    pub fn sorted(&self) -> Vec<(&str, GlobalAccess)> {
        let mut v: Vec<_> = self.map.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        v.sort_unstable_by(|a, b| a.0.cmp(b.0));
        v
    }

    pub fn extend(&mut self, other: &Globals) {
        for (k, v) in &other.map {
            self.map.insert(k.clone(), *v);
        }
    }
}

/// Everything about a file's language setup that rules may need.
#[derive(Clone, Debug)]
pub struct LintEnv {
    pub source_type: SourceType,
    /// `languageOptions.ecmaVersion` (`latest` = 2026).
    pub ecma_version: u32,
    /// Built-in ES globals for `ecma_version` plus configured globals.
    pub globals: Arc<Globals>,
    /// Parsed with the TypeScript grammar (a `.ts`-family file, or a TS parser configured).
    pub typescript: bool,
    /// The JSX factory that TSX files implicitly reference (typescript-eslint's `jsxPragma`).
    pub jsx_pragma: Option<String>,
    /// `languageOptions.parserOptions.ecmaFeatures.globalReturn`.
    pub global_return: bool,
    /// `languageOptions.parserOptions.ecmaFeatures.impliedStrict`: all code is strict.
    pub implied_strict: bool,
    /// The fragment factory that TSX fragments implicitly reference (`jsxFragmentName`).
    pub jsx_fragment_name: Option<String>,
    /// `emitDecoratorMetadata` and `experimentalDecorators` are both on: decorated classes
    /// reference their parameter and property types as values.
    pub decorator_metadata: bool,
    /// `parserOptions.isolatedDeclarations` (read by rules, not by the parser).
    pub isolated_declarations: bool,
    /// The config's `settings` object.
    pub settings: Arc<serde_json::Value>,
    /// TypeScript lib globals typescript-eslint's scope analysis defines (`None` unless the
    /// file is parsed by the TypeScript parser). See [`crate::lib_globals`].
    pub lib: Option<Arc<LibGlobals>>,
    /// Set when the lib comes from the file's tsconfig (`parserOptions.project` /
    /// `projectService`); the engine resolves it per file into `lib`.
    pub project_lib: Option<Arc<ProjectLib>>,
    /// ESLint's `cwd` (the CLI's working directory): `unicorn/filename-case` checks the
    /// directory names of a file's path relative to it. `None` checks the filename only.
    pub cwd: Option<Arc<str>>,
}

impl LintEnv {
    /// A global the configuration or the TypeScript lib defines (for `builtinGlobals`).
    pub fn is_builtin_global(&self, name: &str) -> bool {
        self.globals.contains(name) || self.lib.as_ref().is_some_and(|l| l.is_global(name))
    }
}

impl Default for LintEnv {
    fn default() -> Self {
        LintEnv {
            source_type: SourceType::Module,
            ecma_version: crate::globals::LATEST_ECMA_VERSION,
            globals: Arc::new(crate::globals::builtin_globals(
                crate::globals::LATEST_ECMA_VERSION,
            )),
            typescript: false,
            jsx_pragma: None,
            global_return: false,
            implied_strict: false,
            jsx_fragment_name: None,
            decorator_metadata: false,
            isolated_declarations: false,
            settings: Arc::new(serde_json::Value::Null),
            lib: None,
            project_lib: None,
            cwd: None,
        }
    }
}
