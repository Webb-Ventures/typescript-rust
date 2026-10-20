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
    CacheMode, DevConfig, DevServer, PluginHooks, PluginHost, PluginResolution, ServerHandle,
    Stage, Transformed,
};

#[derive(Debug, Default)]
struct Host {
    server: Mutex<Option<ServerHandle>>,
    hot_calls: Mutex<Vec<(String, Vec<String>)>>,
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
            transform_pre: true,
            transform_post: true,
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
