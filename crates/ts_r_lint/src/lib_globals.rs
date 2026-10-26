//! TypeScript lib globals: the names typescript-eslint's scope analysis defines in the global
//! scope of code it parses (observed behaviour; see design record 0006).
//!
//! - Which lib files: `parserOptions.lib` if set; else, with `parserOptions.project` or
//!   `projectService`, the `lib` (or `target`'s default lib) of the file's tsconfig; else
//!   `esnext`. `/// <reference lib>` directives are followed.
//! - A lib name is a global variable when it has a type meaning (interface, type alias, class,
//!   enum, namespace). It satisfies type references, and value references too when it also
//!   has a value meaning (`Map`: interface + `declare var`). A name with only a value meaning
//!   (`document`, `structuredClone`) is not defined at all.
//! - `builtinGlobals` options count every lib global variable.
//!
//! The names come from [`crate::lib_globals_generated`], generated from the pinned TypeScript
//! lib files by `cargo xtask codegen lint-globals`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use rustc_hash::FxHashMap;

use crate::lib_globals_generated::{LIB_FILES, LibFile};

const TYPE: u8 = 1;
const VALUE: u8 = 2;

/// typescript-eslint's lib when there is neither `parserOptions.lib` nor a program.
pub const DEFAULT_LIB_FILE: &str = "lib.esnext.d.ts";

/// The global names of a set of lib files.
#[derive(Default, PartialEq, Eq)]
pub struct LibGlobals {
    /// Every file in the closure, sorted.
    files: Vec<&'static str>,
    names: FxHashMap<&'static str, u8>,
}

impl std::fmt::Debug for LibGlobals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LibGlobals")
            .field("files", &self.files)
            .field("names", &self.names.len())
            .finish()
    }
}

fn lib_file(name: &str) -> Option<&'static LibFile> {
    LIB_FILES
        .binary_search_by(|f| f.file.cmp(name))
        .ok()
        .map(|i| &LIB_FILES[i])
}

impl LibGlobals {
    /// typescript-eslint's default: `esnext`.
    pub fn default_lib() -> Arc<LibGlobals> {
        Self::for_files(&[DEFAULT_LIB_FILE])
    }

    /// From `lib` option spellings (`es2022`, `DOM.Iterable`). Unknown names are ignored.
    pub fn for_lib_names<S: AsRef<str>>(names: &[S]) -> Arc<LibGlobals> {
        let files: Vec<&'static str> = names
            .iter()
            .filter_map(|n| ts_r_config::lib_file_name(n.as_ref()))
            .collect();
        Self::for_files(&files)
    }

    /// From lib file names (`lib.es2020.full.d.ts`) and everything they reference. Cached per
    /// set of root files.
    pub fn for_files(roots: &[&'static str]) -> Arc<LibGlobals> {
        static CACHE: OnceLock<Mutex<FxHashMap<Vec<&'static str>, Arc<LibGlobals>>>> =
            OnceLock::new();
        let mut key = roots.to_vec();
        key.sort_unstable();
        key.dedup();
        let cache = CACHE.get_or_init(Default::default);
        if let Ok(c) = cache.lock()
            && let Some(g) = c.get(&key)
        {
            return g.clone();
        }
        let g = Arc::new(Self::build(&key));
        // Another thread may have built the same set meanwhile: keep the first one, so equal
        // lib sets always share one table.
        match cache.lock() {
            Ok(mut c) => c.entry(key).or_insert(g).clone(),
            Err(_) => g,
        }
    }

    fn build(roots: &[&'static str]) -> LibGlobals {
        let mut files: Vec<&'static str> = Vec::new();
        let mut stack: Vec<&'static LibFile> = roots.iter().filter_map(|r| lib_file(r)).collect();
        // The closure is at most every lib file, so this terminates.
        while let Some(f) = stack.pop() {
            if files.contains(&f.file) {
                continue;
            }
            files.push(f.file);
            stack.extend(f.references.iter().filter_map(|r| lib_file(r)));
        }
        files.sort_unstable();
        let mut names: FxHashMap<&'static str, u8> = FxHashMap::default();
        for f in files.iter().filter_map(|f| lib_file(f)) {
            for &t in f.types {
                *names.entry(t).or_default() |= TYPE;
            }
            for &v in f.values {
                *names.entry(v).or_default() |= VALUE;
            }
        }
        // Only names with a type meaning are variables.
        names.retain(|_, b| *b & TYPE != 0);
        LibGlobals { files, names }
    }

    /// The lib files in the closure, sorted.
    pub fn files(&self) -> &[&'static str] {
        &self.files
    }

    /// A global variable defined by the lib (counts for `builtinGlobals`).
    pub fn is_global(&self, name: &str) -> bool {
        self.names.contains_key(name)
    }

    /// A type reference to `name` resolves to the lib.
    pub fn resolves_type(&self, name: &str) -> bool {
        self.names.contains_key(name)
    }

    /// A value reference to `name` resolves to the lib.
    pub fn resolves_value(&self, name: &str) -> bool {
        self.names.get(name).is_some_and(|b| *b & VALUE != 0)
    }
}

/// `parserOptions.project` / `projectService`: the lib comes from the file's tsconfig.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProjectLib {
    /// `tsconfigRootDir` (absolute), or the config file's directory.
    pub root_dir: PathBuf,
    /// `project` paths relative to `root_dir`. Empty: the nearest `tsconfig.json` above the
    /// file (`project: true`, `projectService`).
    pub projects: Vec<String>,
    /// `projectService` (rather than `project`) configured the program.
    pub service: bool,
    /// The lib globals come from the tsconfig (no `parserOptions.lib`).
    pub lib_from_tsconfig: bool,
    /// `parserOptions.emitDecoratorMetadata` / `experimentalDecorators`. Unset ones come
    /// from the file's tsconfig, as typescript-eslint takes them from the program.
    pub emit_decorator_metadata: Option<bool>,
    pub experimental_decorators: Option<bool>,
}

impl ProjectLib {
    /// The lib globals for `file` (an absolute path), from its tsconfig; the default lib when
    /// no tsconfig is found.
    pub fn resolve(&self, file: &str) -> Arc<LibGlobals> {
        let abs;
        let file = if Path::new(file).is_absolute() {
            file
        } else {
            abs = self.root_dir.join(file).to_string_lossy().into_owned();
            &abs
        };
        match self.tsconfig_for(file) {
            Some(cfg) => tsconfig_libs(&cfg),
            None => LibGlobals::default_lib(),
        }
    }

    /// Whether decorated classes in `file` (absolute path) reference their parameter and
    /// property types as values: `emitDecoratorMetadata` and `experimentalDecorators`, each
    /// from the parser options or else the file's tsconfig.
    pub fn decorator_metadata(&self, file: &str) -> bool {
        if self.emit_decorator_metadata == Some(false)
            || self.experimental_decorators == Some(false)
        {
            return false;
        }
        let from_tsconfig = match (self.emit_decorator_metadata, self.experimental_decorators) {
            (Some(true), Some(true)) => return true,
            _ => self
                .tsconfig_for(file)
                .map(|c| load(&c).decorators)
                .unwrap_or((false, false)),
        };
        self.emit_decorator_metadata.unwrap_or(from_tsconfig.0)
            && self.experimental_decorators.unwrap_or(from_tsconfig.1)
    }

    /// The tsconfig typescript-eslint would build `file`'s program from (absolute path).
    pub fn tsconfig_for(&self, file: &str) -> Option<String> {
        let fs = ts_r_config::RealFs::new();
        if self.projects.is_empty() {
            let dir = Path::new(file).parent()?.to_str()?;
            return nearest_tsconfig(dir);
        }
        let candidates: Vec<String> = self
            .projects
            .iter()
            .filter(|p| !p.contains('*'))
            .map(|p| self.root_dir.join(p).to_string_lossy().replace('\\', "/"))
            .filter(|p| ts_r_config::FileSystem::file_exists(&fs, p))
            .collect();
        if candidates.len() > 1 {
            // typescript-eslint uses the first project whose program contains the file.
            let norm = file.replace('\\', "/");
            if let Some(c) = candidates
                .iter()
                .find(|c| tsconfig_files(c).contains(&norm))
            {
                return Some(c.clone());
            }
        }
        candidates.into_iter().next()
    }
}

/// Nearest `tsconfig.json` at or above `dir`. Not cached: a long-lived process (the LSP
/// daemon) must see tsconfigs created or deleted later, and checking a cached answer would
/// take the same few `stat`s as the lookup.
fn nearest_tsconfig(dir: &str) -> Option<String> {
    ts_r_config::find_config_file(&ts_r_config::RealFs::new(), dir, "tsconfig.json")
}

struct LoadedTsconfig {
    libs: Arc<LibGlobals>,
    files: Arc<Vec<String>>,
    /// `emitDecoratorMetadata`, `experimentalDecorators`.
    decorators: (bool, bool),
    /// The tsconfig and every config it extends, with their modification time and size
    /// when loaded; the entry is reloaded once any of them changes.
    stamps: Vec<(String, Option<(SystemTime, u64)>)>,
}

fn stamp(path: &str) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

fn load(path: &str) -> Arc<LoadedTsconfig> {
    static CACHE: OnceLock<Mutex<FxHashMap<String, Arc<LoadedTsconfig>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Ok(c) = cache.lock()
        && let Some(r) = c.get(path)
        && r.stamps.iter().all(|(p, s)| stamp(p) == *s)
    {
        return r.clone();
    }
    let parsed = ts_r_config::load_tsconfig(&ts_r_config::RealFs::new(), path);
    let roots = parsed.options.effective().lib_files();
    let stamps = std::iter::once(path.to_owned())
        .chain(parsed.extended_config_paths.iter().cloned())
        .map(|p| {
            let s = stamp(&p);
            (p, s)
        })
        .collect();
    let r = Arc::new(LoadedTsconfig {
        libs: LibGlobals::for_files(&roots),
        decorators: (
            parsed.options.emit_decorator_metadata() == Some(true),
            parsed.options.experimental_decorators() == Some(true),
        ),
        files: Arc::new(parsed.file_names),
        stamps,
    });
    if let Ok(mut c) = cache.lock() {
        c.insert(path.to_owned(), r.clone());
    }
    r
}

/// The lib globals for a tsconfig's `lib` / `target` (cached per path while the tsconfig and
/// the configs it extends are unchanged).
pub fn tsconfig_libs(path: &str) -> Arc<LibGlobals> {
    load(path).libs.clone()
}

fn tsconfig_files(path: &str) -> Arc<Vec<String>> {
    load(path).files.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_closed() {
        assert!(LIB_FILES.windows(2).all(|w| w[0].file < w[1].file));
        for f in LIB_FILES {
            assert!(f.types.windows(2).all(|w| w[0] < w[1]), "{}", f.file);
            assert!(f.values.windows(2).all(|w| w[0] < w[1]), "{}", f.file);
            for r in f.references {
                assert!(lib_file(r).is_some(), "{} -> {r}", f.file);
            }
        }
    }

    #[test]
    fn default_is_esnext_without_dom() {
        let g = LibGlobals::default_lib();
        assert!(g.files().contains(&"lib.es5.d.ts"));
        assert!(g.files().contains(&"lib.esnext.disposable.d.ts"));
        assert!(!g.files().contains(&"lib.dom.d.ts"));
        assert!(g.resolves_type("PropertyKey"));
        assert!(!g.resolves_value("PropertyKey"));
        assert!(g.resolves_value("Map") && g.resolves_type("Map"));
        assert!(!g.is_global("RequestInit"));
        // Namespaces are both.
        assert!(g.resolves_value("Intl") && g.resolves_type("Intl"));
    }

    /// `(line, col, message)` of the no-undef reports for `src` linted as `path` with the
    /// TypeScript parser and `parser_options`.
    fn undef(base: &Path, path: &str, src: &str, parser_options: &str) -> Vec<String> {
        use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};
        let reg = crate::Registry::global();
        let cfg: serde_json::Value = serde_json::from_str(&format!(
            r#"[{{"files": ["**/*.ts"], "languageOptions": {{"parser": "typescript-eslint/parser",
                "parserOptions": {parser_options}}}, "rules": {{"no-undef": "error"}}}}]"#
        ))
        .unwrap();
        let objs = objects_from_eslint_json(&cfg, reg).unwrap();
        let file = ConfigFile::new(ConfigKind::Eslint, None, base.to_path_buf(), objs);
        let rel = Path::new(path)
            .strip_prefix(base)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let fc = file.file_config(&rel, &ResolveOptions::new()).unwrap();
        let r = crate::lint_source(path, src, &fc, &crate::NoTypeInfo, crate::types::FileKey(0));
        let pos = crate::output::Positions::new(src);
        r.diagnostics
            .iter()
            .map(|d| {
                let (l, c) = pos.line_col(d.span.start);
                format!("{l}:{c} {}", d.message)
            })
            .collect()
    }

    const SRC: &str = "type A = RequestInit;\ntype B = PropertyKey;\nconst c = PropertyKey;\n\
                       const d: Map<1, 2> = new Map();\nconst e = document;\ntype F = Nope;\n\
                       export { c, d, e };\n";

    #[test]
    fn no_undef_uses_the_lib() {
        let base = Path::new("/lg");
        assert_eq!(
            undef(base, "/lg/a.ts", SRC, "{}"),
            [
                "1:10 'RequestInit' is not defined.",
                "3:11 'PropertyKey' is not defined.",
                "5:11 'document' is not defined.",
                "6:10 'Nope' is not defined.",
            ]
        );
        assert_eq!(
            undef(base, "/lg/a.ts", SRC, r#"{"lib": ["es2015", "dom"]}"#),
            [
                "3:11 'PropertyKey' is not defined.",
                "5:11 'document' is not defined.",
                "6:10 'Nope' is not defined.",
            ]
        );
    }

    #[test]
    fn project_lib_comes_from_tsconfig() {
        let dir = std::env::temp_dir().join(format!("ts-r-lint-libs-{}", std::process::id()));
        let sub = dir.join("pkg/src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            dir.join("pkg/tsconfig.json"),
            r#"{"compilerOptions": {"target": "es2020"}}"#,
        )
        .unwrap();
        let file = sub.join("a.ts").to_string_lossy().into_owned();
        // `target: es2020` without `lib` is `es2020.full`, which includes the DOM.
        let got = undef(&dir, &file, SRC, r#"{"projectService": true}"#);
        assert_eq!(
            got,
            [
                "3:11 'PropertyKey' is not defined.",
                "5:11 'document' is not defined.",
                "6:10 'Nope' is not defined.",
            ]
        );
        // `lib` wins over the project.
        let got = undef(
            &dir,
            &file,
            SRC,
            r#"{"projectService": true, "lib": ["es5"]}"#,
        );
        assert!(got[0].starts_with("1:10 'RequestInit'"), "{got:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn project_lib_follows_tsconfig_changes() {
        let dir = std::env::temp_dir().join(format!("ts-r-lint-libs-edit-{}", std::process::id()));
        let sub = dir.join("pkg/src");
        std::fs::create_dir_all(&sub).unwrap();
        let base = dir.join("tsconfig.base.json");
        let cfg = dir.join("pkg/tsconfig.json");
        std::fs::write(&base, r#"{"compilerOptions": {"lib": ["es2020"]}}"#).unwrap();
        std::fs::write(&cfg, r#"{"extends": "../tsconfig.base.json"}"#).unwrap();
        let project = ProjectLib {
            root_dir: dir.clone(),
            projects: Vec::new(),
            service: false,
            lib_from_tsconfig: true,
            emit_decorator_metadata: None,
            experimental_decorators: None,
        };
        let file = sub.join("a.ts").to_string_lossy().into_owned();
        let dom = || project.resolve(&file).is_global("RequestInit");
        assert!(!dom());
        // The tsconfig itself changes.
        std::fs::write(
            &cfg,
            r#"{"extends": "../tsconfig.base.json", "compilerOptions": {"lib": ["es2020", "dom"]}}"#,
        )
        .unwrap();
        assert!(dom());
        // Back, then the config it extends changes.
        std::fs::write(&cfg, r#"{"extends": "../tsconfig.base.json"}"#).unwrap();
        assert!(!dom());
        std::fs::write(&base, r#"{"compilerOptions": {"lib": ["es2020", "dom"]}}"#).unwrap();
        assert!(dom());
        // A nearer tsconfig created later is found; deleting it falls back again.
        let nearer = sub.join("tsconfig.json");
        std::fs::write(&nearer, r#"{"compilerOptions": {"lib": ["es5"]}}"#).unwrap();
        assert!(!dom());
        std::fs::remove_file(&nearer).unwrap();
        assert!(dom());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn decorator_metadata_comes_from_tsconfig_when_unset() {
        let dir = std::env::temp_dir().join(format!("ts-r-lint-deco-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("tsconfig.json"),
            r#"{"compilerOptions": {"emitDecoratorMetadata": true, "experimentalDecorators": true}}"#,
        )
        .unwrap();
        let file = dir.join("a.ts").to_string_lossy().into_owned();
        let project = |emit, exp| ProjectLib {
            root_dir: dir.clone(),
            projects: Vec::new(),
            service: false,
            lib_from_tsconfig: true,
            emit_decorator_metadata: emit,
            experimental_decorators: exp,
        };
        assert!(project(None, None).decorator_metadata(&file));
        assert!(project(Some(true), None).decorator_metadata(&file));
        // An explicit parser option wins over the tsconfig.
        assert!(!project(Some(false), None).decorator_metadata(&file));
        assert!(!project(None, Some(false)).decorator_metadata(&file));
        std::fs::write(dir.join("tsconfig.json"), r#"{"compilerOptions": {}}"#).unwrap();
        assert!(!project(None, None).decorator_metadata(&file));
        assert!(project(Some(true), Some(true)).decorator_metadata(&file));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn values_only_are_not_globals() {
        let g = LibGlobals::for_lib_names(&["es2015", "DOM"]);
        assert!(g.resolves_type("RequestInit"));
        assert!(g.is_global("NodeListOf"));
        assert!(g.resolves_value("Event"));
        assert!(!g.is_global("document"));
        assert!(!g.is_global("structuredClone"));
        // `dom` references `es2015` and `es2018.asynciterable`.
        assert!(g.resolves_type("AsyncIterable"));
        assert!(!LibGlobals::for_lib_names(&["es2015"]).resolves_type("AsyncIterable"));
        assert!(Arc::ptr_eq(
            &g,
            &LibGlobals::for_lib_names(&["dom", "ES2015"])
        ));
    }
}
