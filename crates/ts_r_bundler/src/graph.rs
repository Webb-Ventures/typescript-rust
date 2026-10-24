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
}

impl Module {
    pub fn message_at(&self, severity: Severity, pos: u32, text: String) -> Message {
        LineIndex::new(&self.source.js).message(severity, &self.pretty, pos, text)
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
                        if !q.starts_with(DISABLED_PREFIX) {
                            queue(q.clone());
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
        });
        ctx.cache
            .modules
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(p.clone(), n.loaded);
    }
    g.entries = entries.iter().filter_map(|e| ids.get(e).copied()).collect();
    g
}

/// `path` relative to the working directory, `/`-separated.
pub fn pretty_path(wd: &str, p: &str) -> String {
    if let Some(rest) = p.strip_prefix(DISABLED_PREFIX) {
        return format!("{DISABLED_PREFIX}{rest}");
    }
    let r = path::relative(wd, p, true);
    if r.is_empty() { p.to_owned() } else { r }
}

fn process(ctx: &GraphContext<'_>, p: &str, wd: &str) -> Node {
    let loaded = load_module(ctx, p, wd);
    let resolver = &ctx.prep.resolver;
    let dir = path::directory_of(p).to_owned();
    let mut messages = Vec::new();
    let mut targets = Vec::with_capacity(loaded.meta.records.len());
    let pretty = &loaded.pretty;
    let mut lines: Option<LineIndex> = None;
    for r in &loaded.meta.records {
        let kind = match r.kind {
            RecordKind::Require => ResolveKind::Require,
            _ => ResolveKind::Import,
        };
        let res = if p.starts_with(DISABLED_PREFIX) {
            Resolution::NotFound(String::new())
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
                let lines = lines.get_or_insert_with(|| LineIndex::new(&loaded.source.js));
                messages.push(lines.message(Severity::Error, pretty, r.start, text));
            }
        }
        targets.push(match res {
            Resolution::File(f) => RawTarget::Path(f),
            Resolution::External(e) => match r.kind {
                RecordKind::Import | RecordKind::ReExport => {
                    RawTarget::External(external_key(&e, r.phase, &r.attrs))
                }
                _ => RawTarget::External(e),
            },
            Resolution::Disabled => RawTarget::Path(format!("{DISABLED_PREFIX}{}", r.spec)),
            Resolution::NotFound(msg) => {
                let lines = lines.get_or_insert_with(|| LineIndex::new(&loaded.source.js));
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
    h.update(ctx.prep.cache_key(p).as_bytes());
    *h.finalize().as_bytes()
}

fn load_module(ctx: &GraphContext<'_>, p: &str, wd: &str) -> Arc<Loaded> {
    if let Some(changed) = ctx.changed
        && !changed.contains(p)
        && let Some(l) = ctx
            .cache
            .modules
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(p)
    {
        return Arc::clone(l);
    }
    let opts = &ctx.prep.opts;
    let disabled = p.starts_with(DISABLED_PREFIX);
    let loader = if disabled {
        Loader::Empty
    } else {
        opts.loader_for(p)
    };
    let fs = ctx.prep.resolver.fs();
    let mut messages = Vec::new();
    let pretty = pretty_path(wd, p);
    let loaded = if !disabled && !opts.has_loader(p) {
        let name = p.rsplit('/').next().unwrap_or(p);
        let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
        Err(format!(
            "No loader is configured for \".{ext}\" files: {pretty}"
        ))
    } else {
        prepare::load(fs.as_ref(), p, loader, opts)
    };
    let source = match loaded {
        Ok(s) => s,
        Err(e) => {
            messages.push(Message::new(Severity::Error, Some(pretty.clone()), e));
            Source {
                js: Arc::from(""),
                css: None,
                asset: None,
                hash: [0; 32],
            }
        }
    };
    let key = prep_key(ctx, p, &source);
    if let Some(l) = ctx
        .cache
        .modules
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(p)
        && l.prep_key == key
    {
        return Arc::clone(l);
    }
    let disk_key = ts_r_cache::Key::of(&key);
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
            prep_key: key,
            messages,
        });
    }
    ctx.parsed
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let prepared = prepare::prepare(ctx.prep, p, &pretty, Arc::clone(&source.js), loader);
    let meta = crate::scan::scan(&prepared, !opts.ignore_annotations);
    messages.extend(prepared.messages.iter().cloned());
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
        prep_key: key,
        messages,
    })
}
