//! The `Transform` seam: source file → browser-ready JS module.
//!
//! The dev server never looks inside a transform. It hands over a path, the source text and
//! a [`Mode`], and gets back JS code, an optional source map and the module's import
//! specifiers with their byte spans in the output code (which the server rewrites to URLs).
//! The TS/TSX/JSX transform plugs in here later by implementing [`Transform`]; until then
//! [`PassthroughJs`] serves `.js`/`.mjs` files unchanged.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::scan::{self, ImportRecord};

/// What the output is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Dev server: keep `import.meta.hot`, no minification, inline-friendly source maps.
    Dev,
    /// Production build (used by ts-r-build later).
    Build,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Dev => "dev",
            Mode::Build => "build",
        }
    }
}

/// Input to [`Transform::transform`].
#[derive(Clone, Copy, Debug)]
pub struct TransformInput<'a> {
    /// Absolute path of the source file (for diagnostics, JSX/TS dialect, source map `sources`).
    pub path: &'a Path,
    /// Path relative to the project root with `/` separators (what appears in source maps).
    pub id: &'a str,
    pub source: &'a str,
    pub mode: Mode,
}

/// Output of [`Transform::transform`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransformOutput {
    /// ES module JS code.
    pub code: String,
    /// Source map v3 JSON, if the code differs from the source.
    pub map: Option<String>,
    /// Every import specifier in `code`, with its span *in `code`*, in source order.
    pub imports: Vec<ImportRecord>,
}

/// A transform failure, shown in the error overlay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransformError {
    pub message: String,
    /// 1-based line and column in the source, if known.
    pub line: Option<u32>,
    pub column: Option<u32>,
}

impl TransformError {
    pub fn new(message: impl Into<String>) -> TransformError {
        TransformError {
            message: message.into(),
            line: None,
            column: None,
        }
    }

    pub fn at(message: impl Into<String>, line: u32, column: u32) -> TransformError {
        TransformError {
            message: message.into(),
            line: Some(line),
            column: Some(column),
        }
    }
}

impl fmt::Display for TransformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.line, self.column) {
            (Some(l), Some(c)) => write!(f, "{} ({l}:{c})", self.message),
            _ => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for TransformError {}

/// Turns one source file into a JS module. Implementations must be deterministic: the same
/// `(cache_id, path id, source, mode)` must produce the same output, because outputs are
/// cached by content hash (in memory and in `ts_r_cache` across restarts).
pub trait Transform: Send + Sync + fmt::Debug {
    /// Identifies the transform *and its version/options* in cache keys. Change it whenever
    /// output for the same input could change.
    fn cache_id(&self) -> &str;

    /// Whether this transform handles `path` (usually by extension).
    fn handles(&self, path: &Path) -> bool;

    fn transform(&self, input: &TransformInput<'_>) -> Result<TransformOutput, TransformError>;

    /// Files besides the source whose contents the output for `input` depends on (for
    /// example the declarations `parse<T>` validators are generated from; type-only imports
    /// never reach the module graph). Called before the cache lookup: their contents become
    /// part of the cache key, and a change to one re-prepares the module and hot-updates it
    /// as if its own source had changed. Must be cheap when nothing changed. Default: none.
    fn deps(&self, input: &TransformInput<'_>) -> Vec<PathBuf> {
        let _ = input;
        Vec::new()
    }
}

/// Serves `.js` and `.mjs` files unchanged; imports come from [`scan::scan`].
#[derive(Clone, Copy, Debug, Default)]
pub struct PassthroughJs;

impl Transform for PassthroughJs {
    fn cache_id(&self) -> &str {
        "passthrough-js/1"
    }

    fn handles(&self, path: &Path) -> bool {
        matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("js" | "mjs")
        )
    }

    fn transform(&self, input: &TransformInput<'_>) -> Result<TransformOutput, TransformError> {
        let scan = scan::scan(input.source);
        Ok(TransformOutput {
            code: input.source.to_owned(),
            map: None,
            imports: scan.imports,
        })
    }
}

/// File extensions that are served as JS modules when a transform handles them.
pub fn is_script_extension(ext: &str) -> bool {
    matches!(
        ext,
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" | "mts" | "cts"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_keeps_code_and_finds_imports() {
        let t = PassthroughJs;
        assert!(t.handles(Path::new("a/b.js")) && t.handles(Path::new("x.mjs")));
        assert!(!t.handles(Path::new("a.ts")) && !t.handles(Path::new("a.css")));
        let src = "import a from './a.js';\nexport { b } from 'b';\n";
        let out = t
            .transform(&TransformInput {
                path: Path::new("/p/x.js"),
                id: "x.js",
                source: src,
                mode: Mode::Dev,
            })
            .unwrap();
        assert_eq!(out.code, src);
        assert_eq!(out.map, None);
        assert_eq!(out.imports.len(), 2);
        assert_eq!(out.imports[1].specifier, "b");
    }
}
