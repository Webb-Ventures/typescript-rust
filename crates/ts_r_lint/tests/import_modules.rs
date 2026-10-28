//! Regression tests for the eslint-plugin-import rules that read other modules, on a
//! memory file system: long and cyclic module graphs, self-referential packages and
//! re-exports, malformed `package.json` files and non-ASCII paths. None may hang, overflow
//! the stack or panic.

use std::path::PathBuf;
use std::sync::Arc;

use ts_r_config::MemoryFs;
use ts_r_lint::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};
use ts_r_lint::modules::ModuleHost;
use ts_r_lint::types::{FileKey, NoTypeInfo};
use ts_r_lint::{Registry, lint_source};

ts_r_testing::cap_memory!();

fn host(files: &[(&str, String)]) -> Arc<ModuleHost> {
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_dir("/p");
    for (path, text) in files {
        fs.add_file(path, text);
    }
    ModuleHost::with_fs(Arc::new(fs))
}

/// The messages of `rule` on `/p/<file>` (read from `host`).
fn messages(
    host: &Arc<ModuleHost>,
    rule: &str,
    options: serde_json::Value,
    file: &str,
    code: &str,
) -> Vec<String> {
    let mut entry = vec![serde_json::json!("error")];
    if let serde_json::Value::Array(o) = options {
        entry.extend(o);
    }
    let config = serde_json::json!([{
        "files": ["**/*.{js,mjs,cjs,jsx,ts,tsx}"],
        "rules": { rule: entry },
    }]);
    let objs = objects_from_eslint_json(&config, Registry::global()).expect("config");
    let cfg = ConfigFile::new(ConfigKind::Eslint, None, PathBuf::from("/p"), objs);
    let mut opts = ResolveOptions::new();
    opts.cwd = Some("/p".into());
    opts.modules = Some(host.clone());
    let fc = cfg.file_config(file, &opts).expect("file config");
    let r = lint_source(&format!("/p/{file}"), code, &fc, &NoTypeInfo, FileKey(0));
    assert!(!r.diagnostics.iter().any(|d| d.fatal), "fatal in {code:?}");
    r.diagnostics.iter().map(|d| d.message.clone()).collect()
}

/// `/p/m0.js` .. `/p/m{n-1}.js`, each importing the next, the last importing `m0`.
fn ring(n: usize) -> Vec<(&'static str, String)> {
    (0..n)
        .map(|i| {
            let path: &'static str = Box::leak(format!("/p/m{i}.js").into_boxed_str());
            let next = (i + 1) % n;
            (
                path,
                format!("import x from './m{next}';\nexport default x;\n"),
            )
        })
        .collect()
}

#[test]
fn no_cycle_walks_a_long_ring_without_recursion() {
    let h = host(&ring(3000));
    let code = "import x from './m1';\nexport default x;\n";
    let m = messages(&h, "import/no-cycle", serde_json::json!([]), "m0.js", code);
    assert_eq!(m.len(), 1, "{m:?}");
    assert!(
        m[0].starts_with("Dependency cycle via ./m2:1=>"),
        "{}",
        &m[0][..60]
    );
    // The same ring with the search cut short finds nothing.
    let m = messages(
        &h,
        "import/no-cycle",
        serde_json::json!([{ "maxDepth": 2 }]),
        "m0.js",
        code,
    );
    assert!(m.is_empty(), "{m:?}");
    // Without the component pass the search still ends.
    let m = messages(
        &h,
        "import/no-cycle",
        serde_json::json!([{ "disableScc": true }]),
        "m0.js",
        code,
    );
    assert_eq!(m.len(), 1);
}

#[test]
fn no_cycle_skips_type_only_and_side_effect_imports() {
    let h = host(&[
        (
            "/p/a.ts",
            "import { b } from './b';\nexport type A = 1;\n".into(),
        ),
        (
            "/p/b.ts",
            "import type { A } from './a';\nexport const b = 1;\n".into(),
        ),
    ]);
    let m = messages(
        &h,
        "import/no-cycle",
        serde_json::json!([]),
        "a.ts",
        "import { b } from './b';\nimport './b';\nexport type A = 1;\n",
    );
    assert!(m.is_empty(), "{m:?}");
}

#[test]
fn self_referential_packages_and_reexports_end() {
    let h = host(&[
        // `main` pointing back at the package directory, and a chain of directories.
        (
            "/p/node_modules/loop/package.json",
            r#"{"main": "./sub"}"#.into(),
        ),
        (
            "/p/node_modules/loop/sub/package.json",
            r#"{"main": ".."}"#.into(),
        ),
        ("/p/node_modules/bad/package.json", "{ not json".into()),
        (
            "/p/node_modules/bad/index.js",
            "module.exports = 1;\n".into(),
        ),
        (
            "/p/node_modules/bom/package.json",
            "\u{feff}{\"main\": \"lib.js\"}".into(),
        ),
        ("/p/node_modules/bom/lib.js", "export const x = 1;\n".into()),
        (
            "/p/self.js",
            "export * from './self';\nexport * from './other';\n".into(),
        ),
        (
            "/p/other.js",
            "export * from './self';\nexport const o = 1;\n".into(),
        ),
    ]);
    let code = "import a from 'loop';\nimport b from 'bad';\nimport { x } from 'bom';\n\
                import { o, nope } from './self';\n";
    let unresolved = messages(
        &h,
        "import/no-unresolved",
        serde_json::json!([]),
        "a.js",
        code,
    );
    // `resolve` parses `package.json` without stripping a BOM (then there is no `main`).
    assert_eq!(
        unresolved,
        [
            "Unable to resolve path to module 'loop'.",
            "Unable to resolve path to module 'bom'."
        ]
    );
    let named = messages(&h, "import/named", serde_json::json!([]), "a.js", code);
    assert_eq!(named, ["nope not found in './self'"]);
}

#[test]
fn deep_reexport_chain_is_bounded() {
    let mut files: Vec<(&str, String)> = (0..2000)
        .map(|i| {
            let path: &'static str = Box::leak(format!("/p/r{i}.js").into_boxed_str());
            (path, format!("export * from './r{}';\n", i + 1))
        })
        .collect();
    files.push(("/p/r2000.js", "export const deep = 1;\n".into()));
    let h = host(&files);
    for rule in [
        "import/named",
        "import/namespace",
        "import/export",
        "import/default",
    ] {
        let _ = messages(
            &h,
            rule,
            serde_json::json!([]),
            "a.js",
            "import * as ns from './r0';\nimport { deep } from './r0';\nexport * from './r0';\n\
             ns.deep;\n",
        );
    }
}

#[test]
fn non_ascii_paths() {
    let h = host(&[
        ("/p/ünï/çödé.js", "export const ü = 1;\n".into()),
        ("/p/ünï/index.js", "export const i = 1;\n".into()),
        ("/p/ünï/çödé/ü.js", "export default 1;\n".into()),
    ]);
    let m = messages(
        &h,
        "import/no-useless-path-segments",
        serde_json::json!([{ "noUselessIndex": true }]),
        "a.js",
        "import { ü } from './ünï/../ünï/çödé.js';\nimport { i } from './ünï/index.js';\n",
    );
    assert_eq!(
        m,
        [
            "Useless path segments for \"./ünï/../ünï/çödé.js\", should be \"./ünï/çödé.js\"",
            "Useless path segments for \"./ünï/index.js\", should be \"./ünï\"",
        ]
    );
    let m = messages(
        &h,
        "import/extensions",
        serde_json::json!(["always"]),
        "a.js",
        "import { ü } from './ünï/çödé';\n",
    );
    assert_eq!(m, ["Missing file extension \"js\" for \"./ünï/çödé\""]);
    let m = messages(
        &h,
        "import/no-internal-modules",
        serde_json::json!([{ "allow": ["**/ünï/*"] }]),
        "a.js",
        "import { ü } from './ünï/çödé';\nimport x from './ünï/çödé/ü';\n",
    );
    assert_eq!(m, ["Reaching to \"./ünï/çödé/ü\" is not allowed."], "{m:?}");
}

#[test]
fn restricted_paths_with_globs_and_bad_exceptions() {
    let h = host(&[
        ("/p/server/a.js", "export default 1;\n".into()),
        ("/p/client/b.js", "export default 1;\n".into()),
    ]);
    let code = "import a from '../server/a';\n";
    let zone = |from: serde_json::Value, except: serde_json::Value| serde_json::json!([{ "zones": [{ "target": "./client", "from": from, "except": except }] }]);
    let m = messages(
        &h,
        "import/no-restricted-paths",
        zone(serde_json::json!("./server/**/*"), serde_json::json!([])),
        "client/b.js",
        code,
    );
    assert_eq!(
        m,
        ["Unexpected path \"../server/a\" imported in restricted zone."]
    );
    let m = messages(
        &h,
        "import/no-restricted-paths",
        zone(
            serde_json::json!(["./server/**", "./other"]),
            serde_json::json!([]),
        ),
        "client/b.js",
        code,
    );
    assert_eq!(
        m,
        ["Restricted path `from` must contain either only glob patterns or none"]
    );
    let m = messages(
        &h,
        "import/no-restricted-paths",
        zone(
            serde_json::json!("./server"),
            serde_json::json!(["../client"]),
        ),
        "client/b.js",
        code,
    );
    assert_eq!(
        m,
        [
            "Restricted path exceptions must be descendants of the configured `from` path for that zone."
        ]
    );
}

#[test]
fn polyfill_targets_come_from_browserslist_or_engines() {
    let code = "import \"core-js/features/array/includes\";\nrequire(\"object-assign\");\n";
    let rule = "unicorn/no-unnecessary-polyfills";
    let both = ["Use built-in instead.", "Use built-in instead."];
    let h = host(&[("/p/package.json", r#"{"engines": {"node": ">=20"}}"#.into())]);
    assert_eq!(
        messages(&h, rule, serde_json::json!([]), "a.js", code),
        both
    );
    // A Browserslist config wins over `engines`; its `production` section first.
    let h = host(&[
        ("/p/package.json", r#"{"engines": {"node": ">=20"}}"#.into()),
        (
            "/p/sub/.browserslistrc",
            "# old\nnode 0.10\n[production]\nnode 20, chrome 120\n".into(),
        ),
    ]);
    assert_eq!(
        messages(&h, rule, serde_json::json!([]), "sub/a.js", code),
        both
    );
    let h = host(&[
        (
            "/p/package.json",
            r#"{"browserslist": ["node 0.10"]}"#.into(),
        ),
        ("/p/x/package.json", r#"{"name": "x"}"#.into()),
    ]);
    assert!(messages(&h, rule, serde_json::json!([]), "x/a.js", code).is_empty());
    // Two configs in one directory: no targets.
    let h = host(&[
        ("/p/package.json", r#"{"browserslist": "node 20"}"#.into()),
        ("/p/.browserslistrc", "node 20\n".into()),
    ]);
    assert!(messages(&h, rule, serde_json::json!([]), "a.js", code).is_empty());
    // Nothing found: no targets.
    let h = host(&[]);
    assert!(messages(&h, rule, serde_json::json!([]), "a.js", code).is_empty());
}

/// Every export-map rule on `code` from `a.js`.
fn all_export_rules(h: &Arc<ModuleHost>, code: &str) -> Vec<String> {
    let mut out = Vec::new();
    for rule in [
        "import/named",
        "import/namespace",
        "import/export",
        "import/default",
        "import/no-deprecated",
        "import/no-named-as-default",
        "import/no-named-as-default-member",
        "import/no-cycle",
    ] {
        out.extend(messages(h, rule, serde_json::json!([]), "a.js", code));
    }
    out
}

#[test]
fn export_star_cycles_end_quickly() {
    // Each of x, y and z re-exports the other two: without a visited set every lookup
    // branches twice per level until the depth limit (about 2^65 calls).
    let h = host(&[
        (
            "/p/x.js",
            "export * from './y';\nexport * from './z';\nexport const x = 1;\n".into(),
        ),
        (
            "/p/y.js",
            "export * from './x';\nexport * from './z';\n".into(),
        ),
        (
            "/p/z.js",
            "export * from './x';\nexport * from './y';\n".into(),
        ),
    ]);
    let start = std::time::Instant::now();
    let m = all_export_rules(
        &h,
        "import * as ns from './y';\nimport d, { x, nope } from './z';\nexport * from './x';\n\
         ns.x;\nns.missing;\nexport { d };\n",
    );
    assert!(start.elapsed().as_secs() < 5, "took {:?}", start.elapsed());
    assert!(m.contains(&"nope not found in './z'".to_owned()), "{m:?}");
    assert!(
        m.contains(&"'missing' not found in imported namespace 'ns'.".to_owned()),
        "{m:?}"
    );
}

#[test]
fn export_star_diamonds_are_walked_once() {
    // 30 layers of two modules, each re-exporting both modules of the next layer: 2^30
    // paths to the last layer without memoization.
    let mut files: Vec<(&str, String)> = Vec::new();
    for layer in 0..30 {
        for side in ["l", "r"] {
            let path: &'static str = Box::leak(format!("/p/d{layer}{side}.js").into_boxed_str());
            let next = layer + 1;
            files.push((
                path,
                format!("export * from './d{next}l';\nexport * from './d{next}r';\n"),
            ));
        }
    }
    files.push(("/p/d30l.js", "export const leaf = 1;\n".into()));
    files.push(("/p/d30r.js", "export const other = 1;\n".into()));
    let h = host(&files);
    let start = std::time::Instant::now();
    let m = all_export_rules(
        &h,
        "import * as ns from './d0l';\nimport { leaf, nope } from './d0l';\nexport * from './d0r';\n\
         ns.leaf;\nns.gone;\n",
    );
    assert!(start.elapsed().as_secs() < 5, "took {:?}", start.elapsed());
    assert!(m.contains(&"nope not found in './d0l'".to_owned()), "{m:?}");
}

/// A fresh directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let d = std::env::temp_dir().join(format!("ts-r-import-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        TempDir(d)
    }
    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.0.join(name), text).expect("write");
    }
    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().replace('\\', "/")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn editor_jobs_see_edits_to_imported_files() {
    // The editor keeps one `LintOptions` and one loaded config for the whole session and
    // lints a file again after another file changes.
    let dir = TempDir::new("editor");
    dir.write("b.js", "export const x = 1;\n");
    let opts = ts_r_lint::project::LintOptions::new(dir.0.clone());
    let config = serde_json::json!([{
        "files": ["**/*.js"],
        "rules": { "import/named": ["error"] },
    }]);
    let objs = objects_from_eslint_json(&config, Registry::global()).expect("config");
    let cfg = ConfigFile::new(ConfigKind::Eslint, None, dir.0.clone(), objs);
    let code = "import { x } from './b';\n";
    let lint = || {
        let fc = cfg.file_config("a.js", &opts.resolve).expect("file config");
        lint_source(&dir.path("a.js"), code, &fc, &NoTypeInfo, FileKey(0))
            .diagnostics
            .into_iter()
            .map(|d| d.message)
            .collect::<Vec<_>>()
    };
    assert!(lint().is_empty());
    dir.write("b.js", "export const y = 1;\n");
    assert_eq!(lint(), ["x not found in './b'"]);
}

#[test]
fn each_cli_run_reads_the_files_again() {
    let dir = TempDir::new("cli");
    dir.write(
        ".oxlintrc.json",
        r#"{ "plugins": ["import"], "rules": { "import/named": "error" } }"#,
    );
    dir.write("a.js", "import { x } from './b';\n");
    dir.write("b.js", "export const x = 1;\n");
    let mut opts = ts_r_lint::project::LintOptions::new(dir.0.clone());
    opts.threads = 1;
    let messages = |opts: &ts_r_lint::project::LintOptions| {
        let run = ts_r_lint::project::lint_paths(opts).expect("run");
        run.reports
            .iter()
            .flat_map(|r| r.result.diagnostics.iter())
            .filter(|d| d.rule == Some("import/named"))
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
    };
    let first = messages(&opts);
    assert!(first.is_empty(), "{first:?}");
    dir.write("b.js", "export const y = 1;\n");
    assert_eq!(messages(&opts), ["x not found in './b'"]);
}

#[test]
fn namespace_jsx_members_respect_shadowing() {
    let h = host(&[("/p/named.js", "export const a = 1;\n".into())]);
    let code = "import * as Names from './named';\n\
                export const A = () => <Names.a/>;\n\
                export const B = () => <Names.e/>;\n\
                export function F(Names) { return <Names.e/>; }\n\
                export function G() { const Names = {}; return <Names.e/>; }\n";
    let m = messages(&h, "import/namespace", serde_json::json!([]), "a.jsx", code);
    assert_eq!(m, ["'e' not found in imported namespace 'Names'."]);
}

#[test]
fn deprecated_reports_references_only() {
    let h = host(&[(
        "/p/dep.js",
        "/** @deprecated use g */\nexport function fn() {}\nexport const ok = 1;\n".into(),
    )]);
    let code = "import { fn } from './dep';\n\
                const obj = { fn: 1, [fn]: 2, fn };\n\
                obj.fn;\n\
                fn: for (;;) { break fn; }\n\
                fn();\n\
                export { obj };\n";
    let m = messages(
        &h,
        "import/no-deprecated",
        serde_json::json!([]),
        "a.js",
        code,
    );
    // The import itself, then `[fn]`, the shorthand `fn` and the call.
    assert_eq!(m.len(), 4, "{m:?}");
    assert!(m.iter().all(|s| s == "Deprecated: use g"), "{m:?}");
}
