//! The build driver: graph, link, plan, print (cached), assemble, write. [`BuildContext`]
//! keeps modules and printed fragments between builds for fast rebuilds, and can use the
//! persistent `ts_r_cache` (namespace `build`) across processes.

use std::hash::Hasher;
use std::sync::{Arc, Mutex};

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_config::{FileSystem, path};

use crate::chunk::{ChunkOutput, assemble};
use crate::graph::{
    Graph, GraphContext, ModuleCache, ModuleId, Target, build_graph, pretty_path, split_with_type,
};
use crate::link::{Kind, LinkOptions, link};
use crate::meta::RecordKind;
use crate::options::{BuildOptions, Format, Loader};
use crate::plan::{ModulePlan, PlanInput, plan};

/// `--keep-names` without bundling: the helper's name in each output file (esbuild's).
const KEEP_NAMES_HELPER: &str = "__name";
use crate::prepare::{Asset, Defines, PrepareContext, Prepared};
use crate::print::{Fragment, print_module};
use crate::resolve::{Resolution, ResolveKind, Resolver};
use crate::{Message, Severity, pool};

/// Absolute entry paths and, for each, its output name if one was given.
type Entries = (Vec<String>, Vec<Option<String>>);

/// A file produced by a build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputFile {
    /// Absolute path.
    pub path: String,
    pub contents: Vec<u8>,
}

impl OutputFile {
    pub fn text(&self) -> &str {
        std::str::from_utf8(&self.contents).unwrap_or("")
    }
}

/// Counters for one build (what was reused).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BuildStats {
    pub modules: usize,
    /// Modules parsed in this build.
    pub parsed: usize,
    pub fragments_printed: usize,
    pub fragments_reused: usize,
}

#[derive(Debug, Default)]
pub struct BuildResult {
    pub outputs: Vec<OutputFile>,
    pub messages: Vec<Message>,
    /// esbuild-format metafile JSON (when requested).
    pub metafile: Option<String>,
    pub stats: BuildStats,
    /// Absolute paths of every file the build read (modules), for watch mode. Also filled
    /// when the build failed after the graph was built.
    pub inputs: Vec<String>,
}

impl BuildResult {
    pub fn has_errors(&self) -> bool {
        self.messages.iter().any(Message::is_error)
    }

    pub fn errors(&self) -> impl Iterator<Item = &Message> {
        self.messages.iter().filter(|m| m.is_error())
    }

    fn fail(messages: Vec<Message>) -> BuildResult {
        BuildResult {
            messages,
            ..BuildResult::default()
        }
    }
}

/// Persistent-cache format version (bump when meta, fragments or plans change shape).
const CACHE_VERSION: u32 = 8;

/// A build that can be repeated cheaply (watch mode, the dev server, benchmarks).
pub struct BuildContext {
    opts: Arc<BuildOptions>,
    prep: Arc<PrepareContext>,
    modules: ModuleCache,
    fragments: Mutex<FxHashMap<[u8; 32], Arc<Fragment>>>,
    disk: Option<ts_r_cache::Cache>,
    threads: usize,
    fs_cache: Arc<crate::fscache::CachedFs>,
    /// Set by [`BuildContext::invalidate`]: the files that changed since the last build.
    /// Cached modules of every other file are reused without reading them again. `None`
    /// (fresh context, or after a structural change) re-reads and re-hashes every file.
    trusted: Mutex<Option<FxHashSet<String>>>,
}

impl std::fmt::Debug for BuildContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuildContext").finish_non_exhaustive()
    }
}

/// One-shot build on the real file system.
pub fn build(opts: BuildOptions) -> BuildResult {
    match BuildContext::new(opts, Arc::new(ts_r_config::RealFs::new())) {
        Ok(ctx) => ctx.build(),
        Err(m) => BuildResult::fail(m),
    }
}

/// `std::hash::Hasher` over blake3 (stable across runs, unlike `DefaultHasher` seeds).
struct Blake(blake3::Hasher);

impl Hasher for Blake {
    fn finish(&self) -> u64 {
        let b = self.0.finalize();
        let mut a = [0u8; 8];
        a.copy_from_slice(&b.as_bytes()[..8]);
        u64::from_le_bytes(a)
    }
    fn write(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
}

fn fragment_key(module_key: &[u8; 32], plan: &ModulePlan) -> [u8; 32] {
    use std::hash::Hash;
    let mut h = Blake(blake3::Hasher::new());
    h.0.update(&CACHE_VERSION.to_le_bytes());
    h.0.update(module_key);
    plan.hash(&mut h);
    *h.0.finalize().as_bytes()
}

impl BuildContext {
    /// Ends a one-shot build in a process about to exit: flushes the persistent cache and
    /// leaks the in-memory caches instead of freeing them one allocation at a time (about
    /// 10 ms for three.js x10). Watch mode and the dev server keep their context instead.
    pub fn finish_and_leak(mut self) {
        drop(self.disk.take());
        std::mem::forget(self);
    }

    pub fn new(opts: BuildOptions, fs: Arc<dyn FileSystem>) -> Result<BuildContext, Vec<Message>> {
        let defines = Defines::from_options(&opts)
            .map_err(|e| vec![Message::new(Severity::Error, None, e)])?;
        let mut opts = opts;
        // Symlinked working directories (macOS `/var` -> `/private/var`) would otherwise give
        // modules reached through a realpath a different prefix than the entries.
        let real = fs.realpath(&opts.working_dir);
        if !real.is_empty() && fs.directory_exists(&real) {
            opts.working_dir = path::normalize(&real);
        }
        let opts = Arc::new(opts);
        let fs_cache = Arc::new(crate::fscache::CachedFs::new(fs));
        let fs: Arc<dyn FileSystem> = Arc::clone(&fs_cache) as Arc<dyn FileSystem>;
        let resolver = Arc::new(Resolver::new(fs, &opts));
        let prep = Arc::new(PrepareContext::new(Arc::clone(&opts), defines, resolver));
        let disk = if opts.persistent_cache {
            ts_r_cache::Cache::open("build", CACHE_VERSION).ok()
        } else {
            None
        };
        let threads = pool::thread_count(opts.threads);
        Ok(BuildContext {
            opts,
            prep,
            modules: ModuleCache::default(),
            fragments: Mutex::default(),
            disk,
            threads,
            fs_cache,
            trusted: Mutex::default(),
        })
    }

    pub fn options(&self) -> &BuildOptions {
        &self.opts
    }

    pub fn resolver(&self) -> &Resolver {
        &self.prep.resolver
    }

    /// Tells the context that files changed on disk (absolute paths, or relative to the
    /// working directory). The next build re-reads only these files. Resolution caches and
    /// the tsconfig-derived transform options are dropped when files were added or removed,
    /// or a file that is not a module (`package.json`, a tsconfig or one it extends) changed.
    pub fn invalidate(&self, changed: &[String]) {
        let wd = &self.opts.working_dir;
        let changed: Vec<String> = changed
            .iter()
            .map(|p| {
                let abs = path::resolve(wd, p);
                self.fs_cache.forget(path::directory_of(&abs));
                // Module paths are real paths; symlinked spellings must match them.
                let dir = path::directory_of(&abs);
                let real_dir = self.fs_cache.realpath(dir);
                let name = path::file_name(&abs);
                if real_dir.is_empty() || real_dir == dir {
                    abs
                } else {
                    self.fs_cache.forget(&real_dir);
                    format!("{}/{name}", real_dir.trim_end_matches('/'))
                }
            })
            .collect();
        let known = self
            .modules
            .modules
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let structural = changed.iter().any(|p| {
            p.ends_with("package.json")
                || p.ends_with("tsconfig.json")
                || !known.contains_key(p)
                || !self.fs_cache.file_exists(p)
        });
        drop(known);
        let mut trusted = self.trusted.lock().unwrap_or_else(|e| e.into_inner());
        if structural {
            self.fs_cache.clear();
            self.prep.resolver.clear_caches();
            // Transform options are cached per tsconfig path, and modules' preparation keys
            // hash those options: a stale entry would keep stale modules.
            self.prep.clear_options();
            *trusted = None;
        } else {
            trusted
                .get_or_insert_with(FxHashSet::default)
                .extend(changed);
        }
    }

    /// Absolute entry paths and their output names (`name=path`). An entry point given
    /// twice with the same name is built once, as in esbuild.
    fn resolve_entries(&self) -> Result<Entries, Vec<Message>> {
        let wd = &self.opts.working_dir;
        let mut out = Vec::new();
        let mut names: Vec<Option<String>> = Vec::new();
        let mut seen = FxHashSet::default();
        let mut errs = Vec::new();
        for e in &self.opts.entry_points {
            let abs = path::resolve(wd, &e.path);
            let fs = self.prep.resolver.fs();
            if e.path.contains('*') && !fs.file_exists(&abs) {
                // A glob entry: every matching file, in sorted order.
                let found = match glob_entries(&**fs, &abs) {
                    Ok(found) => found,
                    Err(text) => {
                        errs.push(Message::new(
                            Severity::Error,
                            None,
                            format!("Entry point glob {:?} {text}", e.path),
                        ));
                        continue;
                    }
                };
                if found.is_empty() {
                    errs.push(Message::new(
                        Severity::Error,
                        None,
                        format!("Could not resolve {:?}", e.path),
                    ));
                }
                for f in found {
                    let f = fs.realpath(&f);
                    if seen.insert((f.clone(), None)) {
                        out.push(f);
                        names.push(None);
                    }
                }
                continue;
            }
            let resolved = if fs.file_exists(&abs) {
                fs.realpath(&abs)
            } else {
                // Like esbuild: a path that is not an existing file resolves as an import
                // path (so `pkg` is a package and `./entry` probes extensions).
                match self.prep.resolver.resolve(&e.path, wd, ResolveKind::Import) {
                    Resolution::File(f) => f,
                    _ => {
                        errs.push(Message::new(
                            Severity::Error,
                            None,
                            format!("Could not resolve {:?}", e.path),
                        ));
                        continue;
                    }
                }
            };
            if !seen.insert((resolved.clone(), e.name.clone())) {
                continue;
            }
            out.push(resolved);
            names.push(e.name.clone());
        }
        if errs.is_empty() {
            Ok((out, names))
        } else {
            Err(errs)
        }
    }

    /// Runs a build.
    pub fn build(&self) -> BuildResult {
        let opts = &*self.opts;
        if let Err(e) = opts.validate() {
            return BuildResult::fail(vec![Message::new(Severity::Error, None, e)]);
        }
        if let Some(t) = &opts.tsconfig {
            let full = path::resolve(&opts.working_dir, t);
            if !self.prep.resolver.fs().file_exists(&full) {
                let text = format!(
                    "Cannot find tsconfig file {:?}",
                    pretty_path(&opts.working_dir, &full)
                );
                return BuildResult::fail(vec![Message::new(Severity::Error, None, text)]);
            }
        }
        let (entries, entry_names) = match self.resolve_entries() {
            Ok(e) => e,
            Err(m) => return BuildResult::fail(m),
        };
        let (outdir, outfile_name) = self.output_location(&entries);
        if !opts.bundle {
            return self.transform_only(&entries, &entry_names, &outdir, outfile_name.as_deref());
        }
        let trusted = self
            .trusted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let gctx = GraphContext {
            prep: &self.prep,
            cache: &self.modules,
            disk: self.disk.as_ref(),
            threads: self.threads,
            changed: trusted.as_ref(),
            parsed: Default::default(),
            read_ahead: (trusted.is_none() && self.threads > 1).then_some(&*self.fs_cache),
        };
        let mut timer = Timer::new();
        let graph = build_graph(&gctx, &entries);
        timer.lap("graph");
        let inputs: Vec<String> = graph
            .modules
            .iter()
            .filter(|m| path::is_rooted(&m.path))
            .map(|m| m.path.clone())
            .collect();
        let fail = |m: Vec<Message>| BuildResult {
            inputs: inputs.clone(),
            ..BuildResult::fail(m)
        };
        let mut messages = graph.messages.clone();
        for mut m in self.prep.resolver.package_json_errors() {
            m.file = m.file.map(|f| pretty_path(&self.opts.working_dir, &f));
            messages.push(m);
        }
        let mut stats = BuildStats {
            modules: graph.modules.len(),
            parsed: gctx.parsed.load(std::sync::atomic::Ordering::Relaxed),
            ..BuildStats::default()
        };
        if messages.iter().any(Message::is_error) {
            return fail(sorted(messages));
        }
        let format = opts.effective_format();
        let mut dynamic_entries: Vec<ModuleId> = Vec::new();
        if opts.splitting {
            for m in &graph.modules {
                for (i, r) in m.meta.records.iter().enumerate() {
                    if r.kind == RecordKind::Dynamic
                        && let Target::Module(t) = m.targets[i]
                        && !graph.entries.contains(&t)
                        && !dynamic_entries.contains(&t)
                    {
                        dynamic_entries.push(t);
                    }
                }
            }
        }
        timer.lap("pre-link");
        let lk = link(
            &graph,
            &graph.entries,
            &dynamic_entries,
            LinkOptions {
                format,
                tree_shaking: opts.tree_shaking(),
                splitting: opts.splitting,
                keep_entry_exports: format != Format::Iife || opts.global_name.is_some(),
            },
        );
        messages.extend(lk.messages.iter().cloned());
        if format != Format::Esm {
            for (i, m) in graph.modules.iter().enumerate() {
                if lk.included[i] && m.meta.has_tla && lk.kinds[i] == Kind::Esm {
                    messages.push(Message::new(
                        Severity::Error,
                        Some(m.pretty.clone()),
                        format!(
                            "Top-level await is currently not supported with the {:?} output format",
                            format.as_str()
                        ),
                    ));
                }
            }
        }
        if format == Format::Esm
            && opts
                .unsupported()
                .has(crate::features::Feature::ArbitraryModuleNamespaceNames)
        {
            messages.extend(self.string_name_errors(&graph, &lk.included));
        }
        messages.extend(crate::css::link::check(&graph, &lk));
        if messages.iter().any(Message::is_error) {
            return fail(sorted(messages));
        }
        // Output names.
        let outbase = common_dir(&entries);
        let entry_files: Vec<String> = match &outfile_name {
            Some(f) => vec![f.clone()],
            None => entries
                .iter()
                .enumerate()
                .map(|(i, e)| self.entry_file_name(e, &outbase, entry_names[i].as_deref()))
                .collect(),
        };
        // Each entry's stylesheet: the entry template with `[ext]` = `css` (a CSS entry with
        // `--outfile` writes to that file as named, as esbuild does).
        let entry_css_files: Vec<String> = match &outfile_name {
            Some(f) => {
                let css_entry = graph
                    .entries
                    .first()
                    .is_some_and(|&e| graph.modules[e as usize].loader.is_css());
                vec![if css_entry {
                    f.clone()
                } else {
                    with_css_extension(f)
                }]
            }
            None => entries
                .iter()
                .enumerate()
                .map(|(i, e)| self.entry_name(e, &outbase, entry_names[i].as_deref(), "css"))
                .collect(),
        };
        let dynamic_files: Vec<String> = dynamic_entries
            .iter()
            .map(|&t| self.dynamic_file_name(&graph.modules[t as usize]))
            .collect();
        timer.lap("link");
        let mut pl = plan(&PlanInput {
            graph: &graph,
            link: &lk,
            opts,
            entries: &graph.entries,
            dynamic_entries: &dynamic_entries,
            outdir: outdir.clone(),
            entry_files,
            dynamic_files,
        });
        timer.lap("plan");
        // Print fragments.
        let todo: Vec<ModuleId> = (0..graph.modules.len() as ModuleId)
            .filter(|&m| pl.plans[m as usize].is_some())
            .collect();
        let printed = Mutex::new(0usize);
        let used_keys: Mutex<Vec<[u8; 32]>> = Mutex::default();
        let frags: Vec<(ModuleId, Option<Arc<Fragment>>)> = pool::map(self.threads, todo, |m| {
            let module = &graph.modules[m as usize];
            let Some(mp) = &pl.plans[m as usize] else {
                return (m, None);
            };
            let key = fragment_key(&module.prep_key, mp);
            used_keys
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(key);
            if let Some(f) = self
                .fragments
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&key)
            {
                return (m, Some(Arc::clone(f)));
            }
            let disk_key = ts_r_cache::Key::of(&key);
            if let Some(disk) = &self.disk
                && let Some(blob) = disk.get(&disk_key)
                && let Some(f) = Fragment::decode(&blob.to_vec())
            {
                let f = Arc::new(f);
                self.fragments
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(key, Arc::clone(&f));
                return (m, Some(f));
            }
            let prepared: Arc<Prepared> = match &module.prepared {
                Some(p) => Arc::clone(p),
                None => Arc::new(crate::prepare::prepare(
                    &self.prep,
                    &module.path,
                    &module.pretty,
                    Arc::clone(&module.source.js),
                    module.loader,
                )),
            };
            let nodes: Vec<u32> = module.meta.records.iter().map(|r| r.node).collect();
            let f = Arc::new(print_module(&prepared, mp, &nodes));
            *printed.lock().unwrap_or_else(|e| e.into_inner()) += 1;
            if let Some(disk) = &self.disk {
                let _ = disk.put(&disk_key, &f.encode());
            }
            self.fragments
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, Arc::clone(&f));
            (m, Some(f))
        });
        stats.fragments_printed = printed.into_inner().unwrap_or(0);
        stats.fragments_reused = frags.len() - stats.fragments_printed;
        {
            let used: FxHashSet<[u8; 32]> = used_keys
                .into_inner()
                .unwrap_or_default()
                .into_iter()
                .collect();
            self.fragments
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|k, _| used.contains(k));
        }
        let mut by_module: Vec<Option<Arc<Fragment>>> = vec![None; graph.modules.len()];
        for (m, f) in frags {
            by_module[m as usize] = f;
        }
        timer.lap("print");
        // Assemble.
        let mut chunk_outs: Vec<ChunkOutput> = (0..pl.chunks.len())
            .map(|ci| assemble(ci, &pl, &graph, &by_module, opts, self.threads, &outbase))
            .collect();
        let mut outputs = Vec::new();
        // Assets from the file loader, with paths relative to outdir.
        let mut assets: Vec<(&Asset, String)> = Vec::new();
        for (i, m) in graph.modules.iter().enumerate() {
            // A `copy` file imported from JavaScript is copied though no chunk has it.
            let copied = m.loader == Loader::Copy && !graph.entries.contains(&(i as ModuleId));
            if (lk.included[i] || copied)
                && let Some(a) = &m.source.asset
            {
                assets.push((a, asset_path(&a.rel_path, &m.path, &outbase)));
            }
        }
        // Final names: `[hash]` from the contents; a CSS entry's chunk is its stylesheet.
        for (ci, co) in chunk_outs.iter().enumerate() {
            let c = &pl.chunks[ci];
            let css_entry = c
                .entry
                .is_some_and(|e| graph.modules[e as usize].loader.is_css());
            let name = match c.user_entry {
                Some(i) if css_entry => fill_hash(
                    &entry_css_files[i],
                    co.css.as_deref().unwrap_or("").as_bytes(),
                ),
                _ => fill_hash(&c.file, co.code.as_bytes()),
            };
            pl.chunks[ci].file = name;
        }
        // The outputs take the chunks' text (the metafile only needs `module_bytes`).
        for (ci, co) in chunk_outs.iter_mut().enumerate() {
            let file = format!("{outdir}/{}", pl.chunks[ci].file);
            let css_file = match pl.chunks[ci].user_entry {
                Some(i) => format!("{outdir}/{}", entry_css_files[i]),
                None => with_css_extension(&file),
            };
            // A CSS entry point produces only its stylesheet.
            let css_entry = pl.chunks[ci]
                .entry
                .is_some_and(|e| graph.modules[e as usize].loader.is_css());
            // A `copy` entry point is its file, copied under the entry's name.
            if let Some(e) = pl.chunks[ci].entry
                && pl.chunks[ci].user_entry.is_some()
                && graph.modules[e as usize].loader == Loader::Copy
            {
                let m = &graph.modules[e as usize];
                if let Some(a) = &m.source.asset {
                    let stem = file
                        .strip_suffix(opts.out_extension_js.as_str())
                        .unwrap_or(&file);
                    let name = path::file_name(crate::graph::split_query(&m.path).0);
                    let ext = name
                        .rsplit_once('.')
                        .map(|(_, e)| format!(".{e}"))
                        .unwrap_or_default();
                    outputs.push(OutputFile {
                        path: format!("{stem}{ext}"),
                        contents: a.bytes.to_vec(),
                    });
                }
                continue;
            }
            if css_entry {
                let css = co.css.take().unwrap_or_default();
                outputs.push(OutputFile {
                    path: file,
                    contents: css.into_bytes(),
                });
                continue;
            }
            if let Some(map) = co.map.take() {
                outputs.push(OutputFile {
                    path: format!("{file}.map"),
                    contents: map.into_bytes(),
                });
            }
            outputs.push(OutputFile {
                path: file.clone(),
                contents: std::mem::take(&mut co.code).into_bytes(),
            });
            if let Some(css) = &co.css {
                let path = fill_hash(&css_file, css.as_bytes());
                // Entries with the same stylesheet under a `[hash]` name share one file.
                if !outputs
                    .iter()
                    .any(|o: &OutputFile| o.path == path && o.contents == css.as_bytes())
                {
                    outputs.push(OutputFile {
                        path,
                        contents: css.as_bytes().to_vec(),
                    });
                }
            }
        }
        // Assets from the file loader.
        let mut seen_assets = FxHashSet::default();
        for (a, rel) in assets {
            if seen_assets.insert(rel.clone()) {
                outputs.push(OutputFile {
                    path: format!("{outdir}/{rel}"),
                    contents: a.bytes.to_vec(),
                });
            }
        }
        timer.lap("assemble");
        let metafile = opts
            .metafile
            .then(|| crate::metafile::render(&graph, &lk, &pl, &chunk_outs, &outputs, opts));
        if opts.write {
            messages.extend(write_outputs(&outputs, self.threads));
        }
        timer.lap("write");
        timer.report(&stats);
        BuildResult {
            outputs,
            messages: sorted(messages),
            metafile,
            stats,
            inputs,
        }
    }

    /// Output directory and, with `outfile`, the output file name.
    fn output_location(&self, entries: &[String]) -> (String, Option<String>) {
        let wd = &self.opts.working_dir;
        if let Some(f) = &self.opts.outfile {
            let abs = path::resolve(wd, f);
            let dir = path::directory_of(&abs).to_owned();
            return (dir, Some(path::file_name(&abs).to_owned()));
        }
        if let Some(d) = &self.opts.outdir {
            return (path::resolve(wd, d), None);
        }
        let _ = entries;
        (wd.clone(), None)
    }

    fn entry_file_name(&self, entry: &str, outbase: &str, name: Option<&str>) -> String {
        self.entry_name(entry, outbase, name, "js")
    }

    /// An entry's output path from `--entry-names`: `[ext]` is `js` or `css` (`ext`), and
    /// `[hash]` stays until the contents are known ([`fill_hash`]).
    fn entry_name(&self, entry: &str, outbase: &str, name: Option<&str>, ext: &str) -> String {
        let dir = path::directory_of(entry);
        let mut rel_dir = path::relative(outbase, dir, true);
        if rel_dir == "." {
            rel_dir.clear();
        }
        let rel_dir = dotdot_dirs(&rel_dir);
        let file = path::file_name(entry);
        let stem = name
            .map(str::to_owned)
            .unwrap_or_else(|| match file.rsplit_once('.') {
                Some((s, _)) if !s.is_empty() => s.to_owned(),
                _ => file.to_owned(),
            });
        let mut n = self.opts.entry_names.clone();
        if rel_dir.is_empty() {
            n = n.replace("[dir]/", "").replace("[dir]", "");
        } else {
            n = n.replace("[dir]", &rel_dir);
        }
        n = n.replace("[name]", &stem).replace("[ext]", ext);
        let n = n
            .trim_start_matches("./")
            .trim_start_matches('/')
            .to_owned();
        if ext == "css" {
            format!("{n}.css")
        } else {
            format!("{n}{}", self.opts.out_extension_js)
        }
    }

    /// The file of a module that only `import()` makes an entry: esbuild names it with the
    /// chunk template (`[name]-[hash]`, `[name]` the file's stem, no `[dir]`), observed with
    /// esbuild 0.28.2 (`import('./sub/c')` gives `c-HASH.js`). The hash covers the module's
    /// preparation key and path, so two `index.js` files in different folders differ.
    fn dynamic_file_name(&self, m: &crate::graph::Module) -> String {
        let file = path::file_name(&m.path);
        let stem = match file.rsplit_once('.') {
            Some((s, _)) if !s.is_empty() => s,
            _ => file,
        };
        let mut h = blake3::Hasher::new();
        h.update(&m.prep_key);
        h.update(m.pretty.as_bytes());
        let hash = crate::hash_text(h.finalize().as_bytes());
        let n = self
            .opts
            .chunk_names
            .replace("[dir]/", "")
            .replace("[dir]", "")
            .replace("[name]", stem)
            .replace("[hash]", &hash);
        let n = n.trim_start_matches("./").trim_start_matches('/');
        format!("{n}{}", self.opts.out_extension_js)
    }

    /// Without `--bundle`: each entry is transformed (and minified) on its own.
    /// String module names that reach ESM output (`lowering::string_names`): the entries'
    /// exports and what they `export *`, and imports of external modules.
    fn string_name_errors(&self, graph: &Graph, included: &[bool]) -> Vec<Message> {
        let mut exported = vec![false; graph.modules.len()];
        let mut stack: Vec<ModuleId> = graph.entries.clone();
        while let Some(m) = stack.pop() {
            if std::mem::replace(&mut exported[m as usize], true) {
                continue;
            }
            let module = &graph.modules[m as usize];
            for &r in &module.meta.star_exports {
                if let Some(Target::Module(t)) = module.targets.get(r as usize) {
                    stack.push(*t);
                }
            }
        }
        let mut out = Vec::new();
        for (i, module) in graph.modules.iter().enumerate() {
            let external = |spec: &str| {
                module.meta.records.iter().enumerate().any(|(r, rec)| {
                    rec.spec == spec && matches!(module.targets.get(r), Some(Target::External(_)))
                })
            };
            let has_external = module
                .targets
                .iter()
                .any(|t| matches!(t, Target::External(_)));
            if !included[i]
                || !matches!(
                    module.loader,
                    Loader::Js | Loader::Jsx | Loader::Ts | Loader::Tsx
                )
                || !(exported[i] || has_external)
            {
                continue;
            }
            let prepared: Arc<Prepared> = match &module.prepared {
                Some(p) => Arc::clone(p),
                None => Arc::new(crate::prepare::prepare(
                    &self.prep,
                    &module.path,
                    &module.pretty,
                    Arc::clone(&module.source.js),
                    module.loader,
                )),
            };
            for (at, text) in crate::lowering::string_names(&prepared.ast, exported[i], &external) {
                out.push(module.message_at(Severity::Error, at, text));
            }
        }
        out
    }

    fn transform_only(
        &self,
        entries: &[String],
        names: &[Option<String>],
        outdir: &str,
        outfile: Option<&str>,
    ) -> BuildResult {
        let opts = &*self.opts;
        let outbase = common_dir(entries);
        let wd = opts.working_dir.clone();
        let work: Vec<(usize, String)> = entries.iter().cloned().enumerate().collect();
        let results = pool::map(self.threads, work, |(i, e)| {
            let fs = self.prep.resolver.fs();
            let loader = opts.loader_for(&e);
            let source = match crate::prepare::load(fs.as_ref(), &e, loader, opts) {
                Ok(s) => s,
                Err(m) => return Err(vec![Message::new(Severity::Error, None, m)]),
            };
            let pretty = pretty_path(&wd, &e);
            if let Some(sheet) = &source.css {
                // A CSS entry is its own `.css` output, printed as written.
                let css =
                    crate::css::print_sheet(sheet, opts.minify_whitespace, opts.minify_syntax);
                let file = match outfile {
                    Some(f) => f.to_owned(),
                    None => {
                        let js = self.entry_file_name(&e, &outbase, names[i].as_deref());
                        let stem = js
                            .strip_suffix(opts.out_extension_js.as_str())
                            .unwrap_or(&js);
                        format!("{stem}.css")
                    }
                };
                let full = format!("{outdir}/{file}");
                let meta = (pretty, css.len(), full.clone(), css.len());
                let out = OutputFile {
                    path: full,
                    contents: css.as_bytes().to_vec(),
                };
                return Ok((vec![out], Vec::new(), meta));
            }
            if loader == Loader::Copy
                && let Some(a) = &source.asset
            {
                // A `copy` entry is copied under its entry name (observed in esbuild).
                let file = match outfile {
                    Some(f) => f.to_owned(),
                    None => {
                        let js = self.entry_file_name(&e, &outbase, names[i].as_deref());
                        let stem = js
                            .strip_suffix(opts.out_extension_js.as_str())
                            .unwrap_or(&js);
                        let name = path::file_name(&e);
                        let ext = name
                            .rsplit_once('.')
                            .map(|(_, x)| format!(".{x}"))
                            .unwrap_or_default();
                        format!("{stem}{ext}")
                    }
                };
                let full = format!("{outdir}/{file}");
                let n = a.bytes.len();
                let meta = (pretty, n, full.clone(), n);
                let out = OutputFile {
                    path: full,
                    contents: a.bytes.to_vec(),
                };
                return Ok((vec![out], Vec::new(), meta));
            }
            let format = opts.effective_format();
            let module = match (opts.format, format) {
                (Some(_), Format::Cjs | Format::Iife) => {
                    Some(ts_r_transform::ModuleFormat::CommonJs)
                }
                _ => None,
            };
            let prepared = crate::prepare::prepare_with(
                &self.prep,
                &e,
                &pretty,
                Arc::clone(&source.js),
                loader,
                module,
            );
            if prepared.messages.iter().any(Message::is_error) {
                return Err(prepared.messages.clone());
            }
            if module.is_none()
                && opts
                    .unsupported()
                    .has(crate::features::Feature::ArbitraryModuleNamespaceNames)
            {
                let lines = crate::LineIndex::new(&source.js);
                let errs: Vec<Message> =
                    crate::lowering::string_names(&prepared.ast, true, &|_| true)
                        .into_iter()
                        .map(|(at, text)| lines.message(Severity::Error, &pretty, at, text))
                        .collect();
                if !errs.is_empty() {
                    return Err(errs);
                }
            }
            if module.is_some() && crate::scan::scan(&prepared, false).has_tla {
                return Err(vec![Message::new(
                    Severity::Error,
                    Some(pretty.clone()),
                    format!(
                        "Top-level await is currently not supported with the \"{}\" output format",
                        format.as_str()
                    ),
                )]);
            }
            let file = match outfile {
                Some(f) => f.to_owned(),
                None => self.entry_file_name(&e, &outbase, names[i].as_deref()),
            };
            let mut ast = prepared.ast.clone();
            let unsupported = opts.unsupported();
            if unsupported.has(crate::features::Feature::ImportMeta) {
                let kind = if unsupported.has(crate::features::Feature::ConstAndLet) {
                    ts_r_ast::VarKind::Var
                } else {
                    ts_r_ast::VarKind::Const
                };
                crate::print::empty_import_meta(&mut ast, kind);
            }
            let mut keep_names_used = false;
            if opts.minify_syntax || opts.minify_identifiers {
                let module = prepared.strict;
                keep_names_used = ts_r_minify::minify_ast_keep_names(
                    &mut ast,
                    &prepared.src,
                    &ts_r_minify::MinifyOptions {
                        whitespace: opts.minify_whitespace,
                        identifiers: opts.minify_identifiers,
                        syntax: opts.minify_syntax,
                        module,
                    },
                    module,
                    opts.keep_names.then_some(KEEP_NAMES_HELPER),
                );
            }
            // `--keep-names` without bundling: the helper is defined at the top of the file.
            let prelude = if keep_names_used {
                let mut h = crate::runtime::Helpers::with_names(
                    crate::runtime::Helper::ALL.map(|h| h.name().to_owned()),
                );
                h.names[crate::runtime::Helper::Name as usize] = KEEP_NAMES_HELPER.to_owned();
                h.mark(crate::runtime::Helper::Name);
                let mut p = h.render(opts.minify_whitespace && opts.minify_identifiers);
                p.push_str(&prepared.prelude);
                p
            } else {
                prepared.prelude.clone()
            };
            let cg = ts_r_codegen::CodegenOptions {
                layout: if opts.minify_whitespace {
                    ts_r_codegen::Layout::Compact
                } else {
                    ts_r_codegen::Layout::Readable
                },
                comments: !opts.minify_whitespace,
                source_map: (opts.sourcemap != crate::options::SourceMapMode::None).then(|| {
                    ts_r_codegen::SourceMapOptions {
                        file: Some(path::file_name(&file).to_owned()),
                        source: path::relative(
                            path::directory_of(&format!("{outdir}/{file}")),
                            &e,
                            true,
                        ),
                        source_root: None,
                        include_content: opts.sources_content,
                    }
                }),
                ..ts_r_codegen::CodegenOptions::default()
            };
            let out = ts_r_codegen::print_with(
                &ast,
                &prepared.src,
                &cg,
                &ts_r_codegen::PrintHints {
                    erased: &prepared.erased,
                    prelude: &prelude,
                    ..Default::default()
                },
            );
            let mut code = out.code;
            if opts.format == Some(Format::Iife) {
                code = if opts.minify_whitespace {
                    format!("(()=>{{{code}}})();\n")
                } else {
                    format!("(() => {{\n{code}}})();\n")
                };
            }
            let mut files = Vec::new();
            let full = format!("{outdir}/{file}");
            if let Some(map) = out.map {
                use crate::options::SourceMapMode::*;
                let json = map.to_json();
                match opts.sourcemap {
                    Linked => code.push_str(&format!(
                        "//# sourceMappingURL={}.map\n",
                        path::file_name(&file)
                    )),
                    Inline | Both => {
                        code.push_str(&format!("//# sourceMappingURL={}\n", map.to_data_url()))
                    }
                    _ => {}
                }
                if matches!(opts.sourcemap, Linked | External | Both) {
                    files.push(OutputFile {
                        path: format!("{full}.map"),
                        contents: json.into_bytes(),
                    });
                }
            }
            let meta = (pretty.clone(), source.js.len(), full.clone(), code.len());
            files.push(OutputFile {
                path: full,
                contents: code.into_bytes(),
            });
            Ok((files, prepared.messages.clone(), meta))
        });
        let mut outputs = Vec::new();
        let mut messages = Vec::new();
        let mut metas = Vec::new();
        for r in results {
            match r {
                Ok((f, m, meta)) => {
                    outputs.extend(f);
                    messages.extend(m);
                    metas.push(meta);
                }
                Err(m) => messages.extend(m),
            }
        }
        if messages.iter().any(Message::is_error) {
            return BuildResult::fail(sorted(messages));
        }
        if opts.write {
            messages.extend(write_outputs(&outputs, self.threads));
        }
        BuildResult {
            outputs,
            messages: sorted(messages),
            metafile: opts
                .metafile
                .then(|| crate::metafile::render_transform(&metas, &opts.working_dir)),
            stats: BuildStats::default(),
            inputs: entries.to_vec(),
        }
    }
}

fn sorted(mut m: Vec<Message>) -> Vec<Message> {
    m.sort_by(|a, b| {
        (a.severity, &a.file, a.line, a.column, &a.text)
            .cmp(&(b.severity, &b.file, b.line, b.column, &b.text))
    });
    m.dedup();
    m
}

/// Lowest common ancestor directory of the entry files.
/// A file-loader asset's output path relative to the output directory: `template` (the
/// asset names with all but `[dir]` filled in) with `[dir]` the directory of `module`
/// relative to `outbase`, `..` written `_.._` as for entry names.
pub(crate) fn asset_path(template: &str, module: &str, outbase: &str) -> String {
    let file = crate::graph::split_query(split_with_type(module).0).0;
    let mut dir = path::relative(outbase, path::directory_of(file), true);
    if dir == "." {
        dir.clear();
    }
    let dir = dotdot_dirs(&dir);
    let n = if dir.is_empty() {
        template.replace("[dir]/", "").replace("[dir]", "")
    } else {
        template.replace("[dir]", &dir)
    };
    n.trim_start_matches("./")
        .trim_start_matches('/')
        .to_owned()
}

/// `[dir]` text for a relative directory: each `..` component is written `_.._` (esbuild's
/// spelling, so output stays inside the output directory).
fn dotdot_dirs(dir: &str) -> String {
    dir.split('/')
        .map(|c| if c == ".." { "_.._" } else { c })
        .collect::<Vec<_>>()
        .join("/")
}

/// The URL of asset `rel` (relative to `outdir`) from output file `file`: under
/// `public_path` when set, else relative to the file's directory (`./a.png`, `../a.png`).
pub(crate) fn asset_url(outdir: &str, file: &str, rel: &str, public_path: &str) -> String {
    if !public_path.is_empty() {
        return format!("{}/{rel}", public_path.trim_end_matches('/'));
    }
    let r = path::relative(path::directory_of(file), &format!("{outdir}/{rel}"), true);
    if r.starts_with("../") {
        r
    } else {
        format!("./{r}")
    }
}

fn common_dir(entries: &[String]) -> String {
    let mut it = entries.iter();
    let Some(first) = it.next() else {
        return "/".into();
    };
    let mut dir: Vec<&str> = path::directory_of(first).split('/').collect();
    for e in it {
        let d: Vec<&str> = path::directory_of(e).split('/').collect();
        let n = dir.iter().zip(&d).take_while(|(a, b)| a == b).count();
        dir.truncate(n);
    }
    let s = dir.join("/");
    if s.is_empty() { "/".into() } else { s }
}

/// `file` with its extension replaced by `.css`.
fn with_css_extension(file: &str) -> String {
    let name_start = file.rfind('/').map_or(0, |i| i + 1);
    match file[name_start..].rfind('.') {
        Some(i) if i > 0 => format!("{}.css", &file[..name_start + i]),
        _ => format!("{file}.css"),
    }
}

/// An output path with `[hash]` (from `--entry-names`) replaced by a hash of its contents.
fn fill_hash(path: &str, contents: &[u8]) -> String {
    if !path.contains("[hash]") {
        return path.to_owned();
    }
    path.replace(
        "[hash]",
        &crate::hash_text(blake3::hash(contents).as_bytes()),
    )
}

/// Writes the outputs in parallel (a bundle and its source map are large); one error
/// message per file that could not be written, in output order.
fn write_outputs(outputs: &[OutputFile], threads: usize) -> Vec<Message> {
    let mut paths = FxHashSet::default();
    let mut messages = Vec::new();
    for o in outputs {
        if !paths.insert(&o.path) {
            messages.push(Message::new(
                Severity::Error,
                None,
                format!("Two output files share the same path: {}", o.path),
            ));
        }
    }
    if !messages.is_empty() {
        return messages;
    }
    let work: Vec<usize> = (0..outputs.len()).collect();
    pool::map(threads, work, |i| {
        write_file(&outputs[i].path, &outputs[i].contents)
    })
    .into_iter()
    .zip(outputs)
    .filter_map(|(r, o)| {
        let e = r.err()?;
        Some(Message::new(
            Severity::Error,
            None,
            format!("Failed to write to output file {}: {e}", o.path),
        ))
    })
    .collect()
}

fn write_file(p: &str, contents: &[u8]) -> std::io::Result<()> {
    let dir = path::directory_of(p);
    if !dir.is_empty() {
        std::fs::create_dir_all(dir)?;
    }
    if same_contents(p, contents) {
        return Ok(());
    }
    std::fs::write(p, contents)
}

/// Whether file `p` already holds `contents` (then it is left alone, keeping its mtime for
/// watchers). Compares the size first and then 1 MB at a time, without reading the old file
/// into one buffer.
fn same_contents(p: &str, contents: &[u8]) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(p) else {
        return false;
    };
    if f.metadata().map(|m| m.len()).ok() != Some(contents.len() as u64) {
        return false;
    }
    let mut buf = vec![0u8; (1 << 20).min(contents.len().max(1))];
    let mut at = 0;
    loop {
        let n = match f.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        };
        if n == 0 {
            return at == contents.len();
        }
        if contents.get(at..at + n) != Some(&buf[..n]) {
            return false;
        }
        at += n;
    }
}

/// Loader of a module (for callers that inspect the graph).
pub fn is_js_loader(l: Loader) -> bool {
    l.is_js()
}

/// Phase timings on stderr when `TS_R_BUILD_TIMING` is set.
struct Timer {
    on: bool,
    last: std::time::Instant,
    laps: Vec<(&'static str, std::time::Duration)>,
}

impl Timer {
    fn new() -> Timer {
        Timer {
            on: std::env::var_os("TS_R_BUILD_TIMING").is_some(),
            last: std::time::Instant::now(),
            laps: Vec::new(),
        }
    }

    fn lap(&mut self, name: &'static str) {
        if self.on {
            let now = std::time::Instant::now();
            self.laps.push((name, now - self.last));
            self.last = now;
        }
    }

    fn report(&self, stats: &BuildStats) {
        if self.on {
            let parts: Vec<String> = self
                .laps
                .iter()
                .map(|(n, d)| format!("{n} {:.1}ms", d.as_secs_f64() * 1000.0))
                .collect();
            eprintln!("[timing] {} | {stats:?}", parts.join(", "));
        }
    }
}

/// Files matching an absolute entry-point glob (`*` within a name, `**/` any directories),
/// sorted. The walk starts at the directory before the first wildcard and is bounded in
/// depth and size.
fn glob_entries(fs: &dyn FileSystem, pattern: &str) -> Result<Vec<String>, String> {
    const MAX_DEPTH: usize = 64;
    const MAX_FILES: usize = 100_000;
    let Some(star) = pattern.find('*') else {
        return Ok(Vec::new());
    };
    let Some(slash) = pattern[..star].rfind('/') else {
        return Ok(Vec::new());
    };
    let base = if slash == 0 { "/" } else { &pattern[..slash] };
    let rest = &pattern[slash + 1..];
    let mut found = Vec::new();
    let mut seen = 0usize;
    let mut stack: Vec<(String, String, usize)> = vec![(base.to_owned(), String::new(), 0)];
    while let Some((dir, rel, depth)) = stack.pop() {
        for entry in fs.read_dir(&dir) {
            seen += 1;
            if seen > MAX_FILES {
                return Err(format!(
                    "exceeds the traversal limit of {MAX_FILES} entries"
                ));
            }
            let child_rel = if rel.is_empty() {
                entry.name.clone()
            } else {
                format!("{rel}/{}", entry.name)
            };
            let child = path::combine(&dir, &entry.name);
            match entry.kind {
                ts_r_config::EntryKind::File => {
                    if crate::resolve::glob(rest, &child_rel) {
                        found.push(child);
                    }
                }
                ts_r_config::EntryKind::Directory if glob_directory(rest, &child_rel) => {
                    if depth >= MAX_DEPTH {
                        return Err(format!("exceeds the traversal depth limit of {MAX_DEPTH}"));
                    }
                    stack.push((child, child_rel, depth + 1));
                }
                ts_r_config::EntryKind::Directory => {}
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Whether any continuation of a directory can match the pattern. A directory prefix
/// must end at a pattern slash or within a `**` (which can consume slashes).
fn glob_directory(pattern: &str, dir: &str) -> bool {
    let prefix = format!("{dir}/");
    let bytes = pattern.as_bytes();
    bytes.iter().enumerate().any(|(i, &b)| {
        let end = if b == b'/' {
            i + 1
        } else if b == b'*' && bytes.get(i + 1) == Some(&b'*') {
            i + 2
        } else {
            return false;
        };
        crate::resolve::glob(&pattern[..end], &prefix)
    })
}
