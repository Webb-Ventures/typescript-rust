//! The module graph: loads, prepares, scans and resolves every reachable module in
//! parallel, then numbers modules deterministically (depth-first from the entries, imports
//! in source order) so output never depends on thread timing.

use std::sync::{Arc, Mutex};

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_config::path;

use crate::meta::{ModuleMeta, RecordKind};
use crate::options::Loader;
use crate::pool;
use crate::prepare::{self, PrepareContext, Prepared, Source};
use crate::resolve::{Resolution, ResolveKind};
use crate::{LineIndex, Message, Severity};

pub type ModuleId = u32;

/// Where a record points after resolution.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Target {
    Module(ModuleId),
    /// Kept as an import of this path.
    External(String),
    /// Unresolved (an error or warning was reported).
    Missing,
}

/// The [`Target::External`] string of a static import with a phase (`import defer`) or
/// import attributes: `path\0phase\0attrs`, so it stays its own import statement in the
/// output and keeps both. Other imports of an external use the bare path.
pub fn external_key(path: &str, phase: u8, attrs: &str) -> String {
    if phase == crate::meta::PHASE_EVALUATION && attrs.is_empty() {
        path.to_owned()
    } else {
        format!("{path}\0{phase}\0{attrs}")
    }
}

/// Splits an [`external_key`] into `(path, phase, attrs)`.
pub fn split_external(key: &str) -> (&str, u8, &str) {
    let mut it = key.splitn(3, '\0');
    let path = it.next().unwrap_or(key);
    let phase = it.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (path, phase, it.next().unwrap_or(""))
}

/// The word esbuild uses for an import phase in its messages.
fn phase_words(phase: u8) -> &'static str {
    if phase == crate::meta::PHASE_DEFER {
        "deferred imports"
    } else {
        "source phase imports"
    }
}

/// Prefix of the paths of modules disabled by a `browser` map.
pub const DISABLED_PREFIX: &str = "(disabled):";

/// Joins a file to the import attribute `type` it is loaded with in a module key: esbuild
/// loads `import x from './data.json' with { type: 'json' }` as its own module instance, with
/// the loader the type names (`json`, `text` or `bytes`) whatever the file's extension.
pub const WITH_TYPE_SEP: &str = "\0with:";

/// Joins a file to the `?query` or `#hash` its import named it with (`./file.txt?foo`):
/// esbuild gives each suffix its own module instance, and the file loader's URL keeps it.
pub const QUERY_SEP: &str = "\0query:";

/// A module file (after [`split_with_type`]) and its `?query` / `#hash` suffix, if any
/// ([`QUERY_SEP`]).
pub fn split_query(p: &str) -> (&str, &str) {
    p.split_once(QUERY_SEP).unwrap_or((p, ""))
}

/// A module key's file and its import attribute `type`, if any ([`WITH_TYPE_SEP`]).
pub fn split_with_type(key: &str) -> (&str, Option<&str>) {
    match key.split_once(WITH_TYPE_SEP) {
        Some((p, t)) => (p, Some(t)),
        None => (key, None),
    }
}

/// The `type` an import's attributes ask for (`assert { ... }` is ignored, like esbuild), or
/// the error for attributes a bundle cannot honour.
fn attribute_type(attrs: &str) -> Result<Option<&'static str>, String> {
    let Some(inner) = attrs
        .strip_prefix("with {")
        .and_then(|a| a.strip_suffix('}'))
    else {
        return Ok(None);
    };
    let unquote = |s: &str| {
        let s = s.trim();
        s.strip_prefix(['\'', '"'])
            .and_then(|t| t.strip_suffix(['\'', '"']))
            .unwrap_or(s)
            .to_owned()
    };
    let mut ty = None;
    for part in inner.split(", ").filter(|p| !p.trim().is_empty()) {
        let Some((k, v)) = part.split_once(": ") else {
            continue;
        };
        let (k, v) = (unquote(k), unquote(v));
        if k != "type" {
            return Err(format!(
                "Importing with the {k:?} attribute is not supported"
            ));
        }
        ty = Some(match v.as_str() {
            "json" => "json",
            "text" => "text",
            "bytes" => "bytes",
            _ => {
                return Err(format!(
                    "Importing with a type attribute of {v:?} is not supported"
                ));
            }
        });
    }
    Ok(ty)
}

/// One module of the graph.
#[derive(Debug, Clone)]
pub struct Module {
    /// Absolute path (or `(disabled):...`).
    pub path: String,
    /// Path relative to the working directory, for messages, comments and the metafile.
    pub pretty: String,
    pub loader: Loader,
    pub source: Source,
    pub meta: Arc<ModuleMeta>,
    /// The prepared tree, when still in memory.
    pub prepared: Option<Arc<Prepared>>,
    /// Per record.
    pub targets: Vec<Target>,
    /// The package's `sideEffects` allows side effects in this file.
    pub side_effects: bool,
    /// `.mjs`/`.mts` or a `"type": "module"` package: default imports of CommonJS are
    /// `module.exports` (Node's semantics).
    pub node_mode: bool,
    /// Hash of everything preparation depends on.
    pub prep_key: [u8; 32],
    /// Imported `with { type: 'json' }`: the module has a default export only.
    pub default_only: bool,
}

impl Module {
    pub fn message_at(&self, severity: Severity, pos: u32, text: String) -> Message {
        let text_of = match &self.source.css {
            Some(sheet) => &*sheet.src,
            None => &*self.source.js,
        };
        LineIndex::new(text_of).message(severity, &self.pretty, pos, text)
    }
}

#[derive(Debug, Default)]
pub struct Graph {
    pub modules: Vec<Module>,
    pub entries: Vec<ModuleId>,
    pub messages: Vec<Message>,
}

impl Graph {
    pub fn module(&self, id: ModuleId) -> &Module {
        &self.modules[id as usize]
    }
}

/// A loaded module before numbering.
#[derive(Debug)]
pub struct Loaded {
    pub path: String,
    /// `path` relative to the working directory ([`pretty_path`]), for messages and comments.
    pub pretty: String,
    pub loader: Loader,
    pub source: Source,
    pub meta: Arc<ModuleMeta>,
    pub prepared: Option<Arc<Prepared>>,
    pub prep_key: [u8; 32],
    pub messages: Vec<Message>,
}

/// Modules kept between rebuilds (by path).
#[derive(Debug, Default)]
pub struct ModuleCache {
    pub modules: Mutex<FxHashMap<String, Arc<Loaded>>>,
}

/// Version of the prepared-module format (part of every cache key).
pub const PREP_VERSION: u32 = 1;

#[derive(Debug)]
enum RawTarget {
    Path(String),
    External(String),
    Missing,
}

#[derive(Debug)]
struct Node {
    loaded: Arc<Loaded>,
    targets: Vec<RawTarget>,
    side_effects: bool,
    node_mode: bool,
    messages: Vec<Message>,
}

/// Inputs for [`build_graph`].
#[derive(Debug)]
pub struct GraphContext<'a> {
    pub prep: &'a PrepareContext,
    pub cache: &'a ModuleCache,
    pub disk: Option<&'a ts_r_cache::Cache>,
    pub threads: usize,
    /// When set, only these files may have changed since the cached modules were loaded:
    /// any other cached module is reused without reading the file.
    pub changed: Option<&'a FxHashSet<String>>,
    /// Counts modules parsed (not served from a cache) by this build.
    pub parsed: std::sync::atomic::AtomicUsize,
    /// Read module files ahead on one extra thread (when every module will be read).
    pub read_ahead: Option<&'a crate::fscache::CachedFs>,
}

/// Loads everything reachable from `entries` (absolute paths).
pub fn build_graph(ctx: &GraphContext<'_>, entries: &[String]) -> Graph {
    let seen: Mutex<FxHashSet<String>> = Mutex::new(entries.iter().cloned().collect());
    let nodes: Mutex<FxHashMap<String, Node>> = Mutex::default();
    let wd = ctx.prep.opts.working_dir.clone();
    let load_all = |queue: &(dyn Fn(String) + Sync)| {
        pool::run_dynamic(ctx.threads, entries.to_vec(), |p: String, sp| {
            let node = process(ctx, &p, &wd);
            for t in &node.targets {
                if let RawTarget::Path(q) = t {
                    let new = seen
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(q.clone());
                    if new {
                        if !q.starts_with(DISABLED_PREFIX)
                            && !q.starts_with(crate::data_url::PREFIX)
                        {
                            queue(split_query(split_with_type(q).0).0.to_owned());
                        }
                        sp.spawn(q.clone());
                    }
                }
            }
            nodes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(p, node);
        })
    };
    match ctx.read_ahead {
        Some(fs) => fs.read_ahead(load_all),
        None => load_all(&|_| {}),
    }
    let mut nodes = nodes.into_inner().unwrap_or_else(|e| e.into_inner());
    // A package imported through `module` and also `require`d uses `main` for both.
    let dual = ctx.prep.resolver.dual_package_entries();
    if !dual.is_empty() {
        let required: FxHashSet<String> = nodes
            .values()
            .flat_map(|n| {
                n.loaded
                    .meta
                    .records
                    .iter()
                    .zip(&n.targets)
                    .filter(|(r, _)| r.kind == RecordKind::Require)
                    .filter_map(|(_, t)| match t {
                        RawTarget::Path(q) => Some(q.clone()),
                        _ => None,
                    })
            })
            .collect();
        for n in nodes.values_mut() {
            let loaded = Arc::clone(&n.loaded);
            for (r, t) in loaded.meta.records.iter().zip(n.targets.iter_mut()) {
                if r.kind != RecordKind::Require
                    && let RawTarget::Path(q) = t
                    && let Some(main) = dual.get(q.as_str())
                    && required.contains(main)
                {
                    *q = main.clone();
                }
            }
        }
    }
    // Deterministic numbering: depth-first pre-order from the entries.
    let mut ids: FxHashMap<String, ModuleId> = FxHashMap::default();
    let mut order: Vec<String> = Vec::new();
    let mut stack: Vec<String> = entries.iter().rev().cloned().collect();
    while let Some(p) = stack.pop() {
        if ids.contains_key(&p) || !nodes.contains_key(&p) {
            continue;
        }
        ids.insert(p.clone(), order.len() as ModuleId);
        order.push(p.clone());
        if let Some(n) = nodes.get(&p) {
            for t in n.targets.iter().rev() {
                if let RawTarget::Path(q) = t
                    && !ids.contains_key(q)
                {
                    stack.push(q.clone());
                }
            }
        }
    }
    let mut g = Graph::default();
    for p in &order {
        let Some(n) = nodes.remove(p) else { continue };
        g.messages.extend(n.loaded.messages.iter().cloned());
        g.messages.extend(n.messages);
        let targets = n
            .targets
            .into_iter()
            .map(|t| match t {
                RawTarget::Path(q) => match ids.get(&q) {
                    Some(&id) => Target::Module(id),
                    None => Target::Missing,
                },
                RawTarget::External(e) => Target::External(e),
                RawTarget::Missing => Target::Missing,
            })
            .collect();
        let l = &n.loaded;
        g.modules.push(Module {
            path: l.path.clone(),
            pretty: l.pretty.clone(),
            loader: l.loader,
            source: l.source.clone(),
            meta: Arc::clone(&l.meta),
            prepared: l.prepared.clone(),
            targets,
            side_effects: n.side_effects,
            node_mode: n.node_mode,
            prep_key: l.prep_key,
            default_only: split_with_type(p).1 == Some("json"),
        });
        ctx.cache
            .modules
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(p.clone(), n.loaded);
    }
    copy_imports_external(&mut g);
    g.entries = entries.iter().filter_map(|e| ids.get(e).copied()).collect();
    g
}

/// The external path standing for the copy of `module` until chunks know its URL: the
/// asset's placeholder and the file name, so generated names read `import_a` for `a.png`.
pub fn copy_path(asset: &crate::prepare::Asset, module: &str) -> String {
    let file = path::file_name(split_query(module).0);
    format!("{}/{file}", asset.placeholder)
}

/// Whether an external path is a [`copy_path`].
pub fn is_copy_path(p: &str) -> bool {
    p.starts_with("__TSR_ASSET_")
}

/// JavaScript imports of a `copy`-loader file stay imports, of the copy: their target is
/// an external path that chunk assembly replaces with the copy's URL (the placeholder of
/// its [`crate::prepare::Asset`]). The copied module itself is never part of a chunk.
fn copy_imports_external(g: &mut Graph) {
    let copies: Vec<Option<String>> = g
        .modules
        .iter()
        .map(|m| {
            (m.loader == crate::options::Loader::Copy)
                .then(|| m.source.asset.as_ref().map(|a| copy_path(a, &m.path)))
                .flatten()
        })
        .collect();
    if copies.iter().all(Option::is_none) {
        return;
    }
    for m in &mut g.modules {
        if m.loader.is_css() {
            continue;
        }
        for (t, r) in m.targets.iter_mut().zip(&m.meta.records) {
            if let Target::Module(id) = *t
                && let Some(ph) = &copies[id as usize]
            {
                *t = Target::External(match r.kind {
                    RecordKind::Import | RecordKind::ReExport => {
                        external_key(ph, r.phase, &r.attrs)
                    }
                    _ => ph.clone(),
                });
            }
        }
    }
}

/// A stylesheet's references as module records (after the JavaScript stub's, which has
/// none, so record `i` is the sheet's record `i`), and its problems as messages.
fn css_records(
    sheet: &crate::css::Sheet,
    meta: &mut ModuleMeta,
    pretty: &str,
    messages: &mut Vec<Message>,
) {
    use crate::css::RecKind;
    debug_assert!(meta.records.is_empty());
    for (i, r) in sheet.records.iter().enumerate() {
        meta.records.push(crate::meta::Record {
            kind: match r.kind {
                RecKind::Import => RecordKind::CssImport,
                RecKind::Url => RecordKind::CssUrl,
                RecKind::Composes => RecordKind::CssComposes,
            },
            spec: r.spec.clone(),
            start: r.pos,
            end: r.pos,
            node: i as u32,
            in_try: false,
            phase: crate::meta::PHASE_EVALUATION,
            attrs: String::new(),
        });
    }
    let lines = LineIndex::new(&sheet.src);
    for m in sheet.messages.iter().filter(|m| m.error) {
        messages.push(lines.message(Severity::Error, pretty, m.pos, m.text.clone()));
    }
}

/// `path` relative to the working directory, `/`-separated.
pub fn pretty_path(wd: &str, p: &str) -> String {
    if let Some(rest) = p.strip_prefix(DISABLED_PREFIX) {
        return format!("{DISABLED_PREFIX}{rest}");
    }
    if p.starts_with(crate::data_url::PREFIX) {
        return format!("<{p}>");
    }
    let r = path::relative(wd, p, true);
    if r.is_empty() { p.to_owned() } else { r }
}

/// `./x`, `../x`, `.` or `..`.
fn is_relative(spec: &str) -> bool {
    spec.starts_with("./") || spec.starts_with("../") || spec == "." || spec == ".."
}

fn process(ctx: &GraphContext<'_>, p: &str, wd: &str) -> Node {
    let loaded = load_module(ctx, p, wd);
    let resolver = &ctx.prep.resolver;
    let p = split_query(split_with_type(p).0).0;
    let is_data = p.starts_with(crate::data_url::PREFIX);
    let dir = if is_data {
        String::new()
    } else {
        path::directory_of(p).to_owned()
    };
    let mut messages = Vec::new();
    let mut targets = Vec::with_capacity(loaded.meta.records.len());
    let pretty = &loaded.pretty;
    let mut lines: Option<LineIndex> = None;
    let text_of_lines: &str = match &loaded.source.css {
        Some(sheet) => &sheet.src,
        None => &loaded.source.js,
    };
    for r in &loaded.meta.records {
        let kind = match r.kind {
            RecordKind::Require => ResolveKind::Require,
            RecordKind::CssImport | RecordKind::CssComposes => ResolveKind::CssImport,
            RecordKind::CssUrl => ResolveKind::CssUrl,
            _ => ResolveKind::Import,
        };
        if r.kind.is_css() && (!ctx.prep.opts.bundle || ctx.prep.opts.css_prebundled) {
            targets.push(RawTarget::External(r.spec.clone()));
            continue;
        }
        let res = if p.starts_with(DISABLED_PREFIX) {
            Resolution::NotFound(String::new())
        } else if is_data && is_relative(&r.spec) {
            // A `data:` URL module has no directory to resolve against.
            Resolution::NotFound(format!("Could not resolve {:?}", r.spec))
        } else {
            resolver.resolve(&r.spec, &dir, kind)
        };
        if r.phase != crate::meta::PHASE_EVALUATION && ctx.prep.opts.bundle {
            // esbuild bundles neither phase: such imports must stay imports in ESM output.
            let fmt = ctx.prep.opts.effective_format();
            let text = if fmt != crate::Format::Esm {
                Some(format!(
                    "Bundling {} with the {:?} output format is not supported",
                    phase_words(r.phase),
                    fmt.as_str()
                ))
            } else if !matches!(res, Resolution::External(_)) {
                Some(format!(
                    "Bundling with {} is not supported unless they are external",
                    phase_words(r.phase)
                ))
            } else {
                None
            };
            if let Some(text) = text {
                let lines = lines.get_or_insert_with(|| LineIndex::new(text_of_lines));
                messages.push(lines.message(Severity::Error, pretty, r.start, text));
            }
        }
        targets.push(match res {
            // Attributes of an import of a `copy` file stay on the import of the copy.
            Resolution::File(f)
                if ctx.prep.opts.bundle
                    && matches!(r.kind, RecordKind::Import | RecordKind::ReExport)
                    && ctx.prep.opts.loader_for(&f) != Loader::Copy =>
            {
                match attribute_type(&r.attrs) {
                    Ok(Some(t)) => RawTarget::Path(format!("{f}{WITH_TYPE_SEP}{t}")),
                    Ok(None) => RawTarget::Path(f),
                    Err(text) => {
                        let lines = lines.get_or_insert_with(|| LineIndex::new(text_of_lines));
                        messages.push(lines.message(Severity::Error, pretty, r.start, text));
                        RawTarget::Path(f)
                    }
                }
            }
            Resolution::File(f) => match &ctx.prep.opts.dynamic_import_url {
                Some(hook) if r.kind == RecordKind::Dynamic => match (hook.0)(&f) {
                    Some(url) => RawTarget::External(url),
                    None => RawTarget::Path(f),
                },
                _ => RawTarget::Path(f),
            },
            Resolution::External(e) => match r.kind {
                RecordKind::Import | RecordKind::ReExport => {
                    RawTarget::External(external_key(&e, r.phase, &r.attrs))
                }
                _ => RawTarget::External(e),
            },
            Resolution::Disabled => RawTarget::Path(format!("{DISABLED_PREFIX}{}", r.spec)),
            Resolution::NotFound(msg) => {
                let lines = lines.get_or_insert_with(|| LineIndex::new(text_of_lines));
                let soft = r.in_try;
                let sev = if soft {
                    Severity::Warning
                } else {
                    Severity::Error
                };
                let text = if msg.is_empty() {
                    format!("Could not resolve {:?}", r.spec)
                } else {
                    msg
                };
                messages.push(lines.message(sev, pretty, r.start, text));
                if soft {
                    RawTarget::External(r.spec.clone())
                } else {
                    RawTarget::Missing
                }
            }
        });
    }
    let (side_effects, node_mode) = if p.starts_with(DISABLED_PREFIX) {
        (false, false)
    } else if is_data {
        (true, false)
    } else {
        let pkg = resolver.package_scope(&dir);
        let lower = p.to_ascii_lowercase();
        let node_mode = lower.ends_with(".mjs")
            || lower.ends_with(".mts")
            || ((lower.ends_with(".js") || lower.ends_with(".ts"))
                && pkg.as_ref().is_some_and(|k| k.is_module));
        (
            pkg.as_ref().is_none_or(|k| k.has_side_effects(p)),
            node_mode,
        )
    };
    Node {
        loaded,
        targets,
        side_effects,
        node_mode,
        messages,
    }
}

fn prep_key(ctx: &GraphContext<'_>, p: &str, source: &Source) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(&PREP_VERSION.to_le_bytes());
    h.update(p.as_bytes());
    h.update(&[0]);
    h.update(&source.hash);
    h.update(ctx.prep.cache_key(split_with_type(p).0).as_bytes());
    *h.finalize().as_bytes()
}

fn load_module(ctx: &GraphContext<'_>, key: &str, wd: &str) -> Arc<Loaded> {
    let (full, with_type) = split_with_type(key);
    let (p, query) = split_query(full);
    if let Some(changed) = ctx.changed
        && !changed.contains(p)
        && let Some(l) = ctx
            .cache
            .modules
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
    {
        return Arc::clone(l);
    }
    let opts = &ctx.prep.opts;
    let disabled = p.starts_with(DISABLED_PREFIX);
    let loader = match with_type {
        _ if disabled => Loader::Empty,
        Some("json") => Loader::Json,
        Some("text") => Loader::Text,
        Some(_) => Loader::Bytes,
        None => crate::data_url::loader(p).unwrap_or_else(|| opts.loader_for(p)),
    };
    let fs = ctx.prep.resolver.fs();
    let mut messages = Vec::new();
    let pretty = match with_type {
        Some(t) => format!("{}{query} with {{ type: '{t}' }}", pretty_path(wd, p)),
        None => format!("{}{query}", pretty_path(wd, p)),
    };
    let data = p.starts_with(crate::data_url::PREFIX);
    let loaded = if !disabled && !data && with_type.is_none() && !opts.has_loader(p) {
        let name = p.rsplit('/').next().unwrap_or(p);
        let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
        Err(format!(
            "No loader is configured for \".{ext}\" files: {pretty}"
        ))
    } else {
        prepare::load(fs.as_ref(), full, loader, opts)
    };
    let source = match loaded {
        Ok(s) => s,
        Err(e) => {
            messages.push(Message::new(Severity::Error, Some(pretty.clone()), e));
            Source {
                js: Arc::from(""),
                css: None,
                inline_url: None,
                asset: None,
                json_keys: None,
                hash: [0; 32],
            }
        }
    };
    let prep = prep_key(ctx, key, &source);
    if let Some(l) = ctx
        .cache
        .modules
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(key)
        && l.prep_key == prep
    {
        return Arc::clone(l);
    }
    let disk_key = ts_r_cache::Key::of(&prep);
    if messages.is_empty()
        && let Some(disk) = ctx.disk
        && let Some(blob) = disk.get(&disk_key)
        && let Some(meta) = ModuleMeta::decode(&blob.to_vec())
    {
        return Arc::new(Loaded {
            path: p.to_owned(),
            pretty,
            loader,
            source,
            meta: Arc::new(meta),
            prepared: None,
            prep_key: prep,
            messages,
        });
    }
    ctx.parsed
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let prepared = prepare::prepare(ctx.prep, p, &pretty, Arc::clone(&source.js), loader);
    let mut meta = crate::scan::scan(&prepared, !opts.ignore_annotations);
    if let Some(props) = &ctx.prep.props {
        meta.props = crate::props::collect(props, &prepared, true);
    }
    messages.extend(prepared.messages.iter().cloned());
    if let Some(sheet) = &source.css {
        css_records(sheet, &mut meta, &pretty, &mut messages);
    }
    if messages.is_empty()
        && let Some(disk) = ctx.disk
    {
        let _ = disk.put(&disk_key, &meta.encode());
    }
    Arc::new(Loaded {
        path: p.to_owned(),
        pretty,
        loader,
        source,
        meta: Arc::new(meta),
        prepared: Some(Arc::new(prepared)),
        prep_key: prep,
        messages,
    })
}
