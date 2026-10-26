//! Configuration: ESLint flat config (evaluated by Node, see [`eslint_eval`]), `.oxlintrc.json`
//! and `biome.json`, all lowered to the same model: an ordered list of [`ConfigObject`]s with
//! `files`/`ignores` globs, matched per file and merged like ESLint's flat config.

pub mod biome;
pub mod eslint_eval;
pub mod glob;
pub mod mapping;
pub mod oxlint;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rustc_hash::FxHashMap;
use serde_json::Value;

use crate::diagnostic::Severity;
use crate::engine::{ConfiguredRule, FileConfig, RuleSet};
use crate::env::{GlobalAccess, Globals, LintEnv, SourceType};
use crate::registry::Registry;
use glob::Glob;
use mapping::UnsupportedReason;

/// Which tool's config a [`ConfigFile`] came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfigKind {
    Eslint,
    Oxlint,
    Biome,
    /// No config file: ts-r-lint's built-in defaults.
    Default,
}

/// `languageOptions` of one config object (only what we use).
#[derive(Clone, Debug, Default)]
pub struct LanguagePartial {
    pub ecma_version: Option<u32>,
    pub source_type: Option<SourceType>,
    pub globals: Vec<(String, GlobalAccess)>,
    pub parser: Option<String>,
    pub jsx_pragma: Option<Option<String>>,
    pub global_return: Option<bool>,
    pub jsx_fragment_name: Option<Option<String>>,
    pub emit_decorator_metadata: Option<bool>,
    pub experimental_decorators: Option<bool>,
    /// `parserOptions.lib` (TypeScript lib names).
    pub lib: Option<Vec<String>>,
    /// `parserOptions.project`: `Some(None)` when off (`false`/`null`), `Some(Some(paths))`
    /// otherwise (`true` = no paths = nearest tsconfig).
    pub project: Option<Option<Vec<String>>>,
    /// `parserOptions.projectService` is truthy.
    pub project_service: Option<bool>,
    /// `parserOptions.tsconfigRootDir`.
    pub tsconfig_root_dir: Option<String>,
}

/// `linterOptions` of one config object.
#[derive(Clone, Debug, Default)]
pub struct LinterPartial {
    pub report_unused_disable_directives: Option<Severity>,
    pub no_inline_config: Option<bool>,
}

/// One entry of `files`: a pattern, or several that must all match.
#[derive(Clone, Debug)]
pub enum FilesEntry {
    One(Glob),
    All(Vec<Glob>),
}

impl FilesEntry {
    fn matches(&self, rel: &str) -> bool {
        match self {
            FilesEntry::One(g) => g.matches(rel),
            FilesEntry::All(gs) => gs.iter().all(|g| g.matches(rel)),
        }
    }
    fn is_universal(&self) -> bool {
        match self {
            FilesEntry::One(g) => g.is_universal(),
            FilesEntry::All(gs) => gs.iter().all(|g| g.is_universal()),
        }
    }
}

/// A rule entry in a config object, already mapped to a built-in rule (or not).
#[derive(Clone, Debug)]
pub struct RuleEntry {
    /// The name as written in the config.
    pub written: String,
    /// Our rule name, or why it is not supported.
    pub mapped: Result<&'static str, UnsupportedReason>,
    pub severity: Severity,
    /// `None` = severity only (keeps options from earlier objects).
    pub options: Option<Vec<Value>>,
}

/// One flat-config object.
#[derive(Clone, Debug, Default)]
pub struct ConfigObject {
    pub name: Option<String>,
    /// ESLint `basePath`: the object applies only below this directory, and its `files` and
    /// `ignores` are relative to it. As written until [`ConfigFile::new`] makes it relative to
    /// the config's base (`/`-separated, `None` for the base itself).
    pub base_path: Option<String>,
    pub files: Option<Vec<FilesEntry>>,
    pub ignores: Option<Vec<(bool, Glob)>>,
    /// Only `ignores` (and `name`): global ignores.
    pub global_ignores: bool,
    pub rules: Vec<RuleEntry>,
    pub language: LanguagePartial,
    pub linter: LinterPartial,
    pub settings: Option<Value>,
    /// Notes about parts we cannot honour (function patterns, processors).
    pub notes: Vec<String>,
}

impl ConfigObject {
    /// `rel` relative to the object's `basePath`, or `None` when it is outside.
    fn within<'r>(&self, rel: &'r str) -> Option<&'r str> {
        match &self.base_path {
            None => Some(rel),
            Some(b) => rel.strip_prefix(b.as_str())?.strip_prefix('/'),
        }
    }
}

/// A loaded config file.
#[derive(Debug)]
pub struct ConfigFile {
    pub kind: ConfigKind,
    pub path: Option<PathBuf>,
    /// Base directory for `files`/`ignores` patterns.
    pub base: PathBuf,
    pub objects: Vec<ConfigObject>,
    /// Local files an ESLint flat config imports: a change to one changes the config.
    pub deps: Vec<PathBuf>,
    cache: Mutex<FxHashMap<Vec<u32>, Arc<FileConfig>>>,
}

/// Why a file is not linted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NotLinted {
    Ignored,
    NoMatchingConfig,
}

impl ConfigFile {
    pub fn new(
        kind: ConfigKind,
        path: Option<PathBuf>,
        base: PathBuf,
        mut objects: Vec<ConfigObject>,
    ) -> ConfigFile {
        let mut all = default_objects(kind);
        all.append(&mut objects);
        for o in &mut all {
            let Some(bp) = o.base_path.take() else {
                continue;
            };
            let dir = crate::project::normalize(&base.join(&bp));
            match dir.strip_prefix(crate::project::normalize(&base)) {
                Ok(rel) => {
                    let rel: Vec<_> = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy())
                        .collect();
                    if !rel.is_empty() {
                        o.base_path = Some(rel.join("/"));
                    }
                }
                Err(_) => o.notes.push(format!(
                    "basePath {bp:?} outside the config directory is not supported"
                )),
            }
        }
        ConfigFile {
            kind,
            path,
            base,
            objects: all,
            deps: Vec::new(),
            cache: Mutex::new(FxHashMap::default()),
        }
    }

    /// Path of `file` relative to the base, `/`-separated; `None` if outside the base.
    pub fn relative(&self, file: &Path) -> Option<String> {
        let rel = file.strip_prefix(&self.base).ok()?;
        let s: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        Some(s.join("/"))
    }

    /// True when a directory (relative path) is ignored by a global ignore pattern.
    pub fn is_dir_ignored(&self, rel_dir: &str) -> bool {
        let mut ignored = false;
        for o in self.objects.iter().filter(|o| o.global_ignores) {
            let Some(rel_dir) = o.within(rel_dir) else {
                continue;
            };
            for (neg, g) in o.ignores.iter().flatten() {
                if *neg {
                    if g.matches_dir(rel_dir) {
                        ignored = false;
                    }
                } else if g.matches_dir(rel_dir) {
                    ignored = true;
                }
            }
        }
        ignored
    }

    /// True when the file is ignored by global ignores (including its parent directories).
    pub fn is_file_ignored(&self, rel: &str) -> bool {
        let mut parts: Vec<&str> = rel.split('/').collect();
        parts.pop();
        let mut dir = String::new();
        for p in parts {
            if !dir.is_empty() {
                dir.push('/');
            }
            dir.push_str(p);
            if self.is_dir_ignored(&dir) {
                return true;
            }
        }
        let mut ignored = false;
        for o in self.objects.iter().filter(|o| o.global_ignores) {
            let Some(rel) = o.within(rel) else {
                continue;
            };
            for (neg, g) in o.ignores.iter().flatten() {
                if g.dir_only {
                    continue;
                }
                if g.matches(rel) {
                    ignored = !*neg;
                }
            }
        }
        ignored
    }

    /// Indices of the objects that apply to `rel`, or why the file is not linted.
    pub fn matching(&self, rel: &str) -> Result<Vec<u32>, NotLinted> {
        if self.is_file_ignored(rel) {
            return Err(NotLinted::Ignored);
        }
        let mut explicit = false;
        let mut out = Vec::new();
        for (i, o) in self.objects.iter().enumerate() {
            if o.global_ignores {
                continue;
            }
            let Some(rel) = o.within(rel) else {
                continue;
            };
            if let Some(files) = &o.files {
                let hit: Vec<&FilesEntry> = files.iter().filter(|f| f.matches(rel)).collect();
                if hit.is_empty() {
                    continue;
                }
                if let Some(ign) = &o.ignores {
                    let mut ignored = false;
                    for (neg, g) in ign {
                        if g.matches(rel) {
                            ignored = !*neg;
                        }
                    }
                    if ignored {
                        continue;
                    }
                }
                if hit.iter().any(|f| !f.is_universal()) {
                    explicit = true;
                }
            } else if let Some(ign) = &o.ignores {
                let mut ignored = false;
                for (neg, g) in ign {
                    if g.matches(rel) {
                        ignored = !*neg;
                    }
                }
                if ignored {
                    continue;
                }
            }
            out.push(i as u32);
        }
        if !explicit {
            return Err(NotLinted::NoMatchingConfig);
        }
        Ok(out)
    }

    /// The resolved config for a file (cached per distinct set of matching objects).
    pub fn file_config(
        &self,
        rel: &str,
        opts: &ResolveOptions,
    ) -> Result<Arc<FileConfig>, NotLinted> {
        let idx = self.matching(rel)?;
        if let Ok(c) = self.cache.lock()
            && let Some(fc) = c.get(&idx)
        {
            return Ok(fc.clone());
        }
        let fc = Arc::new(self.build(&idx, opts));
        if let Ok(mut c) = self.cache.lock() {
            c.insert(idx, fc.clone());
        }
        Ok(fc)
    }

    /// Sets the TypeScript lib globals of a TypeScript-parsed file's environment, as
    /// typescript-eslint picks them: `parserOptions.lib`, else the tsconfig's (with `project`
    /// or `projectService`, resolved per file by the engine), else `esnext`.
    fn with_lib(&self, mut env: LintEnv, lang: &LanguagePartial) -> LintEnv {
        use crate::lib_globals::{LibGlobals, ProjectLib};
        if !env.typescript {
            return env;
        }
        env.lib = Some(match &lang.lib {
            Some(l) => LibGlobals::for_lib_names(l),
            None => LibGlobals::default_lib(),
        });
        let projects = match (&lang.project, lang.project_service) {
            (_, Some(true)) => Some((Vec::new(), true)),
            (Some(Some(p)), _) => Some((p.clone(), false)),
            _ => None,
        };
        if let Some((projects, service)) = projects {
            let root_dir = match &lang.tsconfig_root_dir {
                Some(d) => self.base.join(d),
                None => self.base.clone(),
            };
            env.project_lib = Some(Arc::new(ProjectLib {
                root_dir,
                projects,
                service,
                lib_from_tsconfig: lang.lib.is_none(),
                emit_decorator_metadata: lang.emit_decorator_metadata,
                experimental_decorators: lang.experimental_decorators,
            }));
        }
        env
    }

    fn build(&self, idx: &[u32], opts: &ResolveOptions) -> FileConfig {
        // Merge rules in order, by the rule they map to: several written names can name one
        // rule (oxlint/biome aliases), and the last one written wins.
        let mut merged: BTreeMap<Result<&'static str, String>, MergedRule> = BTreeMap::new();
        let mut lang = LanguagePartial::default();
        let mut globals = Globals::new();
        let mut linter = LinterPartial::default();
        let mut settings = serde_json::Map::new();
        for &i in idx {
            let o = &self.objects[i as usize];
            for r in &o.rules {
                let key = r.mapped.map_err(|_| r.written.clone());
                let e = merged.entry(key).or_insert((
                    String::new(),
                    r.mapped,
                    Severity::Off,
                    Vec::new(),
                ));
                e.0.clone_from(&r.written);
                e.2 = r.severity;
                if let Some(opts) = &r.options {
                    e.3 = opts.clone();
                }
            }
            if let Some(v) = o.language.ecma_version {
                lang.ecma_version = Some(v);
            }
            if let Some(v) = o.language.source_type {
                lang.source_type = Some(v);
            }
            if let Some(p) = &o.language.parser {
                lang.parser = Some(p.clone());
            }
            if let Some(p) = &o.language.jsx_pragma {
                lang.jsx_pragma = Some(p.clone());
            }
            if let Some(g) = o.language.global_return {
                lang.global_return = Some(g);
            }
            if let Some(p) = &o.language.jsx_fragment_name {
                lang.jsx_fragment_name = Some(p.clone());
            }
            if let Some(v) = o.language.emit_decorator_metadata {
                lang.emit_decorator_metadata = Some(v);
            }
            if let Some(v) = o.language.experimental_decorators {
                lang.experimental_decorators = Some(v);
            }
            if let Some(v) = &o.language.lib {
                lang.lib = Some(v.clone());
            }
            if let Some(v) = &o.language.project {
                lang.project = Some(v.clone());
            }
            if let Some(v) = o.language.project_service {
                lang.project_service = Some(v);
            }
            if let Some(v) = &o.language.tsconfig_root_dir {
                lang.tsconfig_root_dir = Some(v.clone());
            }
            for (name, access) in &o.language.globals {
                globals.insert(name, *access);
            }
            if let Some(v) = o.linter.report_unused_disable_directives {
                linter.report_unused_disable_directives = Some(v);
            }
            if let Some(v) = o.linter.no_inline_config {
                linter.no_inline_config = Some(v);
            }
            if let Some(Value::Object(s)) = &o.settings {
                for (k, v) in s {
                    settings.insert(k.clone(), v.clone());
                }
            }
        }
        let ecma = lang
            .ecma_version
            .unwrap_or(crate::globals::LATEST_ECMA_VERSION);
        let source_type = lang.source_type.unwrap_or(SourceType::Module);
        let mut all_globals = crate::globals::builtin_globals(ecma);
        if source_type == SourceType::CommonJs {
            for g in crate::globals::COMMONJS {
                all_globals.insert(g, GlobalAccess::Readonly);
            }
        }
        all_globals.extend(&globals);
        let typescript = lang
            .parser
            .as_deref()
            .is_some_and(|p| p.contains("typescript"));
        let env = LintEnv {
            source_type,
            ecma_version: ecma,
            globals: Arc::new(all_globals),
            typescript,
            jsx_pragma: if typescript {
                lang.jsx_pragma.clone().unwrap_or(Some("React".to_owned()))
            } else {
                None
            },
            global_return: lang.global_return.unwrap_or(false),
            jsx_fragment_name: if typescript {
                lang.jsx_fragment_name.clone().flatten()
            } else {
                None
            },
            decorator_metadata: typescript
                && lang.emit_decorator_metadata == Some(true)
                && lang.experimental_decorators == Some(true),
            settings: Arc::new(Value::Object(settings)),
            lib: None,
            project_lib: None,
        };
        let env = self.with_lib(env, &lang);
        let reg = opts.registry;
        let mut rules = Vec::new();
        let mut skipped = Vec::new();
        for (written, mapped, sev, options) in merged.into_values() {
            let sev = if opts.quiet && sev == Severity::Warn {
                Severity::Off
            } else {
                sev
            };
            if sev == Severity::Off {
                continue;
            }
            match mapped {
                Ok(name) => {
                    let Some(def) = reg.find(name) else { continue };
                    if def.meta.requires_types && !opts.types_available {
                        skipped.push(written);
                        continue;
                    }
                    if let Some(only) = &opts.only_rules
                        && !only.iter().any(|o| o == name)
                    {
                        continue;
                    }
                    match reg.instantiate(def, &options) {
                        Ok(rule) => rules.push(ConfiguredRule {
                            def,
                            severity: sev,
                            rule,
                        }),
                        Err(e) => {
                            opts.note(format!("{written}: invalid options ({e}); rule skipped"));
                            skipped.push(written);
                        }
                    }
                }
                Err(_) => skipped.push(written),
            }
        }
        FileConfig {
            rules: Arc::new(RuleSet::new(rules)),
            env,
            report_unused_disable_directives: if opts.quiet {
                Severity::Off
            } else {
                opts.report_unused_override.unwrap_or(
                    linter
                        .report_unused_disable_directives
                        .unwrap_or(Severity::Warn),
                )
            },
            no_inline_config: opts.no_inline_config || linter.no_inline_config.unwrap_or(false),
            quiet: opts.quiet,
            skipped_rules: Arc::new(skipped),
            registry: reg,
            typed_half: false,
        }
    }

    /// Every configured rule we cannot run, with the reason (for the summary).
    pub fn unsupported(&self, types_available: bool) -> Vec<(String, UnsupportedReason)> {
        let mut out: BTreeMap<String, UnsupportedReason> = BTreeMap::new();
        for o in &self.objects {
            for r in &o.rules {
                if r.severity == Severity::Off {
                    continue;
                }
                match r.mapped {
                    Err(reason) => {
                        out.insert(r.written.clone(), reason);
                    }
                    Ok(name) => {
                        if !types_available
                            && Registry::global()
                                .find(name)
                                .is_some_and(|d| d.meta.requires_types)
                        {
                            out.insert(r.written.clone(), UnsupportedReason::TypeCheckOff);
                        }
                    }
                }
            }
        }
        // Rules turned off later in the file still count as configured somewhere; only
        // report names that are enabled in at least one object.
        out.into_iter().collect()
    }

    /// Notes collected while loading (function patterns, processors, ...).
    pub fn notes(&self) -> Vec<String> {
        let mut v: Vec<String> = self.objects.iter().flat_map(|o| o.notes.clone()).collect();
        v.sort();
        v.dedup();
        v
    }
}

/// A rule after merging config objects: last written name, mapping, severity, options.
type MergedRule = (
    String,
    Result<&'static str, UnsupportedReason>,
    Severity,
    Vec<Value>,
);

/// Options that affect how configs resolve.
#[derive(Debug)]
pub struct ResolveOptions {
    pub registry: &'static Registry,
    /// `--quiet`: warnings are not run at all.
    pub quiet: bool,
    pub types_available: bool,
    pub no_inline_config: bool,
    /// `--report-unused-disable-directives[-severity]`.
    pub report_unused_override: Option<Severity>,
    /// `--rule` filter used by tests and the oracle (`None` = all).
    pub only_rules: Option<Vec<String>>,
    pub notes: Mutex<Vec<String>>,
}

impl ResolveOptions {
    pub fn new() -> ResolveOptions {
        ResolveOptions {
            registry: Registry::global(),
            quiet: false,
            types_available: false,
            no_inline_config: false,
            report_unused_override: None,
            only_rules: None,
            notes: Mutex::new(Vec::new()),
        }
    }
    fn note(&self, s: String) {
        if let Ok(mut n) = self.notes.lock()
            && !n.contains(&s)
        {
            n.push(s);
        }
    }
}

impl Default for ResolveOptions {
    fn default() -> Self {
        ResolveOptions::new()
    }
}

/// ESLint's built-in default objects (prepended to every flat config).
fn default_objects(kind: ConfigKind) -> Vec<ConfigObject> {
    let mut v = Vec::new();
    v.push(ConfigObject {
        name: Some("eslint/defaults/ignores".to_owned()),
        ignores: Some(vec![
            (false, Glob::new("**/node_modules/")),
            (false, Glob::new(".git/")),
        ]),
        global_ignores: true,
        ..ConfigObject::default()
    });
    let js = |pats: &[&str], st: SourceType| ConfigObject {
        files: Some(pats.iter().map(|p| FilesEntry::One(Glob::new(p))).collect()),
        language: LanguagePartial {
            source_type: Some(st),
            ..LanguagePartial::default()
        },
        ..ConfigObject::default()
    };
    v.push(js(&["**/*.js", "**/*.mjs"], SourceType::Module));
    v.push(js(&["**/*.cjs"], SourceType::CommonJs));
    if kind != ConfigKind::Eslint {
        // oxlint and Biome lint TS and JSX files without being told to.
        v.push(js(&["**/*.{ts,tsx,mts,cts,jsx}"], SourceType::Module));
    }
    v
}

// ---------------------------------------------------------------------------------------
// ESLint flat config (from the JSON our Node script prints)
// ---------------------------------------------------------------------------------------

/// Builds config objects from the serialized flat config array.
pub fn objects_from_eslint_json(v: &Value, reg: &Registry) -> Result<Vec<ConfigObject>, String> {
    let arr = match v {
        Value::Array(a) => a.clone(),
        Value::Object(_) => vec![v.clone()],
        _ => return Err("config must export an array or an object".to_owned()),
    };
    let mut out = Vec::new();
    for item in &arr {
        let Value::Object(o) = item else {
            return Err("config array entries must be objects".to_owned());
        };
        let mut obj = ConfigObject {
            name: o.get("name").and_then(Value::as_str).map(str::to_owned),
            base_path: o.get("basePath").and_then(Value::as_str).map(str::to_owned),
            ..ConfigObject::default()
        };
        let pat = |p: &Value, notes: &mut Vec<String>| -> Option<Glob> {
            match p.as_str() {
                Some("<function>") | None => {
                    notes.push("function patterns in files/ignores are not supported".to_owned());
                    None
                }
                Some(s) => Some(Glob::new(s)),
            }
        };
        if let Some(Value::Array(files)) = o.get("files") {
            let mut entries = Vec::new();
            for f in files {
                match f {
                    Value::Array(all) => {
                        let gs: Vec<Glob> =
                            all.iter().filter_map(|p| pat(p, &mut obj.notes)).collect();
                        if !gs.is_empty() {
                            entries.push(FilesEntry::All(gs));
                        }
                    }
                    p => {
                        if let Some(g) = pat(p, &mut obj.notes) {
                            entries.push(FilesEntry::One(g));
                        }
                    }
                }
            }
            obj.files = Some(entries);
        }
        if let Some(Value::Array(ign)) = o.get("ignores") {
            let mut v = Vec::new();
            for p in ign {
                if let Some(s) = p.as_str() {
                    if s == "<function>" {
                        obj.notes.push(
                            "function patterns in files/ignores are not supported".to_owned(),
                        );
                        continue;
                    }
                    match s.strip_prefix('!') {
                        Some(rest) => v.push((true, Glob::new(rest))),
                        None => v.push((false, Glob::new(s))),
                    }
                }
            }
            obj.ignores = Some(v);
        }
        let keys: Vec<&str> = o.keys().map(String::as_str).collect();
        obj.global_ignores = obj.ignores.is_some()
            && keys
                .iter()
                .all(|k| *k == "ignores" || *k == "name" || *k == "basePath");
        if let Some(Value::Object(rules)) = o.get("rules") {
            for (name, val) in rules {
                obj.rules
                    .push(rule_entry(reg, name, val, mapping::map_eslint_name)?);
            }
        }
        if let Some(Value::Object(lo)) = o.get("languageOptions") {
            obj.language = language_from_json(lo);
        }
        if let Some(Value::Object(lo)) = o.get("linterOptions") {
            if let Some(v) = lo.get("reportUnusedDisableDirectives") {
                obj.linter.report_unused_disable_directives = match v {
                    Value::Bool(true) => Some(Severity::Warn),
                    Value::Bool(false) => Some(Severity::Off),
                    v => Severity::from_json(v),
                };
            }
            if let Some(Value::Bool(b)) = lo.get("noInlineConfig") {
                obj.linter.no_inline_config = Some(*b);
            }
        }
        if let Some(s) = o.get("settings") {
            obj.settings = Some(s.clone());
        }
        if o.get("processor").is_some() {
            obj.notes.push("processors are not supported".to_owned());
        }
        out.push(obj);
    }
    Ok(out)
}

/// Parses one rule entry, mapping the name with `map`.
pub fn rule_entry(
    reg: &Registry,
    name: &str,
    val: &Value,
    map: fn(&Registry, &str) -> Result<&'static str, UnsupportedReason>,
) -> Result<RuleEntry, String> {
    let (severity, options) = match val {
        Value::Array(items) if !items.is_empty() => {
            let sev = Severity::from_json(&items[0])
                .ok_or_else(|| format!("rule {name}: bad severity {}", items[0]))?;
            let opts = if items.len() > 1 {
                Some(items[1..].to_vec())
            } else {
                None
            };
            (sev, opts)
        }
        v => (
            Severity::from_json(v).ok_or_else(|| format!("rule {name}: bad severity {v}"))?,
            None,
        ),
    };
    Ok(RuleEntry {
        written: name.to_owned(),
        mapped: map(reg, name),
        severity,
        options,
    })
}

fn language_from_json(lo: &serde_json::Map<String, Value>) -> LanguagePartial {
    let mut l = LanguagePartial::default();
    match lo.get("ecmaVersion") {
        Some(Value::Number(n)) => l.ecma_version = n.as_u64().map(|v| v as u32),
        Some(Value::String(s)) if s == "latest" => {
            l.ecma_version = Some(crate::globals::LATEST_ECMA_VERSION)
        }
        _ => {}
    }
    if let Some(Value::String(s)) = lo.get("sourceType") {
        l.source_type = SourceType::parse(s);
    }
    if let Some(Value::Object(g)) = lo.get("globals") {
        for (k, v) in g {
            if let Some(a) = GlobalAccess::from_json(v) {
                l.globals.push((k.clone(), a));
            }
        }
    }
    if let Some(Value::String(p)) = lo.get("parser") {
        l.parser = Some(p.clone());
    }
    if let Some(Value::Object(po)) = lo.get("parserOptions") {
        if let Some(Value::Object(ef)) = po.get("ecmaFeatures")
            && let Some(Value::Bool(b)) = ef.get("globalReturn")
        {
            l.global_return = Some(*b);
        }
        match po.get("jsxPragma") {
            Some(Value::String(s)) => l.jsx_pragma = Some(Some(s.clone())),
            Some(Value::Null) => l.jsx_pragma = Some(None),
            _ => {}
        }
        match po.get("jsxFragmentName") {
            Some(Value::String(s)) => l.jsx_fragment_name = Some(Some(s.clone())),
            Some(Value::Null) => l.jsx_fragment_name = Some(None),
            _ => {}
        }
        if let Some(Value::Bool(b)) = po.get("emitDecoratorMetadata") {
            l.emit_decorator_metadata = Some(*b);
        }
        if let Some(Value::Bool(b)) = po.get("experimentalDecorators") {
            l.experimental_decorators = Some(*b);
        }
        if let Some(Value::Array(a)) = po.get("lib") {
            l.lib = Some(
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect(),
            );
        }
        match po.get("project") {
            Some(Value::Bool(true)) => l.project = Some(Some(Vec::new())),
            Some(Value::Bool(false) | Value::Null) => l.project = Some(None),
            Some(Value::String(s)) => l.project = Some(Some(vec![s.clone()])),
            Some(Value::Array(a)) => {
                l.project = Some(Some(
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect(),
                ))
            }
            _ => {}
        }
        match po.get("projectService") {
            Some(Value::Bool(b)) => l.project_service = Some(*b),
            Some(Value::Null) => l.project_service = Some(false),
            Some(Value::Object(_)) => l.project_service = Some(true),
            _ => {}
        }
        if let Some(Value::String(s)) = po.get("tsconfigRootDir") {
            l.tsconfig_root_dir = Some(s.clone());
        }
    }
    l
}

/// The built-in config used when no config file is found: ESLint's and typescript-eslint's
/// recommended sets.
pub fn default_config(base: PathBuf, reg: &Registry) -> ConfigFile {
    use crate::rule::Preset;
    let mut js = ConfigObject::default();
    let mut ts = ConfigObject {
        files: Some(vec![FilesEntry::One(Glob::new("**/*.{ts,tsx,mts,cts}"))]),
        language: LanguagePartial {
            parser: Some("typescript-eslint/parser".to_owned()),
            ..LanguagePartial::default()
        },
        ..ConfigObject::default()
    };
    for d in reg.all() {
        let entry = RuleEntry {
            written: d.meta.name.to_owned(),
            mapped: Ok(d.meta.name),
            severity: Severity::Error,
            options: None,
        };
        if d.meta.presets.contains(&Preset::EslintRecommended) {
            js.rules.push(entry.clone());
        }
        if d.meta.presets.contains(&Preset::TsRecommended)
            || d.meta.presets.contains(&Preset::TsEslintRecommended)
        {
            ts.rules.push(entry);
        }
    }
    // typescript-eslint's eslint-recommended turns these off for TS files.
    for off in TS_OFF_RULES {
        ts.rules.push(RuleEntry {
            written: (*off).to_owned(),
            mapped: Ok(off),
            severity: Severity::Off,
            options: None,
        });
    }
    ConfigFile::new(ConfigKind::Default, None, base, vec![js, ts])
}

/// Core rules typescript-eslint's `eslint-recommended` disables for TS files.
pub const TS_OFF_RULES: &[&str] = &[
    "constructor-super",
    "getter-return",
    "no-class-assign",
    "no-const-assign",
    "no-dupe-args",
    "no-dupe-class-members",
    "no-dupe-keys",
    "no-func-assign",
    "no-import-assign",
    "no-new-native-nonconstructor",
    "no-obj-calls",
    "no-redeclare",
    "no-setter-return",
    "no-this-before-super",
    "no-undef",
    "no-unreachable",
    "no-unsafe-negation",
    "no-with",
    "no-unused-vars",
    "no-array-constructor",
    "no-unused-expressions",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_path_scopes_objects() {
        // Results observed from ESLint 9.39 on the same config.
        let v = serde_json::json!([
            { "basePath": "src", "files": ["**/*.js"], "rules": { "no-debugger": "error" } },
            { "basePath": "src", "rules": { "no-var": "error" } },
            { "basePath": "src", "ignores": ["gen/**"] },
            { "basePath": "./lib/", "files": ["*.js"], "rules": { "no-var": "warn" } },
        ]);
        let objs = objects_from_eslint_json(&v, Registry::global()).unwrap();
        let cfg = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let rules = |rel: &str| -> Vec<(&str, Severity)> {
            let fc = cfg.file_config(rel, &ResolveOptions::new()).unwrap();
            let r = fc.rules.rules.iter();
            r.map(|r| (r.def.meta.name, r.severity)).collect()
        };
        assert_eq!(rules("outside.js"), []);
        assert_eq!(
            rules("src/a.js"),
            [
                ("no-debugger", Severity::Error),
                ("no-var", Severity::Error)
            ]
        );
        assert_eq!(rules("lib/l.js"), [("no-var", Severity::Warn)]);
        assert!(cfg.is_file_ignored("src/gen/g.js"));
        assert!(!cfg.is_file_ignored("gen/g.js"));
    }
}
