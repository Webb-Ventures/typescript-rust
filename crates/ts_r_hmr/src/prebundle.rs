//! The dependency pre-bundling seam (decision D3: unbundled ESM for source, pre-bundled
//! `node_modules`).
//!
//! A [`DepBundler`] produces the ESM the browser gets for bare imports. There are two ways:
//!
//! - **Batch** ([`DepBundler::bundle_batch`], the normal path with the M3 bundler): the
//!   server scans the app from `index.html` for every dependency entry (`deps.rs`, like
//!   Vite's optimizer), and the bundler builds them all at once with code splitting, so a
//!   file shared by two entries (React used by `react-dom`, a package's internal module
//!   reached from two of its entry points) exists once. Files are served at
//!   `/@ts-r/deps/<name>?v=<hash>`.
//! - **One at a time** ([`DepBundler::bundle`]): `/@modules/<specifier>?v=<hash>`, used by
//!   bundlers without batch support ([`UnbundledDeps`], which serves a package's own ESM
//!   files and rejects CommonJS with a clear message) and for dependency files reached
//!   through relative paths.

use std::fmt;
use std::path::Path;

use crate::scan;
use crate::transform::{TransformError, TransformOutput};

/// One dependency module to produce.
#[derive(Clone, Copy, Debug)]
pub struct DepRequest<'a> {
    /// Bare specifier as imported, e.g. `react`, `react-dom/client`, or a file inside a
    /// package (`pkg/dist/chunk.js`) when an unbundled dep imports a sibling file.
    pub specifier: &'a str,
    /// Package name (`@scope/name` or `name`).
    pub package: &'a str,
    /// The file the specifier resolved to.
    pub file: &'a Path,
}

/// The output of [`DepBundler::bundle_batch`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DepBatch {
    /// Parallel to the requests: the name (in `files`) of the module each one is served as.
    /// Requests that resolve to the same file share a name.
    pub entries: Vec<String>,
    /// Every output module (entries and shared chunks) by file name (`[A-Za-z0-9_.-]`, ends
    /// in `.js`). Imports between them are relative (`./chunk-X.js`); any other import is
    /// rewritten by the server like a source module's. A module's `map` is its source map
    /// (`sources` relative to the served URL `/@ts-r/deps/`, e.g. `../../node_modules/x/y.js`;
    /// no absolute paths, since batches are cached). A `<stem>.css` file is the CSS that
    /// `<stem>.js` imports (code = the stylesheet, no imports); the server loads it with it.
    pub files: Vec<(String, TransformOutput)>,
}

/// Produces the ESM served for a dependency. The server rewrites the returned module's
/// imports exactly like source modules (bare → `/@modules/`, relative files inside
/// `node_modules` → `/@modules/<package>/<path>`), so a bundler may emit either
/// self-contained modules or modules that import shared chunks.
pub trait DepBundler: Send + Sync + fmt::Debug {
    /// Identity + version; part of the `?v=` hash, so changing it busts browser caches.
    fn id(&self) -> &str;

    fn bundle(&self, dep: &DepRequest<'_>, source: &str)
    -> Result<TransformOutput, TransformError>;

    /// Bundles every dependency entry of the app together with code splitting. `None` means
    /// batches aren't supported and the server falls back to [`DepBundler::bundle`] per
    /// specifier. Must be deterministic: the result is cached on disk keyed by the lockfile,
    /// the bundler's [`id`](DepBundler::id) and the requests.
    fn bundle_batch(&self, deps: &[DepRequest<'_>]) -> Option<Result<DepBatch, TransformError>> {
        let _ = deps;
        None
    }
}

/// The pre-M3 stand-in: ESM files are served as they are.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnbundledDeps;

impl DepBundler for UnbundledDeps {
    fn id(&self) -> &str {
        "unbundled/1"
    }

    fn bundle(
        &self,
        dep: &DepRequest<'_>,
        source: &str,
    ) -> Result<TransformOutput, TransformError> {
        let found = scan::scan(source);
        if !found.has_module_syntax && looks_commonjs(source) {
            return Err(TransformError::new(format!(
                "\"{}\" is CommonJS ({}). ts-r-dev serves dependencies as native ES modules \
                 until dependency pre-bundling ships with the M3 bundler; use an ESM build of \
                 this package for now.",
                dep.specifier,
                dep.file.display()
            )));
        }
        Ok(TransformOutput {
            code: source.to_owned(),
            map: None,
            imports: found.imports,
        })
    }
}

fn looks_commonjs(source: &str) -> bool {
    source.contains("module.exports")
        || source.contains("exports.")
        || source.contains("require(")
        || source.contains("exports[")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esm_passes_and_cjs_is_explained() {
        let dep = DepRequest {
            specifier: "x",
            package: "x",
            file: Path::new("/n/x/index.js"),
        };
        let ok = UnbundledDeps
            .bundle(&dep, "import y from 'y'; export default y;")
            .unwrap();
        assert_eq!(ok.imports.len(), 1);
        let e = UnbundledDeps
            .bundle(&dep, "module.exports = require('y');")
            .unwrap_err();
        assert!(e.message.contains("CommonJS"), "{e}");
        // No module syntax but no CJS markers either (a plain script): served as is.
        assert!(UnbundledDeps.bundle(&dep, "window.x = 1;").is_ok());
    }
}
