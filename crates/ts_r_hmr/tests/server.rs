//! The dev server end to end without a browser: real sockets, the real file watcher, and a
//! WebSocket client standing in for the page.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use ts_r_hmr::testutil::{TempDir, counter_js, header, http_get, write_fixture_app};
use ts_r_hmr::ws::WsClient;
use ts_r_hmr::{CacheMode, DevConfig, DevServer};

struct Fixture {
    app: TempDir,
    _cache: TempDir,
    server: DevServer,
    log: Arc<Mutex<Vec<String>>>,
}

fn start() -> Fixture {
    let app = TempDir::new("srv-app");
    let cache = TempDir::new("srv-cache");
    write_fixture_app(&app);
    let log = Arc::new(Mutex::new(Vec::new()));
    let l = Arc::clone(&log);
    let mut config = DevConfig::new(app.path());
    config.port = 0;
    config.cache = CacheMode::Dir(cache.path().to_path_buf());
    config.logger = Some(Arc::new(move |line: &str| {
        l.lock().unwrap().push(line.to_owned())
    }));
    let server = DevServer::start(config).expect("server starts");
    Fixture {
        app,
        _cache: cache,
        server,
        log,
    }
}

impl Fixture {
    fn get(&self, path: &str) -> (u16, Vec<(String, String)>, String) {
        http_get(self.server.addr(), path, &[])
    }

    /// Requests the modules a browser would, so the graph matches a loaded page.
    fn load_page(&self) {
        for p in [
            "/",
            "/src/main.js",
            "/src/counter.js",
            "/src/style.css?import",
            "/src/plain.js",
            "/src/accepted.js",
            "/src/data.json?import",
            "/src/logo.svg?import",
        ] {
            let (status, _, body) = self.get(p);
            assert_eq!(status, 200, "{p}: {body}");
        }
        let (status, _, _) = http_get(
            self.server.addr(),
            "/src/global.css",
            &[("Accept", "text/css,*/*;q=0.1")],
        );
        assert_eq!(status, 200);
    }

    fn ws(&self) -> WsClient {
        let mut ws = WsClient::connect(&self.server.addr().to_string(), "/@ts-r/ws").unwrap();
        ws.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let hello: Value = serde_json::from_str(&ws.recv_text().unwrap()).unwrap();
        assert_eq!(hello["type"], "connected");
        // Wait until the server registered us before editing files.
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.server.client_count() == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        ws
    }
}

fn next(ws: &mut WsClient) -> Value {
    serde_json::from_str(&ws.recv_text().expect("message before timeout")).unwrap()
}

/// Skips messages until one of `kind` arrives.
fn next_of(ws: &mut WsClient, kind: &str) -> Value {
    loop {
        let m = next(ws);
        if m["type"] == kind {
            return m;
        }
    }
}

#[test]
fn serves_html_modules_deps_and_static_files() {
    let f = start();
    let (status, h, body) = f.get("/");
    assert_eq!(status, 200);
    assert_eq!(header(&h, "content-type"), Some("text/html"));
    assert!(body.contains("<script type=\"module\" src=\"/@ts-r/client\"></script>"));

    let (status, h, main) = f.get("/src/main.js");
    assert_eq!(status, 200, "{main}");
    assert_eq!(
        header(&h, "content-type"),
        Some("text/javascript; charset=utf-8")
    );
    assert_eq!(header(&h, "cache-control"), Some("no-cache"));
    assert!(main.contains("from '/src/counter.js'"), "{main}");
    assert!(main.contains("from '/@modules/tiny-dep?v="), "{main}");
    assert!(main.contains("import '/src/style.css?import'"), "{main}");
    assert!(main.contains("from '/src/data.json?import'"), "{main}");
    assert!(main.contains("from '/src/logo.svg?import'"), "{main}");
    // hot.accept dep rewritten to its URL (no query), hot context injected.
    assert!(
        main.contains("import.meta.hot.accept('/src/accepted.js'"),
        "{main}"
    );
    assert!(main.starts_with("import { createHotContext as __ts_r_hot } from \"/@ts-r/client\";import.meta.hot = __ts_r_hot(\"/src/main.js\");"));

    // ETag → 304 on revalidation.
    let etag = header(&h, "etag").unwrap().to_owned();
    let (status, _, body) = http_get(f.server.addr(), "/src/main.js", &[("If-None-Match", &etag)]);
    assert_eq!((status, body.as_str()), (304, ""));

    // Dependency: rewritten sibling import, immutable when versioned.
    let v = main.split("/@modules/tiny-dep?v=").nth(1).unwrap()[..8].to_owned();
    let (status, h, dep) = f.get(&format!("/@modules/tiny-dep?v={v}"));
    assert_eq!(status, 200, "{dep}");
    assert_eq!(
        header(&h, "cache-control"),
        Some("max-age=31536000, immutable")
    );
    assert!(
        dep.contains(&format!("from '/@modules/tiny-dep/esm/util.js?v={v}'")),
        "{dep}"
    );
    let (status, _, util) = f.get(&format!("/@modules/tiny-dep/esm/util.js?v={v}"));
    assert_eq!(status, 200);
    assert!(util.contains("suffix = 'ok'"));

    // CSS as JS and as CSS.
    let (_, h, css_js) = f.get("/src/style.css?import");
    assert_eq!(
        header(&h, "content-type"),
        Some("text/javascript; charset=utf-8")
    );
    assert!(css_js.contains("__ts_r_update_style") && css_js.contains("rgb(255, 0, 0)"));
    let (_, h, css) = http_get(
        f.server.addr(),
        "/src/global.css",
        &[("Accept", "text/css")],
    );
    assert_eq!(header(&h, "content-type"), Some("text/css; charset=utf-8"));
    assert_eq!(css, "body { background-color: rgb(1, 2, 3); }\n");

    // JSON (named exports, like Vite) / assets (small SVGs inline) / raw / url.
    assert_eq!(
        f.get("/src/data.json?import").2,
        "export const name = \"fixture-data\";\nexport default {\n  name\n};\n"
    );
    assert_eq!(
        f.get("/src/logo.svg?import").2,
        "export default \"data:image/svg+xml,%3csvg%20xmlns='http://www.w3.org/2000/svg'%3e%3c/svg%3e\";\n"
    );
    assert!(
        f.get("/src/logo.svg?import&inline")
            .2
            .contains("data:image/svg+xml,%3csvg")
    );
    assert!(
        f.get("/src/global.css?inline")
            .2
            .starts_with("export default \"body {")
    );
    assert!(
        f.get("/src/plain.js?raw")
            .2
            .starts_with("export default \"export const label")
    );
    let (_, h, html) = f.get("/index.html?raw");
    assert_eq!(
        header(&h, "content-type"),
        Some("text/javascript; charset=utf-8")
    );
    assert!(
        html.starts_with("export default \"<!") && !html.contains("/@ts-r/client"),
        "{html}"
    );
    assert_eq!(
        f.get("/index.html?url").2,
        "export default \"/index.html\";\n"
    );
    let (_, h, svg) = f.get("/src/logo.svg");
    assert_eq!(header(&h, "content-type"), Some("image/svg+xml"));
    assert!(svg.starts_with("<svg"));

    // public/, client, 404, traversal, fallback, stats.
    let (status, h, robots) = f.get("/robots.txt");
    assert_eq!((status, robots.as_str()), (200, "User-agent: *\n"));
    let etag = header(&h, "etag").unwrap().to_owned();
    assert_eq!(
        http_get(f.server.addr(), "/robots.txt", &[("If-None-Match", &etag)]).0,
        304
    );
    let (status, _, client) = f.get("/@ts-r/client");
    assert_eq!(status, 200);
    assert!(client.contains("export function createHotContext"));
    assert_eq!(f.get("/@vite/client").0, 200);
    assert_eq!(f.get("/nope.js").0, 404);
    assert_eq!(f.get("/../../etc/passwd").0, 403);
    assert_eq!(f.get("/@fs/etc/passwd").0, 403);
    let up = "..%2f".repeat(20);
    let (status, _, body) = f.get(&format!("/@modules/tiny-dep/{up}etc/passwd"));
    assert_eq!(status, 404, "{body}");
    let (status, _, html) = http_get(f.server.addr(), "/some/route", &[("Accept", "text/html")]);
    assert!(status == 200 && html.contains("/@ts-r/client"));
    let (status, _, stats) = f.get("/@ts-r/stats");
    assert_eq!(status, 200);
    assert_eq!(serde_json::from_str::<Value>(&stats).unwrap()["count"], 0);
    assert!(f.server.module_count() >= 6);
}

#[test]
fn denied_files_case_html_fallback_and_asset_queries() {
    let f = start();
    std::fs::write(f.app.path().join(".env"), "SECRET=1\n").unwrap();
    std::fs::write(f.app.path().join("about.html"), "<p>about</p>").unwrap();
    assert_eq!(f.get("/.env").0, 403);
    let html = [("Accept", "text/html")];
    let (status, h, body) = http_get(f.server.addr(), "/about", &html);
    assert_eq!(
        (status, header(&h, "content-type")),
        (200, Some("text/html"))
    );
    assert!(body.contains("<p>about</p>"), "{body}");
    // `fetch` accepts `*/*`: it gets the fallback page too.
    let (status, _, body) = http_get(f.server.addr(), "/missing", &[("Accept", "*/*")]);
    assert!(status == 200 && body.contains("/@ts-r/client"), "{body}");
    // A public file matches only in its own case (the disk may not care).
    let (status, _, body) = f.get("/ROBOTS.TXT");
    assert_eq!(status, 404, "{body}");
    let (_, _, js) = f.get("/src/logo.svg?import&no-inline&x=1");
    assert!(js.contains("\"/src/logo.svg?no-inline&x=1\""), "{js}");
    // `//x` is not a file of the root or the public directory: the fallback page.
    let (status, _, body) = http_get(f.server.addr(), "//robots.txt", &html);
    assert!(status == 200 && body.contains("/@ts-r/client"), "{body}");
    // A file without an extension has no type to name.
    std::fs::write(f.app.path().join("LICENSE"), "MIT\n").unwrap();
    let (status, h, _) = f.get("/LICENSE");
    assert_eq!((status, header(&h, "content-type")), (200, Some("")));
    // The client and env modules, under either name and with any query, are the server's
    // own URLs: imports of them are kept, not resolved as files.
    std::fs::write(
        f.app.path().join("src/client-user.js"),
        "import '/@vite/client';\nimport '/@vite/env?x';\n",
    )
    .unwrap();
    let (status, _, js) = f.get("/src/client-user.js");
    assert_eq!(status, 200, "{js}");
    assert!(
        js.contains("import '/@vite/client';\nimport '/@vite/env?x';"),
        "{js}"
    );
    for url in ["/@vite/client", "/@vite/env?x"] {
        assert_eq!(f.get(url).0, 200, "{url}");
    }
}

#[test]
fn keep_alive_serves_several_requests_on_one_connection() {
    use std::io::{BufRead, BufReader, Read, Write};
    let f = start();
    let mut s = std::net::TcpStream::connect(f.server.addr()).unwrap();
    s.write_all(b"GET /src/plain.js HTTP/1.1\r\nHost: x\r\n\r\nHEAD /src/plain.js HTTP/1.1\r\nHost: x\r\n\r\nGET /robots.txt HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
    let mut r = BufReader::new(s);
    let mut statuses = Vec::new();
    for i in 0..3 {
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        statuses.push(line.trim().to_owned());
        let mut len = 0usize;
        loop {
            let mut h = String::new();
            r.read_line(&mut h).unwrap();
            if h == "\r\n" {
                break;
            }
            if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap();
            }
        }
        if i != 1 {
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
        }
    }
    assert_eq!(statuses, ["HTTP/1.1 200 OK"; 3]);
}

#[test]
fn creating_a_missing_import_retries_a_module_that_failed_over_http() {
    let f = start();
    f.app.write(
        "src/needs.js",
        "import { x } from './later.js';\nexport const y = x;\n",
    );
    let (status, _, body) = f.get("/src/needs.js");
    assert_eq!(status, 500, "{body}");
    let mut ws = f.ws();
    f.app.write("src/later.js", "export const x = 1;\n");
    f.server.notify_changes(ts_r_hmr::ChangeBatch {
        changes: vec![ts_r_hmr::FileChange {
            path: f.app.path().join("src/later.js"),
            kind: ts_r_hmr::ChangeKind::Created,
            first_event: Instant::now(),
            first_event_wall: std::time::SystemTime::now(),
        }],
        flushed: Instant::now(),
    });
    // Not an `error`: the retry prepared the module, so the page reloads or updates.
    let m = loop {
        let m = next(&mut ws);
        if m["type"] != "error" {
            break m;
        }
    };
    assert!(m["type"] == "full-reload" || m["type"] == "update", "{m}");
    assert_eq!(f.get("/src/needs.js").0, 200);
}

#[test]
fn a_pruned_module_imported_again_is_fetched_fresh() {
    let f = start();
    f.app.write("src/child.js", "export const v = 1;\n");
    let with_child =
        "import { v } from './child.js';\nconsole.log(v);\nimport.meta.hot.accept();\n";
    f.app.write("src/parent.js", with_child);
    let (status, _, body) = f.get("/src/parent.js");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("from '/src/child.js'"), "{body}");
    assert_eq!(f.get("/src/child.js").0, 200);
    let mut ws = f.ws();
    // Dropping the import prunes child.js; the prune carries the update's timestamp.
    f.app.write("src/parent.js", "import.meta.hot.accept();\n");
    let update = next_of(&mut ws, "update");
    let t = update["updates"][0]["timestamp"].as_u64().unwrap();
    assert_eq!(next_of(&mut ws, "prune")["paths"][0], "/src/child.js");
    // Restoring it imports child.js with that `?t=`, so the browser doesn't reuse the
    // disposed instance.
    f.app.write("src/parent.js", with_child);
    next_of(&mut ws, "update");
    let (_, _, body) = f.get("/src/parent.js");
    assert!(
        body.contains(&format!("from '/src/child.js?t={t}'")),
        "{body}"
    );
}

#[test]
fn accepting_a_raw_import_keeps_it_raw_after_an_update() {
    let f = start();
    f.app.write("src/message.txt", "hello");
    f.app.write(
        "src/uses.js",
        "import text from './message.txt?raw';\nexport let current = text;\nimport.meta.hot.accept('./message.txt?raw', (m) => { current = m.default; });\n",
    );
    let (status, _, body) = f.get("/src/uses.js");
    assert_eq!(status, 200, "{body}");
    // Assets always get `import` (as Vite rewrites them); `raw` stays.
    assert!(
        body.contains("from '/src/message.txt?import&raw'"),
        "{body}"
    );
    assert!(
        body.contains("import.meta.hot.accept('/src/message.txt?raw'"),
        "{body}"
    );
    assert_eq!(f.get("/src/message.txt?raw").0, 200);
    let mut ws = f.ws();
    f.app.write("src/message.txt", "bye");
    f.server.notify_changes(ts_r_hmr::ChangeBatch {
        changes: vec![ts_r_hmr::FileChange {
            path: f.app.path().join("src/message.txt"),
            kind: ts_r_hmr::ChangeKind::Modified,
            first_event: Instant::now(),
            first_event_wall: std::time::SystemTime::now(),
        }],
        flushed: Instant::now(),
    });
    let m = next_of(&mut ws, "update");
    let u = &m["updates"][0];
    assert_eq!(u["path"], "/src/uses.js", "{m}");
    assert_eq!(u["acceptedPath"], "/src/message.txt?raw", "{m}");
    // What the client imports for that update: `?import&t=…&raw`.
    let (_, _, fresh) = f.get(&format!("/src/message.txt?import&t={}&raw", u["timestamp"]));
    assert_eq!(fresh, "export default \"bye\";\n");
}

#[test]
fn hot_updates_over_websocket() {
    let f = start();
    f.load_page();
    let mut ws = f.ws();

    // 1. Self-accepting module, saved atomically (temp file + rename).
    f.app.write_atomic("src/counter.js", &counter_js("v2", ""));
    let m = next_of(&mut ws, "update");
    let u = &m["updates"][0];
    assert_eq!(m["updates"].as_array().unwrap().len(), 1, "{m}");
    assert_eq!(u["type"], "js-update");
    assert_eq!(u["path"], "/src/counter.js");
    assert_eq!(u["acceptedPath"], "/src/counter.js");
    let t = u["timestamp"].as_u64().unwrap();
    // The re-fetched module, and main.js's import of it, now carry ?t=.
    assert!(f.get(&format!("/src/counter.js?t={t}")).2.contains("'v2'"));
    assert!(
        f.get("/src/main.js")
            .2
            .contains(&format!("from '/src/counter.js?t={t}'"))
    );

    // The browser's acknowledgement completes the latency record.
    let id = m["tsrId"].as_u64().unwrap();
    ws.send_text(&format!(
        r#"{{"type":"custom","event":"ts-r:applied","data":{{"id":{id},"ms":3.5}}}}"#
    ))
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while f.server.stats_json()["count"] != 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let stats = f.server.stats_json();
    assert_eq!(stats["count"], 1, "{stats}");
    assert_eq!(stats["browser"]["p50"], 3.5);
    assert!(
        f.log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("hmr /src/counter.js js-update · watch"))
    );

    // 2. Saving identical content (a touch or double write) sends nothing; the next real
    //    change is the next message.
    f.app.write("src/counter.js", &counter_js("v2", ""));
    std::thread::sleep(Duration::from_millis(50));
    // 3. Dependency accepted by its importer.
    f.app.write(
        "src/accepted.js",
        "export const accepted = 'accepted-v2';\n",
    );
    let m = next(&mut ws);
    assert_eq!(m["type"], "update", "{m}");
    assert_eq!(m["updates"][0]["path"], "/src/main.js");
    assert_eq!(m["updates"][0]["acceptedPath"], "/src/accepted.js");

    // 4. CSS imported from JS → js-update with explicit import; linked CSS → css-update.
    f.app
        .write("src/style.css", "#app { color: rgb(0, 0, 255); }\n");
    let m = next_of(&mut ws, "update");
    assert_eq!(m["updates"][0]["acceptedPath"], "/src/style.css");
    assert_eq!(m["updates"][0]["explicitImportRequired"], true);
    f.app.write(
        "src/global.css",
        "body { background-color: rgb(4, 5, 6); }\n",
    );
    let m = next_of(&mut ws, "update");
    assert_eq!(m["updates"][0]["type"], "css-update");
    assert_eq!(m["updates"][0]["path"], "/src/global.css");

    // 5. Broken import → error overlay payload; fixing it → update.
    f.app.write(
        "src/counter.js",
        &counter_js("v3", "import './missing.js';"),
    );
    let m = next_of(&mut ws, "error");
    assert!(
        m["err"]["message"].as_str().unwrap().contains("missing.js"),
        "{m}"
    );
    assert_eq!(m["err"]["loc"]["line"], 1);
    assert!(
        m["err"]["frame"]
            .as_str()
            .unwrap()
            .contains("> 1 | import './missing.js';")
    );
    // A page opened now sees the error too.
    let mut late = f.ws();
    assert_eq!(next(&mut late)["type"], "error");
    f.app.write("src/missing.js", "export {};\n");
    let m = next_of(&mut ws, "update");
    assert_eq!(m["updates"][0]["path"], "/src/counter.js");

    // 6. Removing an import prunes the orphaned module.
    f.app.write("src/counter.js", &counter_js("v4", ""));
    let m = next_of(&mut ws, "update");
    assert_eq!(m["updates"][0]["path"], "/src/counter.js");
    let m = next_of(&mut ws, "prune");
    assert_eq!(m["paths"][0], "/src/missing.js");
    // A pruned module is no longer on the page: its changes (or a late watcher event from
    // before the prune) don't reload it.
    f.app.write("src/missing.js", "export const late = 1;\n");
    std::thread::sleep(Duration::from_millis(100));

    // 7. No boundary → full reload.
    f.app
        .write("src/plain.js", "export const label = 'plain-v2';\n");
    assert_eq!(next_of(&mut ws, "full-reload")["type"], "full-reload");

    // 8. invalidate() from the client hands the update to importers (main.js has no
    //    self-accept → full reload).
    ws.send_text(r#"{"type":"custom","event":"vite:invalidate","data":{"path":"/src/counter.js","message":"test"}}"#)
        .unwrap();
    assert_eq!(next_of(&mut ws, "full-reload")["type"], "full-reload");
    //    main.js accepts accepted.js: its update names the module that started the chain.
    ws.send_text(
        r#"{"type":"custom","event":"vite:invalidate","data":{"path":"/src/accepted.js"}}"#,
    )
    .unwrap();
    let m = next_of(&mut ws, "update");
    assert_eq!(m["updates"][0]["path"], "/src/main.js", "{m}");
    assert_eq!(m["updates"][0]["firstInvalidatedBy"], "/src/accepted.js");
    //    An invalidation from inside that update that would re-run the module that started
    //    it is a loop: reload instead.
    ws.send_text(r#"{"type":"custom","event":"vite:invalidate","data":{"path":"/src/accepted.js","firstInvalidatedBy":"/src/main.js"}}"#)
        .unwrap();
    assert_eq!(next_of(&mut ws, "full-reload")["type"], "full-reload");

    // 9. HTML change → full reload with its path.
    f.app.write(
        "index.html",
        &f.app.read("index.html").replace("fixture", "fixture 2"),
    );
    let m = next_of(&mut ws, "full-reload");
    assert_eq!(m["path"], "/index.html");

    // 10. Deleting the module pruned in an earlier batch: nothing accepts it, so the page
    //     reloads (Vite keeps it as a module with no importers, a dead end).
    std::fs::remove_file(f.app.path().join("src/missing.js")).unwrap();
    assert_eq!(next_of(&mut ws, "full-reload")["type"], "full-reload");
    let log = f.log.lock().unwrap();
    assert!(
        log.iter()
            .any(|l| l.contains("/src/missing.js was deleted and nothing imports it")),
        "{log:#?}"
    );
    assert!(
        !log.iter().any(|l| l.contains("missing.js changed")),
        "{log:#?}"
    );
}

#[test]
fn transform_cache_survives_restart() {
    let app = TempDir::new("srv-restart");
    let cache = TempDir::new("srv-restart-cache");
    write_fixture_app(&app);
    for round in 0..2 {
        let mut config = DevConfig::new(app.path());
        config.port = 0;
        config.watch = false;
        config.cache = CacheMode::Dir(cache.path().to_path_buf());
        let server = DevServer::start(config).unwrap();
        assert_eq!(http_get(server.addr(), "/src/main.js", &[]).0, 200);
        server.shutdown();
        let entries = std::fs::read_dir(cache.path().join("dev-transform")).map(|d| d.count());
        assert!(entries.unwrap_or(0) > 0, "round {round}: cache written");
    }
}

/// A transform whose output reads another file: `/*inline:<name>*/` becomes that file's
/// text (like `parse<T>` validators generated from a type-only import). A file whose text is
/// `bad` fails the transform (like a type `parse<T>` can't check).
#[derive(Debug)]
struct Inlining;

impl ts_r_hmr::Transform for Inlining {
    fn cache_id(&self) -> &str {
        "inlining/1"
    }

    fn handles(&self, path: &std::path::Path) -> bool {
        path.to_string_lossy().ends_with(".inl.js")
    }

    fn transform(
        &self,
        input: &ts_r_hmr::TransformInput<'_>,
    ) -> Result<ts_r_hmr::TransformOutput, ts_r_hmr::TransformError> {
        let mut code = input.source.to_owned();
        for d in self.deps(input) {
            let name = d.file_name().unwrap_or_default().to_string_lossy();
            let text = std::fs::read_to_string(&d).unwrap_or_default();
            if text.trim() == "bad" {
                return Err(ts_r_hmr::TransformError {
                    message: format!("{name} is bad"),
                    line: None,
                    column: None,
                });
            }
            code = code.replace(&format!("/*inline:{name}*/"), text.trim());
        }
        let imports = ts_r_hmr::scan::scan(&code).imports;
        Ok(ts_r_hmr::TransformOutput {
            code,
            map: None,
            imports,
        })
    }

    fn deps(&self, input: &ts_r_hmr::TransformInput<'_>) -> Vec<std::path::PathBuf> {
        let dir = input.path.parent().unwrap_or(input.path);
        input
            .source
            .split("/*inline:")
            .skip(1)
            .filter_map(|s| s.split("*/").next())
            .map(|n| dir.join(n))
            .collect()
    }
}

fn inlining_server(app: &TempDir, cache: &TempDir, watch: bool) -> DevServer {
    let mut config = DevConfig::new(app.path());
    config.port = 0;
    config.watch = watch;
    config.cache = CacheMode::Dir(cache.path().to_path_buf());
    config.transforms.insert(0, Arc::new(Inlining));
    DevServer::start(config).expect("server starts")
}

#[test]
fn a_file_a_transform_reads_hot_updates_its_module() {
    let app = TempDir::new("srv-deps");
    let cache = TempDir::new("srv-deps-cache");
    write_fixture_app(&app);
    app.write("src/types.txt", "one\n");
    app.write(
        "src/typed.inl.js",
        "export const v = '/*inline:types.txt*/';\nif (import.meta.hot) import.meta.hot.accept();\n",
    );
    let server = inlining_server(&app, &cache, true);
    let (status, _, body) = http_get(server.addr(), "/src/typed.inl.js", &[]);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("'one'"), "{body}");
    let mut ws = WsClient::connect(&server.addr().to_string(), "/@ts-r/ws").unwrap();
    ws.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    assert_eq!(next(&mut ws)["type"], "connected");
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.client_count() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    // types.txt is not in the module graph; the module that read it is updated.
    app.write_atomic("src/types.txt", "two\n");
    let m = next_of(&mut ws, "update");
    assert_eq!(m["updates"][0]["path"], "/src/typed.inl.js", "{m}");
    let t = m["updates"][0]["timestamp"].as_u64().unwrap();
    let (_, _, body) = http_get(server.addr(), &format!("/src/typed.inl.js?t={t}"), &[]);
    assert!(body.contains("'two'"), "{body}");
    server.shutdown();
    // The cache key covers the read file: a restart serves the current text, not a stale hit.
    app.write("src/types.txt", "three\n");
    let server = inlining_server(&app, &cache, false);
    let (_, _, body) = http_get(server.addr(), "/src/typed.inl.js", &[]);
    assert!(body.contains("'three'"), "{body}");
    server.shutdown();
}

/// A linked workspace package outside the root (`node_modules/ui-lib` →
/// `../../packages/ui-lib`, as pnpm `workspace:` and `npm link` do) is source: served via
/// `/@fs/`, watched, and hot-updated, both its JS and the CSS it ships.
#[cfg(unix)]
#[test]
fn linked_package_outside_the_root_is_watched() {
    let ws_dir = TempDir::new("srv-linked");
    let cache = TempDir::new("srv-linked-cache");
    ws_dir.write(
        "app/index.html",
        "<!doctype html><script type=\"module\" src=\"/src/main.js\"></script>\n",
    );
    ws_dir.write(
        "app/src/main.js",
        "import { label } from 'ui-lib';\nimport 'ui-lib/style.css';\nconsole.log(label);\n",
    );
    ws_dir.write(
        "packages/ui-lib/package.json",
        r#"{ "name": "ui-lib", "type": "module", "exports": { ".": "./src/index.js", "./style.css": "./src/style.css" } }"#,
    );
    let lib_js = |v: &str| {
        format!("export const label = '{v}';\nif (import.meta.hot) import.meta.hot.accept();\n")
    };
    ws_dir.write("packages/ui-lib/src/index.js", &lib_js("v1"));
    ws_dir.write("packages/ui-lib/src/style.css", ".ui { color: red; }\n");
    std::fs::create_dir_all(ws_dir.path().join("app/node_modules")).unwrap();
    std::os::unix::fs::symlink(
        "../../packages/ui-lib",
        ws_dir.path().join("app/node_modules/ui-lib"),
    )
    .unwrap();

    let log = Arc::new(Mutex::new(Vec::new()));
    let l = Arc::clone(&log);
    let mut config = DevConfig::new(ws_dir.path().join("app"));
    config.port = 0;
    config.cache = CacheMode::Dir(cache.path().to_path_buf());
    config.logger = Some(Arc::new(move |line: &str| {
        l.lock().unwrap().push(line.to_owned())
    }));
    let server = DevServer::start(config).expect("server starts");
    let (status, _, main) = http_get(server.addr(), "/src/main.js", &[]);
    assert_eq!(status, 200, "{main}");
    let lib = ws_dir.path().join("packages/ui-lib/src/index.js");
    let lib_url = format!("/@fs{}", lib.display());
    let css_url = format!(
        "/@fs{}",
        ws_dir
            .path()
            .join("packages/ui-lib/src/style.css")
            .display()
    );
    assert!(main.contains(&format!("'{lib_url}'")), "{main}");
    assert!(main.contains(&format!("'{css_url}?import'")), "{main}");
    for u in [lib_url.clone(), format!("{css_url}?import")] {
        let (status, _, body) = http_get(server.addr(), &u, &[]);
        assert_eq!(status, 200, "{u}: {body}");
    }
    let f = Fixture {
        app: ws_dir,
        _cache: cache,
        server,
        log,
    };
    let mut ws = f.ws();
    // The watch on the package directory is added asynchronously, and under load the native
    // watcher can start late and miss the first write. Rewrite the file (with a changing
    // comment) until an update arrives.
    ws.set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut attempt = 0;
    let m = loop {
        attempt += 1;
        f.app.write_atomic(
            "packages/ui-lib/src/index.js",
            &format!("{}// {attempt}\n", lib_js("v2")),
        );
        let got = loop {
            let Ok(text) = ws.recv_text() else { break None };
            let m: Value = serde_json::from_str(&text).unwrap();
            if m["type"] == "update" {
                break Some(m);
            }
        };
        match got {
            Some(m) => break m,
            None => assert!(
                Instant::now() < deadline,
                "no update for the linked package"
            ),
        }
    };
    ws.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    assert_eq!(m["updates"][0]["path"], lib_url.as_str(), "{m}");
    let t = m["updates"][0]["timestamp"].as_u64().unwrap();
    assert!(f.get(&format!("{lib_url}?t={t}")).2.contains("'v2'"));
    f.app
        .write("packages/ui-lib/src/style.css", ".ui { color: blue; }\n");
    let m = next_of(&mut ws, "update");
    assert_eq!(m["updates"][0]["acceptedPath"], css_url.as_str(), "{m}");
    let log = f.log.lock().unwrap().clone();
    assert!(
        log.iter()
            .any(|l| l.starts_with("watching ") && l.contains("packages/ui-lib")),
        "{log:?}"
    );
}

#[test]
fn a_read_file_that_recovers_clears_the_error() {
    let app = TempDir::new("srv-deps-recover");
    let cache = TempDir::new("srv-deps-recover-cache");
    write_fixture_app(&app);
    app.write("src/types.txt", "one\n");
    app.write(
        "src/typed.inl.js",
        "export const v = '/*inline:types.txt*/';\nif (import.meta.hot) import.meta.hot.accept();\n",
    );
    let server = inlining_server(&app, &cache, false);
    let (status, _, body) = http_get(server.addr(), "/src/typed.inl.js", &[]);
    assert_eq!(status, 200, "{body}");
    let mut ws = WsClient::connect(&server.addr().to_string(), "/@ts-r/ws").unwrap();
    ws.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    assert_eq!(next(&mut ws)["type"], "connected");
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.client_count() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let changed = |text: &str| {
        app.write("src/types.txt", text);
        server.notify_changes(ts_r_hmr::ChangeBatch {
            changes: vec![ts_r_hmr::FileChange {
                path: app.path().join("src/types.txt"),
                kind: ts_r_hmr::ChangeKind::Modified,
                first_event: Instant::now(),
                first_event_wall: std::time::SystemTime::now(),
            }],
            flushed: Instant::now(),
        });
    };
    changed("bad\n");
    let m = next(&mut ws);
    assert_eq!(m["type"], "error", "{m}");
    // Back to the text the last good output came from: same output, but the error is gone.
    changed("one\n");
    let m = next(&mut ws);
    assert_eq!(m["type"], "update", "{m}");
    assert_eq!(m["updates"][0]["path"], "/src/typed.inl.js", "{m}");
    server.shutdown();
}

#[test]
fn virtual_module_imports_keep_queries_and_reach_files_outside_the_root() {
    let base = TempDir::new("srv-virtual");
    let app = base.path().join("app");
    let linked = base.path().join("linked");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::create_dir_all(&linked).unwrap();
    std::fs::write(app.join("index.html"), "<!doctype html>\n").unwrap();
    std::fs::write(app.join("message.txt"), "hello").unwrap();
    std::fs::write(linked.join("index.js"), "export const l = 1;\n").unwrap();
    let mut config = DevConfig::new(&app);
    config.port = 0;
    config.watch = false;
    config.cache = CacheMode::Memory;
    config.virtual_modules.push((
        "/@ts-r/virtual".to_owned(),
        "import t from './message.txt?raw';\nimport { l } from '../linked/index.js';\nexport { t, l };\n"
            .to_owned(),
    ));
    let server = DevServer::start(config).unwrap();
    let (status, _, body) = http_get(server.addr(), "/@ts-r/virtual", &[]);
    assert_eq!(status, 200, "{body}");
    let urls: Vec<String> = ts_r_hmr::scan::scan(&body)
        .imports
        .into_iter()
        .map(|r| r.specifier)
        .collect();
    assert_eq!(urls.len(), 2, "{body}");
    // The query survives (after the `import` assets always get): the text arrives as a module.
    assert!(urls[0].ends_with("message.txt?import&raw"), "{body}");
    let (status, _, raw) = http_get(server.addr(), &urls[0], &[]);
    assert_eq!(status, 200, "{raw}");
    assert!(raw.contains("export default \"hello\""), "{raw}");
    // A file outside the root is registered, so `/@fs/` serves it.
    assert!(urls[1].starts_with("/@fs/"), "{body}");
    let (status, _, lib) = http_get(server.addr(), &urls[1], &[]);
    assert_eq!(status, 200, "{lib}");
    assert!(lib.contains("export const l = 1"), "{lib}");
    server.shutdown();
}

/// An inline `<script type="module">` is served as its own module from the page's
/// `?html-proxy&index=N.js` URL: imports resolved from the page's directory, `import.meta`
/// replaced, and a nested page's scripts numbered on their own.
#[test]
fn inline_module_scripts_are_served_through_html_proxy_urls() {
    let app = TempDir::new("srv-proxy");
    let cache = TempDir::new("srv-proxy-cache");
    app.write(
        "index.html",
        "<!doctype html><body><script type=\"module\">import { a } from './src/a.js';\ndocument.body.dataset.mode = import.meta.env.MODE + a;</script></body>\n",
    );
    app.write(
        "nested/page.html",
        "<script type=\"module\" src=\"./x.js\"></script>\n<script type=\"module\">import './x.js'</script>\n",
    );
    app.write("src/a.js", "export const a = 1;\n");
    app.write("nested/x.js", "export {};\n");
    let mut config = DevConfig::new(app.path());
    config.port = 0;
    config.cache = CacheMode::Dir(cache.path().to_path_buf());
    let server = DevServer::start(config).unwrap();
    let addr = server.addr();
    let (_, _, html) = http_get(addr, "/", &[]);
    assert!(
        html.contains(
            "<script type=\"module\" src=\"/index.html?html-proxy&index=0.js\"></script>"
        ),
        "{html}"
    );
    let (status, headers, js) = http_get(addr, "/index.html?html-proxy&index=0.js", &[]);
    assert_eq!(status, 200, "{js}");
    assert!(
        header(&headers, "content-type").is_some_and(|c| c.contains("javascript")),
        "{headers:?}"
    );
    assert!(js.contains("from '/src/a.js'"), "{js}");
    assert!(js.contains("\"MODE\": \"development\""), "{js}");
    let (_, _, html) = http_get(addr, "/nested/page.html", &[]);
    assert!(
        html.contains("src=\"/nested/page.html?html-proxy&index=0.js\""),
        "{html}"
    );
    let (_, _, js) = http_get(addr, "/nested/page.html?html-proxy&index=0.js", &[]);
    assert!(js.contains("import '/nested/x.js'"), "{js}");
    assert_eq!(
        http_get(addr, "/index.html?html-proxy&index=5.js", &[]).0,
        404
    );
    server.shutdown();
}
