//! `ts-r-dev`: the HMR dev server (`ts-r dev` execs this binary).

mod guard;
mod react_refresh;
mod vite_plugins;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ts_r_hmr::{
    CacheMode, DebounceOptions, DevConfig, DevServer, Resolve, Resolved, SimpleResolver, Transform,
};

const USAGE: &str = "\
ts-r-dev: dev server with hot module replacement (replaces vite dev)

Usage: ts-r-dev [root] [options]

Options:
  --port <n>         Port to listen on (default 5173, or server.port; tries the next ones if taken)
  --strict-port      Fail if the port is taken
  --host [addr]      Address to bind (default 127.0.0.1; no value = all addresses)
  --mode <mode>      Mode: import.meta.env.MODE and which .env.[mode] files load (default development)
  --base <path>      Public base path (default /, or the config's base)
  --open [path]      Open the browser (at path) once the server is ready
  --no-cache         Keep transform results in memory only (no ~/.cache/ts-r)
  --cache-dir <dir>  Use <dir> as the cache root
  --debounce <ms>    Watcher quiet window in milliseconds (default 3)
  --no-watch         Don't watch files
  --force            Pre-bundle dependencies again, ignoring the cached batch
  --no-react-refresh Turn off React Fast Refresh (on when a React Refresh runtime is installed)
  --no-plugins       Don't load vite.config plugins (run in a Node sidecar when present)
  --quiet            Don't log updates
  -h, --help         Show this help
  -V, --version      Show the version
";

struct Args {
    root: PathBuf,
    /// `None`: the config's `server.port`, else 5173.
    port: Option<u16>,
    strict_port: bool,
    host: Option<String>,
    mode: String,
    base: Option<String>,
    /// `Some("")`: open the base; `Some(path)`: open that path.
    open: Option<String>,
    cache: CacheMode,
    debounce_ms: Option<u64>,
    watch: bool,
    quiet: bool,
    force: bool,
    react_refresh: bool,
    plugins: bool,
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Args>, String> {
    let mut a = Args {
        root: PathBuf::from("."),
        port: None,
        strict_port: false,
        host: None,
        mode: "development".to_owned(),
        base: None,
        open: None,
        cache: CacheMode::Default,
        debounce_ms: None,
        watch: true,
        quiet: false,
        force: false,
        react_refresh: true,
        plugins: true,
    };
    let mut it = args.into_iter().peekable();
    let mut root_set = false;
    while let Some(arg) = it.next() {
        // `--opt=value` is the same as `--opt value`.
        let (arg, inline) = match arg.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k.to_owned(), Some(v.to_owned())),
            _ => (arg, None),
        };
        // `--open` and `--host` may come without a value: only a path (`/x`) or an address
        // (`0.0.0.0`, `::`, `localhost`) is taken as theirs, anything else is the root.
        match arg.as_str() {
            "--open" => {
                a.open = Some(
                    inline
                        .clone()
                        .or_else(|| it.next_if(|v| v.starts_with('/')))
                        .unwrap_or_default(),
                );
                continue;
            }
            "--host" => {
                let looks_like_host = |v: &String| {
                    !v.starts_with('-')
                        && (v.contains('.') || v.contains(':') || v == "localhost")
                        && !Path::new(v).is_dir()
                };
                a.host = Some(
                    inline
                        .clone()
                        .or_else(|| it.next_if(looks_like_host))
                        .unwrap_or_else(|| "0.0.0.0".to_owned()),
                );
                continue;
            }
            _ => {}
        }
        let mut value = |name: &str| {
            inline
                .clone()
                .or_else(|| it.next())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("ts-r-dev {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--port" => {
                let v = value("--port")?;
                a.port = Some(v.parse().map_err(|_| format!("invalid port: {v}"))?);
            }
            "--strict-port" => a.strict_port = true,
            "--mode" | "-m" => a.mode = value("--mode")?,
            "--base" => a.base = Some(value("--base")?),
            "--no-cache" => a.cache = CacheMode::Memory,
            "--cache-dir" => a.cache = CacheMode::Dir(PathBuf::from(value("--cache-dir")?)),
            "--debounce" => {
                let v = value("--debounce")?;
                a.debounce_ms = Some(v.parse().map_err(|_| format!("invalid --debounce: {v}"))?);
            }
            "--no-watch" => a.watch = false,
            "--force" => a.force = true,
            "--no-react-refresh" => a.react_refresh = false,
            "--no-plugins" => a.plugins = false,
            "--quiet" => a.quiet = true,
            s if s.starts_with('-') => return Err(format!("unknown option: {s}")),
            s => {
                if root_set {
                    return Err(format!("unexpected argument: {s}"));
                }
                a.root = PathBuf::from(s);
                root_set = true;
            }
        }
    }
    Ok(Some(a))
}

/// `scheme://host:port` of `url`.
fn origin_of(url: &str) -> &str {
    let start = url.find("://").map_or(0, |i| i + 3);
    match url[start..].find('/') {
        Some(i) => &url[..start + i],
        None => url,
    }
}

/// `server.open` / `--open`: the `BROWSER` command (`none` = don't), else the OS opener.
fn open_browser(url: &str) {
    let browser = std::env::var("BROWSER").ok().filter(|b| !b.is_empty());
    let mut cmd = match browser.as_deref() {
        Some(b) if b.eq_ignore_ascii_case("none") => return,
        Some(b) => std::process::Command::new(b),
        None if cfg!(target_os = "macos") => std::process::Command::new("open"),
        None if cfg!(windows) => {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", "start", ""]);
            c
        }
        None => std::process::Command::new("xdg-open"),
    };
    let spawned = cmd
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match spawned {
        // Reap it in the background so it doesn't linger as a zombie.
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => eprintln!("ts-r-dev: couldn't open the browser ({e}); open {url}"),
    }
}

/// The React Refresh runtime's specifier, if the project has one installed.
fn find_refresh_runtime(root: &Path) -> Option<String> {
    // Absolute, so the lookup walks up past a relative root (`ts-r-dev .` in an app folder
    // whose node_modules is in a parent directory).
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let root = root.as_path();
    let resolver = SimpleResolver::new(root);
    let importer = root.join("index.html");
    react_refresh::RUNTIME_CANDIDATES
        .iter()
        .find(|c| matches!(resolver.resolve(c, &importer), Ok(Resolved::Dep { .. })))
        .map(|c| (*c).to_owned())
}

fn main() -> ExitCode {
    let started = Instant::now();
    let args = match parse(std::env::args().skip(1)) {
        Ok(Some(a)) => a,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ts-r-dev: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let mut config = DevConfig::new(&args.root);
    // vite.config plugins, run by a Node sidecar (skipped when the only plugins are ones
    // with built-in replacements, like @vitejs/plugin-react). Node starts first; the
    // Rust-side setup below runs while it loads the config.
    let starting = match vite_plugins::find_config(&args.root).filter(|_| args.plugins) {
        None => None,
        Some(file) => {
            let cache = match &args.cache {
                CacheMode::Default => vite_plugins::ManifestCache::Default,
                CacheMode::Dir(d) => vite_plugins::ManifestCache::Dir(d.clone()),
                CacheMode::Memory => vite_plugins::ManifestCache::Off,
            };
            match vite_plugins::VitePlugins::begin(
                &args.root, &file, &cache, args.force, &args.mode,
            ) {
                Ok(p) => Some(p),
                Err(e) => {
                    eprintln!("ts-r-dev: failed to load plugins from {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
    };
    // TypeScript, TSX and JSX (ts_r_transform); plain .js/.mjs stay on the passthrough.
    // `parse<T>` call sites are expanded first (validators inlined, as in ts-r-run).
    let mut ts_transform = ts_r_transform::hmr::TsTransform::for_root(&args.root);
    let guard_options = guard::options_for_root(&args.root);
    let refresh_runtime = if args.react_refresh {
        find_refresh_runtime(&args.root)
    } else {
        None
    };
    let root_abs = ts_r_hmr::paths::canonicalize(&args.root).unwrap_or_else(|_| args.root.clone());
    let plugins = match starting.map(vite_plugins::Starting::finish) {
        None => None,
        Some(Ok(p)) => Some(p),
        Some(Err(e)) => {
            eprintln!("ts-r-dev: failed to load plugins from {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(m) = plugins.as_ref().map(|p| &p.manifest)
        && (m.jsx_import_source.is_some() || m.jsx_classic)
    {
        // JSX options the plugins set (`react({ jsxImportSource })`).
        let mut o = ts_transform.options().clone();
        if let Some(s) = &m.jsx_import_source {
            o.jsx_import_source = s.clone();
        }
        if m.jsx_classic {
            o.jsx = ts_r_transform::JsxMode::Classic;
        }
        ts_transform = ts_r_transform::hmr::TsTransform::new(o);
    }
    let ts: Arc<dyn Transform> = Arc::new(guard::GuardTransform::new(
        Arc::new(ts_transform),
        guard_options,
    ));
    match &refresh_runtime {
        // React Fast Refresh: registrations in every TS/JSX module, React's runtime
        // installed by a preamble before the app's scripts and pre-bundled with the deps.
        Some(runtime) => {
            config
                .transforms
                .push(Arc::new(react_refresh::ReactRefresh::new(ts)));
            config.html_inject.push(react_refresh::preamble());
            config.virtual_modules.push((
                react_refresh::GLUE_URL.to_owned(),
                react_refresh::glue_module(runtime),
            ));
            config.optimize_include.push(runtime.clone());
        }
        None => config.transforms.push(ts),
    }
    // node_modules: scanned from index.html and pre-bundled in one batch with code
    // splitting (ts_r_bundler), cached in ~/.cache/ts-r/dev-deps.
    // Source maps of the batch point at the original files under `node_modules`.
    config.dep_bundler = Arc::new(ts_r_bundler::hmr::DepBundle::new().serve_root(&root_abs));
    config.force_deps = args.force;
    config.mode = args.mode.clone();
    config.cache = args.cache;
    config.watch = args.watch;
    if let Some(ms) = args.debounce_ms {
        config.debounce = DebounceOptions {
            quiet: Duration::from_millis(ms),
            ..DebounceOptions::default()
        };
    }
    let mut plugin_lines = Vec::new();
    let mut open_config = None;
    if let Some(p) = plugins {
        let p = Arc::new(p);
        // define, env, base, server.* from the config; the command line wins.
        let weak = Arc::downgrade(&p);
        let rewrite = move |index: usize| -> ts_r_hmr::proxy::Rewrite {
            let weak = weak.clone();
            Arc::new(move |url: &str| weak.upgrade()?.proxy_rewrite(index, url))
        };
        let dev_warnings = vite_plugins::apply_dev_options(&p.manifest.dev, &mut config, &rewrite);
        open_config = match &p.manifest.dev["server"]["open"] {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Bool(true) => Some(String::new()),
            _ => None,
        };
        let m = &p.manifest;
        let startup = p.startup.map_or_else(
            || "starting in the background, hooks cached".to_owned(),
            |d| format!("{} ms", d.as_millis()),
        );
        let mut line = format!(
            "plugins: {} from {}",
            if m.plugins.is_empty() {
                "none".to_owned()
            } else {
                m.plugins.join(", ")
            },
            p.config.file_name().unwrap_or_default().to_string_lossy()
        );
        if p.is_running() {
            let pid = p.pid().unwrap_or(0);
            line.push_str(&format!(" (Node sidecar pid {pid}, {startup})"));
        }
        if !m.replaced.is_empty() {
            line.push_str(&format!("; built in: {}", m.replaced.join(", ")));
        }
        plugin_lines.push(line);
        plugin_lines.extend(
            m.warnings
                .iter()
                .chain(&dev_warnings)
                .map(|w| format!("plugins: {w}")),
        );
        // `optimizeDeps.include` (plugin-react lists React's entries): pre-bundled up front.
        for s in &m.optimize_include {
            if !config.optimize_include.contains(s) {
                config.optimize_include.push(s.clone());
            }
        }
        if p.is_running() {
            config.plugins = Some(p);
        }
    }
    if let Some(port) = args.port {
        config.port = port;
    }
    if args.strict_port {
        config.strict_port = true;
    }
    if let Some(host) = args.host {
        config.host = host;
    }
    if let Some(base) = &args.base {
        config.base = ts_r_hmr::server::normalize_base(base);
    }
    let open = args.open.clone().or(open_config);
    if !args.quiet {
        config.logger = Some(Arc::new(|line: &str| {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "  {line}");
            let _ = out.flush();
        }));
    }
    let server = match DevServer::start(config) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ts-r-dev: {e}");
            return ExitCode::FAILURE;
        }
    };
    {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(
            out,
            "\n  ts-r-dev {} ready in {} ms\n\n  Local: {}\n  Root:  {}\n",
            env!("CARGO_PKG_VERSION"),
            started.elapsed().as_millis(),
            server.url(),
            server.root().display()
        );
        for l in &plugin_lines {
            let _ = writeln!(out, "  {l}");
        }
        let _ = out.flush();
    }
    if let Some(path) = open {
        let url = server.url();
        let url = if path.is_empty() {
            url
        } else if path.starts_with("http://") || path.starts_with("https://") {
            path
        } else if path.starts_with('/') {
            // Resolved like `new URL(path, url)`: a root-absolute path replaces the base.
            format!("{}{path}", origin_of(&url))
        } else {
            format!("{url}{path}")
        };
        open_browser(&url);
    }
    // Serve until killed (Ctrl-C); the OS reclaims sockets and threads.
    loop {
        std::thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(v: &[&str]) -> Result<Option<Args>, String> {
        parse(v.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_flags() {
        let a = p(&[
            "app",
            "--port",
            "0",
            "--no-cache",
            "--quiet",
            "--debounce",
            "5",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(a.root, PathBuf::from("app"));
        assert_eq!(a.port, Some(0));
        assert_eq!(a.cache, CacheMode::Memory);
        assert_eq!(a.debounce_ms, Some(5));
        assert!(a.quiet && a.watch);
        assert!(p(&["--port"]).is_err());
        assert!(p(&["--port", "x"]).is_err());
        assert!(p(&["--bogus"]).is_err());
        assert!(p(&["a", "b"]).is_err());
    }
}
