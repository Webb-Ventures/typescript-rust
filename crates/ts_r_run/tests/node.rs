//! `ts-r-run` end to end on small projects, for the tsx behaviours the run oracle
//! (`bench/run-oracle`) found missing: resolution inside packages, plain JavaScript with ES
//! syntax in CommonJS scope, JSON without import attributes, tsconfig `paths` and `baseUrl`
//! from JavaScript parents, fatal `--tsconfig` errors, `--test` locations, the signal relay,
//! `import.meta` and the `__esModule` marker in CommonJS output, `?query` instances, syntax
//! errors behind `import()`, `require` of top-level `await`, the `import()` default unwrap,
//! JSON named exports, TypeScript inside packages, and `--test` without files. Each case writes its files to a scratch
//! directory and runs under `scripts/limit.sh`. Skipped when Node is missing.

use std::path::PathBuf;
use std::process::{Command, Output};

ts_r_testing::cap_memory!();

fn have_node() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The major version of the `node` on `PATH` (0 when it can't be read).
fn node_major() -> u32 {
    Command::new("node")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            let v = String::from_utf8_lossy(&o.stdout).into_owned();
            v.trim()
                .trim_start_matches('v')
                .split('.')
                .next()?
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

/// Writes `files` into a fresh scratch directory and runs `ts-r-run <args>` there.
fn run(name: &str, files: &[(&str, &str)], args: &[&str]) -> Option<Output> {
    if !have_node() {
        eprintln!("skipped: node not found");
        return None;
    }
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("run-node-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(&p, text).expect("write");
    }
    let out = ts_r_testing::limited(env!("CARGO_BIN_EXE_ts-r-run"), 1024, 120)
        .args(args)
        .current_dir(&dir)
        .env("TS_R_CACHE_DIR", dir.join(".cache"))
        .output()
        .expect("ts-r-run runs");
    let _ = std::fs::remove_dir_all(&dir);
    Some(out)
}

fn stdout(out: &Output) -> String {
    assert!(
        out.status.success(),
        "exit {:?}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn packages_resolve_typescript_extensionless_paths_and_nested_mains() {
    let files = [
        ("package.json", r#"{ "type": "module" }"#),
        (
            "main.ts",
            "import { a } from 'pkg/a.js';\nimport { a as b } from 'pkg/a';\nimport { c } from 'pkg/sub';\nimport { d } from 'pkg/dual';\nconsole.log(a, b, c, d);\n",
        ),
        (
            "node_modules/pkg/package.json",
            r#"{ "name": "pkg", "type": "module" }"#,
        ),
        ("node_modules/pkg/a.ts", "export const a: string = 'a';\n"),
        (
            "node_modules/pkg/sub/package.json",
            r#"{ "main": "./lib.ts" }"#,
        ),
        (
            "node_modules/pkg/sub/lib.ts",
            "export const c: string = 'c';\n",
        ),
        // Next to each other in node_modules, JavaScript wins.
        ("node_modules/pkg/dual.js", "export const d = 'js';\n"),
        (
            "node_modules/pkg/dual.ts",
            "export const d: string = 'ts';\n",
        ),
    ];
    let Some(out) = run("packages", &files, &["main.ts"]) else {
        return;
    };
    assert_eq!(stdout(&out), "a a c js\n");
}

#[test]
fn package_exports_only_get_the_typescript_swap() {
    let files = [
        ("package.json", r#"{ "type": "module" }"#),
        (
            "main.ts",
            "import { a } from 'pkg/a.js';\nconsole.log(a);\nawait import('pkg/a').catch((e) => console.log(e.code));\n",
        ),
        (
            "node_modules/pkg/package.json",
            r#"{ "name": "pkg", "type": "module", "exports": { "./*": "./*" } }"#,
        ),
        ("node_modules/pkg/a.ts", "export const a: string = 'a';\n"),
    ];
    let Some(out) = run("exports", &files, &["main.ts"]) else {
        return;
    };
    assert_eq!(stdout(&out), "a\nERR_MODULE_NOT_FOUND\n");
}

#[test]
fn es_syntax_in_commonjs_javascript_runs_as_commonjs() {
    let files = [
        ("package.json", r#"{ "type": "commonjs" }"#),
        (
            "a.js",
            "import { x } from './b.js';\nconsole.log('a', x, typeof module);\n",
        ),
        ("b.js", "export const x = 1;\n"),
    ];
    let Some(out) = run("lenient-js", &files, &["a.js"]) else {
        return;
    };
    assert_eq!(stdout(&out), "a 1 object\n");
}

#[test]
fn json_needs_no_import_attribute() {
    let files = [
        ("package.json", r#"{ "type": "module" }"#),
        (
            "main.ts",
            "import j from './json/';\nimport k from './k.json';\nconsole.log(j.a, k.b);\n",
        ),
        ("json/index.json", r#"{ "a": 1 }"#),
        ("k.json", r#"{ "b": 2 }"#),
    ];
    let Some(out) = run("json", &files, &["main.ts"]) else {
        return;
    };
    assert_eq!(stdout(&out), "1 2\n");
}

#[test]
fn a_directory_with_nothing_to_load_is_a_missing_module() {
    let files = [
        ("package.json", r#"{ "type": "module" }"#),
        (
            "main.ts",
            "await import('./empty/').catch((e) => console.log(e.code, e.message.startsWith('Cannot find module')));\n",
        ),
        ("empty/readme.txt", ""),
    ];
    let Some(out) = run("empty-dir", &files, &["main.ts"]) else {
        return;
    };
    assert_eq!(stdout(&out), "ERR_MODULE_NOT_FOUND true\n");
}

#[test]
fn tsconfig_paths_and_base_url_apply_to_javascript_parents() {
    let files = [
        ("package.json", r#"{ "type": "module" }"#),
        (
            "tsconfig.json",
            r#"{ "compilerOptions": { "allowJs": true, "baseUrl": ".", "paths": { "alias": ["./value.ts"], "@/*": ["./*"] } } }"#,
        ),
        (
            "entry.ts",
            "import { f } from 'src/file';\nawait import('./parent.js?q=1');\nconsole.log(f);\n",
        ),
        (
            "parent.js",
            "import { value } from 'alias';\nimport j from '@/data/';\nconsole.log(value, j.n);\n",
        ),
        ("value.ts", "export const value: string = 'from-alias';\n"),
        ("data/index.json", r#"{ "n": 3 }"#),
        ("src/file.ts", "export const f: string = 'from-base-url';\n"),
    ];
    let Some(out) = run("paths", &files, &["entry.ts"]) else {
        return;
    };
    assert_eq!(stdout(&out), "from-alias 3\nfrom-base-url\n");
}

#[test]
fn typescript_assigning_module_exports_runs_as_commonjs_in_a_module_package() {
    let files = [
        ("package.json", r#"{ "type": "module" }"#),
        (
            "load.cjs",
            "console.log(JSON.stringify(require('./cjs-style.ts')));\n",
        ),
        (
            "cjs-style.ts",
            "import path = require('node:path');\nmodule.exports = { name: path.basename(__filename) };\n",
        ),
    ];
    let Some(out) = run("cjs-exports", &files, &["load.cjs"]) else {
        return;
    };
    assert_eq!(stdout(&out), "{\"name\":\"cjs-style.ts\"}\n");
}

#[test]
fn jsx_files_without_allow_js_use_react_create_element() {
    let jsx = "try {\n  console.log(JSON.stringify(<b>x</b>));\n} catch (e) {\n  console.log(e.message);\n}\n";
    let base = r#"{ "compilerOptions": { "jsxFactory": "Array", "jsxFragmentFactory": "null" } }"#;
    let allow = r#"{ "extends": "./tsconfig.json", "compilerOptions": { "allowJs": true } }"#;
    let files = [
        ("tsconfig.json", base),
        ("tsconfig-allowJs.json", allow),
        ("a.jsx", jsx),
    ];
    let Some(out) = run("jsx-plain", &files, &["a.jsx"]) else {
        return;
    };
    assert_eq!(stdout(&out), "React is not defined\n");
    let Some(out) = run(
        "jsx-allow",
        &files,
        &["--tsconfig", "tsconfig-allowJs.json", "a.jsx"],
    ) else {
        return;
    };
    assert_eq!(stdout(&out), "[\"b\",null,\"x\"]\n");
}

#[test]
fn a_tsconfig_given_with_a_missing_extends_stops_the_run() {
    let files = [
        ("tsconfig.json", r#"{ "extends": "doesnt-exist" }"#),
        ("index.ts", "console.log('ran');\n"),
    ];
    let Some(out) = run(
        "bad-extends",
        &files,
        &["--tsconfig", "tsconfig.json", "index.ts"],
    ) else {
        return;
    };
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("TS6053"));
}

#[test]
fn test_runner_locations_map_to_typescript_lines() {
    let test = "import assert from 'node:assert/strict'\nimport { test } from 'node:test'\n\n\
                test('fails', () => {\n  const x: number = 1\n  assert.ok(x > 1)\n})\n";
    let Some(out) = run("test-loc", &[("t.test.ts", test)], &["--test", "t.test.ts"]) else {
        return;
    };
    let all = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{all}");
    assert!(all.contains("t.test.ts:4:1"), "{all}");
}

#[test]
fn test_runner_without_files_finds_typescript_tests() {
    let files = [
        (
            "a.test.ts",
            "import { test } from 'node:test';\nenum E { A = 1 }\ntest('enum test', () => { if (E.A !== 1) throw new Error(); });\n",
        ),
        (
            "test/b.ts",
            "import { test } from 'node:test';\nnamespace N { export const x: number = 2; }\ntest('namespace test', () => { if (N.x !== 2) throw new Error(); });\n",
        ),
        ("other.ts", "throw new Error('not a test file');\n"),
    ];
    let Some(out) = run("test-glob", &files, &["--test"]) else {
        return;
    };
    let all = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{all}");
    assert!(
        all.contains("enum test") && all.contains("namespace test"),
        "{all}"
    );
    assert!(all.contains("pass 2"), "{all}");
}

/// The program signals its parent (ts-r-run), which relays the signal back: a listener of
/// the program's own runs, and the relay's own listener stays hidden.
#[cfg(unix)]
#[test]
fn signals_sent_to_ts_r_run_reach_the_program() {
    let app = "process.on('SIGTERM', () => {\n\
                 console.log('listeners', process.listeners('SIGTERM').length, process.listenerCount('SIGTERM'));\n\
                 console.log('raw', process.rawListeners('SIGTERM').length);\n\
                 process.exit(3);\n\
               });\n\
               setTimeout(() => {}, 1e5);\n\
               process.kill(process.ppid, 'SIGTERM');\n";
    let Some(out) = run("relay", &[("app.ts", app)], &["app.ts"]) else {
        return;
    };
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "listeners 1 1\nraw 2\n"
    );
}

/// A program that never returns to its event loop is killed, with the code of the signal.
#[cfg(unix)]
#[test]
fn an_unresponsive_program_is_killed_on_sigint() {
    let app = "process.kill(process.ppid, 'SIGINT');\nwhile (true) {}\n";
    let Some(out) = run("relay-loop", &[("app.ts", app)], &["app.ts"]) else {
        return;
    };
    assert_eq!(out.status.code(), Some(130));
}

#[test]
fn import_meta_runs_in_commonjs_output() {
    let files = [
        ("package.json", r#"{ "type": "commonjs" }"#),
        (
            "main.ts",
            "import { name } from './a';\nconst m: ImportMeta = import.meta;\nconsole.log(name, m.url.endsWith('/main.ts'), m.filename === __filename, typeof module);\n",
        ),
        ("a.ts", "export const name: string = 'a';\n"),
    ];
    let Some(out) = run("import-meta-cjs", &files, &["main.ts"]) else {
        return;
    };
    assert_eq!(stdout(&out), "a true true object\n");
}

#[test]
fn compiled_commonjs_hides_its_esmodule_marker_from_named_exports() {
    let files = [
        ("package.json", r#"{ "type": "module" }"#),
        (
            "main.ts",
            "import * as ns from './a.cts';\nconsole.log(Object.keys(ns).sort().join());\n",
        ),
        ("a.cts", "export const x: number = 1;\n"),
    ];
    let Some(out) = run("esmodule-marker", &files, &["main.ts"]) else {
        return;
    };
    // Node 23+ also exports a CommonJS module's `module.exports` by that name (tsx shows the
    // same names on each version).
    let expected = if node_major() >= 23 {
        "default,module.exports,x\n"
    } else {
        "default,x\n"
    };
    assert_eq!(stdout(&out), expected);
}

#[test]
fn a_query_makes_a_commonjs_module_of_its_own() {
    let files = [
        ("package.json", r#"{ "type": "commonjs" }"#),
        (
            "main.ts",
            "import { n as a } from './count?a';\nimport { n as b } from './count?b';\nimport { n as c } from './count?a';\nconsole.log(a, b, c);\n",
        ),
        (
            "count.ts",
            "const g = globalThis as { hits?: number };\ng.hits = (g.hits ?? 0) + 1;\nexport const n: number = g.hits;\nconsole.log(import.meta.url.slice(import.meta.url.indexOf('?')));\n",
        ),
    ];
    let Some(out) = run("query-cjs", &files, &["main.ts"]) else {
        return;
    };
    assert_eq!(stdout(&out), "?a\n?b\n1 2 1\n");
}

#[test]
fn a_syntax_error_behind_import_throws_only_when_loaded() {
    let files = [
        ("package.json", r#"{ "type": "module" }"#),
        (
            "main.ts",
            "console.log('start');\nawait import('./broken').catch((e) => console.log(e.name, e.message.startsWith('Transform failed')));\n",
        ),
        ("broken.ts", "const = ;\n"),
    ];
    let Some(out) = run("lazy-error", &files, &["main.ts"]) else {
        return;
    };
    assert_eq!(stdout(&out), "start\nTransformError true\n");
    // A static import of the same file still stops the run before Node starts.
    let files = [
        ("package.json", r#"{ "type": "module" }"#),
        ("main.ts", "import './broken';\nconsole.log('start');\n"),
        ("broken.ts", "const = ;\n"),
    ];
    let Some(out) = run("eager-error", &files, &["main.ts"]) else {
        return;
    };
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
}

#[test]
fn requiring_top_level_await_is_a_transform_error() {
    let files = [
        ("package.json", r#"{ "type": "commonjs" }"#),
        (
            "main.ts",
            "try { require('./tla.mts'); } catch (e) { console.log((e as Error).name, (e as Error).message.startsWith('Top-level await')); }\n",
        ),
        ("tla.mts", "await Promise.resolve();\nexport {};\n"),
    ];
    let Some(out) = run("tla-require", &files, &["main.ts"]) else {
        return;
    };
    assert_eq!(stdout(&out), "TransformError true\n");
}

#[test]
fn dynamic_import_unwraps_compiled_es_modules_and_json_has_named_exports() {
    let files = [
        ("package.json", r#"{ "type": "module" }"#),
        (
            "main.ts",
            "import * as j from './j.json';\nconst m = await import('pkg');\nconst p = await import('./plain.mjs');\nconsole.log(m.default, m.named, j['loaded-file'], p.ok);\n",
        ),
        ("j.json", r#"{ "loaded-file": "json", "default": 0 }"#),
        (
            "plain.mjs",
            "const m = await import ('pkg');\nexport const ok = m.default === 1;\n",
        ),
        (
            "node_modules/pkg/package.json",
            r#"{ "name": "pkg", "type": "commonjs" }"#,
        ),
        (
            "node_modules/pkg/index.js",
            "export const named = 2;\nexport default 1;\n",
        ),
    ];
    let Some(out) = run("import-interop", &files, &["main.ts"]) else {
        return;
    };
    assert_eq!(stdout(&out), "1 2 json true\n");
}

#[test]
fn require_finds_typescript_in_packages_and_takes_file_urls() {
    let files = [
        ("package.json", r#"{ "type": "commonjs" }"#),
        (
            "main.ts",
            "const { a } = require('pkg/a.js');\nconst url = require('node:url').pathToFileURL(__dirname + '/b.ts').href;\nconsole.log(a, require(url).b);\n",
        ),
        (
            "node_modules/pkg/package.json",
            r#"{ "name": "pkg", "type": "commonjs" }"#,
        ),
        ("node_modules/pkg/a.ts", "export const a: string = 'a';\n"),
        ("b.ts", "export const b: string = 'b';\n"),
    ];
    let Some(out) = run("require-ts-pkg", &files, &["main.ts"]) else {
        return;
    };
    assert_eq!(stdout(&out), "a b\n");
}

#[test]
fn an_import_of_typescript_in_a_package_is_not_type_stripped_by_node() {
    let files = [
        ("package.json", r#"{ "type": "module" }"#),
        (
            "main.ts",
            "import { a } from 'pkg/a.js';\nconsole.log(a);\n",
        ),
        (
            "node_modules/pkg/package.json",
            r#"{ "name": "pkg", "type": "commonjs" }"#,
        ),
        ("node_modules/pkg/a.ts", "export const a: string = 'a';\n"),
    ];
    let Some(out) = run("pkg-ts-import", &files, &["main.ts"]) else {
        return;
    };
    assert_eq!(stdout(&out), "a\n");
}

#[test]
fn require_of_a_parent_directory_in_a_package_finds_its_typescript_index() {
    let files = [
        ("package.json", r#"{ "type": "commonjs" }"#),
        ("entry.cjs", "console.log(require('dep').tag);\n"),
        (
            "node_modules/dep/package.json",
            r#"{ "name": "dep", "main": "./lib/sub/entry.js" }"#,
        ),
        (
            "node_modules/dep/lib/index.ts",
            "export const tag: string = 'ts-index';\n",
        ),
        (
            "node_modules/dep/lib/sub/entry.js",
            "module.exports = require('..');\n",
        ),
    ];
    let Some(out) = run("require-dotdot", &files, &["entry.cjs"]) else {
        return;
    };
    assert_eq!(stdout(&out), "ts-index\n");
}
