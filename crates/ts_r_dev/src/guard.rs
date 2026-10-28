//! `parse<T>` call sites in the dev server: a [`Transform`] wrapper that expands
//! `@ts-r.dev/guard` calls (record 0007) before the TypeScript transform, with the validators
//! inlined per file, exactly as `ts-r-run` does. Errors (TSR1501, ...) go to the overlay.
//!
//! The files a module's validators were generated from (its program's other sources) are
//! reported as [`Transform::deps`], so the server keys the cached output on them too and
//! re-transforms and hot-updates the module when only a type changes.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ts_r_guard::{ExpandOptions, Expanded, Expander, GuardOptions, Layout};
use ts_r_hmr::{Transform, TransformError, TransformInput, TransformOutput};

/// What the last expansion of a file found.
#[derive(Debug)]
struct Memo {
    /// Hash of the source it was expanded from.
    source: u64,
    deps: Vec<PathBuf>,
    /// Hash of the deps' contents when they were computed: a dep that changes can change
    /// the dependency list itself (a type file that starts importing another one).
    contents: u64,
    /// The expansion `deps` just computed, for the `transform` call that follows a cache
    /// miss (taken once; never reused after the dependencies could have changed).
    fresh: Option<Expanded>,
}

#[derive(Debug)]
pub struct GuardTransform {
    inner: Arc<dyn Transform>,
    expander: Expander,
    cache_id: String,
    memo: Mutex<HashMap<PathBuf, Memo>>,
}

fn source_hash(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn contents_hash(deps: &[PathBuf]) -> u64 {
    let mut h = DefaultHasher::new();
    for d in deps {
        std::fs::read(d).ok().hash(&mut h);
    }
    h.finish()
}

fn file_of(input: &TransformInput<'_>) -> String {
    ts_r_config::path::normalize_slashes(&input.path.to_string_lossy())
}

/// `[sound.guard]` for the project at `root` (its tsconfig's `"ts-r"` key, else a
/// `tsrconfig.json` or `ts-r.toml` above it), read once at startup; problems are printed
/// (TSR0020).
pub fn options_for_root(root: &Path) -> GuardOptions {
    let fs = ts_r_config::RealFs::new();
    let root = ts_r_config::path::normalize_slashes(&root.to_string_lossy());
    let root = ts_r_config::path::resolve(&ts_r_config::FileSystem::cwd(&fs), &root);
    let tsconfig = ts_r_config::find_config_file(&fs, &root, "tsconfig.json")
        .unwrap_or_else(|| ts_r_config::path::combine(&root, "tsconfig.json"));
    let settings = ts_r_sound::config::guard_options(&fs, &tsconfig);
    for e in &settings.errors {
        eprintln!("{}", e.render());
    }
    settings.options
}

impl GuardTransform {
    pub fn new(inner: Arc<dyn Transform>, guard: GuardOptions) -> GuardTransform {
        // The options change the output, so they are part of the cache id (refinements too:
        // `/<name>=<module>#<export>` each, in order).
        let mut cache_id = format!(
            "{}+ts-r-guard/1/{:?}/{}/{}",
            inner.cache_id(),
            guard.explain,
            guard.max_depth,
            guard.max_validator_kb
        );
        for r in &guard.refinements {
            cache_id.push_str(&format!("/{}={}#{}", r.name, r.module, r.export));
        }
        GuardTransform {
            inner,
            expander: Expander::new(
                Arc::new(ts_r_config::RealFs::new()),
                ExpandOptions {
                    guard,
                    layout: Layout::Inline,
                },
            ),
            cache_id,
            memo: Mutex::new(HashMap::new()),
        }
    }

    /// The files a module's validators were generated from (memoized per source).
    fn guard_deps(&self, input: &TransformInput<'_>) -> Vec<PathBuf> {
        if !ts_r_guard::expand::mentions_guard(input.source) {
            return Vec::new();
        }
        let hash = source_hash(input.source);
        let mut memo = self.memo.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = memo.get_mut(input.path)
            && m.source == hash
            && contents_hash(&m.deps) == m.contents
        {
            // The deps' contents are checked by the server's cache key; an expansion made
            // before they changed must not be reused.
            m.fresh = None;
            return m.deps.clone();
        }
        drop(memo);
        let x = self.expand(input);
        let deps: Vec<PathBuf> = x
            .as_ref()
            .map(|x| x.deps.iter().map(PathBuf::from).collect())
            .unwrap_or_default();
        self.memo.lock().unwrap_or_else(|e| e.into_inner()).insert(
            input.path.to_path_buf(),
            Memo {
                source: hash,
                contents: contents_hash(&deps),
                deps: deps.clone(),
                fresh: x,
            },
        );
        deps
    }

    fn expand(&self, input: &TransformInput<'_>) -> Option<Expanded> {
        self.expander.expand(&file_of(input), input.source)
    }
}

impl Transform for GuardTransform {
    fn cache_id(&self) -> &str {
        &self.cache_id
    }

    fn handles(&self, path: &Path) -> bool {
        self.inner.handles(path)
    }

    fn deps(&self, input: &TransformInput<'_>) -> Vec<PathBuf> {
        // The wrapped transform's own deps (the file's tsconfig chain) come first.
        let mut deps = self.inner.deps(input);
        for d in self.guard_deps(input) {
            if !deps.contains(&d) {
                deps.push(d);
            }
        }
        deps
    }

    fn transform(&self, input: &TransformInput<'_>) -> Result<TransformOutput, TransformError> {
        let fresh = self
            .memo
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(input.path)
            .filter(|m| m.source == source_hash(input.source))
            .and_then(|m| m.fresh.take());
        let Some(x) = fresh.or_else(|| self.expand(input)) else {
            return self.inner.transform(input);
        };
        if let Some(e) = x.errors.first() {
            let mut message = format!("{}: {}", e.code_label(), e.message);
            if x.errors.len() > 1 {
                message.push_str(&format!(" (+{} more)", x.errors.len() - 1));
            }
            return Err(TransformError {
                message,
                line: Some(e.line),
                column: Some(e.column),
            });
        }
        self.inner.transform(&TransformInput {
            path: input.path,
            id: input.id,
            source: &x.code,
            mode: input.mode,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_r_hmr::Mode;

    fn fixture() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../ts_r_guard/tests/fixtures/app")
    }

    #[test]
    fn expands_like_ts_r_run_and_reports_errors() {
        let root = fixture();
        let ts: Arc<dyn Transform> = Arc::new(ts_r_transform::hmr::TsTransform::for_root(&root));
        let t = GuardTransform::new(ts.clone(), GuardOptions::default());
        assert!(t.cache_id().ends_with("+ts-r-guard/1/Full/512/8"));
        let path = root.join("report.ts");
        let src = std::fs::read_to_string(&path).expect("fixture");
        let out = t
            .transform(&TransformInput {
                path: &path,
                id: "report.ts",
                source: &src,
                mode: Mode::Dev,
            })
            .expect("transform");
        assert!(!out.code.contains("@ts-r.dev/guard"), "{}", out.code);
        assert!(
            out.code.contains("const ParseError = $ParseError"),
            "{}",
            out.code
        );
        // Files without guard imports are passed through untouched.
        let plain = "export const x: number = 1;\n";
        let a = t
            .transform(&TransformInput {
                path: &root.join("plain.ts"),
                id: "plain.ts",
                source: plain,
                mode: Mode::Dev,
            })
            .expect("plain");
        let b = ts
            .transform(&TransformInput {
                path: &root.join("plain.ts"),
                id: "plain.ts",
                source: plain,
                mode: Mode::Dev,
            })
            .expect("plain");
        assert_eq!(a, b);
        // An unsupported type is an overlay error with its location.
        let bad = "import { parse } from \"@ts-r.dev/guard\";\nexport const f = (x: unknown) => parse<() => void>(x);\n";
        let e = t
            .transform(&TransformInput {
                path: &root.join("bad.ts"),
                id: "bad.ts",
                source: bad,
                mode: Mode::Dev,
            })
            .expect_err("unsupported");
        assert!(
            e.message
                .starts_with("TSR1501: Type '() => void' cannot be checked at runtime"),
            "{}",
            e.message
        );
        assert_eq!((e.line, e.column), (Some(2), Some(40)));
    }

    #[test]
    fn reports_the_files_validators_are_generated_from() {
        let root = fixture();
        let ts: Arc<dyn Transform> = Arc::new(ts_r_transform::hmr::TsTransform::for_root(&root));
        // The fixture's tsrconfig.json registers the refinements main.ts validates (`Email`,
        // `Port`, `Positive`).
        let opts = options_for_root(&root);
        assert_eq!(opts.refinements.len(), 3, "{opts:?}");
        let t = GuardTransform::new(ts.clone(), opts);
        let path = root.join("main.ts");
        let src = std::fs::read_to_string(&path).expect("fixture");
        let input = TransformInput {
            path: &path,
            id: "main.ts",
            source: &src,
            mode: Mode::Dev,
        };
        let deps = t.deps(&input);
        assert!(deps.iter().any(|d| d.ends_with("types.ts")), "{deps:?}");
        // A second call with the same source is answered from the memo.
        assert_eq!(t.deps(&input), deps);
        let out = t.transform(&input).expect("transform");
        assert!(out.code.contains("$ParseError"), "{}", out.code);
        // The TS transform's own deps (the tsconfig.json the options come from) come
        // first; files without guard imports have only those.
        let tsconfig = ts.deps(&input);
        assert!(
            tsconfig.iter().any(|d| d.ends_with("tsconfig.json")),
            "{tsconfig:?}"
        );
        assert_eq!(deps[..tsconfig.len()], tsconfig[..]);
        let plain = TransformInput {
            path: &root.join("plain.ts"),
            id: "plain.ts",
            source: "export const x = 1;\n",
            mode: Mode::Dev,
        };
        assert_eq!(t.deps(&plain), ts.deps(&plain));
    }

    #[test]
    fn deps_follow_a_type_file_that_starts_importing_another() {
        let root = std::env::temp_dir().join(format!("ts-r-dev-guard-deps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let guard = root.join("node_modules/@ts-r.dev/guard");
        std::fs::create_dir_all(&guard).expect("temp dir");
        let from = fixture().join("node_modules/@ts-r.dev/guard");
        for f in ["index.d.ts", "index.js", "package.json"] {
            std::fs::copy(from.join(f), guard.join(f)).expect("copy guard package");
        }
        std::fs::copy(fixture().join("tsconfig.json"), root.join("tsconfig.json")).expect("copy");
        let main = "import { parse } from \"@ts-r.dev/guard\";\nimport type { T } from \"./types\";\nexport const f = (x: unknown) => parse<T>(x);\n";
        std::fs::write(root.join("main.ts"), main).expect("write");
        std::fs::write(root.join("types.ts"), "export type T = { a: number };\n").expect("write");
        std::fs::write(root.join("nested.ts"), "export type N = string;\n").expect("write");
        let ts: Arc<dyn Transform> = Arc::new(ts_r_transform::hmr::TsTransform::for_root(&root));
        let t = GuardTransform::new(ts, GuardOptions::default());
        let path = root.join("main.ts");
        let input = TransformInput {
            path: &path,
            id: "main.ts",
            source: main,
            mode: Mode::Dev,
        };
        let names = |deps: Vec<PathBuf>| -> Vec<String> {
            let mut n: Vec<String> = deps
                .iter()
                .filter_map(|d| d.file_name())
                .map(|f| f.to_string_lossy().into_owned())
                .collect();
            n.sort();
            n
        };
        assert!(names(t.deps(&input)).contains(&"types.ts".to_owned()));
        // main.ts is unchanged, but its type now comes partly from nested.ts.
        std::fs::write(
            root.join("types.ts"),
            "import type { N } from \"./nested\";\nexport type T = { a: N };\n",
        )
        .expect("write");
        let deps = names(t.deps(&input));
        assert!(deps.contains(&"nested.ts".to_owned()), "{deps:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
