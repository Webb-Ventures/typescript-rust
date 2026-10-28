//! The persisted history's fast path (design record 0005, implementation notes "Lazy
//! loading"): a warm `ts-r-check` that parses only what it must.
//!
//! 1. **Scan** every saved file in parallel: a matching stat fingerprint means the saved
//!    text hash still holds (nothing read); otherwise the file is read and hashed. Binding
//!    options and module resolutions are verified. Edited files are parsed and their
//!    requests resolved.
//! 2. Nothing changed: the stored output.
//! 3. Otherwise rebuild the program's file list the way `load_program` does (same order, same
//!    `node_modules` flags) from the saved requests, parsing only new files.
//! 4. Shapes of the edited files, stored shapes for the rest, deep hashes: the files to
//!    re-check, as [`History::check`](crate::incremental::History::check) decides.
//! 5. Link diagnostics can only change for files sharing a global, ambient-module or
//!    augmentation key with an edited or removed file; those are re-linked.
//! 6. Parse what the re-checked files reach through their uses (and the re-linked files'
//!    merge partners), check on a program of just those files plus the libs, and reuse
//!    everything else.
//!
//! Whatever this path does not handle (libs or options changed, a missing lib file that
//! appeared, an edited or new file's `/// <reference lib>` adding a lib, blocking syntax
//! errors, a manifest saved after one) returns `None`: the caller loads the whole program.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::SourceKind;
use ts_r_binder::{BindOptions, RequestKind};
use ts_r_cache::{Cache, Key};
use ts_r_checker::driver::{CHECKER_GRAMMAR_CODES, link_diagnostic_reported};
use ts_r_checker::program::{
    Resolution, bind_options_for, expand_wildcard_types, reference_target, resolve_request,
    resolve_request_in,
};
use ts_r_checker::{CheckRun, Checker, LibStore, ProgramFiles, SourceFile, check_files};
use ts_r_config::{CompilerOptions, FileSystem, ParsedConfig, StrictFlag};
use ts_r_diagnostics::{Diagnostic, FileId};
use ts_r_resolver::{ResolutionMode, Resolver, ResolverOptions};

use crate::incremental::{
    CheckStats, FileState, PathDiag, global_diagnostics, program_key_from, resolved_hash_of,
    syntax_only,
};
use crate::libinfo::{self, DOM_GROUP};
use crate::persist::{
    Manifest, Mode, Outcome, Paths, Phases, SavedFile, SavedReference, SavedRequest, encode,
    has_pragma, kind_code, kind_of, sidecar_key,
};
use crate::shape::{DeepHashes, FileShape, Hash, Provides, Ref, shape_of};
use crate::stat::{self, Sidecar, Stat};

/// One fast-path run.
pub(crate) struct Run<'a, F: FileSystem> {
    pub fs: &'a F,
    pub cache: &'a Cache,
    pub key: &'a Key,
    /// Digest of the manifest bytes being checked against: refreshed stat fingerprints are
    /// tied to this manifest, not to whatever the cache holds by the time they are saved.
    pub digest: [u8; 32],
    pub paths: &'a Paths<'a>,
    pub config: &'a ParsedConfig,
    pub libs: Option<&'a LibStore>,
    pub threads: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    /// Text, binding options and resolutions as saved.
    Same,
    /// Same text and binding options; resolutions or references differ.
    Moved,
    /// Text or binding options differ (parsed).
    Edited,
    /// Unreadable now.
    Gone,
}

/// What the scan found about one saved file.
struct Scanned {
    status: Status,
    hash: Hash,
    bind: Hash,
    stat: Option<Stat>,
    text: Option<String>,
    file: Option<SourceFile>,
    /// The text mentions `@ts-r` (the saved flag when the text was not read).
    pragma: bool,
    /// Current requests and references (`Moved`, `Edited`).
    requests: Option<Vec<SavedRequest>>,
    references: Option<Vec<SavedReference>>,
}

impl Scanned {
    fn gone() -> Scanned {
        Scanned {
            status: Status::Gone,
            hash: [0; 32],
            bind: [0; 32],
            stat: None,
            text: None,
            file: None,
            pragma: false,
            requests: None,
            references: None,
        }
    }
}

/// A file that is not in the manifest.
struct NewFile {
    rel: String,
    file: Option<SourceFile>,
    hash: Hash,
    bind: Hash,
    stat: Option<Stat>,
    pragma: bool,
    requests: Vec<SavedRequest>,
    references: Vec<SavedReference>,
}

#[derive(Clone, Copy, Debug)]
enum Src {
    Saved(usize),
    New(usize),
}

/// One file of the rebuilt program (relative path).
struct Sk {
    rel: String,
    external: bool,
    src: Src,
    /// Included resolutions and reference targets, in `load_program`'s order.
    deps: Vec<String>,
}

/// Runs `f` over `items` on up to `threads` threads; results in order.
pub(crate) fn par_map<T: Sync, R: Send>(
    items: &[T],
    threads: usize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let n = threads.clamp(1, 4).min(items.len().max(1));
    if n == 1 {
        return items.iter().map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let out: Vec<Mutex<Option<R>>> = items.iter().map(|_| Mutex::new(None)).collect();
    let work = || {
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(it) = items.get(i) else { break };
            let r = f(it);
            *out[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        }
    };
    std::thread::scope(|s| {
        for _ in 1..n {
            s.spawn(work);
        }
        work();
    });
    out.into_iter()
        .filter_map(|m| m.into_inner().unwrap_or_else(|e| e.into_inner()))
        .collect()
}

fn hash_of(text: &str) -> Hash {
    *blake3::hash(text.as_bytes()).as_bytes()
}

fn bind_key(o: &BindOptions) -> Hash {
    *blake3::hash(format!("{o:?}").as_bytes()).as_bytes()
}

/// `should_check` from a path alone.
fn checks(options: &CompilerOptions, path: &str) -> bool {
    let kind = SourceKind::from_path(path);
    if kind.is_declaration() && options.skip_lib_check() == Some(true) {
        return false;
    }
    if !kind.is_typescript() && options.check_js() != Some(true) {
        return false;
    }
    true
}

fn blocking(d: &Diagnostic) -> bool {
    d.is_error() && !CHECKER_GRAMMAR_CODES.contains(&d.code)
}

/// Link keys of a file: what it merges with in other files.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
enum LinkKey<'s> {
    Global(&'s str),
    Ambient(&'s str),
    /// Augmentations of a module (path, or unresolved specifier).
    Aug(&'s str),
    /// A module augmentation's non-relative specifier: it merges into a matching ambient
    /// module.
    AmbSpec(&'s str),
}

fn link_keys<'s>(rel: &'s str, s: &'s FileShape) -> Vec<LinkKey<'s>> {
    let mut out: Vec<LinkKey<'s>> = vec![LinkKey::Aug(rel)];
    for p in &s.provides {
        match &p.what {
            Provides::Global(g) => out.push(LinkKey::Global(g)),
            Provides::Ambient(a) => out.push(LinkKey::Ambient(a)),
            Provides::Augments(t) => {
                out.push(LinkKey::Aug(t));
                for r in &p.deps {
                    if let crate::shape::Ref::Ambient(spec) = r {
                        out.push(LinkKey::AmbSpec(spec));
                    }
                }
            }
            Provides::Export(_) => {}
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Which files provide which link keys.
struct Partners<'s> {
    by_key: FxHashMap<LinkKey<'s>, Vec<usize>>,
    ambient: Vec<(&'s str, Vec<usize>)>,
    specs: Vec<(&'s str, Vec<usize>)>,
}

impl<'s> Partners<'s> {
    fn new(keys: impl Iterator<Item = (usize, LinkKey<'s>)>) -> Partners<'s> {
        let mut by_key: FxHashMap<LinkKey<'s>, Vec<usize>> = FxHashMap::default();
        for (i, k) in keys {
            by_key.entry(k).or_default().push(i);
        }
        let mut ambient = Vec::new();
        let mut specs = Vec::new();
        for (k, v) in &by_key {
            match k {
                LinkKey::Ambient(a) => ambient.push((*a, v.clone())),
                LinkKey::AmbSpec(x) => specs.push((*x, v.clone())),
                _ => {}
            }
        }
        ambient.sort();
        specs.sort();
        Partners {
            by_key,
            ambient,
            specs,
        }
    }

    /// Whether module `t` has augmentations in other files.
    fn augmented(&self, t: &str) -> bool {
        self.by_key
            .get(&LinkKey::Aug(t))
            .is_some_and(|v| v.len() > 1)
    }

    /// Files whose link diagnostics can involve key `k`; `file_of` finds a module path.
    fn add(
        &self,
        k: &LinkKey<'s>,
        file_of: &dyn Fn(&str) -> Option<usize>,
        out: &mut FxHashSet<usize>,
    ) {
        let key = *k;
        let find = |k: &LinkKey<'s>| self.by_key.get(k).into_iter().flatten().copied();
        match key {
            LinkKey::Global(_) => out.extend(find(&key)),
            LinkKey::Aug(t) => {
                out.extend(find(&key));
                if let Some(i) = file_of(t) {
                    out.insert(i);
                }
            }
            LinkKey::Ambient(a) => {
                out.extend(find(&key));
                for (x, v) in &self.specs {
                    if crate::levels::ambient_matches(a, x) {
                        out.extend(v.iter().copied());
                    }
                }
            }
            LinkKey::AmbSpec(x) => {
                out.extend(find(&key));
                for (a, v) in &self.ambient {
                    if crate::levels::ambient_matches(a, x) {
                        out.extend(v.iter().copied());
                        for (y, w) in &self.specs {
                            if crate::levels::ambient_matches(a, y) {
                                out.extend(w.iter().copied());
                            }
                        }
                    }
                }
            }
        }
    }
}

impl<'a, F: FileSystem> Run<'a, F> {
    fn options(&self) -> &'a CompilerOptions {
        &self.config.options
    }

    /// Resolutions of `f`'s module requests and its reference targets, as saved.
    fn edges(
        &self,
        resolver: &Resolver<&F>,
        f: &SourceFile,
    ) -> (Vec<SavedRequest>, Vec<SavedReference>) {
        let allow_js = self.options().effective().allow_js();
        let mut first = FxHashMap::default();
        let requests = crate::module_requests_with_modes(f, self.options())
            .map(|(spec, kind, mode)| {
                let res = first
                    .entry((spec.clone(), mode))
                    .or_insert_with(|| {
                        match mode {
                            Some(mode) => {
                                resolve_request_in(resolver, allow_js, &f.path, &spec, mode)
                            }
                            None => resolve_request(resolver, allow_js, &f.path, &spec, kind),
                        }
                        .map(|r| self.paths.res(&r))
                    })
                    .clone();
                SavedRequest {
                    spec: spec.into_owned(),
                    kind: kind_code(kind),
                    mode,
                    res,
                }
            })
            .collect();
        let references = f
            .references
            .iter()
            .filter(|(k, _)| *k != "lib")
            .map(|(k, v)| {
                let t =
                    reference_target(self.fs, resolver, &f.path, k, v).map(|t| self.paths.rel(&t));
                ((*k).to_owned(), v.clone(), t)
            })
            .collect();
        (requests, references)
    }

    /// Scans saved file `f`.
    fn scan(&self, resolver: &Resolver<&F>, f: &SavedFile, old: Option<Stat>) -> Scanned {
        let options = self.options();
        let path = self.paths.abs(&f.path);
        let st = stat::trusted(&path);
        let mut text = None;
        let hash = if st.is_some() && st == old {
            f.text
        } else {
            match self.fs.read_file(&path) {
                Some(t) => {
                    let h = hash_of(&t);
                    text = Some(t);
                    h
                }
                None => return Scanned::gone(),
            }
        };
        let bind_opts = bind_options_for(&path, options, Some(resolver));
        let bind = bind_key(&bind_opts);
        if hash != f.text || bind != f.bind {
            let Some(t) = text.or_else(|| self.fs.read_file(&path)) else {
                return Scanned::gone();
            };
            let hash = hash_of(&t);
            let pragma = has_pragma(&t);
            let file = SourceFile::new(path, t, false, &bind_opts);
            let (requests, references) = self.edges(resolver, &file);
            return Scanned {
                status: Status::Edited,
                hash,
                bind,
                stat: st,
                text: None,
                file: Some(file),
                pragma,
                requests: Some(requests),
                references: Some(references),
            };
        }
        // Same text: verify resolutions and references.
        let allow_js = options.effective().allow_js();
        let mut first = FxHashMap::default();
        let mut moved = false;
        let requests: Vec<SavedRequest> = f
            .requests
            .iter()
            .map(|r| {
                let kind = kind_of(r.kind).unwrap_or(RequestKind::Import);
                let res = first
                    .entry((r.spec.as_str(), r.mode))
                    .or_insert_with(|| {
                        match r.mode {
                            Some(mode) => {
                                resolve_request_in(resolver, allow_js, &path, &r.spec, mode)
                            }
                            None => resolve_request(resolver, allow_js, &path, &r.spec, kind),
                        }
                        .map(|x| self.paths.res(&x))
                    })
                    .clone();
                moved |= res != r.res;
                SavedRequest {
                    spec: r.spec.clone(),
                    kind: r.kind,
                    mode: r.mode,
                    res,
                }
            })
            .collect();
        let references: Vec<SavedReference> = f
            .references
            .iter()
            .map(|(k, v, t)| {
                let now =
                    reference_target(self.fs, resolver, &path, k, v).map(|x| self.paths.rel(&x));
                moved |= now != *t;
                (k.clone(), v.clone(), now)
            })
            .collect();
        Scanned {
            status: if moved { Status::Moved } else { Status::Same },
            hash,
            bind,
            stat: st,
            pragma: text.as_deref().map_or(f.pragma, has_pragma),
            text,
            file: None,
            requests: moved.then_some(requests),
            references: moved.then_some(references),
        }
    }

    /// Reads, parses and resolves a file that is not in the manifest.
    fn load_new(&self, resolver: &Resolver<&F>, rel: &str) -> Option<NewFile> {
        let path = self.paths.abs(rel);
        let st = stat::trusted(&path);
        let text = self.fs.read_file(&path)?;
        let hash = hash_of(&text);
        let pragma = has_pragma(&text);
        let bind_opts = bind_options_for(&path, self.options(), Some(resolver));
        let file = SourceFile::new(path, text, false, &bind_opts);
        let (requests, references) = self.edges(resolver, &file);
        Some(NewFile {
            rel: rel.to_owned(),
            file: Some(file),
            hash,
            bind: bind_key(&bind_opts),
            stat: st,
            pragma,
            requests,
            references,
        })
    }

    /// The lib files in program order (prefetched in parallel) and the missing lib names.
    /// Binding options of lib files (as `load_program` binds them).
    fn lib_bind_options(&self) -> BindOptions {
        BindOptions {
            always_strict: Some(
                self.options()
                    .effective()
                    .strict_flag(StrictFlag::AlwaysStrict),
            ),
            ..BindOptions::default()
        }
    }

    /// Parses the saved libs at `which` (indices into `names`, in order), largest first.
    fn load_libs(
        &self,
        store: &LibStore,
        names: &[(String, Hash)],
        which: &[usize],
    ) -> Option<Vec<Arc<SourceFile>>> {
        let opts = self.lib_bind_options();
        let mut by_size: Vec<usize> = which.to_vec();
        by_size.sort_by_key(|&i| {
            std::cmp::Reverse(
                std::fs::metadata(store.dir().join(&names[i].0)).map_or(0, |m| m.len()),
            )
        });
        par_map(&by_size, self.threads, |&i| {
            let _ = store.get(&names[i].0, &opts);
        });
        which
            .iter()
            .map(|&i| store.get(&names[i].0, &opts))
            .collect()
    }

    /// The fast path; `None` hands over to the full load.
    pub fn check(&self, m: &Manifest, old_stats: Sidecar, phases: &mut Phases) -> Option<Outcome> {
        let store = self.libs?;
        let options = self.options();
        let paths = self.paths;
        // Libs.
        let mut lib_stats: Vec<Option<Stat>> = Vec::with_capacity(m.libs.len());
        for (i, (name, h)) in m.libs.iter().enumerate() {
            let p = store.dir().join(name);
            let st = stat::trusted(&p.to_string_lossy());
            if st.is_some() && st == old_stats.libs.get(i).copied().flatten() {
                lib_stats.push(st);
                continue;
            }
            let text = std::fs::read(&p).ok()?;
            if blake3::hash(&text).as_bytes() != h {
                return None;
            }
            lib_stats.push(st);
        }
        // Lib files that were missing must still be (a lib that appears changes globals).
        // No libs (`noLib`): the full load reports missing core global types (TS2318).
        if !m.lib_store
            || m.libs.is_empty()
            || m.missing_libs.iter().any(|n| store.dir().join(n).is_file())
        {
            return None;
        }
        let resolver = Resolver::new(self.fs, ResolverOptions::from_compiler_options(options));
        let rel_roots: Vec<String> = self
            .config
            .file_names
            .iter()
            .map(|r| paths.rel(r))
            .collect();
        let types: Vec<(String, Option<(String, bool)>)> =
            expand_wildcard_types(self.fs, &resolver, options.effective().types())
                .into_iter()
                .map(|t| {
                    let r = resolver
                        .resolve_type_reference(&t, None, ResolutionMode::Import)
                        .map(|r| {
                            (
                                paths.rel(&r.resolved_file_name),
                                r.is_external_library_import,
                            )
                        });
                    (t, r)
                })
                .collect();
        let rel_types: Vec<(String, Option<String>)> = types
            .iter()
            .map(|(t, r)| (t.clone(), r.as_ref().map(|(p, _)| p.clone())))
            .collect();
        // Scan.
        let saved: Vec<(usize, Option<Stat>)> = (0..m.files.len())
            .map(|i| (i, old_stats.files.get(i).copied().flatten()))
            .collect();
        let mut scanned: Vec<Scanned> = par_map(&saved, self.threads, |&(i, old)| {
            self.scan(&resolver, &m.files[i], old)
        });
        phases.lap("scan");
        let saved_idx: FxHashMap<&str, usize> = m
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.as_str(), i))
            .collect();
        let new_roots = self
            .config
            .file_names
            .iter()
            .zip(&rel_roots)
            .any(|(r, rel)| !saved_idx.contains_key(rel.as_str()) && self.fs.file_exists(r));
        let all_same = scanned.iter().all(|s| s.status == Status::Same)
            && rel_roots == m.roots
            && rel_types == m.types
            && !new_roots;
        if all_same {
            let out = self.unchanged(m, &scanned, lib_stats, &old_stats);
            phases.lap("output");
            return Some(out);
        }
        // States that do not describe the saved files (a run stopped by syntax errors), or
        // no link diagnostics: the full load starts over.
        if !m.consistent || m.link.is_none() {
            return None;
        }

        // The program's files, as `load_program` would find them.
        let mut news: Vec<NewFile> = Vec::new();
        let mut new_of: FxHashMap<String, usize> = FxHashMap::default();
        let mut queue: Vec<(String, bool)> = Vec::new();
        let mut queued: FxHashSet<String> = FxHashSet::default();
        for r in &rel_roots {
            if queued.insert(r.clone()) {
                queue.push((r.clone(), false));
            }
        }
        let mut type_roots: Vec<String> = Vec::new();
        let mut missing_types: Vec<String> = Vec::new();
        for (t, r) in &types {
            match r {
                Some((p, ext)) => {
                    if queued.insert(p.clone()) {
                        queue.push((p.clone(), *ext));
                    }
                    type_roots.push(p.clone());
                }
                None if !options.effective().types().contains(t) => {}
                None => missing_types.push(t.clone()),
            }
        }
        let mut sks: Vec<Sk> = Vec::new();
        let mut loaded: FxHashMap<String, usize> = FxHashMap::default();
        let mut guard = 0;
        while !queue.is_empty() && guard < 100_000 {
            guard += 1;
            let wave = std::mem::take(&mut queue);
            let unknown: Vec<&str> = wave
                .iter()
                .map(|(p, _)| p.as_str())
                .filter(|p| !saved_idx.contains_key(p) && !new_of.contains_key(*p))
                .collect();
            let loaded_new = par_map(&unknown, self.threads, |p| self.load_new(&resolver, p));
            for (p, n) in unknown.iter().zip(loaded_new) {
                if let Some(n) = n {
                    new_of.insert((*p).to_owned(), news.len());
                    news.push(n);
                }
            }
            for (p, ext) in wave {
                let src = match saved_idx.get(p.as_str()) {
                    Some(&i) if scanned[i].status == Status::Gone => continue,
                    Some(&i) => Src::Saved(i),
                    None => match new_of.get(&p) {
                        Some(&j) => Src::New(j),
                        None => continue,
                    },
                };
                let (requests, references): (&[SavedRequest], &[SavedReference]) = match src {
                    Src::Saved(i) => (
                        scanned[i]
                            .requests
                            .as_deref()
                            .unwrap_or(&m.files[i].requests),
                        scanned[i]
                            .references
                            .as_deref()
                            .unwrap_or(&m.files[i].references),
                    ),
                    Src::New(j) => (&news[j].requests, &news[j].references),
                };
                let mut deps: Vec<String> = Vec::new();
                let mut seen = FxHashSet::default();
                for r in requests {
                    let first = seen.insert((r.spec.as_str(), r.mode));
                    let Some(res) = r.res.as_ref().filter(|r| r.included) else {
                        continue;
                    };
                    deps.push(res.path.clone());
                    if first && queued.insert(res.path.clone()) {
                        queue.push((res.path.clone(), res.external || ext));
                    }
                }
                for (_, _, t) in references {
                    if let Some(t) = t {
                        deps.push(t.clone());
                        if queued.insert(t.clone()) {
                            queue.push((t.clone(), ext));
                        }
                    }
                }
                loaded.insert(p.clone(), sks.len());
                sks.push(Sk {
                    rel: p,
                    external: ext,
                    src,
                    deps,
                });
            }
        }
        // Order: depth-first post-order from the `types` entries, then the roots.
        let mut order: Vec<usize> = Vec::with_capacity(sks.len());
        {
            let mut done: FxHashSet<usize> = FxHashSet::default();
            let mut starts: Vec<&str> = type_roots.iter().map(String::as_str).collect();
            starts.extend(rel_roots.iter().map(String::as_str));
            for s in starts {
                let Some(&root) = loaded.get(s) else {
                    continue;
                };
                let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
                while let Some((cur, i)) = stack.pop() {
                    if i == 0 && !done.insert(cur) {
                        continue;
                    }
                    let deps = &sks[cur].deps;
                    if i < deps.len() {
                        stack.push((cur, i + 1));
                        if let Some(&d) = loaded.get(deps[i].as_str())
                            && !done.contains(&d)
                        {
                            stack.push((d, 0));
                        }
                    } else {
                        order.push(cur);
                    }
                }
            }
        }
        // Per file in order: its absolute path, text hash and resolution hash.
        let state_idx: FxHashMap<&str, usize> = m
            .states
            .iter()
            .enumerate()
            .map(|(i, (p, _))| (p.as_str(), i))
            .collect();
        let n = order.len();
        let rels: Vec<&str> = order.iter().map(|&k| sks[k].rel.as_str()).collect();
        let abs: Vec<String> = rels.iter().map(|r| paths.abs(r)).collect();
        let pos_of: FxHashMap<&str, usize> =
            rels.iter().enumerate().map(|(i, r)| (*r, i)).collect();
        let mut scan_files: Vec<(Option<SourceFile>, Option<String>)> = scanned
            .iter_mut()
            .map(|s| (s.file.take(), s.text.take()))
            .collect();
        let mut new_files: Vec<(String, Option<SourceFile>)> = news
            .iter_mut()
            .map(|n| (n.rel.clone(), n.file.take()))
            .collect();
        let requests_of = |k: usize| -> &[SavedRequest] {
            match sks[k].src {
                Src::Saved(i) => scanned[i]
                    .requests
                    .as_deref()
                    .unwrap_or(&m.files[i].requests),
                Src::New(j) => &news[j].requests,
            }
        };
        let mut hashes: Vec<(Hash, Hash)> = Vec::with_capacity(n);
        for (i, &k) in order.iter().enumerate() {
            let sk = &sks[k];
            let text = match sk.src {
                Src::Saved(s) => scanned[s].hash,
                Src::New(j) => news[j].hash,
            };
            let same = match sk.src {
                Src::Saved(s) => {
                    scanned[s].status == Status::Same && m.files[s].external == sk.external
                }
                Src::New(_) => false,
            };
            let state = state_idx.get(rels[i]).map(|&s| &m.states[s].1);
            let resolved = match state {
                Some(st) if same && st.text == text => st.resolved,
                _ => {
                    let reqs = requests_of(k);
                    let res_abs: Vec<Option<(String, bool, bool, bool)>> = reqs
                        .iter()
                        .map(|r| {
                            r.res
                                .as_ref()
                                .map(|x| (paths.abs(&x.path), x.included, x.external, x.package))
                        })
                        .collect();
                    let deps_abs: Vec<String> = sk.deps.iter().map(|d| paths.abs(d)).collect();
                    resolved_hash_of(
                        sk.external,
                        reqs.iter().zip(&res_abs).map(|(r, x)| {
                            (
                                r.spec.as_str(),
                                Some(
                                    x.as_ref()
                                        .map(|(p, inc, ext, pkg)| (p.as_str(), *inc, *ext, *pkg)),
                                ),
                            )
                        }),
                        deps_abs.iter().map(String::as_str),
                    )
                }
            };
            hashes.push((text, resolved));
        }
        // Which files changed (as `History::check` decides).
        let mut fresh: Vec<usize> = Vec::new();
        let mut changed: FxHashSet<&str> = FxHashSet::default();
        for i in 0..n {
            let (t, r) = hashes[i];
            match state_idx.get(rels[i]).map(|&s| &m.states[s].1) {
                Some(s) if s.text == t && s.resolved == r => {}
                Some(s) if s.text == t => fresh.push(i),
                _ => {
                    changed.insert(rels[i]);
                    fresh.push(i);
                }
            }
        }
        let present_set: FxHashSet<&str> = rels.iter().copied().collect();
        let removed: Vec<&str> = m
            .states
            .iter()
            .map(|(p, _)| p.as_str())
            .filter(|p| !present_set.contains(p))
            .collect();
        phases.lap("layout");

        // Libs: names and order from their cached infos; parsed only for the check.
        let lib_opts = self.lib_bind_options();
        let infos = libinfo::infos(self.cache, store, &m.libs, &lib_opts, self.threads)?;
        let lib_roots = options.effective().lib_files();
        let exists = |n: &str| store.dir().join(n).is_file();
        let missing_libs = libinfo::walk(&lib_roots, &m.libs, &infos, &exists)?;
        // The saved libs are the configured closure, so the unchanged files' `/// <reference
        // lib>` add nothing. One in an edited or new file naming a lib outside it would (as
        // `load_program` adds them): the full load handles that.
        let saved_libs: FxHashSet<&str> = m.libs.iter().map(|(n, _)| n.as_str()).collect();
        let adds_lib = scan_files
            .iter()
            .filter_map(|(f, _)| f.as_ref())
            .chain(new_files.iter().filter_map(|(_, f)| f.as_ref()))
            .flat_map(|f| &f.references)
            .filter(|(k, _)| *k == "lib")
            .filter_map(|(_, v)| ts_r_config::lib_file_name(v))
            .any(|file| !saved_libs.contains(file));
        if adds_lib {
            return None;
        }
        // As `persist::manifest_of` saves them.
        let mut saved_missing = missing_libs.clone();
        saved_missing.sort();
        saved_missing.dedup();
        if program_key_from(options, m.libs.iter().map(|(n, h)| (n.as_str(), h))) != m.program_key {
            return None;
        }
        let lib_rels: Vec<String> = m.libs.iter().map(|(n, _)| format!("lib:{n}")).collect();
        let lib_paths: Vec<String> = m
            .libs
            .iter()
            .map(|(n, _)| format!("{}/{n}", store.dir().to_string_lossy().replace('\\', "/")))
            .collect();
        let lib_globals: FxHashSet<String> = infos
            .iter()
            .flat_map(|i| i.globals.iter().cloned())
            .collect();
        // Partial lib sets need a full link to find missing core types (TS2318), including
        // any supplied by project files.
        if ts_r_checker::driver::REQUIRED_GLOBAL_TYPES
            .iter()
            .any(|name| !lib_globals.contains(*name))
        {
            return None;
        }
        phases.lap("libs");

        // Parse what is still unparsed among the fresh files.
        let mut parsed: Vec<Option<Arc<SourceFile>>> = (0..n).map(|_| None).collect();
        let mut texts: Vec<Option<String>> = (0..n).map(|_| None).collect();
        for (i, &k) in order.iter().enumerate() {
            match sks[k].src {
                Src::Saved(s) => {
                    if let Some(mut f) = scan_files[s].0.take() {
                        f.is_external = sks[k].external;
                        parsed[i] = Some(Arc::new(f));
                    }
                    texts[i] = scan_files[s].1.take();
                }
                Src::New(_) => {}
            }
        }
        for (rel, file) in new_files.iter_mut() {
            if let Some(&i) = pos_of.get(rel.as_str())
                && let Some(mut f) = file.take()
            {
                f.is_external = sks[order[i]].external;
                parsed[i] = Some(Arc::new(f));
            }
        }
        let need: Vec<usize> = fresh
            .iter()
            .copied()
            .filter(|&i| parsed[i].is_none())
            .collect();
        self.parse_into(
            &need,
            &order,
            &sks,
            &abs,
            &mut parsed,
            &mut texts,
            &resolver,
        );
        // Blocking syntax errors: the full path reports them.
        for i in 0..n {
            let blocked = match &parsed[i] {
                Some(f) if fresh.contains(&i) => syntax_only(f).any(|d| blocking(&d)),
                _ => state_idx
                    .get(rels[i])
                    .is_some_and(|&s| m.states[s].1.syntax.iter().any(|d| blocking(&d.diag))),
            };
            if blocked {
                return None;
            }
        }
        let t_shapes = std::time::Instant::now();
        // Shapes of the fresh files, on a program holding just them.
        let fresh_set: FxHashSet<usize> = fresh.iter().copied().collect();
        let base_files: Vec<Arc<SourceFile>> =
            fresh.iter().filter_map(|&i| parsed[i].clone()).collect();
        let base = self.program_of(
            base_files,
            &fresh,
            &order,
            &sks,
            &abs,
            &requests_of,
            &missing_types,
            &missing_libs,
        );
        let rel = |p: &str| paths.rel(p);
        let mut new_shapes: FxHashMap<usize, FileShape> = FxHashMap::default();
        {
            let items: Vec<usize> = fresh.clone();
            let shapes = par_map(&items, self.threads, |&i| {
                parsed[i]
                    .as_ref()
                    .map(|f| shape_of(&base, f).map_paths(&rel))
            });
            for (i, s) in items.into_iter().zip(shapes) {
                new_shapes.insert(i, s?);
            }
        }
        drop(base);
        let empty = FileShape::default();
        let shape_at = |i: usize| -> &FileShape {
            new_shapes.get(&i).unwrap_or_else(|| {
                state_idx
                    .get(rels[i])
                    .map_or(&empty, |&s| &m.states[s].1.shape)
            })
        };
        // In program order (see `DeepHashes::new`).
        let list: Vec<(&str, &FileShape)> = (0..n).map(|i| (rels[i], shape_at(i))).collect();
        let shapes_ms = t_shapes.elapsed().as_secs_f64() * 1e3;
        let t_plan = std::time::Instant::now();
        let mut deep = DeepHashes::new(&list, &lib_globals);
        let lib_set: FxHashSet<&str> = lib_rels.iter().map(String::as_str).collect();
        let removed_set: FxHashSet<&str> = removed.iter().copied().collect();
        let mut targets: Vec<usize> = Vec::new();
        let mut used_now: FxHashMap<usize, Vec<Hash>> = FxHashMap::default();
        for (i, &rel) in rels.iter().enumerate() {
            if !checks(options, rel) {
                continue;
            }
            let used: Vec<Hash> = shape_at(i).uses.iter().map(|u| deep.of(u)).collect();
            let reuse = !fresh_set.contains(&i)
                && state_idx.get(rel).is_some_and(|&s| {
                    let st = &m.states[s].1;
                    st.used == used
                        && st.diags.as_ref().is_some_and(|ds| {
                            ds.iter().all(|d| {
                                d.paths().all(|p| {
                                    (present_set.contains(p) || lib_set.contains(p))
                                        && !changed.contains(p)
                                        && !removed_set.contains(p)
                                })
                            })
                        })
                });
            if !reuse {
                targets.push(i);
                if std::env::var_os("TS_R_HISTORY_DEBUG").is_some()
                    && !fresh_set.contains(&i)
                    && let Some(&s) = state_idx.get(rel)
                {
                    let st = &m.states[s].1;
                    let why: Vec<&crate::shape::Ref> = shape_at(i)
                        .uses
                        .iter()
                        .zip(&used)
                        .zip(&st.used)
                        .filter(|((_, a), b)| a != b)
                        .map(|((u, _), _)| u)
                        .collect();
                    eprintln!("lazy: re-check {rel}: uses {why:?}");
                }
            }
            used_now.insert(i, used);
        }
        // Files whose link diagnostics can change, and their merge partners.
        let providers = Partners::new((0..n).flat_map(|i| {
            link_keys(rels[i], shape_at(i))
                .into_iter()
                .map(move |k| (i, k))
        }));
        let file_of = |t: &str| pos_of.get(t).copied();
        let partners =
            |k: &LinkKey<'_>, out: &mut FxHashSet<usize>| providers.add(k, &file_of, out);
        let mut relink: FxHashSet<usize> = fresh_set.clone();
        for &i in &fresh {
            for k in link_keys(rels[i], shape_at(i)) {
                partners(&k, &mut relink);
            }
            if let Some(&s) = state_idx.get(rels[i]) {
                for k in link_keys(rels[i], &m.states[s].1.shape) {
                    partners(&k, &mut relink);
                }
            }
        }
        for r in &removed {
            if let Some(&s) = state_idx.get(r) {
                for k in link_keys(r, &m.states[s].1.shape) {
                    partners(&k, &mut relink);
                }
            }
        }
        // An augmentation merges with its module's exports, re-exports included: a fresh or
        // removed file can change the link diagnostics of augmentations of the modules that
        // re-export from it (`export { X } from`, `export *`), transitively.
        let mut reexporting: FxHashSet<&str> = fresh
            .iter()
            .map(|&i| rels[i])
            .chain(removed.iter().copied())
            .collect();
        loop {
            let mut grew = false;
            for (t, &rel) in rels.iter().enumerate() {
                if reexporting.contains(rel) {
                    continue;
                }
                let s = shape_at(t);
                let from = |x: &Ref| match x {
                    Ref::Export(p, _) | Ref::All(p) => reexporting.contains(p.as_str()),
                    _ => false,
                };
                let hit = s.stars.iter().any(|x| reexporting.contains(x.as_str()))
                    || s.provides
                        .iter()
                        .any(|p| matches!(p.what, Provides::Export(_)) && p.deps.iter().any(from));
                if hit {
                    reexporting.insert(rel);
                    grew = true;
                    if providers.augmented(rel) {
                        partners(&LinkKey::Aug(rel), &mut relink);
                    }
                }
            }
            if !grew {
                break;
            }
        }
        let mut need_files: FxHashSet<usize> = relink.clone();
        for &i in &relink {
            for k in link_keys(rels[i], shape_at(i)) {
                partners(&k, &mut need_files);
            }
        }
        // Global names the check can look up: what the targets' and re-linked files' uses
        // reach, and every global a file of the sub-program declares (merges).
        let mut names: FxHashSet<String> = FxHashSet::default();
        let mut all_globals = false;
        for &i in &targets {
            need_files.insert(i);
            let reach = deep.reach(shape_at(i).uses.iter());
            for r in reach.files {
                if let Some(&j) = pos_of.get(r) {
                    need_files.insert(j);
                }
            }
            names.extend(reach.names);
            all_globals |= reach.all_globals;
        }
        for &i in &relink {
            let reach = deep.reach(shape_at(i).uses.iter());
            // Linking resolves their re-exports too (`b` re-exports `X` through `c`).
            for r in reach.files {
                if let Some(&j) = pos_of.get(r) {
                    need_files.insert(j);
                }
            }
            names.extend(reach.names);
            all_globals |= reach.all_globals;
        }
        for &i in &need_files {
            for p in &shape_at(i).provides {
                if let Provides::Global(g) = &p.what {
                    names.insert(g.clone());
                }
            }
        }
        drop(deep);
        if std::env::var_os("TS_R_HISTORY_DEBUG").is_some() {
            let names = |v: &mut dyn Iterator<Item = &usize>| -> Vec<&str> {
                let mut x: Vec<&str> = v.map(|&i| rels[i]).collect();
                x.sort_unstable();
                x
            };
            eprintln!(
                "lazy: fresh {:?} targets {:?} relink {:?} files {:?}",
                names(&mut fresh.iter()),
                names(&mut targets.iter()),
                names(&mut relink.iter()),
                names(&mut need_files.iter())
            );
        }
        let plan_ms = t_plan.elapsed().as_secs_f64() * 1e3;
        phases.lap("plan");

        // Check the targets (and re-link) on a program of just the files they need.
        let t_check = std::time::Instant::now();
        let mut sub_idx: Vec<usize> = need_files.into_iter().collect();
        sub_idx.sort_unstable();
        let need: Vec<usize> = sub_idx
            .iter()
            .copied()
            .filter(|&i| parsed[i].is_none())
            .collect();
        self.parse_into(
            &need,
            &order,
            &sks,
            &abs,
            &mut parsed,
            &mut texts,
            &resolver,
        );
        let mut new_diags: FxHashMap<usize, Vec<PathDiag>> = FxHashMap::default();
        let mut new_link: Vec<PathDiag> = Vec::new();
        let relink_rels: FxHashSet<&str> = relink.iter().map(|&i| rels[i]).collect();
        // A stored link diagnostic in a lib file can't be tied to the project file that
        // caused it, so a relink or removal with any of them takes the full path.
        let lib_side = |d: &PathDiag| {
            std::iter::once(&d.file)
                .chain(&d.related)
                .any(|f| f.as_deref().is_some_and(|f| f.starts_with("lib:")))
        };
        if (!relink.is_empty() || !removed.is_empty()) && m.link.iter().flatten().any(lib_side) {
            return None;
        }
        // The libs: all but the DOM group, which only when the check can see it.
        let dom: Vec<usize> = (0..m.libs.len())
            .filter(|&i| DOM_GROUP.contains(&m.libs[i].0.as_str()))
            .collect();
        let dom_needed = !dom.is_empty()
            && (all_globals
                || dom
                    .iter()
                    .any(|&i| infos[i].globals.iter().any(|g| names.contains(g)))
                // A script's top-level `this` is `typeof globalThis`.
                || targets
                    .iter()
                    .any(|&i| parsed[i].as_ref().is_none_or(|f| !f.bound.is_module))
                // `import.meta` is an `ImportMeta`, which the DOM lib extends.
                || sub_idx
                    .iter()
                    .any(|&i| parsed[i].as_ref().is_none_or(|f| uses_import_meta(f))));
        let lib_idx: Vec<usize> = (0..m.libs.len())
            .filter(|i| dom_needed || !dom.contains(i))
            .collect();
        if std::env::var_os("TS_R_HISTORY_DEBUG").is_some() {
            let mut hit: Vec<&String> = dom
                .iter()
                .flat_map(|&i| infos[i].globals.iter().filter(|g| names.contains(*g)))
                .collect();
            hit.sort();
            hit.dedup();
            eprintln!("lazy: dom {dom_needed} (all globals {all_globals}; names {hit:?})");
        }
        let lib_files = if !targets.is_empty() || !relink.is_empty() {
            self.load_libs(store, &m.libs, &lib_idx)?
        } else {
            Vec::new()
        };
        let mut sub_files = lib_files.clone();
        sub_files.extend(sub_idx.iter().filter_map(|&i| parsed[i].clone()));
        let sub = self.program_of(
            sub_files,
            &sub_idx,
            &order,
            &sks,
            &abs,
            &requests_of,
            &missing_types,
            &missing_libs,
        );
        let sub_rels: Vec<&str> = lib_idx
            .iter()
            .map(|&i| lib_rels[i].as_str())
            .chain(sub_idx.iter().map(|&i| rels[i]))
            .collect();
        let to_path = |d: &Diagnostic| -> PathDiag {
            let p = |f: FileId| sub_rels.get(f.0 as usize).map(|s| (*s).to_owned());
            PathDiag {
                file: p(d.file),
                related: d.related.iter().map(|r| p(r.file)).collect(),
                diag: d.clone(),
            }
        };
        let parsed_count = lib_files.len() + parsed.iter().filter(|p| p.is_some()).count();
        if !targets.is_empty() || !relink.is_empty() {
            let checker = Checker::new(&sub);
            // As `check_program`: a checked file's, unless `@ts-ignore` or
            // `@ts-expect-error` governs its line.
            for d in &checker.link.diagnostics {
                if !link_diagnostic_reported(&sub, d) {
                    continue;
                }
                // One in a lib file (a project declaration clashing with a lib one): the
                // sub-program holds only some libs, so the full path decides.
                if sub.files.get(d.file.0 as usize).is_some_and(|f| f.is_lib) {
                    return None;
                }
                if relink_rels.contains(sub_rels[d.file.0 as usize]) {
                    new_link.push(to_path(d));
                }
            }
            let sub_pos: FxHashMap<usize, usize> = sub_idx
                .iter()
                .enumerate()
                .map(|(k, &i)| (i, lib_files.len() + k))
                .collect();
            let tids: Vec<usize> = targets.iter().map(|i| sub_pos[i]).collect();
            let results = check_files(
                &checker,
                &tids,
                CheckRun {
                    threads: self.threads,
                },
            );
            for (&i, ds) in targets.iter().zip(results) {
                new_diags.insert(i, ds.iter().map(&to_path).collect());
            }
        }
        drop(sub);
        let check_ms = t_check.elapsed().as_secs_f64() * 1e3;
        phases.lap("check");

        // New states, output and manifest.
        let mut link: Vec<PathDiag> = m
            .link
            .iter()
            .flatten()
            .filter(|d| {
                d.file.as_deref().is_some_and(|f| {
                    (present_set.contains(f) && !relink_rels.contains(f)) || f.starts_with("lib:")
                })
            })
            .cloned()
            .collect();
        link.extend(new_link);
        let mut stats = CheckStats {
            changed: fresh.len() + removed.len(),
            checked: targets.len(),
            reused: 0,
            ms: [shapes_ms, plan_ms, check_ms],
            blocked: false,
        };
        let rel_ids: Vec<&str> = lib_rels
            .iter()
            .map(String::as_str)
            .chain(rels.iter().copied())
            .collect();
        let mut states: Vec<(String, FileState)> = Vec::with_capacity(n);
        let reference_paths: FxHashSet<&str> =
            abs.iter().chain(&lib_paths).map(String::as_str).collect();
        for i in 0..n {
            let old = state_idx.get(rels[i]).map(|&s| &m.states[s].1);
            let diags = match new_diags.remove(&i) {
                Some(ds) => Some(ds),
                None if checks(options, rels[i]) => {
                    stats.reused += 1;
                    old.and_then(|s| s.diags.clone())
                }
                None => None,
            };
            let syntax = match (&parsed[i], fresh_set.contains(&i)) {
                // As `check_program` reports them: related infos keep their file ids.
                (Some(f), true) => syntax_only(f)
                    .map(|d| PathDiag {
                        related: d
                            .related
                            .iter()
                            .map(|r| rel_ids.get(r.file.0 as usize).map(|s| (*s).to_owned()))
                            .collect(),
                        diag: d,
                        file: Some(rels[i].to_owned()),
                    })
                    .collect(),
                _ => old.map(|s| s.syntax.clone()).unwrap_or_default(),
            };
            let references = match (&parsed[i], fresh_set.contains(&i)) {
                (Some(f), true) => {
                    // `/// <reference types>` the loader would not find: no saved target.
                    let current: &[SavedReference] = match sks[order[i]].src {
                        Src::Saved(s) => scanned[s]
                            .references
                            .as_deref()
                            .unwrap_or(&m.files[s].references),
                        Src::New(j) => &news[j].references,
                    };
                    let missing: Vec<&str> = if options.no_resolve() == Some(true) {
                        Vec::new()
                    } else {
                        current
                            .iter()
                            .filter(|(k, _, t)| k.starts_with("types") && t.is_none())
                            .map(|(_, v, _)| v.as_str())
                            .collect()
                    };
                    ts_r_checker::reference_diagnostics(f, options, &reference_paths)
                        .into_iter()
                        .chain(ts_r_checker::type_reference_diagnostics(f, &missing))
                        .map(|diag| PathDiag {
                            diag,
                            file: Some(rels[i].to_owned()),
                            related: Vec::new(),
                        })
                        .collect()
                }
                _ => old.map(|s| s.references.clone()).unwrap_or_default(),
            };
            states.push((
                rels[i].to_owned(),
                FileState {
                    text: hashes[i].0,
                    resolved: hashes[i].1,
                    shape: shape_at(i).clone(),
                    used: used_now.remove(&i).unwrap_or_default(),
                    diags,
                    syntax,
                    references,
                },
            ));
        }
        // Ids: lib files first, then the program's files in order.
        let mut index: FxHashMap<&str, u32> = FxHashMap::default();
        for (i, r) in lib_rels.iter().enumerate() {
            index.insert(r.as_str(), i as u32);
        }
        let nl = lib_paths.len();
        for (i, r) in rels.iter().enumerate() {
            index.insert(r, (nl + i) as u32);
        }
        let mut out: Vec<Diagnostic> = global_diagnostics_of(&missing_types);
        for (_, st) in &states {
            for d in st
                .syntax
                .iter()
                .chain(&st.references)
                .chain(st.diags.iter().flatten())
            {
                if let Some(d) = d.to_diag(&index) {
                    out.push(d);
                }
            }
        }
        for d in &link {
            if let Some(d) = d.to_diag(&index) {
                out.push(d);
            }
        }
        let id_paths: Vec<&str> = lib_paths
            .iter()
            .map(String::as_str)
            .chain(abs.iter().map(String::as_str))
            .collect();
        let out = sort_and_dedup(&id_paths, out);
        let output: Vec<PathDiag> = out
            .iter()
            .map(|d| {
                let p = |f: FileId| rel_ids.get(f.0 as usize).map(|s| (*s).to_owned());
                PathDiag {
                    file: p(d.file),
                    related: d.related.iter().map(|r| p(r.file)).collect(),
                    diag: d.clone(),
                }
            })
            .collect();
        // Sources: text for the files diagnostics point into.
        let mut shown: FxHashSet<usize> = FxHashSet::default();
        let mut shown_libs: FxHashSet<usize> = FxHashSet::default();
        for d in &out {
            for f in std::iter::once(d.file).chain(d.related.iter().map(|r| r.file)) {
                let f = f.0 as usize;
                if f < nl {
                    shown_libs.insert(f);
                } else if f < nl + n {
                    shown.insert(f - nl);
                }
            }
        }
        let mut sources: Vec<Option<(String, String)>> = lib_sources(&lib_paths, &shown_libs);
        for i in 0..n {
            let text = if shown.contains(&i) {
                match (&parsed[i], &texts[i]) {
                    (Some(f), _) => f.text.clone(),
                    (None, Some(t)) => t.clone(),
                    (None, None) => self.fs.read_file(&abs[i]).unwrap_or_default(),
                }
            } else {
                String::new()
            };
            sources.push(Some((abs[i].clone(), text)));
        }
        // The manifest and its fingerprints.
        let mut files: Vec<SavedFile> = Vec::with_capacity(n);
        let mut file_stats: Vec<Option<Stat>> = Vec::with_capacity(n);
        let mut any_pragma = false;
        for (i, &k) in order.iter().enumerate() {
            let sk = &sks[k];
            let (requests, references) = match sk.src {
                Src::Saved(s) => (
                    scanned[s]
                        .requests
                        .take()
                        .unwrap_or_else(|| m.files[s].requests.clone()),
                    scanned[s]
                        .references
                        .take()
                        .unwrap_or_else(|| m.files[s].references.clone()),
                ),
                Src::New(j) => (
                    std::mem::take(&mut news[j].requests),
                    std::mem::take(&mut news[j].references),
                ),
            };
            let (bind, st, pragma) = match sk.src {
                Src::Saved(s) => (scanned[s].bind, scanned[s].stat, scanned[s].pragma),
                Src::New(j) => (news[j].bind, news[j].stat, news[j].pragma),
            };
            any_pragma |= pragma;
            files.push(SavedFile {
                path: rels[i].to_owned(),
                text: hashes[i].0,
                bind,
                external: sk.external,
                pragma,
                requests,
                references,
            });
            file_stats.push(st);
        }
        let files_out: Vec<(String, bool)> = order
            .iter()
            .enumerate()
            .map(|(i, &k)| (abs[i].clone(), sks[k].external))
            .collect();
        let manifest = Manifest {
            program_key: m.program_key,
            consistent: true,
            libs: m.libs.clone(),
            missing_libs: saved_missing,
            lib_store: true,
            roots: rel_roots,
            types: rel_types,
            files,
            output,
            states,
            link: Some(link),
        };
        let bytes = encode(&manifest);
        let digest = *blake3::hash(&bytes).as_bytes();
        let _ = self.cache.put(self.key, &bytes);
        let side = Sidecar {
            libs: lib_stats,
            files: file_stats,
        };
        let _ = self
            .cache
            .put(&sidecar_key(self.key), &side.encode(&digest));
        phases.lap("save");
        Some(Outcome {
            sources,
            diagnostics: out,
            mode: Mode::Incremental,
            stats,
            phases: Vec::new(),
            files: files_out,
            parsed: parsed_count,
            pragma: any_pragma,
        })
    }

    /// Parses files `need` (positions in `order`) into `parsed`.
    #[allow(clippy::too_many_arguments)]
    fn parse_into(
        &self,
        need: &[usize],
        order: &[usize],
        sks: &[Sk],
        abs: &[String],
        parsed: &mut [Option<Arc<SourceFile>>],
        texts: &mut [Option<String>],
        resolver: &Resolver<&F>,
    ) {
        let items: Vec<(usize, Option<String>)> =
            need.iter().map(|&i| (i, texts[i].take())).collect();
        let files = par_map(&items, self.threads, |(i, text)| {
            let path = &abs[*i];
            let text = text.clone().or_else(|| self.fs.read_file(path))?;
            let opts = bind_options_for(path, self.options(), Some(resolver));
            let mut f = SourceFile::new(path.clone(), text, false, &opts);
            f.is_external = sks[order[*i]].external;
            Some(Arc::new(f))
        });
        for ((i, _), f) in items.iter().zip(files) {
            parsed[*i] = f;
        }
    }

    /// A program of `files` (libs first) with the resolutions and edges of the non-lib
    /// files at positions `which`.
    #[allow(clippy::too_many_arguments)]
    fn program_of<'r>(
        &self,
        files: Vec<Arc<SourceFile>>,
        which: &[usize],
        order: &[usize],
        sks: &[Sk],
        abs: &[String],
        requests_of: &dyn Fn(usize) -> &'r [SavedRequest],
        missing_types: &[String],
        missing_libs: &[String],
    ) -> ProgramFiles {
        let mut resolutions: FxHashMap<(String, String), Option<Resolution>> = FxHashMap::default();
        let mut mode_resolutions = FxHashMap::default();
        let mut deps: FxHashMap<String, Vec<String>> = FxHashMap::default();
        for &i in which {
            let k = order[i];
            for r in requests_of(k) {
                let resolution = r.res.as_ref().map(|x| Resolution {
                    resolved: self.paths.abs(&x.path),
                    extension: x.ext,
                    external: x.external,
                    included: x.included,
                    package: x.package,
                    package_name: None,
                    alternate: None,
                    reference_source: None,
                    ts_extension: written_ts_extension_found(&r.spec, &x.path, x.ext),
                    arbitrary_extension: !x.included
                        && !self.options().effective().allow_arbitrary_extensions()
                        && ts_r_checker::program::needs_arbitrary_extensions(
                            &self.paths.abs(&x.path),
                            &abs[i],
                        ),
                });
                if let Some(mode) = r.mode {
                    mode_resolutions
                        .entry((abs[i].clone(), r.spec.clone(), mode))
                        .or_insert(resolution);
                } else {
                    resolutions
                        .entry((abs[i].clone(), r.spec.clone()))
                        .or_insert(resolution);
                }
            }
            deps.insert(
                abs[i].clone(),
                sks[k].deps.iter().map(|d| self.paths.abs(d)).collect(),
            );
        }
        ProgramFiles {
            files,
            resolutions,
            mode_resolutions,
            options: self.options().clone(),
            roots: self.config.file_names.clone(),
            missing_libs: missing_libs.to_vec(),
            missing_types: missing_types.to_vec(),
            missing_type_references: Vec::new(),
            ambiguous_roots: Vec::new(),
            deps,
        }
    }

    /// The stored output (nothing changed). Refreshes the fingerprints when files had to be
    /// read.
    fn unchanged(
        &self,
        m: &Manifest,
        scanned: &[Scanned],
        lib_stats: Vec<Option<Stat>>,
        old: &Sidecar,
    ) -> Outcome {
        let paths = self.paths;
        // Lib ids in program order, as the manifest keeps them (`load_program` puts the lib
        // files first, in its lib walk's order).
        let lib_paths: Vec<String> = m
            .libs
            .iter()
            .map(|(n, _)| paths.abs(&format!("lib:{n}")))
            .collect();
        let mut index: FxHashMap<&str, u32> = FxHashMap::default();
        let lib_rels: Vec<String> = m.libs.iter().map(|(n, _)| format!("lib:{n}")).collect();
        for (i, r) in lib_rels.iter().enumerate() {
            index.insert(r.as_str(), i as u32);
        }
        let nl = lib_paths.len();
        for (i, f) in m.files.iter().enumerate() {
            index.insert(f.path.as_str(), (nl + i) as u32);
        }
        let diagnostics: Vec<Diagnostic> =
            m.output.iter().filter_map(|d| d.to_diag(&index)).collect();
        let mut shown: FxHashSet<usize> = FxHashSet::default();
        let mut shown_libs: FxHashSet<usize> = FxHashSet::default();
        for d in &diagnostics {
            for f in std::iter::once(d.file).chain(d.related.iter().map(|r| r.file)) {
                let f = f.0 as usize;
                if f < nl {
                    shown_libs.insert(f);
                } else if f < nl + m.files.len() {
                    shown.insert(f - nl);
                }
            }
        }
        let mut sources: Vec<Option<(String, String)>> = lib_sources(&lib_paths, &shown_libs);
        let mut files = Vec::with_capacity(m.files.len());
        for (i, (f, s)) in m.files.iter().zip(scanned).enumerate() {
            let abs = paths.abs(&f.path);
            let text = if shown.contains(&i) {
                s.text
                    .clone()
                    .or_else(|| self.fs.read_file(&abs))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            files.push((abs.clone(), f.external));
            sources.push(Some((abs, text)));
        }
        let side = Sidecar {
            libs: lib_stats,
            files: scanned.iter().map(|s| s.stat).collect(),
        };
        if side != *old {
            let _ = self
                .cache
                .put(&sidecar_key(self.key), &side.encode(&self.digest));
        }
        Outcome {
            sources,
            diagnostics,
            mode: Mode::Unchanged,
            stats: CheckStats {
                reused: m.states.iter().filter(|(_, s)| s.diags.is_some()).count(),
                ..CheckStats::default()
            },
            phases: Vec::new(),
            files,
            parsed: 0,
            pragma: scanned.iter().any(|s| s.pragma),
        }
    }
}

/// Whether `f` contains `import.meta`.
/// Sources for the lib files: text for the ones diagnostics point into (`shown`, by id),
/// `None` for the rest.
fn lib_sources(lib_paths: &[String], shown: &FxHashSet<usize>) -> Vec<Option<(String, String)>> {
    lib_paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            shown
                .contains(&i)
                .then(|| (p.clone(), std::fs::read_to_string(p).unwrap_or_default()))
        })
        .collect()
}

fn uses_import_meta(f: &SourceFile) -> bool {
    f.text.contains("meta")
        && (1..=f.ast.node_count() as u32)
            .filter_map(ts_r_ast::NodeId::from_raw)
            .any(|n| {
                matches!(
                    f.ast.node(n),
                    ts_r_ast::Node::MetaProperty {
                        kind: ts_r_ast::MetaPropKind::ImportMeta
                    }
                )
            })
}

/// Unresolved `types` entries, as `check_program` reports them.
fn global_diagnostics_of(missing_types: &[String]) -> Vec<Diagnostic> {
    let p = ProgramFiles {
        files: Vec::new(),
        resolutions: FxHashMap::default(),
        mode_resolutions: FxHashMap::default(),
        options: CompilerOptions::default(),
        roots: Vec::new(),
        missing_libs: Vec::new(),
        missing_types: missing_types.to_vec(),
        missing_type_references: Vec::new(),
        ambiguous_roots: Vec::new(),
        deps: FxHashMap::default(),
    };
    global_diagnostics(&p)
}

/// `ts_r_checker::sort_and_dedup` with file paths by id.
fn sort_and_dedup(paths: &[&str], mut diags: Vec<Diagnostic>) -> Vec<Diagnostic> {
    let path_of = |d: &Diagnostic| -> &str { paths.get(d.file.0 as usize).copied().unwrap_or("") };
    diags.sort_by(|a, b| {
        path_of(a)
            .cmp(path_of(b))
            .then(a.span.start.cmp(&b.span.start))
            .then(a.span.len().cmp(&b.span.len()))
            .then(a.code.cmp(&b.code))
            .then(a.message.cmp(&b.message))
    });
    diags.dedup_by(|a, b| {
        a.file == b.file && a.span == b.span && a.code == b.code && a.message == b.message
    });
    diags
}

/// Whether `spec`'s own TypeScript extension found `path` (the resolver's
/// `resolved_using_ts_extension`, which the saved history does not keep): the file has a
/// TypeScript extension and the same stem as the specifier's last segment, so directory
/// `index` results do not count.
fn written_ts_extension_found(spec: &str, path: &str, ext: ts_r_resolver::Extension) -> bool {
    let stem = |s: &str| {
        let name = s.rsplit('/').next().unwrap_or(s);
        ts_r_resolver::Extension::of(name)
            .filter(|e| e.is_ts_like())
            .map(|e| name[..name.len().saturating_sub(e.as_str().len())].to_owned())
    };
    ext.is_ts_like() && stem(spec).is_some_and(|a| stem(path).is_some_and(|b| a == b))
}
