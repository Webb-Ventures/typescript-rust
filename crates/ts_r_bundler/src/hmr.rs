//! The dev server's dependency pre-bundler (feature `hmr`): [`DepBundle`] implements
//! `ts_r_hmr`'s [`DepBundler`] seam, so `ts-r-dev` serves every bare import as one ES module
//! built by this crate, and CommonJS packages (React, React DOM, scheduler) work.
//!
//! Per request, the dependency's entry file and everything it reaches through relative
//! imports are bundled into one ESM module. Every bare specifier (other packages, and the
//! package's own other entry points) stays external, so the browser loads one copy of each
//! package: the server rewrites those imports to `/@modules/…` like any other.
//!
//! - ESM entries keep their exports. An entry is ESM when it has `import`/`export` syntax or
//!   Node would load it as a module (`.mjs`, `"type": "module"`), even if its only imports
//!   are dynamic.
//! - A dynamic `import()` of a stylesheet stays external, as the URL the server serves the
//!   original file at (when [`DepBundle::serve_root`] is set), as Vite does.
//! - CommonJS entries get a generated ESM facade: `default` (with the usual `__esModule`
//!   interop), a named export per statically detected export name ([`crate::cjs_exports`],
//!   following `module.exports = require("./x")` chains such as React's
//!   `index.js` → `cjs/react.development.js`), and `"module.exports"`, the raw exports
//!   object, which is what a peer's `require()` of this package returns.
//! - `require("peer")` inside a dependency becomes `import * as … from "peer"` unwrapped by
//!   `__tsrToCjs` (`BuildOptions::require_to_import`).
//! - `process.env.NODE_ENV` is `"development"` unless set otherwise, and the `development`
//!   export condition is on, as dev servers conventionally do.
//!
//! [`DepBundle::batch`] (the seam's `bundle_batch`, which `ts-r-dev` uses) bundles every
//! scanned dependency entry in one build with code splitting instead: packages are bundled
//! *into* the batch (not external), one output per entry named after its specifier
//! (`react-dom_client.js`), and modules reached from more than one entry go to shared
//! `chunk-<hash>.js` files, so React exists once however many of its entry points the app
//! imports. CommonJS entries get the same facade as above. Output names are unique
//! case-insensitively, never start with `chunk-`, and are deterministic for the same inputs.
//! Each output has a source map (`sources` relative to the virtual output directory, which
//! [`DepBundle::serve_root`] puts at the URL the server serves batches from, and
//! `sourcesContent`), and an entry's CSS (dependencies importing `.css`) comes back as a
//! same-named `.css` file.

use std::collections::BTreeMap;
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_config::fs::{DirEntry, EntryKind};
use ts_r_config::{FileSystem, RealFs, path};
use ts_r_hmr::{DepBatch, DepBundler, DepRequest, TransformError, TransformOutput};

use crate::options::{BuildOptions, DynamicImportUrl, EntryPoint, Format, Platform, SourceMapMode};
use crate::resolve::{Resolution, ResolveKind, Resolver};
use crate::{BuildContext, cjs_exports};

/// Name of the generated facade module, next to the dependency's entry file.
const FACADE: &str = "__tsr_dep_entry__.js";

/// Virtual output directory of batch builds (nothing is written).
const OUT_DIR: &str = "/__tsr_dep_out__";

/// Depth limit when following `module.exports = require(…)` chains.
const MAX_REEXPORT_DEPTH: usize = 16;

/// The `DepBundler` for `ts-r-dev`. See the module docs.
pub struct DepBundle {
    fs: Arc<dyn FileSystem>,
    node_env: String,
    id: String,
    /// Virtual output directory of batch builds; source map `sources` are relative to it.
    out_dir: String,
    /// The dev server root ([`DepBundle::serve_root`]), for URLs in dependency CSS.
    root: Option<std::path::PathBuf>,
}

impl std::fmt::Debug for DepBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DepBundle")
            .field("node_env", &self.node_env)
            .finish_non_exhaustive()
    }
}

impl Default for DepBundle {
    fn default() -> Self {
        DepBundle::new()
    }
}

impl DepBundle {
    /// Bundles from the real file system with `NODE_ENV=development`.
    pub fn new() -> DepBundle {
        DepBundle::with_fs(Arc::new(RealFs::new()))
    }

    pub fn with_fs(fs: Arc<dyn FileSystem>) -> DepBundle {
        DepBundle::build(fs, "development".into())
    }

    /// Sets the value `process.env.NODE_ENV` is replaced with (it is part of [`DepBundler::id`]).
    pub fn node_env(self, env: &str) -> DepBundle {
        let out_dir = self.out_dir;
        let root = self.root;
        DepBundle {
            out_dir,
            root,
            ..DepBundle::build(self.fs, env.to_owned())
        }
    }

    /// Batch outputs are served from `<root>/@ts-r/deps/` (the dev server's `DEPS_PREFIX`), so
    /// source map `sources` come out relative to that URL: `../../node_modules/react/index.js`
    /// resolves to `/node_modules/react/index.js`, and no absolute path is in the output.
    /// Dependency CSS is read with `@import`s inlined and `url()`s rewritten to the URLs the
    /// server serves those files at (`ts_r_hmr::css`).
    ///
    /// A Windows verbatim root (`\\?\C:\x`, as `fs::canonicalize` returns) is used in its
    /// plain form, so it shares a prefix with the resolver's file paths.
    pub fn serve_root(mut self, root: &std::path::Path) -> DepBundle {
        let root = ts_r_hmr::paths::strip_verbatim(root.to_path_buf());
        let root = path::normalize_slashes(&root.to_string_lossy());
        self.out_dir = format!("{}/@ts-r/deps", root.trim_end_matches('/'));
        self.root = Some(std::path::PathBuf::from(root));
        self
    }

    fn build(fs: Arc<dyn FileSystem>, node_env: String) -> DepBundle {
        let id = format!("ts-r-bundler/{}/{node_env}", env!("CARGO_PKG_VERSION"));
        DepBundle {
            fs,
            node_env,
            id,
            out_dir: OUT_DIR.to_owned(),
            root: None,
        }
    }

    fn options(&self, dir: &str, entry: &str) -> BuildOptions {
        BuildOptions {
            entry_points: vec![EntryPoint {
                path: entry.to_owned(),
                name: Some("dep".into()),
            }],
            working_dir: dir.to_owned(),
            bundle: true,
            outdir: Some("/__tsr_dep_out__".into()),
            format: Some(Format::Esm),
            platform: Platform::Browser,
            packages_external: true,
            require_to_import: true,
            conditions: Some(vec!["development".into(), "module".into()]),
            define: vec![(
                "process.env.NODE_ENV".into(),
                format!("\"{}\"", self.node_env.replace(['"', '\\'], "")),
            )],
            write: false,
            threads: 2,
            persistent_cache: false,
            // Read through `Overlay`, which bundles CSS for the dev server (`ts_r_hmr::css`).
            css_prebundled: self.root.is_some(),
            // Served as read, as Vite serves dependency CSS.
            charset_utf8: true,
            dynamic_import_url: self.root.clone().map(|root| {
                DynamicImportUrl(Arc::new(move |file: &str| {
                    is_style_file(file)
                        .then(|| ts_r_hmr::css::asset_url(&root, std::path::Path::new(file)))
                }))
            }),
            ..BuildOptions::default()
        }
    }

    /// The entry is an ES module: it has `import`/`export` syntax, or Node would load it as
    /// one (`.mjs`, or `.js` in a `"type": "module"` package) even though its only imports
    /// are dynamic. Anything else gets the CommonJS facade.
    fn is_esm(&self, file: &str, source: &str) -> bool {
        if ts_r_hmr::scan::scan(source).has_module_syntax {
            return true;
        }
        let lower = file.to_ascii_lowercase();
        if lower.ends_with(".mjs") || lower.ends_with(".mts") {
            return true;
        }
        if lower.ends_with(".cjs") || lower.ends_with(".cts") {
            return false;
        }
        let dir = path::directory_of(file);
        let opts = self.options(dir, file);
        Resolver::new(Arc::clone(&self.fs), &opts)
            .package_scope(dir)
            .is_some_and(|p| p.is_module)
    }

    /// Export names of a CommonJS file, following re-export chains (bare re-exports are
    /// followed too: their names are re-exported by value at runtime).
    fn cjs_names(&self, file: &str, opts: &BuildOptions) -> Vec<String> {
        let mut resolve_opts = opts.clone();
        resolve_opts.packages_external = false;
        let resolver = Resolver::new(Arc::clone(&self.fs), &resolve_opts);
        let mut names = Vec::new();
        let mut seen_names = FxHashSet::default();
        let mut visited = FxHashSet::default();
        let mut stack = vec![(file.to_owned(), 0usize)];
        while let Some((f, depth)) = stack.pop() {
            if depth > MAX_REEXPORT_DEPTH || !visited.insert(f.clone()) {
                continue;
            }
            let Some(src) = self.fs.read_file(&f) else {
                continue;
            };
            let found = cjs_exports::analyze(&src);
            for n in found.names {
                if seen_names.insert(n.clone()) {
                    names.push(n);
                }
            }
            // Reverse so the first re-export is analyzed first.
            for spec in found.reexports.iter().rev() {
                if let Resolution::File(p) =
                    resolver.resolve(spec, path::directory_of(&f), ResolveKind::Require)
                {
                    stack.push((p, depth + 1));
                }
            }
        }
        names
    }
}

/// The ESM facade for a CommonJS entry.
fn facade(entry_name: &str, names: &[String]) -> String {
    let spec = serde_like_string(&format!("./{entry_name}"));
    let mut s = format!(
        "const __tsr_m = require({spec});\n\
         export default (__tsr_m && __tsr_m.__esModule ? __tsr_m.default : __tsr_m);\n\
         export {{ __tsr_m as \"module.exports\" }};\n"
    );
    let mut i = 0;
    for n in names {
        if n == "default" || n == "__esModule" || !is_identifier_name(n) {
            continue;
        }
        s.push_str(&format!(
            "const __tsr_{i} = __tsr_m.{n};\nexport {{ __tsr_{i} as {n} }};\n"
        ));
        i += 1;
    }
    s
}

fn is_identifier_name(n: &str) -> bool {
    let mut chars = n.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c == '$' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c == '$' || c.is_ascii_alphanumeric())
}

/// A JS string literal for a file name (no escapes needed beyond quotes and backslashes).
fn serde_like_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

impl DepBundle {
    /// Pre-bundles every dependency entry of an app in one build with code splitting
    /// ([`DepBundler::bundle_batch`]). Unlike [`DepBundler::bundle`], packages are not
    /// external: each entry's whole dependency graph is bundled, and a module reached from
    /// several entries (React from `react-dom`, a package's internal file shared by two of
    /// its entry points) goes to one shared chunk, so it exists, and runs, once.
    ///
    /// Entries are named after their specifier (`react-dom/client` → `react-dom_client.js`);
    /// shared chunks are `chunk-<hash>.js`. Requests that resolve to the same file share an
    /// entry.
    pub fn batch(&self, deps: &[DepRequest<'_>]) -> Result<DepBatch, TransformError> {
        let mut overlay = Overlay::new(Arc::clone(&self.fs));
        overlay.css_root = self.root.clone();
        let mut entry_points: Vec<EntryPoint> = Vec::new();
        let mut entry_of_file: FxHashMap<String, usize> = FxHashMap::default();
        let mut used_names: FxHashSet<String> = FxHashSet::default();
        let mut per_request = Vec::with_capacity(deps.len());
        let mut real_files = Vec::new();
        for dep in deps {
            let file = plain_file(dep.file);
            let file = self.fs.realpath(&file);
            if let Some(&i) = entry_of_file.get(&file) {
                per_request.push(i);
                continue;
            }
            let Some(source) = self.fs.read_file(&file) else {
                return Err(TransformError::new(format!(
                    "pre-bundling \"{}\": cannot read {file}",
                    dep.specifier
                )));
            };
            let i = entry_points.len();
            let entry = if self.is_esm(&file, &source) {
                file.clone()
            } else {
                let dir = path::directory_of(&file);
                let probe = self.options(dir, &file);
                let names = self.cjs_names(&file, &probe);
                let facade_path = format!("{}/__tsr_dep_{i}__.js", dir.trim_end_matches('/'));
                overlay
                    .files
                    .insert(facade_path.clone(), facade(path::file_name(&file), &names));
                facade_path
            };
            let name = unique_name(&mut used_names, dep.specifier);
            entry_points.push(EntryPoint {
                path: entry,
                name: Some(name),
            });
            entry_of_file.insert(file.clone(), i);
            real_files.push(file);
            per_request.push(i);
        }
        if entry_points.is_empty() {
            return Ok(DepBatch::default());
        }
        let working_dir = common_dir(&real_files);
        let mut opts = self.options(&working_dir, "");
        opts.entry_points = entry_points;
        opts.outdir = Some(self.out_dir.clone());
        // Maps point at the original files (`sources` relative to the output directory,
        // `sourcesContent` included); the server links them with `sourceMappingURL`.
        opts.sourcemap = SourceMapMode::External;
        opts.packages_external = false;
        opts.require_to_import = false;
        opts.splitting = true;
        opts.entry_names = "[name]".into();
        opts.chunk_names = "chunk-[hash]".into();
        opts.threads = 0;
        let names: Vec<String> = opts
            .entry_points
            .iter()
            .map(|e| format!("{}.js", e.name.as_deref().unwrap_or("dep")))
            .collect();
        let ctx = BuildContext::new(opts, Arc::new(overlay)).map_err(|e| {
            TransformError::new(
                e.iter()
                    .map(|m| m.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        })?;
        let r = ctx.build();
        if r.has_errors() {
            let msg = r
                .errors()
                .map(|m| m.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            return Err(TransformError::new(format!(
                "pre-bundling dependencies failed:\n{msg}"
            )));
        }
        let prefix = format!("{}/", self.out_dir);
        let maps: FxHashMap<&str, &crate::OutputFile> = r
            .outputs
            .iter()
            .filter_map(|o| Some((o.path.strip_prefix(&prefix)?.strip_suffix(".map")?, o)))
            .collect();
        let mut files = Vec::new();
        for o in &r.outputs {
            let Some(name) = o.path.strip_prefix(&prefix) else {
                continue;
            };
            if name.contains('/') {
                continue;
            }
            if name.ends_with(".css") {
                // CSS the same-named `.js` file imports (concatenated in import order).
                files.push((
                    name.to_owned(),
                    TransformOutput {
                        code: o.text().to_owned(),
                        map: None,
                        imports: Vec::new(),
                    },
                ));
                continue;
            }
            if !name.ends_with(".js") {
                continue;
            }
            let code = o.text().to_owned();
            let imports = ts_r_hmr::scan::scan(&code).imports;
            files.push((
                name.to_owned(),
                TransformOutput {
                    code,
                    map: maps.get(name).map(|m| m.text().to_owned()),
                    imports,
                },
            ));
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        for n in &names {
            if !files.iter().any(|(f, _)| f == n) {
                return Err(TransformError::new(format!(
                    "pre-bundling dependencies produced no {n}"
                )));
            }
        }
        Ok(DepBatch {
            entries: per_request.into_iter().map(|i| names[i].clone()).collect(),
            files,
        })
    }
}

/// Stylesheets the dev server serves (and transforms) itself: a dependency's dynamic
/// `import()` of one stays a request for the original file, as in Vite.
fn is_style_file(file: &str) -> bool {
    let lower = split_query_free(file).to_ascii_lowercase();
    [".css", ".scss", ".sass", ".less", ".styl", ".stylus"]
        .iter()
        .any(|e| lower.ends_with(e))
}

fn split_query_free(file: &str) -> &str {
    file.split(['?', '#']).next().unwrap_or(file)
}

/// `react-dom/client` → `react-dom_client`; `@scope/pkg` → `@scope_pkg`; unique in `used`
/// (case-insensitively, for case-insensitive file systems and URLs alike).
fn unique_name(used: &mut FxHashSet<String>, specifier: &str) -> String {
    let base: String = specifier
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '@' => c,
            _ => '_',
        })
        .collect();
    let base = base.trim_matches('.');
    let base = if base.is_empty() || base.starts_with("chunk-") {
        format!("dep_{base}")
    } else {
        base.to_owned()
    };
    let mut name = base.clone();
    let mut n = 2;
    while !used.insert(name.to_ascii_lowercase()) {
        name = format!("{base}_{n}");
        n += 1;
    }
    name
}

/// The deepest directory containing every file.
fn common_dir(files: &[String]) -> String {
    let mut it = files.iter();
    let Some(first) = it.next() else {
        return "/".into();
    };
    let mut dir: Vec<&str> = path::directory_of(first).split('/').collect();
    for f in it {
        let d: Vec<&str> = path::directory_of(f).split('/').collect();
        let n = dir.iter().zip(&d).take_while(|(a, b)| a == b).count();
        dir.truncate(n);
    }
    let s = dir.join("/");
    if s.is_empty() { "/".into() } else { s }
}

impl DepBundler for DepBundle {
    fn id(&self) -> &str {
        &self.id
    }

    fn bundle_batch(&self, deps: &[DepRequest<'_>]) -> Option<Result<DepBatch, TransformError>> {
        Some(self.batch(deps))
    }

    fn bundle(
        &self,
        dep: &DepRequest<'_>,
        source: &str,
    ) -> Result<TransformOutput, TransformError> {
        let file = plain_file(dep.file);
        let file = self.fs.realpath(&file);
        let dir = path::directory_of(&file).to_owned();
        let esm = self.is_esm(&file, source);
        let (fs, entry): (Arc<dyn FileSystem>, String) = if esm {
            (Arc::clone(&self.fs), file.clone())
        } else {
            let probe = self.options(&dir, &file);
            let names = self.cjs_names(&file, &probe);
            let entry = format!("{}/{FACADE}", dir.trim_end_matches('/'));
            let code = facade(path::file_name(&file), &names);
            let mut overlay = Overlay::new(Arc::clone(&self.fs));
            overlay.files.insert(entry.clone(), code);
            (Arc::new(overlay), entry)
        };
        let opts = self.options(&dir, &entry);
        let ctx = BuildContext::new(opts, fs).map_err(|e| {
            TransformError::new(
                e.iter()
                    .map(|m| m.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        })?;
        let r = ctx.build();
        if r.has_errors() {
            let msg = r
                .errors()
                .map(|m| m.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            return Err(TransformError::new(format!(
                "pre-bundling \"{}\" failed:\n{msg}",
                dep.specifier
            )));
        }
        let Some(out) = r.outputs.iter().find(|o| o.path.ends_with(".js")) else {
            return Err(TransformError::new(format!(
                "pre-bundling \"{}\" produced no JavaScript",
                dep.specifier
            )));
        };
        let code = out.text().to_owned();
        let imports = ts_r_hmr::scan::scan(&code).imports;
        Ok(TransformOutput {
            code,
            map: None,
            imports,
        })
    }
}

/// The real file system plus generated files (the CommonJS facades).
struct Overlay {
    inner: Arc<dyn FileSystem>,
    /// Absolute path -> contents; sorted so directory listings are deterministic.
    files: BTreeMap<String, String>,
    /// The dev server root, when known: CSS is read with `@import`s inlined and `url()`s
    /// made absolute (`ts_r_hmr::css`), since the batch's CSS is served from another URL.
    css_root: Option<std::path::PathBuf>,
}

impl Overlay {
    fn new(inner: Arc<dyn FileSystem>) -> Overlay {
        Overlay {
            inner,
            files: BTreeMap::new(),
            css_root: None,
        }
    }
}

impl FileSystem for Overlay {
    fn read_bytes(&self, p: &str) -> Option<Vec<u8>> {
        if let Some(c) = self.files.get(p) {
            return Some(c.as_bytes().to_vec());
        }
        self.inner.read_bytes(p)
    }

    fn read_file(&self, p: &str) -> Option<String> {
        if let Some(c) = self.files.get(p) {
            return Some(c.clone());
        }
        if let Some(root) = &self.css_root
            && p.ends_with(".css")
        {
            let read = |f: &std::path::Path| self.inner.read_file(&f.to_string_lossy());
            let url_of = |f: &std::path::Path| ts_r_hmr::css::asset_url(root, f);
            return ts_r_hmr::css::bundle_css(std::path::Path::new(p), &read, &url_of);
        }
        self.inner.read_file(p)
    }

    fn file_exists(&self, p: &str) -> bool {
        self.files.contains_key(p) || self.inner.file_exists(p)
    }

    fn directory_exists(&self, p: &str) -> bool {
        self.inner.directory_exists(p)
    }

    fn read_dir(&self, p: &str) -> Vec<DirEntry> {
        let mut v = self.inner.read_dir(p);
        let mut added = false;
        for f in self.files.keys() {
            if path::directory_of(f) == p {
                let name = path::file_name(f).to_owned();
                if !v.iter().any(|e| e.name == name) {
                    v.push(DirEntry {
                        name,
                        kind: EntryKind::File,
                    });
                    added = true;
                }
            }
        }
        if added {
            v.sort_by(|a, b| a.name.cmp(&b.name));
        }
        v
    }

    fn realpath(&self, p: &str) -> String {
        if self.files.contains_key(p) {
            return p.to_owned();
        }
        self.inner.realpath(p)
    }

    fn case_sensitive(&self) -> bool {
        self.inner.case_sensitive()
    }

    fn cwd(&self) -> String {
        self.inner.cwd()
    }
}

/// A request's file with `/` separators and without a Windows verbatim prefix, so it shares a
/// prefix with the serve root ([`DepBundle::serve_root`]) and the resolver's paths.
fn plain_file(file: &std::path::Path) -> String {
    let file = ts_r_hmr::paths::strip_verbatim(file.to_path_buf());
    path::normalize_slashes(&file.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows CI: a request for `\\?\D:\x\pkg\index.js` (what `fs::canonicalize`
    /// returns) imported `/@ts-r/up/9/D:/%5C/x/...` instead of `/x/...` under a plain root.
    #[test]
    fn verbatim_request_files_lose_their_prefix() {
        assert_eq!(
            plain_file(std::path::Path::new(r"\\?\D:\x\pkg\index.js")),
            "D:/x/pkg/index.js"
        );
        assert_eq!(
            plain_file(std::path::Path::new("/x/index.js")),
            "/x/index.js"
        );
    }

    #[test]
    fn facade_exports() {
        let f = facade(
            "index.js",
            &["useState".into(), "default".into(), "a-b".into()],
        );
        assert!(f.contains("require(\"./index.js\")"), "{f}");
        assert!(f.contains("export { __tsr_0 as useState };"), "{f}");
        assert!(!f.contains("a-b"), "{f}");
        assert!(f.contains("\"module.exports\""), "{f}");
    }
}
