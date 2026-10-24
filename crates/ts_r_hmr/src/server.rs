//! The dev server: HTTP/1.1 + WebSocket on one port, one thread per connection.
//!
//! URL space:
//!
//! | URL | Serves |
//! |---|---|
//! | `/`, `/x.html`, unknown HTML navigations | HTML with the client injected |
//! | `/src/a.js` (any file a [`Transform`] handles) | transformed ESM, imports rewritten |
//! | `/x.css` | CSS for `<link>`; with `?import` a self-accepting JS module |
//! | `/x.json?import`, `/x.svg?import`, `?raw`, `?url` | JS module with a default export |
//! | `/@modules/<specifier>?v=<hash>` | a dependency, via the [`DepBundler`] |
//! | `/@fs/<abs path>` | a module outside the root that the graph imports |
//! | `/@id/<plugin id>` | a plugin module (virtual id, `x.svg?react`), see [`crate::plugin`] |
//! | `/@ts-r/deps/<name>[.map]` | a pre-bundled dependency file, or its source map |
//! | `/@ts-r/up/<n>/<path>` | a file inside `node_modules` above the root (dependency CSS assets) |
//! | `/@ts-r/client` (`/@vite/client`) | the client runtime |
//! | `/@ts-r/ws` (any upgrade) | the HMR WebSocket |
//! | `/@ts-r/stats` | latency stats JSON |
//! | `/@ts-r/ping` | 204, for reconnecting clients |
//! | `/@ts-r/env` (`/@vite/env`) | the global `define`s (also prepended to the client) |
//! | anything else | static file from `public/` (exact case), then the root (not `.env*`, keys, `.git`), then plugin middlewares, then the HTML fallback (`<path>.html`, else `index.html`) |
//!
//! Before routing: `server.cors` (preflights answered), `server.proxy` (matching requests
//! forwarded), then `base` is stripped (`/` redirects to the base; URLs outside the base
//! are still served). Responses that aren't proxied get `server.headers`.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{self, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ts_r_cache::{Key, KeyBuilder};

use crate::cache::TransformCache;
use crate::cors::Cors;
use crate::deps::{DepEntry, Deps};
use crate::env::{Env, EnvOptions};
use crate::glob::GlobWatch;
use crate::graph::{ImportedBindings, ModuleGraph, ModuleId, ModuleKind, ModuleNode};
use crate::http::{self, Request, Response};
use crate::net::Duplex;
use crate::plugin::{
    self as plugins, PluginHooks, PluginHost, PluginResolution, ServerHandle, Stage,
};
use crate::prebundle::{DepBundler, DepRequest, UnbundledDeps};
use crate::proxy::{self, ProxyRule};
use crate::render::{self, JS_CONTENT_TYPE};
use crate::resolve::{Resolve, Resolved, SimpleResolver, normalize};
use crate::scan::{self, HotInfo};
use crate::timing::Timings;
use crate::transform::{
    Mode, PassthroughJs, Transform, TransformError, TransformInput, TransformOutput,
};
use crate::watch::{self, DebounceOptions, FileWatcher, WatchAdder};
use crate::ws;

mod app_css;

const CLIENT_JS: &str = include_str!("client/client.js");

/// Where transform results are cached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CacheMode {
    /// Memory plus `ts_r_cache` in the default cache root (`~/.cache/ts-r`).
    Default,
    /// Memory plus `ts_r_cache` under this root.
    Dir(PathBuf),
    /// Memory only.
    Memory,
}

/// A log sink for update timings and server messages.
pub type Logger = Arc<dyn Fn(&str) + Send + Sync>;

/// Dev server configuration.
#[derive(Clone)]
pub struct DevConfig {
    pub root: PathBuf,
    pub host: String,
    /// 0 = any free port.
    pub port: u16,
    /// Fail instead of trying the next port when `port` is taken.
    pub strict_port: bool,
    /// Tried in order; the first whose `handles` matches serves the file.
    pub transforms: Vec<Arc<dyn Transform>>,
    /// Defaults to [`SimpleResolver`] rooted at `root`.
    pub resolver: Option<Arc<dyn Resolve>>,
    pub dep_bundler: Arc<dyn DepBundler>,
    /// Bare specifiers pre-bundled even if the scan doesn't find them (Vite's
    /// `optimizeDeps.include`), resolved from the root.
    pub optimize_include: Vec<String>,
    /// Ignore the cached dependency batch and pre-bundle again (Vite's `--force`).
    pub force_deps: bool,
    /// HTML inserted into every page right after the client `<script>` (before the app's
    /// own scripts), e.g. a framework's runtime preamble.
    pub html_inject: Vec<String>,
    /// Modules served from memory: `(url, code)`. Imports are resolved from the root and
    /// rewritten like a source module's.
    pub virtual_modules: Vec<(String, String)>,
    /// Vite-style plugin hooks (see [`crate::plugin`]); `None` = no plugins.
    pub plugins: Option<Arc<dyn PluginHost>>,
    pub cache: CacheMode,
    pub watch: bool,
    pub debounce: DebounceOptions,
    pub logger: Option<Logger>,
    /// `mode` (`MODE`, which `.env.[mode]` files load).
    pub mode: String,
    /// The public base path (`/` or `/app/`; see [`normalize_base`]).
    pub base: String,
    /// Where the `.env*` files are (relative = from the root); `None` = don't load them.
    pub env_dir: Option<PathBuf>,
    /// `envPrefix`: variables exposed on `import.meta.env`.
    pub env_prefix: Vec<String>,
    /// `define`: key → JS expression text.
    pub define: Vec<(String, String)>,
    /// Process variables the config file set, used over the server's own for `.env`
    /// expansion and prefixed variables.
    pub config_env: Vec<(String, String)>,
    /// `publicDir` (relative = from the root); `None` = none.
    pub public_dir: Option<PathBuf>,
    /// `assetsInclude`: more files served as assets (a URL), not as plugin modules.
    pub assets_include: crate::glob::PathFilter,
    /// `build.assetsInlineLimit`: SVGs smaller than this many bytes are inlined as data
    /// URLs (the only assets Vite inlines in dev).
    pub assets_inline_limit: u64,
    /// `experimental.hmrPartialAccept`: an update of a module that accepts some of its
    /// exports (`import.meta.hot.acceptExports`) skips the importers that only use those.
    pub hmr_partial_accept: bool,
    /// `server.headers`, added to every response that isn't proxied.
    pub headers: Vec<(String, String)>,
    pub cors: Cors,
    pub proxy: Vec<ProxyRule>,
    /// `server.https`: serve TLS with this PEM certificate chain and key.
    pub https: Option<Https>,
}

/// PEM certificate chain and private key for `server.https`.
#[derive(Clone)]
pub struct Https {
    pub cert: Vec<u8>,
    pub key: Vec<u8>,
}

impl std::fmt::Debug for Https {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Https").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for DevConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DevConfig")
            .field("root", &self.root)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("transforms", &self.transforms)
            .field("resolver", &self.resolver)
            .field("dep_bundler", &self.dep_bundler)
            .field("optimize_include", &self.optimize_include)
            .field("force_deps", &self.force_deps)
            .field("plugins", &self.plugins)
            .field("cache", &self.cache)
            .field("watch", &self.watch)
            .field("debounce", &self.debounce)
            .field("mode", &self.mode)
            .field("base", &self.base)
            .field("env_dir", &self.env_dir)
            .field("env_prefix", &self.env_prefix)
            .field("define", &self.define)
            .field("public_dir", &self.public_dir)
            .field("assets_include", &self.assets_include)
            .field("headers", &self.headers)
            .field("cors", &self.cors)
            .field("proxy", &self.proxy)
            .field("https", &self.https.is_some())
            .finish_non_exhaustive()
    }
}

impl DevConfig {
    pub fn new(root: impl Into<PathBuf>) -> DevConfig {
        DevConfig {
            root: root.into(),
            host: "127.0.0.1".to_owned(),
            port: 5173,
            strict_port: false,
            transforms: vec![Arc::new(PassthroughJs)],
            resolver: None,
            dep_bundler: Arc::new(UnbundledDeps),
            optimize_include: Vec::new(),
            force_deps: false,
            html_inject: Vec::new(),
            virtual_modules: Vec::new(),
            plugins: None,
            cache: CacheMode::Default,
            watch: true,
            debounce: DebounceOptions::default(),
            logger: None,
            mode: "development".to_owned(),
            base: "/".to_owned(),
            env_dir: Some(PathBuf::new()),
            env_prefix: vec!["VITE_".to_owned()],
            define: Vec::new(),
            config_env: Vec::new(),
            public_dir: Some(PathBuf::from("public")),
            assets_include: crate::glob::PathFilter::default(),
            assets_inline_limit: crate::assets::SVG_INLINE_LIMIT as u64,
            hmr_partial_accept: false,
            headers: Vec::new(),
            cors: Cors::default(),
            proxy: Vec::new(),
            https: None,
        }
    }
}

/// `base` in the form the server uses: `/` or `/x/` (a full URL's path; `./` and `""`
/// mean `/` in dev).
pub fn normalize_base(base: &str) -> String {
    let path = match base.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("/", |i| &rest[i..]),
        None => base,
    };
    let path = path.trim_start_matches('.').trim_matches('/');
    if path.is_empty() {
        "/".to_owned()
    } else {
        format!("/{path}/")
    }
}

/// `url` with `base` in front when it is root-absolute (not `//host`).
pub fn with_base(base: &str, url: &str) -> String {
    if base == "/" || !url.starts_with('/') || url.starts_with("//") {
        return url.to_owned();
    }
    format!("{base}{}", &url[1..])
}

/// A transformed module with its imports resolved; cached on the graph node until the
/// file's content hash changes.
#[derive(Debug)]
pub struct Prepared {
    /// BLAKE3 of the source bytes.
    pub hash: Key,
    pub output: Arc<TransformOutput>,
    pub hot: HotInfo,
    /// Parallel to `output.imports`.
    pub targets: Vec<Target>,
    /// Parallel to `hot.accepted`.
    pub accept_targets: Vec<Target>,
    /// Parallel to `hot.asset_urls` (`new URL('./x.png', import.meta.url)`): the URL each
    /// literal becomes, without the base; `None` leaves the literal alone.
    pub asset_urls: Vec<Option<String>>,
}

/// What an import specifier was rewritten to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A graph module; gets `?t=` after hot updates. `query` is extra query (`import`, `raw`).
    Module { id: ModuleId, query: String },
    /// A fixed URL (external, non-JS dependency).
    Url(String),
    /// A JS dependency: its URL comes from the current dependency batch at render time.
    Dep(DepEntry),
}

/// A module-level failure, shown in the overlay.
#[derive(Clone, Debug)]
pub(crate) struct ModuleError {
    pub message: String,
    pub file: PathBuf,
    pub url: String,
    pub loc: Option<(u32, u32)>,
    pub frame: String,
}

impl ModuleError {
    pub fn payload(&self) -> serde_json::Value {
        let file = self.file.to_string_lossy();
        serde_json::json!({
            "type": "error",
            "err": {
                "message": self.message,
                "stack": "",
                "id": self.url,
                "frame": self.frame,
                "plugin": "ts-r",
                "loc": self.loc.map(|(line, column)| serde_json::json!({
                    "file": file, "line": line, "column": column
                })).unwrap_or_else(|| serde_json::json!({ "file": file })),
            }
        })
    }
}

/// State shared by connection threads and the watcher.
pub(crate) struct Shared {
    pub root: PathBuf,
    pub public_dir: Option<PathBuf>,
    pub assets_include: crate::glob::PathFilter,
    pub assets_inline_limit: u64,
    pub hmr_partial_accept: bool,
    pub base: String,
    pub env_options: EnvOptions,
    /// Reloaded when an env file changes.
    pub env: RwLock<Arc<Env>>,
    pub headers: Vec<(String, String)>,
    pub cors: Cors,
    pub proxy: Vec<ProxyRule>,
    /// The client runtime with the base and the global `define`s filled in.
    pub client_js: RwLock<Arc<String>>,
    /// `import.meta.glob` patterns per importing module: a matching file appearing or
    /// disappearing re-prepares the importer.
    pub glob_watches: Mutex<HashMap<ModuleId, Vec<GlobWatch>>>,
    /// The same for pre-bundled files (by name, expanded when served): a matching file
    /// appearing or disappearing reloads the page.
    pub dep_glob_watches: Mutex<BTreeMap<String, Vec<GlobWatch>>>,
    #[cfg(feature = "tls")]
    pub tls: Option<Arc<rustls::ServerConfig>>,
    pub transforms: Vec<Arc<dyn Transform>>,
    pub resolver: Arc<dyn Resolve>,
    pub dep_bundler: Arc<dyn DepBundler>,
    pub cache: TransformCache,
    pub graph: Mutex<ModuleGraph>,
    pub hub: Hub,
    pub timings: Timings,
    pub dep_version: String,
    pub dep_cache: Mutex<HashMap<String, Arc<(String, String)>>>,
    pub deps: Deps,
    pub optimize_include: Vec<String>,
    pub html_inject: String,
    pub virtual_modules: HashMap<String, String>,
    pub plugins: Option<Arc<dyn PluginHost>>,
    /// `plugins`' hooks (all false without plugins).
    pub plugin_hooks: PluginHooks,
    /// The error currently shown (replayed to clients that connect later), and the module.
    pub last_error: Mutex<Option<(String, serde_json::Value)>>,
    pub update_seq: AtomicU64,
    pub last_timestamp: Mutex<u64>,
    /// Serializes change batches and invalidations.
    pub change_lock: Mutex<()>,
    pub shutdown: AtomicBool,
    pub logger: Option<Logger>,
    /// Adds directories outside the root to the file watcher (linked packages).
    pub watch_adder: OnceLock<WatchAdder>,
    /// Directories outside the root already being watched.
    pub watched_outside: Mutex<HashMap<PathBuf, bool>>,
    /// When each changed file last changed (an update's timestamp): stylesheets add it to
    /// the URLs of the files they reference, so the browser fetches the new version.
    pub changed_at: Mutex<HashMap<PathBuf, u64>>,
    /// The files each page's `<style>` blocks and `style` attributes read or reference: a
    /// change to one reloads the page.
    pub html_deps: Mutex<HashMap<PathBuf, Vec<PathBuf>>>,
}

impl Shared {
    pub fn log(&self, line: &str) {
        if let Some(l) = &self.logger {
            l(line);
        }
    }

    pub fn env(&self) -> Arc<Env> {
        Arc::clone(&self.env.read().unwrap_or_else(|e| e.into_inner()))
    }

    /// Reloads the env files (one changed) and the client that carries the `define`s.
    pub fn reload_env(&self) {
        let env = Arc::new(crate::env::load(&self.env_options));
        *self.client_js.write().unwrap_or_else(|e| e.into_inner()) =
            Arc::new(client_js(&self.base, &env));
        *self.env.write().unwrap_or_else(|e| e.into_inner()) = env;
    }

    /// A root-absolute URL as the browser must request it (with the base).
    /// The timestamp of the update that last changed `file`, if one did.
    pub(crate) fn changed_at(&self, file: &Path) -> Option<u64> {
        self.changed_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(file)
            .copied()
    }

    pub fn public_url(&self, url: &str) -> String {
        with_base(&self.base, url)
    }

    fn client_url(&self) -> String {
        self.public_url(render::CLIENT_URL)
    }

    fn tls_on(&self) -> bool {
        #[cfg(feature = "tls")]
        {
            self.tls.is_some()
        }
        #[cfg(not(feature = "tls"))]
        {
            false
        }
    }
}

/// The client runtime for `base`, with the global `define`s first.
fn client_js(base: &str, env: &Env) -> String {
    let js = CLIENT_JS
        .replacen(
            "const base = \"/\";",
            &format!("const base = {};", render::js_string(base)),
            1,
        )
        .replacen(
            "const wsPath = \"/@ts-r/ws\";",
            &format!(
                "const wsPath = {};",
                render::js_string(&with_base(base, "/@ts-r/ws"))
            ),
            1,
        )
        .replacen(
            "const pingPath = \"/@ts-r/ping\";",
            &format!(
                "const pingPath = {};",
                render::js_string(&with_base(base, "/@ts-r/ping"))
            ),
            1,
        );
    if env.globals.is_empty() {
        js
    } else {
        format!("{}\n{js}", env.globals)
    }
}

/// Connected HMR sockets.
type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

#[derive(Default)]
pub(crate) struct Hub {
    clients: Mutex<Vec<(u64, SharedWriter, TcpStream)>>,
    next: AtomicU64,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub").field("clients", &self.len()).finish()
    }
}

impl Hub {
    fn add(&self, writer: Box<dyn Write + Send>, tcp: TcpStream) -> (u64, SharedWriter) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let s = Arc::new(Mutex::new(writer));
        self.clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((id, Arc::clone(&s), tcp));
        (id, s)
    }

    fn remove(&self, id: u64) {
        self.clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(i, _, _)| *i != id);
    }

    pub fn len(&self) -> usize {
        self.clients.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Sends a text message to every client; drops clients whose socket failed.
    pub fn broadcast(&self, text: &str) -> usize {
        let frame = ws::encode_frame(ws::Opcode::Text, text.as_bytes(), None);
        let clients: Vec<(u64, SharedWriter)> = self
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(i, w, _)| (*i, Arc::clone(w)))
            .collect();
        let mut sent = 0;
        for (id, s) in clients {
            let ok = s
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .write_all(&frame)
                .is_ok();
            if ok {
                sent += 1;
            } else {
                self.remove(id);
            }
        }
        sent
    }

    fn close_all(&self) {
        for (_, _, tcp) in self
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            let _ = tcp.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// A running dev server. Dropping it (or calling [`DevServer::shutdown`]) stops it.
#[derive(Debug)]
pub struct DevServer {
    shared: Arc<Shared>,
    addr: SocketAddr,
    _watcher: Option<FileWatcher>,
    accept_thread: Option<thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl DevServer {
    /// Binds, starts the watcher and the accept loop, and returns immediately.
    pub fn start(config: DevConfig) -> io::Result<DevServer> {
        let root = crate::paths::canonicalize(&config.root)?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} is not a directory", root.display()),
            ));
        }
        crate::env::check_prefixes(&config.env_prefix)
            .map_err(|m| io::Error::new(io::ErrorKind::InvalidInput, m))?;
        #[cfg(feature = "tls")]
        let tls = match &config.https {
            Some(h) => Some(
                crate::net::tls::server_config(&h.cert, &h.key)
                    .map_err(|m| io::Error::new(io::ErrorKind::InvalidInput, m))?,
            ),
            None => None,
        };
        #[cfg(not(feature = "tls"))]
        if config.https.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "server.https needs ts_r_hmr's `tls` feature",
            ));
        }
        let base = normalize_base(&config.base);
        let env_options = EnvOptions {
            dir: config.env_dir.as_ref().map(|d| root.join(d)),
            mode: config.mode.clone(),
            prefixes: config.env_prefix.clone(),
            base: base.clone(),
            define: config.define.clone(),
            process_env: config.config_env.clone(),
        };
        let env = crate::env::load(&env_options);
        let client = client_js(&base, &env);
        let listener = bind(&config.host, config.port, config.strict_port)?;
        let addr = listener.local_addr()?;
        let cache = match &config.cache {
            CacheMode::Default => TransformCache::persistent(None),
            CacheMode::Dir(d) => TransformCache::persistent(Some(d)),
            CacheMode::Memory => TransformCache::in_memory(),
        };
        let resolver = config
            .resolver
            .clone()
            .unwrap_or_else(|| Arc::new(SimpleResolver::new(root.clone())));
        let dep_version = dep_version(&root, config.dep_bundler.id());
        let deps = if config.dep_bundler.bundle_batch(&[]).is_some() {
            Deps::new(
                &root,
                &config.cache,
                config.dep_bundler.id(),
                &config.optimize_include,
            )
        } else {
            Deps::off()
        };
        let shared = Arc::new(Shared {
            public_dir: config.public_dir.as_ref().map(|d| root.join(d)),
            assets_include: config.assets_include.clone(),
            assets_inline_limit: config.assets_inline_limit,
            hmr_partial_accept: config.hmr_partial_accept,
            base,
            env_options,
            env: RwLock::new(Arc::new(env)),
            headers: config.headers.clone(),
            cors: config.cors.clone(),
            proxy: config.proxy.clone(),
            client_js: RwLock::new(Arc::new(client)),
            glob_watches: Mutex::default(),
            dep_glob_watches: Mutex::default(),
            #[cfg(feature = "tls")]
            tls,
            root: root.clone(),
            transforms: config.transforms.clone(),
            resolver,
            dep_bundler: Arc::clone(&config.dep_bundler),
            cache,
            graph: Mutex::new(ModuleGraph::new()),
            hub: Hub::default(),
            timings: Timings::default(),
            dep_version,
            dep_cache: Mutex::default(),
            deps,
            optimize_include: config.optimize_include.clone(),
            html_inject: config.html_inject.concat(),
            virtual_modules: config.virtual_modules.iter().cloned().collect(),
            plugins: config.plugins.clone(),
            plugin_hooks: config
                .plugins
                .as_ref()
                .map(|p| p.hooks())
                .unwrap_or_default(),
            last_error: Mutex::new(None),
            update_seq: AtomicU64::new(1),
            last_timestamp: Mutex::new(0),
            change_lock: Mutex::new(()),
            shutdown: AtomicBool::new(false),
            logger: config.logger.clone(),
            watch_adder: OnceLock::new(),
            watched_outside: Mutex::default(),
            changed_at: Mutex::default(),
            html_deps: Mutex::default(),
        });
        let watcher = if config.watch {
            let s = Arc::clone(&shared);
            Some(
                watch::watch(&root, config.debounce, move |batch| {
                    crate::hmr::on_batch(&s, batch);
                })
                .map_err(|e| io::Error::other(format!("file watcher: {e}")))?,
            )
        } else {
            None
        };
        if let Some(w) = &watcher {
            let _ = shared.watch_adder.set(w.adder());
            if let Some(dir) = &shared.env_options.dir
                && !dir.starts_with(&shared.root)
                && dir.is_dir()
            {
                w.adder().add(dir, false);
            }
        }
        if let Some(p) = &shared.plugins {
            p.attach(server_handle(&shared));
        }
        crate::deps::start(&shared, config.force_deps);
        let s = Arc::clone(&shared);
        let accept_thread = thread::Builder::new()
            .name("ts-r-dev-accept".into())
            .spawn(move || accept_loop(s, listener))?;
        Ok(DevServer {
            shared,
            addr,
            _watcher: watcher,
            accept_thread: Some(accept_thread),
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http(s)://host:port/base/` (a wildcard bind shows as `localhost`).
    pub fn url(&self) -> String {
        let scheme = if self.shared.tls_on() {
            "https"
        } else {
            "http"
        };
        let host = if self.addr.ip().is_unspecified() {
            "localhost".to_owned()
        } else if self.addr.is_ipv6() {
            format!("[{}]", self.addr.ip())
        } else {
            self.addr.ip().to_string()
        };
        format!("{scheme}://{host}:{}{}", self.addr.port(), self.shared.base)
    }

    /// The canonical root.
    pub fn root(&self) -> &Path {
        &self.shared.root
    }

    /// Feeds a change batch as if the watcher had produced it (for tests and tools that
    /// do their own watching).
    pub fn notify_changes(&self, batch: watch::ChangeBatch) {
        crate::hmr::on_batch(&self.shared, batch);
    }

    /// `/@ts-r/stats` as JSON.
    pub fn stats_json(&self) -> serde_json::Value {
        self.shared.timings.stats_json()
    }

    /// Number of modules in the graph.
    pub fn module_count(&self) -> usize {
        self.shared
            .graph
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Number of connected HMR clients.
    pub fn client_count(&self) -> usize {
        self.shared.hub.len()
    }

    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if self.shared.shutdown.swap(true, Ordering::SeqCst) {
            return;
        }
        // Wake the accept loop.
        let mut wake = self.addr;
        if wake.ip().is_unspecified() {
            wake.set_ip(if wake.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            });
        }
        let _ = TcpStream::connect_timeout(&wake, Duration::from_millis(200));
        if let Some(t) = self.accept_thread.take() {
            let _ = t.join();
        }
        self.shared.hub.close_all();
        self.shared.cache.flush();
    }
}

impl Drop for DevServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn bind(host: &str, port: u16, strict: bool) -> io::Result<TcpListener> {
    let tries = if strict || port == 0 { 1 } else { 20 };
    let mut last = None;
    for p in 0..tries {
        match TcpListener::bind((host, port + p)) {
            Ok(l) => return Ok(l),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("bind failed")))
}

/// `?v=` for dependency URLs: lockfile (or package.json) + bundler identity.
fn dep_version(root: &Path, bundler_id: &str) -> String {
    let mut b = ts_r_cache::KeyBuilder::new("ts-r-dev/deps");
    b.str(bundler_id);
    for name in [
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "bun.lock",
        "bun.lockb",
        "package.json",
    ] {
        if let Ok(bytes) = fs::read(root.join(name)) {
            b.str(name).bytes(&bytes);
        }
    }
    b.key().to_hex()[..8].to_owned()
}

fn accept_loop(shared: Arc<Shared>, listener: TcpListener) {
    for stream in listener.incoming() {
        if shared.shutdown.load(Ordering::SeqCst) {
            return;
        }
        let Ok(stream) = stream else { continue };
        let s = Arc::clone(&shared);
        let _ = thread::Builder::new()
            .name("ts-r-dev-conn".into())
            .spawn(move || {
                let _ = handle_connection(&s, stream);
            });
    }
}

fn handle_connection(shared: &Arc<Shared>, stream: TcpStream) -> io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    #[cfg(feature = "tls")]
    let conn = match &shared.tls {
        Some(cfg) => crate::net::tls::accept(stream, cfg)?,
        None => Duplex::plain(stream)?,
    };
    #[cfg(not(feature = "tls"))]
    let conn = Duplex::plain(stream)?;
    let Duplex {
        reader,
        mut writer,
        tcp,
    } = conn;
    let mut reader = BufReader::new(reader);
    loop {
        let mut req = match http::read_request(&mut reader) {
            Ok(Some(r)) => r,
            Ok(None) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                let _ = Response::text(400, e.to_string()).write_to(&mut writer, false, false);
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        if shared.shutdown.load(Ordering::SeqCst) {
            return Ok(());
        }
        if req.is_websocket_upgrade() {
            let hmr = req
                .header("sec-websocket-protocol")
                .is_some_and(|p| p.split(',').any(|p| p.trim() == "vite-hmr"));
            if !hmr && let Some(rule) = proxy::find(&shared.proxy, &req.target, true) {
                if let Err(e) = proxy::tunnel(rule, &req, &mut reader, writer, &tcp) {
                    shared.log(&format!("ws proxy error: {}\n{e}", req.target));
                }
                return Ok(());
            }
            return websocket(shared, reader, writer, tcp, &req);
        }
        let head_only = req.method == "HEAD";
        if let Some(resp) = shared.cors.preflight(&req) {
            if !http::drain_body(&mut reader, &req)? {
                resp.write_to(&mut writer, false, false)?;
                return Ok(());
            }
            resp.write_to(&mut writer, false, req.keep_alive())?;
            if req.keep_alive() {
                continue;
            }
            return Ok(());
        }
        if let Some(rule) = proxy::find(&shared.proxy, &req.target, false) {
            let mut cors = Response::new(200, "", Vec::new());
            cors.headers.clear();
            shared.cors.apply(&req, &mut cors);
            let extra: Vec<(String, String)> = cors
                .headers
                .into_iter()
                .map(|(k, v)| (k.into_owned(), v))
                .collect();
            match proxy::forward(rule, &req, &mut reader, &mut writer, &extra) {
                Ok(true) => continue,
                Ok(false) => return Ok(()),
                Err(e) => {
                    shared.log(&format!("http proxy error: {}\n{e}", req.target));
                    return Ok(());
                }
            }
        }
        if shared.base != "/" {
            match strip_base(&shared.base, &mut req) {
                BaseMatch::Inside => {}
                // Outside the base: a 404 that names the URL meant (Vite's base
                // middleware, `vite: packages/vite/src/node/server/middlewares/base.ts`).
                // The server's own `/@...` URLs are still served.
                BaseMatch::Outside if !req.path.starts_with("/@") => {
                    let _ = http::drain_body(&mut reader, &req)?;
                    let html = req
                        .header("accept")
                        .is_some_and(|a| a.contains("text/html"));
                    let suggestion = format!("{}{}", shared.base, req.path.trim_start_matches('/'));
                    let (kind, body) = if html {
                        let s = http::html_escape(&suggestion);
                        (
                            "text/html",
                            format!(
                                "The server is configured with a public base URL of {} - did you mean to visit <a href=\"{s}\">{s}</a> instead?",
                                http::html_escape(&shared.base)
                            ),
                        )
                    } else {
                        (
                            "text/plain",
                            format!(
                                "The server is configured with a public base URL of {} - did you mean to visit {suggestion} instead?",
                                shared.base
                            ),
                        )
                    };
                    let mut resp = Response::new(404, kind, body.into_bytes());
                    shared.cors.apply(&req, &mut resp);
                    resp.write_to(&mut writer, head_only, req.keep_alive())?;
                    if req.keep_alive() {
                        continue;
                    }
                    return Ok(());
                }
                BaseMatch::Outside => {}
                BaseMatch::Redirect => {
                    let _ = http::drain_body(&mut reader, &req)?;
                    let mut resp = Response {
                        status: 302,
                        headers: Vec::new(),
                        body: Vec::new(),
                    }
                    .header("Location", shared.base.clone());
                    shared.cors.apply(&req, &mut resp);
                    resp.write_to(&mut writer, head_only, req.keep_alive())?;
                    if req.keep_alive() {
                        continue;
                    }
                    return Ok(());
                }
            }
        }
        let mut keep_alive = req.keep_alive();
        let get = req.method == "GET" || head_only;
        let mut resp = if !get && shared.plugin_hooks.middleware {
            // Other methods only reach plugin middlewares (API mocks, proxies).
            match http::read_body(&mut reader, &req)? {
                Some(body) => plugin_middleware(shared, &req, &body)
                    .unwrap_or_else(|| Response::text(404, format!("Not found: {}", req.path))),
                None => {
                    keep_alive = false;
                    Response::text(413, "request body too large (1 MiB, Content-Length only)")
                }
            }
        } else {
            if !http::drain_body(&mut reader, &req)? {
                keep_alive = false;
            }
            if get {
                route(shared, &req)
            } else {
                Response::text(405, "ts-r-dev serves GET and HEAD only")
                    .header("Allow", "GET, HEAD")
            }
        };
        finish_response(shared, &req, &mut resp);
        resp.write_to(&mut writer, head_only, keep_alive)?;
        if !keep_alive {
            return Ok(());
        }
    }
}

/// `server.headers` (replacing same-named headers) and CORS on a response we produced.
fn finish_response(shared: &Shared, req: &Request, resp: &mut Response) {
    for (k, v) in &shared.headers {
        resp.headers.retain(|(h, _)| !h.eq_ignore_ascii_case(k));
        resp.headers
            .push((std::borrow::Cow::Owned(k.clone()), v.clone()));
    }
    shared.cors.apply(req, resp);
}

enum BaseMatch {
    Inside,
    Outside,
    Redirect,
}

/// Removes `base` from the request path and target. `/` (and nothing else outside the
/// base) redirects to the base.
fn strip_base(base: &str, req: &mut Request) -> BaseMatch {
    let bare = base.trim_end_matches('/');
    let rest = if req.path == bare {
        Some(String::new())
    } else {
        req.path.strip_prefix(base).map(str::to_owned)
    };
    match rest {
        Some(rest) => {
            req.path = format!("/{rest}");
            let target_rest = req
                .target
                .strip_prefix(base)
                .or_else(|| req.target.strip_prefix(bare))
                .unwrap_or("")
                .trim_start_matches('/');
            req.target = format!("/{target_rest}");
            BaseMatch::Inside
        }
        None if req.path == "/" => BaseMatch::Redirect,
        None => BaseMatch::Outside,
    }
}

fn websocket(
    shared: &Arc<Shared>,
    mut reader: BufReader<Box<dyn Read + Send>>,
    mut writer: Box<dyn Write + Send>,
    tcp: TcpStream,
    req: &Request,
) -> io::Result<()> {
    let Some(resp) = ws::handshake_response(req) else {
        return Response::text(400, "bad websocket handshake").write_to(&mut writer, false, false);
    };
    writer.write_all(resp.as_bytes())?;
    writer.flush()?;
    tcp.set_write_timeout(Some(Duration::from_secs(2)))?;
    tcp.set_read_timeout(None)?;
    // Joining the hub and replaying the last error happen under the error's lock, which
    // `report_error` holds while it broadcasts: an error reaches this client either live or as
    // the replay, never both, and never replayed after the update that cleared it.
    let last_error = shared.last_error.lock().unwrap_or_else(|e| e.into_inner());
    let (id, out) = shared.hub.add(writer, tcp);
    let send = |text: &str| -> io::Result<()> {
        let frame = ws::encode_frame(ws::Opcode::Text, text.as_bytes(), None);
        out.lock()
            .unwrap_or_else(|e| e.into_inner())
            .write_all(&frame)
    };
    send(r#"{"type":"connected"}"#)?;
    if let Some((_, payload)) = last_error.as_ref() {
        send(&payload.to_string())?;
    }
    drop(last_error);
    crate::hmr::on_client_lifecycle(shared, true);
    let mut partial = None;
    let result = loop {
        match ws::read_message(&mut reader, &mut partial) {
            Ok(ws::Message::Text(t)) => crate::hmr::on_client_message(shared, &t),
            Ok(ws::Message::Ping(p)) => {
                let frame = ws::encode_frame(ws::Opcode::Pong, &p, None);
                let _ = out
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .write_all(&frame);
            }
            Ok(ws::Message::Close) => {
                let frame = ws::encode_frame(ws::Opcode::Close, &[], None);
                let _ = out
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .write_all(&frame);
                break Ok(());
            }
            Ok(_) => {}
            Err(e) => break Err(e),
        }
    };
    shared.hub.remove(id);
    crate::hmr::on_client_lifecycle(shared, false);
    result
}

// ---------------------------------------------------------------------------------------
// Routing

fn route(shared: &Shared, req: &Request) -> Response {
    let path = req.path.as_str();
    match path {
        "/@ts-r/client" | "/@vite/client" => {
            let js = Arc::clone(&shared.client_js.read().unwrap_or_else(|e| e.into_inner()));
            return cacheable(req, JS_CONTENT_TYPE, js.as_bytes().to_vec(), "no-cache");
        }
        "/@ts-r/env" | "/@vite/env" => {
            let env = shared.env();
            return cacheable(
                req,
                JS_CONTENT_TYPE,
                format!("{}\n", env.globals).into_bytes(),
                "no-cache",
            );
        }
        "/@ts-r/ping" => return Response::new(204, "text/plain", Vec::new()),
        "/@ts-r/stats" => {
            return Response::new(
                200,
                "application/json",
                shared.timings.stats_json().to_string(),
            )
            .header("Cache-Control", "no-store");
        }
        _ => {}
    }
    if let Some(spec) = path.strip_prefix("/@modules/") {
        return serve_dep(shared, req, spec);
    }
    if let Some(name) = path.strip_prefix(crate::deps::DEPS_PREFIX) {
        if let Some(file) = name.strip_suffix(".map") {
            return match crate::deps::map(shared, file) {
                Some(m) => cacheable(req, "application/json", m.as_bytes().to_vec(), "no-cache"),
                None => Response::text(404, format!("no source map for {file}")),
            };
        }
        return match crate::deps::file(shared, name, req.query_value("v")) {
            Some((code, immutable)) => {
                let (code, globbed) = dep_globs(shared, name, &code);
                cacheable(
                    req,
                    JS_CONTENT_TYPE,
                    code.into_bytes(),
                    // Its glob results change without its version changing.
                    if immutable && !globbed {
                        "max-age=31536000, immutable"
                    } else {
                        "no-cache"
                    },
                )
            }
            None => Response::text(404, format!("no pre-bundled dependency file {name}")),
        };
    }
    if let Some(code) = shared.virtual_modules.get(path) {
        return serve_virtual(shared, req, path, code);
    }
    if let Some(rest) = path.strip_prefix("/@ts-r/up/") {
        return serve_up(shared, req, rest);
    }
    if let Some(rest) = path.strip_prefix("/@id/") {
        // Only ids the graph knows (a module imported them), like `/@fs/`.
        let file = PathBuf::from(plugins::id_from_url_path(rest));
        let url = {
            let g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
            g.by_file(&file).map(|id| g.node(id).url.clone())
        };
        return match url {
            Some(url) => serve_script(shared, req, &file, &url),
            None => Response::text(404, format!("unknown plugin module {path}")),
        };
    }
    if let Some(abs) = path.strip_prefix("/@fs/") {
        let file = normalize(&crate::paths::from_fs_url(abs));
        let known = shared
            .graph
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .by_file(&file)
            .is_some();
        if !known {
            return Response::text(403, "/@fs/ only serves modules the app imports");
        }
        return serve_file(shared, req, &file, path);
    }
    let Some(rel) = safe_relative(path) else {
        return Response::text(403, "path escapes the project root");
    };
    // `//x` names no file (Vite serves no files for it either): straight to the fallback.
    let files = !path.starts_with("//");
    // `public/` first, served verbatim (`?import`: its URL, like Vite).
    if files
        && !rel.as_os_str().is_empty()
        && let Some(public) = &shared.public_dir
    {
        let p = public.join(&rel);
        if p.is_file() && same_case(&p) {
            if req.has_query("inline") {
                return match fs::read(&p) {
                    Ok(bytes) => {
                        let ext = ext_of(&p);
                        let data =
                            crate::assets::data_url(&ext, render::content_type(&ext), &bytes);
                        js_response(req, crate::assets::default_export(&data))
                    }
                    Err(_) => Response::text(404, "Not found"),
                };
            }
            if req.has_query("import") || req.has_query("url") {
                let url = http::encode_uri(&shared.public_url(path));
                return js_response(req, crate::assets::default_export(&url));
            }
            return serve_static(req, &p);
        }
    }
    let file = shared.root.join(&rel);
    if files && file.is_file() {
        if denied(&rel) {
            return Response::text(
                403,
                format!("Restricted: {path} is denied by the dev server"),
            );
        }
        return serve_file(shared, req, &file, path);
    }
    if file.is_dir() {
        let index = file.join("index.html");
        if index.is_file() {
            return serve_html(shared, req, &index);
        }
    }
    if let Some(resp) = plugin_middleware(shared, req, &[]) {
        return resp;
    }
    // Nothing at this path: a page for it, else the root page (client-side routes). Only
    // for requests that accept HTML (`*/*` too, like `fetch`), as Vite's HTML fallback
    // does (`vite: packages/vite/src/node/server/middlewares/htmlFallback.ts`).
    let wants_html = req
        .header("accept")
        .is_some_and(|a| a.contains("text/html") || a.contains("*/*"));
    if wants_html && path != "/favicon.ico" {
        if !path.ends_with(".html") && !path.ends_with('/') {
            let page = shared.root.join(format!("{}.html", rel.display()));
            if page.is_file() {
                return serve_html(shared, req, &page);
            }
        }
        let index = shared.root.join("index.html");
        if index.is_file() {
            return serve_html(shared, req, &index);
        }
    }
    Response::text(404, format!("Not found: {path}"))
}

/// Whether `file` exists with its name in this case (the file system may not care; URLs
/// do).
fn same_case(file: &Path) -> bool {
    match (fs::canonicalize(file), file.file_name()) {
        (Ok(real), Some(name)) => real.file_name().is_none_or(|r| {
            r == name
                || !r
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&name.to_string_lossy())
        }),
        _ => true,
    }
}

/// Project files the server never serves (Vite's default `server.fs.deny`): `.env` files,
/// certificates and keys, anything in `.git`.
fn denied(rel: &Path) -> bool {
    let name = rel
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".crt")
        || name.ends_with(".pem")
        || rel.components().any(|c| c.as_os_str() == ".git")
}

/// `/@ts-r/up/<n>/<path>` ([`crate::css::asset_url`]): a file in a `node_modules` directory
/// above the root (a workspace's hoisted packages), referenced from dependency CSS.
fn serve_up(shared: &Shared, req: &Request, rest: &str) -> Response {
    let Some((n, rel)) = rest.split_once('/') else {
        return Response::text(404, "Not found");
    };
    let (Ok(n), Some(rel)) = (n.parse::<usize>(), safe_relative(rel)) else {
        return Response::text(403, "invalid /@ts-r/up/ path");
    };
    let in_deps = rel.components().any(|c| c.as_os_str() == "node_modules");
    let Some(base) = shared.root.ancestors().nth(n).filter(|_| in_deps) else {
        return Response::text(403, "/@ts-r/up/ serves only files inside node_modules");
    };
    serve_static(req, &base.join(rel))
}

/// The URL path as a relative path under the root, or `None` if it escapes.
fn safe_relative(url_path: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in Path::new(url_path.trim_start_matches('/')).components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

fn ext_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Wraps a body with a content-hash ETag and answers 304 when it matches.
fn cacheable(req: &Request, content_type: &str, body: Vec<u8>, cache_control: &str) -> Response {
    let etag = format!("W/\"{}\"", &Key::of(&body).to_hex()[..20]);
    if http::etag_matches(req, &etag) {
        return Response::not_modified(&etag).header("Cache-Control", cache_control);
    }
    Response::new(200, content_type, body)
        .header("ETag", etag)
        .header("Cache-Control", cache_control)
}

fn serve_static(req: &Request, file: &Path) -> Response {
    let Ok(meta) = fs::metadata(file) else {
        return Response::text(404, "Not found");
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let etag = format!("W/\"{:x}-{:x}\"", meta.len(), mtime);
    if http::etag_matches(req, &etag) {
        return Response::not_modified(&etag).header("Cache-Control", "no-cache");
    }
    match fs::read(file) {
        Ok(body) => {
            let ext = ext_of(file);
            let mut r = Response::new(200, render::content_type(&ext), body)
                .header("ETag", etag)
                .header("Cache-Control", "no-cache");
            // A file without an extension has no known type: Vite's static server sends the
            // header empty, and so do we.
            if ext.is_empty() {
                for (k, v) in &mut r.headers {
                    if k == "Content-Type" {
                        v.clear();
                    }
                }
            }
            r
        }
        Err(_) => Response::text(404, "Not found"),
    }
}

/// A pre-bundled file with its `import.meta.glob` calls expanded (the bundler keeps them,
/// as Vite's does): patterns resolve as if the file sat at its URL under the root, so the
/// relative imports it gets point at the files' own URLs. Also says whether it had globs
/// (whose patterns are then watched).
fn dep_globs(shared: &Shared, name: &str, code: &str) -> (String, bool) {
    if !code.contains("import.meta.glob") {
        return (code.to_owned(), false);
    }
    let globs = scan::scan_hot(code).globs;
    if globs.is_empty() {
        return (code.to_owned(), false);
    }
    let importer = shared
        .root
        .join(crate::deps::DEPS_PREFIX.trim_matches('/'))
        .join(name);
    let no_alias = |_: &str| None;
    let cx = crate::glob::GlobContext {
        root: &shared.root,
        importer: &importer,
        alias: &no_alias,
    };
    match crate::glob::expand(code, &globs, &cx) {
        Ok(e) => {
            shared
                .dep_glob_watches
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(name.to_owned(), e.watches);
            (e.code, true)
        }
        Err(_) => (code.to_owned(), false),
    }
}

/// An HTML file after `%ENV%` replacement and plugin `transformIndexHtml` hooks.
fn transformed_html(shared: &Shared, url: &str, file: &Path) -> io::Result<Result<String, String>> {
    let env = shared.env();
    fs::read_to_string(file).map(|html| {
        let html = crate::env::replace_html(&html, &env);
        match &shared.plugins {
            Some(p) if shared.plugin_hooks.index_html => p.transform_index_html(url, file, &html),
            _ => Ok(html),
        }
    })
}

fn serve_html(shared: &Shared, req: &Request, file: &Path) -> Response {
    let html = transformed_html(shared, &req.path, file).map(|r| {
        r.map(|html| crate::html_proxy::with_proxies(&html, &url_for_file(&shared.root, file)))
    });
    match html {
        Ok(Err(message)) => Response::text(
            500,
            format!("transformIndexHtml failed for {}: {message}", req.path),
        ),
        Ok(Ok(html)) => {
            // Relative references of the root page resolve against the request's directory:
            // under a base, or served for another path, they need the base.
            let page_url = url_for_file(&shared.root, file);
            let relative = page_url == "/index.html" && (shared.base != "/" || req.path != "/");
            let mut host = app_css::AppCss::new(shared);
            let html = crate::html_urls::rewrite(&html, &shared.base, relative, &mut |css| {
                host.transform(file, css)
            });
            let deps = host.into_deps();
            {
                let mut map = shared.html_deps.lock().unwrap_or_else(|e| e.into_inner());
                if deps.is_empty() {
                    map.remove(file);
                } else {
                    map.insert(file.to_path_buf(), deps);
                }
            }
            // Pages are `text/html` with no charset, as Vite sends them (the page's own
            // `<meta charset>` decides).
            cacheable(
                req,
                "text/html",
                render::inject_client_at(&html, &shared.client_url(), &shared.html_inject)
                    .into_bytes(),
                "no-cache",
            )
        }
        Err(_) => Response::text(404, "Not found"),
    }
}

/// A [`DevConfig::virtual_modules`] module: imports resolved from the root and rewritten.
fn serve_virtual(shared: &Shared, req: &Request, url: &str, code: &str) -> Response {
    let importer = shared.root.join("index.html");
    let found = scan::scan(code);
    let mut failed = None;
    let out = render::rewrite_imports(code, &found.imports, "", |_, rec| {
        match resolve_target(shared, &rec.specifier, &importer) {
            Ok(p) => Some(shared.public_url(&pending_url(shared, p))),
            Err(m) => {
                failed.get_or_insert(m);
                None
            }
        }
    });
    if let Some(message) = failed {
        return error_response(
            shared,
            ModuleError {
                message,
                file: PathBuf::from(url),
                url: url.to_owned(),
                loc: None,
                frame: String::new(),
            },
        );
    }
    js_response(req, out)
}

/// The URL for a resolved import outside the graph (dependencies, virtual modules).
fn pending_url(shared: &Shared, p: Pending) -> String {
    match p {
        Pending::Url(u) => u,
        // A file joins the graph like any import (so `/@fs/` serves it, and one outside the
        // root is watched) and keeps its query (`?raw`, `?url`, `?import`).
        Pending::File { path, url, query } => {
            if !path.starts_with(&shared.root) {
                watch_outside(shared, &path);
            }
            let mut g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
            let id = g.ensure(&path, &url, kind_for(&path));
            module_url(g.node(id), &query)
        }
        Pending::Plugin { id } => {
            let url = plugins::plugin_url(&id);
            let mut g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
            g.ensure(Path::new(&id), &url, ModuleKind::Script);
            url
        }
        Pending::Dep(e) => crate::deps::url_for(shared, &e)
            .unwrap_or_else(|| dep_url(shared, &e.specifier, &e.file)),
    }
}

/// What a request for a project file wants.
fn serve_file(shared: &Shared, req: &Request, file: &Path, url_path: &str) -> Response {
    let ext = ext_of(file);
    // `?raw` and `?url` of an HTML file are its text and URL as JS, not the page.
    if (ext == "html" || ext == "htm") && !req.has_query("raw") && !req.has_query("url") {
        // An inline `<script type="module">` of the page, as its own module.
        if let Some(n) = req
            .query_value("index")
            .filter(|_| req.has_query("html-proxy"))
            .and_then(crate::html_proxy::index_of)
        {
            let url = format!("{url_path}?html-proxy&index={n}.js");
            return serve_script(shared, req, &crate::html_proxy::module_file(file, n), &url);
        }
        return serve_html(shared, req, file);
    }
    let url = url_path.to_owned();
    let as_script =
        req.has_query("import") || req.header("sec-fetch-dest").is_some_and(|d| d == "script");
    // `?raw`, and CSS `?inline`: the text.
    if req.has_query("raw") || (ext == "css" && req.has_query("inline")) {
        return match fs::read(file) {
            Ok(b) => {
                let text = String::from_utf8_lossy(&b);
                ensure_asset(shared, file, &url);
                js_response(req, crate::assets::default_export(&text))
            }
            Err(_) => Response::text(404, "Not found"),
        };
    }
    if req.has_query("inline") {
        return match fs::read(file) {
            Ok(b) => {
                ensure_asset(shared, file, &url);
                let data = crate::assets::data_url(&ext, render::content_type(&ext), &b);
                js_response(req, crate::assets::default_export(&data))
            }
            Err(_) => Response::text(404, "Not found"),
        };
    }
    let asset_import =
        as_script && !is_module_ext(shared, file, &ext) && ext != "css" && ext != "json";
    if req.has_query("url") || asset_import {
        ensure_asset(shared, file, &url);
        let out = asset_export_url(shared, file, &url, &req.query);
        return js_response(req, crate::assets::default_export(&out));
    }
    match ext.as_str() {
        "css" => {
            if as_script {
                serve_css_module(shared, req, file, &url)
            } else {
                serve_linked_css(shared, req, file, &url)
            }
        }
        "json" if as_script => match fs::read_to_string(file) {
            Ok(text) => {
                ensure_asset(shared, file, &url);
                match crate::assets::json_module(&text) {
                    Ok(module) => js_response(req, module),
                    Err(e) => error_response(
                        shared,
                        ModuleError {
                            message: format!("invalid JSON: {e}"),
                            file: file.to_path_buf(),
                            url,
                            loc: Some((e.line() as u32, e.column() as u32)),
                            frame: render::code_frame(&text, e.line() as u32, e.column() as u32),
                        },
                    ),
                }
            }
            Err(_) => Response::text(404, "Not found"),
        },
        _ if is_module_ext(shared, file, &ext) => serve_script(shared, req, file, &url),
        _ if crate::transform::is_script_extension(&ext) => error_response(
            shared,
            ModuleError {
                message: format!(
                    "No transform handles .{ext} files yet (the TS/JSX transform plugs into ts_r_hmr::Transform)."
                ),
                file: file.to_path_buf(),
                url,
                loc: None,
                frame: String::new(),
            },
        ),
        _ => serve_static(req, file),
    }
}

/// What an asset import (`import x from './a.png'`, `?url`, `new URL(...)`) evaluates to:
/// a small SVG's data URL, else the file's URL under the base.
/// The URL an asset import exports: [`asset_url`] (`?no-inline` never inlines) with its path
/// `encodeURI`d and the import's other query parameters kept (`?no-inline`, `&foo=bar`), as
/// Vite's asset plugin does (`vite: packages/vite/src/node/plugins/asset.ts`, `fileToDevUrl`).
fn asset_export_url(shared: &Shared, file: &Path, url: &str, query: &str) -> String {
    let out = if http::query_param(query, "no-inline").is_some() {
        shared.public_url(url)
    } else {
        asset_url(shared, file, url)
    };
    match out.starts_with("data:") {
        true => out,
        false => format!("{}{}", http::encode_uri(&out), asset_postfix(query)),
    }
}

/// The query an asset's URL keeps: the request's, without the server's own `import`, `url`
/// and `t` parameters; `?` and the rest, or empty.
fn asset_postfix(query: &str) -> String {
    let kept: Vec<&str> = query
        .split('&')
        .filter(|p| {
            let k = p.split('=').next().unwrap_or(p);
            !p.is_empty() && !matches!(k, "import" | "url" | "t")
        })
        .collect();
    match kept.is_empty() {
        true => String::new(),
        false => format!("?{}", kept.join("&")),
    }
}

fn asset_url(shared: &Shared, file: &Path, url: &str) -> String {
    let ext = ext_of(file);
    if ext == "svg"
        && let Ok(meta) = fs::metadata(file)
        && crate::assets::inline_under(&ext, meta.len(), shared.assets_inline_limit)
        && let Ok(bytes) = fs::read(file)
    {
        return crate::assets::data_url(&ext, render::content_type(&ext), &bytes);
    }
    shared.public_url(url)
}

fn is_module_ext(shared: &Shared, file: &Path, ext: &str) -> bool {
    crate::transform::is_script_extension(ext) && shared.transforms.iter().any(|t| t.handles(file))
}

fn js_response(req: &Request, code: String) -> Response {
    cacheable(req, JS_CONTENT_TYPE, code.into_bytes(), "no-cache")
}

fn error_response(shared: &Shared, err: ModuleError) -> Response {
    crate::hmr::report_error(shared, &err);
    Response::text(500, format!("{}\n\n{}", err.message, err.frame))
}

fn ensure_asset(shared: &Shared, file: &Path, url: &str) -> ModuleId {
    let mut g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
    g.ensure(file, url, kind_for(file))
}

pub(crate) fn kind_for(file: &Path) -> ModuleKind {
    match ext_of(file).as_str() {
        "css" => ModuleKind::Css,
        e if crate::transform::is_script_extension(e) => ModuleKind::Script,
        _ => ModuleKind::Asset,
    }
}

fn serve_linked_css(shared: &Shared, req: &Request, file: &Path, url: &str) -> Response {
    let Some(css) = app_stylesheet(shared, file, url, false) else {
        return Response::text(404, "Not found");
    };
    cacheable(req, "text/css; charset=utf-8", css.into_bytes(), "no-cache")
}

fn serve_css_module(shared: &Shared, req: &Request, file: &Path, url: &str) -> Response {
    let Some(css) = app_stylesheet(shared, file, url, true) else {
        return Response::text(404, "Not found");
    };
    js_response(req, render::css_module_at(&shared.client_url(), url, &css))
}

/// A stylesheet of the app with its `@import`s inlined and references rewritten
/// ([`app_css`]), recorded on its graph node: served as a module or linked, its source's
/// hash, and the files it depends on.
fn app_stylesheet(shared: &Shared, file: &Path, url: &str, as_js: bool) -> Option<String> {
    let source = fs::read_to_string(file).ok()?;
    let mut host = app_css::AppCss::new(shared);
    let css = host.transform(file, &source);
    let deps = host.into_deps();
    let mut g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
    let id = g.ensure(file, url, ModuleKind::Css);
    let n = g.node_mut(id);
    if as_js {
        n.served_as_js = true;
    } else {
        n.linked_css = true;
    }
    n.pruned = false;
    set_hash_only(n, Key::of(source.as_bytes()));
    g.set_watch_deps(id, deps);
    Some(css)
}

/// Records a non-script module's content hash (its "prepared" state).
pub(crate) fn set_hash_only(n: &mut crate::graph::ModuleNode, hash: Key) {
    if n.prepared.as_ref().is_some_and(|p| p.hash == hash) {
        return;
    }
    n.prepared = Some(Arc::new(Prepared {
        hash,
        output: Arc::new(TransformOutput::default()),
        hot: HotInfo::default(),
        targets: Vec::new(),
        accept_targets: Vec::new(),
        asset_urls: Vec::new(),
    }));
}

fn serve_script(shared: &Shared, req: &Request, file: &Path, url: &str) -> Response {
    let id = {
        let mut g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
        let id = g.ensure(file, url, ModuleKind::Script);
        let n = g.node_mut(id);
        n.served_as_js = true;
        n.pruned = false;
        id
    };
    let source = match module_source(shared, file, url) {
        Ok(b) => b,
        Err(message) if plugins::is_plugin_url(url) => {
            shared
                .graph
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .node_mut(id)
                .errored = true;
            return error_response(
                shared,
                ModuleError {
                    message,
                    file: file.to_path_buf(),
                    url: url.to_owned(),
                    loc: None,
                    frame: String::new(),
                },
            );
        }
        Err(_) => return Response::text(404, "Not found"),
    };
    let hash = Key::of(&source);
    let current = shared
        .graph
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .node(id)
        .prepared
        .clone()
        .filter(|p| p.hash == hash);
    let prepared = match current {
        Some(p) => p,
        None => match prepare_script(shared, id, file, &source, hash) {
            Ok((p, _pruned)) => p,
            Err(e) => {
                // Like the watcher path: a later file creation retries errored modules.
                shared
                    .graph
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .node_mut(id)
                    .errored = true;
                return error_response(shared, e);
            }
        },
    };
    crate::hmr::clear_error_for(shared, url);
    js_response(req, render_script(shared, id, &prepared))
}

/// Transforms (through the cache), scans hot usage, resolves imports and updates the graph.
/// Returns the prepared module and importees that lost their last importer.
pub(crate) fn prepare_script(
    shared: &Shared,
    id: ModuleId,
    file: &Path,
    source: &[u8],
    hash: Key,
) -> Result<(Arc<Prepared>, Vec<ModuleId>), ModuleError> {
    let url = shared
        .graph
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .node(id)
        .url
        .clone();
    let fail = |message: String, loc: Option<(u32, u32)>, src: &str| ModuleError {
        frame: loc.map_or_else(String::new, |(l, c)| render::code_frame(src, l, c)),
        message,
        file: file.to_path_buf(),
        url: url.clone(),
        loc,
    };
    let Ok(text) = std::str::from_utf8(source) else {
        return Err(fail("file is not valid UTF-8".to_owned(), None, ""));
    };
    let plugin_module = plugins::is_plugin_url(&url);
    let id_str = crate::paths::id(file);
    // A plugin module's dialect comes from the file behind its id (`x.tsx?query`).
    let real = if plugin_module {
        Path::new(plugins::id_file(&id_str))
    } else {
        file
    };
    let transform: &dyn Transform = match shared.transforms.iter().find(|t| t.handles(real)) {
        Some(t) => t.as_ref(),
        None if plugin_module => &PassthroughJs,
        None => {
            return Err(fail(
                format!("no transform handles {}", file.display()),
                None,
                text,
            ));
        }
    };
    let module_id = url.trim_start_matches('/');
    let input = TransformInput {
        path: real,
        id: module_id,
        source: text,
        mode: Mode::Dev,
    };
    // Files the output also depends on: their contents join the cache key.
    let mut watch_deps = transform.deps(&input);
    if plugin_module && real != file && real.is_absolute() {
        watch_deps.push(real.to_path_buf());
    }
    let content = if watch_deps.is_empty() {
        hash
    } else {
        let mut b = KeyBuilder::new("ts-r-dev/transform-deps");
        b.key_field(&hash);
        for d in &watch_deps {
            b.str(&crate::paths::id(d.strip_prefix(&shared.root).unwrap_or(d)));
            match fs::read(d) {
                Ok(bytes) => b.key_field(&Key::of(&bytes)),
                Err(_) => b.str("\0missing"),
            };
        }
        b.key()
    };
    let plugin_transforms = transform_plugins(shared);
    let cache_id = transform_cache_id(shared, transform);
    let key = TransformCache::key(&cache_id, Mode::Dev, module_id, &content);
    let output = shared
        .cache
        .get_or_insert(key, || {
            run_transform(shared, plugin_transforms, transform, &input, &id_str)
        })
        .map_err(|e: TransformError| {
            let loc = e.line.zip(e.column);
            fail(e.message, loc, text)
        })?;
    let mut output = with_relative_sources(output, module_id);
    let mut hot = scan::scan_hot(&output.code);

    // `import.meta.glob`: expanded here (after the cache: the files it matches change
    // without the module changing), then imports are scanned again.
    let mut glob_watches = Vec::new();
    if !hot.globs.is_empty() {
        let importer_id = crate::paths::id(real);
        let alias = |pattern: &str| {
            alias_rewrite(shared, pattern, &importer_id)
                .filter(|r| r != pattern)
                .or_else(|| crate::glob::subpath_import(pattern, real))
        };
        let cx = crate::glob::GlobContext {
            root: &shared.root,
            importer: real,
            alias: &alias,
        };
        let expanded = crate::glob::expand(&output.code, &hot.globs, &cx).map_err(|(at, m)| {
            let loc = render::line_col(&output.code, at);
            fail(m, Some(loc), &output.code)
        })?;
        let edits: Vec<crate::sourcemap::TextEdit<'_>> = expanded
            .edits
            .iter()
            .map(|(start, end, text)| crate::sourcemap::TextEdit {
                start: *start,
                end: *end,
                text,
            })
            .collect();
        let map = output
            .map
            .as_deref()
            .map(|m| crate::sourcemap::shift_for_edits(m, &output.code, &expanded.prelude, &edits));
        output = Arc::new(TransformOutput {
            imports: scan::scan(&expanded.code).imports,
            code: expanded.code,
            map,
        });
        hot = scan::scan_hot(&output.code);
        glob_watches = expanded.watches;
    }
    {
        let mut w = shared
            .glob_watches
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if glob_watches.is_empty() {
            w.remove(&id);
        } else {
            w.insert(id, glob_watches);
        }
    }
    let mut asset_files = Vec::new();
    let asset_urls: Vec<Option<String>> = hot
        .asset_urls
        .iter()
        .map(|rec| {
            let (url, file) = new_url_asset(shared, &rec.specifier, real)?;
            asset_files.extend(file);
            Some(url)
        })
        .collect();
    for f in &asset_files {
        if !f.starts_with(&shared.root) {
            watch_outside(shared, f);
        }
    }

    // Resolve outside the graph lock (it touches the filesystem).
    let resolve_failed = |(i, message): (usize, String), recs: &[scan::ImportRecord]| {
        let at = recs.get(i).map_or(0, |r| r.start as usize);
        let loc = render::line_col(&output.code, at);
        fail(message, Some(loc), &output.code)
    };
    let specifiers: Vec<String> = output.imports.iter().map(|r| r.specifier.clone()).collect();
    let pending =
        resolve_all(shared, &specifiers, file).map_err(|e| resolve_failed(e, &output.imports))?;
    let accepted_specifiers: Vec<String> =
        hot.accepted.iter().map(|r| r.specifier.clone()).collect();
    let accept_pending = resolve_all(shared, &accepted_specifiers, file)
        .map_err(|e| resolve_failed(e, &hot.accepted))?;

    for p in &pending {
        if let Pending::File { path, .. } = p
            && !path.starts_with(&shared.root)
        {
            watch_outside(shared, path);
        }
    }
    // A plugin module's backing file (`../shared/icon.svg?react`) or a transform input
    // outside the root is only reached through `watch_deps`.
    for d in &watch_deps {
        if d.is_absolute() && !d.starts_with(&shared.root) {
            watch_outside(shared, d);
        }
    }
    let mut g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
    let to_target = |p: Pending, g: &mut ModuleGraph| match p {
        Pending::Url(u) => Target::Url(u),
        Pending::Dep(e) => Target::Dep(e),
        Pending::File { path, url, query } => Target::Module {
            id: g.ensure(&path, &url, kind_for(&path)),
            query,
        },
        Pending::Plugin { id } => Target::Module {
            id: g.ensure(
                Path::new(&id),
                &plugins::plugin_url(&id),
                ModuleKind::Script,
            ),
            query: String::new(),
        },
    };
    let targets: Vec<Target> = pending.into_iter().map(|p| to_target(p, &mut g)).collect();
    let accept_targets: Vec<Target> = accept_pending
        .into_iter()
        .map(|p| to_target(p, &mut g))
        .collect();
    let mut importees: Vec<ModuleId> = targets
        .iter()
        .filter_map(|t| match t {
            Target::Module { id, .. } => Some(*id),
            Target::Url(_) | Target::Dep(_) => None,
        })
        .collect();
    // `new URL()` files: known to the graph (so `/@fs/` serves them); an asset is also an
    // importee and a watched file, so editing it re-prepares this module (an SVG's data
    // URL) and updates it like an imported asset.
    for f in asset_files {
        let kind = kind_for(&f);
        let asset = g.ensure(&f, &url_for_file(&shared.root, &f), kind);
        if kind == ModuleKind::Asset {
            importees.push(asset);
            watch_deps.push(f);
        }
    }
    let accepted: Vec<ModuleId> = accept_targets
        .iter()
        .filter_map(|t| match t {
            Target::Module { id, .. } => Some(*id),
            Target::Url(_) | Target::Dep(_) => None,
        })
        .collect();
    let (self_accepting, accepted_exports, bindings) =
        partial_accept(shared, &output, &hot, &targets);
    let pruned = g.set_imports(id, importees, accepted, self_accepting, hot.declined);
    g.set_partial_accept(id, accepted_exports, bindings);
    g.set_watch_deps(id, watch_deps);
    let prepared = Arc::new(Prepared {
        hash,
        output,
        hot,
        targets,
        accept_targets,
        asset_urls,
    });
    let node = g.node_mut(id);
    node.prepared = Some(Arc::clone(&prepared));
    node.errored = false;
    Ok((prepared, pruned))
}

/// What a module accepts and binds, for partial accept (`import.meta.hot.acceptExports`,
/// `vite: packages/vite/src/node/plugins/importAnalysis.ts`): accepting every export it
/// has (or having none) makes it self-accepting; otherwise it keeps the accepted names. The
/// bindings it imports from each module are recorded only with `hmrPartialAccept` on.
fn partial_accept(
    shared: &Shared,
    output: &TransformOutput,
    hot: &HotInfo,
    targets: &[Target],
) -> (bool, Option<Vec<String>>, Option<ImportedBindings>) {
    let full = (hot.accepted_exports.is_some() || shared.hmr_partial_accept)
        .then(|| scan::scan(&output.code));
    let mut self_accepting = hot.self_accepting;
    let mut accepted_exports = hot.accepted_exports.clone();
    if let (Some(names), Some(m)) = (&accepted_exports, &full)
        && !self_accepting
        && m.exports.iter().all(|e| names.binary_search(e).is_ok())
    {
        self_accepting = true;
    }
    if self_accepting {
        accepted_exports = None;
    }
    let bindings = full.filter(|_| shared.hmr_partial_accept).map(|mut m| {
        m.bindings.sort_by_key(|(at, _)| *at);
        let mut by_module: BTreeMap<ModuleId, Vec<String>> = BTreeMap::new();
        for (rec, target) in output.imports.iter().zip(targets) {
            let Target::Module { id, .. } = target else {
                continue;
            };
            let Ok(at) = m.bindings.binary_search_by_key(&rec.start, |(at, _)| *at) else {
                continue;
            };
            let names = &m.bindings[at].1;
            let set = by_module.entry(*id).or_default();
            set.extend(names.iter().cloned());
            set.sort();
            set.dedup();
        }
        by_module.into_iter().collect()
    });
    (self_accepting, accepted_exports, bindings)
}

/// A map whose single source is the module id (`src/a.tsx`, what the built-in transform
/// writes) names the file relative to the module's URL instead (`a.tsx`), as Vite does, so
/// the browser shows it at its real path.
fn with_relative_sources(output: Arc<TransformOutput>, module_id: &str) -> Arc<TransformOutput> {
    let Some(map) = &output.map else {
        return output;
    };
    let id_json = render::js_string(module_id);
    let needle = format!("\"sources\":[{id_json}]");
    let Some(at) = map.find(&needle) else {
        return output;
    };
    let base = module_id.rsplit('/').next().unwrap_or(module_id);
    let mut new_map = String::with_capacity(map.len());
    new_map.push_str(&map[..at]);
    new_map.push_str(&format!("\"sources\":[{}]", render::js_string(base)));
    new_map.push_str(&map[at + needle.len()..]);
    Arc::new(TransformOutput {
        code: output.code.clone(),
        map: Some(new_map),
        imports: output.imports.clone(),
    })
}

/// `resolve.alias` for a glob pattern or a stylesheet reference, through plugins' pre `resolveId` hooks (where the
/// sidecar applies the Vite config's aliases).
fn alias_rewrite(shared: &Shared, pattern: &str, importer: &str) -> Option<String> {
    let host = shared
        .plugins
        .as_deref()
        .filter(|_| shared.plugin_hooks.resolve_pre)?;
    match host
        .resolve_ids(Stage::Pre, importer, &[pattern.to_owned()])
        .ok()?
        .into_iter()
        .next()??
    {
        PluginResolution::Rewrite(s) => Some(s),
        PluginResolution::Id(_) | PluginResolution::External(_) => None,
    }
}

/// `s` escaped as the body of a string literal delimited by `quote` (`'`, `"` or `` ` ``):
/// an SVG data URL turns attribute quotes into `'`.
fn literal_body(s: &str, quote: u8) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            '$' if quote == b'`' && chars.peek() == Some(&'{') => out.push_str("\\$"),
            c if c.is_ascii() && c as u8 == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

/// The URL a `new URL('<spec>', import.meta.url)` literal becomes, and the project file
/// behind it: a project file's URL (a data URL for a small SVG) or a public file's (no
/// file), under the base. `None` when the file doesn't exist.
fn new_url_asset(
    shared: &Shared,
    spec: &str,
    importer: &Path,
) -> Option<(String, Option<PathBuf>)> {
    let clean = spec.split(['?', '#']).next().unwrap_or(spec);
    let suffix = &spec[clean.len()..];
    if clean.is_empty() || clean.contains("://") || clean.starts_with("data:") {
        return None;
    }
    let file = if let Some(rest) = clean.strip_prefix('/') {
        if let Some(public) = &shared.public_dir
            && let Some(rel) = safe_relative(rest)
            && public.join(&rel).is_file()
        {
            return Some((format!("{}{suffix}", shared.public_url(clean)), None));
        }
        shared.root.join(safe_relative(rest)?)
    } else {
        normalize(&importer.parent()?.join(clean))
    };
    // Not a file as written: an alias (`@/x.png`) or a path without its extension.
    let file = match file.is_file() {
        true => file,
        false => app_css::resolve_ref(shared, clean, importer, true)?,
    };
    let url = url_for_file(&shared.root, &file);
    let out = asset_url(shared, &file, &url);
    let out = if out.starts_with("data:") {
        out
    } else {
        format!("{out}{suffix}")
    };
    Some((out, Some(file)))
}

/// Makes sure a source file outside the root (a linked workspace package, `npm link`) is
/// watched: its package directory (the nearest one with a `package.json`) is added to the
/// watcher recursively, once. When that directory contains the root (a file in a parent of
/// the app), only the file's own directory is watched, non-recursively.
pub(crate) fn watch_outside(shared: &Shared, file: &Path) {
    let Some(adder) = shared.watch_adder.get() else {
        return;
    };
    let Some(parent) = file.parent() else {
        return;
    };
    let pkg = parent
        .ancestors()
        .find(|d| d.join("package.json").is_file())
        .filter(|d| !shared.root.starts_with(d));
    let (dir, recursive) = match pkg {
        Some(d) => (d, true),
        None => (parent, false),
    };
    {
        let mut seen = shared
            .watched_outside
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Already watched, or covered by a recursive watch of an ancestor.
        if seen.contains_key(dir) || seen.iter().any(|(w, rec)| *rec && dir.starts_with(w)) {
            return;
        }
        seen.insert(dir.to_path_buf(), recursive);
    }
    // `/` separators on every OS (Vite's display form): on Windows `dir` may come from a
    // native path or from a plugin id (`C:/x/y`), and both must print the same.
    shared.log(&format!(
        "watching {} (outside the root)",
        crate::paths::id(dir)
    ));
    adder.add(dir, recursive);
}

enum Pending {
    Url(String),
    Dep(DepEntry),
    File {
        path: PathBuf,
        url: String,
        query: String,
    },
    /// A plugin module (see [`crate::plugin`]).
    Plugin {
        id: String,
    },
}

/// Resolves one specifier from `importer` to a URL target (graph modules get ids later).
fn resolve_target(shared: &Shared, specifier: &str, importer: &Path) -> Result<Pending, String> {
    if let Some(u) = public_import(shared, specifier) {
        return Ok(Pending::Url(u));
    }
    // The server's own modules (`import '/@vite/env'`), with any query, are served URLs.
    let path = specifier.split(['?', '#']).next().unwrap_or(specifier);
    if matches!(
        path,
        "/@ts-r/client" | "/@vite/client" | "/@ts-r/env" | "/@vite/env"
    ) {
        return Ok(Pending::Url(shared.public_url(specifier)));
    }
    let fs_form = absolute_as_fs(&shared.root, specifier);
    let resolved = shared
        .resolver
        .resolve(fs_form.as_deref().unwrap_or(specifier), importer)
        .map_err(|e| e.message)?;
    let spec_query = specifier
        .split_once('?')
        .map(|(_, q)| q.split('#').next().unwrap_or(q))
        .unwrap_or("");
    Ok(match resolved {
        Resolved::External(u) => Pending::Url(u),
        Resolved::Dep {
            specifier,
            file,
            package,
        } => {
            if shared.deps.enabled()
                && spec_query.is_empty()
                && kind_for(&file) == ModuleKind::Script
            {
                Pending::Dep(DepEntry {
                    specifier,
                    package,
                    file,
                })
            } else {
                Pending::Url(dep_url(shared, &specifier, &file))
            }
        }
        Resolved::File(path) => file_pending(shared, path, spec_query),
    })
}

/// `import x from '/img/a.png'` naming a file in `public/` (and not in the root): the
/// file's URL with `?import` (non-script files), served as its URL.
fn public_import(shared: &Shared, specifier: &str) -> Option<String> {
    let public = shared.public_dir.as_ref()?;
    let rest = specifier.strip_prefix('/')?;
    if rest.starts_with('/') || rest.starts_with('@') {
        return None;
    }
    let clean = rest.split(['?', '#']).next().unwrap_or(rest);
    let rel = safe_relative(clean)?;
    if rel.as_os_str().is_empty()
        || !public.join(&rel).is_file()
        || shared.root.join(&rel).is_file()
    {
        return None;
    }
    let path = &specifier[..clean.len() + 1];
    let query = &specifier[clean.len() + 1..];
    if kind_for(&rel) == ModuleKind::Script {
        return Some(specifier.to_owned());
    }
    Some(match query.strip_prefix('?') {
        Some(q) if !q.is_empty() => format!("{path}?import&{q}"),
        _ => format!("{path}?import"),
    })
}

/// `/abs/path` specifiers that name a file system path rather than a root-relative one (a
/// path inside the root, or one whose root-relative directory doesn't exist but whose
/// absolute directory does), as `/@fs/abs/path`. Plugins and `resolve.alias` produce these.
fn absolute_as_fs(root: &Path, specifier: &str) -> Option<String> {
    // A Windows drive path (`C:/x`, `C:\\x`: what aliases give there) names the file system.
    if cfg!(windows) && crate::paths::is_drive_absolute(specifier) {
        return Some(format!("/@fs/{}", specifier.replace('\\', "/")));
    }
    if !specifier.starts_with('/') || specifier.starts_with("//") || specifier.starts_with("/@") {
        return None;
    }
    let p = Path::new(specifier.split(['?', '#']).next().unwrap_or(specifier));
    let root_relative = root.join(p.strip_prefix("/").unwrap_or(p));
    let fs_path = p.starts_with(root)
        || (!root_relative.parent().is_some_and(Path::exists)
            && p.parent().is_some_and(Path::exists));
    fs_path.then(|| format!("/@fs{specifier}"))
}

/// A resolved project file, imported with `spec_query`.
fn file_pending(shared: &Shared, path: PathBuf, spec_query: &str) -> Pending {
    if let Some(u) = node_modules_url(shared, &path) {
        return Pending::Url(u);
    }
    let kind = kind_for(&path);
    if shared.plugin_hooks.modules()
        && is_plugin_import(
            &path,
            kind,
            spec_query,
            shared.assets_include.matches(&path),
        )
    {
        let id = if spec_query.is_empty() {
            crate::paths::id(&path).into_owned()
        } else {
            format!("{}?{spec_query}", crate::paths::id(&path))
        };
        return Pending::Plugin { id };
    }
    let url = url_for_file(&shared.root, &path);
    let mut query = spec_query.to_owned();
    // Like Vite: assets always get `import`; CSS only when imported for its styles.
    let explicit = kind == ModuleKind::Asset
        || (kind == ModuleKind::Css
            && ["raw", "url", "inline"]
                .iter()
                .all(|k| http::query_param(&query, k).is_none()));
    if explicit {
        query = if query.is_empty() {
            "import".to_owned()
        } else {
            format!("import&{query}")
        };
    }
    Pending::File { path, url, query }
}

/// Query keys the server itself understands.
const BUILTIN_QUERY_KEYS: &[&str] = &["import", "raw", "url", "inline", "t", "v"];

/// Asset types served as URLs unless a query asks a plugin for something else (Vite's
/// known asset types).
const KNOWN_ASSETS: &[&str] = &[
    "apng",
    "avif",
    "bmp",
    "cur",
    "gif",
    "ico",
    "jfif",
    "jpeg",
    "jpg",
    "jxl",
    "pjp",
    "pjpeg",
    "png",
    "svg",
    "webp",
    "mp4",
    "webm",
    "ogg",
    "mp3",
    "wav",
    "flac",
    "aac",
    "opus",
    "mov",
    "m4a",
    "vtt",
    "woff",
    "woff2",
    "eot",
    "ttf",
    "otf",
    "webmanifest",
    "pdf",
    "txt",
    "wasm",
];

/// Whether an import of `path` goes through plugin `load`/`transform` hooks as a plugin
/// module: a query the server doesn't know (`?react`), or a file that is neither a script,
/// CSS, JSON nor a known asset (`.md`, `.yaml`, `.vue`) nor `included` by `assetsInclude`.
fn is_plugin_import(path: &Path, kind: ModuleKind, spec_query: &str, included: bool) -> bool {
    let unknown_query = spec_query
        .split('&')
        .filter(|p| !p.is_empty())
        .any(|p| !BUILTIN_QUERY_KEYS.contains(&p.split('=').next().unwrap_or(p)));
    if unknown_query {
        return true;
    }
    let ext = ext_of(path);
    kind == ModuleKind::Asset
        && spec_query.is_empty()
        && !included
        && ext != "json"
        && !KNOWN_ASSETS.contains(&ext.as_str())
}

/// Where a plugin-resolved id goes: plain files take the normal path, anything else
/// (virtual ids, files with a query) becomes a plugin module.
fn plugin_pending(shared: &Shared, id: &str) -> Pending {
    if !id.starts_with('\0') {
        let file = plugins::id_file(id);
        let path = Path::new(file);
        if path.is_absolute() && path.is_file() {
            let query = id[file.len()..].trim_start_matches('?');
            let query = query.split('#').next().unwrap_or(query);
            return file_pending(shared, normalize(path), query);
        }
    }
    Pending::Plugin { id: id.to_owned() }
}

/// Resolves the specifiers one module imports: plugins' pre `resolveId` hooks, the
/// built-in resolver, then post hooks for what it couldn't resolve. On failure, the index
/// of the first unresolved specifier and the message.
fn resolve_all(
    shared: &Shared,
    specifiers: &[String],
    importer: &Path,
) -> Result<Vec<Pending>, (usize, String)> {
    let hooks = shared.plugin_hooks;
    let host = shared
        .plugins
        .as_deref()
        .filter(|_| (hooks.resolve_pre || hooks.resolve_post) && !specifiers.is_empty());
    let Some(host) = host else {
        return specifiers
            .iter()
            .enumerate()
            .map(|(i, s)| resolve_target(shared, s, importer).map_err(|e| (i, e)))
            .collect();
    };
    let importer_id = crate::paths::id(importer);
    let n = specifiers.len();
    let mut effective: Vec<String> = specifiers.to_vec();
    let mut out: Vec<Option<Pending>> = (0..n).map(|_| None).collect();
    if hooks.resolve_pre {
        let answers = host
            .resolve_ids(Stage::Pre, &importer_id, specifiers)
            .map_err(|e| (0, e))?;
        for (i, a) in answers.into_iter().enumerate().take(n) {
            match a {
                Some(PluginResolution::Id(id)) => out[i] = Some(plugin_pending(shared, &id)),
                Some(PluginResolution::External(u)) => out[i] = Some(Pending::Url(u)),
                Some(PluginResolution::Rewrite(s)) => effective[i] = s,
                None => {}
            }
        }
    }
    let mut failed = Vec::new();
    for i in 0..n {
        if out[i].is_some() {
            continue;
        }
        match resolve_target(shared, &effective[i], importer) {
            Ok(p) => out[i] = Some(p),
            Err(e) => failed.push((i, e)),
        }
    }
    if hooks.resolve_post && !failed.is_empty() {
        let specs: Vec<String> = failed.iter().map(|(i, _)| effective[*i].clone()).collect();
        let answers = host
            .resolve_ids(Stage::Post, &importer_id, &specs)
            .map_err(|e| (failed[0].0, e))?;
        let mut still = Vec::new();
        let answers = answers.into_iter().chain(std::iter::repeat(None));
        for ((i, e), a) in failed.into_iter().zip(answers) {
            match a {
                Some(PluginResolution::Id(id)) => out[i] = Some(plugin_pending(shared, &id)),
                Some(PluginResolution::External(u)) => out[i] = Some(Pending::Url(u)),
                Some(PluginResolution::Rewrite(s)) => match resolve_target(shared, &s, importer) {
                    Ok(p) => out[i] = Some(p),
                    Err(e) => still.push((i, e)),
                },
                None => still.push((i, e)),
            }
        }
        failed = still;
    }
    if let Some(first) = failed.into_iter().next() {
        return Err(first);
    }
    Ok(out.into_iter().flatten().collect())
}

/// The plugin host whose `transform` hooks wrap the built-in transform, if any.
pub(crate) fn transform_plugins(shared: &Shared) -> Option<&dyn PluginHost> {
    shared
        .plugins
        .as_deref()
        .filter(|_| shared.plugin_hooks.transforms())
}

/// The transform cache id: the transform's, plus the plugin host's when its `transform`
/// hooks run around it.
pub(crate) fn transform_cache_id(shared: &Shared, transform: &dyn Transform) -> String {
    match transform_plugins(shared) {
        Some(p) => format!("{}+{}", transform.cache_id(), p.cache_id()),
        None => transform.cache_id().to_owned(),
    }
}

/// What the dependency scan follows.
pub(crate) enum ScanHit {
    Dep(DepEntry),
    File(PathBuf),
}

/// Resolves a module's imports for the dependency scan, through plugins' `resolveId` hooks
/// and `resolve.alias` when there are any (so aliased imports are scanned too).
pub(crate) fn resolve_for_scan(
    shared: &Shared,
    specifiers: &[String],
    importer: &Path,
) -> Vec<Option<ScanHit>> {
    let hooks = shared.plugin_hooks;
    if (hooks.resolve_pre || hooks.resolve_post)
        && let Ok(all) = resolve_all(shared, specifiers, importer)
    {
        return all
            .into_iter()
            .map(|p| match p {
                Pending::Dep(e) => Some(ScanHit::Dep(e)),
                Pending::File { path, .. } => Some(ScanHit::File(path)),
                Pending::Url(_) | Pending::Plugin { .. } => None,
            })
            .collect();
    }
    specifiers
        .iter()
        .map(|s| match shared.resolver.resolve(s, importer).ok()? {
            Resolved::Dep {
                package,
                specifier,
                file,
            } => (kind_for(&file) == ModuleKind::Script).then_some(ScanHit::Dep(DepEntry {
                specifier,
                package,
                file,
            })),
            Resolved::File(p) => Some(ScanHit::File(p)),
            Resolved::External(_) => None,
        })
        .collect()
}

/// The built-in transform between plugins' pre and post `transform` hooks. The plugins'
/// source maps and the transform's are composed into one map back to the original file.
pub(crate) fn run_transform(
    shared: &Shared,
    host: Option<&dyn PluginHost>,
    transform: &dyn Transform,
    input: &TransformInput<'_>,
    plugin_id: &str,
) -> Result<TransformOutput, TransformError> {
    let Some(host) = host else {
        return transform.transform(input);
    };
    let hooks = shared.plugin_hooks;
    let pre = if hooks.transform_pre {
        host.transform(Stage::Pre, plugin_id, input.source)
            .map_err(TransformError::new)?
    } else {
        None
    };
    // Maps in the order the code went through them; `None` = moved nothing.
    let mut chain: Vec<Option<String>> = Vec::new();
    let mut out = match pre {
        Some(t) => {
            chain.extend(t.maps);
            transform.transform(&TransformInput {
                source: &t.code,
                ..*input
            })?
        }
        None => transform.transform(input)?,
    };
    if hooks.transform_post
        && let Some(t) = host
            .transform(Stage::Post, plugin_id, &out.code)
            .map_err(TransformError::new)?
    {
        chain.push(out.map.take());
        chain.extend(t.maps);
        out = TransformOutput {
            imports: scan::scan(&t.code).imports,
            code: t.code,
            map: None,
        };
    } else {
        chain.push(out.map.take());
    }
    out.map = compose_maps(&chain, input);
    Ok(out)
}

/// One map for a chain of transforms (see [`run_transform`]); sources that name the
/// module's own file become its file name, with the original text as their content.
fn compose_maps(chain: &[Option<String>], input: &TransformInput<'_>) -> Option<String> {
    let present: Vec<&String> = chain.iter().flatten().collect();
    if present.is_empty() {
        return None;
    }
    if present.len() == 1 && chain.len() == 1 {
        return Some(present[0].clone());
    }
    let mut maps = Vec::with_capacity(present.len());
    for m in present {
        maps.push(crate::sourcemap::SourceMap::parse(m)?);
    }
    let mut map = crate::sourcemap::compose(&maps)?;
    let file_name = input
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let real = crate::paths::id(input.path);
    for (i, s) in map.sources.iter_mut().enumerate() {
        let own = *s == real
            || *s == input.id
            || s.trim_start_matches('/') == input.id
            || s.split('?').next() == Some(&*real);
        if own {
            *s = file_name.clone();
            if let Some(c) = map.sources_content.get_mut(i)
                && c.is_none()
            {
                *c = Some(input.source.to_owned());
            }
        }
    }
    Some(map.to_json())
}

/// A module's source: the file, or for a plugin module what a plugin's `load` returned,
/// else (Vite's load fallback) a known asset's URL as the default export, else the text of
/// the file behind the id.
pub(crate) fn module_source(shared: &Shared, file: &Path, url: &str) -> Result<Vec<u8>, String> {
    if let Some((page, n)) = crate::html_proxy::split_module_file(file) {
        let page_url = url_for_file(&shared.root, &page);
        let html = transformed_html(shared, &page_url, &page)
            .map_err(|e| format!("{}: {e}", page.display()))??;
        return crate::html_proxy::script(&html, n)
            .map(|s| s.as_bytes().to_vec())
            .ok_or_else(|| format!("{page_url} has no inline module script {n}"));
    }
    if !plugins::is_plugin_url(url) {
        return fs::read(file).map_err(|e| format!("{}: {e}", file.display()));
    }
    let id = crate::paths::id(file);
    if let Some(host) = shared
        .plugins
        .as_deref()
        .filter(|_| shared.plugin_hooks.load)
        && let Some(code) = host.load(&id)?
    {
        return Ok(code.into_bytes());
    }
    let real = Path::new(plugins::id_file(&id));
    if !id.starts_with('\0') && real.is_absolute() && real.is_file() {
        if KNOWN_ASSETS.contains(&ext_of(real).as_str()) {
            let url = url_for_file(&shared.root, real);
            let query = id.split_once('?').map_or("", |(_, q)| q);
            let out = asset_export_url(shared, real, &url, query);
            return Ok(crate::assets::default_export(&out).into_bytes());
        }
        return fs::read(real).map_err(|e| format!("{}: {e}", real.display()));
    }
    Err(format!(
        "Could not load {}: no plugin's load hook returned it",
        id.replace('\0', "\\0")
    ))
}

/// Plugin `configureServer` middlewares for a request no built-in route served.
fn plugin_middleware(shared: &Shared, req: &Request, body: &[u8]) -> Option<Response> {
    let host = shared
        .plugins
        .as_deref()
        .filter(|_| shared.plugin_hooks.middleware)?;
    match host.middleware(req, body) {
        Ok(r) => r,
        Err(message) => Some(Response::text(
            500,
            format!("plugin middleware failed: {message}"),
        )),
    }
}

/// What a plugin host may use of the server: broadcasting and the built-in resolver. Holds
/// the server weakly (the host lives inside it).
fn server_handle(shared: &Arc<Shared>) -> ServerHandle {
    let weak = Arc::downgrade(shared);
    let weak2 = Arc::downgrade(shared);
    ServerHandle {
        broadcast: Arc::new(move |text: &str| {
            if let Some(s) = weak.upgrade() {
                s.hub.broadcast(text);
            }
        }),
        resolve: Arc::new(move |specifier: &str, importer: &str| {
            let s = weak2.upgrade()?;
            let importer = if importer.is_empty() || importer.starts_with('\0') {
                s.root.join("index.html")
            } else {
                PathBuf::from(plugins::id_file(importer))
            };
            let query = specifier.split_once('?').map(|(_, q)| q);
            let with_query = |p: &Path| match query {
                Some(q) => format!("{}?{q}", crate::paths::id(p)),
                None => crate::paths::id(p).into_owned(),
            };
            let fs_form = absolute_as_fs(&s.root, specifier);
            let spec = fs_form.as_deref().unwrap_or(specifier);
            match s.resolver.resolve(spec, &importer).ok()? {
                Resolved::File(p) => Some((with_query(&p), false)),
                Resolved::Dep { file, .. } => Some((with_query(&file), false)),
                Resolved::External(u) => Some((u, true)),
            }
        }),
    }
}

/// `/@modules/<specifier>?v=<hash>`; JSON/CSS deps carry `?import` like project files.
fn dep_url(shared: &Shared, specifier: &str, file: &Path) -> String {
    let explicit = matches!(kind_for(file), ModuleKind::Css | ModuleKind::Asset);
    format!(
        "/@modules/{}?{}v={}",
        http::percent_encode_path(specifier),
        if explicit { "import&" } else { "" },
        shared.dep_version
    )
}

/// A file inside `node_modules` as `/@modules/<package>/<path>`.
fn node_modules_url(shared: &Shared, path: &Path) -> Option<String> {
    let comps: Vec<&str> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    let nm = comps.iter().rposition(|&c| c == "node_modules")?;
    let rest = &comps[nm + 1..];
    let pkg_len = if rest.first()?.starts_with('@') { 2 } else { 1 };
    if rest.len() <= pkg_len {
        return None;
    }
    Some(dep_url(shared, &rest.join("/"), path))
}

/// Root-relative URL for a project file, or `/@fs/<abs>` outside the root.
pub fn url_for_file(root: &Path, file: &Path) -> String {
    match file.strip_prefix(root) {
        Ok(rel) => {
            let rel: Vec<_> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect();
            http::percent_encode_path(&format!("/{}", rel.join("/")))
        }
        Err(_) => {
            let abs = file.to_string_lossy().replace('\\', "/");
            http::percent_encode_path(&format!("/@fs/{}", abs.trim_start_matches('/')))
        }
    }
}

/// A graph module's URL with `query` and its last update's timestamp.
fn module_url(n: &ModuleNode, query: &str) -> String {
    let mut q = query.to_owned();
    if n.last_hmr_timestamp > 0 {
        if !q.is_empty() {
            q.push('&');
        }
        q.push_str(&format!("t={}", n.last_hmr_timestamp));
    }
    if q.is_empty() {
        n.url.clone()
    } else {
        format!("{}?{q}", n.url)
    }
}

/// `url` plus the representation part of `query` (`raw`, `url`; not `import`): the identity
/// `hot.accept` registers and `js-update` names, so `./x.txt?raw` stays raw after an update.
pub(crate) fn accepted_url(url: &str, query: &str) -> String {
    let rest: Vec<&str> = query
        .split('&')
        .filter(|p| !p.is_empty() && *p != "import")
        .collect();
    if rest.is_empty() {
        url.to_owned()
    } else {
        format!("{url}?{}", rest.join("&"))
    }
}

/// Renders a prepared module for the browser with the graph's current timestamps.
pub(crate) fn render_script(shared: &Shared, id: ModuleId, p: &Prepared) -> String {
    // Dependency URLs first: they may wait for (or trigger) a dependency batch, so not
    // under the graph lock.
    let dep_urls: HashMap<&str, String> = p
        .targets
        .iter()
        .chain(&p.accept_targets)
        .filter_map(|t| match t {
            Target::Dep(e) => Some((
                e.specifier.as_str(),
                crate::deps::url_for(shared, e)
                    .unwrap_or_else(|| dep_url(shared, &e.specifier, &e.file)),
            )),
            _ => None,
        })
        .collect();
    let g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
    let url_of = |t: &Target, with_query: bool| -> String {
        match t {
            Target::Url(u) => shared.public_url(u),
            Target::Dep(e) => shared.public_url(&dep_urls[e.specifier.as_str()]),
            Target::Module { id, query } => {
                let n = g.node(*id);
                if !with_query {
                    // `accept('./dep')`: matched against update paths, which are base-less.
                    return n.url.clone();
                }
                shared.public_url(&module_url(n, query))
            }
        }
    };
    let mut edits: Vec<render::Edit> = Vec::with_capacity(p.targets.len() + p.accept_targets.len());
    for (rec, t) in p.output.imports.iter().zip(&p.targets) {
        edits.push(render::Edit {
            start: rec.start,
            end: rec.end,
            text: url_of(t, true),
        });
    }
    for (rec, t) in p.hot.accepted.iter().zip(&p.accept_targets) {
        let text = match t {
            Target::Module { query, .. } => accepted_url(&url_of(t, false), query),
            Target::Url(_) | Target::Dep(_) => url_of(t, false),
        };
        edits.push(render::Edit {
            start: rec.start,
            end: rec.end,
            text,
        });
    }
    let mut prelude = String::new();
    if p.hot.uses_hot {
        prelude.push_str(&render::hot_prelude_at(
            &shared.client_url(),
            &g.node(id).url,
        ));
    }
    drop(g);
    let env = shared.env();
    if p.hot.uses_env {
        prelude.push_str(&env.prelude);
    }
    for &(start, end) in &p.hot.node_env {
        edits.push(render::Edit {
            start,
            end,
            text: env.node_env.clone(),
        });
    }
    for (rec, u) in p.hot.asset_urls.iter().zip(&p.asset_urls) {
        if let Some(u) = u {
            // The record is the inside of the literal; its delimiter stays.
            let quote = (rec.start as usize)
                .checked_sub(1)
                .and_then(|i| p.output.code.as_bytes().get(i))
                .copied()
                .unwrap_or(b'"');
            edits.push(render::Edit {
                start: rec.start,
                end: rec.end,
                text: literal_body(u, quote),
            });
        }
    }
    let mut code = render::apply_edits(&p.output.code, &prelude, &mut edits);
    if let Some(map) = &p.output.map {
        // `apply_edits` sorted the edits; it skips overlapping ones, and so must the map.
        let mut text_edits: Vec<crate::sourcemap::TextEdit<'_>> = Vec::with_capacity(edits.len());
        let mut at = 0usize;
        for e in &edits {
            let (s, t) = (e.start as usize, e.end as usize);
            if s < at || t > p.output.code.len() || s > t {
                continue;
            }
            text_edits.push(crate::sourcemap::TextEdit {
                start: s,
                end: t,
                text: &e.text,
            });
            at = t;
        }
        let map = crate::sourcemap::shift_for_edits(map, &p.output.code, &prelude, &text_edits);
        code.push_str("\n//# sourceMappingURL=data:application/json;base64,");
        code.push_str(&ws::base64(map.as_bytes()));
        code.push('\n');
    }
    code
}

// ---------------------------------------------------------------------------------------
// Dependencies

fn serve_dep(shared: &Shared, req: &Request, spec: &str) -> Response {
    let immutable = req.query_value("v") == Some(shared.dep_version.as_str());
    let cache_control = if immutable {
        "max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let as_script =
        req.has_query("import") || req.header("sec-fetch-dest").is_some_and(|d| d == "script");
    let cache_key = format!("{spec}?{}", if as_script { "import" } else { "" });
    if let Some(hit) = shared
        .dep_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&cache_key)
        .cloned()
    {
        return cacheable(req, &hit.0, hit.1.clone().into_bytes(), cache_control);
    }
    let importer = shared.root.join("index.html");
    let (package, specifier, file) = match shared.resolver.resolve(spec, &importer) {
        Ok(Resolved::Dep {
            package,
            specifier,
            file,
        }) => (package, specifier, file),
        Ok(other) => return Response::text(404, format!("{spec} is not a dependency ({other:?})")),
        Err(e) => return Response::text(404, e.message),
    };
    let ext = ext_of(&file);
    // Assets (fonts, images) a package ships: the file itself, or its URL for `?import`.
    if kind_for(&file) == ModuleKind::Asset && ext != "json" {
        if as_script {
            let u = crate::css::asset_url(&shared.root, &file);
            return js_response(req, format!("export default {};\n", render::js_string(&u)));
        }
        return serve_static(req, &file);
    }
    let Ok(source) = fs::read_to_string(&file) else {
        return Response::text(404, format!("cannot read {}", file.display()));
    };
    let url = format!("/@modules/{spec}");
    // Dependency CSS: `@import`s inlined, `url()`s pointing at the package's files.
    let css = || {
        crate::css::bundle_css(&file, &|p| fs::read_to_string(p).ok(), &|p| {
            crate::css::asset_url(&shared.root, p)
        })
        .unwrap_or_else(|| source.clone())
    };
    let (ctype, body) =
        match ext.as_str() {
            "css" if !as_script => ("text/css; charset=utf-8", css()),
            "css" => (
                JS_CONTENT_TYPE,
                render::css_module_at(&shared.client_url(), &url, &css()),
            ),
            "json" => (
                JS_CONTENT_TYPE,
                format!("export default {};\n", source.trim()),
            ),
            _ => {
                let out = match shared.dep_bundler.bundle(
                    &DepRequest {
                        specifier: &specifier,
                        package: &package,
                        file: &file,
                    },
                    &source,
                ) {
                    Ok(o) => o,
                    Err(e) => {
                        return error_response(
                            shared,
                            ModuleError {
                                message: e.message,
                                file: file.clone(),
                                url,
                                loc: None,
                                frame: String::new(),
                            },
                        );
                    }
                };
                let mut failed = None;
                let code = render::rewrite_imports(&out.code, &out.imports, "", |_, rec| {
                    match resolve_target(shared, &rec.specifier, &file) {
                        Ok(p) => Some(shared.public_url(&pending_url(shared, p))),
                        Err(m) => {
                            failed.get_or_insert(m);
                            None
                        }
                    }
                });
                if let Some(m) = failed {
                    return error_response(
                        shared,
                        ModuleError {
                            message: m,
                            file: file.clone(),
                            url,
                            loc: None,
                            frame: String::new(),
                        },
                    );
                }
                (JS_CONTENT_TYPE, code)
            }
        };
    shared
        .dep_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(cache_key, Arc::new((ctype.to_owned(), body.clone())));
    cacheable(req, ctype, body.into_bytes(), cache_control)
}

/// Milliseconds since the epoch, strictly increasing across calls.
pub(crate) fn next_timestamp(shared: &Shared) -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let mut last = shared
        .last_timestamp
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *last = now.max(*last + 1);
    *last
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assets_include_files_are_assets_not_plugin_modules() {
        let f = Path::new("/p/app/nested/foo.unknown");
        let kind = kind_for(f);
        assert!(is_plugin_import(f, kind, "", false));
        assert!(!is_plugin_import(f, kind, "", true));
        // A query the server doesn't know still goes to plugins.
        assert!(is_plugin_import(f, kind, "react", true));
        assert!(!is_plugin_import(Path::new("/p/a.png"), kind, "", false));
    }

    #[test]
    fn urls_for_files() {
        let root = Path::new("/p/app");
        assert_eq!(
            url_for_file(root, Path::new("/p/app/src/a b.js")),
            "/src/a%20b.js"
        );
        assert_eq!(
            url_for_file(root, Path::new("/p/lib/x.js")),
            "/@fs/p/lib/x.js"
        );
    }

    /// A plugin that prepends a line (with its map), then the built-in transform: the
    /// composed map points at the original file's lines, named by its file name.
    #[test]
    fn plugin_and_transform_maps_compose() {
        let original = "let a = 1;\nlet b = 2;\n";
        // Plugin output: "// banner\n" + original; line n+1 → original line n.
        let plugin =
            r#"{"version":3,"sources":["/p/app/src/m.ts"],"names":[],"mappings":";AAAA;AACA"}"#;
        // Built-in transform: identity on its input (sources = module id).
        let ours = r#"{"version":3,"sources":["src/m.ts"],"sourcesContent":["// banner\nlet a = 1;\nlet b = 2;\n"],"names":[],"mappings":"AAAA;AACA;AACA"}"#;
        let input = TransformInput {
            path: Path::new("/p/app/src/m.ts"),
            id: "src/m.ts",
            source: original,
            mode: Mode::Dev,
        };
        let map =
            compose_maps(&[Some(plugin.to_owned()), Some(ours.to_owned())], &input).expect("a map");
        let m = crate::sourcemap::SourceMap::parse(&map).unwrap();
        assert_eq!(m.sources, ["m.ts"]);
        assert_eq!(m.sources_content, [Some(original.to_owned())]);
        // Final line 2 (`let b`) → original line 1.
        assert_eq!(m.lines[2][0].line, 1);
        assert!(m.lines[0].is_empty());
        // No plugin maps: the transform's map passes through untouched.
        assert_eq!(
            compose_maps(&[Some(ours.to_owned())], &input).as_deref(),
            Some(ours)
        );
        assert_eq!(compose_maps(&[None, None], &input), None);
    }

    #[test]
    fn bases() {
        assert_eq!(normalize_base("/app"), "/app/");
        assert_eq!(normalize_base("./"), "/");
        assert_eq!(normalize_base(""), "/");
        assert_eq!(normalize_base("https://cdn.x/a/b/"), "/a/b/");
        assert_eq!(with_base("/app/", "/src/a.js"), "/app/src/a.js");
        assert_eq!(with_base("/app/", "//cdn/x.js"), "//cdn/x.js");
        assert_eq!(with_base("/", "/src/a.js"), "/src/a.js");
        let mut req = Request {
            method: "GET".into(),
            target: "/app/src/a.js?t=1".into(),
            path: "/app/src/a.js".into(),
            query: "t=1".into(),
            headers: Vec::new(),
            http10: false,
        };
        assert!(matches!(strip_base("/app/", &mut req), BaseMatch::Inside));
        assert_eq!(
            (req.path.as_str(), req.target.as_str()),
            ("/src/a.js", "/src/a.js?t=1")
        );
        req.path = "/".into();
        assert!(matches!(strip_base("/app/", &mut req), BaseMatch::Redirect));
    }

    #[test]
    fn safe_paths() {
        assert_eq!(safe_relative("/src/a.js"), Some(PathBuf::from("src/a.js")));
        assert_eq!(safe_relative("/"), Some(PathBuf::new()));
        assert_eq!(safe_relative("/../etc/passwd"), None);
        assert_eq!(safe_relative("/src/../../x"), None);
    }
}
