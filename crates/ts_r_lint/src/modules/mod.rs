//! Module resolution and export maps for the eslint-plugin-import rules that look at other
//! files (`no-unresolved`, `named`, `namespace`, `no-cycle`, ...).
//!
//! - [`ModuleHost`]: the file system plus caches of resolutions and export maps. A project
//!   run shares one host across files and threads (`ResolveOptions::modules`); without one,
//!   each file gets a fresh host, so an editor never sees stale results.
//! - [`ModCtx`]: one file's view (its settings, parse options and cache key), built once per
//!   file by [`crate::rule::FileCtx::modules`].
//! - `node_resolve`: `eslint-import-resolver-node`'s algorithm (CommonJS lookup with
//!   extensions, `main`, `index` and `node_modules`), the default resolver.
//! - `export_map`: what a module exports (`ExportMap`), built from our own parse of it.
//!
//! Paths are `/`-separated strings (`ts_r_config::path`).

pub(crate) mod export_map;
pub(crate) mod import_type;
pub(crate) mod minimatch;
mod node_resolve;

use std::any::Any;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use rustc_hash::{FxHashMap, FxHashSet, FxHasher};
use serde_json::Value;
use ts_r_config::path;
use ts_r_config::{FileSystem, RealFs};

pub(crate) use export_map::{ExportMap, ExportMeta};

use crate::env::{LintEnv, SourceType};

/// Where a module specifier leads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Resolved {
    /// A builtin (`fs`, a name in `import/core-modules`): found, with no path.
    Core,
    /// A file (real path, `/`-separated).
    File(Arc<str>),
    /// No resolver found it.
    NotFound,
}

impl Resolved {
    pub(crate) fn path(&self) -> Option<&str> {
        match self {
            Resolved::File(p) => Some(p),
            _ => None,
        }
    }
    pub(crate) fn found(&self) -> bool {
        !matches!(self, Resolved::NotFound)
    }
}

type ResolveKey = (u64, String, String);
type MapEntry = Option<Arc<ExportMap>>;

/// The file system and the caches shared by the files of one run.
pub struct ModuleHost {
    fs: Arc<dyn FileSystem>,
    resolutions: Mutex<FxHashMap<ResolveKey, Result<Resolved, String>>>,
    maps: Mutex<FxHashMap<(u64, String), MapEntry>>,
    packages: Mutex<FxHashMap<String, Option<Arc<Value>>>>,
    /// Whole-project results rules compute once per run (`import/no-unused-modules`),
    /// by settings key and the rule's own key.
    memo: Mutex<FxHashMap<(u64, String), Arc<dyn Any + Send + Sync>>>,
}

impl fmt::Debug for ModuleHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModuleHost").finish_non_exhaustive()
    }
}

impl ModuleHost {
    /// A host over the real file system.
    pub fn real() -> Arc<ModuleHost> {
        ModuleHost::with_fs(Arc::new(RealFs::new()))
    }

    /// A host over `fs` (tests mount fixture trees in memory).
    pub fn with_fs(fs: Arc<dyn FileSystem>) -> Arc<ModuleHost> {
        Arc::new(ModuleHost {
            fs,
            resolutions: Mutex::new(FxHashMap::default()),
            maps: Mutex::new(FxHashMap::default()),
            packages: Mutex::new(FxHashMap::default()),
            memo: Mutex::new(FxHashMap::default()),
        })
    }

    pub(crate) fn fs(&self) -> &dyn FileSystem {
        &*self.fs
    }

    /// The parsed `package.json` in `dir` (`None` when missing or not JSON), cached.
    pub(crate) fn package_json(&self, dir: &str) -> Option<Arc<Value>> {
        if let Ok(m) = self.packages.lock()
            && let Some(v) = m.get(dir)
        {
            return v.clone();
        }
        let file = path::combine(dir, "package.json");
        let v = if self.fs.file_exists(&file) {
            self.fs
                .read_file(&file)
                .and_then(|t| {
                    serde_json::from_str::<Value>(t.strip_prefix('\u{feff}').unwrap_or(&t)).ok()
                })
                .map(Arc::new)
        } else {
            None
        };
        if let Ok(mut m) = self.packages.lock() {
            m.insert(dir.to_owned(), v.clone());
        }
        v
    }

    /// The nearest directory at or above `dir` holding a `package.json` (`pkg-up`).
    pub(crate) fn package_dir(&self, dir: &str) -> Option<String> {
        let mut cur = path::normalize(dir);
        loop {
            if self.fs.file_exists(&path::combine(&cur, "package.json")) {
                return Some(cur);
            }
            let parent = path::directory_of(&cur).to_owned();
            if parent == cur || parent.is_empty() {
                return None;
            }
            cur = parent;
        }
    }
}

/// One configured resolver (`settings["import/resolver"]`).
#[derive(Debug, Clone)]
enum ResolverCfg {
    Node(node_resolve::NodeOptions),
    /// A resolver we cannot load (a JavaScript module): resolving reports an error.
    Unknown(String),
}

/// The `import/*` settings a file's rules read, parsed once per file.
#[derive(Debug)]
pub(crate) struct ModSettings {
    resolvers: Vec<ResolverCfg>,
    pub core_modules: Vec<String>,
    /// File extensions export maps are built for (`import/extensions` plus the extensions
    /// of `import/parsers`).
    pub extensions: Vec<String>,
    pub ignore: Vec<String>,
    /// `import/parsers`: parser name, extensions.
    parsers: Vec<(String, Vec<String>)>,
    pub external_folders: Vec<String>,
    pub internal_regex: Option<String>,
    pub tomdoc: bool,
    pub jsdoc: bool,
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_owned))
        .collect()
}

impl ModSettings {
    fn parse(settings: &Value) -> ModSettings {
        let mut resolvers = Vec::new();
        let configured = &settings["import/resolver"];
        if configured.is_null() || configured == &Value::Bool(false) {
            resolvers.push(node_resolver(&settings["import/resolve"]));
        } else {
            add_resolvers(configured, &mut resolvers);
        }
        let mut extensions = settings
            .get("import/extensions")
            .filter(|v| v.is_array())
            .map(strings)
            .unwrap_or_else(|| vec![".js".into(), ".mjs".into(), ".cjs".into()]);
        let mut parsers = Vec::new();
        if let Some(Value::Object(p)) = settings.get("import/parsers") {
            for (name, exts) in p {
                let exts = strings(exts);
                for e in &exts {
                    if !extensions.contains(e) {
                        extensions.push(e.clone());
                    }
                }
                parsers.push((name.clone(), exts));
            }
        }
        let docstyle = settings
            .get("import/docstyle")
            .filter(|v| v.is_array())
            .map(strings);
        let (jsdoc, tomdoc) = match &docstyle {
            Some(d) => (
                d.iter().any(|s| s == "jsdoc"),
                d.iter().any(|s| s == "tomdoc"),
            ),
            None => (true, false),
        };
        ModSettings {
            resolvers,
            core_modules: strings(&settings["import/core-modules"]),
            extensions,
            ignore: strings(&settings["import/ignore"]),
            parsers,
            external_folders: settings
                .get("import/external-module-folders")
                .filter(|v| v.is_array())
                .map(strings)
                .unwrap_or_else(|| vec!["node_modules".into()]),
            internal_regex: settings["import/internal-regex"]
                .as_str()
                .map(str::to_owned),
            tomdoc,
            jsdoc,
        }
    }

    /// `getFileExtensions`: `import/extensions` (default `.js`, `.mjs`, `.cjs`) plus the
    /// extensions of `import/parsers`.
    pub(crate) fn file_extensions(&self) -> Vec<String> {
        self.extensions.clone()
    }

    /// The `import/parsers` entry naming `ext`, if any.
    fn parser_for(&self, ext: &str) -> Option<&str> {
        self.parsers
            .iter()
            .find(|(_, exts)| exts.iter().any(|e| e == ext))
            .map(|(n, _)| n.as_str())
    }
}

fn node_resolver(cfg: &Value) -> ResolverCfg {
    ResolverCfg::Node(node_resolve::NodeOptions::from_config(cfg))
}

fn add_resolvers(v: &Value, out: &mut Vec<ResolverCfg>) {
    let named = |name: &str, cfg: &Value| match name {
        "node" | "eslint-import-resolver-node" => node_resolver(cfg),
        other => ResolverCfg::Unknown(other.to_owned()),
    };
    match v {
        Value::Array(a) => a.iter().for_each(|r| add_resolvers(r, out)),
        Value::String(s) => out.push(named(s, &Value::Null)),
        Value::Object(o) => {
            for (k, cfg) in o {
                out.push(named(k, cfg));
            }
        }
        _ => out.push(ResolverCfg::Unknown("invalid resolver config".into())),
    }
}

/// How dependency files are parsed: like the linted file, unless `import/parsers` names
/// a TypeScript parser for the extension.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ParseCfg {
    pub typescript: bool,
    pub module: bool,
}

/// One file's view of the module machinery.
pub(crate) struct ModCtx {
    host: Arc<ModuleHost>,
    key: u64,
    /// The linted file (`/`-separated, absolute).
    pub file: String,
    pub settings: ModSettings,
    parse: ParseCfg,
    /// Rules that already reported a resolve error in this file (once per rule and file).
    errored: Mutex<FxHashSet<&'static str>>,
    pub cwd: String,
}

impl fmt::Debug for ModCtx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModCtx").field("file", &self.file).finish()
    }
}

/// The deepest chain of `export *` and re-exports a lookup follows.
pub(crate) const MAX_DEPTH: usize = 64;

impl ModCtx {
    pub(crate) fn new(file: &str, env: &LintEnv) -> ModCtx {
        let host = env.modules.clone().unwrap_or_else(ModuleHost::real);
        let settings = ModSettings::parse(&env.settings);
        let parse = ParseCfg {
            typescript: env.typescript,
            module: env.source_type == SourceType::Module,
        };
        let mut h = FxHasher::default();
        env.settings.to_string().hash(&mut h);
        parse.typescript.hash(&mut h);
        parse.module.hash(&mut h);
        let cwd = match &env.cwd {
            Some(c) => path::normalize(c),
            None => host.fs.cwd(),
        };
        let file = path::normalize(file);
        let file = if path::is_rooted(&file) {
            file
        } else {
            path::resolve(&cwd, &file)
        };
        ModCtx {
            host,
            key: h.finish(),
            file,
            settings,
            parse,
            errored: Mutex::new(FxHashSet::default()),
            cwd,
        }
    }

    /// `make()`'s result for `key`, computed once per host (a project run shares one host;
    /// an editor lint gets a fresh one, so nothing outlives the job).
    pub(crate) fn memo<T: Any + Send + Sync>(&self, key: &str, make: impl FnOnce() -> T) -> Arc<T> {
        let k = (self.key, key.to_owned());
        if let Ok(m) = self.host.memo.lock()
            && let Some(v) = m.get(&k)
            && let Ok(t) = v.clone().downcast::<T>()
        {
            return t;
        }
        let v = Arc::new(make());
        if let Ok(mut m) = self.host.memo.lock() {
            m.insert(k, v.clone());
        }
        v
    }

    pub(crate) fn host(&self) -> &ModuleHost {
        &self.host
    }

    pub(crate) fn fs(&self) -> &dyn FileSystem {
        self.host.fs()
    }

    /// The linted file's directory.
    pub(crate) fn dir(&self) -> &str {
        path::directory_of(&self.file)
    }

    /// Resolves `spec` from the linted file. `Err` is a resolver that failed to load (its
    /// message, for "Resolve error: ...").
    pub(crate) fn resolve(&self, spec: &str) -> Result<Resolved, String> {
        self.resolve_from(spec, &self.file)
    }

    /// Resolves `spec` from `file` (another module's own imports).
    pub(crate) fn resolve_from(&self, spec: &str, file: &str) -> Result<Resolved, String> {
        if self.settings.core_modules.iter().any(|c| c == spec) {
            return Ok(Resolved::Core);
        }
        let dir = path::directory_of(file).to_owned();
        let key = (self.key, dir, spec.to_owned());
        if let Ok(m) = self.host.resolutions.lock()
            && let Some(r) = m.get(&key)
        {
            return r.clone();
        }
        let r = self.resolve_uncached(spec, &key.1);
        if let Ok(mut m) = self.host.resolutions.lock() {
            m.insert(key, r.clone());
        }
        r
    }

    fn resolve_uncached(&self, spec: &str, dir: &str) -> Result<Resolved, String> {
        for r in &self.settings.resolvers {
            match r {
                ResolverCfg::Node(opts) => {
                    let found = node_resolve::resolve(self.fs(), spec, dir, opts, &self.cwd);
                    if found.found() {
                        return Ok(found);
                    }
                }
                ResolverCfg::Unknown(name) if name == "invalid resolver config" => {
                    return Err(name.clone());
                }
                ResolverCfg::Unknown(name) => {
                    return Err(format!("unable to load resolver \"{name}\"."));
                }
            }
        }
        Ok(Resolved::NotFound)
    }

    /// Records that `rule` reported a resolve error; false when it already did.
    pub(crate) fn first_error(&self, rule: &'static str) -> bool {
        self.errored
            .lock()
            .map(|mut s| s.insert(rule))
            .unwrap_or(false)
    }

    /// `import/ignore` and the valid-extension test: no export map for `p`.
    pub(crate) fn is_ignored(&self, p: &str) -> bool {
        if !self.has_valid_extension(p) {
            return true;
        }
        self.settings
            .ignore
            .iter()
            .any(|re| crate::regex::compile(re).is_some_and(|r| r.is_match(p, false)))
    }

    pub(crate) fn has_valid_extension(&self, p: &str) -> bool {
        let ext = extname(p);
        !ext.is_empty() && self.settings.extensions.iter().any(|e| e == ext)
    }

    /// The export map of the module `spec` names (ExportMap's `get`): `None` for builtins,
    /// unresolved or ignored modules and files that are not ES modules.
    pub(crate) fn exports_of(&self, spec: &str) -> Result<Option<Arc<ExportMap>>, String> {
        match self.resolve(spec)? {
            Resolved::File(p) => Ok(self.map_for(&p)),
            _ => Ok(None),
        }
    }

    /// The export map of the file `p`, cached.
    pub(crate) fn map_for(&self, p: &str) -> Option<Arc<ExportMap>> {
        let key = (self.key, p.to_owned());
        if let Ok(m) = self.host.maps.lock()
            && let Some(v) = m.get(&key)
        {
            return v.clone();
        }
        let built = self.build_map(p).map(Arc::new);
        if let Ok(mut m) = self.host.maps.lock() {
            m.insert(key, built.clone());
        }
        built
    }

    fn build_map(&self, p: &str) -> Option<ExportMap> {
        if self.is_ignored(p) {
            return None;
        }
        let text = self.fs().read_file(p)?;
        if !export_map::maybe_module(&text) {
            return None;
        }
        let ext = extname(p);
        let typescript = match self.settings.parser_for(ext) {
            Some(name) => name.contains("typescript") || name.contains("tsr:ts"),
            None => self.parse.typescript,
        };
        export_map::build(self, p, &text, typescript, self.parse.module)
    }

    /// The linted file's package entry point (`require.resolve` of the directory holding
    /// the nearest `package.json`: its `main`, else `index`), if it resolves.
    pub(crate) fn entry_point(&self) -> Option<String> {
        let dir = self.host.package_dir(self.dir())?;
        let opts = node_resolve::NodeOptions::from_config(&serde_json::json!({
            "extensions": [".js", ".json", ".node"]
        }));
        node_resolve::resolve(self.fs(), &dir, &dir, &opts, &self.cwd)
            .path()
            .map(str::to_owned)
    }

    /// The directory of the nearest `package.json` above the linted file, or the cwd.
    pub(crate) fn package_path(&self) -> String {
        self.host
            .package_dir(self.dir())
            .unwrap_or_else(|| self.cwd.clone())
    }
}

/// `path.extname`: the last `.suffix` of the file name (none for dotfiles).
pub(crate) fn extname(p: &str) -> &str {
    let name = path::file_name(p);
    if name == ".." {
        return "";
    }
    match name.rfind('.') {
        Some(0) | None => "",
        Some(i) => &name[i..],
    }
}

/// Node's `path.relative(from, to)` on `/`-separated absolute paths.
pub(crate) fn relative(from: &str, to: &str) -> String {
    path::relative(&path::normalize(from), &path::normalize(to), true)
}

/// A module name Node treats as builtin (`is-core-module`): `fs`, `fs/promises`,
/// `node:test`.
pub(crate) fn is_core_module(name: &str) -> bool {
    use crate::rules::unicorn_prefer_node_protocol::BARE_BUILTINS;
    match name.strip_prefix("node:") {
        Some(b) => {
            BARE_BUILTINS.contains(&b) || matches!(b, "sea" | "sqlite" | "test" | "test/reporters")
        }
        None => BARE_BUILTINS.contains(&name),
    }
}
