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
    pub resolver: Resolver<RealFs>,
    /// The tsconfig's compiler options (module resolution for `parse<T>` validators).
    pub options: CompilerOptions,
}

impl std::fmt::Debug for Project {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Project")
            .field("config_files", &self.config_files)
            .finish_non_exhaustive()
    }
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
        let (options, config_files) = match &config {
            Some(c) => {
                let parsed = load_tsconfig(&fs, c);
                let mut files = vec![parsed.config_path.clone()];
                files.extend(parsed.extended_config_paths.iter().cloned());
                (parsed.options, files)
            }
            None => (CompilerOptions::new(), Vec::new()),
        };
        Project::from_options(&options, config_files)
    }

    pub fn from_options(options: &CompilerOptions, config_files: Vec<String>) -> Project {
        let mut base = TransformOptions::from_compiler_options(options, "", false);
        // ts-r emits ES2022+; Node 24 runs all of it. Class-field semantics stay as the
        // tsconfig's target implies (`useDefineForClassFields` was computed above).
        if base.target < ScriptTarget::ES2022 {
            base.target = ScriptTarget::ES2022;
        }
        // Node can't run JSX, and the dev runtime needs absolute file names in the output.
        if base.jsx != JsxMode::Classic {
            base.jsx = JsxMode::Automatic;
        }
        base.jsx_file_name = String::new();
        base.jsx_preserve_explicit = false;
        // Specifiers are resolved by ts-r-run; tslib may not be installed.
        base.rewrite_relative_import_extensions = false;
        base.import_helpers = false;
        let mut ropts = ResolverOptions::from_compiler_options(options);
        // Like tsx: extensionless and directory imports work in ES modules too.
        ropts.algorithm = ModuleResolution::Bundler;
        Project {
            config_files,
            base,
            resolver: Resolver::new(RealFs::new(), ropts),
            options: options.clone(),
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

    /// Resolves `spec` imported from `parent` when Node's own resolution would not find the
    /// same file: relative and absolute specifiers (extension substitution, extensionless
    /// and directory imports) and tsconfig `paths`. Bare package specifiers are left to Node,
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
        if !relative && !self.matches_paths(spec) {
            return None;
        }
        let mode = if require {
            ResolutionMode::Require
        } else {
            self.resolver.mode_for(parent, ImportKind::Static)
        };
        let r = self.resolver.resolve_module(spec, parent, mode)?;
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
    fn jsx_defaults_to_the_automatic_runtime() {
        let p = Project::from_options(&CompilerOptions::new(), Vec::new());
        assert_eq!(p.base.jsx, JsxMode::Automatic);
        assert!(p.base.target >= ScriptTarget::ES2022);
    }

    #[test]
    fn bare_specifiers_are_left_to_node() {
        let p = Project::from_options(&CompilerOptions::new(), Vec::new());
        assert_eq!(p.resolve("react", "/x/a.ts", false), None);
        assert_eq!(p.resolve("node:fs", "/x/a.ts", false), None);
        assert_eq!(p.resolve("#internal", "/x/a.ts", true), None);
    }
}
