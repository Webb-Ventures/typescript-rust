//! [`ts_r_hmr::Transform`] for TypeScript, TSX and JSX, so `ts-r-dev` serves `.ts`/`.tsx`/
//! `.mts`/`.cts`/`.jsx` (feature `hmr`).
//!
//! Output is always an ES module (the dev server is unbundled ESM). `import.meta.hot` and
//! `import.meta.env` pass through untouched. Imports come from the printer's specifier spans,
//! so the server can rewrite them without rescanning.

use std::path::Path;

use ts_r_ast::SourceKind;
use ts_r_codegen::{CodegenOptions, SourceMapOptions, SpecifierKind};
use ts_r_config::{FileSystem, RealFs, ScriptTarget};
use ts_r_hmr::scan::{ImportKind, ImportRecord};
use ts_r_hmr::transform::{Mode, Transform, TransformError, TransformInput, TransformOutput};
use ts_r_span::SourceText;

use crate::{JsxMode, ModuleFormat, TransformOptions, transpile};

/// The TypeScript/JSX transform for the dev server.
#[derive(Debug, Clone)]
pub struct TsTransform {
    options: TransformOptions,
    cache_id: String,
}

impl TsTransform {
    /// Uses `options` for every file (the module format is forced to ESM).
    pub fn new(mut options: TransformOptions) -> TsTransform {
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
        let cache_id = format!(
            "ts-r-transform/{}+{build}/{:016x}",
            env!("CARGO_PKG_VERSION"),
            fx_hash(&options.cache_key())
        );
        TsTransform { options, cache_id }
    }

    /// Options from `root/tsconfig.json` when present, else tsc's defaults.
    pub fn for_root(root: &Path) -> TsTransform {
        let fs = RealFs::new();
        let config = root.join("tsconfig.json");
        let config = config.to_string_lossy();
        let options = if fs.file_exists(&config) {
            let parsed = ts_r_config::load_tsconfig(&fs, &config);
            TransformOptions::from_compiler_options(&parsed.options, "", false)
        } else {
            TransformOptions::default()
        };
        TsTransform::new(options)
    }

    pub fn options(&self) -> &TransformOptions {
        &self.options
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

    fn transform(&self, input: &TransformInput<'_>) -> Result<TransformOutput, TransformError> {
        let path = input.path.to_string_lossy();
        let mut opts = self.options.clone();
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
