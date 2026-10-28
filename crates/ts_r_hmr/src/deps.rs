//! Dependency optimizer: scan the app for bare imports up front, pre-bundle them all in one
//! batch with code splitting ([`DepBundler::bundle_batch`]), cache the result, and serve it
//! at `/@ts-r/deps/<name>?v=<version>` (like Vite's `/node_modules/.vite/deps/`).
//!
//! - **Scan** ([`scan_deps`]): from every `<script type="module" src>` in `index.html`, walk
//!   the source graph (static, re-export and literal dynamic imports) through the server's
//!   transforms and resolver, and collect every bare import that resolves into
//!   `node_modules` (JS only; CSS/JSON dependencies are served as files). Transforms go
//!   through the transform cache, so the scan also warms it for the page's first load.
//! - **Cache**: one `ts_r_cache` entry (namespace `dev-deps`) per root, keyed by the
//!   lockfile(s), `package.json`, the bundler's id, the server binary and the extra entries
//!   ([`DevConfig::optimize_include`](crate::DevConfig::optimize_include)). A warm start
//!   loads it and skips the scan.
//! - **Version**: `v` hashes that key plus the entry list, so browser-cached files
//!   (`immutable`) are never mixed across different batches.
//! - **Misses**: an import of a dependency that isn't in the batch (a new import, or one the
//!   scan couldn't see) re-runs the batch with it added, stores the new batch, and reloads
//!   open pages, because modules already running came from the previous batch.
//! - Source modules that import dependencies wait for the first batch; everything else is
//!   served meanwhile.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use ts_r_cache::{Cache, Key, KeyBuilder};

use crate::cache;
use crate::prebundle::{DepBatch, DepRequest};
use crate::render;
use crate::resolve::Resolved;
use crate::server::{self, CacheMode, Shared};
use crate::transform::{Mode, TransformInput, TransformOutput};

/// URL prefix of batch files.
pub const DEPS_PREFIX: &str = "/@ts-r/deps/";

const NAMESPACE: &str = "dev-deps";
const FORMAT_VERSION: u32 = 2;
const MAGIC: &[u8; 4] = b"TRDB";

/// A dependency entry: specifier as imported and the file it resolves to.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DepEntry {
    pub specifier: String,
    pub package: String,
    pub file: PathBuf,
}

/// A batch ready to serve.
#[derive(Debug)]
pub struct Optimized {
    pub version: String,
    /// Specifier → served file name.
    pub entry_of: HashMap<String, String>,
    /// File name → code with imports rewritten to `/@ts-r/deps/...?v=`.
    pub files: HashMap<String, Arc<String>>,
    /// What the batch was built from (kept to re-batch with an added entry), sorted by
    /// specifier; parallel to `raw.entries`.
    pub entries: Vec<DepEntry>,
    /// The bundler's output as returned, without source maps (what the disk cache stores).
    raw: DepBatch,
    /// Files that have a source map (served at `<name>.map`).
    has_map: HashSet<String>,
    /// Source maps kept in memory (only without a disk cache; otherwise each map is its own
    /// cache entry, read when DevTools asks for it).
    maps: HashMap<String, Arc<String>>,
}

impl Optimized {
    pub fn url_of(&self, specifier: &str) -> Option<String> {
        self.entry_of
            .get(specifier)
            .map(|name| format!("{DEPS_PREFIX}{name}?v={}", self.version))
    }
}

#[derive(Debug)]
enum State {
    /// The bundler has no batch support: per-specifier serving.
    Off,
    Pending,
    Ready(Arc<Optimized>),
    /// The batch failed; per-specifier serving takes over (the error was reported).
    Failed,
}

/// The optimizer's shared state (lives in [`Shared`]).
#[derive(Debug)]
pub struct Deps {
    state: Mutex<State>,
    ready: Condvar,
    /// Serializes re-batches.
    rebatch: Mutex<()>,
    disk: Option<Cache>,
    /// Lockfile + package.json + bundler + binary + include list.
    base: Key,
}

impl Deps {
    pub(crate) fn new(root: &Path, mode: &CacheMode, bundler_id: &str, include: &[String]) -> Deps {
        let disk = match mode {
            CacheMode::Default => Cache::open(NAMESPACE, FORMAT_VERSION).ok(),
            CacheMode::Dir(d) => Cache::open_in(d, NAMESPACE, FORMAT_VERSION).ok(),
            CacheMode::Memory => None,
        };
        Deps {
            state: Mutex::new(State::Pending),
            ready: Condvar::new(),
            rebatch: Mutex::new(()),
            disk,
            base: base_key(root, bundler_id, include),
        }
    }

    pub(crate) fn off() -> Deps {
        Deps {
            state: Mutex::new(State::Off),
            ready: Condvar::new(),
            rebatch: Mutex::new(()),
            disk: None,
            base: Key::of(b""),
        }
    }

    /// Waits for the first batch; `None` when batching is off or failed.
    pub fn wait(&self) -> Option<Arc<Optimized>> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            match &*s {
                State::Pending => {
                    s = self.ready.wait(s).unwrap_or_else(|e| e.into_inner());
                }
                State::Ready(o) => return Some(Arc::clone(o)),
                State::Off | State::Failed => return None,
            }
        }
    }

    /// Whether dependencies are batched (the bundler supports it).
    pub fn enabled(&self) -> bool {
        !matches!(
            *self.state.lock().unwrap_or_else(|e| e.into_inner()),
            State::Off
        )
    }

    fn set(&self, state: State) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = state;
        self.ready.notify_all();
    }

    fn cache_key(&self, root: &Path) -> Key {
        KeyBuilder::new("ts-r-dev/deps-batch")
            .key_field(&self.base)
            .str(&root.to_string_lossy())
            .key()
    }
}

/// Everything the cache key covers apart from the root.
fn base_key(root: &Path, bundler_id: &str, include: &[String]) -> Key {
    let mut b = KeyBuilder::new("ts-r-dev/deps");
    b.str(env!("CARGO_PKG_VERSION")).str(bundler_id);
    // The binary stands in for the bundler's revision (its version is not bumped per change).
    if let Ok(m) = std::env::current_exe().and_then(fs::metadata) {
        let t = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos());
        b.str(&format!("{}-{t}", m.len()));
    }
    for i in include {
        b.str("include").str(i);
    }
    if let Ok(bytes) = fs::read(root.join("package.json")) {
        b.str("package.json").bytes(&bytes);
    }
    // The nearest lockfile, searching up (workspaces keep it at the top).
    'up: for dir in root.ancestors() {
        for name in [
            "pnpm-lock.yaml",
            "package-lock.json",
            "yarn.lock",
            "bun.lock",
            "bun.lockb",
        ] {
            if let Ok(bytes) = fs::read(dir.join(name)) {
                b.str(name).bytes(&bytes);
                break 'up;
            }
        }
    }
    b.key()
}

/// Starts the optimizer on a background thread (called once at startup).
pub(crate) fn start(shared: &Arc<Shared>, force: bool) {
    if matches!(
        *shared.deps.state.lock().unwrap_or_else(|e| e.into_inner()),
        State::Off
    ) {
        return;
    }
    let s = Arc::clone(shared);
    let spawned = std::thread::Builder::new()
        .name("ts-r-dev-deps".into())
        .spawn(move || run_initial(&s, force));
    if spawned.is_err() {
        shared.deps.set(State::Failed);
    }
}

fn run_initial(shared: &Shared, force: bool) {
    let t = Instant::now();
    let key = shared.deps.cache_key(&shared.root);
    if !force
        && let Some(disk) = &shared.deps.disk
        && let Some(blob) = disk.get(&key)
        && let Some(opt) = decode(&blob.to_vec(), &shared.deps.base)
    {
        shared.log(&format!(
            "deps: {} pre-bundled entries from cache in {:.1} ms",
            opt.entries.len(),
            t.elapsed().as_secs_f64() * 1000.0
        ));
        shared.deps.set(State::Ready(Arc::new(opt)));
        return;
    }
    let entries = scan_deps(shared);
    let scanned = t.elapsed();
    match build(shared, entries) {
        Ok(mut opt) => {
            shared.log(&format!(
                "deps: pre-bundled {} entries into {} files in {:.0} ms (scan {:.0} ms)",
                opt.entries.len(),
                opt.files.len(),
                t.elapsed().as_secs_f64() * 1000.0,
                scanned.as_secs_f64() * 1000.0
            ));
            store(shared, &key, &mut opt);
            shared.deps.set(State::Ready(Arc::new(opt)));
        }
        Err(message) => {
            shared.log(&format!(
                "deps: pre-bundling failed, serving dependencies one by one: {message}"
            ));
            shared.deps.set(State::Failed);
        }
    }
}

/// The URL a source module imports `specifier` (which resolved to `file`) from, re-batching
/// when the current batch doesn't have it. `None`: serve it through `/@modules/`.
pub(crate) fn url_for(shared: &Shared, entry: &DepEntry) -> Option<String> {
    let opt = shared.deps.wait()?;
    if let Some(u) = opt.url_of(&entry.specifier) {
        return Some(u);
    }
    // A dependency the batch doesn't have: build a new batch with it.
    let _serial = shared
        .deps
        .rebatch
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let opt = shared.deps.wait()?;
    if let Some(u) = opt.url_of(&entry.specifier) {
        return Some(u); // another request added it meanwhile
    }
    let t = Instant::now();
    let mut entries = opt.entries.clone();
    // After a warm start the entries' files are unknown: resolve them again.
    let index = shared.root.join("index.html");
    entries.retain_mut(|e| {
        if !e.file.as_os_str().is_empty() {
            return true;
        }
        match shared.resolver.resolve(&e.specifier, &index) {
            Ok(Resolved::Dep { file, .. }) => {
                e.file = file;
                true
            }
            _ => false,
        }
    });
    entries.push(entry.clone());
    match build(shared, entries) {
        Ok(mut next) => {
            let url = next.url_of(&entry.specifier);
            shared.log(&format!(
                "deps: new dependency \"{}\", re-bundled {} entries in {:.0} ms; reloading the page",
                entry.specifier,
                next.entries.len(),
                t.elapsed().as_secs_f64() * 1000.0
            ));
            store(shared, &shared.deps.cache_key(&shared.root), &mut next);
            shared.deps.set(State::Ready(Arc::new(next)));
            // Modules already running in open pages come from the previous batch.
            shared
                .hub
                .broadcast(&serde_json::json!({ "type": "full-reload" }).to_string());
            url
        }
        Err(message) => {
            shared.log(&format!(
                "deps: re-bundling for \"{}\" failed, serving it on its own: {message}",
                entry.specifier
            ));
            None
        }
    }
}

/// Serves `/@ts-r/deps/<name>`: `(code, immutable)`.
pub(crate) fn file(
    shared: &Shared,
    name: &str,
    version: Option<&str>,
) -> Option<(Arc<String>, bool)> {
    let opt = shared.deps.wait()?;
    let code = opt.files.get(name)?;
    Some((Arc::clone(code), version == Some(opt.version.as_str())))
}

fn build(shared: &Shared, mut entries: Vec<DepEntry>) -> Result<Optimized, String> {
    entries.sort();
    entries.dedup_by(|a, b| a.specifier == b.specifier);
    let requests: Vec<DepRequest<'_>> = entries
        .iter()
        .map(|e| DepRequest {
            specifier: &e.specifier,
            package: &e.package,
            file: &e.file,
        })
        .collect();
    let batch = match shared.dep_bundler.bundle_batch(&requests) {
        Some(Ok(b)) => b,
        Some(Err(e)) => return Err(e.message),
        None => return Err("the dependency bundler has no batch support".into()),
    };
    if batch.entries.len() != entries.len() {
        return Err("the dependency bundler returned a malformed batch".into());
    }
    let specs: Vec<String> = entries.iter().map(|e| e.specifier.clone()).collect();
    Ok(finish(
        &shared.deps.base,
        &specs,
        entries,
        batch,
        HashSet::new(),
    ))
}

/// Computes the version, rewrites imports between batch files, links source maps
/// (`//# sourceMappingURL=<name>.map`) and turns each `.css` file into a style module that
/// its same-named `.js` file imports. Maps in `batch` move to [`Optimized::maps`]; `has_map`
/// names files whose maps are stored elsewhere (on disk).
fn finish(
    base: &Key,
    specs: &[String],
    entries: Vec<DepEntry>,
    mut batch: DepBatch,
    mut has_map: HashSet<String>,
) -> Optimized {
    let mut kb = KeyBuilder::new("ts-r-dev/deps-version");
    kb.key_field(base);
    for s in specs {
        kb.str(s);
    }
    // The output too (the code as bundled, before it is rewritten below; not the source maps,
    // which a cached batch reloads without): `--force` after patching a package (same lockfile)
    // must not reuse URLs that browsers cache as immutable.
    let mut outputs: Vec<&(String, TransformOutput)> = batch.files.iter().collect();
    outputs.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, out) in outputs {
        kb.str(name);
        kb.str(&out.code);
    }
    let version = kb.key().to_hex()[..8].to_owned();
    let mut maps = HashMap::new();
    for (name, out) in &mut batch.files {
        if let Some(m) = out.map.take() {
            has_map.insert(name.clone());
            maps.insert(name.clone(), Arc::new(m));
        }
    }
    let names: HashSet<&str> = batch.files.iter().map(|(n, _)| n.as_str()).collect();
    let mut files = HashMap::with_capacity(batch.files.len());
    for (name, out) in &batch.files {
        if name.ends_with(".css") {
            let url = format!("{DEPS_PREFIX}{name}");
            files.insert(name.clone(), Arc::new(render::css_module(&url, &out.code)));
            continue;
        }
        let mut code = render::rewrite_imports(&out.code, &out.imports, "", |_, rec| {
            let target = rec.specifier.strip_prefix("./")?;
            names
                .contains(target)
                .then(|| format!("{DEPS_PREFIX}{target}?v={version}"))
        });
        // Imports are hoisted, so appending keeps the source map's lines intact.
        if let Some(stem) = name.strip_suffix(".js")
            && names.contains(format!("{stem}.css").as_str())
        {
            code.push_str(&format!(
                "\nimport \"{DEPS_PREFIX}{stem}.css?v={version}\";\n"
            ));
        }
        if has_map.contains(name) {
            code.push_str(&format!("\n//# sourceMappingURL={name}.map\n"));
        }
        files.insert(name.clone(), Arc::new(code));
    }
    let entry_of = specs
        .iter()
        .cloned()
        .zip(batch.entries.iter().cloned())
        .collect();
    Optimized {
        version,
        entry_of,
        files,
        entries,
        raw: batch,
        has_map,
        maps,
    }
}

/// Serves `/@ts-r/deps/<name>.map`: from memory, or from its own disk cache entry.
pub(crate) fn map(shared: &Shared, file: &str) -> Option<Arc<String>> {
    let opt = shared.deps.wait()?;
    if let Some(m) = opt.maps.get(file) {
        return Some(Arc::clone(m));
    }
    if !opt.has_map.contains(file) {
        return None;
    }
    let disk = shared.deps.disk.as_ref()?;
    let blob = disk.get(&map_key(&shared.deps.base, &opt.version, file))?;
    String::from_utf8(blob.to_vec()).ok().map(Arc::new)
}

fn map_key(base: &Key, version: &str, file: &str) -> Key {
    KeyBuilder::new("ts-r-dev/deps-map")
        .key_field(base)
        .str(version)
        .str(file)
        .key()
}

// ---------------------------------------------------------------------------------------
// Scanning

/// Module scripts (`<script type="module" src>`) of an HTML page, in order.
pub fn html_module_scripts(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = lower[from..].find("<script") {
        let start = from + i;
        let Some(end) = lower[start..].find('>').map(|j| start + j) else {
            break;
        };
        let tag = &html[start..end];
        let tag_lower = &lower[start..end];
        from = end + 1;
        if !(tag_lower.contains("type=\"module\"") || tag_lower.contains("type='module'")) {
            continue;
        }
        if let Some(src) = attr(tag, tag_lower, "src") {
            out.push(src);
        }
    }
    out
}

fn attr(tag: &str, tag_lower: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(i) = tag_lower[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        let before_ok = at > 0 && tag_lower.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = tag_lower[from..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let off = tag.len() - rest.len() + 1;
        let value = tag[off..].trim_start();
        let (q, body) = match value.chars().next()? {
            c @ ('"' | '\'') => (Some(c), &value[1..]),
            _ => (None, value),
        };
        let end = match q {
            Some(c) => body.find(c)?,
            None => body
                .find(|c: char| c.is_ascii_whitespace() || c == '>')
                .unwrap_or(body.len()),
        };
        return Some(body[..end].to_owned());
    }
    None
}

/// Walks the source graph from `index.html` and returns the dependency entries it imports,
/// plus [`DevConfig::optimize_include`](crate::DevConfig::optimize_include).
pub(crate) fn scan_deps(shared: &Shared) -> Vec<DepEntry> {
    let index = shared.root.join("index.html");
    let mut found: BTreeMap<String, DepEntry> = BTreeMap::new();
    for spec in &shared.optimize_include {
        if let Ok(Resolved::Dep {
            package,
            specifier,
            file,
        }) = shared.resolver.resolve(spec, &index)
        {
            found.entry(specifier.clone()).or_insert(DepEntry {
                specifier,
                package,
                file,
            });
        }
    }
    let Ok(html) = fs::read_to_string(&index) else {
        return found.into_values().collect();
    };
    let mut queue: VecDeque<PathBuf> = VecDeque::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for src in html_module_scripts(&html) {
        if let Ok(Resolved::File(p)) = shared.resolver.resolve(&src, &index)
            && seen.insert(p.clone())
        {
            queue.push_back(p);
        }
    }
    while let Some(file) = queue.pop_front() {
        let Some(out) = transform_for_scan(shared, &file) else {
            continue;
        };
        let mut specifiers: Vec<String> = out.imports.iter().map(|r| r.specifier.clone()).collect();
        // `import.meta.glob` imports are followed too (pages that import dependencies).
        let globs = crate::scan::scan_hot(&out.code).globs;
        if !globs.is_empty() {
            let no_alias = |_: &str| None;
            let cx = crate::glob::GlobContext {
                root: &shared.root,
                importer: &file,
                alias: &no_alias,
            };
            if let Ok(e) = crate::glob::expand(&out.code, &globs, &cx) {
                specifiers.extend(e.specifiers);
            }
        }
        for hit in server::resolve_for_scan(shared, &specifiers, &file)
            .into_iter()
            .flatten()
        {
            match hit {
                server::ScanHit::Dep(e) => {
                    found.entry(e.specifier.clone()).or_insert(e);
                }
                server::ScanHit::File(p) => {
                    let in_node_modules = p.components().any(|c| c.as_os_str() == "node_modules");
                    if !in_node_modules
                        && server::kind_for(&p) == crate::graph::ModuleKind::Script
                        && seen.insert(p.clone())
                    {
                        queue.push_back(p);
                    }
                }
            }
        }
    }
    found.into_values().collect()
}

/// The transform the browser will request, through the cache (so the scan warms it): the
/// same source (`load` hooks included) and key as the request's.
fn transform_for_scan(shared: &Shared, file: &Path) -> Option<Arc<TransformOutput>> {
    let transform = shared.transforms.iter().find(|t| t.handles(file))?;
    let url = server::url_for_file(&shared.root, file);
    let source = server::module_source(shared, file, &url).ok()?;
    let text = std::str::from_utf8(&source).ok()?;
    let id = url.trim_start_matches('/');
    let input = TransformInput {
        path: file,
        id,
        source: text,
        mode: Mode::Dev,
    };
    let plugin_id = crate::paths::id(file);
    let deps = transform.deps(&input);
    server::cached_transform(
        shared,
        file,
        transform.as_ref(),
        &input,
        &plugin_id,
        Key::of(&source),
        deps,
    )
    .ok()
    .map(|(out, _)| out)
}

// ---------------------------------------------------------------------------------------
// Cache payload: magic, entries (specifier, package, served name), files (name, encoded
// TransformOutput). Files are stored as the bundler produced them (relative imports), so
// the version is recomputed on load. No absolute paths: entry files are not stored (a
// re-batch after a warm start resolves them again).

/// Stores the batch, and each source map as its own entry (maps are large and only DevTools
/// reads them, so they leave memory once stored).
fn store(shared: &Shared, key: &Key, opt: &mut Optimized) {
    let Some(disk) = &shared.deps.disk else {
        return;
    };
    let mut maps_ok = true;
    for (name, m) in &opt.maps {
        maps_ok &= disk
            .put(
                &map_key(&shared.deps.base, &opt.version, name),
                m.as_bytes(),
            )
            .is_ok();
    }
    let _ = disk.put(key, &encode(opt));
    if disk.flush().is_ok() && maps_ok {
        opt.maps.clear();
    }
}

fn encode(opt: &Optimized) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(MAGIC);
    put_u32(&mut b, opt.entries.len() as u32);
    for (e, name) in opt.entries.iter().zip(&opt.raw.entries) {
        put_str(&mut b, &e.specifier);
        put_str(&mut b, &e.package);
        put_str(&mut b, name);
    }
    put_u32(&mut b, opt.raw.files.len() as u32);
    for (name, out) in &opt.raw.files {
        put_str(&mut b, name);
        let enc = cache::encode(out);
        put_u32(&mut b, enc.len() as u32);
        b.extend_from_slice(&enc);
    }
    let mut with_map: Vec<&String> = opt.has_map.iter().collect();
    with_map.sort();
    put_u32(&mut b, with_map.len() as u32);
    for n in with_map {
        put_str(&mut b, n);
    }
    b
}

fn decode(data: &[u8], base: &Key) -> Option<Optimized> {
    let mut r = Reader { data, pos: 0 };
    if r.take(4)? != MAGIC {
        return None;
    }
    let n = r.u32()? as usize;
    let mut specs = Vec::with_capacity(n.min(1 << 16));
    let mut entries = Vec::with_capacity(n.min(1 << 16));
    let mut names = Vec::with_capacity(n.min(1 << 16));
    for _ in 0..n {
        let spec = r.string()?;
        let package = r.string()?;
        let name = r.string()?;
        entries.push(DepEntry {
            specifier: spec.clone(),
            package,
            file: PathBuf::new(),
        });
        specs.push(spec);
        names.push(name);
    }
    let m = r.u32()? as usize;
    let mut files = Vec::with_capacity(m.min(1 << 16));
    for _ in 0..m {
        let name = r.string()?;
        let len = r.u32()? as usize;
        let out = cache::decode(r.take(len)?)?;
        files.push((name, out));
    }
    let k = r.u32()? as usize;
    let mut has_map = HashSet::with_capacity(k.min(1 << 16));
    for _ in 0..k {
        has_map.insert(r.string()?);
    }
    if r.pos != data.len() {
        return None;
    }
    let batch = DepBatch {
        entries: names,
        files,
    };
    // Entry files are not stored (empty = unknown); a re-batch resolves them again.
    Some(finish(base, &specs, entries, batch, has_map))
}

fn put_u32(b: &mut Vec<u8>, n: u32) {
    b.extend_from_slice(&n.to_le_bytes());
}

fn put_str(b: &mut Vec<u8>, s: &str) {
    put_u32(b, s.len() as u32);
    b.extend_from_slice(s.as_bytes());
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.data.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(s)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn string(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{ImportKind, ImportRecord};

    #[test]
    fn module_scripts_in_html() {
        let html = r#"<!doctype html><html><head><script>var a</script>
<script type="module" src="/src/main.tsx"></script>
<SCRIPT TYPE='module' SRC='./b.js'></SCRIPT>
<script type="module">import './inline.js'</script>
<script src="/classic.js"></script></head></html>"#;
        assert_eq!(html_module_scripts(html), ["/src/main.tsx", "./b.js"]);
    }

    fn sample_batch() -> DepBatch {
        let entry = "import { a } from \"./chunk-AB.js\";\nexport { a };\n".to_owned();
        let chunk = "export const a = 1;\n".to_owned();
        let at = entry.find("./chunk").unwrap() as u32;
        DepBatch {
            entries: vec!["react.js".into(), "react.js".into()],
            files: vec![
                (
                    "chunk-AB.js".into(),
                    TransformOutput {
                        code: chunk,
                        map: None,
                        imports: vec![],
                    },
                ),
                (
                    "react.js".into(),
                    TransformOutput {
                        code: entry,
                        map: Some("{\"version\":3}".into()),
                        imports: vec![ImportRecord {
                            specifier: "./chunk-AB.js".into(),
                            start: at,
                            end: at + 13,
                            kind: ImportKind::Static,
                        }],
                    },
                ),
                (
                    "react.css".into(),
                    TransformOutput {
                        code: ".a { color: red; }\n".into(),
                        map: None,
                        imports: vec![],
                    },
                ),
            ],
        }
    }

    fn entries() -> Vec<DepEntry> {
        ["react", "react/index.js"]
            .iter()
            .map(|s| DepEntry {
                specifier: (*s).into(),
                package: "react".into(),
                file: PathBuf::from("/x/node_modules/react/index.js"),
            })
            .collect()
    }

    #[test]
    fn batch_files_link_to_each_other_by_versioned_url_and_round_trip() {
        let base = Key::of(b"lock");
        let specs = vec!["react".to_owned(), "react/index.js".to_owned()];
        let opt = finish(&base, &specs, entries(), sample_batch(), HashSet::new());
        let v = opt.version.clone();
        assert_eq!(
            opt.url_of("react"),
            Some(format!("/@ts-r/deps/react.js?v={v}"))
        );
        assert_eq!(opt.url_of("react/index.js"), opt.url_of("react"));
        assert_eq!(opt.url_of("vue"), None);
        assert!(
            opt.files["react.js"].contains(&format!("\"/@ts-r/deps/chunk-AB.js?v={v}\"")),
            "{}",
            opt.files["react.js"]
        );
        // Different lockfile → different version.
        let other = finish(
            &Key::of(b"lock2"),
            &specs,
            entries(),
            sample_batch(),
            HashSet::new(),
        );
        assert_ne!(other.version, v);
        // Same lockfile, rebuilt from changed packages (`--force`) → different version.
        let mut patched = sample_batch();
        patched.files[0].1.code = "export const a = 2;\n".into();
        let rebuilt = finish(&base, &specs, entries(), patched, HashSet::new());
        assert_ne!(rebuilt.version, v);
        let again = finish(&base, &specs, entries(), sample_batch(), HashSet::new());
        assert_eq!(again.version, v);
        // Cache round trip (no absolute paths stored).
        let bytes = encode(&opt);
        assert!(!String::from_utf8_lossy(&bytes).contains("/x/node_modules"));
        let back = decode(&bytes, &base).expect("decode");
        assert_eq!(back.version, v);
        assert_eq!(back.files, opt.files);
        assert_eq!(back.entry_of, opt.entry_of);
        assert!(decode(&bytes[..bytes.len() - 1], &base).is_none());
        assert!(decode(b"junk", &base).is_none());
        // Source maps: linked from the file, kept out of the cached batch, served from memory
        // (or their own disk entry after a reload).
        assert!(opt.files["react.js"].contains("\n//# sourceMappingURL=react.js.map\n"));
        assert!(!opt.files["chunk-AB.js"].contains("sourceMappingURL"));
        assert_eq!(opt.maps["react.js"].as_str(), "{\"version\":3}");
        assert!(back.maps.is_empty() && back.has_map.contains("react.js"));
        assert!(!String::from_utf8_lossy(&bytes).contains("\"version\":3"));
        // CSS the entry imports: a style module, imported by the same-named JS file.
        assert!(
            opt.files["react.js"].contains(&format!("import \"/@ts-r/deps/react.css?v={v}\";")),
            "{}",
            opt.files["react.js"]
        );
        assert!(opt.files["react.css"].contains("__ts_r_update_style"));
        assert!(opt.files["react.css"].contains(".a { color: red; }"));
    }
}
