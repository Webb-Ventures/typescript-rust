//! End to end: the `ts-r-dev` binary serves the fixture app to headless Chrome; edits are
//! applied in place (no page reload) where an HMR boundary exists, and reload the page
//! where none does. Skipped (with a message) when Chrome isn't installed.

mod support;

use std::time::Duration;

use support::{Chrome, DevProcess, find_chrome};
use ts_r_hmr::testutil::{TempDir, counter_js, write_fixture_app};

const T: Duration = Duration::from_secs(20);

#[test]
fn hot_updates_in_headless_chrome() {
    let Some(chrome_bin) = find_chrome() else {
        eprintln!("skipping: Chrome not found (set TS_R_CHROME)");
        return;
    };
    let app = TempDir::new("e2e-app");
    let cache = TempDir::new("e2e-cache");
    write_fixture_app(&app);
    let dev = DevProcess::start(app.path(), cache.path(), &[]);
    let mut chrome = Chrome::launch(&chrome_bin);
    chrome.open(&dev.url);
    let cdp = &mut chrome.cdp;

    // Initial load: modules, dependency, JSON, asset, both kinds of CSS.
    cdp.wait_for(
        "document.getElementById('app')?.textContent === 'count v1'",
        T,
    );
    assert_eq!(
        cdp.eval("document.getElementById('dep').textContent")
            .unwrap(),
        "dep-ok"
    );
    assert_eq!(
        cdp.eval("document.getElementById('data').textContent")
            .unwrap(),
        "fixture-data"
    );
    // A small SVG is inlined as a data URL, as Vite does in dev.
    assert_eq!(
        cdp.eval("document.getElementById('logo').textContent")
            .unwrap(),
        "data:image/svg+xml,%3csvg%20xmlns='http://www.w3.org/2000/svg'%3e%3c/svg%3e"
    );
    cdp.wait_for(
        "getComputedStyle(document.getElementById('app')).color === 'rgb(255, 0, 0)'",
        T,
    );
    cdp.wait_for(
        "getComputedStyle(document.body).backgroundColor === 'rgb(1, 2, 3)'",
        T,
    );
    // The HMR socket is up before we edit anything.
    cdp.wait_for(
        "performance.getEntriesByType('resource').some(e => e.name.includes('/@ts-r/client')) && window.__loads === 1",
        T,
    );
    std::thread::sleep(Duration::from_millis(300));
    let marker = cdp.eval("window.__marker").unwrap();
    assert!(
        marker.as_str().is_some_and(|m| m.starts_with("page-")),
        "{marker}"
    );
    let same_page = format!("window.__marker === {marker}");

    // 1. Self-accepting module, saved atomically → updated in place; hot.data persists and
    //    dispose ran once.
    app.write_atomic("src/counter.js", &counter_js("v2", ""));
    cdp.wait_for(
        "document.getElementById('app').textContent === 'count v2'",
        T,
    );
    assert_eq!(cdp.eval(&same_page).unwrap(), true, "no full reload");
    assert_eq!(cdp.eval("window.__accepted").unwrap(), 1);
    assert_eq!(
        cdp.eval("window.__loads").unwrap(),
        2,
        "import.meta.hot.data persisted"
    );
    assert_eq!(cdp.eval("window.__disposed").unwrap(), 1);
    // Events fire once each: the old instance's listeners are dropped when the new one runs.
    assert_eq!(
        cdp.eval("[window.__before, window.__after]").unwrap(),
        serde_json::json!([1, 1])
    );

    // 2. Dependency accepted by its importer (main.js accepts './accepted.js').
    app.write(
        "src/accepted.js",
        "export const accepted = 'accepted-v2';\n",
    );
    cdp.wait_for(
        "document.getElementById('accepted').textContent === 'accepted-v2'",
        T,
    );
    assert_eq!(cdp.eval("window.__depAccepts").unwrap(), 1);
    assert_eq!(cdp.eval(&same_page).unwrap(), true);

    // 3. CSS imported from JS and CSS linked from HTML.
    app.write("src/style.css", "#app { color: rgb(0, 0, 255); }\n");
    cdp.wait_for(
        "getComputedStyle(document.getElementById('app')).color === 'rgb(0, 0, 255)'",
        T,
    );
    // The link's own query survives the swap; only `t` changes.
    cdp.eval("document.querySelector('link[rel=stylesheet]').href = '/src/global.css?keep=1'")
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    app.write(
        "src/global.css",
        "body { background-color: rgb(4, 5, 6); }\n",
    );
    cdp.wait_for(
        "getComputedStyle(document.body).backgroundColor === 'rgb(4, 5, 6)'",
        T,
    );
    cdp.wait_for(
        "/[?]keep=1&t=\\d+$/.test(document.querySelector('link[rel=stylesheet]').href)",
        T,
    );
    assert_eq!(cdp.eval(&same_page).unwrap(), true);
    assert_eq!(
        cdp.eval("document.querySelectorAll('link[rel=stylesheet]').length")
            .unwrap(),
        1,
        "old <link> removed after swap"
    );

    // 4. Error overlay on a broken import, cleared by the fix, still without a reload.
    app.write(
        "src/counter.js",
        &counter_js("v3", "import './missing.js';"),
    );
    cdp.wait_for("document.querySelector('vite-error-overlay')", T);
    let msg = cdp
        .eval("document.querySelector('vite-error-overlay').shadowRoot.querySelector('.message').textContent")
        .unwrap();
    assert!(msg.as_str().unwrap().contains("missing.js"), "{msg}");
    app.write("src/counter.js", &counter_js("v4", ""));
    cdp.wait_for(
        "document.getElementById('app').textContent === 'count v4'",
        T,
    );
    cdp.wait_for("!document.querySelector('vite-error-overlay')", T);
    assert_eq!(cdp.eval(&same_page).unwrap(), true);

    // 5. A module with no boundary → full page reload.
    app.write("src/plain.js", "export const label = 'plain-v2';\n");
    cdp.wait_for("document.body?.dataset.label === 'plain-v2'", T);
    assert_eq!(cdp.eval(&same_page).unwrap(), false, "page reloaded");
    cdp.wait_for(
        "document.getElementById('app')?.textContent === 'count v4'",
        T,
    );

    // Latency lines were logged with the per-step breakdown.
    let lines = dev.log_lines();
    let hmr: Vec<_> = lines
        .iter()
        .filter(|l| l.contains("hmr /src/counter.js js-update"))
        .collect();
    assert!(!hmr.is_empty(), "{lines:#?}");
    assert!(
        hmr[0].contains("browser ") && !hmr[0].contains("browser -"),
        "{}",
        hmr[0]
    );
    for l in &lines {
        eprintln!("{l}");
    }
}

/// An error thrown inside a pre-bundled CommonJS dependency: Chrome's stack trace points at
/// the batch file, and the batch's source map takes that position back to the line in the
/// original `node_modules` file (also after a warm restart, when maps come from the disk
/// cache).
#[test]
fn dependency_stack_traces_map_back_to_node_modules() {
    let Some(chrome_bin) = find_chrome() else {
        eprintln!("skipping: Chrome not found (set TS_R_CHROME)");
        return;
    };
    let app = TempDir::new("e2e-depmap");
    let cache = TempDir::new("e2e-depmap-cache");
    app.write(
        "index.html",
        "<!doctype html><body><script type=\"module\" src=\"/src/main.js\"></script></body>\n",
    );
    app.write(
        "src/main.js",
        "import { run } from 'thrower';\ntry { run(); } catch (e) { window.__stack = e.stack; }\n",
    );
    app.write(
        "node_modules/thrower/package.json",
        r#"{ "name": "thrower", "version": "1.0.0", "main": "index.js" }"#,
    );
    app.write(
        "node_modules/thrower/index.js",
        "'use strict';\nconst impl = require('./lib/impl.js');\nexports.run = function run() { return impl.fail(); };\n",
    );
    // `new Error` (where V8 records the frame) is on line 5, column 9 (0-based 4 and 8).
    app.write(
        "node_modules/thrower/lib/impl.js",
        "'use strict';\n\n// padding\nexports.fail = function fail() {\n  throw new Error('thrown in thrower');\n};\n",
    );
    let dev = DevProcess::start(app.path(), cache.path(), &[]);
    let mut chrome = Chrome::launch(&chrome_bin);
    chrome.open(&dev.url);
    let stack = chrome.cdp.wait_for("window.__stack", T);
    let stack = stack.as_str().unwrap().to_owned();
    // "    at fail (http://127.0.0.1:1234/@ts-r/deps/thrower.js?v=abcd1234:12:9)"
    let frame = stack
        .lines()
        .find(|l| l.contains("/@ts-r/deps/"))
        .unwrap_or_else(|| panic!("no dependency frame in {stack}"));
    let loc = frame
        .rsplit_once('(')
        .map_or(frame, |(_, r)| r)
        .trim_end_matches(')')
        .trim()
        .trim_start_matches("at ");
    let (url, line_col) = loc.split_once("/@ts-r/").unwrap();
    let mut parts = line_col.rsplitn(3, ':');
    let col: u32 = parts.next().unwrap().parse().unwrap();
    let line: u32 = parts.next().unwrap().parse().unwrap();
    let path = format!("/@ts-r/{}", parts.next().unwrap());
    assert!(url.starts_with("http://"), "{frame}");
    let addr = dev.addr.parse().unwrap();
    let (status, _, js) = ts_r_hmr::testutil::http_get(addr, &path, &[]);
    assert_eq!(status, 200, "{path}");
    let file = path.split('?').next().unwrap().rsplit('/').next().unwrap();
    assert!(
        js.contains(&format!("//# sourceMappingURL={file}.map")),
        "{js}"
    );
    let check_map = |addr| {
        let (status, h, map) =
            ts_r_hmr::testutil::http_get(addr, &format!("/@ts-r/deps/{file}.map"), &[]);
        assert_eq!(status, 200, "{map}");
        assert_eq!(
            ts_r_hmr::testutil::header(&h, "content-type"),
            Some("application/json")
        );
        let v: serde_json::Value = serde_json::from_str(&map).unwrap();
        let segs = ts_r_codegen::sourcemap::decode_mappings(v["mappings"].as_str().unwrap());
        let seg = segs
            .iter()
            .filter(|s| s.gen_line == line - 1 && s.gen_col < col && s.source.is_some())
            .max_by_key(|s| s.gen_col)
            .unwrap_or_else(|| panic!("no mapping for {line}:{col}"));
        let (si, orig_line, orig_col, _) = seg.source.unwrap();
        let source = v["sources"][si as usize].as_str().unwrap();
        // Relative to the map's URL (/@ts-r/deps/), so it resolves to /node_modules/...
        assert_eq!(source, "../../node_modules/thrower/lib/impl.js", "{v}");
        assert_eq!((orig_line, orig_col), (4, 8), "{frame}");
        let content = v["sourcesContent"][si as usize].as_str().unwrap();
        assert!(content.contains("thrown in thrower"), "{content}");
        map
    };
    let first = check_map(addr);
    // Warm restart: the batch and its maps come from the disk cache.
    drop(chrome);
    drop(dev);
    let dev = DevProcess::start(app.path(), cache.path(), &[]);
    let addr = dev.addr.parse().unwrap();
    assert!(
        dev.log_lines().iter().any(|l| l.contains("from cache"))
            || std::iter::repeat_with(|| {
                std::thread::sleep(Duration::from_millis(20));
                dev.log_lines()
            })
            .take(100)
            .any(|ls| ls.iter().any(|l| l.contains("from cache"))),
        "{:?}",
        dev.log_lines()
    );
    assert_eq!(check_map(addr), first);
}

/// CSS from packages: a dependency's own `import './x.css'` (bundled into the batch, its
/// `@import` inlined, `url()` pointing at the package's file), a stylesheet the app imports
/// from a package hoisted above the root (served via `/@ts-r/up/`), and a linked package's
/// CSS, which hot-updates without a reload.
#[cfg(unix)]
#[test]
fn dependency_css_is_bundled_served_and_linked_css_hot_updates() {
    let Some(chrome_bin) = find_chrome() else {
        eprintln!("skipping: Chrome not found (set TS_R_CHROME)");
        return;
    };
    let ws = TempDir::new("e2e-depcss");
    let cache = TempDir::new("e2e-depcss-cache");
    ws.write(
        "app/index.html",
        "<!doctype html><body><div id=\"a\" class=\"fancy\">a</div><div id=\"b\" class=\"theme\">b</div><div id=\"c\" class=\"linked\">c</div><script type=\"module\" src=\"/src/main.js\"></script></body>\n",
    );
    ws.write(
        "app/src/main.js",
        "import { x } from 'fancy-ui';\nimport 'fancy-ui/theme.css';\nimport 'linked-ui/linked.css';\nwindow.__marker ??= 'page-' + x + Math.random();\n",
    );
    // Hoisted above the root, like a workspace's node_modules.
    ws.write(
        "node_modules/fancy-ui/package.json",
        r#"{ "name": "fancy-ui", "version": "1.0.0", "type": "module", "main": "index.js" }"#,
    );
    ws.write(
        "node_modules/fancy-ui/index.js",
        "import './fancy.css';\nexport const x = 1;\n",
    );
    ws.write(
        "node_modules/fancy-ui/fancy.css",
        "@import './base.css';\n.fancy { background-image: url(./img/dot.svg); }\n",
    );
    ws.write(
        "node_modules/fancy-ui/base.css",
        ".fancy { color: rgb(1, 2, 3); }\n",
    );
    ws.write(
        "node_modules/fancy-ui/theme.css",
        ".theme { color: rgb(4, 5, 6); background-image: url(\"./img/dot.svg\"); }\n",
    );
    ws.write(
        "node_modules/fancy-ui/img/dot.svg",
        "<svg xmlns=\"http://www.w3.org/2000/svg\"/>\n",
    );
    ws.write(
        "packages/linked-ui/package.json",
        r#"{ "name": "linked-ui", "version": "1.0.0" }"#,
    );
    ws.write(
        "packages/linked-ui/linked.css",
        ".linked { color: rgb(7, 8, 9); }\n",
    );
    std::fs::create_dir_all(ws.path().join("app/node_modules")).unwrap();
    std::os::unix::fs::symlink(
        "../../packages/linked-ui",
        ws.path().join("app/node_modules/linked-ui"),
    )
    .unwrap();

    let dev = DevProcess::start(&ws.path().join("app"), cache.path(), &[]);
    let mut chrome = Chrome::launch(&chrome_bin);
    chrome.open(&dev.url);
    let cdp = &mut chrome.cdp;
    let color = |id: &str, rgb: &str| {
        format!("getComputedStyle(document.getElementById('{id}')).color === '{rgb}'")
    };
    cdp.wait_for(&color("a", "rgb(1, 2, 3)"), T);
    cdp.wait_for(&color("b", "rgb(4, 5, 6)"), T);
    cdp.wait_for(&color("c", "rgb(7, 8, 9)"), T);
    let addr = dev.addr.parse().unwrap();
    for id in ["a", "b"] {
        let bg = cdp
            .eval(&format!(
                "getComputedStyle(document.getElementById('{id}')).backgroundImage"
            ))
            .unwrap();
        let bg = bg.as_str().unwrap();
        // url("http://127.0.0.1:1234/@ts-r/up/1/node_modules/fancy-ui/img/dot.svg")
        let path = bg
            .split_once("/@ts-r/")
            .map(|(_, p)| format!("/@ts-r/{}", p.trim_end_matches("\")")))
            .unwrap_or_else(|| panic!("{id}: {bg}"));
        assert_eq!(
            path, "/@ts-r/up/1/node_modules/fancy-ui/img/dot.svg",
            "{id}: {bg}"
        );
        let (status, h, body) = ts_r_hmr::testutil::http_get(addr, &path, &[]);
        assert_eq!(status, 200, "{path}");
        assert_eq!(
            ts_r_hmr::testutil::header(&h, "content-type"),
            Some("image/svg+xml")
        );
        assert!(body.contains("<svg"), "{body}");
    }
    // Only files inside node_modules are served that way.
    let (status, _, _) =
        ts_r_hmr::testutil::http_get(addr, "/@ts-r/up/1/packages/linked-ui/linked.css", &[]);
    assert_eq!(status, 403);
    std::thread::sleep(Duration::from_millis(300));
    let marker = cdp.eval("window.__marker").unwrap();
    ws.write(
        "packages/linked-ui/linked.css",
        ".linked { color: rgb(9, 8, 7); }\n",
    );
    cdp.wait_for(&color("c", "rgb(9, 8, 7)"), T);
    assert_eq!(
        cdp.eval(&format!("window.__marker === {marker}")).unwrap(),
        true,
        "no reload"
    );
}
