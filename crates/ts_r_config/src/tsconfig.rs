//! tsconfig.json loading: parsing, `extends` chains, merging and file discovery.

use std::collections::HashMap;
use std::sync::Arc;

use crate::diagnostic::ConfigDiagnostic;
use crate::enums::{
    JsxEmit, LIBS, ModuleDetection, ModuleKind, ModuleResolution, NewLine, PollingKind,
    ScriptTarget, WatchDirectoryKind, WatchFileKind, lib_file_name,
};
use crate::fs::FileSystem;
use crate::glob::{self, MatchInput};
use crate::jsonc::{self, Value, ValueKind};
use crate::options::{
    CompilerOptions, OptionDecl, OptionId, OptionType, OptionValue, Origin, PathsMap, find_option,
    find_option_ignore_case, flags,
};
use crate::path;

/// The `${configDir}` template variable.
pub const CONFIG_DIR_TEMPLATE: &str = "${configDir}";

/// A `references` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectReference {
    /// `path` as written.
    pub path: String,
    /// Absolute path of the referenced tsconfig file (`<dir>/tsconfig.json` for directories).
    pub config_path: String,
    /// `circular`.
    pub circular: bool,
    /// `prepend` (removed in TS 7; parsed and ignored).
    pub prepend: bool,
    /// Span of the reference object in the config that lists it (TS6306, TS6310).
    pub span: (u32, u32),
}

/// `watchOptions`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchOptions {
    /// `watchFile`.
    pub watch_file: Option<WatchFileKind>,
    /// `watchDirectory`.
    pub watch_directory: Option<WatchDirectoryKind>,
    /// `fallbackPolling`.
    pub fallback_polling: Option<PollingKind>,
    /// `synchronousWatchDirectory`.
    pub synchronous_watch_directory: Option<bool>,
    /// `excludeDirectories` (absolute).
    pub exclude_directories: Option<Vec<String>>,
    /// `excludeFiles` (absolute).
    pub exclude_files: Option<Vec<String>>,
}

/// `typeAcquisition`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypeAcquisition {
    /// `enable`.
    pub enable: Option<bool>,
    /// `include`.
    pub include: Option<Vec<String>>,
    /// `exclude`.
    pub exclude: Option<Vec<String>>,
    /// `disableFilenameBasedTypeAcquisition`.
    pub disable_filename_based_type_acquisition: Option<bool>,
}

/// A `files`/`include`/`exclude` list together with the config that defined it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecList {
    /// Specs as written (after `${configDir}` substitution).
    pub specs: Vec<String>,
    /// Directory of the config file that defined the list.
    pub base_dir: String,
    /// Config file that defined the list.
    pub file: Arc<str>,
    /// Span of the list's value.
    pub span: (u32, u32),
    /// Entries that are not strings (TS5024): their index in the written list and their JSON
    /// text. They name no files, but the list is not empty and tsc shows them in TS18003.
    pub invalid: Vec<(usize, String)>,
}

impl SpecList {
    /// Specs as tsc displays them relative to the leaf config directory `leaf_dir`
    /// (`../base/x/**/*` for specs inherited from `../base/tsconfig.json`).
    pub fn display_specs(&self, leaf_dir: &str, case_sensitive: bool) -> Vec<String> {
        let rel = path::relative(leaf_dir, &self.base_dir, case_sensitive);
        self.specs
            .iter()
            .map(|s| {
                if path::is_rooted(s) || rel.is_empty() || s.starts_with(CONFIG_DIR_TEMPLATE) {
                    s.clone()
                } else if path::is_rooted(&rel) {
                    path::combine(&rel, s)
                } else {
                    format!("{rel}/{s}")
                }
            })
            .collect()
    }

    /// Specs as absolute, normalised paths.
    pub fn absolute_specs(&self) -> Vec<String> {
        self.specs
            .iter()
            .map(|s| path::resolve(&self.base_dir, s))
            .collect()
    }
}

/// A fully loaded tsconfig: merged options, root files and diagnostics.
#[derive(Debug, Clone)]
pub struct ParsedConfig {
    /// Absolute path of the config file.
    pub config_path: String,
    /// Directory of the config file.
    pub config_dir: String,
    /// Merged compiler options.
    pub options: CompilerOptions,
    /// Root file names (absolute): `files` entries, then `include` matches.
    pub file_names: Vec<String>,
    /// Effective `files` list (`None` when not specified).
    pub files: Option<SpecList>,
    /// Effective `include` list (`None` when not specified; the default is `**/*`).
    pub include: Option<SpecList>,
    /// Effective `exclude` list (`None` when not specified).
    pub exclude: Option<SpecList>,
    /// Absolute exclude paths used when `exclude` is not specified (`outDir`,
    /// `declarationDir`).
    pub default_excludes: Vec<String>,
    /// Project references (own only; not inherited).
    pub references: Vec<ProjectReference>,
    /// The config has a `references` key (an empty list too): with no inputs it is a
    /// container, not TS18003 (observed: `tsbuild/solution/when-solution-is-referenced-
    /// indirectly`).
    pub declares_references: bool,
    /// Merged `watchOptions`.
    pub watch_options: Option<WatchOptions>,
    /// Merged `typeAcquisition`.
    pub type_acquisition: Option<TypeAcquisition>,
    /// `compileOnSave`.
    pub compile_on_save: Option<bool>,
    /// Every config file in the `extends` chain (absolute, resolved), for watchers.
    pub extended_config_paths: Vec<String>,
    /// Parse, option and file-discovery diagnostics, in file/position order.
    pub diagnostics: Vec<ConfigDiagnostic>,
    /// Span of the leaf config's `compilerOptions` key, if present.
    pub compiler_options_key: Option<(u32, u32)>,
    /// Files named on the command line, no tsconfig.json ([`ConfigLoader::command_line`]):
    /// `config_path` names no file that was read.
    pub command_line: bool,
    case_sensitive: bool,
}

impl ParsedConfig {
    /// Whether paths were compared case-sensitively while loading.
    pub fn case_sensitive(&self) -> bool {
        self.case_sensitive
    }

    /// `include` specs as tsc reports them (`["**/*"]` by default when `files` is absent).
    pub fn include_display(&self) -> Option<Vec<String>> {
        match (&self.include, &self.files) {
            (Some(i), _) => Some(i.display_specs(&self.config_dir, self.case_sensitive)),
            (None, None) => Some(vec!["**/*".to_owned()]),
            (None, Some(_)) => None,
        }
    }

    /// Program-level option diagnostics (removed options, invalid combinations); see
    /// [`crate::verify::verify_options`].
    pub fn option_diagnostics(&self) -> Vec<ConfigDiagnostic> {
        crate::verify::verify_options(
            &self.options,
            self.compiler_options_key
                .map(|s| (self.config_path.as_str(), s)),
        )
    }

    /// TS5055 for JavaScript inputs whose output would overwrite them (`allowJs` without
    /// `outDir`, emitting).
    pub fn emit_overwrite_diagnostics(&self) -> Vec<ConfigDiagnostic> {
        let o = &self.options;
        let e = o.effective();
        if e.no_emit() || o.emit_declaration_only() == Some(true) || o.out_dir().is_some() {
            return Vec::new();
        }
        let preserve_jsx = o.jsx() == Some(crate::enums::JsxEmit::Preserve);
        self.file_names
            .iter()
            .filter(|f| {
                let n = path::file_name(f);
                path::ext::has(n, ".js")
                    || path::ext::has(n, ".mjs")
                    || path::ext::has(n, ".cjs")
                    || (preserve_jsx && path::ext::has(n, ".jsx"))
            })
            .map(|f| {
                ConfigDiagnostic::global(
                    5055,
                    format!("Cannot write file '{f}' because it would overwrite input file."),
                )
            })
            .collect()
    }

    /// Every config diagnostic tsc reports when building this project: parse and
    /// file-discovery diagnostics plus [`Self::option_diagnostics`] and
    /// [`Self::emit_overwrite_diagnostics`], sorted.
    pub fn all_diagnostics(&self) -> Vec<ConfigDiagnostic> {
        let mut all = self.diagnostics.clone();
        all.extend(self.option_diagnostics());
        all.extend(self.emit_overwrite_diagnostics());
        sort_and_dedupe(&mut all);
        all
    }

    /// `exclude` specs as tsc reports them (absolute defaults when not specified).
    pub fn exclude_display(&self) -> Vec<String> {
        match &self.exclude {
            Some(e) => e.display_specs(&self.config_dir, self.case_sensitive),
            None => self.default_excludes.clone(),
        }
    }
}

#[derive(Debug, Clone, Default)]
enum Field {
    #[default]
    Absent,
    Null,
    Specs(SpecList),
}

#[derive(Debug, Clone)]
struct RawRef {
    path: String,
    circular: bool,
    prepend: bool,
    span: (u32, u32),
}

/// One config file, parsed but not merged.
#[derive(Debug, Clone, Default)]
struct RawFile {
    path: String,
    dir: String,
    diagnostics: Vec<ConfigDiagnostic>,
    options: Vec<(OptionId, Option<OptionValue>, Origin)>,
    sets_paths: Option<bool>,
    compiler_options_key: Option<(u32, u32)>,
    files: Field,
    include: Field,
    exclude: Field,
    references: Vec<RawRef>,
    /// A `references` key is present.
    declares_references: bool,
    extends: Vec<(String, (u32, u32))>,
    compile_on_save: Option<bool>,
    watch: Option<WatchOptions>,
    type_acquisition: Option<TypeAcquisition>,
}

/// Merged state of an `extends` chain.
#[derive(Debug, Clone, Default)]
struct Merged {
    options: CompilerOptions,
    files: Option<Option<SpecList>>,
    include: Option<Option<SpecList>>,
    exclude: Option<Option<SpecList>>,
    compile_on_save: Option<bool>,
    watch: Option<WatchOptions>,
    type_acquisition: Option<TypeAcquisition>,
    chain: Vec<String>,
}

/// Loads tsconfig files, caching parsed files (so shared bases are read once).
pub struct ConfigLoader<'fs> {
    fs: &'fs dyn FileSystem,
    raw_cache: HashMap<String, Arc<RawFile>>,
    /// Completed merges of extended configs (before rebasing), with the diagnostics they
    /// produced, so a base reached along several paths (a diamond of `extends`) is merged once.
    merged_cache: HashMap<String, (Merged, Vec<ConfigDiagnostic>)>,
}

impl std::fmt::Debug for ConfigLoader<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigLoader")
            .field("cached", &self.raw_cache.len())
            .finish()
    }
}

/// Loads and fully resolves the tsconfig at `config_path`.
pub fn load_tsconfig(fs: &dyn FileSystem, config_path: &str) -> ParsedConfig {
    ConfigLoader::new(fs).load(config_path)
}

impl<'fs> ConfigLoader<'fs> {
    /// A loader over `fs`.
    pub fn new(fs: &'fs dyn FileSystem) -> Self {
        Self {
            fs,
            raw_cache: HashMap::new(),
            merged_cache: HashMap::new(),
        }
    }

    fn raw(&mut self, file: &str) -> Option<Arc<RawFile>> {
        let key = path::key(file, self.fs.case_sensitive());
        if let Some(r) = self.raw_cache.get(&key) {
            return Some(r.clone());
        }
        let text = self.fs.read_file(file)?;
        let raw = Arc::new(parse_raw(&text, file));
        self.raw_cache.insert(key, raw.clone());
        Some(raw)
    }

    /// Loads the config at `config_path` (absolute or relative to the fs cwd).
    pub fn load(&mut self, config_path: &str) -> ParsedConfig {
        self.load_with_overrides(config_path, |_| {})
    }

    /// Loads a config, applying option overrides after inheritance and before file discovery.
    pub fn load_with_overrides(
        &mut self,
        config_path: &str,
        overrides: impl FnOnce(&mut CompilerOptions),
    ) -> ParsedConfig {
        let config_path = path::resolve(&self.fs.cwd(), &path::normalize_slashes(config_path));
        let config_dir = path::directory_of(&config_path).to_owned();
        let cs = self.fs.case_sensitive();
        let mut diags = Vec::new();
        let Some(raw) = self.raw(&config_path) else {
            diags.push(ConfigDiagnostic::global(
                5083,
                format!("Cannot read file '{config_path}'."),
            ));
            return ParsedConfig {
                config_path,
                config_dir,
                options: CompilerOptions::new(),
                file_names: Vec::new(),
                files: None,
                include: None,
                exclude: None,
                default_excludes: Vec::new(),
                references: Vec::new(),
                declares_references: false,
                watch_options: None,
                type_acquisition: None,
                compile_on_save: None,
                extended_config_paths: Vec::new(),
                diagnostics: diags,
                compiler_options_key: None,
                command_line: false,
                case_sensitive: cs,
            };
        };
        let mut stack = vec![path::key(&config_path, cs)];
        let mut merged = self.merge_file(&raw, &mut stack, &mut diags);
        merged.options.config_file_path = Some(config_path.clone());

        // ${configDir} substitution against the leaf config directory.
        substitute_config_dir(&mut merged.options, &config_dir);
        overrides(&mut merged.options);
        // TS18003 quotes `include` and `exclude` as written, before substitution.
        let written_include = merged.include.clone().flatten();
        let written_exclude = merged.exclude.clone().flatten();
        let fix_specs = |s: &mut Option<Option<SpecList>>| {
            if let Some(Some(list)) = s {
                for spec in &mut list.specs {
                    if let Some(rest) = spec.strip_prefix(CONFIG_DIR_TEMPLATE) {
                        *spec = format!("{config_dir}{rest}");
                    }
                }
            }
        };
        fix_specs(&mut merged.files);
        fix_specs(&mut merged.include);
        fix_specs(&mut merged.exclude);
        if let Some(w) = &mut merged.watch {
            for list in [&mut w.exclude_directories, &mut w.exclude_files]
                .into_iter()
                .flatten()
            {
                for p in list.iter_mut() {
                    if let Some(rest) = p.strip_prefix(CONFIG_DIR_TEMPLATE) {
                        *p = path::normalize(&format!("{config_dir}{rest}"));
                    }
                }
            }
        }

        let files = merged.files.clone().flatten();
        let include = merged.include.clone().flatten();
        let exclude = merged.exclude.clone().flatten();

        // References.
        let references: Vec<ProjectReference> = raw
            .references
            .iter()
            .map(|r| {
                let p = path::resolve(&config_dir, &r.path);
                let config = if self.fs.file_exists(&p) || p.to_ascii_lowercase().ends_with(".json")
                {
                    p.clone()
                } else {
                    path::combine(&p, "tsconfig.json")
                };
                if !self.fs.file_exists(&config) {
                    diags.push(ConfigDiagnostic::at(
                        6053,
                        format!("File '{p}' not found."),
                        &config_path,
                        r.span,
                    ));
                }
                ProjectReference {
                    path: r.path.clone(),
                    config_path: config,
                    circular: r.circular,
                    prepend: r.prepend,
                    span: r.span,
                }
            })
            .collect();

        // Default excludes.
        let eff = merged.options.effective();
        let mut default_excludes = Vec::new();
        if exclude.is_none() {
            if let Some(o) = merged.options.out_dir() {
                default_excludes.push(o.to_owned());
            }
            if let Some(d) = merged.options.declaration_dir() {
                default_excludes.push(d.to_owned());
            }
        }

        // File discovery.
        let mut file_names: Vec<String> = Vec::new();
        if let Some(f) = &files {
            file_names.extend(f.absolute_specs());
        }
        let include_abs: Vec<String> = match (&include, &files) {
            (Some(i), _) => i.absolute_specs(),
            (None, None) => vec![path::combine(&config_dir, "**/*")],
            (None, Some(_)) => Vec::new(),
        };
        let exclude_abs: Vec<String> = match &exclude {
            Some(e) => e.absolute_specs(),
            None => default_excludes.clone(),
        };
        if !include_abs.is_empty() {
            let matched = glob::match_files(
                self.fs,
                &MatchInput {
                    include: &include_abs,
                    exclude: &exclude_abs,
                    allow_js: eff.allow_js(),
                    allow_json: eff.resolve_json_module(),
                },
            );
            file_names.extend(matched);
        }

        let mut parsed = ParsedConfig {
            config_path: config_path.clone(),
            config_dir: config_dir.clone(),
            options: merged.options,
            file_names,
            files,
            include,
            exclude,
            default_excludes,
            references,
            declares_references: raw.declares_references,
            watch_options: merged.watch,
            type_acquisition: merged.type_acquisition,
            compile_on_save: merged.compile_on_save,
            extended_config_paths: merged.chain,
            diagnostics: Vec::new(),
            compiler_options_key: raw.compiler_options_key,
            command_line: false,
            case_sensitive: cs,
        };

        // Empty inputs.
        // An empty `files` list is TS18002 unless references are listed; no inputs found is
        // TS18003 unless there is a `references` key at all (an empty list too; observed:
        // `tsbuild/solution/has-empty-files-diagnostic-*`, `when-solution-is-referenced-
        // indirectly`).
        if parsed.file_names.is_empty() && parsed.references.is_empty() {
            match (&parsed.files, &parsed.include) {
                (Some(f), None) if f.specs.is_empty() && f.invalid.is_empty() => {
                    diags.push(ConfigDiagnostic::at(
                        18002,
                        format!("The 'files' list in config file '{config_path}' is empty."),
                        &f.file,
                        f.span,
                    ))
                }
                // A `files` list with only non-string entries: TS5024 alone (observed).
                (Some(_), None) => {}
                _ if parsed.declares_references => {}
                _ => {
                    let inc = match (&written_include, &parsed.files) {
                        (Some(i), _) => i.display_specs(&parsed.config_dir, cs),
                        (None, None) => vec!["**/*".to_owned()],
                        (None, Some(_)) => Vec::new(),
                    };
                    let exc = match &written_exclude {
                        Some(e) => e.display_specs(&parsed.config_dir, cs),
                        None => parsed.default_excludes.clone(),
                    };
                    let shown = |items: Vec<String>, list: Option<&SpecList>| match list {
                        Some(l) if !l.invalid.is_empty() => json_array_with_raw(&items, &l.invalid),
                        _ => json_string_array(&items),
                    };
                    diags.push(ConfigDiagnostic::global(
                        18003,
                        format!(
                            "No inputs were found in config file '{config_path}'. Specified 'include' paths were '{}' and 'exclude' paths were '{}'.",
                            shown(inc, parsed.include.as_ref()),
                            shown(exc, parsed.exclude.as_ref())
                        ),
                    ));
                }
            }
        }

        sort_and_dedupe(&mut diags);
        parsed.diagnostics = diags;
        parsed
    }

    /// A project of files named on the command line (`tsc a.ts b.ts`, or `--ignoreConfig`):
    /// default options with `overrides` applied, the files as roots in the order given
    /// (resolved against the fs cwd, missing ones included: the caller reports them), and
    /// `config_path` set to `<cwd>/tsconfig.json` without reading it. `files` holds the names
    /// as written.
    pub fn command_line(
        &mut self,
        names: &[String],
        overrides: impl FnOnce(&mut CompilerOptions),
    ) -> ParsedConfig {
        let cwd = path::normalize_slashes(&self.fs.cwd());
        let config_path = path::combine(&cwd, "tsconfig.json");
        let mut options = CompilerOptions::new();
        overrides(&mut options);
        let specs: Vec<String> = names.iter().map(|n| path::normalize_slashes(n)).collect();
        let file_names = specs.iter().map(|s| path::resolve(&cwd, s)).collect();
        let files = Some(SpecList {
            specs,
            base_dir: cwd.clone(),
            file: Arc::from(config_path.as_str()),
            span: (0, 0),
            invalid: Vec::new(),
        });
        ParsedConfig {
            config_path,
            config_dir: cwd,
            options,
            file_names,
            files,
            include: None,
            exclude: None,
            default_excludes: Vec::new(),
            references: Vec::new(),
            declares_references: false,
            watch_options: None,
            type_acquisition: None,
            compile_on_save: None,
            extended_config_paths: Vec::new(),
            diagnostics: Vec::new(),
            compiler_options_key: None,
            command_line: true,
            case_sensitive: self.fs.case_sensitive(),
        }
    }

    /// Merges `raw` with everything it extends.
    fn merge_file(
        &mut self,
        raw: &RawFile,
        stack: &mut Vec<String>,
        diags: &mut Vec<ConfigDiagnostic>,
    ) -> Merged {
        diags.extend(raw.diagnostics.iter().cloned());
        let cs = self.fs.case_sensitive();
        let mut merged = Merged::default();
        for (spec, span) in &raw.extends {
            let Some(target) = resolve_extends(self.fs, spec, &raw.dir) else {
                diags.push(ConfigDiagnostic::at(
                    6053,
                    format!("File '{spec}' not found."),
                    &raw.path,
                    *span,
                ));
                continue;
            };
            let key = path::key(&target, cs);
            if stack.contains(&key) {
                diags.push(ConfigDiagnostic::global(
                    18000,
                    "Circularity detected while resolving configuration: {0}",
                ));
                continue;
            }
            let mut base = if let Some((base, base_diags)) = self.merged_cache.get(&key) {
                diags.extend(base_diags.iter().cloned());
                base.clone()
            } else {
                let Some(base_raw) = self.raw(&target) else {
                    diags.push(ConfigDiagnostic::global(
                        5083,
                        format!("Cannot read file '{target}'."),
                    ));
                    continue;
                };
                stack.push(key.clone());
                let mut base_diags = Vec::new();
                let base = self.merge_file(&base_raw, stack, &mut base_diags);
                stack.pop();
                // Without a cycle below it, a merge does not depend on how it was reached.
                if !base_diags.iter().any(|d| d.code == 18000) {
                    self.merged_cache
                        .insert(key, (base.clone(), base_diags.clone()));
                }
                diags.extend(base_diags);
                base
            };
            // tsc rebases inherited specs one `extends` level at a time.
            for list in [&mut base.files, &mut base.include, &mut base.exclude]
                .into_iter()
                .flatten()
                .flatten()
            {
                list.specs = list.display_specs(&raw.dir, cs);
                list.base_dir = raw.dir.clone();
            }
            // Each config once, in first-seen order.
            for p in std::iter::once(&target).chain(&base.chain) {
                if !merged.chain.contains(p) {
                    merged.chain.push(p.clone());
                }
            }
            overlay_merged(&mut merged, base);
        }
        // Own values on top.
        for (id, value, origin) in &raw.options {
            merged.options.set(*id, value.clone(), Some(origin.clone()));
        }
        match raw.sets_paths {
            Some(true) => merged.options.paths_base_path = Some(raw.dir.clone()),
            Some(false) => merged.options.paths_base_path = None,
            None => {}
        }
        let field = |f: &Field| -> Option<Option<SpecList>> {
            match f {
                Field::Absent => None,
                Field::Null => Some(None),
                Field::Specs(s) => Some(Some(s.clone())),
            }
        };
        if let Some(f) = field(&raw.files) {
            merged.files = Some(f);
        }
        if let Some(f) = field(&raw.include) {
            merged.include = Some(f);
        }
        if let Some(f) = field(&raw.exclude) {
            merged.exclude = Some(f);
        }
        if raw.compile_on_save.is_some() {
            merged.compile_on_save = raw.compile_on_save;
        }
        if let Some(w) = &raw.watch {
            merged.watch = Some(merge_watch(merged.watch.take().unwrap_or_default(), w));
        }
        if let Some(t) = &raw.type_acquisition {
            merged.type_acquisition = Some(merge_type_acquisition(
                merged.type_acquisition.take().unwrap_or_default(),
                t,
            ));
        }
        merged
    }
}

fn overlay_merged(dst: &mut Merged, src: Merged) {
    for (id, v) in src.options.iter() {
        dst.options
            .set(id, Some(v.clone()), src.options.origin(id).cloned());
    }
    if src.options.is_set(OptionId::Paths) {
        dst.options.paths_base_path = src.options.paths_base_path.clone();
    }
    if src.files.is_some() {
        dst.files = src.files;
    }
    if src.include.is_some() {
        dst.include = src.include;
    }
    if src.exclude.is_some() {
        dst.exclude = src.exclude;
    }
    if src.compile_on_save.is_some() {
        dst.compile_on_save = src.compile_on_save;
    }
    if let Some(w) = src.watch {
        dst.watch = Some(merge_watch(dst.watch.take().unwrap_or_default(), &w));
    }
    if let Some(t) = src.type_acquisition {
        dst.type_acquisition = Some(merge_type_acquisition(
            dst.type_acquisition.take().unwrap_or_default(),
            &t,
        ));
    }
}

fn merge_watch(mut base: WatchOptions, over: &WatchOptions) -> WatchOptions {
    if over.watch_file.is_some() {
        base.watch_file = over.watch_file;
    }
    if over.watch_directory.is_some() {
        base.watch_directory = over.watch_directory;
    }
    if over.fallback_polling.is_some() {
        base.fallback_polling = over.fallback_polling;
    }
    if over.synchronous_watch_directory.is_some() {
        base.synchronous_watch_directory = over.synchronous_watch_directory;
    }
    if over.exclude_directories.is_some() {
        base.exclude_directories = over.exclude_directories.clone();
    }
    if over.exclude_files.is_some() {
        base.exclude_files = over.exclude_files.clone();
    }
    base
}

fn merge_type_acquisition(mut base: TypeAcquisition, over: &TypeAcquisition) -> TypeAcquisition {
    if over.enable.is_some() {
        base.enable = over.enable;
    }
    if over.include.is_some() {
        base.include = over.include.clone();
    }
    if over.exclude.is_some() {
        base.exclude = over.exclude.clone();
    }
    if over.disable_filename_based_type_acquisition.is_some() {
        base.disable_filename_based_type_acquisition = over.disable_filename_based_type_acquisition;
    }
    base
}

fn substitute_config_dir(opts: &mut CompilerOptions, dir: &str) {
    let ids: Vec<OptionId> = opts.iter().map(|(id, _)| id).collect();
    for id in ids {
        let decl = id.decl();
        let origin = opts.origin(id).cloned();
        let Some(v) = opts.get(id).cloned() else {
            continue;
        };
        // As tsc: the template stands for `./` relative to the config directory, so
        // `${configDir}..` is the parent directory (not `<dir>..`).
        let sub = |s: &str| -> Option<String> {
            s.strip_prefix(CONFIG_DIR_TEMPLATE)
                .map(|rest| path::normalize(&path::combine(dir, &format!("./{rest}"))))
        };
        let new = match (decl.ty, v) {
            (OptionType::Path, OptionValue::String(s)) => sub(&s).map(OptionValue::String),
            (OptionType::PathList, OptionValue::List(list)) => {
                if list.iter().any(|s| s.starts_with(CONFIG_DIR_TEMPLATE)) {
                    Some(OptionValue::List(
                        list.iter()
                            .map(|s| sub(s).unwrap_or_else(|| s.clone()))
                            .collect(),
                    ))
                } else {
                    None
                }
            }
            (OptionType::PathMap, OptionValue::Paths(map)) => {
                let changed = map
                    .0
                    .iter()
                    .any(|(_, subs)| subs.iter().any(|s| s.starts_with(CONFIG_DIR_TEMPLATE)));
                if changed {
                    Some(OptionValue::Paths(PathsMap(
                        map.0
                            .iter()
                            .map(|(k, subs)| {
                                (
                                    k.clone(),
                                    subs.iter()
                                        .map(|s| sub(s).unwrap_or_else(|| s.clone()))
                                        .collect(),
                                )
                            })
                            .collect(),
                    )))
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(n) = new {
            opts.set(id, Some(n), origin);
        }
    }
}

/// Resolves an `extends` specifier from directory `from_dir`.
pub fn resolve_extends(fs: &dyn FileSystem, spec: &str, from_dir: &str) -> Option<String> {
    let spec_n = path::normalize_slashes(spec);
    // `.` and `..` go through module resolution (observed): `<dir>.json`, then
    // `<dir>/tsconfig.json`. `./dir` and `../` are plain paths and fail (TS6053).
    if spec_n == "." || spec_n == ".." {
        let dir = path::resolve(from_dir, &spec_n);
        let with_json = format!("{dir}.json");
        if fs.file_exists(&with_json) {
            return Some(with_json);
        }
        let in_dir = path::combine(&dir, "tsconfig.json");
        return fs.file_exists(&in_dir).then_some(in_dir);
    }
    if path::is_rooted(&spec_n) || path::is_explicitly_relative(&spec_n) {
        // Like tsc: a missing `.json` path is returned as is (reading it then fails with
        // TS5083); a missing extensionless path gets `.json` appended or fails with TS6053.
        let candidate = path::resolve(from_dir, &spec_n);
        if fs.file_exists(&candidate) || candidate.to_ascii_lowercase().ends_with(".json") {
            return Some(candidate);
        }
        let with_json = format!("{candidate}.json");
        return fs.file_exists(&with_json).then_some(with_json);
    }
    // Package name (optionally with a subpath).
    let (pkg_name, subpath) = split_package_name(&spec_n)?;
    let mut dir = from_dir.to_owned();
    loop {
        if path::file_name(&dir) != "node_modules" {
            let pkg_dir = path::combine(&path::combine(&dir, "node_modules"), pkg_name);
            if fs.directory_exists(&pkg_dir)
                && let Some(found) = resolve_in_config_package(fs, &pkg_dir, subpath)
            {
                return Some(fs.realpath(&found));
            }
        }
        let parent = path::directory_of(&dir).to_owned();
        if parent == dir || parent.is_empty() {
            break;
        }
        dir = parent;
    }
    // No `node_modules` copy: a Yarn Plug'n'Play manifest may place the package (esbuild's
    // `TsconfigPackageJsonExportsYarnPnP`). tsc 7 has no PnP support and reports TS6053 here.
    let pnp = crate::pnp::PnpManifest::find(fs, from_dir)?;
    let pkg_dir = pnp.package_dir(pkg_name, &path::combine(from_dir, "tsconfig.json"))?;
    let found = resolve_in_config_package(fs, pkg_dir.trim_end_matches('/'), subpath)?;
    Some(fs.realpath(&found))
}

/// Splits `@scope/name/sub/path` into (`@scope/name`, `sub/path`).
pub fn split_package_name(spec: &str) -> Option<(&str, &str)> {
    let mut idx = spec.find('/');
    if spec.starts_with('@') {
        let first = idx?;
        idx = spec[first + 1..].find('/').map(|i| i + first + 1);
    }
    match idx {
        Some(i) => Some((&spec[..i], &spec[i + 1..])),
        None => Some((spec, "")),
    }
}

const EXTENDS_CONDITIONS: &[&str] = &["node", "require", "types", "default"];

fn resolve_in_config_package(fs: &dyn FileSystem, pkg_dir: &str, subpath: &str) -> Option<String> {
    let pkg_json_path = path::combine(pkg_dir, "package.json");
    let pkg = fs
        .read_file(&pkg_json_path)
        .and_then(|t| jsonc::parse(&t, &pkg_json_path).root);
    if let Some(pkg) = &pkg {
        if let Some(exports) = pkg.get("exports")
            && !exports.is_null()
        {
            let key = if subpath.is_empty() {
                ".".to_owned()
            } else {
                format!("./{subpath}")
            };
            let target = crate::exports::resolve_exports(exports, &key, EXTENDS_CONDITIONS)?;
            let file = path::resolve(pkg_dir, &target);
            return fs.file_exists(&file).then_some(file);
        }
        if subpath.is_empty()
            && let Some(tsconfig) = pkg.get("tsconfig").and_then(|v| v.as_str())
        {
            let file = path::resolve(pkg_dir, tsconfig);
            if fs.file_exists(&file) {
                return Some(file);
            }
            let with_json = format!("{file}.json");
            if fs.file_exists(&with_json) {
                return Some(with_json);
            }
            // A directory: its `tsconfig.json` (observed).
            let in_dir = path::combine(&file, "tsconfig.json");
            if fs.file_exists(&in_dir) {
                return Some(in_dir);
            }
        }
    }
    let base = if subpath.is_empty() {
        pkg_dir.to_owned()
    } else {
        path::resolve(pkg_dir, subpath)
    };
    if !subpath.is_empty() {
        if fs.file_exists(&base) {
            return Some(base);
        }
        let with_json = format!("{base}.json");
        if fs.file_exists(&with_json) {
            return Some(with_json);
        }
    }
    let in_dir = path::combine(&base, "tsconfig.json");
    fs.file_exists(&in_dir).then_some(in_dir)
}

fn json_string_array(items: &[String]) -> String {
    let mut out = String::from("[");
    for (i, s) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        jsonc::write_json_string(s, &mut out);
    }
    out.push(']');
    out
}

/// `items` as a JSON array with the raw JSON text of non-string entries put back at their
/// indices (`[1]`, `["a",2]`), as tsc shows lists in TS18003.
fn json_array_with_raw(items: &[String], raw: &[(usize, String)]) -> String {
    let mut parts: Vec<String> = items
        .iter()
        .map(|s| {
            let mut q = String::new();
            jsonc::write_json_string(s, &mut q);
            q
        })
        .collect();
    for (at, text) in raw {
        let at = (*at).min(parts.len());
        parts.insert(at, text.clone());
    }
    format!("[{}]", parts.join(","))
}

/// Sorts diagnostics like tsc: by file, position, span end, code and message (located ones
/// after global ones from the same
/// file) and removes exact duplicates.
pub fn sort_and_dedupe(diags: &mut Vec<ConfigDiagnostic>) {
    diags.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.span.map(|s| s.0).cmp(&b.span.map(|s| s.0)))
            .then_with(|| a.span.map(|s| s.1).cmp(&b.span.map(|s| s.1)))
            .then_with(|| a.code.cmp(&b.code))
            .then_with(|| a.message.cmp(&b.message))
    });
    diags.dedup();
}

// ---------------------------------------------------------------------------------------
// Parsing a single file
// ---------------------------------------------------------------------------------------

fn parse_raw(text: &str, file: &str) -> RawFile {
    let file_arc: Arc<str> = Arc::from(file);
    let dir = path::directory_of(file).to_owned();
    let parsed = jsonc::parse(text, file);
    let mut raw = RawFile {
        path: file.to_owned(),
        dir: dir.clone(),
        ..Default::default()
    };
    raw.diagnostics = parsed.diagnostics;
    let Some(root) = parsed.root else { return raw };
    if root.as_object().is_none() {
        raw.diagnostics.push(ConfigDiagnostic::global(
            5092,
            "The root value of a 'tsconfig.json' file must be an object.",
        ));
        return raw;
    }
    let mut d = Diags {
        file,
        out: &mut raw.diagnostics,
    };

    if let Some(m) = root.member("compilerOptions") {
        raw.compiler_options_key = Some(m.key_span);
        match &m.value.kind {
            ValueKind::Object(members) => {
                for member in members {
                    if let Some((id, value)) = convert_option(member, &dir, &mut d) {
                        if id == OptionId::Paths {
                            raw.sets_paths = Some(value.is_some());
                        }
                        raw.options.retain(|(i, _, _)| *i != id);
                        raw.options.push((
                            id,
                            value,
                            Origin {
                                file: file_arc.clone(),
                                key_span: member.key_span,
                                value_span: member.value.span,
                            },
                        ));
                    }
                }
            }
            ValueKind::Null => {}
            _ => d.type_error(&m.value, "compilerOptions", "object"),
        }
    }

    if let Some(m) = root.member("extends") {
        let v = &m.value;
        match &v.kind {
            ValueKind::String(s) => {
                if s.is_empty() {
                    d.at(
                        v.span,
                        18051,
                        "Compiler option 'extends' cannot be given an empty string.".into(),
                    );
                } else {
                    raw.extends.push((s.clone(), v.span));
                }
            }
            ValueKind::Array(items) => {
                for it in items {
                    match &it.kind {
                        ValueKind::String(s) if !s.is_empty() => {
                            raw.extends.push((s.clone(), it.span))
                        }
                        ValueKind::String(_) => d.at(
                            it.span,
                            18051,
                            "Compiler option 'extends' cannot be given an empty string.".into(),
                        ),
                        _ => d.type_error(it, "extends", "string"),
                    }
                }
            }
            ValueKind::Null => {}
            _ => d.type_error(v, "extends", "string or Array"),
        }
    }

    for (key, slot) in [("files", 0), ("include", 1), ("exclude", 2)] {
        let Some(m) = root.member(key) else { continue };
        let field = match &m.value.kind {
            ValueKind::Null => Field::Null,
            ValueKind::Array(items) => {
                let mut specs = Vec::new();
                let mut invalid = Vec::new();
                for (i, it) in items.iter().enumerate() {
                    match &it.kind {
                        ValueKind::String(s) => specs.push(path::normalize_slashes(s)),
                        _ => {
                            d.type_error(it, key, "string");
                            let raw = text
                                .get(it.span.0 as usize..it.span.1 as usize)
                                .unwrap_or_default();
                            invalid.push((i, raw.to_owned()));
                        }
                    }
                }
                Field::Specs(SpecList {
                    specs,
                    base_dir: dir.clone(),
                    file: file_arc.clone(),
                    span: m.value.span,
                    invalid,
                })
            }
            _ => {
                d.type_error(&m.value, key, "Array");
                Field::Absent
            }
        };
        match slot {
            0 => raw.files = field,
            1 => raw.include = field,
            _ => raw.exclude = field,
        }
    }

    if let Some(m) = root.member("references") {
        raw.declares_references = true;
        match &m.value.kind {
            ValueKind::Array(items) => {
                for it in items {
                    let Some(_) = it.as_object() else {
                        d.type_error(it, "references", "object");
                        continue;
                    };
                    match it.get("path") {
                        Some(Value {
                            kind: ValueKind::String(p),
                            ..
                        }) => raw.references.push(RawRef {
                            path: p.clone(),
                            circular: it
                                .get("circular")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false),
                            prepend: it.get("prepend").and_then(|v| v.as_bool()).unwrap_or(false),
                            span: it.span,
                        }),
                        Some(other) => d.type_error(other, "path", "string"),
                        None => {}
                    }
                }
            }
            ValueKind::Null => {}
            _ => d.type_error(&m.value, "references", "Array"),
        }
    }

    if let Some(v) = root.get("compileOnSave")
        && let Some(b) = v.as_bool()
    {
        raw.compile_on_save = Some(b);
    }

    if let Some(v) = root.get("watchOptions")
        && let Some(members) = v.as_object()
    {
        raw.watch = Some(parse_watch_options(members, &dir));
    }

    if let Some(v) = root.get("typeAcquisition")
        && let Some(members) = v.as_object()
    {
        raw.type_acquisition = Some(parse_type_acquisition(members, &mut d));
    }
    raw
}

struct Diags<'a> {
    file: &'a str,
    out: &'a mut Vec<ConfigDiagnostic>,
}

impl Diags<'_> {
    fn at(&mut self, span: (u32, u32), code: u32, msg: String) {
        self.out
            .push(ConfigDiagnostic::at(code, msg, self.file, span));
    }

    /// TS5024 anchored like tsc: at the token for literals, at the full start (including
    /// leading trivia) for invalid tokens.
    fn type_error(&mut self, v: &Value, name: &str, ty: &str) {
        let start = if v.kind == ValueKind::Invalid && !matches!(v.kind, ValueKind::Array(_)) {
            v.full_start
        } else {
            v.span.0
        };
        self.at(
            (start, v.span.1),
            5024,
            format!("Compiler option '{name}' requires a value of type {ty}."),
        );
    }
}

fn enum_error(d: &mut Diags<'_>, v: &Value, name: &str, valid: &[&str]) {
    let list = valid
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(", ");
    d.at(
        v.span,
        6046,
        format!("Argument for '--{name}' option must be: {list}."),
    );
}

/// Converts one `compilerOptions` member. Returns `None` on error (nothing recorded),
/// `Some((id, None))` for an explicit `null`.
fn convert_option(
    m: &jsonc::Member,
    dir: &str,
    d: &mut Diags<'_>,
) -> Option<(OptionId, Option<OptionValue>)> {
    let Some(decl) = find_option(&m.key) else {
        match find_option_ignore_case(&m.key) {
            Some(s) => d.at(
                m.key_span,
                5025,
                format!(
                    "Unknown compiler option '{}'. Did you mean '{}'?",
                    m.key, s.name
                ),
            ),
            None => d.at(
                m.key_span,
                5023,
                format!("Unknown compiler option '{}'.", m.key),
            ),
        }
        return None;
    };
    if decl.flags & flags::CMDLINE_ONLY != 0 {
        d.at(
            m.key_span,
            6266,
            format!(
                "Option '{}' can only be specified on command line.",
                decl.name
            ),
        );
        return None;
    }
    let v = &m.value;
    if v.is_null() {
        return Some((decl.id, None));
    }
    let value = convert_value(decl, v, dir, d)?;
    Some((decl.id, Some(value)))
}

fn resolve_path_value(dir: &str, s: &str) -> String {
    if s.starts_with(CONFIG_DIR_TEMPLATE) {
        path::normalize_slashes(s)
    } else {
        path::resolve(dir, s)
    }
}

fn convert_value(
    decl: &OptionDecl,
    v: &Value,
    dir: &str,
    d: &mut Diags<'_>,
) -> Option<OptionValue> {
    let name = decl.name;
    macro_rules! enum_value {
        ($ty:ty, $variant:ident) => {{
            match &v.kind {
                ValueKind::String(s) => match <$ty>::parse(s) {
                    Some(e) => Some(OptionValue::$variant(e)),
                    None => {
                        enum_error(d, v, name, <$ty>::VALID);
                        None
                    }
                },
                _ => {
                    d.type_error(v, name, "string");
                    None
                }
            }
        }};
    }
    match decl.ty {
        OptionType::Bool => match v.kind {
            ValueKind::Bool(b) => Some(OptionValue::Bool(b)),
            _ => {
                d.type_error(v, name, "boolean");
                None
            }
        },
        OptionType::Number => match v.kind {
            ValueKind::Number(n) => Some(OptionValue::Number(n)),
            _ => {
                d.type_error(v, name, "number");
                None
            }
        },
        OptionType::String => match &v.kind {
            ValueKind::String(s) => Some(OptionValue::String(s.clone())),
            _ => {
                d.type_error(v, name, "string");
                None
            }
        },
        OptionType::Path => match &v.kind {
            ValueKind::String(s) => Some(OptionValue::String(resolve_path_value(dir, s))),
            _ => {
                d.type_error(v, name, "string");
                None
            }
        },
        OptionType::StringList | OptionType::PathList | OptionType::Lib => {
            let Some(items) = v.as_array() else {
                d.type_error(v, name, "Array");
                return None;
            };
            let mut out = Vec::new();
            for it in items {
                match &it.kind {
                    ValueKind::String(s) => match decl.ty {
                        OptionType::PathList => out.push(resolve_path_value(dir, s)),
                        OptionType::Lib => {
                            if lib_file_name(s).is_some() {
                                out.push(s.to_ascii_lowercase());
                            } else {
                                let valid: Vec<&str> = LIBS.iter().map(|(n, _)| *n).collect();
                                enum_error(d, it, name, &valid);
                            }
                        }
                        _ => out.push(s.clone()),
                    },
                    ValueKind::Invalid => d.type_error(it, name, "Array"),
                    _ => d.type_error(it, name, "string"),
                }
            }
            Some(OptionValue::List(out))
        }
        OptionType::Target => {
            if !matches!(v.kind, ValueKind::String(_)) {
                d.type_error(v, name, "enum");
                return None;
            }
            enum_value!(ScriptTarget, Target)
        }
        OptionType::Module => {
            if !matches!(v.kind, ValueKind::String(_)) {
                d.type_error(v, name, "enum");
                return None;
            }
            enum_value!(ModuleKind, Module)
        }
        OptionType::ModuleResolution => {
            if !matches!(v.kind, ValueKind::String(_)) {
                d.type_error(v, name, "enum");
                return None;
            }
            enum_value!(ModuleResolution, ModuleResolution)
        }
        OptionType::ModuleDetection => {
            if !matches!(v.kind, ValueKind::String(_)) {
                d.type_error(v, name, "enum");
                return None;
            }
            enum_value!(ModuleDetection, ModuleDetection)
        }
        OptionType::Jsx => {
            if !matches!(v.kind, ValueKind::String(_)) {
                d.type_error(v, name, "enum");
                return None;
            }
            enum_value!(JsxEmit, Jsx)
        }
        OptionType::NewLine => {
            if !matches!(v.kind, ValueKind::String(_)) {
                d.type_error(v, name, "enum");
                return None;
            }
            enum_value!(NewLine, NewLine)
        }
        OptionType::PathMap => {
            let Some(members) = v.as_object() else {
                d.type_error(v, name, "object");
                return None;
            };
            let mut map = Vec::new();
            for m in members {
                if m.key.matches('*').count() > 1 {
                    d.at(
                        m.key_span,
                        5061,
                        format!("Pattern '{}' can have at most one '*' character.", m.key),
                    );
                }
                let mut subs = Vec::new();
                match &m.value.kind {
                    ValueKind::Array(items) => {
                        for it in items {
                            if let ValueKind::String(s) = &it.kind {
                                if s.matches('*').count() > 1 {
                                    d.at(
                                        it.span,
                                        5062,
                                        format!("Substitution '{s}' in pattern '{}' can have at most one '*' character.", m.key),
                                    );
                                }
                                if !path::is_rooted(s)
                                    && !path::is_explicitly_relative(s)
                                    && !s.starts_with(CONFIG_DIR_TEMPLATE)
                                {
                                    d.at(it.span, 5090, "Non-relative paths are not allowed. Did you forget a leading './'?".into());
                                }
                                subs.push(s.clone());
                            }
                        }
                        if subs.is_empty() {
                            d.at(
                                m.value.span,
                                5066,
                                format!(
                                    "Substitutions for pattern '{}' shouldn't be an empty array.",
                                    m.key
                                ),
                            );
                        }
                    }
                    _ => {
                        d.at(
                            m.value.span,
                            5063,
                            format!("Substitutions for pattern '{}' should be an array.", m.key),
                        );
                    }
                }
                map.retain(|(k, _): &(String, Vec<String>)| *k != m.key);
                map.push((m.key.clone(), subs));
            }
            Some(OptionValue::Paths(PathsMap(map)))
        }
        OptionType::Plugins => {
            let Some(items) = v.as_array() else {
                d.type_error(v, name, "Array");
                return None;
            };
            let mut out = Vec::new();
            for it in items {
                if it.as_object().is_some() {
                    out.push(it.clone());
                } else {
                    d.type_error(it, "plugin", "object");
                }
            }
            Some(OptionValue::Plugins(out))
        }
    }
}

fn parse_watch_options(members: &[jsonc::Member], dir: &str) -> WatchOptions {
    let mut w = WatchOptions::default();
    for m in members {
        let v = &m.value;
        match m.key.as_str() {
            "watchFile" => w.watch_file = v.as_str().and_then(WatchFileKind::parse),
            "watchDirectory" => w.watch_directory = v.as_str().and_then(WatchDirectoryKind::parse),
            "fallbackPolling" => w.fallback_polling = v.as_str().and_then(PollingKind::parse),
            "synchronousWatchDirectory" => w.synchronous_watch_directory = v.as_bool(),
            "excludeDirectories" | "excludeFiles" => {
                let list = v.as_array().map(|items| {
                    items
                        .iter()
                        .filter_map(|i| i.as_str())
                        .map(|s| resolve_path_value(dir, s))
                        .collect()
                });
                if m.key == "excludeDirectories" {
                    w.exclude_directories = list;
                } else {
                    w.exclude_files = list;
                }
            }
            _ => {}
        }
    }
    w
}

fn parse_type_acquisition(members: &[jsonc::Member], d: &mut Diags<'_>) -> TypeAcquisition {
    let mut t = TypeAcquisition::default();
    let strings = |v: &Value| {
        v.as_array().map(|a| {
            a.iter()
                .filter_map(|i| i.as_str().map(str::to_owned))
                .collect()
        })
    };
    for m in members {
        match m.key.as_str() {
            "enable" => t.enable = m.value.as_bool(),
            "include" => t.include = strings(&m.value),
            "exclude" => t.exclude = strings(&m.value),
            "disableFilenameBasedTypeAcquisition" => {
                t.disable_filename_based_type_acquisition = m.value.as_bool()
            }
            other => d.at(
                m.key_span,
                17010,
                format!("Unknown type acquisition option '{other}'."),
            ),
        }
    }
    t
}

/// Finds the nearest `tsconfig.json` (or `file_name`) starting at `start_dir` and walking
/// up to the root, like `tsc` without `-p`.
pub fn find_config_file(fs: &dyn FileSystem, start_dir: &str, file_name: &str) -> Option<String> {
    let mut dir = path::normalize(start_dir);
    loop {
        let candidate = path::combine(&dir, file_name);
        if fs.file_exists(&candidate) {
            return Some(candidate);
        }
        let parent = path::directory_of(&dir).to_owned();
        if parent == dir || parent.is_empty() {
            return None;
        }
        dir = parent;
    }
}

/// Resolves a `-p`/`--project` argument: a config file, or a directory containing
/// `tsconfig.json`.
pub fn resolve_project_arg(fs: &dyn FileSystem, project: &str) -> Option<String> {
    let p = path::resolve(&fs.cwd(), &path::normalize_slashes(project));
    if fs.directory_exists(&p) {
        let c = path::combine(&p, "tsconfig.json");
        return fs.file_exists(&c).then_some(c);
    }
    fs.file_exists(&p).then_some(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryFs;

    /// `extends: ".."` finds the parent's `tsconfig.json`; `"../"` is a plain path (observed
    /// with tsc 7: TS6053). esbuild's `TsconfigExtendsDotDotWithoutSlash` relies on it.
    #[test]
    fn extends_dot_dot() {
        let mut fs = MemoryFs::new();
        fs.add_file("/p/tsconfig.json", "{}")
            .add_file("/p/src/tsconfig.json", r#"{"extends":".."}"#);
        assert_eq!(
            resolve_extends(&fs, "..", "/p/src").as_deref(),
            Some("/p/tsconfig.json")
        );
        assert_eq!(
            resolve_extends(&fs, ".", "/p").as_deref(),
            Some("/p/tsconfig.json")
        );
        assert_eq!(resolve_extends(&fs, "../", "/p/src"), None);
    }

    /// A package's `"tsconfig"` field may name a directory (esbuild's
    /// `TsconfigJsonNodeModulesTsconfigPathDirectory`; tsc 7 loads `<dir>/tsconfig.json`).
    #[test]
    fn package_tsconfig_field_directory() {
        let mut fs = MemoryFs::new();
        fs.add_file(
            "/p/node_modules/foo/package.json",
            r#"{"tsconfig":"over/here"}"#,
        )
        .add_file("/p/node_modules/foo/over/here/tsconfig.json", "{}");
        assert_eq!(
            resolve_extends(&fs, "foo", "/p").as_deref(),
            Some("/p/node_modules/foo/over/here/tsconfig.json")
        );
    }

    /// No inputs (tsc 7's `tsbuild/solution` scenarios): an empty `files` list is TS18002
    /// even with `references: []`; an include that matches nothing is TS18003 unless the
    /// config has a `references` key, which makes it a container.
    #[test]
    fn empty_inputs_and_references_key() {
        let load = |json: &str| {
            let mut fs = MemoryFs::new();
            fs.add_file("/p/tsconfig.json", json);
            load_tsconfig(&fs, "/p/tsconfig.json")
        };
        let codes = |c: &ParsedConfig| c.diagnostics.iter().map(|d| d.code).collect::<Vec<_>>();
        assert_eq!(codes(&load(r#"{"files":[],"references":[]}"#)), [18002]);
        assert_eq!(codes(&load("{}")), [18003]);
        let container = load(r#"{"references":[]}"#);
        assert_eq!(codes(&container), Vec::<u32>::new());
        assert!(container.declares_references);
        assert!(!load("{}").declares_references);
    }

    /// Non-string `files`/`include` entries (tsc 7's `extends-config-with-non-string-*`
    /// scenarios): `files: [1]` is TS5024 alone, `include: [1]` shows `[1]` in TS18003.
    #[test]
    fn non_string_list_entries() {
        let codes = |json: &str| {
            let mut fs = MemoryFs::new();
            fs.add_file("/p/tsconfig.json", json);
            let c = load_tsconfig(&fs, "/p/tsconfig.json");
            c.diagnostics
                .iter()
                .map(|d| (d.code, d.message.clone()))
                .collect::<Vec<_>>()
        };
        let files = codes(r#"{"files":[1]}"#);
        assert_eq!(files.iter().map(|d| d.0).collect::<Vec<_>>(), [5024]);
        let include = codes(r#"{"include":["a",2]}"#);
        assert!(
            include
                .iter()
                .any(|d| d.0 == 18003 && d.1.contains("'include' paths were '[\"a\",2]'")),
            "{include:?}"
        );
        assert_eq!(json_array_with_raw(&[], &[(0, "1".into())]), "[1]");
    }
}
