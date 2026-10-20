//! Runtime module resolution for bundling.
//!
//! `ts_r_resolver` answers TypeScript's question ("which declaration describes this
//! import?"); a bundler needs the file that runs. So this module resolves like Node and
//! esbuild do (documented behaviour): `exports`/`imports` with runtime conditions
//! (`import`/`require`, `browser`/`node`, `module`, `default`, custom ones), main fields
//! (`browser`, `module`, `main`), the `browser` field's replacement map (including `false`),
//! extension probing (`.tsx .ts .jsx .js .css .json`, `.js` -> `.ts` rewrites in TypeScript
//! projects), directory `index` files, and real paths for symlinked `node_modules` (pnpm).
//! tsconfig `paths` and `baseUrl` apply to non-relative specifiers the way esbuild documents
//! them (TypeScript 7 dropped `baseUrl`, but bundlers still honour it): the best `paths`
//! pattern's substitutions are tried in order, then `baseUrl`, then `node_modules`.
//!
//! Results are cached per (directory, specifier, kind); package.json files per directory.

use std::cell::RefCell;
use std::sync::{Arc, RwLock};

use rustc_hash::FxHashMap;
use ts_r_config::FileSystem;
use ts_r_config::jsonc::{self, Value};
use ts_r_config::path;

use crate::options::{BuildOptions, Platform, wildcard_match};

/// `import` or `require` conditions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResolveKind {
    Import,
    Require,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// An absolute, real path.
    File(String),
    /// Left as an import of this path.
    External(String),
    /// Replaced with an empty module (`"browser": { "x": false }`).
    Disabled,
    NotFound(String),
}

/// `sideEffects` from package.json.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum SideEffects {
    #[default]
    Unknown,
    /// `"sideEffects": false`
    None,
    /// `"sideEffects": ["*.css", "./src/polyfill.js"]`: only matching files have effects.
    Only(Vec<String>),
}

/// The fields of a `package.json` a bundler needs.
#[derive(Debug, Default)]
pub struct Package {
    pub dir: String,
    pub name: Option<String>,
    pub is_module: bool,
    /// Main field name -> path.
    pub mains: FxHashMap<String, String>,
    /// `browser` object form: normalized key -> replacement (`None` = disabled).
    pub browser_map: FxHashMap<String, Option<String>>,
    pub exports: Option<Value>,
    pub imports: Option<Value>,
    pub side_effects: SideEffects,
}

impl Package {
    fn parse(dir: &str, text: &str) -> Option<Package> {
        let root = jsonc::parse(text, "package.json").root?;
        root.as_object()?;
        let mut p = Package {
            dir: dir.to_owned(),
            name: root.get("name").and_then(Value::as_str).map(str::to_owned),
            is_module: root.get("type").and_then(Value::as_str) == Some("module"),
            ..Package::default()
        };
        // Every string field can be a main field (`--main-fields` is free-form).
        for m in root.as_object()? {
            if let jsonc::ValueKind::String(s) = &m.value.kind {
                p.mains.insert(m.key.clone(), s.clone());
            }
        }
        if let Some(b) = root.get("browser").and_then(Value::as_object) {
            for m in b {
                let v = match &m.value.kind {
                    jsonc::ValueKind::String(s) => Some(s.clone()),
                    jsonc::ValueKind::Bool(false) => None,
                    _ => continue,
                };
                p.browser_map.insert(m.key.clone(), v);
            }
        }
        p.exports = root.get("exports").cloned();
        p.imports = root.get("imports").cloned();
        p.side_effects = match root.get("sideEffects").map(|v| &v.kind) {
            Some(jsonc::ValueKind::Bool(false)) => SideEffects::None,
            Some(jsonc::ValueKind::Array(items)) => SideEffects::Only(
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect(),
            ),
            Some(jsonc::ValueKind::String(s)) => SideEffects::Only(vec![s.clone()]),
            _ => SideEffects::Unknown,
        };
        Some(p)
    }

    /// Whether `file` (inside this package) may have side effects.
    pub fn has_side_effects(&self, file: &str) -> bool {
        match &self.side_effects {
            SideEffects::Unknown => true,
            SideEffects::None => false,
            SideEffects::Only(pats) => {
                let rel = file.strip_prefix(&self.dir).unwrap_or(file);
                let rel = rel.trim_start_matches('/');
                pats.iter().any(|p| side_effects_glob(p, rel))
            }
        }
    }
}

type ResolveKey = (String, String, ResolveKind);

thread_local! {
    /// Resolutions in progress on this thread (the remapping chain of the current one).
    static ACTIVE: RefCell<Vec<ResolveKey>> = const { RefCell::new(Vec::new()) };
}

/// A resolution in progress; leaves the chain on drop.
struct Active;

impl Active {
    /// `None` if `key` is already being resolved further up this thread's chain.
    fn enter(key: &ResolveKey) -> Option<Active> {
        ACTIVE.with(|a| {
            let mut a = a.borrow_mut();
            if a.contains(key) {
                return None;
            }
            a.push(key.clone());
            Some(Active)
        })
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        ACTIVE.with(|a| a.borrow_mut().pop());
    }
}

/// `sideEffects` array patterns: paths relative to the package, `*`/`**` globs; a pattern
/// without `/` matches the file name anywhere.
fn side_effects_glob(pattern: &str, rel: &str) -> bool {
    let p = pattern.trim_start_matches("./");
    if !p.contains('/') {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        return glob(p, name) || glob(p, rel);
    }
    glob(p, rel)
}

/// Minimal glob: `*` (no `/`), `**` (anything), `?`. Matched over (pattern, string)
/// positions, one row of the table per pattern position, so a pattern of many `*`s cannot
/// backtrack exponentially: O(pattern x string) time, O(string) memory.
fn glob(p: &str, s: &str) -> bool {
    let (p, s) = (p.as_bytes(), s.as_bytes());
    let n = s.len();
    // rows[i % 4][j]: whether `p[i..]` matches `s[j..]`. Row i reads rows i+1..=i+3.
    let mut rows = vec![vec![false; n + 1]; 4];
    for i in (0..=p.len()).rev() {
        let mut row = std::mem::take(&mut rows[i % 4]);
        let next = &rows[(i + 1) % 4];
        for j in (0..=n).rev() {
            row[j] = match p.get(i) {
                None => j == n,
                Some(b'*') if p.get(i + 1) == Some(&b'*') => {
                    let rest = if p.get(i + 2) == Some(&b'/') {
                        i + 3
                    } else {
                        i + 2
                    };
                    rows[rest % 4][j] || (j < n && row[j + 1])
                }
                Some(b'*') => next[j] || (j < n && s[j] != b'/' && row[j + 1]),
                Some(b'?') => j < n && s[j] != b'/' && next[j + 1],
                Some(&c) => j < n && s[j] == c && next[j + 1],
            };
        }
        rows[i % 4] = row;
    }
    rows[0][0]
}

const NODE_BUILTINS: &[&str] = &[
    "assert",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "constants",
    "crypto",
    "dgram",
    "diagnostics_channel",
    "dns",
    "domain",
    "events",
    "fs",
    "http",
    "http2",
    "https",
    "inspector",
    "module",
    "net",
    "os",
    "path",
    "perf_hooks",
    "process",
    "punycode",
    "querystring",
    "readline",
    "repl",
    "stream",
    "string_decoder",
    "sys",
    "timers",
    "tls",
    "trace_events",
    "tty",
    "url",
    "util",
    "v8",
    "vm",
    "wasi",
    "worker_threads",
    "zlib",
];

/// True for Node built-in modules (`fs`, `fs/promises`, `node:test`).
pub fn is_node_builtin(spec: &str) -> bool {
    if spec.starts_with("node:") {
        return true;
    }
    // Exact names only: `process/` (trailing slash) and `fs/x` are packages in
    // node_modules, as in Node's own `require`.
    NODE_BUILTINS.contains(&spec) || NODE_BUILTIN_SUBPATHS.contains(&spec)
}

/// Built-in modules with a subpath (Node 24's `module.builtinModules`).
const NODE_BUILTIN_SUBPATHS: &[&str] = &[
    "assert/strict",
    "dns/promises",
    "fs/promises",
    "inspector/promises",
    "path/posix",
    "path/win32",
    "readline/promises",
    "stream/consumers",
    "stream/promises",
    "stream/web",
    "timers/promises",
    "util/types",
];

/// The `paths`/`baseUrl` settings of one tsconfig (after `extends`).
#[derive(Debug)]
struct TsPaths {
    paths: Option<ts_r_config::PathsMap>,
    /// Directory `paths` substitutions are relative to.
    paths_base: String,
    base_url: Option<String>,
}

impl TsPaths {
    /// Candidate paths for `spec` from the best matching `paths` pattern: an exact key, else
    /// the `*` pattern with the longest prefix.
    fn candidates(&self, spec: &str) -> Vec<String> {
        let Some(paths) = &self.paths else {
            return Vec::new();
        };
        let mut best: Option<(&str, &[String], usize)> = None;
        for (pattern, subs) in &paths.0 {
            match pattern.split_once('*') {
                None if pattern == spec => {
                    best = Some(("", subs, usize::MAX));
                    break;
                }
                None => {}
                Some((pre, suf)) => {
                    if spec.len() >= pre.len() + suf.len()
                        && spec.starts_with(pre)
                        && spec.ends_with(suf)
                        && best.is_none_or(|(_, _, len)| pre.len() > len)
                    {
                        best = Some((&spec[pre.len()..spec.len() - suf.len()], subs, pre.len()));
                    }
                }
            }
        }
        let Some((star, subs, _)) = best else {
            return Vec::new();
        };
        subs.iter()
            .map(|s| path::resolve(&self.paths_base, &s.replacen('*', star, 1)))
            .collect()
    }
}

/// The bundler's resolver. `Send + Sync`; share one per build.
pub struct Resolver {
    fs: Arc<dyn FileSystem>,
    platform: Platform,
    conditions: Vec<String>,
    main_fields: Vec<String>,
    extensions: Vec<String>,
    external: Vec<String>,
    packages_external: bool,
    explicit_tsconfig: Option<String>,
    packages: RwLock<FxHashMap<String, Option<Arc<Package>>>>,
    results: RwLock<FxHashMap<(String, String, ResolveKind), Resolution>>,
    /// tsconfig path -> its `paths`/`baseUrl` (`None` when it sets neither).
    ts: RwLock<FxHashMap<String, Option<Arc<TsPaths>>>>,
    /// Directory -> nearest tsconfig.json.
    tsconfig_for_dir: RwLock<FxHashMap<String, Option<String>>>,
    /// Package entry chosen through `module` -> the package's `main` file, for esbuild's
    /// dual-package-hazard rule (a package that is also `require`d uses `main` everywhere).
    dual: RwLock<FxHashMap<String, String>>,
}

impl std::fmt::Debug for Resolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resolver")
            .field("platform", &self.platform)
            .finish()
    }
}

impl Resolver {
    pub fn new(fs: Arc<dyn FileSystem>, opts: &BuildOptions) -> Resolver {
        Resolver {
            fs,
            platform: opts.platform,
            conditions: opts.effective_conditions(),
            main_fields: opts.effective_main_fields(),
            extensions: opts.resolve_extensions.clone(),
            external: opts.external.clone(),
            packages_external: opts.packages_external,
            explicit_tsconfig: opts.tsconfig.clone(),
            packages: RwLock::default(),
            results: RwLock::default(),
            ts: RwLock::default(),
            tsconfig_for_dir: RwLock::default(),
            dual: RwLock::default(),
        }
    }

    /// `module`-field entries and the `main` file of the same package.
    pub fn dual_package_entries(&self) -> FxHashMap<String, String> {
        self.dual.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn fs(&self) -> &Arc<dyn FileSystem> {
        &self.fs
    }

    /// Forgets cached results (after files were added or removed).
    pub fn clear_caches(&self) {
        self.results
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.packages
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.ts.write().unwrap_or_else(|e| e.into_inner()).clear();
        self.tsconfig_for_dir
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// Is `spec` external by the user's patterns?
    pub fn is_external_spec(&self, spec: &str) -> bool {
        self.external.iter().any(|p| {
            wildcard_match(p, spec)
                || (!p.contains('*')
                    && !p.starts_with('.')
                    && !p.starts_with('/')
                    && spec.starts_with(p.as_str())
                    && spec.as_bytes().get(p.len()) == Some(&b'/'))
        })
    }

    /// Resolves `spec` imported from a file in `dir`.
    pub fn resolve(&self, spec: &str, dir: &str, kind: ResolveKind) -> Resolution {
        let key = (dir.to_owned(), spec.to_owned(), kind);
        if let Some(r) = self
            .results
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return r.clone();
        }
        // `imports` and `browser` remappings resolve again; a cycle among them ends here.
        let Some(_active) = Active::enter(&key) else {
            return Resolution::NotFound(format!(
                "Could not resolve {spec:?}: its package remappings form a cycle"
            ));
        };
        let r = self.resolve_uncached(spec, dir, kind);
        self.results
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, r.clone());
        r
    }

    fn resolve_uncached(&self, spec: &str, dir: &str, kind: ResolveKind) -> Resolution {
        if self.is_external_spec(spec) {
            return Resolution::External(spec.to_owned());
        }
        if spec.starts_with("data:")
            || spec.starts_with("http://")
            || spec.starts_with("https://")
            || spec.starts_with("//")
        {
            return Resolution::External(spec.to_owned());
        }
        let is_relative =
            spec.starts_with("./") || spec.starts_with("../") || spec == "." || spec == "..";
        if spec.starts_with('/')
            && let Some(f) = self.resolve_tsconfig_paths(spec, dir, true)
        {
            return self.finish(f);
        }
        if is_relative || spec.starts_with('/') {
            let target = if spec.starts_with('/') {
                path::normalize(spec)
            } else {
                path::resolve(dir, spec)
            };
            if let Some(r) = self.browser_replace_file(&target, dir) {
                return r;
            }
            let dir_only = spec == "."
                || spec == ".."
                || spec.ends_with('/')
                || spec.ends_with("/.")
                || spec.ends_with("/..");
            if dir_only {
                return match self.load_dir(&target) {
                    Some(f) => self.finish(f),
                    None => Resolution::NotFound(format!("Could not resolve {spec:?}")),
                };
            }
            return match self.load_file_or_dir(&target, true) {
                Some(f) => self.finish(f),
                None => {
                    // `./file.txt?query` and `./file.txt#hash` name `./file.txt` when no file
                    // has the suffix in its name.
                    if let Some(i) = spec.find(['?', '#'])
                        && i > 0
                        && let Resolution::File(f) = self.resolve_uncached(&spec[..i], dir, kind)
                    {
                        return Resolution::File(f);
                    }
                    Resolution::NotFound(format!("Could not resolve {spec:?}"))
                }
            };
        }
        if spec.starts_with('#') {
            if let Some(r) = self.resolve_imports_field(spec, dir, kind) {
                return r;
            }
            if let Some(f) = self.resolve_tsconfig_paths(spec, dir, false) {
                return self.finish(f);
            }
            return Resolution::NotFound(format!("Could not resolve {spec:?}"));
        }
        // Bare specifier.
        if let Some(f) = self.resolve_tsconfig_paths(spec, dir, false) {
            return self.finish(f);
        }
        if self.platform == Platform::Node && is_node_builtin(spec) {
            return Resolution::External(spec.to_owned());
        }
        if self.packages_external {
            return Resolution::External(spec.to_owned());
        }
        // The importing package's `browser` map can redirect or disable bare imports.
        if self.platform == Platform::Browser
            && let Some(pkg) = self.package_scope(dir)
            && let Some(rep) = pkg.browser_map.get(spec)
        {
            return match rep {
                None => Resolution::Disabled,
                Some(r) if r.starts_with("./") || r.starts_with("../") => {
                    match self.load_file_or_dir(&path::resolve(&pkg.dir, r), true) {
                        Some(f) => self.finish(f),
                        None => Resolution::NotFound(format!("Could not resolve {spec:?}")),
                    }
                }
                Some(r) => self.resolve(r, dir, kind),
            };
        }
        match self.resolve_package(spec, dir, kind) {
            Some(r) => r,
            None => {
                if is_node_builtin(spec) {
                    if self.platform == Platform::Browser {
                        return Resolution::NotFound(format!(
                            "Could not resolve {spec:?}: the package {spec:?} wasn't found on the file system but is built into node. Use platform node to bundle for node."
                        ));
                    }
                    return Resolution::External(spec.to_owned());
                }
                Resolution::NotFound(format!(
                    "Could not resolve {spec:?}: you can mark the path {spec:?} as external to exclude it from the bundle"
                ))
            }
        }
    }

    /// Real path plus the target package's `browser` file map.
    fn finish(&self, file: String) -> Resolution {
        let real = if file.contains("/node_modules/") {
            path::normalize_slashes(&self.fs.realpath(&file))
        } else {
            file
        };
        if self.platform == Platform::Browser
            && let Some(r) = self.browser_replace_exact(&real)
        {
            return r;
        }
        Resolution::File(real)
    }

    /// Applies the `browser` map of the package containing `file` to an import path before
    /// extension probing: the path as written, then with each implicit extension, then as a
    /// directory index.
    fn browser_replace_file(&self, file: &str, dir: &str) -> Option<Resolution> {
        if self.platform != Platform::Browser {
            return None;
        }
        let _ = dir;
        let pkg = self.package_scope(path::directory_of(file))?;
        if pkg.browser_map.is_empty() {
            return None;
        }
        let rel = file.strip_prefix(&pkg.dir)?.trim_start_matches('/');
        let rep = self.browser_lookup(&pkg, rel)?;
        Some(self.browser_target(&pkg, rep))
    }

    /// The `browser` map entry for a resolved file (exact key only).
    fn browser_replace_exact(&self, file: &str) -> Option<Resolution> {
        let pkg = self.package_scope(path::directory_of(file))?;
        if pkg.browser_map.is_empty() {
            return None;
        }
        let rel = file.strip_prefix(&pkg.dir)?.trim_start_matches('/');
        let rep = pkg
            .browser_map
            .get(&format!("./{rel}"))
            .or_else(|| pkg.browser_map.get(rel))?;
        Some(self.browser_target(&pkg, rep))
    }

    /// Looks up a package-relative path (no leading `./`) in the `browser` map.
    fn browser_lookup<'p>(&self, pkg: &'p Package, rel: &str) -> Option<&'p Option<String>> {
        let get = |k: &str| {
            pkg.browser_map
                .get(&format!("./{k}"))
                .or_else(|| pkg.browser_map.get(k))
        };
        if let Some(r) = get(rel) {
            return Some(r);
        }
        for ext in &self.extensions {
            if let Some(r) = get(&format!("{rel}{ext}")) {
                return Some(r);
            }
        }
        for ext in &self.extensions {
            let k = if rel.is_empty() {
                format!("index{ext}")
            } else {
                format!("{rel}/index{ext}")
            };
            if let Some(r) = get(&k) {
                return Some(r);
            }
        }
        None
    }

    fn browser_target(&self, pkg: &Package, rep: &Option<String>) -> Resolution {
        match rep {
            None => Resolution::Disabled,
            Some(r) if r.starts_with("./") || r.starts_with("../") || r.starts_with('/') => {
                match self.load_file_or_dir(&path::resolve(&pkg.dir, r), true) {
                    Some(f) => Resolution::File(f),
                    None => Resolution::NotFound(format!("Could not resolve {r:?}")),
                }
            }
            Some(r) => self.resolve(r, &pkg.dir, ResolveKind::Import),
        }
    }

    /// `paths` then `baseUrl` from the importer's tsconfig. `absolute` specifiers only try
    /// `paths` (patterns may start with `/`).
    fn resolve_tsconfig_paths(&self, spec: &str, dir: &str, absolute: bool) -> Option<String> {
        if dir.contains("/node_modules/") {
            return None;
        }
        let config = self.tsconfig_for(dir)?;
        let ts = self.ts_paths(&config)?;
        for c in ts.candidates(spec) {
            // A declaration file only describes types; the code is elsewhere.
            if let Some(f) = self.load_file_or_dir(&c, true)
                && ![".d.ts", ".d.mts", ".d.cts"].iter().any(|d| f.ends_with(d))
            {
                return Some(f);
            }
        }
        if absolute {
            return None;
        }
        let base = ts.base_url.as_ref()?;
        self.load_file_or_dir(&path::resolve(base, spec), true)
    }

    /// The nearest `tsconfig.json` (or `jsconfig.json`) for files in `dir`.
    pub fn tsconfig_for(&self, dir: &str) -> Option<String> {
        if let Some(t) = &self.explicit_tsconfig {
            return Some(t.clone());
        }
        if let Some(r) = self
            .tsconfig_for_dir
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(dir)
        {
            return r.clone();
        }
        let mut found = None;
        let mut d = dir.to_owned();
        loop {
            let ts = path::resolve(&d, "tsconfig.json");
            let js = path::resolve(&d, "jsconfig.json");
            if self.fs.file_exists(&ts) {
                found = Some(ts);
                break;
            }
            if self.fs.file_exists(&js) {
                found = Some(js);
                break;
            }
            let parent = path::directory_of(&d).to_owned();
            if parent.is_empty() || parent == d {
                break;
            }
            d = parent;
        }
        self.tsconfig_for_dir
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(dir.to_owned(), found.clone());
        found
    }

    fn ts_paths(&self, config: &str) -> Option<Arc<TsPaths>> {
        if let Some(r) = self
            .ts
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(config)
        {
            return r.clone();
        }
        let parsed = ts_r_config::load_tsconfig(self.fs.as_ref(), config);
        let o = &parsed.options;
        let base_url = o.base_url().map(str::to_owned);
        let paths = o.paths().cloned();
        let r = (paths.is_some() || base_url.is_some()).then(|| {
            let paths_base = base_url
                .clone()
                .or_else(|| o.paths_base_path.clone())
                .unwrap_or_else(|| path::directory_of(config).to_owned());
            Arc::new(TsPaths {
                paths,
                paths_base,
                base_url,
            })
        });
        self.ts
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(config.to_owned(), r.clone());
        r
    }

    fn conditions_for(&self, kind: ResolveKind) -> Vec<&str> {
        let mut v: Vec<&str> = self.conditions.iter().map(String::as_str).collect();
        v.push(match kind {
            ResolveKind::Import => "import",
            ResolveKind::Require => "require",
        });
        v
    }

    fn resolve_imports_field(
        &self,
        spec: &str,
        dir: &str,
        kind: ResolveKind,
    ) -> Option<Resolution> {
        let pkg = self.package_scope(dir)?;
        let imports = pkg.imports.as_ref()?;
        let conds = self.conditions_for(kind);
        let cond = |c: &str| conds.contains(&c);
        let (v, rep) = ts_r_config::exports::match_subpath(imports, spec)?;
        let target = ts_r_config::exports::resolve_target(
            v,
            rep.as_deref(),
            &cond,
            ts_r_config::exports::MapKind::Imports,
            &mut |t| Some(t.to_owned()),
        )
        .found()?;
        if target.starts_with("./") {
            let f = self.load_file(&path::resolve(&pkg.dir, &target))?;
            Some(self.finish(f))
        } else {
            Some(self.resolve(&target, &pkg.dir, kind))
        }
    }

    fn resolve_package(&self, spec: &str, dir: &str, kind: ResolveKind) -> Option<Resolution> {
        let (name, sub) = ts_r_resolver::split_package_name(spec);
        let sub = sub.trim_start_matches('/');
        // Self-reference through `exports`.
        if let Some(pkg) = self.package_scope(dir)
            && pkg.name.as_deref() == Some(name)
            && let Some(exports) = &pkg.exports
            && let Some(f) = self.resolve_exports(&pkg, exports, sub, kind)
        {
            return Some(self.finish(f));
        }
        let mut cur = Some(dir.to_owned());
        while let Some(d) = cur {
            if !d.ends_with("/node_modules") {
                let pkg_dir = path::resolve(&path::resolve(&d, "node_modules"), name);
                if self.fs.directory_exists(&pkg_dir) {
                    if let Some(r) = self.resolve_in_package(&pkg_dir, sub, kind) {
                        return Some(r);
                    }
                    let pkg = self.package_json(&pkg_dir);
                    if pkg.as_ref().is_some_and(|p| p.exports.is_some()) {
                        return Some(Resolution::NotFound(format!(
                            "Could not resolve {spec:?}: the path {:?} is not exported by package {name:?}",
                            if sub.is_empty() {
                                ".".to_owned()
                            } else {
                                format!("./{sub}")
                            }
                        )));
                    }
                }
            }
            let parent = path::directory_of(&d).to_owned();
            cur = (parent != d && !parent.is_empty()).then_some(parent);
        }
        None
    }

    fn resolve_in_package(
        &self,
        pkg_dir: &str,
        sub: &str,
        kind: ResolveKind,
    ) -> Option<Resolution> {
        let pkg = self.package_json(pkg_dir);
        if let Some(pkg) = &pkg
            && let Some(exports) = &pkg.exports
        {
            return self
                .resolve_exports(pkg, exports, sub, kind)
                .map(|f| self.finish(f));
        }
        if self.platform == Platform::Browser
            && !sub.is_empty()
            && let Some(pkg) = &pkg
            && let Some(rep) = self.browser_lookup(pkg, sub)
        {
            return Some(self.browser_target(pkg, rep));
        }
        if sub.is_empty() {
            if let Some(pkg) = &pkg {
                for field in &self.main_fields {
                    if field == "browser" && self.platform != Platform::Browser {
                        continue;
                    }
                    // `require()` of a package with both fields gets `main` (esbuild's
                    // dual-package-hazard rule).
                    if field == "module"
                        && kind == ResolveKind::Require
                        && (pkg.mains.contains_key("main") || self.load_index(pkg_dir).is_some())
                    {
                        continue;
                    }
                    if let Some(m) = pkg.mains.get(field)
                        && let Some(f) = self.load_file_or_dir(&path::resolve(pkg_dir, m), false)
                    {
                        let r = self.finish(f);
                        if field == "module"
                            && let Resolution::File(module_file) = &r
                        {
                            let main = pkg
                                .mains
                                .get("main")
                                .and_then(|m| {
                                    self.load_file_or_dir(&path::resolve(pkg_dir, m), false)
                                })
                                .or_else(|| self.load_index(pkg_dir));
                            if let Some(main) = main
                                && let Resolution::File(main) = self.finish(main)
                                && main != *module_file
                            {
                                self.dual
                                    .write()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .insert(module_file.clone(), main);
                            }
                        }
                        return Some(r);
                    }
                }
            }
            return self.load_index(pkg_dir).map(|f| self.finish(f));
        }
        self.load_file_or_dir(&path::resolve(pkg_dir, sub), true)
            .map(|f| self.finish(f))
    }

    fn resolve_exports(
        &self,
        pkg: &Package,
        exports: &Value,
        sub: &str,
        kind: ResolveKind,
    ) -> Option<String> {
        let key = if sub.is_empty() {
            ".".to_owned()
        } else {
            format!("./{sub}")
        };
        let conds = self.conditions_for(kind);
        let cond = |c: &str| conds.contains(&c);
        let dir = pkg.dir.clone();
        // Deprecated folder mappings (`"./foo/": "./dir/"`) probe extensions like esbuild.
        let folder = exports.as_object().is_some_and(|ms| {
            ms.iter()
                .any(|m| m.key.ends_with('/') && key.starts_with(m.key.as_str()))
        });
        let mut try_target = |t: &str| {
            let f = path::resolve(&dir, t);
            if self.fs.file_exists(&f) {
                Some(f)
            } else if folder {
                self.load_file(&f)
            } else {
                None
            }
        };
        if let Some(f) =
            ts_r_config::exports::evaluate_exports(exports, &key, &cond, &mut try_target).found()
        {
            return Some(f);
        }
        // esbuild also lets a pattern's `*` match the empty string (`"./foo*"` for `./foo`).
        let ms = exports.as_object()?;
        for m in ms {
            if let Some((pre, post)) = m.key.split_once('*')
                && key.len() == pre.len() + post.len()
                && key.starts_with(pre)
                && key.ends_with(post)
            {
                return ts_r_config::exports::resolve_target(
                    &m.value,
                    Some(""),
                    &cond,
                    ts_r_config::exports::MapKind::Exports,
                    &mut try_target,
                )
                .found();
            }
        }
        None
    }

    // ---- files ----

    /// Exact file, extension probing, `.js` -> `.ts` rewrites, then directory.
    fn load_file_or_dir(&self, p: &str, probe: bool) -> Option<String> {
        if let Some(f) = self.load_file(p) {
            return Some(f);
        }
        if probe || self.fs.directory_exists(p) {
            return self.load_dir(p);
        }
        None
    }

    fn load_file(&self, p: &str) -> Option<String> {
        if self.fs.file_exists(p) {
            return Some(p.to_owned());
        }
        for ext in &self.extensions {
            let cand = format!("{p}{ext}");
            if self.fs.file_exists(&cand) {
                return Some(cand);
            }
        }
        // TypeScript-style imports: `./x.js` may mean `./x.ts`.
        for (from, tos) in [
            (".js", &[".ts", ".tsx"][..]),
            (".jsx", &[".tsx"][..]),
            (".mjs", &[".mts"][..]),
            (".cjs", &[".cts"][..]),
        ] {
            if let Some(base) = p.strip_suffix(from) {
                for to in tos {
                    let cand = format!("{base}{to}");
                    if self.fs.file_exists(&cand) {
                        return Some(cand);
                    }
                }
            }
        }
        None
    }

    fn load_dir(&self, p: &str) -> Option<String> {
        if !self.fs.directory_exists(p) {
            return None;
        }
        if let Some(pkg) = self.package_json(p) {
            for field in &self.main_fields {
                if field == "browser" && self.platform != Platform::Browser {
                    continue;
                }
                if let Some(m) = pkg.mains.get(field)
                    && let Some(f) = self.load_file_or_dir(&path::resolve(p, m), false)
                {
                    return Some(f);
                }
            }
        }
        self.load_index(p)
    }

    fn load_index(&self, dir: &str) -> Option<String> {
        self.load_file(&path::resolve(dir, "index"))
    }

    // ---- package.json ----

    /// The `package.json` in exactly `dir`.
    pub fn package_json(&self, dir: &str) -> Option<Arc<Package>> {
        if let Some(p) = self
            .packages
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(dir)
        {
            return p.clone();
        }
        let file = path::resolve(dir, "package.json");
        let p = self
            .fs
            .read_file(&file)
            .and_then(|t| Package::parse(dir, &t))
            .map(Arc::new);
        self.packages
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(dir.to_owned(), p.clone());
        p
    }

    /// The nearest `package.json` at or above `dir`.
    pub fn package_scope(&self, dir: &str) -> Option<Arc<Package>> {
        let mut d = dir.to_owned();
        loop {
            if !d.ends_with("/node_modules")
                && let Some(p) = self.package_json(&d)
            {
                return Some(p);
            }
            let parent = path::directory_of(&d).to_owned();
            if parent == d || parent.is_empty() {
                return None;
            }
            d = parent;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_r_config::MemoryFs;

    fn resolver(fs: MemoryFs, platform: Platform) -> Resolver {
        let opts = BuildOptions {
            platform,
            ..BuildOptions::default()
        };
        Resolver::new(Arc::new(fs), &opts)
    }

    /// Ecosystem (readable-stream, whatwg-url, tr46): `require('process/')` and
    /// `require('punycode/')` name the npm packages, not the built-ins.
    #[test]
    fn trailing_slash_and_unknown_subpaths_are_packages() {
        let mut fs = MemoryFs::new();
        fs.add_file(
            "/p/node_modules/process/package.json",
            r#"{"main":"./index.js"}"#,
        )
        .add_file("/p/node_modules/process/index.js", "")
        .add_file("/p/node_modules/fs/x.js", "");
        let r = resolver(fs, Platform::Node);
        let file = |p: &str| Resolution::File(p.into());
        let ext = |p: &str| Resolution::External(p.into());
        let res = |s: &str| r.resolve(s, "/p", ResolveKind::Require);
        assert_eq!(res("process/"), file("/p/node_modules/process/index.js"));
        assert_eq!(res("process"), ext("process"));
        assert_eq!(res("fs/x"), file("/p/node_modules/fs/x.js"));
        assert_eq!(res("fs/promises"), ext("fs/promises"));
        assert_eq!(res("path/posix"), ext("path/posix"));
        assert_eq!(res("node:fs/promises"), ext("node:fs/promises"));
        assert_eq!(res("node:test"), ext("node:test"));
    }

    #[test]
    fn relative_and_extensions() {
        let mut fs = MemoryFs::new();
        fs.add_file("/p/src/a.ts", "")
            .add_file("/p/src/dir/index.tsx", "")
            .add_file("/p/src/b.js", "");
        let r = resolver(fs, Platform::Browser);
        assert_eq!(
            r.resolve("./a.js", "/p/src", ResolveKind::Import),
            Resolution::File("/p/src/a.ts".into())
        );
        assert_eq!(
            r.resolve("./dir", "/p/src", ResolveKind::Import),
            Resolution::File("/p/src/dir/index.tsx".into())
        );
        assert_eq!(
            r.resolve("./b", "/p/src", ResolveKind::Import),
            Resolution::File("/p/src/b.js".into())
        );
        assert!(matches!(
            r.resolve("./nope", "/p/src", ResolveKind::Import),
            Resolution::NotFound(_)
        ));
    }

    #[test]
    fn packages_exports_and_main_fields() {
        let mut fs = MemoryFs::new();
        fs.add_file(
            "/p/node_modules/x/package.json",
            r#"{"name":"x","exports":{".":{"browser":"./b.js","import":"./i.mjs","require":"./r.cjs"},"./sub":"./sub.js"}}"#,
        )
        .add_file("/p/node_modules/x/b.js", "")
        .add_file("/p/node_modules/x/i.mjs", "")
        .add_file("/p/node_modules/x/r.cjs", "")
        .add_file("/p/node_modules/x/sub.js", "")
        .add_file(
            "/p/node_modules/y/package.json",
            r#"{"name":"y","main":"main.js","module":"mod.js","browser":{"./main.js":"./browser.js","fs":false}}"#,
        )
        .add_file("/p/node_modules/y/main.js", "")
        .add_file("/p/node_modules/y/mod.js", "")
        .add_file("/p/node_modules/y/browser.js", "");
        let rb = resolver(fs.clone(), Platform::Browser);
        assert_eq!(
            rb.resolve("x", "/p/src", ResolveKind::Import),
            Resolution::File("/p/node_modules/x/b.js".into())
        );
        assert_eq!(
            rb.resolve("x/sub", "/p/src", ResolveKind::Import),
            Resolution::File("/p/node_modules/x/sub.js".into())
        );
        assert!(matches!(
            rb.resolve("x/private", "/p/src", ResolveKind::Import),
            Resolution::NotFound(_)
        ));
        // An object `browser` field is a map, not a main field: `module` comes first.
        assert_eq!(
            rb.resolve("y", "/p/src", ResolveKind::Import),
            Resolution::File("/p/node_modules/y/mod.js".into())
        );
        // The map applies to `main` when that is what resolves.
        assert_eq!(
            rb.resolve("./main.js", "/p/node_modules/y", ResolveKind::Import),
            Resolution::File("/p/node_modules/y/browser.js".into())
        );
        assert_eq!(
            rb.resolve("fs", "/p/node_modules/y", ResolveKind::Require),
            Resolution::Disabled
        );
        let rn = resolver(fs, Platform::Node);
        assert_eq!(
            rn.resolve("x", "/p/src", ResolveKind::Require),
            Resolution::File("/p/node_modules/x/r.cjs".into())
        );
        assert_eq!(
            rn.resolve("y", "/p/src", ResolveKind::Import),
            Resolution::File("/p/node_modules/y/main.js".into())
        );
        assert_eq!(
            rn.resolve("fs", "/p/src", ResolveKind::Import),
            Resolution::External("fs".into())
        );
    }

    #[test]
    fn tsconfig_paths_and_base_url() {
        let mut fs = MemoryFs::new();
        fs.add_file(
            "/p/tsconfig.json",
            r#"{ "compilerOptions": { "baseUrl": "./src", "paths": { "@/*": ["lib/*", "alt/*"], "types": ["t.d.ts"] } } }"#,
        )
        .add_file("/p/src/lib/a.ts", "")
        .add_file("/p/src/alt/b.js", "")
        .add_file("/p/src/util/c.js", "")
        .add_file("/p/src/t.d.ts", "")
        .add_file("/p/node_modules/types/index.js", "");
        let r = resolver(fs, Platform::Browser);
        let file = |s: &str| Resolution::File(s.to_owned());
        assert_eq!(
            r.resolve("@/a", "/p/src", ResolveKind::Import),
            file("/p/src/lib/a.ts")
        );
        assert_eq!(
            r.resolve("@/b", "/p/src", ResolveKind::Import),
            file("/p/src/alt/b.js")
        );
        assert_eq!(
            r.resolve("util/c", "/p/src", ResolveKind::Import),
            file("/p/src/util/c.js")
        );
        // A declaration file has no code: fall through to node_modules.
        assert_eq!(
            r.resolve("types", "/p/src", ResolveKind::Import),
            file("/p/node_modules/types/index.js")
        );
    }

    #[test]
    fn side_effects_patterns() {
        let p = Package {
            dir: "/n/pkg".into(),
            side_effects: SideEffects::Only(vec!["*.css".into(), "./src/poly.js".into()]),
            ..Package::default()
        };
        assert!(p.has_side_effects("/n/pkg/dist/a.css"));
        assert!(p.has_side_effects("/n/pkg/src/poly.js"));
        assert!(!p.has_side_effects("/n/pkg/src/other.js"));
    }

    #[test]
    fn remapping_cycles_are_errors_not_stack_overflows() {
        let mut fs = MemoryFs::new();
        fs.add_file(
            "/p/package.json",
            r##"{"imports":{"#a":"#a","#b":"#c","#c":"#b"},"browser":{"x":"y","y":"x"}}"##,
        );
        let r = resolver(fs, Platform::Browser);
        for spec in ["#a", "#b", "x"] {
            match r.resolve(spec, "/p/src", ResolveKind::Import) {
                Resolution::NotFound(m) => assert!(m.contains("cycle"), "{m}"),
                other => panic!("{spec}: {other:?}"),
            }
        }
    }

    #[test]
    fn glob_semantics_and_no_exponential_backtracking() {
        for (p, s, want) in [
            ("*.css", "a.css", true),
            ("*.css", "d/a.css", false),
            ("src/**/*.js", "src/a/b/c.js", true),
            ("src/**/*.js", "src/c.js", true),
            ("src/**", "src/a/b", true),
            ("**/x.js", "a/x.js", true),
            ("a?c", "abc", true),
            ("a?c", "a/c", false),
            ("a*", "a/b", false),
            ("", "", true),
            ("abc", "ab", false),
        ] {
            assert_eq!(glob(p, s), want, "{p} vs {s}");
        }
        // Twenty `*a` and a `b` against forty `a`s: every alternative fails.
        let p = format!("{}b", "*a".repeat(20));
        let s = format!("{}.js", "a".repeat(40));
        assert!(!glob(&p, &s));
        assert!(!glob(&format!("{}b", "**a".repeat(20)), &s));
    }
}
