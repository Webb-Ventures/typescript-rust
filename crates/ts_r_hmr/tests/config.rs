//! Vite config features, checked against what Vite 8.3.4 serves for the same fixture:
//! `define` and `import.meta.env` (with `.env` files), `base`, `public/`, asset and JSON
//! imports, `new URL(..., import.meta.url)`, `import.meta.glob`, `server.headers`,
//! `server.cors`, `server.proxy` (HTTP, WebSocket, `rewrite`, 502) and `server.https`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use ts_r_hmr::cors::{Cors, CorsOptions, Origin, OriginMatch};
use ts_r_hmr::paths;
use ts_r_hmr::proxy::ProxyRule;
use ts_r_hmr::testutil::{TempDir, header, http_get};
use ts_r_hmr::{CacheMode, ChangeBatch, ChangeKind, DevConfig, DevServer, FileChange};

fn base_config(app: &TempDir) -> DevConfig {
    let mut config = DevConfig::new(app.path());
    config.port = 0;
    config.cache = CacheMode::Memory;
    config.watch = false;
    config
}

fn get(server: &DevServer, path: &str) -> (u16, Vec<(String, String)>, String) {
    http_get(server.addr(), path, &[])
}

#[test]
fn env_define_and_base_like_vite() {
    let app = TempDir::new("cfg-env");
    app.write(
        "index.html",
        "<!doctype html><html><head><title>%VITE_TITLE% %MODE% %SECRET% %BASE_URL%</title><link rel=\"icon\" href=\"/favicon.ico\"></head><body><a href=\"/about\">a</a><script type=\"module\" src=\"/src/main.js\"></script></body></html>\n",
    );
    app.write(".env", "VITE_A=from-env\nVITE_B=b-env\nSECRET=nope\nAPP_B=app-b\nVITE_EXP=${VITE_A}-expanded\nVITE_TITLE=Title\n");
    app.write(".env.local", "VITE_A=from-local\n");
    app.write(".env.development", "VITE_A=from-dev\nVITE_B=b-dev\n");
    app.write(".env.development.local", "VITE_B=b-dev-local\n");
    app.write(
        "src/main.js",
        "import { x } from './dep.js'\nconst a = __APP_VERSION__\nconst b = process.env.NODE_ENV\nconst c = import.meta.env.VITE_A\nconsole.log(a, b, c, x)\n",
    );
    app.write("src/dep.js", "export const x = 1\n");
    let mut config = base_config(&app);
    config.base = "/app/".to_owned();
    config.env_prefix = vec!["VITE_".to_owned(), "APP_".to_owned()];
    config.define = vec![
        ("__APP_VERSION__".to_owned(), "\"1.2.3\"".to_owned()),
        ("__OBJ__".to_owned(), "{\"x\":1}".to_owned()),
    ];
    let server = DevServer::start(config).expect("server starts");
    assert!(server.url().ends_with("/app/"), "{}", server.url());

    // `/` redirects to the base.
    let (status, h, _) = get(&server, "/");
    assert_eq!((status, header(&h, "location")), (302, Some("/app/")));

    let (status, _, html) = get(&server, "/app/");
    assert_eq!(status, 200);
    assert!(
        html.contains("<title>Title development %SECRET% /app/</title>"),
        "{html}"
    );
    assert!(html.contains("src=\"/app/@ts-r/client\""), "{html}");
    assert!(html.contains("src=\"/app/src/main.js\""), "{html}");
    assert!(html.contains("href=\"/app/favicon.ico\""), "{html}");
    assert!(
        html.contains("<a href=\"/about\">"),
        "links between pages keep their URL"
    );

    let (status, _, main) = get(&server, "/app/src/main.js");
    assert_eq!(status, 200, "{main}");
    assert!(main.starts_with(
        "import.meta.env = {\"APP_B\": \"app-b\", \"BASE_URL\": \"/app/\", \"DEV\": true, \"MODE\": \"development\", \"PROD\": false, \"SSR\": false, \"VITE_A\": \"from-dev\", \"VITE_B\": \"b-dev-local\", \"VITE_EXP\": \"from-dev-expanded\", \"VITE_TITLE\": \"Title\"};"
    ), "{main}");
    assert!(main.contains("const b = \"development\"\n"), "{main}");
    assert!(main.contains("from '/app/src/dep.js'"), "{main}");
    // Globals are defined by the client (and `/@vite/env`), not inlined.
    assert!(main.contains("const a = __APP_VERSION__"), "{main}");
    let (_, _, env) = get(&server, "/app/@vite/env");
    assert!(
        env.starts_with(
            "const __tsr_defines = {\"__APP_VERSION__\": \"1.2.3\", \"__OBJ__\": {\"x\":1}};"
        ),
        "{env}"
    );
    let (_, _, client) = get(&server, "/app/@ts-r/client");
    assert!(client.starts_with("const __tsr_defines"), "{client}");
    assert!(client.contains("const base = \"/app/\";"));
    assert!(client.contains("const wsPath = \"/app/@ts-r/ws\";"));

    // A changed env file reloads the page with the new values.
    let ws_addr = server.addr().to_string();
    let mut ws = ts_r_hmr::ws::WsClient::connect(&ws_addr, "/app/@ts-r/ws").expect("ws");
    let _connected = ws.recv_text().unwrap();
    app.write(".env.development.local", "VITE_B=changed\n");
    server.notify_changes(batch(
        &server.root().join(".env.development.local"),
        ChangeKind::Modified,
    ));
    let msg = ws.recv_text().unwrap();
    assert!(msg.contains("full-reload"), "{msg}");
    let (_, _, main) = get(&server, "/app/src/main.js");
    assert!(main.contains("\"VITE_B\": \"changed\""), "{main}");
}

fn batch(path: &Path, kind: ChangeKind) -> ChangeBatch {
    ChangeBatch {
        changes: vec![FileChange {
            path: path.to_path_buf(),
            kind,
            first_event: Instant::now(),
            first_event_wall: SystemTime::now(),
        }],
        flushed: Instant::now(),
    }
}

/// A module edited while pruned gets a fresh `?t=` when an import of it comes back, so the
/// browser fetches the new file instead of reusing the instance it loaded before the prune.
#[test]
fn restored_import_of_a_module_edited_while_pruned_is_refetched() {
    let app = TempDir::new("cfg-pruned");
    app.write(
        "index.html",
        "<!doctype html><script type=\"module\" src=\"/src/a.js\"></script>\n",
    );
    let a_with = "import { v } from './b.js';\nconsole.log(v);\nimport.meta.hot.accept();\n";
    app.write("src/a.js", a_with);
    app.write("src/b.js", "export const v = 1;\n");
    let server = DevServer::start(base_config(&app)).expect("server");
    let (_, _, a) = get(&server, "/src/a.js");
    assert!(a.contains("'/src/b.js'"), "{a}");
    get(&server, "/src/b.js");
    let a_file = server.root().join("src/a.js");
    app.write("src/a.js", "console.log(0);\nimport.meta.hot.accept();\n");
    server.notify_changes(batch(&a_file, ChangeKind::Modified));
    app.write("src/b.js", "export const v = 2;\n");
    server.notify_changes(batch(&server.root().join("src/b.js"), ChangeKind::Modified));
    app.write("src/a.js", a_with);
    server.notify_changes(batch(&a_file, ChangeKind::Modified));
    let (_, _, a) = get(&server, "/src/a.js");
    assert!(a.contains("'/src/b.js?t="), "{a}");
}

#[test]
fn assets_json_public_and_globs_like_vite() {
    let app = TempDir::new("cfg-assets");
    app.write(
        "index.html",
        "<!doctype html><script type=\"module\" src=\"/src/a.js\"></script>\n",
    );
    app.write(
        "src/a.js",
        "import logo from './logo.png'\nimport icon from './icon.svg?inline'\nimport note from './note.txt?raw'\nimport pub from '/img/p.png'\nimport { name } from './data.json'\nconst u = new URL('./logo.png', import.meta.url)\nconst s = new URL(\"./icon.svg\", import.meta.url)\nconst s1 = new URL('./icon.svg', import.meta.url)\nconst g = import.meta.glob('./pages/*.js')\nconst e = import.meta.glob('./pages/*.js', { eager: true, import: 'default' })\nconsole.log(logo, icon, note, pub, name, u, s, g, e)\n",
    );
    app.write("src/logo.png", "\u{89}PNGfake");
    app.write(
        "src/icon.svg",
        "<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>\n",
    );
    app.write("src/note.txt", "hello raw\n");
    app.write("src/data.json", "{\"name\":\"x\",\"kebab-key\":1}");
    app.write("src/pages/one.js", "export default 1\n");
    app.write("public/img/p.png", "png");
    let server = DevServer::start(base_config(&app)).expect("server starts");
    let (status, _, a) = get(&server, "/src/a.js");
    assert_eq!(status, 200, "{a}");
    assert!(
        a.starts_with("import { default as __vite_glob_1_0 } from \"/src/pages/one.js\";import logo from '/src/logo.png?import'"),
        "{a}"
    );
    assert!(a.contains("from '/src/icon.svg?import&inline'"), "{a}");
    assert!(a.contains("from '/src/note.txt?import&raw'"), "{a}");
    assert!(a.contains("from '/img/p.png?import'"), "{a}");
    assert!(
        a.contains("new URL('/src/logo.png', import.meta.url)"),
        "{a}"
    );
    assert!(
        a.contains("new URL(\"data:image/svg+xml,%3csvg%20xmlns='http://www.w3.org/2000/svg'%3e%3crect/%3e%3c/svg%3e\", import.meta.url)"),
        "{a}"
    );
    // An SVG data URL's attribute quotes are escaped in a single-quoted literal.
    assert!(
        a.contains("const s1 = new URL('data:image/svg+xml,%3csvg%20xmlns=\\'http://www.w3.org/2000/svg\\'%3e%3crect/%3e%3c/svg%3e', import.meta.url)"),
        "{a}"
    );
    assert!(
        a.contains("const g = /* #__PURE__ */ Object.assign({\"./pages/one.js\": () => import(\"/src/pages/one.js\")})"),
        "{a}"
    );
    assert!(
        a.contains(
            "const e = /* #__PURE__ */ Object.assign({\"./pages/one.js\": __vite_glob_1_0})"
        ),
        "{a}"
    );
    assert_eq!(
        get(&server, "/img/p.png?import").2,
        "export default \"/img/p.png\";\n"
    );
    assert_eq!(get(&server, "/img/p.png").2, "png");
    assert_eq!(
        get(&server, "/src/note.txt?import&raw").2,
        "export default \"hello raw\\n\";\n"
    );
    assert_eq!(
        get(&server, "/src/data.json?import").2,
        "export const name = \"x\";\nexport default {\n  name,\n  \"kebab-key\": 1\n};\n"
    );

    // A new file under a glob re-expands the importer.
    app.write("src/pages/two.js", "export default 2\n");
    server.notify_changes(batch(
        &server.root().join("src/pages/two.js"),
        ChangeKind::Created,
    ));
    let (_, _, a) = get(&server, "/src/a.js");
    assert!(
        a.contains("\"./pages/two.js\": () => import(\"/src/pages/two.js\")"),
        "{a}"
    );
}

/// `new URL()` files belong to the graph: an edited small SVG changes its importer's data
/// URL, and a file outside the root is served through `/@fs/`.
#[test]
fn new_url_assets_are_module_dependencies() {
    let app = TempDir::new("cfg-new-url");
    let linked = TempDir::new("cfg-new-url-linked");
    linked.write("logo.png", "png");
    let linked_name = linked.path().file_name().unwrap().to_string_lossy();
    app.write(
        "index.html",
        "<!doctype html><script type=\"module\" src=\"/src/a.js\"></script>\n",
    );
    app.write(
        "src/a.js",
        &format!(
            "const i = new URL('./icon.svg', import.meta.url)\nconst l = new URL('../../{linked_name}/logo.png', import.meta.url)\nconsole.log(i, l)\n"
        ),
    );
    app.write("src/icon.svg", "<svg><rect/></svg>\n");
    let server = DevServer::start(base_config(&app)).expect("server starts");
    let (status, _, a) = get(&server, "/src/a.js");
    assert_eq!(status, 200, "{a}");
    assert!(
        a.contains("new URL('data:image/svg+xml,%3csvg%3e%3crect/%3e%3c/svg%3e'"),
        "{a}"
    );
    // Vite's form on every OS: `/@fs/tmp/x/logo.png`, `/@fs/C:/Users/x/logo.png`.
    let logo = linked.path().join("logo.png");
    let fs_url = format!("/@fs/{}", paths::id(&logo).trim_start_matches('/'));
    assert!(a.contains(&format!("new URL('{fs_url}'")), "{a}");
    let (status, _, png) = get(&server, &fs_url);
    assert_eq!((status, png.as_str()), (200, "png"));

    app.write("src/icon.svg", "<svg><circle/></svg>\n");
    server.notify_changes(batch(
        &server.root().join("src/icon.svg"),
        ChangeKind::Modified,
    ));
    let (_, _, a) = get(&server, "/src/a.js");
    assert!(
        a.contains("new URL('data:image/svg+xml,%3csvg%3e%3ccircle/%3e%3c/svg%3e'"),
        "{a}"
    );
}

/// A tiny upstream: answers `METHOD path host=<host> body=<body>`, `/sock` upgrades echo
/// bytes back.
fn upstream() -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    thread::spawn(move || {
        for s in l.incoming() {
            let Ok(s) = s else { continue };
            thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut w = s;
                let mut first = String::new();
                if r.read_line(&mut first).is_err() {
                    return;
                }
                let mut host = String::new();
                let mut len = 0usize;
                let mut upgrade = false;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("host:") {
                        host = v.trim().to_owned();
                    }
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    if lower.starts_with("upgrade:") {
                        upgrade = true;
                    }
                }
                let mut parts = first.split(' ');
                let method = parts.next().unwrap_or("").to_owned();
                let path = parts.next().unwrap_or("").to_owned();
                if upgrade {
                    let _ = write!(
                        w,
                        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nX-Path: {path}\r\n\r\n"
                    );
                    let mut buf = [0u8; 256];
                    while let Ok(n) = r.read(&mut buf) {
                        if n == 0 || w.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                    return;
                }
                let mut body = vec![0u8; len];
                let _ = r.read_exact(&mut body);
                let text = format!(
                    "{method} {path} host={host} body={}",
                    String::from_utf8_lossy(&body)
                );
                let _ = write!(
                    w,
                    "HTTP/1.1 201 Created\r\nContent-Type: text/plain\r\nX-Upstream: yes\r\nContent-Length: {}\r\n\r\n{text}",
                    text.len()
                );
            });
        }
    });
    addr
}

fn raw_request(addr: SocketAddr, req: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(req.as_bytes()).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn proxy_cors_and_headers_like_vite() {
    let up = upstream();
    let app = TempDir::new("cfg-proxy");
    app.write("index.html", "<!doctype html><p>hi</p>\n");
    app.write("public/robots.txt", "pub\n");
    let mut config = base_config(&app);
    config.headers = vec![
        ("X-H".to_owned(), "h1".to_owned()),
        ("Cache-Control".to_owned(), "max-age=5".to_owned()),
    ];
    config.cors = Cors::On(CorsOptions {
        origin: Origin::List(vec![
            OriginMatch::Exact("http://a.test".into()),
            OriginMatch::Regex(ts_r_hmr::cors::js_regex("\\.b\\.test$", "").unwrap()),
        ]),
        methods: Some("GET,POST".into()),
        allowed_headers: None,
        exposed_headers: Some("X-E".into()),
        credentials: true,
        max_age: Some(60),
    });
    let mut api = ProxyRule::new("/api", &format!("http://{up}")).unwrap();
    api.change_origin = true;
    let mut obj = ProxyRule::new("/obj", &format!("http://{up}/base")).unwrap();
    obj.rewrite = Some(Arc::new(|u: &str| Some(u.replacen("/obj", "/rw", 1))));
    let down = ProxyRule::new("/down", "http://127.0.0.1:9").unwrap();
    let mut ws = ProxyRule::new("/ws", &format!("ws://{up}")).unwrap();
    ws.ws = true;
    ws.rewrite = Some(Arc::new(|u: &str| Some(u.replacen("/ws", "/sock", 1))));
    let re = ProxyRule::new("^/re/.*", &format!("http://{up}")).unwrap();
    config.proxy = vec![api, obj, down, ws, re];
    let server = DevServer::start(config).expect("server starts");
    let addr = server.addr();

    // server.headers on our own responses (replacing ours), CORS on all of them.
    let (status, h, body) = http_get(addr, "/robots.txt", &[("Origin", "http://x.b.test")]);
    assert_eq!((status, body.as_str()), (200, "pub\n"));
    assert_eq!(header(&h, "x-h"), Some("h1"));
    assert_eq!(header(&h, "cache-control"), Some("max-age=5"));
    assert_eq!(
        header(&h, "access-control-allow-origin"),
        Some("http://x.b.test")
    );
    assert_eq!(header(&h, "access-control-allow-credentials"), Some("true"));
    assert_eq!(header(&h, "vary"), Some("Origin"));
    let (_, h, _) = http_get(addr, "/robots.txt", &[("Origin", "http://evil.test")]);
    assert_eq!(header(&h, "access-control-allow-origin"), None);

    // Preflight.
    let pre = raw_request(
        addr,
        "OPTIONS /x HTTP/1.1\r\nHost: h\r\nOrigin: http://a.test\r\nAccess-Control-Request-Headers: x-a\r\nConnection: close\r\n\r\n",
    );
    assert!(pre.starts_with("HTTP/1.1 204"), "{pre}");
    assert!(
        pre.contains("Access-Control-Allow-Methods: GET,POST\r\n"),
        "{pre}"
    );
    assert!(pre.contains("Access-Control-Max-Age: 60\r\n"), "{pre}");
    assert!(
        pre.contains("Vary: Origin, Access-Control-Request-Headers\r\n"),
        "{pre}"
    );

    // HTTP proxy: string form changes the origin; CORS but not server.headers.
    let (status, h, body) = http_get(addr, "/api/x?y=1", &[("Origin", "http://a.test")]);
    assert_eq!(status, 201, "{body}");
    assert_eq!(body, format!("GET /api/x?y=1 host={up} body="));
    assert_eq!(header(&h, "x-upstream"), Some("yes"));
    assert_eq!(header(&h, "x-h"), None);
    assert_eq!(
        header(&h, "access-control-allow-origin"),
        Some("http://a.test")
    );
    // Object form keeps the Host; rewrite + target path.
    let (_, _, body) = http_get(addr, "/obj/a?b=1", &[]);
    assert_eq!(body, format!("GET /base/rw/a?b=1 host={addr} body="));
    let (_, _, body) = http_get(addr, "/re/z", &[]);
    assert!(body.starts_with("GET /re/z"), "{body}");
    // A body is streamed through; the connection stays usable.
    let post = raw_request(
        addr,
        "POST /api/p HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\n\r\nhelloGET /api/second HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
    );
    assert!(post.contains("POST /api/p host="), "{post}");
    assert!(post.contains("body=hello"), "{post}");
    assert!(post.contains("GET /api/second"), "{post}");
    // Unreachable upstream.
    let (status, h, body) = http_get(addr, "/down/q", &[]);
    assert_eq!((status, body.as_str()), (502, ""));
    assert_eq!(header(&h, "content-type"), Some("text/plain"));

    // WebSocket tunnel with rewrite.
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(b"GET /ws/x HTTP/1.1\r\nHost: h\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n").unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut head = String::new();
    loop {
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        head.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    assert!(head.contains("X-Path: /sock/x"), "{head}");
    s.write_all(b"ping-bytes").unwrap();
    let mut buf = [0u8; 10];
    r.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"ping-bytes");
}

#[cfg(feature = "tls")]
#[test]
fn https_serves_tls_and_proxies_to_tls() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tls");
    let app = TempDir::new("cfg-https");
    app.write("public/robots.txt", "secret\n");
    // Find a free port first: the proxy rules point at the server itself.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut config = base_config(&app);
    config.port = port;
    config.strict_port = true;
    config.https = Some(ts_r_hmr::Https {
        cert: std::fs::read(dir.join("cert.pem")).unwrap(),
        key: std::fs::read(dir.join("key.pem")).unwrap(),
    });
    let mut insecure = ProxyRule::new("/self", &format!("https://127.0.0.1:{port}")).unwrap();
    insecure.secure = false;
    insecure.rewrite = Some(Arc::new(|u: &str| Some(u.replacen("/self", "", 1))));
    let mut strict = ProxyRule::new("/strict", &format!("https://127.0.0.1:{port}")).unwrap();
    strict.rewrite = Some(Arc::new(|u: &str| Some(u.replacen("/strict", "", 1))));
    config.proxy = vec![insecure, strict];
    let server = DevServer::start(config).expect("server starts");
    assert!(server.url().starts_with("https://"), "{}", server.url());
    let fetch = |path: &str| -> String {
        let mut c = ts_r_hmr::net::connect("127.0.0.1", port, true, true).expect("tls connect");
        write!(
            c.writer,
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        c.writer.flush().unwrap();
        let mut out = Vec::new();
        let _ = c.reader.read_to_end(&mut out);
        String::from_utf8_lossy(&out).into_owned()
    };
    let r = fetch("/robots.txt");
    assert!(
        r.starts_with("HTTP/1.1 200") && r.ends_with("secret\n"),
        "{r}"
    );
    let r = fetch("/self/robots.txt");
    assert!(
        r.starts_with("HTTP/1.1 200") && r.ends_with("secret\n"),
        "{r}"
    );
    // A self-signed certificate is refused unless `secure: false`.
    let r = fetch("/strict/robots.txt");
    assert!(r.starts_with("HTTP/1.1 502"), "{r}");
    // Plain HTTP to the TLS port gets nothing useful (the handshake fails).
    let (status, _, _) = http_get(server.addr(), "/robots.txt", &[]);
    assert_ne!(status, 200);
}

/// A wasm module with one imported function (`./env.js` `log`) and one exported (`run`).
const WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic, version
    0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // type: () -> ()
    0x02, 0x10, 0x01, 0x08, b'.', b'/', b'e', b'n', b'v', b'.', b'j', b's', 0x03, b'l', b'o', b'g',
    0x00, 0x00, // import ./env.js log
    0x03, 0x02, 0x01, 0x00, // function 1 has type 0
    0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01, // export run
    0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b, // code: call 0
];

/// Wasm (`?init` and direct imports), `?worker` / `?sharedworker` imports and dynamic
/// imports of expressions, the way Vite 8.3.4 serves them.
#[test]
fn wasm_workers_and_dynamic_imports_like_vite() {
    let app = TempDir::new("cfg-wasm");
    app.write(
        "index.html",
        "<!doctype html><script type=\"module\" src=\"/src/a.js\"></script>\n",
    );
    app.write(
        "src/a.js",
        "import init from './m.wasm?init'\nimport { run } from './m.wasm'\nimport W from './w.js?worker'\nimport S from './w.js?sharedworker&name=s'\nimport u from './w.js?worker&url'\nconst p = './x.js'\nimport(/* @vite-ignore */ p)\nconst v = 'a'\nimport(`./data/${v}.json`)\nimport(`./data/${v}.js`)\nimport(`#d/${v}.js`)\nconsole.log(init, run, W, S, u)\n",
    );
    app.write("src/env.js", "export const log = () => {}\n");
    app.write(
        "package.json",
        "{\"imports\": {\"#d/*\": \"./src/data/*\"}}",
    );
    app.write("src/data/a.js", "export default 1\n");
    app.write("src/w.js", "self.onmessage = () => {}\n");
    std::fs::write(app.path().join("src/m.wasm"), WASM).expect("write wasm");
    let server = DevServer::start(base_config(&app)).expect("server starts");
    let (_, _, a) = get(&server, "/src/a.js");
    assert!(
        a.starts_with("import { injectQuery as __vite__injectQuery } from \"/@ts-r/client\";import init from '/src/m.wasm?import&init'"),
        "{a}"
    );
    assert!(a.contains("import W from '/src/w.js?worker'"), "{a}");
    // Expressions, and templates naming a non-script file, get `?import` at run time.
    assert!(
        a.contains("import(/* @vite-ignore */ __vite__injectQuery(p, 'import'))"),
        "{a}"
    );
    assert!(
        a.contains("import(__vite__injectQuery(`./data/${v}.json`, 'import'))"),
        "{a}"
    );
    assert!(a.contains("import(`./data/${v}.js`)"), "{a}");
    // A template's subpath-import head resolves to a URL.
    assert!(a.contains("import(`/src/data/${v}.js`)"), "{a}");
    assert_eq!(
        get(&server, "/src/m.wasm?init").2,
        "import initWasm from \"/@id/__x00__vite/wasm-helper.js\"\nexport default opts => initWasm(opts, \"/src/m.wasm\")\n"
    );
    let (status, h, wasm) = get(&server, "/src/m.wasm");
    assert_eq!(status, 200);
    assert_eq!(header(&h, "content-type"), Some("application/wasm"));
    assert!(wasm.starts_with("\0asm"));
    let direct = get(&server, "/src/m.wasm?import").2;
    assert!(
        direct.contains("import * as __tsr_wasmImport_0 from \"/src/env.js\";"),
        "{direct}"
    );
    assert!(
        direct.contains("export const { run } = __tsr_wasmModule;"),
        "{direct}"
    );
    assert!(
        get(&server, "/@id/__x00__vite/wasm-helper.js")
            .2
            .contains("export default async function initWasm(imports = {}, url)")
    );
    assert!(
        get(&server, "/src/w.js?sharedworker&name=s").2.contains(
            "return new SharedWorker(\"/src/w.js?worker_file&type=module&name=s\", { type: \"module\", name: options?.name });"
        )
    );
    assert_eq!(
        get(&server, "/src/w.js?worker&url").2,
        "export default \"/src/w.js?worker_file&type=module\"\n"
    );
    assert_eq!(
        get(&server, "/src/w.js?worker_file&type=module").2,
        "import \"/@vite/env\"\n;\nself.onmessage = () => {}\n"
    );
}
