//! The module graph of a run: every TypeScript/JSX file reachable from the entry through
//! literal specifiers, compiled (through the cache) and with its specifiers resolved.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rustc_hash::FxHashSet;
use ts_r_cache::{Cache, KeyBuilder};
use ts_r_config::RealFs;

use crate::compile::{self, Compiled, Format, PackageType};
use crate::project::Project;

/// Bump when the cache payload or anything that shapes it changes.
pub const CACHE_FORMAT: u32 = 2;

/// Compiles files for one project, through the transpile cache.
#[derive(Debug)]
pub struct Engine {
    pub project: Project,
    /// Expands `parse<T>` call sites (validators inlined per file).
    guard: ts_r_guard::Expander,
    /// The file `[sound.guard]` was read from, an input of every run.
    guard_config: Option<String>,
    cache: Option<Cache>,
    pub source_maps: bool,
    /// Tool build + options, mixed into every cache key.
    salt: String,
}

/// A file read and compiled, with the `package.json` that decided its package type.
#[derive(Debug)]
pub struct Unit {
    pub compiled: Compiled,
    pub package_json: Option<String>,
    /// Files `parse<T>` validators were generated from (inputs of the run).
    pub guard_deps: Vec<String>,
}

impl Engine {
    pub fn new(project: Project, use_cache: bool, source_maps: bool) -> Engine {
        let cache = if use_cache {
            Cache::open("run", CACHE_FORMAT).ok()
        } else {
            None
        };
        // `[sound.guard]` from ts-r.toml or the tsconfig's "ts-r" key, read once per run.
        let tsconfig = project
            .config_files
            .first()
            .cloned()
            .unwrap_or_else(|| crate::project::absolute("tsconfig.json"));
        let settings = ts_r_sound::config::guard_options(&RealFs::new(), &tsconfig);
        for e in &settings.errors {
            eprintln!("{}", e.render());
        }
        let guard_opts = settings.options;
        let salt = format!(
            "{}|{}|{guard_opts:?}",
            build_fingerprint(),
            project.base.cache_key()
        );
        let guard = ts_r_guard::Expander::with_options(
            Arc::new(RealFs::new()),
            ts_r_guard::ExpandOptions {
                guard: guard_opts,
                layout: ts_r_guard::Layout::Inline,
            },
            project.options.clone(),
        );
        Engine {
            guard_config: settings.source,
            guard,
            project,
            cache,
            source_maps,
            salt,
        }
    }

    /// Reads and compiles `path`. Errors are ready to print.
    pub fn compile_file(&self, path: &str) -> Result<Unit, String> {
        let src = std::fs::read_to_string(path)
            .map_err(|e| format!("ts-r-run: cannot read {path}: {e}\n"))?;
        let (pkg, package_json) = self.project.package_type(path);
        // `parse<T>` call sites: the expanded text is what gets compiled and cached, so the
        // cache key covers the validators (and so the declarations they come from).
        let mut guard_deps = Vec::new();
        let src = match self.guard.expand(path, &src) {
            Some(x) => {
                if !x.errors.is_empty() {
                    let mut out = String::new();
                    for e in &x.errors {
                        out.push_str(&e.render(path));
                        out.push('\n');
                    }
                    return Err(out);
                }
                guard_deps = x.deps;
                x.code
            }
            None => src,
        };
        let compiled = self.compile_source(path, &src, pkg)?;
        Ok(Unit {
            compiled,
            package_json,
            guard_deps,
        })
    }

    pub fn compile_source(
        &self,
        path: &str,
        src: &str,
        pkg: PackageType,
    ) -> Result<Compiled, String> {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let (options, plain_jsx) = self.project.options_for(path);
        let key = KeyBuilder::new("ts-r-run/compile")
            .str(&self.salt)
            .u64(u64::from(plain_jsx))
            .str(name)
            .str(match pkg {
                PackageType::Module => "module",
                PackageType::CommonJs => "commonjs",
                PackageType::Unspecified => "",
            })
            .u64(u64::from(self.source_maps))
            .bytes(src.as_bytes())
            .key();
        if let Some(cache) = &self.cache
            && let Some(blob) = cache.get(&key)
            && let Some(c) = compile::decode(&blob)
        {
            return Ok(c);
        }
        let c = compile::compile(path, src, options, pkg, self.source_maps)?;
        if let Some(cache) = &self.cache {
            let _ = cache.put(&key, &compile::encode(&c));
        }
        Ok(c)
    }

    /// Writes pending cache entries.
    pub fn flush(&self) {
        if let Some(c) = &self.cache {
            let _ = c.flush();
        }
    }
}

/// Identifies this build of ts-r-run (size and mtime of the executable), so a rebuilt tool
/// never reuses output from an older one even when the version string is unchanged.
pub fn build_fingerprint() -> String {
    let meta = std::env::current_exe().and_then(std::fs::metadata);
    match meta {
        Ok(m) => {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{}+{}-{}", env!("CARGO_PKG_VERSION"), m.len(), mtime)
        }
        Err(_) => env!("CARGO_PKG_VERSION").to_owned(),
    }
}

/// One compiled module of the graph.
#[derive(Debug)]
pub struct Module {
    pub path: String,
    pub format: Format,
    pub code: String,
    /// `(specifier, resolved file)` for the specifiers ts-r-run resolves itself.
    pub resolved: Vec<(String, Arc<str>)>,
    /// The resolved files loaded as soon as this module runs (not only by an `import()` or
    /// a `require()` inside a function).
    pub eager: Vec<Arc<str>>,
}

#[derive(Debug, Default)]
pub struct Graph {
    /// Entry first, then breadth-first order.
    pub modules: Vec<Module>,
    /// Files whose change invalidates the graph: sources, `package.json`s, tsconfigs.
    pub inputs: BTreeSet<String>,
    /// Rendered errors (syntax errors, unreadable files) in the modules that load with the
    /// entry. Non-empty means don't run.
    pub errors: Vec<String>,
    /// `(file, rendered errors)` for modules only reached by an `import()` or a `require()`
    /// inside a function: the program runs, and loading one of them throws, as under tsx.
    pub deferred: Vec<(String, String)>,
}

type Processed = (Module, Option<String>, Vec<String>);

fn process(engine: &Engine, path: &str) -> Result<Processed, String> {
    let unit = engine.compile_file(path)?;
    let c = unit.compiled;
    let mut resolved = Vec::new();
    let mut eager = Vec::new();
    for s in &c.specs {
        // A query or hash on a path (`./config.ts?v=1`, `@/config.ts?v=1` under `paths`)
        // names the file without it; the hooks put it back on the URL.
        let file = match s.text.find(['?', '#']) {
            Some(cut) if cut > 0 && !s.text.contains(':') => &s.text[..cut],
            _ => s.text.as_str(),
        };
        if let Some(t) = engine
            .project
            .resolve(file, path, s.require || c.format == Format::Cjs)
        {
            if !s.lazy {
                eager.push(t.clone());
            }
            resolved.push((s.text.clone(), t));
        }
    }
    Ok((
        Module {
            path: path.to_owned(),
            format: c.format,
            code: c.code,
            resolved,
            eager,
        },
        unit.package_json,
        unit.guard_deps,
    ))
}

/// Runs `f` over `items` on up to `threads` threads; results keep the input order.
fn parallel_map<T: Sync, R: Send>(
    items: &[T],
    threads: usize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let threads = threads.clamp(1, items.len().max(1));
    if threads == 1 {
        return items.iter().map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let mut parts: Vec<Vec<(usize, R)>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(i) else { break };
                        out.push((i, f(item)));
                    }
                    out
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });
    let mut all: Vec<(usize, R)> = parts.iter_mut().flat_map(std::mem::take).collect();
    all.sort_by_key(|(i, _)| *i);
    all.into_iter().map(|(_, r)| r).collect()
}

/// Worker threads for compiling: `TS_R_RUN_THREADS`, else the core count (at most 8).
pub fn default_threads() -> usize {
    std::env::var("TS_R_RUN_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(4, |n| n.get())
                .min(8)
        })
        .max(1)
}

/// Builds the graph from `entry` (absolute, `/`-separated).
pub fn build(engine: &Engine, entry: &str, threads: usize) -> Graph {
    let mut g = Graph::default();
    g.inputs.extend(engine.project.config_files.iter().cloned());
    // `ts-r.toml` when `[sound.guard]` came from it (an edit must miss the stamp).
    g.inputs.extend(engine.guard_config.iter().cloned());
    if !engine.project.config_errors.is_empty() {
        g.errors.push(engine.project.config_errors.clone());
        return g;
    }
    let mut seen: FxHashSet<String> = FxHashSet::default();
    seen.insert(entry.to_owned());
    let mut frontier = vec![entry.to_owned()];
    let mut failed: Vec<(String, String)> = Vec::new();
    while !frontier.is_empty() {
        let results = parallel_map(&frontier, threads, |p| process(engine, p));
        let mut next = Vec::new();
        for (path, r) in frontier.iter().zip(results) {
            g.inputs.insert(path.clone());
            match r {
                Ok((m, pj, deps)) => {
                    if let Some(pj) = pj {
                        g.inputs.insert(pj);
                    }
                    g.inputs.extend(deps);
                    for (_, t) in &m.resolved {
                        if compile::is_compiled_path(t) && seen.insert(t.to_string()) {
                            next.push(t.to_string());
                        }
                    }
                    g.modules.push(m);
                }
                Err(e) => failed.push((path.clone(), e)),
            }
        }
        frontier = next;
    }
    engine.flush();
    if !failed.is_empty() {
        let eager = eager_closure(&g.modules, entry);
        for (path, e) in failed {
            if eager.contains(path.as_str()) {
                g.errors.push(e);
            } else {
                g.deferred.push((path, e));
            }
        }
    }
    g
}

/// The files that load with `entry`, following eager edges only.
fn eager_closure<'a>(modules: &'a [Module], entry: &'a str) -> FxHashSet<&'a str> {
    let by_path: rustc_hash::FxHashMap<&str, &Module> =
        modules.iter().map(|m| (m.path.as_str(), m)).collect();
    let mut seen: FxHashSet<&str> = FxHashSet::default();
    seen.insert(entry);
    let mut stack = vec![entry];
    while let Some(p) = stack.pop() {
        if let Some(m) = by_path.get(p) {
            for t in &m.eager {
                if seen.insert(t) {
                    stack.push(t);
                }
            }
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_map_keeps_order() {
        let items: Vec<u32> = (0..100).collect();
        let out = parallel_map(&items, 4, |x| x * 2);
        assert_eq!(out, items.iter().map(|x| x * 2).collect::<Vec<_>>());
        assert!(parallel_map(&Vec::<u32>::new(), 4, |x| *x).is_empty());
    }
}
