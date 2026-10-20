//! Arranging a program for a layered check and sharing its lower levels.
//!
//! [`arrange`] orders a program's files by level ([`crate::levels`]) and finds every level
//! but the top one in the process-wide registry, or builds it there. A level is found by a
//! content key (the key of the level below, the options, and per file its path, text hash,
//! flags and how its imports resolved), so two projects with the same lib files, or the
//! same `node_modules` files, share one [`Layer`]: the program's own `Arc<SourceFile>`s for
//! that level are replaced by the layer's (they are equal in content).
//!
//! The registry holds layers weakly: a layer lives as long as a checker or a session uses
//! it.

use std::sync::{Arc, Mutex, OnceLock, Weak};

use rustc_hash::FxHashMap;
use ts_r_checker::{Checker, Layer, ProgramFiles, SourceFile};

use crate::levels::{self, Graph};

/// The result of [`arrange`].
#[derive(Debug)]
pub struct Arranged {
    /// `perm[old index] = new index` of every file.
    pub perm: Vec<u32>,
    /// The level the program's top checker builds on (`None`: no shared level).
    pub parent: Option<Arc<Layer>>,
    /// Kept levels: `(level, one past its last file)`, bottom first.
    pub ends: Vec<(u8, usize)>,
    /// Levels found in the registry / built.
    pub reused: u32,
    pub built: u32,
}

impl Arranged {
    /// The permutation is the identity.
    pub fn same_order(&self) -> bool {
        self.perm.iter().enumerate().all(|(i, &p)| p as usize == i)
    }
}

/// Orders `program`'s files by level (lib, dependencies, project, then `hot` files and
/// what needs them) and returns the shared levels to build its checker on:
/// `Checker::layered(program, arranged.parent.as_ref())`.
pub fn arrange(program: &mut ProgramFiles, hot: &[usize]) -> Arranged {
    share_sources(program);
    let graph = Graph::build(program);
    let lv = levels::plan(program, &graph, hot);
    let (perm, ends) = reorder_in_place(program, &lv);
    let mut parent: Option<Arc<Layer>> = None;
    let mut key = blake3::Hasher::new();
    key.update(b"ts-r layer 2\0");
    key.update(options_key(&program.options).as_bytes());
    let mut key = *key.finalize().as_bytes();
    let (mut reused, mut built) = (0, 0);
    let below_top = ends.len().saturating_sub(1);
    for &(_, end) in &ends[..below_top] {
        let start = parent.as_ref().map_or(0, |p| p.file_end() as usize);
        key = level_key(&key, program, start, end);
        if let Some(l) = registry_find(&key, parent.as_ref(), &program.files[start..end]) {
            program.files[start..end].clone_from_slice(l.files());
            parent = Some(l);
            reused += 1;
            continue;
        }
        let prefix = levels::with_files(program, program.files[..end].to_vec());
        match Checker::layered(&prefix, parent.as_ref()) {
            Ok(c) => {
                let l = c.top_layer().clone();
                registry_insert(key, &l);
                parent = Some(l);
                built += 1;
            }
            // A merge across levels the plan did not see: check the rest as one level.
            Err(_) => break,
        }
    }
    Arranged {
        perm,
        parent,
        ends,
        reused,
        built,
    }
}

/// Reorders `program.files` by `levels` (stable within a level); returns the permutation
/// (`perm[old] = new`) and, per kept level, one past its last file.
pub fn reorder_in_place(program: &mut ProgramFiles, levels: &[u8]) -> (Vec<u32>, Vec<(u8, usize)>) {
    let mut order: Vec<usize> = (0..program.files.len()).collect();
    order.sort_by_key(|&i| levels[i]);
    let mut perm = vec![0u32; order.len()];
    for (new, &old) in order.iter().enumerate() {
        perm[old] = new as u32;
    }
    let files: Vec<Arc<SourceFile>> = order.iter().map(|&i| program.files[i].clone()).collect();
    program.files = files;
    let mut ends: Vec<(u8, usize)> = Vec::new();
    for (k, &i) in order.iter().enumerate() {
        match ends.last_mut() {
            Some((l, end)) if *l == levels[i] => *end = k + 1,
            _ => ends.push((levels[i], k + 1)),
        }
    }
    (perm, ends)
}

/// The content key of `program.files[start..end]` on top of the level keyed `below`.
fn level_key(below: &[u8; 32], program: &ProgramFiles, start: usize, end: usize) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(below);
    for f in &program.files[start..end] {
        h.update(f.path.as_bytes());
        h.update(&[
            0,
            f.is_lib as u8,
            f.is_external as u8,
            f.bound.is_module as u8,
        ]);
        h.update(&text_hash(f));
        for (spec, _, mode) in crate::module_requests_with_modes(f, &program.options) {
            let r = crate::request_resolution(program, f, &spec, mode).and_then(|r| r.as_ref());
            h.update(spec.as_bytes());
            h.update(&[0]);
            if let Some(r) = r {
                h.update(r.resolved.as_bytes());
                h.update(&[r.included as u8]);
            }
            h.update(&[1]);
        }
        if let Some(ds) = program.deps.get(&f.path) {
            for d in ds {
                h.update(d.as_bytes());
                h.update(&[2]);
            }
        }
        h.update(&[3]);
    }
    *h.finalize().as_bytes()
}

type TextHashes = FxHashMap<usize, (Weak<SourceFile>, [u8; 32])>;

/// blake3 of a file's text, remembered per `Arc` (lib and dependency files are hashed once).
/// Options that cannot change what checking files computes, given the same files and
/// resolutions (both part of every level key): file locations, emit, tooling.
const NOT_CHECKING: [&str; 52] = [
    "paths",
    "baseUrl",
    "rootDir",
    "rootDirs",
    "outDir",
    "outFile",
    "declarationDir",
    "tsBuildInfoFile",
    "typeRoots",
    "types",
    "mapRoot",
    "sourceRoot",
    "sourceMap",
    "inlineSourceMap",
    "inlineSources",
    "declarationMap",
    "removeComments",
    "newLine",
    "emitBOM",
    "noEmit",
    "noEmitHelpers",
    "noEmitOnError",
    "emitDeclarationOnly",
    "stripInternal",
    "composite",
    "incremental",
    "diagnostics",
    "extendedDiagnostics",
    "generateCpuProfile",
    "generateTrace",
    "listFiles",
    "explainFiles",
    "listEmittedFiles",
    "pretty",
    "preserveWatchOutput",
    "plugins",
    "checkers",
    "singleThreaded",
    "quiet",
    "pprofDir",
    "version",
    "init",
    "all",
    "project",
    "watch",
    "help",
    "showConfig",
    "listFilesOnly",
    "ignoreConfig",
    "traceResolution",
    "disableSizeLimit",
    "assumeChangesOnlyAffectDirectDependencies",
];

/// The compiler options as far as checking can see them: every set option except
/// [`NOT_CHECKING`] ones, without where they were set. Projects whose tsconfigs differ only
/// in paths and emit settings share their lib and dependency levels.
pub fn options_key(options: &ts_r_config::CompilerOptions) -> String {
    let mut out = String::new();
    for d in ts_r_config::options::OPTIONS {
        if NOT_CHECKING.contains(&d.name) {
            continue;
        }
        if let Some(v) = options.get(d.id) {
            out.push_str(d.name);
            out.push('=');
            out.push_str(&format!("{v:?}"));
            out.push(';');
        }
    }
    out
}

type SourceKey = (String, [u8; 32], [u8; 32]);

/// Replaces non-lib files with equal ones another program of the process already holds
/// (same path, text, and everything binding depends on), so projects that share
/// dependencies or sources keep one parsed and bound copy. Returns the number replaced.
pub fn share_sources(program: &mut ProgramFiles) -> usize {
    static SOURCES: OnceLock<Mutex<FxHashMap<SourceKey, Weak<SourceFile>>>> = OnceLock::new();
    let opts = &program.options;
    let e = opts.effective();
    // Binding depends on the options through `bind_options_for`; its module-format part
    // (which needs a resolver) on `module` and `moduleDetection`.
    let common = format!("{:?}{:?}", e.module(), e.module_detection());
    let keys: Vec<Option<SourceKey>> = program
        .files
        .iter()
        .map(|f| {
            (!f.is_lib).then(|| {
                let b = ts_r_checker::program::bind_options_for::<ts_r_config::RealFs>(
                    &f.path, opts, None,
                );
                let bind = format!("{common}{b:?}{}{}", f.is_external, f.bound.is_module);
                (
                    f.path.clone(),
                    text_hash(f),
                    *blake3::hash(bind.as_bytes()).as_bytes(),
                )
            })
        })
        .collect();
    let Ok(mut map) = SOURCES.get_or_init(Default::default).lock() else {
        return 0;
    };
    if map.len() > 16384 {
        map.retain(|_, w| w.strong_count() > 0);
    }
    let mut n = 0;
    for (f, key) in program.files.iter_mut().zip(keys) {
        let Some(key) = key else { continue };
        match map.get(&key).and_then(Weak::upgrade) {
            Some(s) if Arc::ptr_eq(&s, f) => {}
            Some(s) => {
                *f = s;
                n += 1;
            }
            None => {
                map.insert(key, Arc::downgrade(f));
            }
        }
    }
    n
}

fn text_hash(f: &Arc<SourceFile>) -> [u8; 32] {
    static MEMO: OnceLock<Mutex<TextHashes>> = OnceLock::new();
    let memo = MEMO.get_or_init(Default::default);
    let addr = Arc::as_ptr(f) as usize;
    if let Ok(m) = memo.lock()
        && let Some((w, h)) = m.get(&addr)
        && w.upgrade().is_some_and(|g| Arc::ptr_eq(&g, f))
    {
        return *h;
    }
    let h = *blake3::hash(f.text.as_bytes()).as_bytes();
    if let Ok(mut m) = memo.lock() {
        if m.len() > 4096 {
            m.retain(|_, (w, _)| w.strong_count() > 0);
        }
        m.insert(addr, (Arc::downgrade(f), h));
    }
    h
}

type Registry = Vec<([u8; 32], Weak<Layer>)>;

fn registry() -> &'static Mutex<Registry> {
    static REG: OnceLock<Mutex<Registry>> = OnceLock::new();
    REG.get_or_init(Default::default)
}

fn registry_find(
    key: &[u8; 32],
    parent: Option<&Arc<Layer>>,
    files: &[Arc<SourceFile>],
) -> Option<Arc<Layer>> {
    let reg = registry().lock().ok()?;
    let l = reg
        .iter()
        .filter(|(k, _)| k == key)
        .find_map(|(_, w)| w.upgrade())?;
    let same_parent = match (l.parent(), parent) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        _ => false,
    };
    let same_files = l.files().len() == files.len()
        && l.files().iter().zip(files).all(|(a, b)| a.path == b.path);
    (same_parent && same_files).then_some(l)
}

fn registry_insert(key: [u8; 32], layer: &Arc<Layer>) {
    if let Ok(mut reg) = registry().lock() {
        reg.retain(|(k, w)| w.strong_count() > 0 && *k != key);
        reg.push((key, Arc::downgrade(layer)));
    }
}

/// Live shared levels: `(level, files, heap bytes)`, for status output and tests.
pub fn live_layers() -> Vec<(u8, usize, usize)> {
    let Ok(reg) = registry().lock() else {
        return Vec::new();
    };
    reg.iter()
        .filter_map(|(_, w)| w.upgrade())
        .map(|l| (l.level(), l.files().len(), l.heap_bytes()))
        .collect()
}
