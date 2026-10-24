//! Vite plugin compatibility: a Node sidecar (`vite_sidecar.mjs`) loads the project's
//! `vite.config.*` and runs its plugins' hooks for the dev server ([`ts_r_hmr::PluginHost`]).
//!
//! - Started only when a config exists; its manifest (plugin names, which hooks exist, JSX
//!   options) is cached in `ts_r_cache` (`dev-plugins`), keyed by the config (and the local
//!   files it imports), `package.json` and lockfile contents; plugin transforms are cached
//!   under the same key. A config whose only plugins are ones ts-r-dev replaces with
//!   built-ins (`@vitejs/plugin-react`: Fast Refresh and JSX are built in) needs no sidecar,
//!   so with a warm manifest Node never starts.
//! - Limits: `--max-old-space-size` (`TS_R_PLUGIN_MEM_MB`, default 2048), a per-call
//!   timeout (`TS_R_PLUGIN_TIMEOUT_S`, default 30) covering the request write and the
//!   reply (a call that times out kills the sidecar), killed when ts-r-dev drops the host,
//!   and the sidecar exits by itself when its stdin closes (ts-r-dev died).
//! - Protocol: JSON lines over stdin/stdout (see the script's header); plugin output to
//!   stdout goes to stderr.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use ts_r_cache::{Cache, Key, KeyBuilder};
use ts_r_hmr::http::{Request, Response};
use ts_r_hmr::{PluginHooks, PluginHost, PluginResolution, ServerHandle, Stage, Transformed};

const SIDECAR: &str = include_str!("vite_sidecar.mjs");
const CONFIG_NAMES: &[&str] = &[
    "vite.config.js",
    "vite.config.mjs",
    "vite.config.ts",
    "vite.config.mts",
    "vite.config.cjs",
    "vite.config.cts",
];
const LOCKFILES: &[&str] = &[
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
];
const NAMESPACE: &str = "dev-plugins";
const FORMAT_VERSION: u32 = 1;

/// What the sidecar reported after running the config hooks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    /// Plugins whose hooks run (config order after `enforce` sorting).
    pub plugins: Vec<String>,
    /// Plugins ts-r-dev replaces with built-ins (only their `config` hooks ran).
    pub replaced: Vec<String>,
    pub hooks: PluginHooks,
    /// JSX options from the resolved config (`oxc.jsx`, `esbuild.jsx*`), e.g. set by
    /// `@vitejs/plugin-react`'s `jsxImportSource`.
    pub jsx_import_source: Option<String>,
    pub jsx_classic: bool,
    /// `optimizeDeps.include` from the resolved config (plugin-react adds React's entries).
    pub optimize_include: Vec<String>,
    /// Dev server options from the resolved config (`mode`, `base`, `define`, `envPrefix`,
    /// `envDir`, `publicDir`, `server.{host,port,strictPort,open,headers,cors,proxy,https}`),
    /// as the sidecar serialized them; see [`apply_dev_options`].
    pub dev: Value,
    pub warnings: Vec<String>,
}

impl Manifest {
    fn from_json(v: &Value) -> Manifest {
        let strings = |v: &Value| -> Vec<String> {
            v.as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        let h = &v["hooks"];
        let b = |k: &str| h[k].as_bool().unwrap_or(false);
        Manifest {
            plugins: strings(&v["plugins"]),
            replaced: strings(&v["replaced"]),
            hooks: PluginHooks {
                resolve_pre: b("resolve_pre"),
                resolve_post: b("resolve_post"),
                load: b("load"),
                transform_pre: b("transform_pre"),
                transform_post: b("transform_post"),
                index_html: b("index_html"),
                hot_update: b("hot_update"),
                middleware: b("middleware"),
                client_events: b("client_events"),
            },
            jsx_import_source: v["jsx"]["importSource"].as_str().map(str::to_owned),
            jsx_classic: v["jsx"]["runtime"] == "classic",
            optimize_include: strings(&v["optimizeInclude"]),
            dev: v["dev"].clone(),
            warnings: strings(&v["warnings"]),
        }
    }

    fn to_json(&self) -> Value {
        let h = &self.hooks;
        json!({
            "plugins": self.plugins,
            "replaced": self.replaced,
            "hooks": {
                "resolve_pre": h.resolve_pre, "resolve_post": h.resolve_post, "load": h.load,
                "transform_pre": h.transform_pre, "transform_post": h.transform_post,
                "index_html": h.index_html, "hot_update": h.hot_update,
                "middleware": h.middleware, "client_events": h.client_events,
            },
            "jsx": {
                "importSource": self.jsx_import_source,
                "runtime": if self.jsx_classic { "classic" } else { "automatic" },
            },
            "optimizeInclude": self.optimize_include,
            "dev": self.dev,
            "warnings": self.warnings,
        })
    }

    /// Whether any plugin hook (or a `server.proxy` `rewrite` function) needs the sidecar
    /// at run time.
    pub fn needs_sidecar(&self) -> bool {
        self.hooks != PluginHooks::default()
            || self.dev["server"]["proxy"]
                .as_array()
                .is_some_and(|rules| rules.iter().any(|r| r["rewrite"] == true))
    }
}

/// Applies the manifest's dev options to `config`. `rewrite(index)` makes the function
/// that runs proxy rule `index`'s `rewrite` (in the sidecar). Returns warnings.
pub fn apply_dev_options(
    dev: &Value,
    config: &mut ts_r_hmr::DevConfig,
    rewrite: &dyn Fn(usize) -> ts_r_hmr::proxy::Rewrite,
) -> Vec<String> {
    use ts_r_hmr::cors::{Cors, CorsOptions, Origin, OriginMatch, js_regex};
    let mut warnings = Vec::new();
    if dev.is_null() {
        return warnings;
    }
    if let Some(m) = dev["mode"].as_str() {
        config.mode = m.to_owned();
    }
    if let Some(b) = dev["base"].as_str() {
        config.base = ts_r_hmr::server::normalize_base(b);
    }
    if let Some(d) = dev["define"].as_object() {
        config.define = d
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("undefined").to_owned()))
            .collect();
    }
    if let Some(e) = dev["processEnv"].as_object() {
        config.config_env = e
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
            .collect();
    }
    if let Some(p) = dev["envPrefix"].as_array() {
        config.env_prefix = p
            .iter()
            .filter_map(|s| s.as_str().map(str::to_owned))
            .collect();
    }
    // Relative to the root (the cached manifest may come from another checkout).
    match &dev["envDir"] {
        Value::Bool(false) => config.env_dir = None,
        Value::String(s) => config.env_dir = Some(config.root.join(s)),
        _ => {}
    }
    match &dev["publicDir"] {
        Value::Bool(false) => config.public_dir = None,
        Value::String(s) => config.public_dir = Some(config.root.join(s)),
        _ => {}
    }
    if let Some(n) = dev["assetsInlineLimit"].as_u64() {
        config.assets_inline_limit = n;
    }
    for p in dev["assetsInclude"].as_array().into_iter().flatten() {
        let added = match (p["glob"].as_str(), p["regex"].as_str()) {
            (Some(g), _) => config.assets_include.add_glob(g, &config.root),
            (None, Some(r)) => js_regex(r, p["flags"].as_str().unwrap_or(""))
                .map(|re| config.assets_include.add_regex(re)),
            _ => Ok(()),
        };
        if let Err(e) = added {
            warnings.push(format!("`assetsInclude`: {e}"));
        }
    }
    let s = &dev["server"];
    if let Some(h) = s["host"].as_str() {
        config.host = h.to_owned();
    }
    if let Some(p) = s["port"].as_u64().and_then(|p| u16::try_from(p).ok()) {
        config.port = p;
    }
    if s["strictPort"] == true {
        config.strict_port = true;
    }
    config.headers = s["headers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some((p[0].as_str()?.to_owned(), p[1].as_str()?.to_owned())))
        .collect();
    // A regex the engine can't compile is dropped (never matches) with a warning.
    let mut origin_match = |v: &Value| -> Option<OriginMatch> {
        match v {
            Value::String(s) => Some(OriginMatch::Exact(s.clone())),
            Value::Object(_) => match js_regex(
                v["regex"].as_str().unwrap_or(""),
                v["flags"].as_str().unwrap_or(""),
            ) {
                Ok(r) => Some(OriginMatch::Regex(r)),
                Err(e) => {
                    warnings.push(format!("server.cors.origin: {e}"));
                    None
                }
            },
            _ => None,
        }
    };
    match &s["cors"] {
        Value::Bool(false) => config.cors = Cors::Off,
        Value::Bool(true) => config.cors = Cors::On(CorsOptions::default()),
        Value::Object(c) => {
            let origin = match &c.get("origin").cloned().unwrap_or(Value::Null) {
                Value::Bool(true) => Origin::Reflect,
                Value::Bool(false) => {
                    config.cors = Cors::Off;
                    return finish_proxy(s, config, rewrite, warnings);
                }
                Value::String(o) if o == "*" => Origin::Any,
                Value::String(o) => Origin::Fixed(o.clone()),
                Value::Array(list) => {
                    Origin::List(list.iter().filter_map(&mut origin_match).collect())
                }
                _ => Origin::Any,
            };
            let text = |k: &str| c.get(k).and_then(|v| v.as_str()).map(str::to_owned);
            config.cors = Cors::On(CorsOptions {
                origin,
                methods: text("methods"),
                allowed_headers: text("allowedHeaders"),
                exposed_headers: text("exposedHeaders"),
                credentials: c.get("credentials") == Some(&Value::Bool(true)),
                max_age: c.get("maxAge").and_then(Value::as_u64),
            });
        }
        _ => {}
    }
    finish_proxy(s, config, rewrite, warnings)
}

fn finish_proxy(
    s: &Value,
    config: &mut ts_r_hmr::DevConfig,
    rewrite: &dyn Fn(usize) -> ts_r_hmr::proxy::Rewrite,
    mut warnings: Vec<String>,
) -> Vec<String> {
    for r in s["proxy"].as_array().into_iter().flatten() {
        let key = r["key"].as_str().unwrap_or("");
        let target = r["target"].as_str().unwrap_or("");
        match ts_r_hmr::proxy::ProxyRule::new(key, target) {
            Ok(mut rule) => {
                rule.change_origin = r["changeOrigin"] == true;
                rule.ws = r["ws"] == true;
                rule.secure = r["secure"] != false;
                rule.headers = r["headers"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| Some((p[0].as_str()?.to_owned(), p[1].as_str()?.to_owned())))
                    .collect();
                if r["rewrite"] == true
                    && let Some(i) = r["index"].as_u64()
                {
                    rule.rewrite = Some(rewrite(i as usize));
                }
                config.proxy.push(rule);
            }
            Err(e) => warnings.push(format!("server.proxy['{key}']: {e}")),
        }
    }
    let https = &s["https"];
    if let (Some(cert), Some(key)) = (https["cert"].as_str(), https["key"].as_str()) {
        config.https = Some(ts_r_hmr::Https {
            cert: cert.as_bytes().to_vec(),
            key: key.as_bytes().to_vec(),
        });
    }
    warnings
}

/// Where the manifest cache lives.
#[derive(Clone, Debug)]
pub enum ManifestCache {
    Default,
    Dir(PathBuf),
    Off,
}

/// The project's Vite plugins: the manifest, plus the running sidecar when hooks need it.
#[derive(Debug)]
pub struct VitePlugins {
    pub manifest: Manifest,
    pub config: PathBuf,
    /// The sidecar's startup time (config + plugin import + config hooks), if it ran.
    pub startup: Option<Duration>,
    cache_id: String,
    sidecar: Option<Sidecar>,
}

/// The project's `vite.config.*`, if any.
pub fn find_config(root: &Path) -> Option<PathBuf> {
    CONFIG_NAMES
        .iter()
        .map(|n| root.join(n))
        .find(|p| p.is_file())
}

/// The config file and the local files it imports, transitively (relative specifiers
/// only: packages are covered by the lockfile), at most 256.
fn config_files(config: &Path) -> Vec<PathBuf> {
    const EXTS: &[&str] = &["ts", "mts", "cts", "js", "mjs", "cjs"];
    let mut files = vec![config.to_path_buf()];
    let mut i = 0;
    while i < files.len() && files.len() < 256 {
        let Ok(text) = std::fs::read_to_string(&files[i]) else {
            i += 1;
            continue;
        };
        let dir = files[i].parent().unwrap_or(Path::new("/")).to_path_buf();
        for rec in ts_r_hmr::scan::scan(&text).imports {
            let spec = rec.specifier.split(['?', '#']).next().unwrap_or("");
            if !spec.starts_with("./") && !spec.starts_with("../") {
                continue;
            }
            let base = dir.join(spec);
            let found = std::iter::once(base.clone())
                .chain(EXTS.iter().map(|e| {
                    let mut p = base.clone().into_os_string();
                    p.push(format!(".{e}"));
                    PathBuf::from(p)
                }))
                .chain(EXTS.iter().map(|e| base.join(format!("index.{e}"))))
                .find(|p| p.is_file());
            if let Some(p) = found
                && !files.contains(&p)
            {
                files.push(p);
            }
        }
        i += 1;
    }
    files
}

fn manifest_key(root: &Path, config: &Path, mode: &str) -> Key {
    let mut b = KeyBuilder::new("ts-r-dev/vite-plugins");
    b.str(env!("CARGO_PKG_VERSION"));
    b.str(mode);
    b.key_field(&Key::of(SIDECAR.as_bytes()));
    b.str(
        &config
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default(),
    );
    // The config and the plugin code it imports: their output is cached under this key.
    for f in config_files(config).into_iter().chain(
        ["package.json"]
            .iter()
            .chain(LOCKFILES)
            .map(|n| root.join(n)),
    ) {
        match std::fs::read(&f) {
            Ok(bytes) => b.key_field(&Key::of(&bytes)),
            Err(_) => b.str("\0missing"),
        };
    }
    b.key()
}

/// Plugins being loaded: [`VitePlugins::begin`] starts Node without waiting for it, so the
/// caller can do its own startup work meanwhile; [`Starting::finish`] waits.
#[derive(Debug)]
pub struct Starting(StartingState);

// One per process, moved once: the size difference doesn't matter.
#[allow(clippy::large_enum_variant)]
enum StartingState {
    Done(VitePlugins),
    Waiting {
        sidecar: Sidecar,
        ready: Ready,
        config: PathBuf,
        disk: Option<Cache>,
        key: Key,
        cache_id: String,
        started: std::time::Instant,
    },
}

impl Starting {
    /// The plugins, once the sidecar reported its manifest (immediately when it was
    /// cached).
    pub fn finish(self) -> Result<VitePlugins, String> {
        match self.0 {
            StartingState::Done(p) => Ok(p),
            StartingState::Waiting {
                sidecar,
                ready,
                config,
                disk,
                key,
                cache_id,
                started,
            } => {
                let manifest = Manifest::from_json(&Sidecar::wait_ready(&ready, &config)?);
                if let Some(c) = &disk {
                    let _ = c.put(&key, manifest.to_json().to_string().as_bytes());
                    let _ = c.flush();
                }
                let startup = Some(started.elapsed());
                Ok(VitePlugins {
                    sidecar: manifest.needs_sidecar().then_some(sidecar),
                    manifest,
                    config,
                    startup,
                    cache_id,
                })
            }
        }
    }
}

impl std::fmt::Debug for StartingState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartingState::Done(p) => f.debug_tuple("Done").field(p).finish(),
            StartingState::Waiting { config, .. } => f
                .debug_struct("Waiting")
                .field("config", config)
                .finish_non_exhaustive(),
        }
    }
}

impl VitePlugins {
    /// Reads the manifest (cached, else from a fresh sidecar, see [`Starting::finish`]) and
    /// keeps the sidecar running when plugins have hooks. `force` ignores the cached
    /// manifest. Doesn't wait for a sidecar whose manifest isn't cached.
    pub fn begin(
        root: &Path,
        config: &Path,
        cache: &ManifestCache,
        force: bool,
        mode: &str,
    ) -> Result<Starting, String> {
        let canonical = ts_r_hmr::paths::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let root = canonical.as_path();
        let config_abs =
            ts_r_hmr::paths::canonicalize(config).unwrap_or_else(|_| config.to_path_buf());
        let config = config_abs.as_path();
        let key = manifest_key(root, config, mode);
        let disk = match cache {
            ManifestCache::Default => Cache::open(NAMESPACE, FORMAT_VERSION).ok(),
            ManifestCache::Dir(d) => Cache::open_in(d, NAMESPACE, FORMAT_VERSION).ok(),
            ManifestCache::Off => None,
        };
        let cache_id = format!("vite-plugins/{}", &key.to_hex()[..16]);
        let cached = disk
            .as_ref()
            .filter(|_| !force)
            .and_then(|c| c.get(&key))
            .and_then(|blob| serde_json::from_slice::<Value>(&blob).ok())
            .map(|v| Manifest::from_json(&v));
        if let Some(manifest) = cached.as_ref().filter(|m| !m.needs_sidecar()) {
            return Ok(Starting(StartingState::Done(VitePlugins {
                manifest: manifest.clone(),
                config: config.to_path_buf(),
                startup: None,
                cache_id,
                sidecar: None,
            })));
        }
        let started = std::time::Instant::now();
        let (sidecar, ready) = Sidecar::spawn(root, config, mode)?;
        if let Some(manifest) = cached {
            // The hooks are known: serve without waiting for Node (early requests queue in
            // its stdin). A manifest that changed anyway (a file the config imports) is
            // re-cached for the next start.
            let expected = manifest.clone();
            let config_path = config.to_path_buf();
            std::thread::spawn(move || match Sidecar::wait_ready(&ready, &config_path) {
                Ok(v) => {
                    let fresh = Manifest::from_json(&v);
                    if fresh != expected {
                        if let Some(c) = &disk {
                            let _ = c.put(&key, fresh.to_json().to_string().as_bytes());
                            let _ = c.flush();
                        }
                        eprintln!(
                            "ts-r-dev: the plugins in {} changed since they were cached; restart ts-r-dev to use them",
                            config_path.display()
                        );
                    }
                }
                Err(e) => eprintln!("ts-r-dev: plugins: {e}"),
            });
            return Ok(Starting(StartingState::Done(VitePlugins {
                manifest,
                config: config.to_path_buf(),
                startup: None,
                cache_id,
                sidecar: Some(sidecar),
            })));
        }
        Ok(Starting(StartingState::Waiting {
            sidecar,
            ready,
            config: config.to_path_buf(),
            disk,
            key,
            cache_id,
            started,
        }))
    }

    /// Runs `server.proxy[index].rewrite(url)` in the sidecar.
    pub fn proxy_rewrite(&self, index: usize, url: &str) -> Option<String> {
        self.call("proxyRewrite", json!({ "index": index, "url": url }))
            .ok()?
            .as_str()
            .map(str::to_owned)
    }

    pub fn is_running(&self) -> bool {
        self.sidecar.is_some()
    }

    /// The sidecar's process id, while it runs.
    pub fn pid(&self) -> Option<u32> {
        self.sidecar
            .as_ref()
            .map(|s| s.child.lock().unwrap_or_else(|e| e.into_inner()).id())
    }

    fn call(&self, op: &str, mut params: Value) -> Result<Value, String> {
        let Some(s) = &self.sidecar else {
            return Ok(Value::Null);
        };
        params["op"] = Value::from(op);
        s.call(params)
    }
}

impl PluginHost for VitePlugins {
    fn cache_id(&self) -> &str {
        &self.cache_id
    }

    fn hooks(&self) -> PluginHooks {
        if self.sidecar.is_some() {
            self.manifest.hooks
        } else {
            PluginHooks::default()
        }
    }

    fn attach(&self, server: ServerHandle) {
        if let Some(s) = &self.sidecar {
            let _ = s.server.set(server);
        }
    }

    fn resolve_ids(
        &self,
        stage: Stage,
        importer: &str,
        specifiers: &[String],
    ) -> Result<Vec<Option<PluginResolution>>, String> {
        let v = self.call(
            "resolve",
            json!({ "stage": stage.as_str(), "importer": importer, "specifiers": specifiers }),
        )?;
        let answers = v.as_array().cloned().unwrap_or_default();
        Ok((0..specifiers.len())
            .map(|i| {
                let a = answers.get(i)?;
                if let Some(id) = a["id"].as_str() {
                    Some(PluginResolution::Id(id.to_owned()))
                } else if let Some(u) = a["external"].as_str() {
                    Some(PluginResolution::External(u.to_owned()))
                } else {
                    a["rewrite"]
                        .as_str()
                        .map(|s| PluginResolution::Rewrite(s.to_owned()))
                }
            })
            .collect())
    }

    fn load(&self, id: &str) -> Result<Option<String>, String> {
        let v = self.call("load", json!({ "moduleId": id }))?;
        Ok(v.as_str().map(str::to_owned))
    }

    fn transform(&self, stage: Stage, id: &str, code: &str) -> Result<Option<Transformed>, String> {
        let v = self.call(
            "transform",
            json!({ "stage": stage.as_str(), "moduleId": id, "code": code }),
        )?;
        let Some(code) = v["code"].as_str() else {
            return Ok(None);
        };
        Ok(Some(Transformed {
            code: code.to_owned(),
            maps: v["maps"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|m| m.as_str().map(str::to_owned))
                .collect(),
        }))
    }

    fn transform_index_html(&self, url: &str, file: &Path, html: &str) -> Result<String, String> {
        let v = self.call(
            "html",
            json!({ "url": url, "file": ts_r_hmr::paths::id(file), "html": html }),
        )?;
        Ok(v.as_str().map_or_else(|| html.to_owned(), str::to_owned))
    }

    fn handle_hot_update(
        &self,
        file: &Path,
        module_urls: &[String],
    ) -> Result<Option<Vec<String>>, String> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let v = self.call(
            "hot",
            json!({ "file": ts_r_hmr::paths::id(file), "modules": module_urls, "timestamp": timestamp }),
        )?;
        Ok(v.as_array().map(|a| {
            a.iter()
                .filter_map(|u| u.as_str().map(str::to_owned))
                .collect()
        }))
    }

    fn middleware(&self, req: &Request, body: &[u8]) -> Result<Option<Response>, String> {
        let headers: serde_json::Map<String, Value> = req
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), Value::from(v.as_str())))
            .collect();
        let v = self.call(
            "middleware",
            json!({ "method": req.method, "url": req.target, "headers": headers, "body": base64_encode(body) }),
        )?;
        if v.is_null() {
            return Ok(None);
        }
        let status = v["status"].as_u64().unwrap_or(200).clamp(100, 999) as u16;
        let body = base64_decode(v["body"].as_str().unwrap_or(""));
        let mut resp = Response {
            status,
            headers: Vec::new(),
            body,
        };
        for pair in v["headers"].as_array().into_iter().flatten() {
            let (Some(k), Some(val)) = (pair[0].as_str(), pair[1].as_str()) else {
                continue;
            };
            // The server writes these itself.
            if ["content-length", "connection", "transfer-encoding"]
                .contains(&k.to_ascii_lowercase().as_str())
            {
                continue;
            }
            resp = resp.header_owned(k.to_owned(), val);
        }
        Ok(Some(resp))
    }

    fn client_event(&self, event: &str, data: &Value) {
        if let Some(s) = &self.sidecar {
            s.notify(json!({ "op": "event", "event": event, "data": data }));
        }
    }
}

type Ready = mpsc::Receiver<Result<Value, String>>;
type Waiters = Arc<Mutex<HashMap<u64, mpsc::Sender<Result<Value, String>>>>>;

/// The Node process and its reader thread.
struct Sidecar {
    stdin: Arc<Mutex<ChildStdin>>,
    waiters: Waiters,
    next: AtomicU64,
    child: Mutex<Child>,
    dead: Arc<AtomicBool>,
    server: Arc<OnceLock<ServerHandle>>,
    timeout: Duration,
}

impl std::fmt::Debug for Sidecar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sidecar").finish_non_exhaustive()
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&v| v > 0)
        .unwrap_or(default)
}

/// Writes the embedded script to the temp dir once per content hash.
fn script_path() -> Result<PathBuf, String> {
    let hash = &Key::of(SIDECAR.as_bytes()).to_hex()[..16];
    let path = std::env::temp_dir().join(format!("ts-r-dev-vite-sidecar-{hash}.mjs"));
    if std::fs::read(&path).is_ok_and(|b| b == SIDECAR.as_bytes()) {
        return Ok(path);
    }
    // Unique per call: sidecars started at once in one process must not share the file.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("{}-{n}.tmp", std::process::id()));
    std::fs::write(&tmp, SIDECAR)
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(path)
}

impl Sidecar {
    /// Starts Node. The receiver gets the manifest (or the startup error) once the config
    /// hooks ran; requests sent before that wait in Node's stdin.
    fn spawn(root: &Path, config: &Path, mode: &str) -> Result<(Sidecar, Ready), String> {
        let node = std::env::var_os("TS_R_NODE").unwrap_or_else(|| "node".into());
        let mem = env_u64("TS_R_PLUGIN_MEM_MB", 2048).min(8192);
        let timeout = Duration::from_secs(env_u64("TS_R_PLUGIN_TIMEOUT_S", 30));
        let script = script_path()?;
        let mut child = Command::new(&node)
            .arg(format!("--max-old-space-size={mem}"))
            .arg(&script)
            .current_dir(root)
            .env("TS_R_ROOT", root)
            .env("TS_R_CONFIG", config)
            .env("TS_R_MODE", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                format!(
                    "starting {} for {}: {e} (set TS_R_NODE, or --no-plugins)",
                    node.to_string_lossy(),
                    config.display()
                )
            })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err("sidecar pipes unavailable".to_owned());
        };
        let stdin = Arc::new(Mutex::new(stdin));
        let waiters: Waiters = Arc::default();
        let dead = Arc::new(AtomicBool::new(false));
        let server: Arc<OnceLock<ServerHandle>> = Arc::default();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<Value, String>>();
        {
            let stdin = Arc::clone(&stdin);
            let waiters = Arc::clone(&waiters);
            let dead = Arc::clone(&dead);
            let server = Arc::clone(&server);
            std::thread::Builder::new()
                .name("ts-r-dev-plugins".into())
                .spawn(move || {
                    read_loop(stdout, &stdin, &waiters, &server, ready_tx);
                    dead.store(true, Ordering::SeqCst);
                    for (_, w) in waiters.lock().unwrap_or_else(|e| e.into_inner()).drain() {
                        let _ = w.send(Err("the Vite plugin sidecar exited".to_owned()));
                    }
                })
                .map_err(|e| e.to_string())?;
        }
        let sidecar = Sidecar {
            stdin,
            waiters,
            next: AtomicU64::new(1),
            child: Mutex::new(child),
            dead,
            server,
            timeout,
        };
        Ok((sidecar, ready_rx))
    }

    /// Waits for the manifest.
    fn wait_ready(ready: &Ready, config: &Path) -> Result<Value, String> {
        match ready.recv_timeout(Duration::from_secs(
            env_u64("TS_R_PLUGIN_TIMEOUT_S", 30).max(60),
        )) {
            Ok(Ok(manifest)) => Ok(manifest),
            Ok(Err(message)) => Err(format!("{}: {message}", config.display())),
            Err(_) => Err(format!(
                "{}: the Vite plugin sidecar exited before reporting its plugins (its output is above)",
                config.display()
            )),
        }
    }

    /// Writes one line by `deadline`. A hook that blocks Node's event loop stops it reading
    /// stdin, and a full pipe would block the write (and every caller behind the stdin lock)
    /// for good, so the write runs on its own thread; `Ok(None)` when the deadline passed.
    fn write(&self, msg: &Value, deadline: Instant) -> Result<Option<()>, String> {
        let mut line = msg.to_string();
        line.push('\n');
        let stdin = Arc::clone(&self.stdin);
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("ts-r-dev-plugins-write".into())
            .spawn(move || {
                let r = stdin
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .write_all(line.as_bytes());
                let _ = tx.send(r);
            })
            .map_err(|e| e.to_string())?;
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Ok(())) => Ok(Some(())),
            Ok(Err(e)) => Err(format!("the Vite plugin sidecar is gone: {e}")),
            Err(_) => Ok(None),
        }
    }

    /// Kills an unresponsive sidecar: later calls fail at once, and a write blocked on its
    /// stdin fails instead of waiting forever.
    fn stop(&self) {
        self.dead.store(true, Ordering::SeqCst);
        let _ = self.child.lock().unwrap_or_else(|e| e.into_inner()).kill();
    }

    fn call(&self, mut msg: Value) -> Result<Value, String> {
        if self.dead.load(Ordering::SeqCst) {
            return Err("the Vite plugin sidecar exited".to_owned());
        }
        let deadline = Instant::now() + self.timeout;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        msg["id"] = Value::from(id);
        let (tx, rx) = mpsc::channel();
        self.waiters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        let answer = match self.write(&msg, deadline) {
            Ok(Some(())) => rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .ok(),
            Ok(None) => None,
            Err(e) => {
                self.waiters
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                return Err(e);
            }
        };
        match answer {
            Some(r) => r,
            None => {
                self.waiters
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                self.stop();
                Err(format!(
                    "Vite plugin hook `{}` timed out after {} s; the plugin sidecar was stopped",
                    msg["op"].as_str().unwrap_or("?"),
                    self.timeout.as_secs()
                ))
            }
        }
    }

    fn notify(&self, msg: Value) {
        if !self.dead.load(Ordering::SeqCst)
            && let Ok(None) = self.write(&msg, Instant::now() + self.timeout)
        {
            self.stop();
        }
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn read_loop(
    stdout: std::process::ChildStdout,
    stdin: &Mutex<ChildStdin>,
    waiters: &Waiters,
    server: &OnceLock<ServerHandle>,
    ready: mpsc::Sender<Result<Value, String>>,
) {
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let Ok(line) = line else {
            return;
        };
        let Ok(mut v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(id) = v["id"].as_u64() {
            let w = waiters
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            if let Some(w) = w {
                let r = match v["error"].as_str() {
                    Some(e) => Err(e.to_owned()),
                    None => Ok(v["ok"].take()),
                };
                let _ = w.send(r);
            }
        } else if let Some(rid) = v["rid"].as_u64() {
            // `this.resolve` falling back to the built-in resolver.
            let spec = v["resolve"]["specifier"].as_str().unwrap_or("");
            let importer = v["resolve"]["importer"].as_str().unwrap_or("");
            let result = server
                .get()
                .and_then(|s| (s.resolve)(spec, importer))
                .map_or(Value::Null, |(id, external)| json!([id, external]));
            let mut out = json!({ "rid": rid, "result": result }).to_string();
            out.push('\n');
            let _ = stdin
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .write_all(out.as_bytes());
        } else if !v["ws"].is_null() {
            if let Some(s) = server.get() {
                (s.broadcast)(&v["ws"].to_string());
            }
        } else if !v["ready"].is_null() {
            let _ = ready.send(Ok(v["ready"].take()));
        } else if let Some(f) = v["fatal"].as_str() {
            let _ = ready.send(Err(f.to_owned()));
        }
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base64_decode(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for s in [
            &b""[..],
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"fooba",
            b"foobar",
            &[0, 255, 7],
        ] {
            assert_eq!(base64_decode(&base64_encode(s)), s);
        }
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
    }

    #[test]
    fn dev_dirs_resolve_against_this_root() {
        let root = std::env::temp_dir().join("checkout-b");
        let outside = std::env::temp_dir().join("static");
        let mut config = ts_r_hmr::DevConfig::new(&root);
        let dev = json!({ "envDir": "env", "publicDir": outside });
        let rewrite = |_: usize| -> ts_r_hmr::proxy::Rewrite { unreachable!() };
        apply_dev_options(&dev, &mut config, &rewrite);
        assert_eq!(config.env_dir, Some(root.join("env")));
        assert_eq!(config.public_dir, Some(outside));
    }

    #[test]
    fn assets_include_resolves_against_this_root() {
        let root = std::env::temp_dir().join("checkout-c");
        let mut config = ts_r_hmr::DevConfig::new(&root);
        let dev = json!({ "assetsInclude": [
            { "glob": "**/*.unknown" },
            { "glob": "nested/*.custom" },
            { "regex": "\\.GLTF$", "flags": "i" },
            { "regex": "(" },
        ] });
        let rewrite = |_: usize| -> ts_r_hmr::proxy::Rewrite { unreachable!() };
        let warnings = apply_dev_options(&dev, &mut config, &rewrite);
        let f = &config.assets_include;
        assert!(f.matches(&root.join("a/b.unknown")));
        assert!(f.matches(&root.join("nested/b.custom")));
        assert!(!f.matches(&root.join("other/b.custom")));
        assert!(f.matches(&root.join("m.gltf")));
        assert!(!f.matches(&root.join("m.js")));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].starts_with("`assetsInclude`"), "{warnings:?}");
    }

    #[test]
    fn the_key_covers_plugin_code_the_config_imports() {
        let dir = std::env::temp_dir().join(format!("ts-r-dev-plugin-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("plugins")).expect("dir");
        let config = dir.join("vite.config.ts");
        std::fs::write(
            &config,
            "import { mine } from './plugins/mine';\nexport default { plugins: [mine()] };\n",
        )
        .expect("write");
        std::fs::write(
            dir.join("plugins/mine.ts"),
            "import { word } from '../word.js';\nexport const mine = () => ({ name: 'mine', transform: () => word });\n",
        )
        .expect("write");
        std::fs::write(dir.join("word.js"), "export const word = 'old';\n").expect("write");
        std::fs::write(dir.join("other.js"), "export const x = 1;\n").expect("write");
        let before = manifest_key(&dir, &config, "development");
        std::fs::write(dir.join("other.js"), "export const x = 2;\n").expect("write");
        assert_eq!(manifest_key(&dir, &config, "development"), before);
        std::fs::write(dir.join("word.js"), "export const word = 'new';\n").expect("write");
        assert_ne!(manifest_key(&dir, &config, "development"), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_start_can_await_this_resolve() {
        let node = std::env::var_os("TS_R_NODE").unwrap_or_else(|| "node".into());
        if Command::new(node).arg("--version").output().is_err() {
            eprintln!("skipping: node not found (set TS_R_NODE)");
            return;
        }
        let dir =
            std::env::temp_dir().join(format!("ts-r-dev-plugin-start-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let config = dir.join("vite.config.mjs");
        std::fs::write(
            &config,
            "let seen = 'unset';\n\
             export default { plugins: [{ name: 'start',\n\
               async buildStart() { seen = String(await this.resolve('./src/main.ts')) },\n\
               transform() { return `export default ${JSON.stringify(seen)}` } }] };\n",
        )
        .expect("write");
        let (sidecar, ready) = Sidecar::spawn(&dir, &config, "development").expect("spawn");
        // Sent before the startup hooks finish: answered once they have.
        let out = sidecar
            .call(serde_json::json!({
                "op": "transform", "stage": "post", "moduleId": "/x.ts", "code": "x",
            }))
            .expect("transform");
        assert!(
            out.to_string().contains(r#"export default \"null\""#),
            "{out}"
        );
        Sidecar::wait_ready(&ready, &config).expect("ready");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hung_hook_stops_the_sidecar_instead_of_blocking_writes() {
        let node = std::env::var_os("TS_R_NODE").unwrap_or_else(|| "node".into());
        if Command::new(node).arg("--version").output().is_err() {
            eprintln!("skipping: node not found (set TS_R_NODE)");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ts-r-dev-plugin-hang-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let config = dir.join("vite.config.mjs");
        std::fs::write(
            &config,
            "export default { plugins: [{ name: 'hang', transform() { for (;;) {} } }] };\n",
        )
        .expect("write");
        let (mut sidecar, ready) = Sidecar::spawn(&dir, &config, "development").expect("spawn");
        Sidecar::wait_ready(&ready, &config).expect("ready");
        sidecar.timeout = Duration::from_secs(1);
        let start = Instant::now();
        let call = |code: String| {
            sidecar.call(serde_json::json!({
                "op": "transform", "stage": "post", "moduleId": "/x.ts", "code": code,
            }))
        };
        let first = call("x".to_owned()).expect_err("the hook never returns");
        assert!(first.contains("timed out"), "{first}");
        // More than a pipe buffer: without the deadline on the write this blocks for good.
        call("x".repeat(4 << 20)).expect_err("the sidecar was stopped");
        assert!(start.elapsed() < Duration::from_secs(10));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_round_trips() {
        let m = Manifest {
            plugins: vec!["a".into()],
            replaced: vec!["vite:react-babel".into()],
            hooks: PluginHooks {
                load: true,
                middleware: true,
                ..PluginHooks::default()
            },
            jsx_import_source: Some("@emotion/react".into()),
            jsx_classic: false,
            optimize_include: vec!["react".into()],
            dev: Value::Null,
            warnings: vec![],
        };
        assert_eq!(Manifest::from_json(&m.to_json()), m);
        assert!(m.needs_sidecar());
        assert!(!Manifest::default().needs_sidecar());
    }
}
