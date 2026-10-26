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
