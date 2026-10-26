//! [`ts_r_hmr::Transform`] for TypeScript, TSX and JSX, so `ts-r-dev` serves `.ts`/`.tsx`/
//! `.mts`/`.cts`/`.jsx` (feature `hmr`).
//!
//! Output is always an ES module (the dev server is unbundled ESM). `import.meta.hot` and
//! `import.meta.env` pass through untouched. Imports come from the printer's specifier spans,
//! so the server can rewrite them without rescanning.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rustc_hash::FxHashMap;
use ts_r_ast::SourceKind;
use ts_r_codegen::{CodegenOptions, SourceMapOptions, SpecifierKind};
use ts_r_config::glob::{Pattern, is_supported};
use ts_r_config::{FileStamp, FileSystem, ParsedConfig, RealFs, ScriptTarget};
use ts_r_hmr::scan::{ImportKind, ImportRecord};
use ts_r_hmr::transform::{Mode, Transform, TransformError, TransformInput, TransformOutput};
use ts_r_span::SourceText;

use crate::{JsxMode, ModuleFormat, TransformOptions, transpile};

/// A change applied to every file's options (JSX settings a plugin chose).
type Adjust = Arc<dyn Fn(&mut TransformOptions) + Send + Sync>;

/// The TypeScript/JSX transform for the dev server.
#[derive(Clone)]
pub struct TsTransform {
    /// Options for files no tsconfig.json covers (or every file, from [`TsTransform::new`]).
    options: TransformOptions,
    cache_id: String,
    /// Per-file tsconfig lookup ([`TsTransform::for_root`]); `None` uses `options` for all.
    configs: Option<Arc<Configs>>,
    adjust: Option<Adjust>,
}

impl std::fmt::Debug for TsTransform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TsTransform")
            .field("options", &self.options)
            .field("cache_id", &self.cache_id)
            .field("per_file_tsconfig", &self.configs.is_some())
            .finish()
    }
}

/// What the dev server always forces: ESM output, at least ES2022, JSX the browser can run.
fn for_browser(mut options: TransformOptions) -> TransformOptions {
    options.module = ModuleFormat::Esm {
        node_require: false,
    };
    options.cjs_module_option = false;
    if options.target < ScriptTarget::ES2022 {
        options.target = ScriptTarget::ES2022;
    }
    // Browsers can't run JSX; React's automatic runtime is what Vite uses by default.
    if options.jsx == JsxMode::Preserve {
        options.jsx = JsxMode::Automatic;
    }
    options
}

impl TsTransform {
    /// Uses `options` for every file (the module format is forced to ESM).
    pub fn new(options: TransformOptions) -> TsTransform {
        let options = for_browser(options);
        let cache_id = cache_id_of(&options);
        TsTransform {
            options,
            cache_id,
            configs: None,
            adjust: None,
        }
    }

    /// Each file gets the options of its own tsconfig.json, found as Vite finds it
    /// (tsconfck): the nearest `tsconfig.json` up from the file's directory; when that one
    /// has `references` and doesn't include the file (`files`/`include`/`exclude`), the
    /// first referenced config that does. Files no tsconfig covers get tsc's defaults.
    /// [`TsTransform::options`] are `root/tsconfig.json`'s.
    pub fn for_root(root: &Path) -> TsTransform {
        TsTransform::for_root_with_fs(root, Arc::new(RealFs::new()))
    }

    /// [`TsTransform::for_root`] on any file system (tests).
    pub fn for_root_with_fs(root: &Path, fs: Arc<dyn FileSystem>) -> TsTransform {
        let configs = Arc::new(Configs {
            fs,
            nearest: Mutex::default(),
            loaded: Mutex::default(),
        });
        let config = ts_r_config::path::normalize_slashes(&root.join("tsconfig.json").to_string_lossy());
        let options = match configs.load(&config) {
            Some(c) => TransformOptions::from_compiler_options(&c.parsed.options, "", false),
            None => TransformOptions::default(),
        };
        let mut t = TsTransform::new(options);
        t.configs = Some(configs);
        t
    }

    /// Applies `f` to every file's options after its tsconfig's (and to
    /// [`TsTransform::options`]); the per-file lookup is kept.
    pub fn map_options(mut self, f: impl Fn(&mut TransformOptions) + Send + Sync + 'static) -> TsTransform {
        f(&mut self.options);
        self.options = for_browser(self.options);
        self.cache_id = cache_id_of(&self.options);
        let f: Adjust = Arc::new(f);
        self.adjust = Some(match self.adjust.take() {
            None => f,
            Some(g) => Arc::new(move |o: &mut TransformOptions| {
                g(o);
                f(o);
            }),
        });
        self
    }

    pub fn options(&self) -> &TransformOptions {
        &self.options
    }

    /// The options `path` is transformed with.
    pub fn options_for(&self, path: &Path) -> TransformOptions {
        let Some(configs) = &self.configs else {
            return self.options.clone();
        };
        let file = ts_r_config::path::normalize_slashes(&path.to_string_lossy());
        let mut options = match configs.for_file(&file) {
            Some(c) => TransformOptions::from_compiler_options(&c.parsed.options, &file, false),
            None => TransformOptions::default(),
        };
        if let Some(f) = &self.adjust {
            f(&mut options);
        }
        for_browser(options)
    }
}

fn cache_id_of(options: &TransformOptions) -> String {
    // The executable's size and mtime stand in for the transform's revision, so a rebuilt
    // dev server never serves cached output of an older transform.
    let build = std::env::current_exe()
        .and_then(std::fs::metadata)
        .map(|m| {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{}-{t}", m.len())
        })
        .unwrap_or_default();
    format!(
        "ts-r-transform/{}+{build}/{:016x}",
        env!("CARGO_PKG_VERSION"),
        fx_hash(&options.cache_key())
    )
}

/// Loaded tsconfig files, shared by every clone of a [`TsTransform`].
struct Configs {
    fs: Arc<dyn FileSystem>,
    /// Directory → its nearest `tsconfig.json` (`None`: none up to the root).
    nearest: Mutex<FxHashMap<String, Option<String>>>,
    /// Config path → the parsed config, reloaded when one of its files changes.
    loaded: Mutex<FxHashMap<String, Arc<Loaded>>>,
}

struct Loaded {
    parsed: ParsedConfig,
    /// The config and the configs it extends, with their stamps when loaded.
    stamps: Vec<(String, Option<FileStamp>)>,
    include: Vec<Pattern>,
    exclude: Vec<Pattern>,
    files: Vec<String>,
}

impl Configs {
    fn nearest(&self, dir: &str) -> Option<String> {
        if let Some(hit) = self.nearest.lock().ok().and_then(|m| m.get(dir).cloned()) {
            return hit;
        }
        let found = ts_r_config::find_config_file(&*self.fs, dir, "tsconfig.json");
        if let Ok(mut m) = self.nearest.lock() {
            m.insert(dir.to_owned(), found.clone());
        }
        found
    }

    /// The parsed config at `path` (cached; re-read when it or a config it extends changed).
    fn load(&self, path: &str) -> Option<Arc<Loaded>> {
        let cached = self.loaded.lock().ok().and_then(|m| m.get(path).cloned());
        if let Some(c) = cached
            && c.stamps.iter().all(|(p, s)| self.fs.stamp(p) == *s)
        {
            return Some(c);
        }
        if !self.fs.file_exists(path) {
            return None;
        }
        let parsed = ts_r_config::load_tsconfig(&*self.fs, path);
        let stamps = std::iter::once(parsed.config_path.clone())
            .chain(parsed.extended_config_paths.iter().cloned())
            .map(|p| {
                let s = self.fs.stamp(&p);
                (p, s)
            })
            .collect();
        let files: Vec<String> = parsed
            .files
            .as_ref()
            .map(|f| f.absolute_specs())
            .unwrap_or_default();
        let include = match &parsed.include {
            Some(i) => i.absolute_specs(),
            None if parsed.files.is_some() => Vec::new(),
            None => vec![ts_r_config::path::combine(&parsed.config_dir, "**/*")],
        };
        let exclude = parsed
            .exclude
            .as_ref()
            .map(|e| e.absolute_specs())
            .unwrap_or_default();
        let loaded = Arc::new(Loaded {
            include: include.iter().map(|p| Pattern::new(p, true)).collect(),
            exclude: exclude.iter().map(|p| Pattern::new(p, false)).collect(),
            files,
            stamps,
            parsed,
        });
        if let Ok(mut m) = self.loaded.lock() {
            m.insert(path.to_owned(), loaded.clone());
        }
        Some(loaded)
    }

    /// The config `file` is transformed with (tsconfck's `parse`: nearest, then a
    /// solution's reference that includes the file).
    fn for_file(&self, file: &str) -> Option<Arc<Loaded>> {
        let dir = ts_r_config::path::directory_of(file);
        let config = self.load(&self.nearest(dir)?)?;
        if config.parsed.references.is_empty() || config.includes(file, &*self.fs) {
            return Some(config);
        }
        for r in &config.parsed.references {
            if let Some(c) = self.load(&r.config_path)
                && c.includes(file, &*self.fs)
            {
                return Some(c);
            }
        }
        Some(config)
    }
}

impl Loaded {
    /// `files`, else `include` (default `**/*` without `files`) minus `exclude`, as tsconfck
    /// tests it (no default excludes).
    fn includes(&self, file: &str, fs: &dyn FileSystem) -> bool {
        let cs = fs.case_sensitive();
        if self.files.iter().any(|f| ts_r_config::path::eq(f, file, cs)) {
            return true;
        }
        let allow_js = self.parsed.options.effective().allow_js();
        if !is_supported(file, allow_js) {
            return false;
        }
        self.include.iter().any(|p| p.matches(file, cs))
            && !self.exclude.iter().any(|p| p.matches_self_or_ancestor(file, cs))
    }
}

fn fx_hash(s: &str) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = rustc_hash::FxHasher::default();
    s.hash(&mut h);
    h.finish()
}

impl Transform for TsTransform {
    fn cache_id(&self) -> &str {
        &self.cache_id
    }

    fn handles(&self, path: &Path) -> bool {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return false;
        };
        let lower = name.to_ascii_lowercase();
        let ext = lower.rsplit_once('.').map_or("", |(_, e)| e);
        matches!(ext, "ts" | "tsx" | "mts" | "cts" | "jsx")
            && SourceKind::from_path(&lower) != SourceKind::Dts
    }

    fn deps(&self, input: &TransformInput<'_>) -> Vec<PathBuf> {
        // The file's tsconfig (and what it extends) decides its options.
        let Some(configs) = &self.configs else {
            return Vec::new();
        };
        let file = ts_r_config::path::normalize_slashes(&input.path.to_string_lossy());
        configs
            .for_file(&file)
            .map(|c| c.stamps.iter().map(|(p, _)| PathBuf::from(p)).collect())
            .unwrap_or_default()
    }

    fn transform(&self, input: &TransformInput<'_>) -> Result<TransformOutput, TransformError> {
        let path = input.path.to_string_lossy();
        let mut opts = self.options_for(input.path);
        if input.mode == Mode::Dev && opts.jsx == JsxMode::AutomaticDev {
            // Root-relative: the cache key does not include the project root.
            opts.jsx_file_name = input.id.to_owned();
        } else if opts.jsx == JsxMode::AutomaticDev {
            opts.jsx = JsxMode::Automatic;
        }
        let codegen = CodegenOptions {
            source_map: Some(SourceMapOptions {
                file: None,
                source: input.id.to_owned(),
                source_root: None,
                include_content: input.mode == Mode::Dev,
            }),
            collect_specifiers: true,
            ..CodegenOptions::default()
        };
        let out = transpile(&path, input.source, &opts, &codegen);
        if let Some(d) = out.build_errors.first() {
            let src = SourceText::new(path.to_string(), input.source);
            let lc = src.line_col_utf16(d.span.start);
            return Err(TransformError::at(
                format!("{} (TS{})", d.message, d.code),
                lc.line + 1,
                lc.col + 1,
            ));
        }
        let imports = out
            .specifiers
            .iter()
            .map(|s| ImportRecord {
                specifier: out.code[s.start as usize..s.end as usize].to_owned(),
                start: s.start,
                end: s.end,
                kind: match s.kind {
                    SpecifierKind::Import => ImportKind::Static,
                    SpecifierKind::Export => ImportKind::ReExport,
                    SpecifierKind::Dynamic => ImportKind::Dynamic,
                },
            })
            .collect();
        Ok(TransformOutput {
            code: out.code,
            map: out.map.map(|m| m.to_json()),
            imports,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(path: &str, src: &str) -> Result<TransformOutput, TransformError> {
        let t = TsTransform::new(TransformOptions::default());
        t.transform(&TransformInput {
            path: Path::new(path),
            id: path.trim_start_matches('/'),
            source: src,
            mode: Mode::Dev,
        })
    }

    #[test]
    fn jsx_dev_file_names_are_root_relative() {
        let t = TsTransform::new(TransformOptions::from_json(r#"{"jsx":"react-jsxdev"}"#, ""));
        let out = t
            .transform(&TransformInput {
                path: Path::new("/home/a/project/src/App.tsx"),
                id: "src/App.tsx",
                source: "export const App = () => <div />;\n",
                mode: Mode::Dev,
            })
            .unwrap();
        assert!(out.code.contains(r#"= "src/App.tsx""#), "{}", out.code);
        assert!(!out.code.contains("/home/a"), "{}", out.code);
    }

    fn run_with(t: &TsTransform, path: &str, src: &str) -> String {
        t.transform(&TransformInput {
            path: Path::new(path),
            id: path.trim_start_matches('/'),
            source: src,
            mode: Mode::Dev,
        })
        .unwrap()
        .code
    }

    /// Vite's tsconfig-json playground: each file gets its nearest tsconfig.json.
    fn nested_project() -> Arc<dyn FileSystem> {
        let mut fs = ts_r_config::MemoryFs::new();
        fs.add_file(
            "/p/tsconfig.json",
            r#"{ "compilerOptions": { "verbatimModuleSyntax": true }, "include": ["./src"] }"#,
        )
        .add_file(
            "/p/nested/tsconfig.json",
            r#"{ "compilerOptions": { "useDefineForClassFields": false } }"#,
        )
        .add_file("/p/src/main.ts", "")
        .add_file("/p/nested/main.ts", "");
        Arc::new(fs)
    }

    const TYPE_ONLY_USE: &str = "import { NestedTypeOnlyClass } from './not-used-type';\nexport const x: NestedTypeOnlyClass | null = null;\nexport class C { y = 1; }\n";

    #[test]
    fn each_file_uses_its_nearest_tsconfig() {
        let t = TsTransform::for_root_with_fs(Path::new("/p"), nested_project());
        // nested/tsconfig.json: no verbatimModuleSyntax (the type-only import goes) and
        // useDefineForClassFields off (the field moves into a constructor).
        let nested = run_with(&t, "/p/nested/main.ts", TYPE_ONLY_USE);
        assert!(!nested.contains("not-used-type"), "{nested}");
        assert!(nested.contains("this.y = 1"), "{nested}");
        // The root config's verbatimModuleSyntax keeps the import in src/.
        let src = run_with(&t, "/p/src/main.ts", TYPE_ONLY_USE);
        assert!(src.contains("not-used-type"), "{src}");
        assert!(!src.contains("this.y = 1"), "{src}");
        // Options adjusted afterwards (a plugin's JSX settings) keep the lookup.
        let t = t.map_options(|o| o.jsx_import_source = "preact".into());
        let nested = run_with(&t, "/p/nested/main.ts", TYPE_ONLY_USE);
        assert!(!nested.contains("not-used-type"), "{nested}");
        assert_eq!(t.options_for(Path::new("/p/src/a.tsx")).jsx_import_source, "preact");
        // The cache key covers the configs a file's options come from.
        let deps = t.deps(&TransformInput {
            path: Path::new("/p/nested/main.ts"),
            id: "nested/main.ts",
            source: "",
            mode: Mode::Dev,
        });
        assert_eq!(deps, [PathBuf::from("/p/nested/tsconfig.json")]);
    }

    #[test]
    fn a_solution_config_hands_files_to_the_reference_that_includes_them() {
        let mut fs = ts_r_config::MemoryFs::new();
        fs.add_file(
            "/s/tsconfig.json",
            r#"{ "files": [], "references": [{ "path": "./tsconfig.app.json" }] }"#,
        )
        .add_file(
            "/s/tsconfig.app.json",
            r#"{ "compilerOptions": { "verbatimModuleSyntax": true }, "include": ["src"] }"#,
        )
        .add_file("/s/src/main.ts", "")
        .add_file("/s/other.ts", "");
        let t = TsTransform::for_root_with_fs(Path::new("/s"), Arc::new(fs));
        let app = run_with(&t, "/s/src/main.ts", TYPE_ONLY_USE);
        assert!(app.contains("not-used-type"), "{app}");
        // Not included by any reference: the solution config itself (no options).
        let other = run_with(&t, "/s/other.ts", TYPE_ONLY_USE);
        assert!(!other.contains("not-used-type"), "{other}");
    }

    #[test]
    fn handles_typescript_and_jsx_only() {
        let t = TsTransform::new(TransformOptions::default());
        for p in ["a.ts", "b.tsx", "c.mts", "d.cts", "e.jsx"] {
            assert!(t.handles(Path::new(p)), "{p}");
        }
        for p in ["a.d.ts", "b.js", "c.css", "d"] {
            assert!(!t.handles(Path::new(p)), "{p}");
        }
    }

    #[test]
    fn strips_types_and_reports_imports_in_output() {
        let src = "import type { T } from './t';\nimport { a } from './a.ts';\nexport * from \"./b\";\nconst x: T = a as T;\nimport('./c').then(() => x);\nif (import.meta.hot) import.meta.hot.accept();\n";
        let out = run("/src/m.ts", src).unwrap();
        assert!(!out.code.contains(": T"), "{}", out.code);
        assert!(out.code.contains("import.meta.hot.accept()"));
        let specs: Vec<(&str, ImportKind)> = out
            .imports
            .iter()
            .map(|i| (i.specifier.as_str(), i.kind))
            .collect();
        assert_eq!(
            specs,
            [
                ("./a.ts", ImportKind::Static),
                ("./b", ImportKind::ReExport),
                ("./c", ImportKind::Dynamic)
            ]
        );
        for i in &out.imports {
            assert_eq!(&out.code[i.start as usize..i.end as usize], i.specifier);
        }
        assert!(
            out.map
                .as_deref()
                .is_some_and(|m| m.contains("\"sources\":[\"src/m.ts\"]"))
        );
    }

    #[test]
    fn jsx_uses_the_automatic_runtime_and_errors_have_positions() {
        let out = run("/src/App.tsx", "export const App = () => <div>hi</div>;\n").unwrap();
        assert!(out.code.contains("react/jsx-runtime"), "{}", out.code);
        assert!(
            out.imports
                .iter()
                .any(|i| i.specifier == "react/jsx-runtime")
        );
        let err = run("/src/bad.ts", "let x = 1;\nlet y: = 2;\n").unwrap_err();
        assert_eq!(err.line, Some(2));
    }
}
