//! Resolver settings derived from compiler options.

use ts_r_config::{CompilerOptions, ModuleKind, ModuleResolution, PathsMap, path};

/// Everything module resolution reads from the compiler options, with tsc 7 defaults
/// applied. Build it with [`ResolverOptions::from_compiler_options`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverOptions {
    /// The algorithm actually run: `Node16`, `NodeNext` or `Bundler` (TS 7 removed
    /// `node10`/`classic`; they behave like `bundler`).
    pub algorithm: ModuleResolution,
    /// `moduleResolution` was set explicitly (only the trace says so).
    pub module_resolution_explicit: bool,
    /// The effective `module` kind (decides the resolution mode of each import).
    pub module: ModuleKind,
    /// `paths`, if set.
    pub paths: Option<PathsMap>,
    /// Directory `paths` substitutions are relative to (the config that set `paths`).
    pub paths_base: Option<String>,
    /// `rootDirs` (absolute).
    pub root_dirs: Vec<String>,
    /// `typeRoots` (absolute). `None` means the default `node_modules/@types` ancestors.
    pub type_roots: Option<Vec<String>>,
    /// `moduleSuffixes`, default `[""]`.
    pub module_suffixes: Vec<String>,
    /// `customConditions`.
    pub custom_conditions: Vec<String>,
    /// `resolveJsonModule`.
    pub resolve_json_module: bool,
    /// `resolvePackageJsonExports`.
    pub resolve_package_json_exports: bool,
    /// `resolvePackageJsonImports`.
    pub resolve_package_json_imports: bool,
    /// `preserveSymlinks`.
    pub preserve_symlinks: bool,
    /// Directory of the tsconfig (default type roots and inferred type references start
    /// here). `None` without a config: the file system's cwd is used.
    pub config_dir: Option<String>,
    /// Sources and outputs of referenced projects (`references`): a source resolves to its
    /// declaration output ([`crate::references`]). `None` without references.
    pub project_references: Option<std::sync::Arc<crate::references::ProjectReferences>>,
    /// `outDir`, `declarationDir` (absolute) and the root directory their outputs mirror
    /// (`rootDir`, else the config directory): an `exports`/`imports` target inside them
    /// maps back to its source.
    pub out_dir: Option<String>,
    pub declaration_dir: Option<String>,
    pub root_dir: Option<String>,
    /// `allowJs` (or `checkJs`): JavaScript sources count when mapping outputs back.
    pub allow_js: bool,
    /// `allowArbitraryExtensions`: the resolver finds `x.d.html.ts` for `./x.html` either
    /// way; the program only links it with this set (or from a declaration file).
    pub allow_arbitrary_extensions: bool,
}

impl Default for ResolverOptions {
    fn default() -> Self {
        Self::from_compiler_options(&CompilerOptions::new())
    }
}

impl ResolverOptions {
    /// Derives resolver settings from (merged) compiler options.
    pub fn from_compiler_options(o: &CompilerOptions) -> Self {
        let e = o.effective();
        let config_dir = o
            .config_file_path
            .as_deref()
            .map(|p| path::directory_of(p).to_owned());
        let root_dir = o
            .root_dir()
            .map(str::to_owned)
            .or_else(|| config_dir.clone());
        Self {
            algorithm: e.resolution_algorithm(),
            module_resolution_explicit: o.module_resolution().is_some(),
            module: e.module(),
            paths: o.paths().cloned(),
            paths_base: o.paths_base_path.clone().or_else(|| config_dir.clone()),
            root_dirs: o.root_dirs().map(<[String]>::to_vec).unwrap_or_default(),
            type_roots: o.type_roots().map(<[String]>::to_vec),
            module_suffixes: e.module_suffixes(),
            custom_conditions: e.custom_conditions().to_vec(),
            resolve_json_module: e.resolve_json_module(),
            resolve_package_json_exports: e.resolve_package_json_exports(),
            resolve_package_json_imports: e.resolve_package_json_imports(),
            preserve_symlinks: e.preserve_symlinks(),
            config_dir,
            project_references: None,
            out_dir: o.out_dir().map(str::to_owned),
            declaration_dir: o.declaration_dir().map(str::to_owned),
            root_dir,
            allow_js: e.allow_js(),
            allow_arbitrary_extensions: e.allow_arbitrary_extensions(),
        }
    }

    /// True for `node16`/`nodenext`, where ESM-mode imports need full file extensions.
    pub fn is_node_algorithm(&self) -> bool {
        matches!(
            self.algorithm,
            ModuleResolution::Node16 | ModuleResolution::NodeNext
        )
    }
}
