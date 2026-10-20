//! Transform options, derived from tsconfig `CompilerOptions` plus the file's module format.

use ts_r_config::{CompilerOptions, JsxEmit, ModuleDetection, ModuleKind, ScriptTarget};

/// The module syntax to emit for one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModuleFormat {
    /// ES modules. `import x = require()` is rewritten with `createRequire` when
    /// `node_require` is set (node16/nodenext), and dropped otherwise (tsc reports TS1202).
    Esm { node_require: bool },
    /// CommonJS: `require`/`exports`, `__esModule`, interop helpers.
    CommonJs,
    /// `module: preserve`: ES syntax stays; `import x = require()` → `const x = require()`,
    /// `export =` → `module.exports =`.
    Preserve,
}

/// JSX emit mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JsxMode {
    /// Keep JSX (`jsx: preserve` / `react-native`).
    Preserve,
    /// `React.createElement` (`jsx: react`).
    Classic,
    /// `react/jsx-runtime` (`jsx: react-jsx`).
    Automatic,
    /// `react/jsx-dev-runtime` with source locations (`jsx: react-jsxdev`).
    AutomaticDev,
}

/// Everything the transforms need to know. Build with [`TransformOptions::from_compiler_options`]
/// or start from [`TransformOptions::default`] (ESNext-ish defaults matching tsc 7 with an
/// empty tsconfig and `jsx: react-jsx`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TransformOptions {
    pub target: ScriptTarget,
    pub module: ModuleFormat,
    /// `moduleDetection: force` (every file is a module).
    pub force_module: bool,
    /// `moduleDetection: legacy` (only import/export make a module).
    pub legacy_module_detection: bool,
    pub use_define_for_class_fields: bool,
    pub experimental_decorators: bool,
    pub emit_decorator_metadata: bool,
    pub jsx: JsxMode,
    /// `jsxFactory` (classic), default `React.createElement`.
    pub jsx_factory: String,
    /// `jsxFragmentFactory` (classic), default `React.Fragment`.
    pub jsx_fragment_factory: String,
    /// `jsxImportSource` (automatic), default `react`.
    pub jsx_import_source: String,
    /// Path written into `_jsxFileName` (react-jsxdev).
    pub jsx_file_name: String,
    pub verbatim_module_syntax: bool,
    pub isolated_modules: bool,
    pub preserve_const_enums: bool,
    pub rewrite_relative_import_extensions: bool,
    /// Import helpers from `tslib` instead of inlining them.
    pub import_helpers: bool,
    /// `noEmitHelpers`: call helpers but don't define them (a runtime provides them).
    pub no_emit_helpers: bool,
    /// `alwaysStrict` (via `strict`): add `"use strict"` to scripts and CommonJS output.
    pub always_strict: bool,
    /// `jsx: preserve` was set explicitly (`.tsx` imports are rewritten to `.jsx`).
    pub jsx_preserve_explicit: bool,
    /// `strictNullChecks` (decorator metadata serializes `T | null` as `Object` when set).
    pub strict_null_checks: bool,
    /// The configured `module` is CommonJS. tsc then also adds `"use strict"` to files it
    /// emits as ES modules (`.mts`), observed: `"use strict"; export {};`.
    pub cjs_module_option: bool,
    /// `module` is `node16`/`node18`/`node20`/`nodenext`: CommonJS files keep `import()`
    /// (Node.js supports it there; observed in tsc 7).
    pub node_module: bool,
    /// The file is `.cts`/`.cjs`: under `module: preserve` it gets no `export {};` marker
    /// (it is CommonJS to Node.js).
    pub cjs_extension: bool,
    /// `module: preserve` still adds the `export {};` marker to a module whose import and
    /// export syntax was all erased (default: the bundler relies on it). tsc 7 never adds
    /// it under `preserve` (observed); program emit (`ts_r_emit`) turns this off.
    pub preserve_module_marker: bool,
    /// No `target` set, or one below ES2021: tsc 7 prints a numeric literal with `_`
    /// separators by its value (`10_000` → `10000`, `0xFF_FF` → `65535`; observed). Others
    /// keep their source text.
    pub strip_numeric_separators: bool,
}

impl Default for TransformOptions {
    fn default() -> Self {
        TransformOptions {
            target: ScriptTarget::ES2025,
            module: ModuleFormat::Esm {
                node_require: false,
            },
            force_module: false,
            legacy_module_detection: false,
            use_define_for_class_fields: true,
            experimental_decorators: false,
            emit_decorator_metadata: false,
            jsx: JsxMode::Automatic,
            jsx_factory: "React.createElement".to_owned(),
            jsx_fragment_factory: "React.Fragment".to_owned(),
            jsx_import_source: "react".to_owned(),
            jsx_file_name: String::new(),
            verbatim_module_syntax: false,
            isolated_modules: false,
            preserve_const_enums: false,
            rewrite_relative_import_extensions: false,
            import_helpers: false,
            no_emit_helpers: false,
            always_strict: true,
            jsx_preserve_explicit: false,
            strict_null_checks: true,
            cjs_module_option: false,
            node_module: false,
            cjs_extension: false,
            preserve_module_marker: true,
            strip_numeric_separators: false,
        }
    }
}

impl TransformOptions {
    /// Options for one file. `cjs_file` says whether a `node16`/`nodenext` file is CommonJS
    /// (from its extension and the nearest `package.json` `type`; see
    /// `ts_r_resolver::Resolver::file_format`). It is ignored for other `module` values.
    /// `path` is used for `.mts`/`.cts` and the JSX dev file name.
    pub fn from_compiler_options(o: &CompilerOptions, path: &str, cjs_file: bool) -> Self {
        let e = o.effective();
        let lower = path.to_ascii_lowercase();
        let ext_cjs = lower.ends_with(".cts") || lower.ends_with(".cjs");
        let ext_esm = lower.ends_with(".mts") || lower.ends_with(".mjs");
        // `.cts`/`.cjs` are always CommonJS and `.mts`/`.mjs` always ES modules, whatever
        // `module` says (observed: `module: esnext` emits `exports.x` for a `.cts`), except
        // under `module: preserve`, which keeps every file's syntax.
        let cjs_module_option = matches!(
            e.module(),
            ModuleKind::CommonJS | ModuleKind::AMD | ModuleKind::UMD | ModuleKind::System
        );
        let module = match e.module() {
            ModuleKind::Preserve => ModuleFormat::Preserve,
            m if m.is_node() => {
                if ext_cjs || (cjs_file && !ext_esm) {
                    ModuleFormat::CommonJs
                } else {
                    ModuleFormat::Esm { node_require: true }
                }
            }
            _ if ext_esm => ModuleFormat::Esm {
                node_require: false,
            },
            _ if ext_cjs || cjs_module_option => ModuleFormat::CommonJs,
            _ => ModuleFormat::Esm {
                node_require: false,
            },
        };
        let flag = |v: Option<bool>| v.unwrap_or(false);
        let jsx = match e.jsx() {
            None | Some(JsxEmit::Preserve) | Some(JsxEmit::ReactNative) => JsxMode::Preserve,
            Some(JsxEmit::React) => JsxMode::Classic,
            Some(JsxEmit::ReactJsx) => JsxMode::Automatic,
            Some(JsxEmit::ReactJsxDev) => JsxMode::AutomaticDev,
        };
        let react_ns = o.react_namespace().unwrap_or("React");
        TransformOptions {
            target: e.target(),
            module,
            // `auto` makes `.mts`/`.cts`/`.mjs`/`.cjs` files modules, and under `node16`+ also
            // files whose package.json says `"type": "module"` (observed).
            force_module: match e.module_detection() {
                ModuleDetection::Force => true,
                ModuleDetection::Auto => ext_cjs || ext_esm || (e.module().is_node() && !cjs_file),
                ModuleDetection::Legacy => false,
            },
            legacy_module_detection: e.module_detection() == ModuleDetection::Legacy,
            use_define_for_class_fields: e.use_define_for_class_fields(),
            experimental_decorators: flag(o.experimental_decorators()),
            emit_decorator_metadata: flag(o.emit_decorator_metadata()),
            jsx,
            jsx_factory: o
                .jsx_factory()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{react_ns}.createElement")),
            jsx_fragment_factory: o
                .jsx_fragment_factory()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{react_ns}.Fragment")),
            jsx_import_source: e.jsx_import_source().to_owned(),
            jsx_file_name: path.to_owned(),
            verbatim_module_syntax: flag(o.verbatim_module_syntax()),
            isolated_modules: e.isolated_modules(),
            preserve_const_enums: e.preserve_const_enums(),
            rewrite_relative_import_extensions: flag(o.rewrite_relative_import_extensions()),
            import_helpers: flag(o.import_helpers()),
            no_emit_helpers: flag(o.no_emit_helpers()),
            always_strict: e.strict_flag(ts_r_config::StrictFlag::AlwaysStrict),
            jsx_preserve_explicit: e.jsx() == Some(JsxEmit::Preserve),
            strict_null_checks: e.strict_flag(ts_r_config::StrictFlag::StrictNullChecks),
            cjs_module_option,
            node_module: e.module().is_node(),
            cjs_extension: ext_cjs,
            preserve_module_marker: true,
            strip_numeric_separators: o.target().is_none_or(|t| t < ScriptTarget::ES2021),
        }
    }

    /// Native `accessor` fields and decorators (no lowering) only for ESNext.
    pub fn native_decorators(&self) -> bool {
        self.target >= ScriptTarget::ESNext
    }

    /// TC39 decorators are kept native only for ESNext with `useDefineForClassFields`; tsc
    /// lowers them otherwise.
    pub fn lower_es_decorators(&self) -> bool {
        let native = self.native_decorators() && self.use_define_for_class_fields;
        !self.experimental_decorators && !native
    }

    pub fn is_cjs(&self) -> bool {
        self.module == ModuleFormat::CommonJs
    }

    /// A stable string for cache keys.
    pub fn cache_key(&self) -> String {
        format!("{self:?}")
    }
}

impl TransformOptions {
    /// Options from a `compilerOptions` JSON object (e.g. `{"module": "commonjs"}`), for tests
    /// and tools. Unknown or invalid options are ignored, as with a tsconfig.
    pub fn from_json(compiler_options: &str, path: &str) -> Self {
        let cjs_file = path.ends_with(".cts") || path.ends_with(".cjs");
        Self::from_json_format(compiler_options, path, cjs_file)
    }

    /// [`TransformOptions::from_json`] with the file's CommonJS-ness for `node16`/`nodenext`.
    pub fn from_json_format(compiler_options: &str, path: &str, cjs_file: bool) -> Self {
        let mut fs = ts_r_config::MemoryFs::new();
        let config = format!("{{\"compilerOptions\": {compiler_options}, \"files\": []}}");
        fs.add_file("/tsconfig.json", &config);
        let parsed = ts_r_config::load_tsconfig(&fs, "/tsconfig.json");
        Self::from_compiler_options(&parsed.options, path, cjs_file)
    }
}
