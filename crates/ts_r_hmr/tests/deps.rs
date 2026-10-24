//! The dependency optimizer (`deps.rs`) with a stand-in batch bundler: the scan from
//! `index.html`, batch serving under `/@ts-r/deps/`, the disk cache across restarts, the
//! lockfile in the version, and re-batching (plus a page reload) when a module imports a
//! dependency the batch doesn't have.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use ts_r_hmr::testutil::{TempDir, header, http_get};
use ts_r_hmr::ws::WsClient;
use ts_r_hmr::{
    CacheMode, DepBatch, DepBundler, DepRequest, DevConfig, DevServer, TransformError,
    TransformOutput,
};

/// Every entry imports one shared chunk; records what it was asked for.
#[derive(Debug, Default)]
struct FakeBatch {
    calls: AtomicUsize,
    last: Mutex<Vec<String>>,
    /// Code appended to every entry.
    extra: String,
}

impl DepBundler for FakeBatch {
    fn id(&self) -> &str {
        "fake-batch/1"
    }

    fn bundle(
        &self,
        dep: &DepRequest<'_>,
        _source: &str,
    ) -> Result<TransformOutput, TransformError> {
        Err(TransformError::new(format!(
            "unexpected single bundle of {}",
            dep.specifier
        )))
    }

    fn bundle_batch(&self, deps: &[DepRequest<'_>]) -> Option<Result<DepBatch, TransformError>> {
        if deps.is_empty() {
            return Some(Ok(DepBatch::default()));
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last.lock().unwrap() = deps.iter().map(|d| d.specifier.to_owned()).collect();
        let file = |code: String| TransformOutput {
            imports: ts_r_hmr::scan::scan(&code).imports,
            code,
            map: None,
        };
        let mut files = vec![(
            "chunk-S.js".to_owned(),
            file("export const shared = {};\n".into()),
        )];
        let mut entries = Vec::new();
        for (i, d) in deps.iter().enumerate() {
            let name = format!("e{i}.js");
            files.push((
                name.clone(),
                file(format!(
                    "import {{ shared }} from \"./chunk-S.js\";\nexport const name = {:?};\nexport {{ shared }};\n{}",
                    d.specifier, self.extra
                )),
            ));
            entries.push(name);
        }
        Some(Ok(DepBatch { entries, files }))
    }
}

fn write_app(app: &TempDir) {
    app.write(
        "index.html",
        "<!doctype html><html><head></head><body><script type=\"module\" src=\"/src/main.ts\"></script></body></html>\n",
    );
    app.write(
        "src/main.ts",
        "import { name } from 'alpha';\nimport './util.js';\nexport const lazy = () => import('beta');\n",
    );
    app.write("src/util.js", "export { name } from 'alpha';\n");
    app.write(
        "src/late.js",
        "import { name } from 'gamma';\nexport { name };\n",
    );
    for pkg in ["alpha", "beta", "gamma"] {
        app.write(
            &format!("node_modules/{pkg}/package.json"),
            &format!("{{ \"name\": \"{pkg}\", \"main\": \"index.js\" }}"),
        );
        app.write(
            &format!("node_modules/{pkg}/index.js"),
            "module.exports = 1;\n",
        );
    }
}

/// `.ts` files served as JS (the fake app's TS is plain JS).
#[derive(Debug)]
struct TsAsJs;

impl ts_r_hmr::Transform for TsAsJs {
    fn cache_id(&self) -> &str {
        "ts-as-js/1"
    }
    fn handles(&self, path: &std::path::Path) -> bool {
        path.extension().is_some_and(|e| e == "ts")
    }
    fn transform(
        &self,
        input: &ts_r_hmr::TransformInput<'_>,
    ) -> Result<TransformOutput, TransformError> {
        ts_r_hmr::PassthroughJs.transform(input)
    }
}

fn start(app: &TempDir, cache: &TempDir, bundler: Arc<FakeBatch>) -> DevServer {
    let mut config = DevConfig::new(app.path());
    config.port = 0;
    config.cache = CacheMode::Dir(cache.path().to_path_buf());
    config.dep_bundler = bundler;
    config.transforms.push(Arc::new(TsAsJs));
    DevServer::start(config).expect("server starts")
}

/// The first `/@ts-r/deps/<name>?v=<version>` URL in `code`.
fn dep_url(code: &str, name: &str) -> String {
    let at = code
        .find(&format!("/@ts-r/deps/{name}?v="))
        .unwrap_or_else(|| panic!("no {name} in:\n{code}"));
    code[at..].split(['\'', '"']).next().unwrap().to_owned()
}

#[test]
fn scans_bundles_once_and_serves_the_batch() {
    let app = TempDir::new("deps-app");
    let cache = TempDir::new("deps-cache");
    write_app(&app);
    let fake = Arc::new(FakeBatch::default());
    let server = start(&app, &cache, Arc::clone(&fake));

    let (status, _, main) = http_get(server.addr(), "/src/main.ts", &[]);
    assert_eq!(status, 200, "{main}");
    // Scan found the static import (twice: main and util) and the dynamic one; sorted.
    assert_eq!(*fake.last.lock().unwrap(), ["alpha", "beta"]);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    let alpha = dep_url(&main, "e0.js");
    let beta = dep_url(&main, "e1.js");
    let v = alpha.split("?v=").nth(1).unwrap().to_owned();
    assert_eq!(beta, format!("/@ts-r/deps/e1.js?v={v}"));
    let (_, _, util) = http_get(server.addr(), "/src/util.js", &[]);
    assert_eq!(dep_url(&util, "e0.js"), alpha);

    let (status, h, entry) = http_get(server.addr(), &alpha, &[]);
    assert_eq!(status, 200);
    assert_eq!(
        header(&h, "cache-control"),
        Some("max-age=31536000, immutable")
    );
    assert!(
        entry.contains(&format!("\"/@ts-r/deps/chunk-S.js?v={v}\"")),
        "{entry}"
    );
    let (status, _, chunk) = http_get(server.addr(), &format!("/@ts-r/deps/chunk-S.js?v={v}"), &[]);
    assert_eq!(status, 200);
    assert!(chunk.contains("shared"));
    let (status, h, _) = http_get(server.addr(), "/@ts-r/deps/e0.js?v=stale", &[]);
    assert_eq!(status, 200);
    assert_eq!(header(&h, "cache-control"), Some("no-cache"));
    assert_eq!(http_get(server.addr(), "/@ts-r/deps/nope.js", &[]).0, 404);
    server.shutdown();

    // Warm start: the batch comes from the cache, no scan or bundling.
    let fake2 = Arc::new(FakeBatch::default());
    let server = start(&app, &cache, Arc::clone(&fake2));
    let (_, _, main2) = http_get(server.addr(), "/src/main.ts", &[]);
    assert_eq!(dep_url(&main2, "e0.js"), alpha);
    assert_eq!(fake2.calls.load(Ordering::SeqCst), 0);
    let (status, _, entry2) = http_get(server.addr(), &alpha, &[]);
    assert_eq!(status, 200);
    assert_eq!(entry2, entry);
    server.shutdown();

    // A lockfile change is a new batch with a new version.
    app.write("package-lock.json", "{}\n");
    let fake3 = Arc::new(FakeBatch::default());
    let server = start(&app, &cache, Arc::clone(&fake3));
    let (_, _, main3) = http_get(server.addr(), "/src/main.ts", &[]);
    assert_eq!(fake3.calls.load(Ordering::SeqCst), 1);
    assert_ne!(dep_url(&main3, "e0.js"), alpha);
    server.shutdown();
}

#[test]
fn a_dependency_missing_from_the_batch_rebatches_and_reloads() {
    let app = TempDir::new("deps-miss-app");
    let cache = TempDir::new("deps-miss-cache");
    write_app(&app);
    let fake = Arc::new(FakeBatch::default());
    let server = start(&app, &cache, Arc::clone(&fake));
    let (_, _, main) = http_get(server.addr(), "/src/main.ts", &[]);
    let old = dep_url(&main, "e0.js");

    let mut ws = WsClient::connect(&server.addr().to_string(), "/@ts-r/ws").unwrap();
    ws.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let hello: Value = serde_json::from_str(&ws.recv_text().unwrap()).unwrap();
    assert_eq!(hello["type"], "connected");
    while server.client_count() == 0 {
        std::thread::sleep(Duration::from_millis(5));
    }

    // `late.js` isn't reachable from index.html, so the scan never saw `gamma`.
    let (status, _, late) = http_get(server.addr(), "/src/late.js", &[]);
    assert_eq!(status, 200, "{late}");
    assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
    assert_eq!(*fake.last.lock().unwrap(), ["alpha", "beta", "gamma"]);
    let gamma = dep_url(&late, "e2.js");
    let new_v = gamma.split("?v=").nth(1).unwrap();
    assert!(!old.ends_with(new_v), "version must change");
    let msg: Value = serde_json::from_str(&ws.recv_text().unwrap()).unwrap();
    assert_eq!(msg["type"], "full-reload");
    // Source modules render with the new batch from now on.
    let (_, _, main2) = http_get(server.addr(), "/src/main.ts", &[]);
    assert_eq!(
        dep_url(&main2, "e0.js"),
        format!("/@ts-r/deps/e0.js?v={new_v}")
    );
    server.shutdown();
}

/// `import.meta.glob` in a pre-bundled dependency is expanded when it is served, against the
/// root (Vite's optimizer keeps the call). The file is not cached for good, and a new match
/// reloads the page.
#[test]
fn globs_in_a_dependency_expand_at_serve_time() {
    let app = TempDir::new("deps-glob-app");
    let cache = TempDir::new("deps-glob-cache");
    write_app(&app);
    app.write("src/pages/a.js", "export default 1;\n");
    let fake = Arc::new(FakeBatch {
        extra: "export const pages = import.meta.glob('/src/pages/*.js');\n".into(),
        ..FakeBatch::default()
    });
    let server = start(&app, &cache, Arc::clone(&fake));
    let (_, _, main) = http_get(server.addr(), "/src/main.ts", &[]);
    let alpha = dep_url(&main, "e0.js");
    let (status, h, entry) = http_get(server.addr(), &alpha, &[]);
    assert_eq!(status, 200, "{entry}");
    assert_eq!(header(&h, "cache-control"), Some("no-cache"));
    assert!(
        entry.contains("\"/src/pages/a.js\": () => import(\"../../src/pages/a.js\")"),
        "{entry}"
    );
    let mut ws = WsClient::connect(&server.addr().to_string(), "/@ts-r/ws").unwrap();
    ws.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let hello: Value = serde_json::from_str(&ws.recv_text().unwrap()).unwrap();
    assert_eq!(hello["type"], "connected");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while server.client_count() == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    app.write("src/pages/b.js", "export default 2;\n");
    loop {
        let m: Value = serde_json::from_str(&ws.recv_text().expect("a reload")).unwrap();
        if m["type"] == "full-reload" {
            break;
        }
    }
    let (_, _, entry) = http_get(server.addr(), &alpha, &[]);
    assert!(entry.contains("\"/src/pages/b.js\""), "{entry}");
    server.shutdown();
}
