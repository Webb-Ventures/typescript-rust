//! The plugin seam with an in-process [`PluginHost`]: where each hook runs, plugin
//! modules (virtual ids, ids with a query), hot updates of the files behind them,
//! `handleHotUpdate` narrowing, middlewares and broadcasting.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use ts_r_hmr::http::{Request, Response};
use ts_r_hmr::paths;
use ts_r_hmr::testutil::{TempDir, http_get};
use ts_r_hmr::ws::WsClient;
use ts_r_hmr::{
    CacheMode, DevConfig, DevServer, Loaded, PluginHooks, PluginHost, PluginResolution,
    ServerHandle, Stage, Transformed,
};

#[derive(Debug, Default)]
struct Host {
    server: Mutex<Option<ServerHandle>>,
    hot_calls: Mutex<Vec<(String, Vec<String>)>>,
    client_events: Mutex<Vec<String>>,
}

impl PluginHost for Host {
    fn cache_id(&self) -> &str {
        "test-host/1"
    }

    fn hooks(&self) -> PluginHooks {
        PluginHooks {
            resolve_pre: true,
            resolve_post: true,
            load: true,
            load_files: false,
            transform_pre: true,
            transform_post: true,
            transform_late: false,
            index_html: true,
            hot_update: true,
            middleware: true,
            client_events: true,
        }
    }

    fn attach(&self, server: ServerHandle) {
        *self.server.lock().unwrap() = Some(server);
    }

    fn resolve_ids(
        &self,
        stage: Stage,
        _importer: &str,
        specifiers: &[String],
    ) -> Result<Vec<Option<PluginResolution>>, String> {
        Ok(specifiers
            .iter()
            .map(|s| match (stage, s.as_str()) {
                (Stage::Pre, "virtual:x") => Some(PluginResolution::Id("\0virtual:x".into())),
                (Stage::Pre, "#lib") => Some(PluginResolution::Rewrite("./lib.js".into())),
                // Only reached because the built-in resolver failed.
                (Stage::Post, "missing-pkg") => Some(PluginResolution::External(
                    "https://cdn.example/missing.js".into(),
                )),
                _ => None,
            })
            .collect())
    }

    fn load(&self, id: &str) -> Result<Option<String>, String> {
        Ok((id == "\0virtual:x").then(|| "export const v = 'virtual';\n".to_owned()))
    }

    fn transform(&self, stage: Stage, id: &str, code: &str) -> Result<Option<Transformed>, String> {
        Ok(match stage {
            // Pre: sees the source (TS syntax would still be there).
            Stage::Pre if id.ends_with("main.js") => Some(format!("{code}export const pre = 1;\n")),
            Stage::Post if id.ends_with("?upper") => Some(format!(
                "export default {};\n",
                serde_json::to_string(&code.trim().to_uppercase()).unwrap()
            )),
            Stage::Post if code.contains("__X__") => Some(code.replace("__X__", "42")),
            _ => None,
        }
        .map(Transformed::code))
    }

    fn transform_index_html(&self, _url: &str, _file: &Path, html: &str) -> Result<String, String> {
        Ok(html.replace("<body>", "<body><!-- plugged -->"))
    }

    fn handle_hot_update(
        &self,
        file: &Path,
        module_urls: &[String],
    ) -> Result<Option<Vec<String>>, String> {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        self.hot_calls
            .lock()
            .unwrap()
            .push((name.clone(), module_urls.to_vec()));
        if name == "skip.js" {
            // The file's text ties the message to the write that caused it.
            let text = std::fs::read_to_string(file).unwrap_or_default();
            let msg =
                serde_json::json!({"type": "custom", "event": "skipped", "data": {"text": text}});
            if let Some(s) = self.server.lock().unwrap().as_ref() {
                (s.broadcast)(&msg.to_string());
            }
            return Ok(Some(Vec::new()));
        }
        Ok(None)
    }

    fn client_event(&self, event: &str, _data: &Value) {
        self.client_events.lock().unwrap().push(event.to_owned());
    }

    fn middleware(&self, req: &Request, body: &[u8]) -> Result<Option<Response>, String> {
        Ok((req.path == "/api/x").then(|| {
            Response::text(200, format!("mw {} {}", req.method, body.len()))
                .header_owned("X-Plugin".to_owned(), "yes")
        }))
    }
}

/// Waits (10 s at most) until `handleHotUpdate` ran for `file` with exactly `urls`.
fn wait_for_hot_call(host: &Host, file: &str, urls: &[String]) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let calls = host.hot_calls.lock().unwrap().clone();
        if calls.iter().any(|(f, u)| f == file && u == urls) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no handleHotUpdate({file}, {urls:?}): {calls:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Waits (10 s at most) until the server reported `event` to the plugin host.
fn wait_for_client_event(host: &Host, event: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !host
        .client_events
        .lock()
        .unwrap()
        .iter()
        .any(|e| e == event)
    {
        assert!(Instant::now() < deadline, "no {event}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Fetches `url` until it returns `want` (10 s at most): the change may still be in flight.
fn wait_for_body(addr: std::net::SocketAddr, url: &str, want: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let body = http_get(addr, url, &[]).2;
        if body == want {
            return;
        }
        assert!(Instant::now() < deadline, "{url}: {body}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn next_of(ws: &mut WsClient, kind: &str) -> Value {
    loop {
        let m: Value = serde_json::from_str(&ws.recv_text().expect("message")).unwrap();
        if m["type"] == kind {
            return m;
        }
    }
}

#[test]
fn plugin_hooks_run_at_their_points() {
    let app = TempDir::new("plug-app");
    let cache = TempDir::new("plug-cache");
    app.write(
        "index.html",
        "<!doctype html><body><script type=\"module\" src=\"/src/main.js\"></script></body>\n",
    );
    app.write(
        "src/main.js",
        "import { v } from 'virtual:x';\nimport text from './a.data?upper';\nimport { lib } from '#lib';\nimport m from 'missing-pkg';\nimport './skip.js';\nexport const x = __X__;\nimport.meta.hot.accept();\n",
    );
    app.write("src/a.data", "hello\n");
    app.write("src/lib.js", "export const lib = 1;\n");
    app.write("src/skip.js", "export {};\n");
    let host = Arc::new(Host::default());
    let mut config = DevConfig::new(app.path());
    config.port = 0;
    config.cache = CacheMode::Dir(cache.path().to_path_buf());
    config.plugins = Some(Arc::clone(&host) as Arc<dyn PluginHost>);
    let server = DevServer::start(config).unwrap();
    let addr = server.addr();

    let (_, _, html) = http_get(addr, "/", &[]);
    assert!(html.contains("<body><!-- plugged -->"), "{html}");

    let (status, _, main) = http_get(addr, "/src/main.js", &[]);
    assert_eq!(status, 200, "{main}");
    assert!(main.contains("from '/@id/__x00__virtual:x'"), "{main}");
    assert!(main.contains("from '/src/lib.js'"), "{main}");
    assert!(
        main.contains("from 'https://cdn.example/missing.js'"),
        "{main}"
    );
    assert!(main.contains("export const x = 42;"), "{main}");
    assert!(main.contains("export const pre = 1;"), "{main}");
    let data_url = main
        .split('\'')
        .find(|s| s.starts_with("/@id/") && s.contains("a.data"))
        .unwrap()
        .to_owned();
    assert!(data_url.ends_with("a.data%3Fupper"), "{data_url}");
    assert!(
        http_get(addr, "/@id/__x00__virtual:x", &[])
            .2
            .contains("'virtual'")
    );
    assert_eq!(
        http_get(addr, &data_url, &[]).2,
        "export default \"HELLO\";\n"
    );
    // Load every module the page would, so the graph matches a loaded page: FSEvents can
    // report the writes above after the watcher started, and only a prepared module drops
    // such an event as unchanged (an unloaded lib.js would send an update through main.js).
    assert_eq!(http_get(addr, "/src/lib.js", &[]).0, 200);
    assert_eq!(http_get(addr, "/src/skip.js", &[]).0, 200);
    let (status, headers, body) = http_get(addr, "/api/x", &[]);
    assert_eq!((status, body.as_str()), (200, "mw GET 0"));
    assert_eq!(
        ts_r_hmr::testutil::header(&headers, "x-plugin"),
        Some("yes")
    );

    let mut ws = WsClient::connect(&addr.to_string(), "/@ts-r/ws").unwrap();
    ws.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.client_count() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    // A page's socket opening reaches `server.ws.on('vite:client:connect')`.
    wait_for_client_event(&host, "vite:client:connect");
    // The file behind `a.data?upper` changes: the plugin module is re-loaded and updated
    // through main.js (self-accepting). Each step waits for its own effects rather than taking
    // the next message as its result, so a stray event can't stand in for the write.
    host.hot_calls.lock().unwrap().clear();
    app.write("src/a.data", "bye\n");
    wait_for_hot_call(&host, "a.data", std::slice::from_ref(&data_url));
    let m = next_of(&mut ws, "update");
    assert_eq!(m["updates"][0]["path"], "/src/main.js", "{m}");
    wait_for_body(addr, &data_url, "export default \"BYE\";\n");
    // handleHotUpdate returning [] handles the change: only the plugin's own message.
    host.hot_calls.lock().unwrap().clear();
    let changed = "export const changed = 1;\n";
    app.write("src/skip.js", changed);
    wait_for_hot_call(&host, "skip.js", &["/src/skip.js".to_owned()]);
    loop {
        let m = next_of(&mut ws, "custom");
        if m["event"] == "skipped" && m["data"]["text"] == changed {
            break;
        }
    }
    drop(ws);
    wait_for_client_event(&host, "vite:client:disconnect");
    server.shutdown();
}

/// A plugin module backed by a file outside the root (`../shared/x.data?upper`) has that
/// file's directory added to the watcher, as a plain import outside the root does.
#[test]
fn plugin_module_files_outside_the_root_are_watched() {
    let dir = TempDir::new("plug-outside");
    let cache = TempDir::new("plug-outside-cache");
    dir.write(
        "app/src/main.js",
        "import text from '../../shared/x.data?upper';\nconsole.log(text);\n",
    );
    dir.write("shared/x.data", "hello\n");
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let l = Arc::clone(&log);
    let mut config = DevConfig::new(dir.path().join("app"));
    config.port = 0;
    config.cache = CacheMode::Dir(cache.path().to_path_buf());
    config.plugins = Some(Arc::new(Host::default()) as Arc<dyn PluginHost>);
    config.logger = Some(Arc::new(move |line: &str| {
        l.lock().unwrap().push(line.to_owned())
    }));
    let server = DevServer::start(config).unwrap();
    let addr = server.addr();
    let (status, _, main) = http_get(addr, "/src/main.js", &[]);
    assert_eq!(status, 200, "{main}");
    let data_url = main
        .split('\'')
        .find(|s| s.starts_with("/@id/") && s.contains("x.data"))
        .unwrap()
        .to_owned();
    assert_eq!(
        http_get(addr, &data_url, &[]).2,
        "export default \"HELLO\";\n"
    );
    let shared = dir.path().join("shared");
    let log = log.lock().unwrap().clone();
    assert!(
        log.iter()
            .any(|l| l.starts_with("watching ") && l.contains(&*paths::id(&shared))),
        "{log:?}"
    );
    server.shutdown();
}

/// Counts its `load` calls: each load of `\0virtual:n` exports the next number.
#[derive(Debug, Default)]
struct CountingHost {
    loads: Mutex<u32>,
    server: Mutex<Option<ServerHandle>>,
}

impl PluginHost for CountingHost {
    fn cache_id(&self) -> &str {
        "counting-host/1"
    }

    fn hooks(&self) -> PluginHooks {
        PluginHooks {
            resolve_pre: true,
            load: true,
            ..PluginHooks::default()
        }
    }

    fn attach(&self, server: ServerHandle) {
        *self.server.lock().unwrap() = Some(server);
    }

    fn resolve_ids(
        &self,
        _stage: Stage,
        _importer: &str,
        specifiers: &[String],
    ) -> Result<Vec<Option<PluginResolution>>, String> {
        Ok(specifiers
            .iter()
            .map(|s| (s == "virtual:n").then(|| PluginResolution::Id("\0virtual:n".into())))
            .collect())
    }

    fn load(&self, id: &str) -> Result<Option<String>, String> {
        if id != "\0virtual:n" {
            return Ok(None);
        }
        let mut n = self.loads.lock().unwrap();
        *n += 1;
        Ok(Some(format!(
            "import {{ d }} from '/src/dep.js';\nexport const n = d + {};\n",
            *n
        )))
    }
    fn transform(&self, _: Stage, _: &str, _: &str) -> Result<Option<Transformed>, String> {
        Ok(None)
    }
}

/// What `load` returned is kept until the module is invalidated: fetching it again (an
/// importer's update) reuses it; `server.reloadModule` and `moduleGraph.invalidateModule`
/// load it again.
#[test]
fn plugin_loads_are_kept_until_the_module_is_invalidated() {
    let app = TempDir::new("plug-loads");
    let cache = TempDir::new("plug-loads-cache");
    app.write(
        "src/main.js",
        "import { n } from 'virtual:n';\nconsole.log(n);\nimport.meta.hot.accept('virtual:n', () => {});\n",
    );
    app.write("src/dep.js", "export const d = 0;\n");
    let host = Arc::new(CountingHost::default());
    let mut config = DevConfig::new(app.path());
    config.port = 0;
    config.cache = CacheMode::Dir(cache.path().to_path_buf());
    config.plugins = Some(Arc::clone(&host) as Arc<dyn PluginHost>);
    let server = DevServer::start(config).unwrap();
    let addr = server.addr();
    assert_eq!(http_get(addr, "/src/main.js", &[]).0, 200);
    let url = "/@id/__x00__virtual:n";
    let first = http_get(addr, url, &[]).2;
    assert!(first.contains("d + 1;"), "{first}");
    assert_eq!(http_get(addr, "/src/dep.js", &[]).0, 200);
    let mut ws = WsClient::connect(&addr.to_string(), "/@ts-r/ws").unwrap();
    ws.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.client_count() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    // The dep changes: main.js takes the update of virtual:n, whose code stays as loaded.
    app.write("src/dep.js", "export const d = 10;\n");
    let m = next_of(&mut ws, "update");
    assert_eq!(m["updates"][0]["acceptedPath"], url, "{m}");
    assert!(http_get(addr, url, &[]).2.contains("d + 1;"));
    assert_eq!(*host.loads.lock().unwrap(), 1);
    // `server.reloadModule`: loaded again and hot-updated.
    let handle = host.server.lock().unwrap().clone().unwrap();
    (handle.reload_module)("\0virtual:n");
    let m = next_of(&mut ws, "update");
    assert_eq!(m["updates"][0]["acceptedPath"], url, "{m}");
    assert!(http_get(addr, url, &[]).2.contains("d + 2;"));
    // `moduleGraph.invalidateModule`: the next request loads it again.
    (handle.forget_load)(Some("\0virtual:n"));
    assert!(http_get(addr, url, &[]).2.contains("d + 3;"));
    server.shutdown();
}

/// A plugin like Vite's transform-plugin playground: an unfiltered `load` hook watches
/// `load-dep.txt` for `main.js` (returning no code), and its transform watches `dep.txt` and
/// stamps how often it ran.
#[derive(Debug)]
struct WatchFileHost {
    root: String,
    runs: Mutex<u32>,
    /// How long each `transform` call takes.
    delay: Duration,
}

impl PluginHost for WatchFileHost {
    fn cache_id(&self) -> &str {
        "watch-file-host/1"
    }

    fn hooks(&self) -> PluginHooks {
        PluginHooks {
            load: true,
            load_files: true,
            transform_post: true,
            transform_late: false,
            ..PluginHooks::default()
        }
    }

    fn resolve_ids(
        &self,
        _stage: Stage,
        _importer: &str,
        specifiers: &[String],
    ) -> Result<Vec<Option<PluginResolution>>, String> {
        Ok(vec![None; specifiers.len()])
    }

    fn load(&self, _id: &str) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn load_with_watch(&self, id: &str) -> Result<Loaded, String> {
        Ok(Loaded {
            code: None,
            watch_files: if id.ends_with("/src/main.js") {
                vec![format!("{}/load-dep.txt", self.root)]
            } else {
                Vec::new()
            },
        })
    }

    fn transform(&self, _: Stage, id: &str, code: &str) -> Result<Option<Transformed>, String> {
        if !id.ends_with("/src/main.js") {
            return Ok(None);
        }
        std::thread::sleep(self.delay);
        let mut n = self.runs.lock().unwrap();
        *n += 1;
        Ok(Some(Transformed {
            code: format!("const RUNS = {};\n{code}", *n),
            watch_files: vec![format!("{}/dep.txt", self.root)],
            ..Transformed::default()
        }))
    }
}

/// `this.addWatchFile` in `load` and `transform`: the files' contents join the module's
/// cache key (fetching it again reuses the output), and editing one runs the hooks again and
/// updates the module.
#[test]
fn files_plugins_watch_rerun_the_module_hooks() {
    let app = TempDir::new("plug-watch");
    let cache = TempDir::new("plug-watch-cache");
    app.write("src/main.js", "console.log(RUNS);\n");
    app.write("dep.txt", "a");
    app.write("load-dep.txt", "a");
    let root = paths::id(&paths::canonicalize(app.path()).unwrap()).into_owned();
    let host = Arc::new(WatchFileHost {
        root,
        runs: Mutex::new(0),
        delay: Duration::ZERO,
    });
    let mut config = DevConfig::new(app.path());
    config.port = 0;
    config.cache = CacheMode::Dir(cache.path().to_path_buf());
    config.plugins = Some(Arc::clone(&host) as Arc<dyn PluginHost>);
    let server = DevServer::start(config).unwrap();
    let addr = server.addr();
    assert!(http_get(addr, "/src/main.js", &[]).2.contains("RUNS = 1;"));
    assert!(http_get(addr, "/src/main.js", &[]).2.contains("RUNS = 1;"));
    let mut ws = WsClient::connect(&addr.to_string(), "/@ts-r/ws").unwrap();
    ws.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.client_count() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    app.write("dep.txt", "b");
    next_of(&mut ws, "full-reload");
    assert!(http_get(addr, "/src/main.js", &[]).2.contains("RUNS = 2;"));
    app.write("load-dep.txt", "b");
    next_of(&mut ws, "full-reload");
    assert!(http_get(addr, "/src/main.js", &[]).2.contains("RUNS = 3;"));
    assert_eq!(*host.runs.lock().unwrap(), 3);
    server.shutdown();
}

/// Windows CI flake: the deps scan and the browser's request transform `main.js` at once and
/// share one run. The caller that waited for the other's run must still learn the files the
/// hooks watched (they were recorded only after the waiter had woken and read them), or
/// editing `dep.txt` never runs the hooks again.
#[test]
fn a_request_sharing_the_scans_transform_learns_its_watch_files() {
    let app = TempDir::new("plug-watch-shared");
    let cache = TempDir::new("plug-watch-shared-cache");
    app.write(
        "index.html",
        "<!doctype html><script type=\"module\" src=\"/src/main.js\"></script>\n",
    );
    app.write("src/main.js", "console.log(RUNS);\n");
    app.write("dep.txt", "a");
    app.write("load-dep.txt", "a");
    let root = paths::id(&paths::canonicalize(app.path()).unwrap()).into_owned();
    let host = Arc::new(WatchFileHost {
        root,
        runs: Mutex::new(0),
        // Long enough for the request to join the scan's run (or the other way round).
        delay: Duration::from_millis(300),
    });
    let mut config = DevConfig::new(app.path());
    config.port = 0;
    config.cache = CacheMode::Dir(cache.path().to_path_buf());
    config.plugins = Some(Arc::clone(&host) as Arc<dyn PluginHost>);
    let server = DevServer::start(config).unwrap();
    let addr = server.addr();
    let body = http_get(addr, "/src/main.js", &[]).2;
    assert!(body.contains("RUNS = 1;"), "{body}");
    assert_eq!(*host.runs.lock().unwrap(), 1, "one shared run");
    let mut ws = WsClient::connect(&addr.to_string(), "/@ts-r/ws").unwrap();
    ws.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.client_count() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    app.write("dep.txt", "b");
    next_of(&mut ws, "full-reload");
    assert!(http_get(addr, "/src/main.js", &[]).2.contains("RUNS = 2;"));
    server.shutdown();
}

/// A `transform` hook that fills a stylesheet's `replaced` from `color.txt`, watching it.
#[derive(Debug)]
struct CssWatchHost {
    root: String,
}

impl PluginHost for CssWatchHost {
    fn cache_id(&self) -> &str {
        "css-watch-host/1"
    }

    fn hooks(&self) -> PluginHooks {
        PluginHooks {
            transform_post: true,
            transform_late: false,
            ..PluginHooks::default()
        }
    }

    fn resolve_ids(
        &self,
        _stage: Stage,
        _importer: &str,
        specifiers: &[String],
    ) -> Result<Vec<Option<PluginResolution>>, String> {
        Ok(vec![None; specifiers.len()])
    }

    fn load(&self, _id: &str) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn transform(&self, stage: Stage, id: &str, code: &str) -> Result<Option<Transformed>, String> {
        if stage != Stage::Post || !id.ends_with("/src/main.css") {
            return Ok(None);
        }
        let dep = format!("{}/color.txt", self.root);
        let color = std::fs::read_to_string(&dep).map_err(|e| e.to_string())?;
        Ok(Some(Transformed {
            code: code.replace("replaced", color.trim()),
            watch_files: vec![dep],
            ..Transformed::default()
        }))
    }
}

/// Stylesheets go through the plugins' `transform` hooks too (Vite's hmr "CSS HMR with
/// this.addWatchFile"): editing a file the hook watches updates the stylesheet.
#[test]
fn stylesheet_transforms_run_and_their_watch_files_update_the_stylesheet() {
    let app = TempDir::new("plug-css");
    let cache = TempDir::new("plug-css-cache");
    app.write("src/main.css", ".a { color: replaced; }\n");
    app.write("color.txt", "red");
    let root = paths::id(&paths::canonicalize(app.path()).unwrap()).into_owned();
    let mut config = DevConfig::new(app.path());
    config.port = 0;
    config.cache = CacheMode::Dir(cache.path().to_path_buf());
    config.plugins = Some(Arc::new(CssWatchHost { root }) as Arc<dyn PluginHost>);
    let server = DevServer::start(config).unwrap();
    let addr = server.addr();
    let body = http_get(addr, "/src/main.css?import", &[]).2;
    assert!(body.contains("color: red;"), "{body}");
    let mut ws = WsClient::connect(&addr.to_string(), "/@ts-r/ws").unwrap();
    ws.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.client_count() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    app.write("color.txt", "green");
    let m = next_of(&mut ws, "update");
    assert_eq!(m["updates"][0]["path"], "/src/main.css", "{m}");
    let body = http_get(addr, "/src/main.css?import", &[]).2;
    assert!(body.contains("color: green;"), "{body}");
    server.shutdown();
}

/// `transformIndexHtml` puts a new id in the page on every call.
#[derive(Debug, Default)]
struct RandomIdHost {
    calls: Mutex<u32>,
}

impl PluginHost for RandomIdHost {
    fn cache_id(&self) -> &str {
        "random-id-host/1"
    }

    fn hooks(&self) -> PluginHooks {
        PluginHooks {
            index_html: true,
            ..PluginHooks::default()
        }
    }

    fn resolve_ids(
        &self,
        _stage: Stage,
        _importer: &str,
        specifiers: &[String],
    ) -> Result<Vec<Option<PluginResolution>>, String> {
        Ok(vec![None; specifiers.len()])
    }

    fn load(&self, _id: &str) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn transform(&self, _: Stage, _: &str, _: &str) -> Result<Option<Transformed>, String> {
        Ok(None)
    }

    fn transform_index_html(&self, _url: &str, _file: &Path, html: &str) -> Result<String, String> {
        let mut n = self.calls.lock().unwrap();
        *n += 1;
        Ok(html.replace("{{ id }}", &format!("id{}", *n)))
    }
}

/// A page's inline module script is the one in the page as served: a `transformIndexHtml`
/// that is not pure gives each reload a new script, the same as its page's.
#[test]
fn inline_scripts_match_the_page_as_served() {
    let app = TempDir::new("plug-inline");
    let cache = TempDir::new("plug-inline-cache");
    app.write(
        "index.html",
        "<div id=\"{{ id }}\"></div>\n<script type=\"module\">\nconsole.log('{{ id }}')\n</script>\n",
    );
    let host = Arc::new(RandomIdHost::default());
    let mut config = DevConfig::new(app.path());
    config.port = 0;
    config.cache = CacheMode::Dir(cache.path().to_path_buf());
    config.plugins = Some(Arc::clone(&host) as Arc<dyn PluginHost>);
    let server = DevServer::start(config).unwrap();
    let addr = server.addr();
    let script = "/index.html?html-proxy&index=0.js";
    for n in 1..=3 {
        let page = http_get(addr, "/", &[("Accept", "text/html")]).2;
        assert!(page.contains(&format!("id=\"id{n}\"")), "{page}");
        assert!(page.contains(script), "{page}");
        let js = http_get(addr, script, &[]).2;
        assert!(js.contains(&format!("'id{n}'")), "{n}: {js}");
    }
    server.shutdown();
}
