//! Evaluating `eslint.config.{js,mjs,cjs,ts,mts,cts}` with the project's own Node.
//!
//! A flat config is a JavaScript module, so the only faithful way to read it is to run it. We
//! run one tiny script (below) that imports each config file, flattens the exported array and
//! prints a JSON description of every config object. Results are cached in `ts_r_cache`
//! (namespace `lint-config`) keyed by the config's path and content and the nearest lockfile;
//! the cached entry lists the local files the config imported, with content hashes, and is
//! only used while they are unchanged. A warm run never starts Node.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use ts_r_cache::{Cache, Key, KeyBuilder};

/// Config file names, in ESLint's lookup order.
pub const ESLINT_CONFIG_NAMES: &[&str] = &[
    "eslint.config.js",
    "eslint.config.mjs",
    "eslint.config.cjs",
    "eslint.config.ts",
    "eslint.config.mts",
    "eslint.config.cts",
];

const CACHE_NAMESPACE: &str = "lint-config";
/// 2: entries are only written when the config's imports were tracked.
/// 3: evaluation keeps the TypeScript parser options.
const CACHE_FORMAT: u32 = 3;

/// The evaluation script. It reads a JSON array of absolute config paths on stdin and prints
/// a JSON array of `{ path, config?, deps?, error? }`. `deps` is missing when this Node has no
/// `module.registerHooks`, so the imports are unknown.
pub const EVAL_SCRIPT: &str = r#"
import { pathToFileURL, fileURLToPath } from 'node:url';
import module from 'node:module';
const deps = new Set();
const tracked = typeof module.registerHooks === 'function';
if (tracked) {
  module.registerHooks({
    resolve(s, c, next) {
      const r = next(s, c);
      if (r && typeof r.url === 'string' && r.url.startsWith('file:') && !r.url.includes('/node_modules/')) deps.add(r.url);
      return r;
    },
  });
}
let input = '';
for await (const chunk of process.stdin) input += chunk;
const paths = JSON.parse(input);
const pat = (p) => (typeof p === 'string' ? p : '<function>');
const strs = (v) => (typeof v === 'string' ? v : Array.isArray(v) ? v.filter((x) => typeof x === 'string') : undefined);
// Absent stays absent (parser options merge across config objects); paths stay paths.
const project = (v) => (v === undefined ? undefined : v === null ? null : strs(v) !== undefined ? strs(v) : !!v);
const service = (v) => (v === undefined ? undefined : v === null ? null : typeof v === 'object' ? {} : !!v);
const safe = (v) => {
  try { return JSON.parse(JSON.stringify(v, (k, x) => (typeof x === 'function' ? undefined : typeof x === 'bigint' ? String(x) : x))); } catch { return undefined; }
};
const ser = (o) => {
  if (!o || typeof o !== 'object') return { invalid: typeof o };
  const out = {};
  if (o.name !== undefined) out.name = String(o.name);
  if (o.basePath !== undefined) out.basePath = String(o.basePath);
  if (Array.isArray(o.files)) out.files = o.files.map((f) => (Array.isArray(f) ? f.map(pat) : pat(f)));
  if (Array.isArray(o.ignores)) out.ignores = o.ignores.map(pat);
  if (o.rules) out.rules = safe(o.rules);
  if (o.languageOptions) {
    const lo = o.languageOptions;
    const po = lo.parserOptions;
    out.languageOptions = {
      ecmaVersion: lo.ecmaVersion,
      sourceType: lo.sourceType,
      globals: safe(lo.globals),
      parser: lo.parser ? String((lo.parser.meta && lo.parser.meta.name) || lo.parser.name || '<custom>') : undefined,
      parserOptions: po ? { ecmaFeatures: safe(po.ecmaFeatures), jsxPragma: po.jsxPragma, jsxFragmentName: po.jsxFragmentName, emitDecoratorMetadata: po.emitDecoratorMetadata, experimentalDecorators: po.experimentalDecorators, isolatedDeclarations: po.isolatedDeclarations, lib: Array.isArray(po.lib) ? strs(po.lib) : undefined, project: project(po.project), projectService: service(po.projectService), tsconfigRootDir: typeof po.tsconfigRootDir === 'string' ? po.tsconfigRootDir : po.tsconfigRootDir instanceof URL ? fileURLToPath(po.tsconfigRootDir) : undefined } : undefined,
    };
  }
  if (o.linterOptions) out.linterOptions = safe(o.linterOptions);
  if (o.settings) out.settings = safe(o.settings);
  if (o.plugins) out.plugins = Object.keys(o.plugins);
  if (o.processor) out.processor = typeof o.processor === 'string' ? o.processor : '<object>';
  if (o.language) out.language = String(o.language);
  return out;
};
const results = [];
for (const p of paths) {
  deps.clear();
  try {
    const mod = await import(pathToFileURL(p).href);
    let cfg = await (mod.default !== undefined ? mod.default : mod);
    const arr = (Array.isArray(cfg) ? cfg : [cfg]).flat(Infinity);
    const d = [...deps].map((u) => fileURLToPath(u)).filter((f) => f !== p);
    results.push({ path: p, config: arr.map(ser), deps: tracked ? d : undefined });
  } catch (e) {
    results.push({ path: p, error: String((e && e.stack) || e) });
  }
}
process.stdout.write(JSON.stringify(results));
"#;

/// How to run Node.
#[derive(Clone, Debug)]
pub struct NodeOptions {
    /// The `node` executable (`TS_R_NODE`, else `node` on `PATH`).
    pub node: PathBuf,
    /// A command prefix such as `scripts/limit.sh --`; from `TS_R_LINT_NODE_WRAPPER`.
    pub wrapper: Vec<String>,
    pub timeout: Duration,
    pub use_cache: bool,
}

impl Default for NodeOptions {
    fn default() -> Self {
        NodeOptions {
            node: std::env::var_os("TS_R_NODE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("node")),
            wrapper: std::env::var("TS_R_LINT_NODE_WRAPPER")
                .map(|s| s.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default(),
            timeout: Duration::from_secs(120),
            use_cache: std::env::var_os("TS_R_LINT_NO_CONFIG_CACHE").is_none(),
        }
    }
}

/// The evaluated flat config of one file.
#[derive(Clone, Debug)]
pub struct Evaluated {
    pub path: PathBuf,
    /// The serialized config array.
    pub config: Value,
    pub from_cache: bool,
    /// Local files the config imports (absolute paths; `node_modules` excluded).
    pub deps: Vec<PathBuf>,
}

/// Evaluates several config files, using the cache where possible and one Node process for
/// the rest.
pub fn evaluate_all(paths: &[PathBuf], opts: &NodeOptions) -> Vec<Result<Evaluated, String>> {
    let cache = if opts.use_cache {
        Cache::open(CACHE_NAMESPACE, CACHE_FORMAT).ok()
    } else {
        None
    };
    let mut results: Vec<Option<Result<Evaluated, String>>> = vec![None; paths.len()];
    let mut keys: Vec<Option<Key>> = vec![None; paths.len()];
    let mut todo = Vec::new();
    for (i, p) in paths.iter().enumerate() {
        let Ok(content) = std::fs::read(p) else {
            results[i] = Some(Err(format!("cannot read {}", p.display())));
            continue;
        };
        let key = cache_key(p, &content);
        if let Some(c) = &cache
            && let Some(blob) = c.get(&key)
            && let Ok(v) = serde_json::from_slice::<Value>(&blob)
            && deps_unchanged(p, &v)
            && let Some(cfg) = v.get("config")
        {
            results[i] = Some(Ok(Evaluated {
                path: p.clone(),
                config: cfg.clone(),
                from_cache: true,
                deps: cached_deps(p, &v),
            }));
            continue;
        }
        keys[i] = Some(key);
        todo.push(i);
    }
    if !todo.is_empty() {
        let batch: Vec<PathBuf> = todo.iter().map(|&i| paths[i].clone()).collect();
        match run_node(&batch, opts) {
            Ok(Value::Array(items)) => {
                for (j, &i) in todo.iter().enumerate() {
                    let item = items.get(j).cloned().unwrap_or(Value::Null);
                    if let Some(err) = item.get("error").and_then(Value::as_str) {
                        results[i] = Some(Err(first_lines(err, 6)));
                        continue;
                    }
                    let Some(cfg) = item.get("config").cloned() else {
                        results[i] = Some(Err("no config printed".to_owned()));
                        continue;
                    };
                    if let (Some(c), Some(key), Some(payload)) =
                        (&cache, &keys[i], cache_payload(&paths[i], &cfg, &item))
                        && let Ok(bytes) = serde_json::to_vec(&payload)
                    {
                        let _ = c.put(key, &bytes);
                    }
                    let deps = item
                        .get("deps")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(PathBuf::from)
                        .collect();
                    results[i] = Some(Ok(Evaluated {
                        path: paths[i].clone(),
                        config: cfg,
                        from_cache: false,
                        deps,
                    }));
                }
            }
            Ok(_) => {
                for &i in &todo {
                    results[i] = Some(Err("unexpected output from the config script".to_owned()));
                }
            }
            Err(e) => {
                for &i in &todo {
                    results[i] = Some(Err(e.clone()));
                }
            }
        }
    }
    if let Some(c) = &cache {
        let _ = c.flush();
    }
    results
        .into_iter()
        .map(|r| r.unwrap_or_else(|| Err("not evaluated".to_owned())))
        .collect()
}

fn first_lines(s: &str, n: usize) -> String {
    s.lines().take(n).collect::<Vec<_>>().join("\n")
}

fn cache_key(path: &Path, content: &[u8]) -> Key {
    let mut kb = KeyBuilder::new("ts-r-lint/eslint-config");
    kb.str(env!("CARGO_PKG_VERSION"))
        .str(&path.to_string_lossy())
        .bytes(content);
    // The nearest lockfile: a dependency upgrade changes what the config imports.
    let mut dir = path.parent();
    while let Some(d) = dir {
        let mut found = false;
        for lock in [
            "pnpm-lock.yaml",
            "package-lock.json",
            "yarn.lock",
            "bun.lock",
            "bun.lockb",
        ] {
            if let Ok(b) = std::fs::read(d.join(lock)) {
                kb.str(lock).key_field(&Key::of(&b));
                found = true;
            }
        }
        if found {
            break;
        }
        dir = d.parent();
    }
    kb.key()
}

/// The cache entry for an evaluated config, or `None` when its imports were not tracked: an
/// entry without them would outlive edits to the files the config imports.
fn cache_payload(config: &Path, cfg: &Value, item: &Value) -> Option<Value> {
    let deps = item.get("deps").filter(|d| d.is_array())?;
    let deps = dep_hashes(config, Some(deps));
    Some(serde_json::json!({ "config": cfg, "deps": deps }))
}

/// `[{ path (relative to the config dir), hash }]` for the config's local imports.
fn dep_hashes(config: &Path, deps: Option<&Value>) -> Value {
    let base = config.parent().unwrap_or(Path::new("."));
    let mut out = Vec::new();
    for d in deps.and_then(Value::as_array).into_iter().flatten() {
        let Some(p) = d.as_str() else { continue };
        let pb = PathBuf::from(p);
        let Ok(bytes) = std::fs::read(&pb) else {
            continue;
        };
        let rel = relative_path(base, &pb);
        out.push(serde_json::json!({ "path": rel, "hash": Key::of(&bytes).to_hex() }));
    }
    Value::Array(out)
}

/// The absolute paths of the imports recorded in a cache payload.
fn cached_deps(config: &Path, payload: &Value) -> Vec<PathBuf> {
    let base = config.parent().unwrap_or(Path::new("."));
    payload
        .get("deps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|d| d.get("path").and_then(Value::as_str))
        .map(|p| base.join(p))
        .collect()
}

fn deps_unchanged(config: &Path, payload: &Value) -> bool {
    let base = config.parent().unwrap_or(Path::new("."));
    let Some(deps) = payload.get("deps").and_then(Value::as_array) else {
        return false;
    };
    deps.iter().all(|d| {
        let (Some(p), Some(h)) = (
            d.get("path").and_then(Value::as_str),
            d.get("hash").and_then(Value::as_str),
        ) else {
            return false;
        };
        match std::fs::read(base.join(p)) {
            Ok(b) => Key::of(&b).to_hex() == h,
            Err(_) => false,
        }
    })
}

/// `to` relative to `from` with `..` segments, `/`-separated.
fn relative_path(from: &Path, to: &Path) -> String {
    let f: Vec<_> = from.components().collect();
    let t: Vec<_> = to.components().collect();
    let common = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = Vec::new();
    for _ in common..f.len() {
        parts.push("..".to_owned());
    }
    for c in &t[common..] {
        parts.push(c.as_os_str().to_string_lossy().into_owned());
    }
    parts.join("/")
}

fn run_node(paths: &[PathBuf], opts: &NodeOptions) -> Result<Value, String> {
    let cwd = paths
        .first()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut argv: Vec<String> = opts.wrapper.clone();
    argv.push(opts.node.to_string_lossy().into_owned());
    argv.extend([
        "--no-warnings".to_owned(),
        "--input-type=module".to_owned(),
        "-e".to_owned(),
        EVAL_SCRIPT.to_owned(),
    ]);
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start Node ({}): {e}", opts.node.display()))?;
    let input: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(serde_json::to_string(&input).unwrap_or_default().as_bytes());
    }
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out_thread = std::thread::spawn(move || {
        let mut s = Vec::new();
        if let Some(o) = stdout.as_mut() {
            let _ = o.read_to_end(&mut s);
        }
        s
    });
    let err_thread = std::thread::spawn(move || {
        let mut s = Vec::new();
        if let Some(o) = stderr.as_mut() {
            let _ = o.read_to_end(&mut s);
        }
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {
                if start.elapsed() > opts.timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "evaluating the ESLint config took longer than {}s",
                        opts.timeout.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(format!("waiting for Node: {e}")),
        }
    };
    let out = out_thread.join().unwrap_or_default();
    let err = err_thread.join().unwrap_or_default();
    if !status.success() {
        return Err(format!(
            "Node exited with {status}: {}",
            first_lines(&String::from_utf8_lossy(&err), 8)
        ));
    }
    serde_json::from_slice(&out).map_err(|e| format!("bad JSON from the config script: {e}"))
}

/// The nearest ESLint config file at or above `dir`.
pub fn find_up(dir: &Path) -> Option<PathBuf> {
    let mut d = Some(dir);
    while let Some(cur) = d {
        for name in ESLINT_CONFIG_NAMES {
            let p = cur.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
        d = cur.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        assert_eq!(
            relative_path(Path::new("/a/b"), Path::new("/a/c/d.js")),
            "../c/d.js"
        );
        assert_eq!(relative_path(Path::new("/a"), Path::new("/a/x.js")), "x.js");
    }

    #[test]
    fn typescript_parser_options_survive_evaluation() {
        let opts = NodeOptions {
            use_cache: false,
            ..NodeOptions::default()
        };
        let node_ok = Command::new(&opts.node)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !node_ok {
            eprintln!("skipping: node not found");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ts-r-lint-eval-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("eslint.config.mjs");
        // A `file:` URL names an absolute path on Windows only with a drive (`fileURLToPath`
        // rejects `file:///repo/x/` there), and converts to the native form.
        let (root_url, root_path) = if cfg!(windows) {
            ("file:///C:/repo/x/", r"C:\repo\x\")
        } else {
            ("file:///repo/x/", "/repo/x/")
        };
        std::fs::write(
            &config,
            "export default [\n\
             { languageOptions: { parserOptions: { lib: ['dom', 'es2022'], project: ['./tsconfig.app.json'], tsconfigRootDir: '/repo/app' } } },\n\
             { languageOptions: { parserOptions: { projectService: { allowDefaultProject: ['*.js'] } } } },\n\
             { languageOptions: { parserOptions: { project: './one.json', tsconfigRootDir: new URL('file:///repo/x/') } } },\n\
             { languageOptions: { parserOptions: { project: true } } },\n\
             ];\n"
                .replace("file:///repo/x/", root_url),
        )
        .unwrap();
        let r = evaluate_all(std::slice::from_ref(&config), &opts);
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = r.into_iter().next().unwrap().expect("evaluates").config;
        let po = |i: usize| cfg[i]["languageOptions"]["parserOptions"].clone();
        assert_eq!(po(0)["lib"], serde_json::json!(["dom", "es2022"]));
        assert_eq!(po(0)["project"], serde_json::json!(["./tsconfig.app.json"]));
        assert_eq!(po(0)["tsconfigRootDir"], "/repo/app");
        assert!(po(0).get("projectService").is_none(), "{}", po(0));
        assert_eq!(po(1)["projectService"], serde_json::json!({}));
        assert!(po(1).get("project").is_none(), "{}", po(1));
        assert_eq!(po(2)["project"], "./one.json");
        assert_eq!(po(2)["tsconfigRootDir"], root_path);
        assert_eq!(po(3)["project"], true);
    }

    #[test]
    fn untracked_imports_are_not_cached() {
        let config = Path::new("/p/eslint.config.mjs");
        let cfg = serde_json::json!([{ "rules": { "no-debugger": "off" } }]);
        // Node without `module.registerHooks`: the script prints no `deps`.
        let untracked = serde_json::json!({ "path": "/p/eslint.config.mjs", "config": cfg });
        assert_eq!(cache_payload(config, &cfg, &untracked), None);
        let tracked =
            serde_json::json!({ "path": "/p/eslint.config.mjs", "config": cfg, "deps": [] });
        let payload = cache_payload(config, &cfg, &tracked).unwrap();
        assert!(deps_unchanged(config, &payload));
        assert!(!deps_unchanged(
            config,
            &serde_json::json!({ "config": cfg })
        ));
    }
}
