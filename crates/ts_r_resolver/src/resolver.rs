//! The module and type-reference resolver.
//!
//! Behaviour follows tsc 7's observable results (`--traceResolution`), not its source.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, OnceLock, RwLock};

use ts_r_config::exports::{Lookup, MapKind, is_subpath_map, match_subpath};
use ts_r_config::jsonc::{Value, ValueKind};
use ts_r_config::{FileSystem, ModuleKind, path};

use crate::options::ResolverOptions;
use crate::package_json::PackageJson;
use crate::trace::{Trace, Tracer, tr};

/// The TypeScript version `typesVersions` and `types@<range>` conditions are matched
/// against.
pub const TS_VERSION: (u64, u64, u64) = (7, 0, 2);

/// Whether an import is resolved with `import` or `require` conditions (and, for
/// node16/nodenext, ESM's "full specifier" rules).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ResolutionMode {
    /// ESM `import` semantics.
    Import,
    /// CommonJS `require` semantics.
    Require,
}

/// The module format of a file under node16/nodenext rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleFormat {
    /// ECMAScript module.
    Esm,
    /// CommonJS.
    CommonJs,
}

/// The syntax an import comes from (decides its [`ResolutionMode`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    /// `import ... from "x"`, `export ... from "x"`, `import "x"`, `import type`.
    Static,
    /// `import("x")`
    Dynamic,
    /// `import x = require("x")`
    ImportEquals,
    /// `require("x")` in a JS file.
    RequireCall,
}

/// File extension of a resolved module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Extension {
    /// `.ts`
    Ts,
    /// `.tsx`
    Tsx,
    /// `.d.ts` (also arbitrary-extension declarations like `.d.css.ts`)
    Dts,
    /// `.mts`
    Mts,
    /// `.cts`
    Cts,
    /// `.d.mts`
    Dmts,
    /// `.d.cts`
    Dcts,
    /// `.js`
    Js,
    /// `.jsx`
    Jsx,
    /// `.mjs`
    Mjs,
    /// `.cjs`
    Cjs,
    /// `.json`
    Json,
}

impl Extension {
    /// The extension text, with the leading dot.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ts => ".ts",
            Self::Tsx => ".tsx",
            Self::Dts => ".d.ts",
            Self::Mts => ".mts",
            Self::Cts => ".cts",
            Self::Dmts => ".d.mts",
            Self::Dcts => ".d.cts",
            Self::Js => ".js",
            Self::Jsx => ".jsx",
            Self::Mjs => ".mjs",
            Self::Cjs => ".cjs",
            Self::Json => ".json",
        }
    }

    /// The extension of `file`, if it is one module resolution produces.
    pub fn of(file: &str) -> Option<Self> {
        if path::ext::is_declaration(file) {
            return Some(if file.ends_with(".d.mts") {
                Self::Dmts
            } else if file.ends_with(".d.cts") {
                Self::Dcts
            } else {
                Self::Dts
            });
        }
        Some(match path::ext::of(file) {
            ".ts" => Self::Ts,
            ".tsx" => Self::Tsx,
            ".mts" => Self::Mts,
            ".cts" => Self::Cts,
            ".js" => Self::Js,
            ".jsx" => Self::Jsx,
            ".mjs" => Self::Mjs,
            ".cjs" => Self::Cjs,
            ".json" => Self::Json,
            _ => return None,
        })
    }

    /// True for TypeScript source and declaration extensions.
    pub fn is_ts_like(self) -> bool {
        matches!(
            self,
            Self::Ts | Self::Tsx | Self::Dts | Self::Mts | Self::Cts | Self::Dmts | Self::Dcts
        )
    }

    /// True for declaration files.
    pub fn is_declaration(self) -> bool {
        matches!(self, Self::Dts | Self::Dmts | Self::Dcts)
    }
}

/// `name/sub/module.d.ts@version`, identifying a file inside a versioned package.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackageId {
    /// Package name from `package.json`.
    pub name: String,
    /// Path of the file relative to the package directory.
    pub sub_module_name: String,
    /// Package version from `package.json`.
    pub version: String,
}

impl fmt::Display for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}@{}", self.name, self.sub_module_name, self.version)
    }
}

/// A successful module resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModule {
    /// The file (symlinks resolved for `node_modules` results unless `preserveSymlinks`).
    pub resolved_file_name: String,
    /// The path before symlinks were resolved, when it differs.
    pub original_path: Option<String>,
    /// Extension of the resolved file.
    pub extension: Extension,
    /// True when found through a `node_modules` lookup, also when the real path of a linked
    /// workspace package is outside `node_modules` (tsc 7 does not emit it, observed).
    pub is_external_library_import: bool,
    /// Package identity for files inside a package with `name` and `version`.
    pub package_id: Option<PackageId>,
    /// The TypeScript extension the specifier was written with found the file (`./a.ts` →
    /// `a.ts`, `./a.ts` → `a.d.ts`); false for directory and `index` results, for a `paths`
    /// substitution that carries its own extension, and for `exports`/`imports` targets
    /// unless the `*` brought in the extension (tsc: TS5097, TS2876, TS2877).
    pub resolved_using_ts_extension: bool,
    /// The module is a source (or an output) of a referenced project and
    /// `resolved_file_name` is that project's declaration output for it: the source's path
    /// ([`crate::references`]). The output may not exist yet (tsc: TS6305).
    pub project_reference_source: Option<String>,
    /// For an `import` of a package whose `exports` lead to JavaScript only: the declaration
    /// file a lookup ignoring `exports` finds (tsc names it in TS7016's hint).
    pub alternate_result: Option<String>,
}

/// A successful type reference (`/// <reference types>` or a `types` entry) resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTypeReference {
    /// The declaration file (symlinks resolved unless `preserveSymlinks`).
    pub resolved_file_name: String,
    /// The path before symlinks were resolved, when it differs.
    pub original_path: Option<String>,
    /// Found in a type root (`typeRoots` or the default `node_modules/@types` dirs) rather
    /// than by the secondary `node_modules` search.
    pub primary: bool,
    /// Package identity (secondary lookups only, like tsc).
    pub package_id: Option<PackageId>,
    /// The file is inside a `node_modules` directory.
    pub is_external_library_import: bool,
}

// Extension categories.
const TS: u8 = 1;
const DECL: u8 = 2;
const JS: u8 = 4;
const JSON: u8 = 8;
/// Not an extension: marks the declaration pass over an `@types` package, whose
/// `types`/`typings`/`main` targets still load TypeScript files (observed: `typings: "lquery"`
/// finds `lquery.ts`; the package's own `index` and `<name>.d.ts` stay declaration-only).
const AT_TYPES_FIELDS: u8 = 16;

/// Extensions tried when a candidate has no recognised extension, in priority order.
const APPEND: &[(&str, u8, Extension)] = &[
    (".ts", TS, Extension::Ts),
    (".tsx", TS, Extension::Tsx),
    (".d.ts", DECL, Extension::Dts),
    (".js", JS, Extension::Js),
    (".jsx", JS, Extension::Jsx),
];

/// Replacement lists for a candidate's own extension (the extension is stripped first).
fn replacements(ext: &str) -> &'static [(&'static str, u8, Extension)] {
    match ext {
        ".js" => &[
            (".ts", TS, Extension::Ts),
            (".tsx", TS, Extension::Tsx),
            (".d.ts", DECL, Extension::Dts),
            (".js", JS, Extension::Js),
            (".jsx", JS, Extension::Jsx),
        ],
        ".jsx" => &[
            (".tsx", TS, Extension::Tsx),
            (".ts", TS, Extension::Ts),
            (".d.ts", DECL, Extension::Dts),
            (".jsx", JS, Extension::Jsx),
            (".js", JS, Extension::Js),
        ],
        ".mjs" => &[
            (".mts", TS, Extension::Mts),
            (".d.mts", DECL, Extension::Dmts),
            (".mjs", JS, Extension::Mjs),
        ],
        ".cjs" => &[
            (".cts", TS, Extension::Cts),
            (".d.cts", DECL, Extension::Dcts),
            (".cjs", JS, Extension::Cjs),
        ],
        ".ts" | ".d.ts" => &[
            (".ts", TS, Extension::Ts),
            (".tsx", TS, Extension::Tsx),
            (".d.ts", DECL, Extension::Dts),
        ],
        ".tsx" => &[
            (".tsx", TS, Extension::Tsx),
            (".ts", TS, Extension::Ts),
            (".d.ts", DECL, Extension::Dts),
        ],
        ".mts" | ".d.mts" => &[
            (".mts", TS, Extension::Mts),
            (".d.mts", DECL, Extension::Dmts),
        ],
        ".cts" | ".d.cts" => &[
            (".cts", TS, Extension::Cts),
            (".d.cts", DECL, Extension::Dcts),
        ],
        _ => &[],
    }
}

/// A file found by one of the loaders, before real-path and package-ID handling.
#[derive(Debug, Clone)]
struct Found {
    path: String,
    ext: Extension,
    /// Directory whose `package.json` led here (entry fields, or `index` next to it).
    via_package_json: Option<String>,
    /// Loaded as a directory module (`package.json` fields or `index`).
    from_directory: bool,
    /// A `typesVersions` substitution naming a declaration file that exists as written:
    /// tsc gives such a result no package ID (observed: `"foo": ["./types/foo.d.ts"]`).
    no_package_id: bool,
}

/// Per-call settings.
#[derive(Clone, Copy)]
struct Ctx<'c> {
    /// node16/nodenext in ESM mode: no extension guessing, no directory modules.
    esm_strict: bool,
    conditions: &'c [String],
    /// Package `exports` are read (off only for the trace's second look, see
    /// `resolve_uncached`).
    exports: bool,
}

type ModuleKey = (String, String, ResolutionMode);
type ModuleCache = HashMap<ModuleKey, Option<Arc<ResolvedModule>>>;
type TypeRefCache =
    HashMap<(Option<String>, String, ResolutionMode), Option<Arc<ResolvedTypeReference>>>;

/// Resolves module specifiers and type references for one set of compiler options.
///
/// Thread-safe: results and file-system probes are cached behind `RwLock`s, so one
/// resolver can be shared by every checker thread. Top-level module results are cached
/// by (containing directory, specifier, mode).
pub struct Resolver<F: FileSystem> {
    probes: Arc<Probes<F>>,
    options: ResolverOptions,
    import_conditions: Vec<String>,
    require_conditions: Vec<String>,
    modules: RwLock<ModuleCache>,
    type_refs: RwLock<TypeRefCache>,
    /// Resolvers for the files of each referenced project, with that project's options
    /// (index = [`crate::ProjectReferences`] project index), made on first use. Empty in
    /// such a resolver itself.
    redirects: Vec<OnceLock<Box<Resolver<F>>>>,
    /// `exports`/`imports` entries that could not be resolved for want of a project root
    /// (TS2209/TS2210), in the order met, without repeats.
    ambiguous_roots: std::sync::Mutex<AmbiguousRoots>,
}

#[derive(Default)]
struct AmbiguousRoots {
    entries: Vec<AmbiguousRoot>,
    seen: HashSet<AmbiguousRoot>,
}

/// An `exports` (TS2209) or `imports` (TS2210) entry of the program's own package that tsc
/// 7 does not resolve because the project root is unknown: `outDir` or `declarationDir`
/// set, no `rootDir` and no tsconfig (observed: "The project root is ambiguous, but is
/// required to resolve export map entry '.' in file 'package.json'", and the module is not
/// resolved).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AmbiguousRoot {
    /// An `imports` entry (TS2210), else `exports` (TS2209).
    pub imports: bool,
    /// What a `*` matched, or `.` for an exact key.
    pub entry: String,
    /// The `package.json` path.
    pub package_json: String,
}

/// The file system and what was asked of it: shared by a resolver and its project-reference
/// resolvers, since the answers do not depend on compiler options.
struct Probes<F> {
    fs: F,
    files: RwLock<HashMap<String, bool>>,
    dirs: RwLock<HashMap<String, bool>>,
    package_jsons: RwLock<HashMap<String, Option<Arc<PackageJson>>>>,
    realpaths: RwLock<HashMap<String, String>>,
}

impl<F: FileSystem> fmt::Debug for Resolver<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resolver")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

/// How many package `imports` targets one resolution follows (see `resolve_module_at`).
const MAX_IMPORTS_DEPTH: u32 = 32;

fn cached<K: std::hash::Hash + Eq + Clone, V: Clone>(
    lock: &RwLock<HashMap<K, V>>,
    key: &K,
    compute: impl FnOnce() -> V,
) -> V {
    if let Some(v) = lock.read().unwrap_or_else(|e| e.into_inner()).get(key) {
        return v.clone();
    }
    let v = compute();
    lock.write()
        .unwrap_or_else(|e| e.into_inner())
        .entry(key.clone())
        .or_insert(v)
        .clone()
}

/// Splits `@scope/name/sub` into (`@scope/name`, `sub`) and `name/sub` into (`name`, `sub`).
pub fn split_package_name(spec: &str) -> (&str, &str) {
    let first = spec.find('/');
    let end = if spec.starts_with('@') {
        first.and_then(|i| spec[i + 1..].find('/').map(|j| i + 1 + j))
    } else {
        first
    };
    match end {
        Some(e) => (&spec[..e], &spec[e + 1..]),
        None => (spec, ""),
    }
}

/// `@scope/name` → `scope__name` (the `@types` directory name).
pub fn mangle_scoped_name(name: &str) -> String {
    match name.strip_prefix('@').and_then(|n| n.split_once('/')) {
        Some((scope, rest)) => format!("{scope}__{rest}"),
        None => name.to_owned(),
    }
}

/// True for relative specifiers that can only name a directory: `.`, `..`, a trailing `/`,
/// or a last segment of `.`/`..`.
fn is_directory_specifier(spec: &str) -> bool {
    spec.ends_with('/') || matches!(spec.rsplit('/').next(), Some("." | ".."))
}

/// The package directory (`…/node_modules/name` or `…/node_modules/@scope/name`) a file
/// is in, if any.
fn package_dir_of(file: &str) -> Option<String> {
    let i = file.rfind("/node_modules/")?;
    let after = &file[i + "/node_modules/".len()..];
    let mut segs = after.split('/');
    let first = segs.next()?;
    let len = if first.starts_with('@') {
        first.len() + 1 + segs.next()?.len()
    } else {
        first.len()
    };
    // The file itself must be below the package directory.
    if after.len() <= len {
        return None;
    }
    Some(file[..i + "/node_modules/".len() + len].to_owned())
}

/// Splits a package directory into its `node_modules` directory and package name
/// (`/a/node_modules/@s/p` → (`/a/node_modules`, `@s/p`)).
fn split_node_modules(pkg_dir: &str) -> (Option<&str>, &str) {
    match pkg_dir.rfind("/node_modules/") {
        Some(i) => (
            Some(&pkg_dir[..i + "/node_modules".len()]),
            &pkg_dir[i + "/node_modules/".len()..],
        ),
        None => (None, pkg_dir),
    }
}

/// Matches `spec` against a `paths`-style map: an exact key wins, then the pattern with the
/// longest prefix. Returns the matched key, its substitutions and the text matched by `*`.
fn match_paths<'m>(
    entries: impl Iterator<Item = (&'m str, &'m Value)> + Clone,
    spec: &str,
) -> Option<(&'m str, &'m Value, String)> {
    if let Some((k, v)) = entries
        .clone()
        .find(|(k, _)| !k.contains('*') && *k == spec)
    {
        return Some((k, v, String::new()));
    }
    let mut best: Option<(&'m str, &'m Value, String, usize)> = None;
    for (k, v) in entries {
        let Some((prefix, suffix)) = k.split_once('*') else {
            continue;
        };
        if spec.len() >= prefix.len() + suffix.len()
            && spec.starts_with(prefix)
            && spec.ends_with(suffix)
            && best
                .as_ref()
                .is_none_or(|(_, _, _, len)| prefix.len() > *len)
        {
            best = Some((
                k,
                v,
                spec[prefix.len()..spec.len() - suffix.len()].to_owned(),
                prefix.len(),
            ));
        }
    }
    best.map(|(k, v, m, _)| (k, v, m))
}

fn match_paths_map<'m>(
    map: &'m ts_r_config::PathsMap,
    spec: &str,
) -> Option<(&'m str, &'m [String], String)> {
    if let Some((k, subs)) = map.0.iter().find(|(k, _)| !k.contains('*') && k == spec) {
        return Some((k, subs, String::new()));
    }
    let mut best: Option<(&'m str, &'m [String], String, usize)> = None;
    for (k, subs) in &map.0 {
        let Some((prefix, suffix)) = k.split_once('*') else {
            continue;
        };
        if spec.len() >= prefix.len() + suffix.len()
            && spec.starts_with(prefix)
            && spec.ends_with(suffix)
            && best
                .as_ref()
                .is_none_or(|(_, _, _, len)| prefix.len() > *len)
        {
            best = Some((
                k,
                subs,
                spec[prefix.len()..spec.len() - suffix.len()].to_owned(),
                prefix.len(),
            ));
        }
    }
    best.map(|(k, v, m, _)| (k, v, m))
}

/// tsc's names for a set of extension categories (`TypeScript, JavaScript, Declaration, JSON`).
fn exts_name(exts: u8) -> String {
    let mut v = Vec::with_capacity(4);
    if exts & TS != 0 {
        v.push("TypeScript");
    }
    if exts & JS != 0 {
        v.push("JavaScript");
    }
    if exts & DECL != 0 {
        v.push("Declaration");
    }
    if exts & JSON != 0 {
        v.push("JSON");
    }
    v.join(", ")
}

/// `dir` with a trailing `/` (tsc shows directory specifiers so).
fn with_slash(dir: &str) -> String {
    if dir.ends_with('/') {
        dir.to_owned()
    } else {
        format!("{dir}/")
    }
}

/// The category of a TypeScript source or declaration extension (probed as written in
/// `package.json` fields and `exports` targets), if it is one.
fn ts_ext_category(ext: &str) -> Option<(u8, Extension)> {
    Some(match ext {
        ".ts" => (TS, Extension::Ts),
        ".tsx" => (TS, Extension::Tsx),
        ".mts" => (TS, Extension::Mts),
        ".cts" => (TS, Extension::Cts),
        ".d.ts" => (DECL, Extension::Dts),
        ".d.mts" => (DECL, Extension::Dmts),
        ".d.cts" => (DECL, Extension::Dcts),
        _ => return None,
    })
}

/// A specifier tsc never looks up as a file: `node:fs`, `https://...` (a URI scheme).
fn looks_like_uri(spec: &str) -> bool {
    let Some(colon) = spec.find(':') else {
        return false;
    };
    let scheme = &spec[..colon];
    scheme.len() > 1
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Where [`Resolver::load_directory`] gets the directory's `package.json` from.
#[derive(Clone)]
enum PackageJsonSource {
    /// Look it up and trace the lookup.
    Probe,
    /// Already looked up.
    Known(Option<Arc<PackageJson>>),
    /// The directory has none; the package's (whose `typesVersions` tsc shows) is given.
    Versions(Arc<PackageJson>),
}

/// Whether `pj` has `typesVersions`, in tsc's words.
fn types_versions_line(pj: &PackageJson, tr: &mut Tracer<'_>) {
    if pj.types_versions.is_some() {
        tr!(
            tr,
            "'package.json' has a 'typesVersions' field with version-specific path mappings."
        );
    } else {
        tr!(tr, "'package.json' does not have a 'typesVersions' field.");
    }
}

/// A resolved file with the package identity found with it.
type FoundInPackage = (Found, String, Option<PackageId>);

impl<F: FileSystem> Resolver<F> {
    /// Creates a resolver over `fs`.
    pub fn new(fs: F, options: ResolverOptions) -> Self {
        let probes = Arc::new(Probes {
            fs,
            files: RwLock::default(),
            dirs: RwLock::default(),
            package_jsons: RwLock::default(),
            realpaths: RwLock::default(),
        });
        let projects = options
            .project_references
            .as_ref()
            .map_or(0, |r| r.project_count());
        let mut r = Self::with_probes(probes, options);
        r.redirects = (0..projects).map(|_| OnceLock::new()).collect();
        r
    }

    fn with_probes(probes: Arc<Probes<F>>, options: ResolverOptions) -> Self {
        let node = options.is_node_algorithm();
        let make = |first: &str| {
            let mut c = vec![first.to_owned(), "types".to_owned()];
            if node {
                c.push("node".to_owned());
            }
            c.extend(options.custom_conditions.iter().cloned());
            c
        };
        Self {
            import_conditions: make("import"),
            require_conditions: make("require"),
            probes,
            options,
            modules: RwLock::default(),
            type_refs: RwLock::default(),
            redirects: Vec::new(),
            ambiguous_roots: std::sync::Mutex::default(),
        }
    }

    /// The resolver for requests in `file` when it belongs to a referenced project (one of
    /// its files, or a declaration output standing in for one): tsc resolves them with that
    /// project's compiler options ("Using compiler options of project reference redirect").
    /// Also returns that project's config path.
    fn redirect_for(&self, file: &str) -> Option<(&Self, &str)> {
        let refs = self.options.project_references.as_ref()?;
        let (index, config) = refs.project_of(file).or_else(|| {
            let linked = self.linked_reference_output(file)?;
            refs.project_of(&linked)
        })?;
        let slot = self.redirects.get(index)?;
        let r = slot.get_or_init(|| {
            let mut options = refs.project_options(index).clone();
            options.project_references = Some(refs.clone());
            Box::new(Self::with_probes(self.probes.clone(), options))
        });
        Some((r, config))
    }

    /// The settings in use.
    pub fn options(&self) -> &ResolverOptions {
        &self.options
    }

    /// The underlying file system.
    pub fn fs(&self) -> &F {
        &self.probes.fs
    }

    /// Export conditions used for `mode` (`import`/`require`, `types`, `node` for
    /// node16/nodenext, then `customConditions`).
    pub fn conditions(&self, mode: ResolutionMode) -> &[String] {
        match mode {
            ResolutionMode::Import => &self.import_conditions,
            ResolutionMode::Require => &self.require_conditions,
        }
    }

    // ------------------------------------------------------------------ file system

    fn dir_exists(&self, dir: &str) -> bool {
        cached(&self.probes.dirs, &dir.to_owned(), || {
            self.probes.fs.directory_exists(dir)
        })
    }

    fn file_exists(&self, file: &str) -> bool {
        // A declaration output of a referenced project exists while its source does
        // (resolution before the project is built; `references`), also when reached through
        // a symlinked directory (a workspace package in `node_modules`).
        if let Some(refs) = &self.options.project_references {
            let output = if refs.is_declaration_output(file) {
                Some(file.to_owned())
            } else {
                self.linked_reference_output(file)
            };
            if let Some(src) = output.as_deref().and_then(|o| refs.source_of_output(o)) {
                let src = src.to_owned();
                if cached(&self.probes.files, &src, || {
                    self.probes.fs.file_exists(&src)
                }) {
                    return true;
                }
            }
        }
        let parent = path::directory_of(file);
        if !parent.is_empty() && parent != file && !self.dir_exists(parent) {
            return false;
        }
        cached(&self.probes.files, &file.to_owned(), || {
            self.probes.fs.file_exists(file)
        })
    }

    /// `file` in a `node_modules` directory that is a symlink, when its real path is a
    /// declaration output of a referenced project (which need not exist yet, so the real
    /// path of its directory is used).
    fn linked_reference_output(&self, file: &str) -> Option<String> {
        let refs = self.options.project_references.as_ref()?;
        if !file.contains("/node_modules/") {
            return None;
        }
        let real_dir = self.realpath_of_dir(path::directory_of(file))?;
        let real = path::combine(&real_dir, path::file_name(file));
        refs.is_declaration_output(&real).then_some(real)
    }

    /// The real path of `dir` when it differs (its longest existing ancestor is resolved).
    fn realpath_of_dir(&self, dir: &str) -> Option<String> {
        let mut existing = dir;
        let mut rest = Vec::new();
        while !self.dir_exists(existing) {
            let parent = path::directory_of(existing);
            if parent.is_empty() || parent == existing {
                return None;
            }
            rest.push(path::file_name(existing));
            existing = parent;
        }
        let mut real = cached(&self.probes.realpaths, &existing.to_owned(), || {
            self.probes.fs.realpath(existing)
        });
        if real == existing {
            return None;
        }
        for seg in rest.iter().rev() {
            real = path::combine(&real, seg);
        }
        Some(real)
    }

    /// A probe of a candidate file (traced).
    fn probe_file(&self, file: &str, tr: &mut Tracer<'_>) -> bool {
        let exists = self.file_exists(file);
        tr.file(file, exists);
        exists
    }

    fn realpath(&self, p: &str) -> String {
        let real = cached(&self.probes.realpaths, &p.to_owned(), || {
            self.probes.fs.realpath(p)
        });
        // A referenced project's declaration output reached through a symlink may not be
        // built yet: its real path is its real directory's.
        if real == p
            && let Some(r) = self.linked_reference_output(p)
        {
            return r;
        }
        real
    }

    fn realpath_traced(&self, p: &str, tr: &mut Tracer<'_>) -> String {
        let real = self.realpath(p);
        tr!(tr, "Resolving real path for '{p}', result '{real}'.");
        real
    }

    /// The parsed `package.json` in `dir`, if any (cached).
    pub fn package_json(&self, dir: &str) -> Option<Arc<PackageJson>> {
        cached(&self.probes.package_jsons, &dir.to_owned(), || {
            let file = path::combine(dir, "package.json");
            if !self.file_exists(&file) {
                return None;
            }
            let text = self.probes.fs.read_file(&file)?;
            PackageJson::parse(dir, &file, &text).map(Arc::new)
        })
    }

    /// [`Self::package_json`], traced as a lookup.
    fn package_json_traced(&self, dir: &str, tr: &mut Tracer<'_>) -> Option<Arc<PackageJson>> {
        let pj = self.package_json(dir);
        if tr.enabled() {
            let file = path::combine(dir, "package.json");
            let exists = pj.is_some() || self.file_exists(&file);
            tr.package_json(&file, exists);
        }
        pj
    }

    /// The `exports`/`imports` entries left unresolved for want of a project root
    /// ([`AmbiguousRoot`]), in the order met.
    pub fn ambiguous_roots(&self) -> Vec<AmbiguousRoot> {
        self.ambiguous_roots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .clone()
    }

    /// Whether resolving a target of the `exports`/`imports` of the package at `dir` needs
    /// a project root that is unknown: outputs are configured (`outDir`,
    /// `declarationDir`), there is neither `rootDir` nor a tsconfig, and the package is not
    /// inside `node_modules`.
    fn project_root_ambiguous(&self, dir: &str) -> bool {
        let o = &self.options;
        (o.out_dir.is_some() || o.declaration_dir.is_some())
            && o.root_dir.is_none()
            && !format!("{dir}/").contains("/node_modules/")
    }

    fn record_ambiguous_root(&self, imports: bool, entry: Option<&str>, dir: &str) {
        let root = AmbiguousRoot {
            imports,
            entry: entry.unwrap_or(".").to_owned(),
            package_json: path::combine(dir, "package.json"),
        };
        let mut roots = self
            .ambiguous_roots
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if roots.seen.insert(root.clone()) {
            roots.entries.push(root);
        }
    }

    /// A self-name of the package at `scope` whose `exports` entry has a relative target
    /// while the project root is ambiguous: recorded, and the module is not resolved.
    fn self_name_root_ambiguous(
        &self,
        spec: &str,
        scope: &PackageJson,
        ctx: Ctx<'_>,
        tr: &mut Tracer<'_>,
    ) -> bool {
        if !self.options.resolve_package_json_exports || !self.project_root_ambiguous(&scope.dir) {
            return false;
        }
        let (Some(name), Some(exports)) = (scope.name.as_deref(), scope.exports.as_ref()) else {
            return false;
        };
        let (pkg, rest) = split_package_name(spec);
        if pkg != name {
            return false;
        }
        let key = if rest.is_empty() {
            ".".to_owned()
        } else {
            format!("./{rest}")
        };
        let cond = |c: &str| self.condition_matches(c, ctx);
        let mut tried = false;
        let _ = self.evaluate_exports_traced(
            exports,
            &key,
            &scope.dir,
            &cond,
            tr,
            &mut |_: &str, _: &mut Tracer<'_>| {
                tried = true;
                None::<()>
            },
        );
        if !tried {
            return false;
        }
        let entry = if is_subpath_map(exports) {
            match_subpath_keyed(exports, &key).and_then(|(_, rep, _)| rep)
        } else {
            None
        };
        self.record_ambiguous_root(false, entry.as_deref(), &scope.dir);
        true
    }

    /// Every `package.json` path looked for so far (found or not), sorted: what a build's
    /// resolutions depend on besides the files themselves (E3 records them in the buildinfo).
    pub fn package_json_lookups(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .probes
            .package_jsons
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .map(|d| path::combine(d, "package.json"))
            .collect();
        v.sort();
        v
    }

    /// The nearest `package.json` at or above `dir`.
    pub fn package_scope(&self, dir: &str) -> Option<Arc<PackageJson>> {
        self.package_scope_traced(dir, &mut Tracer::off())
    }

    fn package_scope_traced(&self, dir: &str, tr: &mut Tracer<'_>) -> Option<Arc<PackageJson>> {
        let mut d = dir.to_owned();
        loop {
            if let Some(pj) = self.package_json_traced(&d, tr) {
                return Some(pj);
            }
            let parent = path::directory_of(&d).to_owned();
            if parent == d || parent.is_empty() {
                return None;
            }
            d = parent;
        }
    }

    // ------------------------------------------------------------------ module format

    /// Module format of `file` under node16/nodenext rules: `.mts`/`.mjs` are ESM,
    /// `.cts`/`.cjs` are CommonJS, anything else follows the nearest `package.json`'s
    /// `"type"`. Also returns that `package.json`'s directory when it decided.
    pub fn file_format(&self, file: &str) -> (ModuleFormat, Option<String>) {
        let name = path::file_name(file);
        if [".mts", ".mjs"].iter().any(|e| path::ext::has(name, e)) {
            return (ModuleFormat::Esm, None);
        }
        if [".cts", ".cjs"].iter().any(|e| path::ext::has(name, e)) {
            return (ModuleFormat::CommonJs, None);
        }
        match self.package_scope(path::directory_of(file)) {
            Some(pj) if pj.is_module() => (ModuleFormat::Esm, Some(pj.dir.clone())),
            Some(pj) => (ModuleFormat::CommonJs, Some(pj.dir.clone())),
            None => (ModuleFormat::CommonJs, None),
        }
    }

    /// True when `file` belongs to a referenced project compiled with an ES `module` kind
    /// ([`crate::ProjectReferences::is_esm`]).
    pub fn is_reference_esm(&self, file: &str) -> bool {
        self.options
            .project_references
            .as_ref()
            .is_some_and(|r| r.is_esm(file))
    }

    /// The resolution mode for an import of `kind` written in `file`.
    ///
    /// - `module` node16–nodenext: static imports follow the file's format, `import()` is
    ///   always `Import`, `import = require()` / `require()` are `Require`.
    /// - `module: commonjs`: `Require`, except in `.mts`/`.mjs` files.
    /// - Otherwise (bundler with ES modules / preserve): `require` forms are `Require`,
    ///   static imports in `.cts`/`.cjs` files are `Require`, the rest is `Import`.
    ///
    /// A file of a referenced project follows that project's `module`.
    pub fn mode_for(&self, file: &str, kind: ImportKind) -> ResolutionMode {
        if let Some((r, _)) = self.redirect_for(file) {
            return r.mode_for(file, kind);
        }
        match kind {
            ImportKind::ImportEquals | ImportKind::RequireCall => return ResolutionMode::Require,
            ImportKind::Dynamic if self.options.module != ModuleKind::CommonJS => {
                return ResolutionMode::Import;
            }
            _ => {}
        }
        if self.options.module.is_node() {
            return match self.file_format(file).0 {
                ModuleFormat::Esm => ResolutionMode::Import,
                ModuleFormat::CommonJs => ResolutionMode::Require,
            };
        }
        let name = path::file_name(file);
        if self.options.module == ModuleKind::CommonJS {
            // `.mts`/`.mjs` files stay ES modules (observed: `import` conditions for
            // `real-imports.mts` under `module: commonjs` + `bundler`).
            return if [".mts", ".mjs"].iter().any(|e| path::ext::has(name, e)) {
                ResolutionMode::Import
            } else {
                ResolutionMode::Require
            };
        }
        if [".cts", ".cjs"].iter().any(|e| path::ext::has(name, e)) {
            ResolutionMode::Require
        } else {
            ResolutionMode::Import
        }
    }

    // ------------------------------------------------------------------ public entry points

    /// Resolves `specifier` imported from `containing_file` in `mode`. Results are cached by
    /// (containing directory, specifier, mode).
    pub fn resolve_module(
        &self,
        specifier: &str,
        containing_file: &str,
        mode: ResolutionMode,
    ) -> Option<Arc<ResolvedModule>> {
        let r = self.redirect_for(containing_file).map_or(self, |(r, _)| r);
        r.resolve_module_with(specifier, containing_file, mode, &mut Tracer::off())
    }

    /// [`Self::resolve_module`] logging every step to `trace` in tsc 7's
    /// `--traceResolution` wording. Not cached: every call runs (and logs) in full.
    pub fn resolve_module_traced(
        &self,
        specifier: &str,
        containing_file: &str,
        mode: ResolutionMode,
        trace: &mut Trace,
    ) -> Option<Arc<ResolvedModule>> {
        let mut tr = Tracer::on(trace);
        tr!(
            tr,
            "======== Resolving module '{specifier}' from '{}'. ========",
            self.shown_containing_file(containing_file)
        );
        match self.redirect_for(containing_file) {
            Some((r, config)) => {
                tr!(
                    tr,
                    "Using compiler options of project reference redirect '{config}'."
                );
                r.resolve_module_traced_body(specifier, containing_file, mode, &mut tr)
            }
            None => self.resolve_module_traced_body(specifier, containing_file, mode, &mut tr),
        }
    }

    /// The containing file tsc names in a trace: a referenced project's declaration output
    /// is named by its source (its imports are resolved from there).
    fn shown_containing_file<'a>(&'a self, file: &'a str) -> &'a str {
        self.reference_source(file).unwrap_or(file)
    }

    /// The source of `file` when it is a declaration output of a referenced project (also
    /// through a symlinked `node_modules` directory).
    fn reference_source(&self, file: &str) -> Option<&str> {
        let refs = self.options.project_references.as_ref()?;
        if refs.is_declaration_output(file) {
            return refs.source_of_output(file);
        }
        refs.source_of_output(&self.linked_reference_output(file)?)
    }

    fn resolve_module_traced_body(
        &self,
        specifier: &str,
        containing_file: &str,
        mode: ResolutionMode,
        tr: &mut Tracer<'_>,
    ) -> Option<Arc<ResolvedModule>> {
        let kind = match self.options.algorithm {
            ts_r_config::ModuleResolution::Node16 => "Node16",
            ts_r_config::ModuleResolution::NodeNext => "NodeNext",
            _ => "Bundler",
        };
        if self.options.module_resolution_explicit {
            tr!(tr, "Explicitly specified module resolution kind: '{kind}'.");
        } else {
            tr!(
                tr,
                "Module resolution kind is not specified, using '{kind}'."
            );
        }
        let esm = self.options.is_node_algorithm() && mode == ResolutionMode::Import;
        tr!(
            tr,
            "Resolving in {} mode with conditions {}.",
            if esm { "ESM" } else { "CJS" },
            self.conditions(mode)
                .iter()
                .map(|c| format!("'{c}'"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        // The footer shows the file resolution found, before a referenced project's source
        // is swapped for its declaration output (as tsc shows it).
        let r = self.resolve_module_raw(specifier, containing_file, mode, tr);
        match &r {
            Some(m) => match &m.package_id {
                Some(id) => tr!(
                    tr,
                    "======== Module name '{specifier}' was successfully resolved to '{}' with Package ID '{id}'. ========",
                    m.resolved_file_name
                ),
                None => tr!(
                    tr,
                    "======== Module name '{specifier}' was successfully resolved to '{}'. ========",
                    m.resolved_file_name
                ),
            },
            None => tr!(
                tr,
                "======== Module name '{specifier}' was not resolved. ========"
            ),
        }
        r.map(|r| self.redirect_to_reference_output(r))
    }

    fn resolve_module_with(
        &self,
        specifier: &str,
        containing_file: &str,
        mode: ResolutionMode,
        tr: &mut Tracer<'_>,
    ) -> Option<Arc<ResolvedModule>> {
        let r = self.resolve_module_raw(specifier, containing_file, mode, tr)?;
        Some(self.redirect_to_reference_output(r))
    }

    fn resolve_module_raw(
        &self,
        specifier: &str,
        containing_file: &str,
        mode: ResolutionMode,
        tr: &mut Tracer<'_>,
    ) -> Option<Arc<ResolvedModule>> {
        // A referenced project's declaration output resolves its imports from its source's
        // directory (observed: `../core/x` in `logic/outDir/index.d.ts` finds `core/x`).
        let source = self.reference_source(containing_file);
        let dir = path::directory_of(source.unwrap_or(containing_file));
        self.resolve_module_at(specifier, dir, mode, 0, tr)
    }

    /// Like [`Self::resolve_module`] with the containing directory given directly.
    pub fn resolve_module_from_dir(
        &self,
        specifier: &str,
        dir: &str,
        mode: ResolutionMode,
    ) -> Option<Arc<ResolvedModule>> {
        let r = self.resolve_module_at(specifier, dir, mode, 0, &mut Tracer::off())?;
        Some(self.redirect_to_reference_output(r))
    }

    /// A result inside a referenced project (a source, or one of its outputs) becomes that
    /// source's declaration output ([`crate::references`]).
    fn redirect_to_reference_output(&self, r: Arc<ResolvedModule>) -> Arc<ResolvedModule> {
        let Some(refs) = &self.options.project_references else {
            return r;
        };
        let Some((dts, src)) = refs.redirect(&r.resolved_file_name) else {
            return r;
        };
        let name = path::file_name(dts);
        let extension = if path::ext::has(name, ".d.mts") {
            Extension::Dmts
        } else if path::ext::has(name, ".d.cts") {
            Extension::Dcts
        } else {
            Extension::Dts
        };
        Arc::new(ResolvedModule {
            resolved_file_name: dts.to_owned(),
            original_path: None,
            extension,
            is_external_library_import: false,
            package_id: r.package_id.clone(),
            resolved_using_ts_extension: r.resolved_using_ts_extension,
            project_reference_source: Some(src.to_owned()),
            alternate_result: None,
        })
    }

    /// `depth` counts package `imports` targets followed to get here (`#a` → `#b` → ...).
    /// Past [`MAX_IMPORTS_DEPTH`] the chain is a cycle (`#a` → `#b` → `#a`) or absurd, and
    /// resolution fails instead of overflowing the stack (tsc 7.0.2 overflows).
    fn resolve_module_at(
        &self,
        specifier: &str,
        dir: &str,
        mode: ResolutionMode,
        depth: u32,
        tr: &mut Tracer<'_>,
    ) -> Option<Arc<ResolvedModule>> {
        if depth > MAX_IMPORTS_DEPTH {
            return None;
        }
        // Recursive results depend on the remaining depth budget, absent from the key.
        if tr.enabled() || depth > 0 {
            return self
                .resolve_uncached(specifier, dir, mode, depth, tr)
                .map(Arc::new);
        }
        let key = (dir.to_owned(), specifier.to_owned(), mode);
        cached(&self.modules, &key, || {
            self.resolve_uncached(specifier, dir, mode, depth, &mut Tracer::off())
                .map(Arc::new)
        })
    }

    /// Resolves a type reference: `/// <reference types="name" />` from `containing_file`,
    /// or an entry of the `types` option when `containing_file` is `None`.
    ///
    /// Type roots are searched first (`typeRoots`, default every ancestor
    /// `node_modules/@types` of the config directory); then, unless `typeRoots` is set and
    /// this is a `types` entry, `node_modules` above the containing file (declarations only).
    pub fn resolve_type_reference(
        &self,
        name: &str,
        containing_file: Option<&str>,
        mode: ResolutionMode,
    ) -> Option<Arc<ResolvedTypeReference>> {
        if let Some((r, _)) = containing_file.and_then(|f| self.redirect_for(f)) {
            return r.resolve_type_reference(name, containing_file, mode);
        }
        let dir = containing_file.map(|f| path::directory_of(f).to_owned());
        let key = (dir.clone(), name.to_owned(), mode);
        cached(&self.type_refs, &key, || {
            self.resolve_type_reference_uncached(name, dir.as_deref(), mode, &mut Tracer::off())
                .map(Arc::new)
        })
    }

    /// [`Self::resolve_type_reference`] logging every step to `trace` in tsc 7's
    /// `--traceResolution` wording. A `types` entry (`containing_file` `None`) is shown as
    /// coming from `__inferred type names__.ts` in the config directory, as tsc shows it.
    pub fn resolve_type_reference_traced(
        &self,
        name: &str,
        containing_file: Option<&str>,
        mode: ResolutionMode,
        trace: &mut Trace,
    ) -> Option<Arc<ResolvedTypeReference>> {
        let mut tr = Tracer::on(trace);
        let redirect = containing_file.and_then(|f| self.redirect_for(f));
        let this = redirect.map_or(self, |(r, _)| r);
        let base_dir = this.type_base_dir();
        let roots = this.type_roots(&base_dir);
        let containing = match containing_file {
            Some(f) => self.shown_containing_file(f).to_owned(),
            None => path::combine(&base_dir, "__inferred type names__.ts"),
        };
        let shown = roots.join(",");
        tr!(
            tr,
            "======== Resolving type reference directive '{name}', containing file '{containing}', root directory '{shown}'. ========"
        );
        if let Some((_, config)) = redirect {
            tr!(
                tr,
                "Using compiler options of project reference redirect '{config}'."
            );
        }
        let dir = containing_file.map(|f| path::directory_of(f).to_owned());
        let r = this.resolve_type_reference_uncached(name, dir.as_deref(), mode, &mut tr);
        match &r {
            Some(t) => match &t.package_id {
                Some(id) => tr!(
                    tr,
                    "======== Type reference directive '{name}' was successfully resolved to '{}' with Package ID '{id}', primary: {}. ========",
                    t.resolved_file_name,
                    t.primary
                ),
                None => tr!(
                    tr,
                    "======== Type reference directive '{name}' was successfully resolved to '{}', primary: {}. ========",
                    t.resolved_file_name,
                    t.primary
                ),
            },
            None => tr!(
                tr,
                "======== Type reference directive '{name}' was not resolved. ========"
            ),
        }
        r.map(Arc::new)
    }

    /// Drops every cached result (call after files change).
    pub fn clear_caches(&self) {
        for c in [&self.probes.files, &self.probes.dirs] {
            c.write().unwrap_or_else(|e| e.into_inner()).clear();
        }
        self.modules
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.type_refs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.probes
            .package_jsons
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.probes
            .realpaths
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        for r in self.redirects.iter().filter_map(OnceLock::get) {
            r.clear_caches();
        }
    }

    // ------------------------------------------------------------------ resolution

    fn ctx(&self, mode: ResolutionMode) -> Ctx<'_> {
        Ctx {
            esm_strict: self.options.is_node_algorithm() && mode == ResolutionMode::Import,
            conditions: self.conditions(mode),
            exports: true,
        }
    }

    fn all_exts(&self) -> u8 {
        TS | DECL
            | JS
            | if self.options.resolve_json_module {
                JSON
            } else {
                0
            }
    }

    fn passes(&self) -> [u8; 2] {
        [
            TS | DECL,
            JS | if self.options.resolve_json_module {
                JSON
            } else {
                0
            },
        ]
    }

    fn resolve_uncached(
        &self,
        spec: &str,
        dir: &str,
        mode: ResolutionMode,
        depth: u32,
        tr: &mut Tracer<'_>,
    ) -> Option<ResolvedModule> {
        let ctx = self.ctx(mode);
        let using_ts_ext = Extension::of(spec).is_some_and(Extension::is_ts_like);
        if path::is_explicitly_relative(spec) || path::is_rooted(spec) {
            let candidate = path::resolve(dir, spec);
            let directory = is_directory_specifier(spec);
            let found = if !directory && !self.options.root_dirs.is_empty() {
                tr!(
                    tr,
                    "'rootDirs' option is set, using it to resolve relative module name '{spec}'."
                );
                self.try_root_dirs(&candidate, ctx, tr)
            } else {
                tr!(
                    tr,
                    "Loading module as file / folder, candidate module location '{}', target file types: {}.",
                    if directory {
                        with_slash(&candidate)
                    } else {
                        candidate.clone()
                    },
                    exts_name(self.all_exts())
                );
                if directory {
                    // `.`, `..`, `../..`, `./dir/`: directory modules only.
                    if ctx.esm_strict {
                        None
                    } else if !self.dir_exists(&candidate) {
                        tr.no_directory(&with_slash(&candidate));
                        None
                    } else {
                        self.load_directory(
                            &candidate,
                            self.all_exts(),
                            true,
                            0,
                            PackageJsonSource::Probe,
                            tr,
                        )
                    }
                } else {
                    self.load_file_or_dir(&candidate, self.all_exts(), ctx, tr)
                }
            }?;
            // Relative imports inside a package get its package ID, except directory modules
            // (observed: `./utils` → `utils/index.d.ts` has none).
            let package_id = if found.from_directory {
                None
            } else {
                package_dir_of(&found.path).and_then(|d| self.package_id(&d, &found, true, tr))
            };
            let using_ts_ext = using_ts_ext && !found.from_directory && found.ext.is_ts_like();
            return Some(self.finish(found, package_id, false, using_ts_ext, tr));
        }
        if let Some((found, candidate_ts_ext)) = self.try_paths(spec, ctx, tr) {
            if found.path.contains("/node_modules/") && !self.options.preserve_symlinks {
                self.realpath_traced(&found.path, tr);
            }
            let using_ts_ext = candidate_ts_ext && !found.from_directory && found.ext.is_ts_like();
            return Some(self.finish(found, None, false, using_ts_ext, tr));
        }
        // Under node16 `#/...` is not a valid `imports` specifier (observed: "Invalid import
        // specifier", then a plain package lookup; nodenext and bundler accept `#/*` keys).
        let invalid_import = self.options.algorithm == ts_r_config::ModuleResolution::Node16
            && (spec == "#" || spec.starts_with("#/"));
        if invalid_import {
            tr!(
                tr,
                "Invalid import specifier '{spec}' has no possible resolutions."
            );
        }
        let scope = self.package_scope_traced(dir, tr);
        if spec.starts_with('#') && !invalid_import {
            return self.resolve_imports(spec, scope.as_deref(), mode, ctx, depth, tr);
        }
        if let Some(scope) = &scope {
            if self.self_name_root_ambiguous(spec, scope, ctx, tr) {
                return None;
            }
            if let Some(r) = self.resolve_self_name(spec, scope, ctx, tr) {
                return Some(r);
            }
        }
        if looks_like_uri(spec) {
            tr!(
                tr,
                "Skipping module '{spec}' that looks like an absolute URI, target file types: {}.",
                exts_name(self.all_exts())
            );
            return None;
        }
        tr!(
            tr,
            "Loading module '{spec}' from 'node_modules' folder, target file types: {}.",
            exts_name(self.all_exts())
        );
        let Some((found, pkg_dir, package_id)) =
            self.node_modules_lookup(spec, dir, ctx, &self.passes(), true, tr)
        else {
            return self.resolve_from_type_roots(spec, ctx, tr);
        };
        let mut r = self.finish(found, package_id, true, false, tr);
        // An `import` of a package whose `exports` lead to JavaScript only: tsc looks again
        // without `exports` for declarations, and shows that lookup too. What it finds is
        // only a hint for diagnostics (`alternate_result`), never the resolution.
        if mode == ResolutionMode::Import
            && matches!(
                r.extension,
                Extension::Js | Extension::Jsx | Extension::Mjs | Extension::Cjs
            )
            && self.options.resolve_package_json_exports
            && self
                .package_json(&pkg_dir)
                .is_some_and(|p| p.exports.is_some())
        {
            tr!(
                tr,
                "Resolution of non-relative name failed; trying with modern Node resolution features disabled to see if npm library needs configuration update."
            );
            self.package_scope_traced(dir, tr);
            tr!(
                tr,
                "Loading module '{spec}' from 'node_modules' folder, target file types: {}.",
                exts_name(TS | DECL)
            );
            let loose = Ctx {
                exports: false,
                ..ctx
            };
            if let Some((f, _, id)) =
                self.node_modules_lookup(spec, dir, loose, &[TS | DECL], true, tr)
            {
                r.alternate_result = Some(self.finish(f, id, true, false, tr).resolved_file_name);
            }
        }
        Some(r)
    }

    /// A bare specifier node_modules did not resolve, looked up in custom `typeRoots`
    /// (declarations only, like a type reference; default roots are not searched again).
    /// Observed with tsc 7: `import "phaser"` finds `<typeRoot>/phaser/package.json`'s
    /// `types` (no package ID; a real path only inside `node_modules`). Strict ESM loads only
    /// package directories.
    fn resolve_from_type_roots(
        &self,
        spec: &str,
        ctx: Ctx<'_>,
        tr: &mut Tracer<'_>,
    ) -> Option<ResolvedModule> {
        let roots = self.options.type_roots.as_ref()?;
        for root in roots {
            if !self.dir_exists(root) {
                continue;
            }
            if let Some((found, _)) = self.load_from_type_root(spec, root, true, ctx.esm_strict, tr)
            {
                let in_node_modules = found.path.contains("/node_modules/");
                return Some(self.finish(found, None, in_node_modules, false, tr));
            }
        }
        None
    }

    /// Turns a loader result into a [`ResolvedModule`].
    fn finish(
        &self,
        found: Found,
        package_id: Option<PackageId>,
        from_node_modules: bool,
        using_ts_ext: bool,
        tr: &mut Tracer<'_>,
    ) -> ResolvedModule {
        let (resolved, original) = if from_node_modules && !self.options.preserve_symlinks {
            let real = self.realpath_traced(&found.path, tr);
            if real == found.path {
                (real, None)
            } else {
                (real, Some(found.path.clone()))
            }
        } else {
            (found.path.clone(), None)
        };
        ResolvedModule {
            // Found by a `node_modules` lookup, wherever its real path is: tsc 7 neither emits
            // nor rootDir-checks a workspace package linked into `node_modules` (observed).
            is_external_library_import: from_node_modules,
            resolved_file_name: resolved,
            original_path: original,
            extension: found.ext,
            package_id,
            resolved_using_ts_extension: using_ts_ext,
            project_reference_source: None,
            alternate_result: None,
        }
    }

    /// The package identity of `found`, a file in the package at `pkg_dir` (or in the
    /// directory module whose `package.json` led to it). `probe`: the `package.json` was not
    /// looked up on the way here (traced as a lookup).
    fn package_id(
        &self,
        pkg_dir: &str,
        found: &Found,
        probe: bool,
        tr: &mut Tracer<'_>,
    ) -> Option<PackageId> {
        let id_dir = found.via_package_json.as_deref().unwrap_or(pkg_dir);
        let pj = if probe {
            self.package_json_traced(id_dir, tr)
        } else {
            self.package_json(id_dir)
        }?;
        let (name, mut version) = (pj.name.clone()?, pj.version.clone()?);
        let cs = self.probes.fs.case_sensitive();
        if !path::contains(id_dir, &found.path, cs) {
            return None;
        }
        if pj.peer_dependencies.is_empty() {
            tr!(
                tr,
                "'package.json' does not have a 'peerDependencies' field."
            );
        } else {
            tr!(tr, "'package.json' has a 'peerDependencies' field.");
            // Peers installed next to the package (in the `node_modules` holding its real
            // path), sorted by name; missing ones are skipped.
            let real_dir = self.realpath_traced(id_dir, tr);
            let (nm, _) = split_node_modules(&real_dir);
            if let Some(nm) = nm {
                let mut peers: Vec<&String> = pj.peer_dependencies.iter().collect();
                peers.sort();
                for peer in peers {
                    if let Some(v) = self
                        .package_json_traced(&path::combine(nm, peer), tr)
                        .and_then(|p| p.version.clone())
                    {
                        tr!(tr, "Found peerDependency '{peer}' with '{v}' version.");
                        version.push_str(&format!("+{peer}@{v}"));
                    }
                }
            }
        }
        Some(PackageId {
            name,
            sub_module_name: path::relative(id_dir, &found.path, cs),
            version,
        })
    }

    fn try_root_dirs(&self, candidate: &str, ctx: Ctx<'_>, tr: &mut Tracer<'_>) -> Option<Found> {
        let cs = self.probes.fs.case_sensitive();
        let exts = self.all_exts();
        let with_slash = |r: &str| {
            if r.ends_with('/') {
                r.to_owned()
            } else {
                format!("{r}/")
            }
        };
        let mut matched: Option<&String> = None;
        for r in &self.options.root_dirs {
            let longest =
                path::contains(r, candidate, cs) && matched.is_none_or(|m| r.len() >= m.len());
            tr!(
                tr,
                "Checking if '{}' is the longest matching prefix for '{candidate}' - '{longest}'.",
                with_slash(r)
            );
            if longest {
                matched = Some(r);
            }
        }
        let loading = |tr: &mut Tracer<'_>, c: &str| {
            tr!(
                tr,
                "Loading module as file / folder, candidate module location '{c}', target file types: {}.",
                exts_name(exts)
            );
        };
        let Some(matched) = matched else {
            loading(tr, candidate);
            return self.load_file_or_dir(candidate, exts, ctx, tr);
        };
        let suffix = candidate[matched.len()..].trim_start_matches('/');
        tr!(
            tr,
            "Longest matching prefix for '{candidate}' is '{}'.",
            with_slash(matched)
        );
        tr!(
            tr,
            "Loading '{suffix}' from the root dir '{}', candidate location '{candidate}'.",
            with_slash(matched)
        );
        loading(tr, candidate);
        if let Some(f) = self.load_file_or_dir(candidate, exts, ctx, tr) {
            return Some(f);
        }
        tr!(tr, "Trying other entries in 'rootDirs'.");
        for root in &self.options.root_dirs {
            if root == matched {
                continue;
            }
            let c = if suffix.is_empty() {
                root.clone()
            } else {
                path::combine(root, suffix)
            };
            tr!(
                tr,
                "Loading '{suffix}' from the root dir '{root}', candidate location '{c}'."
            );
            loading(tr, &c);
            if let Some(f) = self.load_file_or_dir(&c, exts, ctx, tr) {
                return Some(f);
            }
        }
        None
    }

    /// The found file, and whether the substituted candidate ended in a TypeScript extension
    /// that came from the specifier (not from the substitution itself).
    fn try_paths(&self, spec: &str, ctx: Ctx<'_>, tr: &mut Tracer<'_>) -> Option<(Found, bool)> {
        let map = self.options.paths.as_ref()?;
        let base = self.options.paths_base.as_deref()?;
        tr!(
            tr,
            "'paths' option is specified, looking for a pattern to match module name '{spec}'."
        );
        let (key, subs, star) = match_paths_map(map, spec)?;
        tr!(tr, "Module name '{spec}', matched pattern '{key}'.");
        for sub in subs {
            let s = sub.replacen('*', &star, 1);
            tr!(
                tr,
                "Trying substitution '{sub}', candidate module location: '{s}'."
            );
            let candidate = path::resolve(base, &s);
            let directory = s.ends_with('/');
            // A substitution written with a module extension (`.ts`, `.d.ts`, `.js`, `.json`,
            // ...; not one the `*` brought in) is tried as written first, whatever the target
            // extensions (observed: `./k.js` resolves to `k.js` next to a `k.ts`;
            // `moduleSuffixes` go before the extension).
            if !directory
                && let Some(kind) = Extension::of(sub)
                && sub.ends_with(kind.as_str())
                && candidate.ends_with(kind.as_str())
                && let Some(f) = self.try_with_suffixes(
                    &candidate[..candidate.len() - kind.as_str().len()],
                    kind.as_str(),
                    kind,
                    tr,
                )
            {
                return Some((f, false));
            }
            let candidate_ts_ext = Extension::of(sub).is_none()
                && Extension::of(&s).is_some_and(Extension::is_ts_like);
            tr!(
                tr,
                "Loading module as file / folder, candidate module location '{}', target file types: {}.",
                if directory {
                    with_slash(&candidate)
                } else {
                    candidate.clone()
                },
                exts_name(self.all_exts())
            );
            let found = if directory && !ctx.esm_strict && !self.dir_exists(&candidate) {
                tr.no_directory(&with_slash(&candidate));
                None
            } else if directory && !ctx.esm_strict {
                // `"./dir/"`: a directory module only (observed: no file probes).
                self.load_directory(
                    &candidate,
                    self.all_exts(),
                    true,
                    0,
                    PackageJsonSource::Probe,
                    tr,
                )
            } else {
                self.load_file_or_dir(&candidate, self.all_exts(), ctx, tr)
            };
            if let Some(found) = found {
                return Some((found, candidate_ts_ext));
            }
        }
        None
    }

    fn resolve_imports(
        &self,
        spec: &str,
        scope: Option<&PackageJson>,
        mode: ResolutionMode,
        ctx: Ctx<'_>,
        depth: u32,
        tr: &mut Tracer<'_>,
    ) -> Option<ResolvedModule> {
        if !self.options.resolve_package_json_imports {
            return None;
        }
        let scope = scope?;
        let imports = scope.imports.as_ref()?;
        let cond = |c: &str| self.condition_matches(c, ctx);
        let Some((target, replacement, key)) = match_subpath_keyed(imports, spec) else {
            tr!(
                tr,
                "Import specifier '{spec}' does not exist in package.json scope at path '{}'.",
                scope.dir
            );
            return None;
        };
        let ambiguous = self.project_root_ambiguous(&scope.dir);
        for (i, exts) in self.passes().into_iter().enumerate() {
            let mut tried = false;
            let r = self.resolve_target_traced(
                target,
                replacement.as_deref(),
                key,
                &scope.dir,
                &cond,
                MapKind::Imports,
                tr,
                &mut |t: &str, tr: &mut Tracer<'_>| {
                    if t.starts_with("./") && ambiguous {
                        tried = true;
                        None
                    } else if t.starts_with("./") {
                        self.load_exact_target(&scope.dir, t, exts, tr).map(|f| {
                            let id = self.package_id(&scope.dir, &f, false, tr);
                            // The `*` brought a TypeScript extension in from the specifier
                            // (`#internal/foo.ts` through `"./internal/*"`).
                            let using_ts_ext = replacement
                                .as_deref()
                                .is_some_and(|r| !r.is_empty() && t.ends_with(r))
                                && Extension::of(t).is_some_and(Extension::is_ts_like)
                                && f.ext.is_ts_like();
                            self.finish(f, id, false, using_ts_ext, tr)
                        })
                    } else if i == 0 {
                        // Bare targets resolve as packages from the package directory.
                        self.resolve_module_at(t, &scope.dir, mode, depth + 1, tr)
                            .map(|r| (*r).clone())
                    } else {
                        None
                    }
                },
            );
            if tried && !matches!(r, Lookup::Found(_)) {
                self.record_ambiguous_root(true, replacement.as_deref(), &scope.dir);
                return None;
            }
            match r {
                Lookup::Found(m) => return Some(m),
                Lookup::Excluded => return None,
                Lookup::NotMatched => {}
            }
        }
        None
    }

    fn resolve_self_name(
        &self,
        spec: &str,
        scope: &PackageJson,
        ctx: Ctx<'_>,
        tr: &mut Tracer<'_>,
    ) -> Option<ResolvedModule> {
        if !self.options.resolve_package_json_exports {
            return None;
        }
        let name = scope.name.as_deref()?;
        let exports = scope.exports.as_ref()?;
        let (pkg, rest) = split_package_name(spec);
        if pkg != name {
            return None;
        }
        let key = if rest.is_empty() {
            ".".to_owned()
        } else {
            format!("./{rest}")
        };
        let cond = |c: &str| self.condition_matches(c, ctx);
        for exts in self.passes() {
            match self.evaluate_exports_traced(
                exports,
                &key,
                &scope.dir,
                &cond,
                tr,
                &mut |t: &str, tr: &mut Tracer<'_>| {
                    self.load_exact_target(&scope.dir, t, exts, tr).map(|f| {
                        let id = self.package_id(&scope.dir, &f, false, tr);
                        (f, id)
                    })
                },
            ) {
                Lookup::Found((f, id)) => {
                    // Observed: the real path is looked up (and shown) for a package inside
                    // `node_modules` only, not for the program's own package.
                    if !self.options.preserve_symlinks && f.path.contains("/node_modules/") {
                        self.realpath_traced(&f.path, tr);
                    }
                    return Some(self.finish(f, id, false, false, tr));
                }
                Lookup::Excluded => return None,
                Lookup::NotMatched => {}
            }
        }
        None
    }

    fn condition_matches(&self, cond: &str, ctx: Ctx<'_>) -> bool {
        if let Some(range) = cond.strip_prefix("types@") {
            return ctx.conditions.iter().any(|c| c == "types")
                && crate::semver::range_matches(range, TS_VERSION);
        }
        ctx.conditions.iter().any(|c| c == cond)
    }

    /// `exports` of the package at `dir` for `key` (`.` or `./sub`), traced.
    #[allow(clippy::type_complexity)]
    fn evaluate_exports_traced<T>(
        &self,
        exports: &Value,
        key: &str,
        dir: &str,
        conditions: &dyn Fn(&str) -> bool,
        tr: &mut Tracer<'_>,
        try_target: &mut dyn FnMut(&str, &mut Tracer<'_>) -> Option<T>,
    ) -> Lookup<T> {
        if is_subpath_map(exports) {
            let r = match match_subpath_keyed(exports, key) {
                Some((v, rep, matched)) => self.resolve_target_traced(
                    v,
                    rep.as_deref(),
                    matched,
                    dir,
                    conditions,
                    MapKind::Exports,
                    tr,
                    try_target,
                ),
                None => Lookup::NotMatched,
            };
            if matches!(r, Lookup::NotMatched) && key != "." {
                tr!(
                    tr,
                    "Export specifier '{key}' does not exist in package.json scope at path '{dir}'."
                );
            }
            r
        } else if key == "." {
            self.resolve_target_traced(
                exports,
                None,
                ".",
                dir,
                conditions,
                MapKind::Exports,
                tr,
                try_target,
            )
        } else {
            Lookup::NotMatched
        }
    }

    /// Evaluates an `exports`/`imports` target value (string, array, condition object or
    /// `null`), as `ts_r_config::exports::resolve_target` does, logging each step: TypeScript
    /// keeps trying the next condition when a target does not resolve.
    #[allow(clippy::too_many_arguments)]
    fn resolve_target_traced<T>(
        &self,
        target: &Value,
        replacement: Option<&str>,
        key: &str,
        dir: &str,
        conditions: &dyn Fn(&str) -> bool,
        kind: MapKind,
        tr: &mut Tracer<'_>,
        try_target: &mut dyn FnMut(&str, &mut Tracer<'_>) -> Option<T>,
    ) -> Lookup<T> {
        let field = match kind {
            MapKind::Exports => "exports",
            MapKind::Imports => "imports",
        };
        match &target.kind {
            ValueKind::String(s) => {
                let valid = match kind {
                    MapKind::Exports => s.starts_with("./"),
                    MapKind::Imports => !s.starts_with("../") && !s.starts_with('/'),
                };
                if !valid {
                    return Lookup::NotMatched;
                }
                let substituted = match replacement {
                    Some(r) if r.starts_with('\u{0}') => format!("{s}{}", &r[1..]),
                    Some(r) => s.replace('*', r),
                    None => s.clone(),
                };
                tr!(
                    tr,
                    "Using '{field}' subpath '{key}' with target '{substituted}'."
                );
                match try_target(&substituted, tr) {
                    Some(t) => Lookup::Found(t),
                    None => Lookup::NotMatched,
                }
            }
            ValueKind::Null => {
                tr!(
                    tr,
                    "package.json scope '{dir}' explicitly maps specifier '{key}' to null."
                );
                Lookup::Excluded
            }
            ValueKind::Array(items) => {
                for it in items {
                    match self.resolve_target_traced(
                        it,
                        replacement,
                        key,
                        dir,
                        conditions,
                        kind,
                        tr,
                        try_target,
                    ) {
                        Lookup::Found(t) => return Lookup::Found(t),
                        Lookup::Excluded | Lookup::NotMatched => continue,
                    }
                }
                Lookup::NotMatched
            }
            ValueKind::Object(members) => {
                tr!(tr, "Entering conditional exports.");
                for m in members {
                    if m.key == "default" || conditions(&m.key) {
                        tr!(tr, "Matched '{field}' condition '{}'.", m.key);
                        match self.resolve_target_traced(
                            &m.value,
                            replacement,
                            key,
                            dir,
                            conditions,
                            kind,
                            tr,
                            try_target,
                        ) {
                            Lookup::Found(t) => {
                                tr!(tr, "Resolved under condition '{}'.", m.key);
                                tr!(tr, "Exiting conditional exports.");
                                return Lookup::Found(t);
                            }
                            Lookup::Excluded => {
                                tr!(tr, "Exiting conditional exports.");
                                return Lookup::Excluded;
                            }
                            Lookup::NotMatched => {
                                tr!(tr, "Failed to resolve under condition '{}'.", m.key);
                            }
                        }
                    } else {
                        tr!(tr, "Saw non-matching condition '{}'.", m.key);
                    }
                }
                tr!(tr, "Exiting conditional exports.");
                Lookup::NotMatched
            }
            _ => Lookup::NotMatched,
        }
    }

    /// Searches `node_modules` directories from `dir` upwards, once per extension pass.
    /// Returns the file, the package directory it belongs to and its package identity.
    fn node_modules_lookup(
        &self,
        spec: &str,
        dir: &str,
        ctx: Ctx<'_>,
        passes: &[u8],
        with_types_dirs: bool,
        tr: &mut Tracer<'_>,
    ) -> Option<FoundInPackage> {
        let (pkg_name, rest) = split_package_name(spec);
        if pkg_name.is_empty() {
            return None;
        }
        for (i, &exts) in passes.iter().enumerate() {
            tr!(
                tr,
                "Searching all ancestor node_modules directories for {} extensions: {}.",
                if i == 0 { "preferred" } else { "fallback" },
                exts_name(exts)
            );
            let mut d = dir.to_owned();
            loop {
                if path::file_name(&d) != "node_modules" {
                    let nm = path::combine(&d, "node_modules");
                    if self.dir_exists(&nm) {
                        // An `exports` target of `null` ends the whole lookup (observed: no
                        // `@types`, no later pass).
                        let mut excluded = false;
                        if let Some(r) =
                            self.load_package(&nm, pkg_name, rest, exts, ctx, &mut excluded, tr)
                        {
                            return Some(r);
                        }
                        if excluded {
                            return None;
                        }
                        if with_types_dirs && exts & DECL != 0 {
                            let types_dir = path::combine(&nm, "@types");
                            if self.dir_exists(&types_dir) {
                                let mangled = mangle_scoped_name(pkg_name);
                                if mangled != pkg_name {
                                    tr!(
                                        tr,
                                        "Scoped package detected, looking in '{}'",
                                        mangle_scoped_name(spec)
                                    );
                                }
                                if let Some(r) = self.load_package(
                                    &types_dir,
                                    &mangled,
                                    rest,
                                    DECL | AT_TYPES_FIELDS,
                                    ctx,
                                    &mut excluded,
                                    tr,
                                ) {
                                    return Some(r);
                                }
                                if excluded {
                                    return None;
                                }
                            } else {
                                tr.no_directory(&types_dir);
                            }
                        }
                    } else {
                        tr.no_directory(&nm);
                    }
                }
                let parent = path::directory_of(&d).to_owned();
                if parent == d || parent.is_empty() {
                    break;
                }
                d = parent;
            }
        }
        None
    }

    /// Loads `name[/rest]` from one `node_modules` (or `@types`) directory. Sets `excluded`
    /// when the package's `exports` map the subpath to `null`.
    #[allow(clippy::too_many_arguments)]
    fn load_package(
        &self,
        nm: &str,
        name: &str,
        rest: &str,
        exts: u8,
        ctx: Ctx<'_>,
        excluded: &mut bool,
        tr: &mut Tracer<'_>,
    ) -> Option<FoundInPackage> {
        let fields = exts & AT_TYPES_FIELDS;
        let exts = exts & !AT_TYPES_FIELDS;
        let pkg_dir = path::combine(nm, name);
        if !self.dir_exists(&pkg_dir) {
            if rest.is_empty() && !ctx.esm_strict && self.dir_exists(path::directory_of(&pkg_dir)) {
                return self
                    .load_file(&pkg_dir, exts, true, tr)
                    .map(|f| (f, pkg_dir, None));
            }
            return None;
        }
        // A subpath's own directory is looked at first (tsc reads its `package.json` before
        // the package's).
        let candidate = path::combine(&pkg_dir, rest);
        let sub_pj = if !rest.is_empty() && self.dir_exists(&candidate) {
            self.package_json_traced(&candidate, tr)
        } else {
            None
        };
        let pj = self.package_json_traced(&pkg_dir, tr);
        if let Some(exports) = pj
            .as_ref()
            .and_then(|p| p.exports.as_ref())
            .filter(|_| ctx.exports && self.options.resolve_package_json_exports)
        {
            let key = if rest.is_empty() {
                ".".to_owned()
            } else {
                format!("./{rest}")
            };
            let cond = |c: &str| self.condition_matches(c, ctx);
            return match self.evaluate_exports_traced(
                exports,
                &key,
                &pkg_dir,
                &cond,
                tr,
                &mut |t: &str, tr: &mut Tracer<'_>| {
                    self.load_exact_target(&pkg_dir, t, exts, tr).map(|f| {
                        let id = self.package_id(&pkg_dir, &f, false, tr);
                        (f, id)
                    })
                },
            ) {
                Lookup::Found((f, id)) => Some((f, pkg_dir.clone(), id)),
                Lookup::Excluded => {
                    *excluded = true;
                    None
                }
                Lookup::NotMatched => None,
            };
        }
        let found = if rest.is_empty() {
            let as_file = if ctx.esm_strict {
                None
            } else {
                self.load_file(&pkg_dir, exts, true, tr)
            };
            // ESM: a package root with a `package.json` falls back to `index` like
            // CommonJS (its `main` defaults to `index.js`); without one it does not.
            // (The trace's second look without `exports` does not fall back to `index` after
            // a `main` field, as tsc shows it.)
            let allow_index =
                !ctx.esm_strict || pj.as_ref().is_some_and(|p| ctx.exports || p.main.is_none());
            as_file.or_else(|| {
                self.load_directory(
                    &pkg_dir,
                    exts | fields,
                    allow_index,
                    0,
                    PackageJsonSource::Known(pj.clone()),
                    tr,
                )
            })
        } else {
            if sub_pj.is_none()
                && let Some(pj) = &pj
            {
                types_versions_line(pj, tr);
            }
            // Every pass maps the subpath (observed: the JavaScript pass probes
            // `ts3.1/other.js` for `"*": ["ts3.1/*"]`).
            let mapped = pj
                .as_ref()
                .and_then(|pj| self.try_types_versions(pj, rest, &pj.dir, exts, ctx, false, tr));
            // The subpath directory uses its own `package.json`; without one, only the
            // package's `typesVersions` are looked at (and shown).
            let source = match (&sub_pj, &pj) {
                (None, Some(root)) => PackageJsonSource::Versions(root.clone()),
                _ => PackageJsonSource::Known(sub_pj.clone()),
            };
            mapped
                .or_else(|| {
                    // A subpath directory that does not exist is skipped without a line
                    // (observed: `@typescript/lib-dom/iterable` in a package without it).
                    if matches!(source, PackageJsonSource::Known(None))
                        && self.dir_exists(path::directory_of(&candidate))
                        && !self.dir_exists(&candidate)
                    {
                        self.load_file(&candidate, exts, !ctx.esm_strict, tr)
                    } else {
                        self.load_file_or_dir_with(&candidate, exts, ctx, source, tr)
                    }
                })
                // ESM: a subpath directory with its own `package.json` still uses its
                // `types`/`main` (not `index`), like a package root (`fp-ts/function`).
                .or_else(|| {
                    if ctx.esm_strict && sub_pj.is_some() {
                        self.load_directory(
                            &candidate,
                            exts,
                            false,
                            0,
                            PackageJsonSource::Known(sub_pj.clone()),
                            tr,
                        )
                    } else {
                        None
                    }
                })
        };
        let found = found?;
        let id = if found.no_package_id {
            None
        } else {
            self.package_id(&pkg_dir, &found, false, tr)
        };
        Some((found, pkg_dir, id))
    }

    /// Applies a matching `typesVersions` map to `sub`; substitutions are relative to `base`
    /// (the package directory, or a sub-directory loaded as a directory module).
    /// `loading`: each candidate is loaded as a file or directory (a `types`/`main` field),
    /// with tsc's "Loading module as file / folder" line.
    #[allow(clippy::too_many_arguments)]
    fn try_types_versions(
        &self,
        pj: &Arc<PackageJson>,
        sub: &str,
        base: &str,
        exts: u8,
        ctx: Ctx<'_>,
        loading: bool,
        tr: &mut Tracer<'_>,
    ) -> Option<Found> {
        let tv = pj.types_versions.as_ref()?;
        let entry = tv
            .as_object()?
            .iter()
            .find(|m| crate::semver::range_matches(&m.key, TS_VERSION))?;
        let members = entry.value.as_object()?;
        if tr.enabled() {
            let version = tr.compiler_version().to_owned();
            tr!(
                tr,
                "'package.json' has a 'typesVersions' entry '{}' that matches compiler version '{version}', looking for a pattern to match module name '{sub}'.",
                entry.key
            );
        }
        let (key, subs, star) =
            match_paths(members.iter().map(|m| (m.key.as_str(), &m.value)), sub)?;
        tr!(tr, "Module name '{sub}', matched pattern '{key}'.");
        let subs: Vec<&str> = match subs.as_array() {
            Some(items) => items.iter().filter_map(Value::as_str).collect(),
            None => subs.as_str().into_iter().collect(),
        };
        for s in subs {
            let substituted = s.replacen('*', &star, 1);
            tr!(
                tr,
                "Trying substitution '{s}', candidate module location: '{substituted}'."
            );
            let candidate = path::resolve(base, &substituted);
            if loading {
                tr!(
                    tr,
                    "Loading module as file / folder, candidate module location '{candidate}', target file types: {}.",
                    exts_name(exts)
                );
            } else if let Some(f) = self.probe_as_written(&candidate, exts & (TS | DECL), tr) {
                return Some(Found {
                    no_package_id: true,
                    ..f
                });
            }
            // A subpath's mapped directory has no `package.json` of its own in tsc's view.
            let source = if loading {
                PackageJsonSource::Probe
            } else {
                PackageJsonSource::Versions(pj.clone())
            };
            if let Some(f) = self.load_file_or_dir_with(&candidate, exts, ctx, source, tr) {
                return Some(f);
            }
        }
        None
    }

    /// Loads an `exports`/`imports` target: a TypeScript extension is probed as written,
    /// other extensions are substituted (no guessing, no directory modules). The target must
    /// stay inside `base`.
    fn load_exact_target(
        &self,
        base: &str,
        target: &str,
        exts: u8,
        tr: &mut Tracer<'_>,
    ) -> Option<Found> {
        let p = path::resolve(base, target);
        if !path::contains(base, &p, self.probes.fs.case_sensitive()) {
            return None;
        }
        if let Some(f) = self.load_from_outputs(&p, tr) {
            return Some(f);
        }
        // With `moduleSuffixes` the suffixed names come first (`load_file`).
        if self.options.module_suffixes.iter().any(|s| !s.is_empty()) {
            return self.load_file(&p, exts, false, tr);
        }
        let name = path::file_name(&p);
        let own_ext = path::ext::of(name);
        // Observed (tsc 7.0.2): a TypeScript extension is probed as written in the
        // TypeScript pass and nothing else; the JavaScript pass tries the matching
        // JavaScript extensions. Files in a directory that does not exist are not probed
        // (the "stripping" line still shows).
        let probe_dir =
            self.dir_exists(path::directory_of(&p)) || self.options.project_references.is_some();
        if ts_ext_category(own_ext).is_some() {
            if exts & (TS | DECL) != 0 {
                return self.probe_as_written(&p, exts, tr);
            }
            tr!(
                tr,
                "File name '{p}' has a '{own_ext}' extension - stripping it."
            );
            if !probe_dir {
                return None;
            }
            let js_ext = match own_ext {
                ".tsx" => ".jsx",
                ".mts" | ".d.mts" => ".mjs",
                ".cts" | ".d.cts" => ".cjs",
                _ => ".js",
            };
            let stem = &p[..p.len() - own_ext.len()];
            return replacements(js_ext)
                .iter()
                .filter(|(_, cat, _)| exts & cat != 0)
                .find_map(|&(ext, _, kind)| self.try_with_suffixes(stem, ext, kind, tr));
        }
        if !probe_dir {
            let ext = if own_ext.is_empty() {
                name.rfind('.')
                    .filter(|&i| i > 0 && i + 1 < name.len())
                    .map(|i| &name[i..])
            } else {
                Some(own_ext)
            };
            if let Some(ext) = ext {
                tr!(
                    tr,
                    "File name '{p}' has a '{ext}' extension - stripping it."
                );
            }
            return None;
        }
        self.load_file(&p, exts, false, tr)
    }

    /// An `exports`/`imports` target inside this program's `outDir` or `declarationDir`
    /// stands for its source (observed, tsc 7.0.2: `./types/index.d.ts` with `outDir: types`
    /// resolves to `src/index.ts`). The possible sources are looked for without a trace; the
    /// first one found is loaded like a target (a TypeScript file as written, a JavaScript
    /// file with its extension replaced, which may find a declaration next to it). None
    /// found: the target is loaded as usual.
    fn load_from_outputs(&self, p: &str, tr: &mut Tracer<'_>) -> Option<Found> {
        let o = &self.options;
        if o.out_dir.is_none() && o.declaration_dir.is_none() {
            return None;
        }
        let root = o.root_dir.as_deref()?;
        if p.contains("/node_modules/") {
            return None;
        }
        let cs = self.probes.fs.case_sensitive();
        let out = [o.declaration_dir.as_deref(), o.out_dir.as_deref()]
            .into_iter()
            .flatten()
            .find(|d| path::contains(d, p, cs))?;
        let rel = path::relative(out, p, cs);
        let (stem, sources): (&str, &[&str]) = if let Some(s) = rel
            .strip_suffix(".d.ts")
            .or_else(|| rel.strip_suffix(".js"))
        {
            (s, &[".ts", ".tsx", ".js", ".jsx"])
        } else if let Some(s) = rel
            .strip_suffix(".d.mts")
            .or_else(|| rel.strip_suffix(".mjs"))
        {
            (s, &[".mts", ".mjs"])
        } else if let Some(s) = rel
            .strip_suffix(".d.cts")
            .or_else(|| rel.strip_suffix(".cjs"))
        {
            (s, &[".cts", ".cjs"])
        } else {
            return None;
        };
        if stem.is_empty() {
            return None;
        }
        let base = path::combine(root, stem);
        for ext in sources {
            let js = matches!(*ext, ".js" | ".jsx" | ".mjs" | ".cjs");
            if js && !o.allow_js {
                continue;
            }
            let candidate = format!("{base}{ext}");
            if !self.file_exists(&candidate) {
                continue;
            }
            return if js {
                self.load_file(&candidate, self.all_exts(), false, tr)
            } else {
                self.probe_as_written(&candidate, TS | DECL, tr)
            };
        }
        None
    }

    /// A path with a TypeScript extension in one of `exts`, probed as written (`package.json`
    /// fields and `exports` targets name declaration files directly).
    fn probe_as_written(&self, p: &str, exts: u8, tr: &mut Tracer<'_>) -> Option<Found> {
        // With `moduleSuffixes` the suffixed names come first (`load_file`).
        if self.options.module_suffixes.iter().any(|s| !s.is_empty()) {
            return None;
        }
        let (cat, kind) = ts_ext_category(path::ext::of(path::file_name(p)))?;
        if exts & cat == 0 || !self.probe_file(p, tr) {
            return None;
        }
        Some(Found {
            path: p.to_owned(),
            ext: kind,
            via_package_json: None,
            from_directory: false,
            no_package_id: false,
        })
    }

    /// Loads `candidate` as a file, then (unless ESM-strict) as a directory module. Nothing
    /// is probed in a directory that does not exist.
    fn load_file_or_dir(
        &self,
        candidate: &str,
        exts: u8,
        ctx: Ctx<'_>,
        tr: &mut Tracer<'_>,
    ) -> Option<Found> {
        self.load_file_or_dir_with(candidate, exts, ctx, PackageJsonSource::Probe, tr)
    }

    fn load_file_or_dir_with(
        &self,
        candidate: &str,
        exts: u8,
        ctx: Ctx<'_>,
        pj: PackageJsonSource,
        tr: &mut Tracer<'_>,
    ) -> Option<Found> {
        let parent = path::directory_of(candidate);
        if self.options.project_references.is_none()
            && !parent.is_empty()
            && parent != candidate
            && !self.dir_exists(parent)
        {
            tr.no_directory(parent);
            return None;
        }
        if let Some(f) = self.load_file(candidate, exts, !ctx.esm_strict, tr) {
            return Some(f);
        }
        if ctx.esm_strict {
            return None;
        }
        self.load_directory(candidate, exts, true, 0, pj, tr)
    }

    fn try_with_suffixes(
        &self,
        base: &str,
        ext: &str,
        kind: Extension,
        tr: &mut Tracer<'_>,
    ) -> Option<Found> {
        for suffix in &self.options.module_suffixes {
            let p = format!("{base}{suffix}{ext}");
            if self.probe_file(&p, tr) {
                return Some(Found {
                    path: p,
                    ext: kind,
                    via_package_json: None,
                    from_directory: false,
                    no_package_id: false,
                });
            }
        }
        None
    }

    /// Loads `candidate` as a file: replace its extension (`.js` → `.ts`, …), then (when
    /// `append`) treat it as extensionless and add each extension.
    fn load_file(
        &self,
        candidate: &str,
        exts: u8,
        append: bool,
        tr: &mut Tracer<'_>,
    ) -> Option<Found> {
        let name = path::file_name(candidate);
        let own_ext = path::ext::of(name);
        if !own_ext.is_empty() {
            tr!(
                tr,
                "File name '{candidate}' has a '{own_ext}' extension - stripping it."
            );
            let base = &candidate[..candidate.len() - own_ext.len()];
            if own_ext == ".json" {
                if exts & DECL != 0
                    && let Some(f) = self.try_with_suffixes(base, ".d.json.ts", Extension::Dts, tr)
                {
                    return Some(f);
                }
                if exts & JSON != 0
                    && let Some(f) = self.try_with_suffixes(base, ".json", Extension::Json, tr)
                {
                    return Some(f);
                }
            } else {
                for &(ext, cat, kind) in replacements(own_ext) {
                    if exts & cat != 0
                        && let Some(f) = self.try_with_suffixes(base, ext, kind, tr)
                    {
                        return Some(f);
                    }
                }
            }
        } else if let Some(dot) = name.rfind('.').filter(|&i| i > 0 && i + 1 < name.len()) {
            // Arbitrary extension: `./styles.css` → `./styles.d.css.ts`.
            let ext = &name[dot..];
            tr!(
                tr,
                "File name '{candidate}' has a '{ext}' extension - stripping it."
            );
            if exts & DECL != 0 {
                let base = &candidate[..candidate.len() - ext.len()];
                if let Some(f) =
                    self.try_with_suffixes(base, &format!(".d{ext}.ts"), Extension::Dts, tr)
                {
                    return Some(f);
                }
            }
        }
        if append {
            for &(ext, cat, kind) in APPEND {
                if exts & cat != 0
                    && let Some(f) = self.try_with_suffixes(candidate, ext, kind, tr)
                {
                    return Some(f);
                }
            }
        }
        None
    }

    /// Loads a directory module: `package.json` `typings`/`types` (declaration passes),
    /// `main` (JS passes, or declaration passes without a types field), then `index`.
    fn load_directory(
        &self,
        dir: &str,
        exts: u8,
        allow_index: bool,
        depth: u32,
        pj: PackageJsonSource,
        tr: &mut Tracer<'_>,
    ) -> Option<Found> {
        self.load_directory_inner(dir, exts, allow_index, depth, pj, tr)
            .map(|f| Found {
                from_directory: true,
                ..f
            })
    }

    fn load_directory_inner(
        &self,
        dir: &str,
        exts: u8,
        allow_index: bool,
        depth: u32,
        pj: PackageJsonSource,
        tr: &mut Tracer<'_>,
    ) -> Option<Found> {
        if depth > 8 {
            return None;
        }
        let field_exts = if exts & AT_TYPES_FIELDS != 0 {
            TS | DECL
        } else {
            exts & (TS | DECL)
        };
        let main_exts = if exts & AT_TYPES_FIELDS != 0 {
            TS | DECL
        } else {
            exts
        };
        let exts = exts & !AT_TYPES_FIELDS;
        let no_strict = Ctx {
            esm_strict: false,
            conditions: &[],
            exports: true,
        };
        // A sub-directory of a package without its own `package.json`: tsc shows the
        // package's `typesVersions` and maps `index` through them, relative to this
        // directory (observed: `ext/ts3.1/other` tries `ext/ts3.1/other/ts3.1/index`), even
        // when the directory does not exist.
        let versions = |root: &Arc<PackageJson>, tr: &mut Tracer<'_>| {
            types_versions_line(root, tr);
            if root.types_versions.is_some() {
                self.try_types_versions(root, "index", dir, exts, no_strict, true, tr)
            } else {
                None
            }
        };
        if !self.dir_exists(dir) {
            match &pj {
                PackageJsonSource::Versions(root) => return versions(root, tr),
                _ => tr.no_directory(dir),
            }
            return None;
        }
        let pj = match pj {
            PackageJsonSource::Probe => self.package_json_traced(dir, tr),
            PackageJsonSource::Known(pj) => pj,
            PackageJsonSource::Versions(root) => {
                if let Some(f) = versions(&root, tr) {
                    return Some(f);
                }
                None
            }
        };
        if let Some(pj) = &pj {
            let tsish = exts & (TS | DECL) != 0;
            let types_entry = pj.types_entry();
            let use_main = exts & (JS | JSON) != 0 || (tsish && types_entry.is_none());
            let field = |tr: &mut Tracer<'_>, name: &str, value: Option<&str>| match value {
                Some(v) => tr!(
                    tr,
                    "'package.json' has '{name}' field '{v}' that references '{}'.",
                    path::resolve(dir, v)
                ),
                None => tr!(tr, "'package.json' does not have a '{name}' field."),
            };
            types_versions_line(pj, tr);
            if tsish {
                field(tr, "typings", pj.typings.as_deref());
                if pj.typings.is_none() {
                    field(tr, "types", pj.types.as_deref());
                }
            }
            let mark = |mut f: Found| {
                f.via_package_json = f.via_package_json.or_else(|| Some(dir.to_owned()));
                f
            };
            if tsish {
                let tv_sub = types_entry
                    .map(|t| path::normalize(t).trim_start_matches("./").to_owned())
                    .unwrap_or_else(|| "index".to_owned());
                if let Some(f) =
                    self.try_types_versions(pj, &tv_sub, dir, exts, no_strict, true, tr)
                {
                    return Some(mark(f));
                }
                if let Some(t) = types_entry {
                    let c = path::resolve(dir, t);
                    if let Some(f) = self.load_field_path(dir, &c, field_exts, depth, tr) {
                        return Some(mark(f));
                    }
                }
            }
            if use_main {
                field(tr, "main", pj.main.as_deref());
            }
            if let (true, Some(main)) = (use_main, pj.main.as_deref()) {
                let c = path::resolve(dir, main);
                if let Some(f) = self.load_field_path(dir, &c, main_exts, depth, tr) {
                    return Some(mark(f));
                }
            }
        }
        if allow_index {
            let has_package_json = pj.is_some();
            return self
                .load_file(&path::combine(dir, "index"), exts, true, tr)
                .map(|mut f| {
                    if has_package_json && f.via_package_json.is_none() {
                        f.via_package_json = Some(dir.to_owned());
                    }
                    f
                });
        }
        None
    }

    /// Loads the file a `package.json` field (`types`, `typings`, `main`) of the package at
    /// `dir` names: as written when it has a TypeScript extension, with its extension
    /// substituted when it has another one, then as a file or a directory's `index` (that
    /// directory's own `package.json` is ignored).
    fn load_field_path(
        &self,
        dir: &str,
        c: &str,
        exts: u8,
        depth: u32,
        tr: &mut Tracer<'_>,
    ) -> Option<Found> {
        let name = path::file_name(c);
        if ts_ext_category(path::ext::of(name)).is_some() {
            if let Some(f) = self.probe_as_written(c, exts, tr) {
                return Some(f);
            }
        } else if name.rfind('.').is_some_and(|i| i > 0 && i + 1 < name.len())
            && let Some(f) = self.load_file(c, exts, false, tr)
        {
            return Some(f);
        }
        tr!(
            tr,
            "Loading module as file / folder, candidate module location '{c}', target file types: {}.",
            exts_name(exts)
        );
        if let Some(f) = self.load_file(c, exts, true, tr) {
            return Some(f);
        }
        if c == dir {
            return None;
        }
        // A directory named by a field loads its `index` only: its own `package.json` is not
        // read (observed: `main: "oof"` with `oof/package.json` naming `main`/`types`).
        self.load_directory(c, exts, true, depth + 1, PackageJsonSource::Known(None), tr)
    }

    // ------------------------------------------------------------------ type references

    /// The directory default type roots and `types` entries start from: the config's, else
    /// the working directory.
    fn type_base_dir(&self) -> String {
        self.options
            .config_dir
            .clone()
            .unwrap_or_else(|| self.probes.fs.cwd())
    }

    /// `typeRoots`, or every ancestor `node_modules/@types` of `start`.
    fn type_roots(&self, start: &str) -> Vec<String> {
        if let Some(r) = &self.options.type_roots {
            return r.clone();
        }
        let mut roots = Vec::new();
        let mut d = start.to_owned();
        loop {
            roots.push(path::combine(&d, "node_modules/@types"));
            let parent = path::directory_of(&d).to_owned();
            if parent == d || parent.is_empty() {
                break;
            }
            d = parent;
        }
        roots
    }

    fn resolve_type_reference_uncached(
        &self,
        name: &str,
        dir: Option<&str>,
        mode: ResolutionMode,
        tr: &mut Tracer<'_>,
    ) -> Option<ResolvedTypeReference> {
        let base_dir = self.type_base_dir();
        let roots = self.type_roots(&base_dir);
        let relative = path::is_explicitly_relative(name) || path::is_rooted(name);
        if roots.is_empty() {
            tr!(
                tr,
                "Root directory cannot be determined, skipping primary search paths."
            );
        } else {
            tr!(
                tr,
                "Resolving with primary search path '{}'.",
                roots.join(", ")
            );
        }
        if relative {
            // tsc shows the type roots it skips, then loads the path from the containing
            // directory.
            for root in &roots {
                if !self.dir_exists(root) {
                    tr.no_directory(root);
                }
            }
            let start = dir.unwrap_or(&base_dir);
            let candidate = path::resolve(start, name);
            tr!(
                tr,
                "Looking up in 'node_modules' folder, initial location '{start}'."
            );
            tr!(
                tr,
                "Loading module as file / folder, candidate module location '{candidate}', target file types: {}.",
                exts_name(DECL)
            );
            let found = self.load_file(&candidate, DECL, true, tr).or_else(|| {
                self.load_directory(&candidate, DECL, true, 0, PackageJsonSource::Probe, tr)
            })?;
            let package_id =
                package_dir_of(&found.path).and_then(|d| self.package_id(&d, &found, true, tr));
            return Some(self.finish_type_ref(found, package_id, false, tr));
        }
        for root in &roots {
            if !self.dir_exists(root) {
                tr.no_directory(root);
                continue;
            }
            // Custom `typeRoots` also accept `<root>/<name>.d.ts`; default roots only hold
            // package directories.
            let found =
                self.load_from_type_root(name, root, self.options.type_roots.is_some(), false, tr);
            if let Some((f, _)) = found {
                // A file found in a package shows that package's `package.json` lookup.
                if !f.from_directory
                    && let Some(d) = package_dir_of(&f.path)
                {
                    self.package_json_traced(&d, tr);
                }
                return Some(self.finish_type_ref(f, None, true, tr));
            }
        }
        if self.options.type_roots.is_some() && dir.is_none() {
            tr!(
                tr,
                "Resolving type reference directive for program that specifies custom typeRoots, skipping lookup in 'node_modules' folder."
            );
            return None;
        }
        let start = dir.unwrap_or(&base_dir);
        tr!(
            tr,
            "Looking up in 'node_modules' folder, initial location '{start}'."
        );
        let ctx = Ctx {
            esm_strict: false,
            ..self.ctx(mode)
        };
        let (found, _, package_id) =
            self.node_modules_lookup(name, start, ctx, &[DECL], true, tr)?;
        Some(self.finish_type_ref(found, package_id, false, tr))
    }

    /// Looks `name` up in the type root `root` (which exists): `<root>/<name>.d.ts` when
    /// `as_file`, then the directory `<root>/<name>` (its `package.json`, then `index.d.ts`).
    /// A root named `@types` holds scoped packages under their mangled name
    /// (`@scope/pkg` → `scope__pkg`); other roots hold them as written. Nothing is probed
    /// when the candidate's parent directory (`<root>/@scope`) does not exist. Under strict
    /// ESM rules (`esm_strict`) only a directory with a `package.json` is loaded. Returns
    /// the file and the candidate directory.
    fn load_from_type_root(
        &self,
        name: &str,
        root: &str,
        as_file: bool,
        esm_strict: bool,
        tr: &mut Tracer<'_>,
    ) -> Option<(Found, String)> {
        let mangled = mangle_scoped_name(name);
        let sub = if path::file_name(root) == "@types" && mangled != name {
            tr!(tr, "Scoped package detected, looking in '{mangled}'");
            mangled.as_str()
        } else {
            name
        };
        let candidate = path::combine(root, sub);
        if !self.dir_exists(path::directory_of(&candidate)) {
            return None;
        }
        if as_file
            && !esm_strict
            && let Some(f) = self.load_file(&candidate, DECL, true, tr)
        {
            return Some((f, candidate));
        }
        if !self.dir_exists(&candidate) {
            return None;
        }
        let allow_index = !esm_strict || self.package_json(&candidate).is_some();
        let f = self.load_directory(
            &candidate,
            DECL,
            allow_index,
            0,
            PackageJsonSource::Probe,
            tr,
        )?;
        Some((f, candidate))
    }

    fn finish_type_ref(
        &self,
        found: Found,
        package_id: Option<PackageId>,
        primary: bool,
        tr: &mut Tracer<'_>,
    ) -> ResolvedTypeReference {
        let (resolved, original) = if self.options.preserve_symlinks {
            (found.path.clone(), None)
        } else {
            let real = self.realpath_traced(&found.path, tr);
            if real == found.path {
                (real, None)
            } else {
                (real, Some(found.path.clone()))
            }
        };
        ResolvedTypeReference {
            is_external_library_import: resolved.contains("/node_modules/"),
            resolved_file_name: resolved,
            original_path: original,
            primary,
            package_id,
        }
    }
}

/// [`match_subpath`] with the key of the entry that matched (the pattern for `*` keys).
fn match_subpath_keyed<'v>(
    map: &'v Value,
    key: &'v str,
) -> Option<(&'v Value, Option<String>, &'v str)> {
    let (v, rep) = match_subpath(map, key)?;
    let matched = map
        .as_object()?
        .iter()
        .rev()
        .find(|m| std::ptr::eq(&m.value, v))
        .map_or(key, |m| m.key.as_str());
    Some((v, rep, matched))
}
