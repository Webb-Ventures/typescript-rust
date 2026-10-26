//! The dev server's dependency adapter (`--features hmr`): CommonJS packages shaped like
//! React and React DOM, plus an ESM package, are pre-bundled one specifier at a time; the
//! modules are linked the way `ts-r-dev` serves them (bare imports rewritten) and run under
//! Node. Set `TS_R_REACT_ROOT` to a project with `react` and `react-dom` installed to also
//! render with the real packages.
#![cfg(feature = "hmr")]

ts_r_testing::cap_memory!();

use std::path::{Path, PathBuf};
use std::process::Command;

use ts_r_bundler::hmr::DepBundle;
use ts_r_hmr::{DepBundler, DepRequest};

fn have_node() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn write(dir: &Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
    std::fs::write(p, content).expect("write");
}

/// Module file name for a bare specifier in the output directory.
fn module_file(spec: &str) -> String {
    format!("{}.mjs", spec.replace('/', "__"))
}

/// Pre-bundles `spec` (whose entry is `entry`) and writes it with bare imports pointing at
/// sibling module files; returns the bare specifiers it imports.
fn serve(bundler: &DepBundle, out: &Path, spec: &str, package: &str, entry: &Path) -> Vec<String> {
    let source = std::fs::read_to_string(entry).expect("read entry");
    let r = bundler
        .bundle(
            &DepRequest {
                specifier: spec,
                package,
                file: entry,
            },
            &source,
        )
        .unwrap_or_else(|e| panic!("{spec}: {e}"));
    let mut code = r.code.clone();
    let mut peers = Vec::new();
    for rec in r.imports.iter().rev() {
        assert!(
            !rec.specifier.starts_with('.') && !rec.specifier.starts_with('/'),
            "{spec}: relative import left in the bundle: {}",
            rec.specifier
        );
        let target = format!("./{}", module_file(&rec.specifier));
        code.replace_range(rec.start as usize..rec.end as usize, &target);
        peers.push(rec.specifier.clone());
    }
    std::fs::write(out.join(module_file(spec)), code).expect("write module");
    peers
}

/// Serves `specs` and everything they import. Bare specifiers resolve like Node's
/// `node_modules` lookup from the importing file's real directory (so pnpm layouts work),
/// with the `main` field or a `<subpath>.js` file; the fixtures and React need no more.
fn serve_all(bundler: &DepBundle, project: &Path, out: &Path, specs: &[&str]) {
    let mut todo: Vec<(String, PathBuf)> = specs
        .iter()
        .map(|s| ((*s).to_owned(), project.to_path_buf()))
        .collect();
    let mut done = std::collections::BTreeSet::new();
    while let Some((spec, from)) = todo.pop() {
        if !done.insert(spec.clone()) {
            continue;
        }
        let (package, sub) = match spec.find('/') {
            Some(i) if !spec.starts_with('@') => (&spec[..i], Some(&spec[i + 1..])),
            _ => (spec.as_str(), None),
        };
        let pkg_dir = from
            .ancestors()
            .map(|d| d.join("node_modules").join(package))
            .find(|d| d.join("package.json").is_file())
            .unwrap_or_else(|| panic!("cannot find {package} from {}", from.display()));
        let entry = match sub {
            Some(s) => {
                let p = pkg_dir.join(s);
                if p.extension().is_some_and(|e| e == "js") {
                    p
                } else {
                    pkg_dir.join(format!("{s}.js"))
                }
            }
            None => {
                let pj = std::fs::read_to_string(pkg_dir.join("package.json")).expect("pkg");
                let main = pj
                    .split("\"main\"")
                    .nth(1)
                    .and_then(|r| r.split('"').nth(1))
                    .unwrap_or("index.js")
                    .to_owned();
                pkg_dir.join(main)
            }
        };
        let real = std::fs::canonicalize(&entry).expect("realpath");
        let dir = real.parent().expect("parent").to_path_buf();
        for peer in serve(bundler, out, &spec, package, &entry) {
            todo.push((peer, dir.clone()));
        }
    }
}

fn run_node(out: &Path, entry: &str) -> String {
    let o = ts_r_testing::limited("node", 1024, 60)
        .arg(out.join(entry))
        .output()
        .expect("run node");
    assert!(
        o.status.success(),
        "node failed:\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn commonjs_and_esm_dependencies_link_in_the_browser_shape() {
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("dep-bundle");
    let _ = std::fs::remove_dir_all(&tmp);
    let project = tmp.join("project");
    let out = tmp.join("out");
    std::fs::create_dir_all(&out).expect("mkdir");
    write(
        &project,
        "node_modules/react/package.json",
        r#"{ "name": "react", "main": "index.js" }"#,
    );
    write(
        &project,
        "node_modules/react/index.js",
        "'use strict';\nif (process.env.NODE_ENV === 'production') {\n  module.exports = require('./cjs/react.production.js');\n} else {\n  module.exports = require('./cjs/react.development.js');\n}\n",
    );
    write(
        &project,
        "node_modules/react/cjs/react.production.js",
        "exports.version = 'prod';\nexports.useState = function (i) { return [i]; };\n",
    );
    write(
        &project,
        "node_modules/react/cjs/react.development.js",
        "'use strict';\n'production' !== process.env.NODE_ENV && (function () {\n  var internals = { owner: null };\n  exports.useState = function (initial) { return [initial, function () {}]; };\n  exports.createElement = function (type, props) { return { type: type, props: props }; };\n  exports.version = '19-test';\n  exports.__INTERNALS = internals;\n})();\n",
    );
    write(
        &project,
        "node_modules/react/jsx-runtime.js",
        "'use strict';\nvar React = require('react');\nexports.jsx = function (type, props) { return React.createElement(type, props); };\n",
    );
    write(
        &project,
        "node_modules/react-dom/package.json",
        r#"{ "name": "react-dom", "main": "index.js" }"#,
    );
    write(
        &project,
        "node_modules/react-dom/index.js",
        "'use strict';\nmodule.exports = require('./cjs/react-dom.development.js');\n",
    );
    write(
        &project,
        "node_modules/react-dom/cjs/react-dom.development.js",
        "'use strict';\nvar React = require('react');\nvar Scheduler = require('scheduler');\nexports.render = function (el) { return 'rendered ' + el.type + ' with ' + React.version + ' at ' + Scheduler.now(); };\nexports.sameReact = function (r) { return r === React && r.__INTERNALS === React.__INTERNALS; };\n",
    );
    write(
        &project,
        "node_modules/scheduler/package.json",
        r#"{ "name": "scheduler", "main": "index.js" }"#,
    );
    write(
        &project,
        "node_modules/scheduler/index.js",
        "Object.defineProperty(exports, '__esModule', { value: true });\nexports.now = function () { return 42; };\n",
    );
    write(
        &project,
        "node_modules/esm-pkg/package.json",
        r#"{ "name": "esm-pkg", "type": "module", "main": "index.js" }"#,
    );
    write(
        &project,
        "node_modules/esm-pkg/index.js",
        "import { useState } from 'react';\nimport { twice } from './util.js';\nexport const hook = () => twice(useState(21)[0]);\n",
    );
    write(
        &project,
        "node_modules/esm-pkg/util.js",
        "export const twice = (n) => n * 2;\n",
    );
    let bundler = DepBundle::new();
    serve_all(
        &bundler,
        &project,
        &out,
        &["react", "react-dom", "react/jsx-runtime", "esm-pkg"],
    );
    let react = std::fs::read_to_string(out.join("react.mjs")).expect("react");
    assert!(
        !react.contains("'prod'"),
        "production build included:\n{react}"
    );
    assert!(react.contains("useState"), "{react}");
    if !have_node() {
        return;
    }
    write(
        &out,
        "app.mjs",
        "import React, { useState, version, createElement } from './react.mjs';\nimport * as ReactDOM from './react-dom.mjs';\nimport { jsx } from './react__jsx-runtime.mjs';\nimport { hook } from './esm-pkg.mjs';\nconsole.log(version, useState(3)[0], ReactDOM.render(createElement('div')), ReactDOM.sameReact(React), jsx('p').type, hook());\n",
    );
    assert_eq!(
        run_node(&out, "app.mjs"),
        "19-test 3 rendered div with 19-test at 42 true p 42\n"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Renders with the real React packages when `TS_R_REACT_ROOT` names a project that has
/// them installed (for example the private-app web app).
#[test]
fn real_react_renders_when_available() {
    let Some(project) = std::env::var_os("TS_R_REACT_ROOT").map(PathBuf::from) else {
        return;
    };
    if !have_node() {
        return;
    }
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("dep-bundle-real-react");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).expect("mkdir");
    let bundler = DepBundle::new();
    serve_all(
        &bundler,
        &project,
        &out,
        &[
            "react",
            "react/jsx-runtime",
            "react-dom/server.browser",
            "react-dom/client",
        ],
    );
    write(
        &out,
        "app.mjs",
        "import { useState, version } from './react.mjs';\nimport { jsx } from './react__jsx-runtime.mjs';\nimport { renderToString } from './react-dom__server.browser.mjs';\nimport { createRoot } from './react-dom__client.mjs';\nfunction App() { const [n] = useState(7); return jsx('p', { children: ['count ', n] }); }\nconsole.log(version.split('.')[0] >= 18, typeof createRoot, renderToString(jsx(App, {})));\n// React DOM's scheduler keeps a MessageChannel open.\nprocess.exit(0);\n",
    );
    assert_eq!(
        run_node(&out, "app.mjs"),
        "true function <p>count <!-- -->7</p>\n"
    );
    let _ = std::fs::remove_dir_all(&out);
}

/// Writes a batch to `out` (flat files, relative imports between them) as ES modules.
fn write_batch(out: &Path, batch: &ts_r_hmr::DepBatch) {
    write(out, "package.json", r#"{ "type": "module" }"#);
    for (name, file) in &batch.files {
        for rec in &file.imports {
            assert!(
                rec.specifier.starts_with("./"),
                "{name}: import outside the batch: {}",
                rec.specifier
            );
        }
        write(out, name, &file.code);
    }
}

/// The batch API: every entry in one build with code splitting. Two entry points of one
/// package that share an internal module get one copy of it (one module state), React is
/// shared between `react`, `react/jsx-runtime` and `react-dom`, and specifiers that resolve
/// to the same file share an entry.
#[test]
fn batch_shares_modules_between_entries() {
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("dep-batch");
    let _ = std::fs::remove_dir_all(&tmp);
    let project = tmp.join("project");
    let out = tmp.join("out");
    write(
        &project,
        "node_modules/react/package.json",
        r#"{ "name": "react", "main": "index.js" }"#,
    );
    write(
        &project,
        "node_modules/react/index.js",
        "'use strict';\nmodule.exports = require('./cjs/react.development.js');\n",
    );
    write(
        &project,
        "node_modules/react/cjs/react.development.js",
        "'use strict';\nvar internals = { renders: 0 };\nexports.createElement = function (type) { internals.renders++; return { type: type }; };\nexports.__INTERNALS = internals;\n",
    );
    write(
        &project,
        "node_modules/react/jsx-runtime.js",
        "'use strict';\nvar React = require('react');\nexports.jsx = function (type) { return React.createElement(type); };\n",
    );
    write(
        &project,
        "node_modules/react-dom/package.json",
        r#"{ "name": "react-dom", "main": "index.js" }"#,
    );
    write(
        &project,
        "node_modules/react-dom/index.js",
        "'use strict';\nvar React = require('react');\nexports.renders = function () { return React.__INTERNALS.renders; };\n",
    );
    write(
        &project,
        "node_modules/two/package.json",
        r#"{ "name": "two", "type": "module", "exports": { "./a": "./a.js", "./b": "./b.js" } }"#,
    );
    write(
        &project,
        "node_modules/two/a.js",
        "import { bump } from './shared.js';\nexport const a = () => bump();\n",
    );
    write(
        &project,
        "node_modules/two/b.js",
        "import { bump } from './shared.js';\nexport const b = () => bump();\n",
    );
    write(
        &project,
        "node_modules/two/shared.js",
        "let n = 0;\nexport const bump = () => ++n;\n",
    );
    let nm = project.join("node_modules");
    let reqs = [
        ("react", "react", nm.join("react/index.js")),
        ("react/index.js", "react", nm.join("react/index.js")),
        (
            "react/jsx-runtime",
            "react",
            nm.join("react/jsx-runtime.js"),
        ),
        ("react-dom", "react-dom", nm.join("react-dom/index.js")),
        ("two/a", "two", nm.join("two/a.js")),
        ("two/b", "two", nm.join("two/b.js")),
    ];
    let requests: Vec<DepRequest<'_>> = reqs
        .iter()
        .map(|(s, p, f)| DepRequest {
            specifier: s,
            package: p,
            file: f,
        })
        .collect();
    let batch = DepBundle::new().batch(&requests).expect("batch");
    assert_eq!(
        batch.entries,
        [
            "react.js",
            "react.js",
            "react_jsx-runtime.js",
            "react-dom.js",
            "two_a.js",
            "two_b.js"
        ]
    );
    let chunks = batch
        .files
        .iter()
        .filter(|(n, _)| n.starts_with("chunk-"))
        .count();
    assert!(chunks >= 1, "no shared chunk: {:?}", batch.files);
    // The shared module's source appears once across all files.
    let copies: usize = batch
        .files
        .iter()
        .map(|(_, f)| f.code.matches("++n").count())
        .sum();
    assert_eq!(copies, 1);
    // Deterministic.
    assert_eq!(DepBundle::new().batch(&requests).expect("batch"), batch);
    if !have_node() {
        return;
    }
    write_batch(&out, &batch);
    write(
        &out,
        "app.mjs",
        "import { createElement } from './react.js';\nimport { jsx } from './react_jsx-runtime.js';\nimport { renders } from './react-dom.js';\nimport { a } from './two_a.js';\nimport { b } from './two_b.js';\ncreateElement('x'); jsx('y');\nconsole.log(renders(), a(), b());\n",
    );
    assert_eq!(run_node(&out, "app.mjs"), "2 1 2\n");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Vite playground `dynamic-import`, "css in package": a `"type": "module"` package whose
/// entry only does `import('./pkg.css')` stays an ES module (no CommonJS facade), and the
/// stylesheet stays a request for the original file instead of a chunk that is never written.
#[test]
fn esm_package_with_only_a_dynamic_css_import() {
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("dep-dynamic-css");
    let _ = std::fs::remove_dir_all(&tmp);
    let project = tmp.join("project");
    write(
        &project,
        "node_modules/@vitejs/test-pkg/package.json",
        r#"{ "name": "@vitejs/test-pkg", "type": "module", "main": "index.js" }"#,
    );
    write(
        &project,
        "node_modules/@vitejs/test-pkg/index.js",
        "import('./pkg.css');\n",
    );
    write(
        &project,
        "node_modules/@vitejs/test-pkg/pkg.css",
        ".pkg { color: red }\n",
    );
    let project = ts_r_hmr::paths::canonicalize(&project).expect("canonical project");
    let entry = project.join("node_modules/@vitejs/test-pkg/index.js");
    let request = DepRequest {
        specifier: "@vitejs/test-pkg",
        package: "@vitejs/test-pkg",
        file: &entry,
    };
    let bundler = DepBundle::new().serve_root(&project);
    let want = "import(\"/node_modules/@vitejs/test-pkg/pkg.css\")";
    let batch = bundler.batch(&[request]).expect("batch");
    assert_eq!(batch.entries, ["@vitejs_test-pkg.js"]);
    let (_, js) = &batch.files[0];
    assert!(!js.code.contains("__tsrCjs"), "{}", js.code);
    assert!(js.code.contains(want), "{}", js.code);
    assert!(
        batch.files.iter().all(|(n, _)| !n.starts_with("chunk-")),
        "{:?}",
        batch.files.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );
    let source = std::fs::read_to_string(&entry).expect("read entry");
    let one = bundler.bundle(&request, &source).expect("bundle");
    assert!(!one.code.contains("__tsrCjs"), "{}", one.code);
    assert!(one.code.contains(want), "{}", one.code);
    let _ = std::fs::remove_dir_all(&tmp);
}
