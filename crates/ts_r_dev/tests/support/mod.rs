//! Shared helpers for the end-to-end tests: run the `ts-r-dev` binary, drive headless Chrome
//! over the DevTools protocol (CDP) using ts_r_hmr's WebSocket client.

#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use ts_r_hmr::testutil::TempDir;
use ts_r_hmr::ws::WsClient;

/// A running `ts-r-dev` process.
pub struct DevProcess {
    child: Child,
    pub url: String,
    pub addr: String,
    pub lines: Arc<Mutex<Vec<String>>>,
}

impl DevProcess {
    pub fn start(root: &Path, cache_dir: &Path, extra: &[&str]) -> DevProcess {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ts-r-dev"))
            .arg(root)
            .args(["--port", "0", "--cache-dir"])
            .arg(cache_dir)
            .args(extra)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn ts-r-dev");
        let stdout = child.stdout.take().unwrap();
        let lines = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        let l = Arc::clone(&lines);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(url) = line.trim().strip_prefix("Local: ") {
                    let _ = tx.send(url.to_owned());
                }
                l.lock().unwrap().push(line);
            }
        });
        let url = rx
            .recv_timeout(Duration::from_secs(20))
            .expect("ts-r-dev printed its URL");
        let addr = url
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_owned();
        DevProcess {
            child,
            url,
            addr,
            lines,
        }
    }

    pub fn log_lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    pub fn stats(&self) -> Value {
        let addr = self.addr.parse().unwrap();
        let (_, _, body) = ts_r_hmr::testutil::http_get(addr, "/@ts-r/stats", &[]);
        serde_json::from_str(&body).unwrap()
    }
}

impl Drop for DevProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Finds Chrome: `TS_R_CHROME`, the macOS app bundle, or Chrome/Chromium on `PATH`.
pub fn find_chrome() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TS_R_CHROME") {
        return Some(PathBuf::from(p));
    }
    let mac = Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
    if mac.exists() {
        return Some(mac.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
        ] {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Headless Chrome with a throwaway profile.
pub struct Chrome {
    child: Child,
    _profile: TempDir,
    pub cdp: Cdp,
}

impl Chrome {
    pub fn launch(binary: &Path) -> Chrome {
        let profile = TempDir::new("chrome-profile");
        let child = Command::new(binary)
            .args([
                "--headless=new",
                "--remote-debugging-port=0",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-extensions",
                "--disable-background-networking",
                "--disable-component-update",
                "--disable-sync",
                "--disable-gpu",
                "--disable-background-timer-throttling",
                "--disable-renderer-backgrounding",
                "--disable-backgrounding-occluded-windows",
                "--disable-features=Translate,MediaRouter,OptimizationHints",
                "--remote-allow-origins=*",
            ])
            .arg(format!("--user-data-dir={}", profile.path().display()))
            .arg("about:blank")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("launch chrome");
        let port_file = profile.path().join("DevToolsActivePort");
        let deadline = Instant::now() + Duration::from_secs(30);
        let (port, path) = loop {
            if let Ok(text) = std::fs::read_to_string(&port_file) {
                let mut l = text.lines();
                if let (Some(port), Some(path)) = (l.next(), l.next()) {
                    break (port.to_owned(), path.to_owned());
                }
            }
            assert!(Instant::now() < deadline, "chrome did not start");
            std::thread::sleep(Duration::from_millis(50));
        };
        let ws = WsClient::connect(&format!("127.0.0.1:{port}"), &path).expect("cdp connect");
        ws.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
        Chrome {
            child,
            _profile: profile,
            cdp: Cdp {
                ws,
                next_id: 1,
                session: None,
            },
        }
    }

    /// Opens a new tab at `url` and attaches the CDP session to it.
    pub fn open(&mut self, url: &str) {
        let target = self
            .cdp
            .call("Target.createTarget", json!({ "url": "about:blank" }))
            .unwrap();
        let target_id = target["targetId"].as_str().unwrap().to_owned();
        let attached = self
            .cdp
            .call(
                "Target.attachToTarget",
                json!({ "targetId": target_id, "flatten": true }),
            )
            .unwrap();
        self.cdp.session = Some(attached["sessionId"].as_str().unwrap().to_owned());
        self.cdp.call("Page.enable", json!({})).unwrap();
        self.cdp
            .call("Page.navigate", json!({ "url": url }))
            .unwrap();
    }
}

impl Drop for Chrome {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A minimal CDP client (flattened sessions).
pub struct Cdp {
    ws: WsClient,
    next_id: u64,
    session: Option<String>,
}

impl Cdp {
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = &self.session
            && !method.starts_with("Target.")
        {
            msg["sessionId"] = json!(s);
        }
        self.ws
            .send_text(&msg.to_string())
            .map_err(|e| e.to_string())?;
        loop {
            let text = self.ws.recv_text().map_err(|e| e.to_string())?;
            let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            if v["id"].as_u64() == Some(id) {
                if let Some(err) = v.get("error") {
                    return Err(err.to_string());
                }
                return Ok(v["result"].clone());
            }
        }
    }

    /// Evaluates `expr` in the page and returns its JSON value.
    pub fn eval(&mut self, expr: &str) -> Result<Value, String> {
        let r = self.call(
            "Runtime.evaluate",
            json!({ "expression": expr, "returnByValue": true, "awaitPromise": true }),
        )?;
        if let Some(ex) = r.get("exceptionDetails") {
            return Err(ex.to_string());
        }
        Ok(r["result"]["value"].clone())
    }

    /// Polls `expr` until it is truthy; returns its value.
    pub fn wait_for(&mut self, expr: &str, timeout: Duration) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            let last = match self.eval(expr) {
                Ok(v) if truthy(&v) => return v,
                Ok(v) => v,
                Err(e) => Value::String(e),
            };
            assert!(
                Instant::now() < deadline,
                "timed out waiting for `{expr}` (last: {last})"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}
