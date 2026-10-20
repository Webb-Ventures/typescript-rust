//! Test helpers shared by unit tests, integration tests and the ts_r_dev end-to-end tests.
//! Not part of the stable API.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A temporary directory removed on drop. The path is canonical (macOS `/var` →
/// `/private/var`), matching what the dev server and the file watcher report.
#[derive(Debug)]
pub struct TempDir {
    path: PathBuf,
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

impl TempDir {
    pub fn new(label: &str) -> TempDir {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let dir = std::env::temp_dir().join(format!(
            "ts-r-{label}-{}-{}-{nanos}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        TempDir {
            path: crate::paths::canonicalize(&dir).expect("canonicalize temp dir"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes `rel` (creating parent directories) in place.
    pub fn write(&self, rel: &str, content: &str) {
        let p = self.path.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&p, content).expect("write file");
    }

    /// Writes `rel` the way many editors save: to a temp file, then rename over the target.
    pub fn write_atomic(&self, rel: &str, content: &str) {
        let p = self.path.join(rel);
        let tmp = p.with_extension(format!(
            "{}.tmp-{}",
            p.extension().and_then(|e| e.to_str()).unwrap_or(""),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&tmp, content).expect("write temp");
        fs::rename(&tmp, &p).expect("rename over target");
    }

    pub fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.path.join(rel)).expect("read file")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Fixture app used by the server integration tests and the browser end-to-end test:
/// a self-accepting module (`counter.js`), a dependency accepted by its importer
/// (`accepted.js`), a module with no boundary (`plain.js` → full reload), CSS imported from JS
/// and linked from HTML, JSON and asset imports, a `public/` file and an ESM package in
/// `node_modules` that imports a sibling file.
pub fn write_fixture_app(dir: &TempDir) {
    dir.write(
        "index.html",
        r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8">
    <title>fixture</title>
    <link rel="stylesheet" href="/src/global.css">
  </head>
  <body>
    <div id="app"></div><div id="dep"></div><div id="data"></div><div id="logo"></div><div id="accepted"></div>
    <script type="module" src="/src/main.js"></script>
  </body>
</html>
"#,
    );
    dir.write(
        "src/main.js",
        r#"import { mount } from './counter.js';
import { depValue } from 'tiny-dep';
import './style.css';
import { label } from './plain.js';
import { accepted } from './accepted.js';
import data from './data.json';
import logo from './logo.svg';

window.__marker ??= 'page-' + Math.random().toString(36).slice(2);
document.getElementById('dep').textContent = depValue;
document.getElementById('data').textContent = data.name;
document.getElementById('logo').textContent = logo;
document.body.dataset.label = label;
document.getElementById('accepted').textContent = accepted;
mount(document.getElementById('app'));

if (import.meta.hot) {
  import.meta.hot.accept('./accepted.js', (mod) => {
    document.getElementById('accepted').textContent = mod.accepted;
    window.__depAccepts = (window.__depAccepts || 0) + 1;
  });
}
"#,
    );
    dir.write("src/counter.js", &counter_js("v1", ""));
    dir.write(
        "src/accepted.js",
        "export const accepted = 'accepted-v1';\n",
    );
    dir.write("src/plain.js", "export const label = 'plain-v1';\n");
    dir.write("src/style.css", "#app { color: rgb(255, 0, 0); }\n");
    dir.write(
        "src/global.css",
        "body { background-color: rgb(1, 2, 3); }\n",
    );
    dir.write("src/data.json", "{ \"name\": \"fixture-data\" }\n");
    dir.write(
        "src/logo.svg",
        "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>\n",
    );
    dir.write("public/robots.txt", "User-agent: *\n");
    dir.write(
        "node_modules/tiny-dep/package.json",
        r#"{ "name": "tiny-dep", "version": "1.0.0", "type": "module", "exports": { ".": { "import": "./esm/index.js" } } }"#,
    );
    dir.write(
        "node_modules/tiny-dep/esm/index.js",
        "import { suffix } from './util.js';\nexport const depValue = 'dep-' + suffix;\n",
    );
    dir.write(
        "node_modules/tiny-dep/esm/util.js",
        "export const suffix = 'ok';\n",
    );
}

/// `src/counter.js` of the fixture: self-accepting, uses `hot.data` and `hot.dispose`.
/// `extra` is inserted at the top (e.g. a broken import).
pub fn counter_js(version: &str, extra: &str) -> String {
    format!(
        r#"{extra}
export const version = '{version}';
export function mount(el) {{ el.textContent = 'count ' + version; }}
if (import.meta.hot) {{
  import.meta.hot.data.loads = (import.meta.hot.data.loads || 0) + 1;
  window.__loads = import.meta.hot.data.loads;
  import.meta.hot.dispose((data) => {{ data.disposed = (data.disposed || 0) + 1; window.__disposed = data.disposed; }});
  import.meta.hot.on('vite:beforeUpdate', () => {{ window.__before = (window.__before || 0) + 1; }});
  import.meta.hot.on('vite:afterUpdate', () => {{ window.__after = (window.__after || 0) + 1; }});
  import.meta.hot.accept((mod) => {{
    if (mod) mod.mount(document.getElementById('app'));
    window.__accepted = (window.__accepted || 0) + 1;
  }});
}}
"#
    )
}

/// A plain HTTP/1.1 GET (one request per connection). Returns status, lowercased headers
/// and body.
pub fn http_get(
    addr: std::net::SocketAddr,
    path: &str,
    headers: &[(&str, &str)],
) -> (u16, Vec<(String, String)>, String) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(addr).expect("connect");
    let mut req = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).expect("write");
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).expect("read");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    (status, headers, body.to_owned())
}

/// Header lookup for [`http_get`] results.
pub fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}
