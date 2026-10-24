//! The project a run belongs to: its tsconfig, the transform options derived from it, and
//! the module resolver.

use std::sync::Arc;

use ts_r_config::{
    CompilerOptions, DirEntry, FileSystem, ModuleResolution, RealFs, ScriptTarget,
    find_config_file, load_tsconfig,
};
use ts_r_resolver::{ImportKind, ResolutionMode, Resolver, ResolverOptions};
use ts_r_transform::{JsxMode, TransformOptions};

use crate::compile::PackageType;

/// The real file system without directory listing: loading a tsconfig for its options must
/// not glob the whole project (`include`), which ts-r-run never needs.
struct NoListing(RealFs);

impl FileSystem for NoListing {
    fn read_file(&self, path: &str) -> Option<String> {
        self.0.read_file(path)
    }
    fn file_exists(&self, path: &str) -> bool {
        self.0.file_exists(path)
    }
    fn directory_exists(&self, path: &str) -> bool {
        self.0.directory_exists(path)
    }
    fn read_dir(&self, _path: &str) -> Vec<DirEntry> {
        Vec::new()
    }
    fn realpath(&self, path: &str) -> String {
        self.0.realpath(path)
    }
    fn case_sensitive(&self) -> bool {
        self.0.case_sensitive()
    }
    fn cwd(&self) -> String {
        self.0.cwd()
    }
}

/// Where a resolved specifier points, when ts-r-run must tell Node.
pub struct Project {
    /// The tsconfig in use and the configs it extends (watched in `--watch`).
    pub config_files: Vec<String>,
    /// Transform options shared by every file (the module format is set per file).
    pub base: TransformOptions,
    /// For `.jsx` files when `allowJs` is off: tsx doesn't apply the tsconfig's JSX options
    /// to them, so they get the classic `React.createElement` runtime.
    pub plain_jsx: Option<TransformOptions>,
    pub resolver: Resolver<RealFs>,
    /// The tsconfig's compiler options (module resolution for `parse<T>` validators).
    pub options: CompilerOptions,
    /// Rendered errors of a `--tsconfig` that stop a run (see [`FATAL_CONFIG_CODES`]).
    pub config_errors: String,
}

/// tsconfig diagnostics that stop a run, as they stop tsx: the file can't be read (TS5083)
/// or an `extends` target doesn't exist (TS6053). Other option diagnostics are tsc's
/// business; ts-r-run runs anyway.
pub const FATAL_CONFIG_CODES: [u32; 2] = [5083, 6053];

impl std::fmt::Debug for Project {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Project")
            .field("config_files", &self.config_files)
            .finish_non_exhaustive()
    }
}

/// The fatal tsconfig diagnostics in tsc's format, paths relative to `cwd`.
fn render_fatal(
    diags: &[ts_r_config::ConfigDiagnostic],
    cwd: &str,
    case_sensitive: bool,
) -> String {
    let mut out = String::new();
    for d in diags
        .iter()
        .filter(|d| FATAL_CONFIG_CODES.contains(&d.code))
    {
        let display = |f: &str| {
            let rel = ts_r_config::path::relative(cwd, f, case_sensitive);
            if rel.starts_with("../") {
                f.to_owned()
            } else {
                rel
            }
        };
        out.push_str(&d.render(display, |f| std::fs::read_to_string(f).ok()));
        out.push('\n');
    }
    out
}

/// Normalised absolute path with `/` separators.
pub fn absolute(path: &str) -> String {
    let cwd = RealFs::new().cwd();
    ts_r_config::path::resolve(&cwd, &ts_r_config::path::normalize_slashes(path))
}

impl Project {
    /// The project for `entry`: `tsconfig` if given, else the nearest `tsconfig.json` above
    /// the entry, else tsc's defaults.
    pub fn load(entry: &str, tsconfig: Option<&str>) -> Project {
        let fs = NoListing(RealFs::new());
        let config = match tsconfig {
            Some(t) => Some(absolute(t)),
            None => find_config_file(&fs, ts_r_config::path::directory_of(entry), "tsconfig.json"),
        };
        let (options, config_files, errors) = match &config {
            Some(c) => {
                let parsed = load_tsconfig(&fs, c);
                let mut files = vec![parsed.config_path.clone()];
                files.extend(parsed.extended_config_paths.iter().cloned());
                // Like tsx: a broken tsconfig found next to the entry is ignored, a broken
                // `--tsconfig` stops the run.
                let errors = match tsconfig {
                    Some(_) => render_fatal(&parsed.diagnostics, &fs.cwd(), fs.case_sensitive()),
                    None => String::new(),
                };
                (parsed.options, files, errors)
            }
            None => (CompilerOptions::new(), Vec::new(), String::new()),
        };
        let mut project = Project::from_options(&options, config_files);
        project.config_errors = errors;
        project
    }

    pub fn from_options(options: &CompilerOptions, config_files: Vec<String>) -> Project {
        let mut base = TransformOptions::from_compiler_options(options, "", false);
        // ts-r emits ES2022+; Node 24 runs all of it. Class-field semantics stay as the
        // tsconfig's target implies (`useDefineForClassFields` was computed above).
        if base.target < ScriptTarget::ES2022 {
            base.target = ScriptTarget::ES2022;
        }
        // Node can't run JSX, and the dev runtime needs absolute file names in the output.
        // With no `jsx` option JSX is `React.createElement` calls (the classic runtime, as
        // under tsx); `preserve` and the `react-jsx*` modes use the automatic runtime.
        if options.jsx().is_none() {
            base.jsx = JsxMode::Classic;
        } else if base.jsx != JsxMode::Classic {
            base.jsx = JsxMode::Automatic;
        }
        base.jsx_file_name = String::new();
        base.jsx_preserve_explicit = false;
        // Specifiers are resolved by ts-r-run; tslib may not be installed.
        base.rewrite_relative_import_extensions = false;
        base.import_helpers = false;
        let allow_js = options.allow_js().or(options.check_js()).unwrap_or(false);
        let plain_jsx = (!allow_js).then(|| TransformOptions {
            jsx: JsxMode::Classic,
            jsx_factory: "React.createElement".to_owned(),
            jsx_fragment_factory: "React.Fragment".to_owned(),
            ..base.clone()
        });
        let mut ropts = ResolverOptions::from_compiler_options(options);
        // Like tsx: extensionless and directory imports work in ES modules too.
        ropts.algorithm = ModuleResolution::Bundler;
        Project {
            config_files,
            base,
            plain_jsx,
            resolver: Resolver::new(RealFs::new(), ropts),
            options: options.clone(),
            config_errors: String::new(),
        }
    }

    /// The `type` of the nearest `package.json`, and that file's path.
    pub fn package_type(&self, file: &str) -> (PackageType, Option<String>) {
        let dir = ts_r_config::path::directory_of(file);
        match self.resolver.package_scope(dir) {
            Some(pj) => {
                let ty = match pj.type_field.as_deref() {
                    Some("module") => PackageType::Module,
                    Some("commonjs") => PackageType::CommonJs,
                    _ => PackageType::Unspecified,
                };
                (
                    ty,
                    Some(ts_r_config::path::combine(&pj.dir, "package.json")),
                )
            }
            None => (PackageType::Unspecified, None),
        }
    }

    /// The transform options for `path` (the module format is set per file), and whether
    /// they are [`Project::plain_jsx`] (part of the cache key).
    pub fn options_for(&self, path: &str) -> (&TransformOptions, bool) {
        match &self.plain_jsx {
            Some(o) if path.to_ascii_lowercase().ends_with(".jsx") => (o, true),
            _ => (&self.base, false),
        }
    }

    /// Resolves `spec` imported from `parent` when Node's own resolution would not find the
    /// same file: relative and absolute specifiers (extension substitution, extensionless
    /// and directory imports), tsconfig `paths`, then a file under `baseUrl` (which tsc 7
    /// dropped but tsx still resolves). Other bare package specifiers are left to Node,
    /// which knows the runtime conditions. Declaration files are never targets.
    pub fn resolve(&self, spec: &str, parent: &str, require: bool) -> Option<Arc<str>> {
        if spec.starts_with("node:") || spec.starts_with("data:") || spec.starts_with('#') {
            return None;
        }
        let relative = spec.starts_with("./")
            || spec.starts_with("../")
            || spec == "."
            || spec == ".."
            || spec.starts_with('/');
        let mode = if require {
            ResolutionMode::Require
        } else {
            self.resolver.mode_for(parent, ImportKind::Static)
        };
        if relative {
            return self.target(self.resolver.resolve_module(spec, parent, mode)?);
        }
        if self.matches_paths(spec) {
            return self.mapped(spec, parent, mode);
        }
        let base = self.options.base_url()?;
        if spec.contains(':') {
            return None;
        }
        self.mapped(
            &format!("{}/{spec}", base.trim_end_matches('/')),
            parent,
            mode,
        )
    }

    /// A `paths` or `baseUrl` target; Node's fallback can't see the mapping, so the JSON
    /// files tsx also finds behind an extensionless or directory specifier are tried here.
    fn mapped(&self, spec: &str, parent: &str, mode: ResolutionMode) -> Option<Arc<str>> {
        if let Some(r) = self.resolver.resolve_module(spec, parent, mode) {
            return self.target(r);
        }
        let stem = spec.trim_end_matches('/');
        [format!("{stem}.json"), format!("{stem}/index.json")]
            .iter()
            .find_map(|s| self.target(self.resolver.resolve_module(s, parent, mode)?))
    }

    fn target(&self, r: Arc<ts_r_resolver::ResolvedModule>) -> Option<Arc<str>> {
        if r.extension.is_declaration() {
            return None;
        }
        Some(Arc::from(r.resolved_file_name.as_str()))
    }

    fn matches_paths(&self, spec: &str) -> bool {
        let Some(paths) = &self.resolver.options().paths else {
            return false;
        };
        paths
            .0
            .iter()
            .any(|(pattern, _)| match pattern.split_once('*') {
                Some((pre, post)) => {
                    spec.len() >= pre.len() + post.len()
                        && spec.starts_with(pre)
                        && spec.ends_with(post)
                }
                None => pattern == spec,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsx_defaults_to_the_classic_runtime() {
        let p = Project::from_options(&CompilerOptions::new(), Vec::new());
        assert_eq!(p.base.jsx, JsxMode::Classic);
        assert_eq!(p.base.jsx_factory, "React.createElement");
        assert!(p.base.target >= ScriptTarget::ES2022);
    }

    #[test]
    fn a_jsx_factory_without_jsx_means_the_classic_runtime() {
        let mut o = CompilerOptions::new();
        let value = ts_r_config::OptionValue::String("Array".into());
        o.set(ts_r_config::OptionId::JsxFactory, Some(value), None);
        assert_eq!(
            Project::from_options(&o, Vec::new()).base.jsx,
            JsxMode::Classic
        );
    }

    #[test]
    fn bare_specifiers_are_left_to_node() {
        let p = Project::from_options(&CompilerOptions::new(), Vec::new());
        assert_eq!(p.resolve("react", "/x/a.ts", false), None);
        assert_eq!(p.resolve("node:fs", "/x/a.ts", false), None);
        assert_eq!(p.resolve("#internal", "/x/a.ts", true), None);
    }

    #[test]
    fn a_missing_extends_stops_the_run() {
        let dir = std::env::temp_dir().join(format!("ts-r-run-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("tsconfig.json");
        std::fs::write(&config, "{ \"extends\": \"doesnt-exist\" }").unwrap();
        let entry = dir.join("index.ts");
        std::fs::write(&entry, "").unwrap();
        let entry = absolute(&entry.to_string_lossy());
        let p = Project::load(&entry, Some(&config.to_string_lossy()));
        assert!(
            p.config_errors.contains("error TS6053"),
            "{}",
            p.config_errors
        );
        // A detected tsconfig.json with the same error is ignored.
        assert_eq!(Project::load(&entry, None).config_errors, "");
        // Option diagnostics don't stop a run.
        std::fs::write(&config, "{ \"compilerOptions\": { \"nope\": 1 } }").unwrap();
        let p = Project::load(&entry, Some(&config.to_string_lossy()));
        assert_eq!(p.config_errors, "");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
