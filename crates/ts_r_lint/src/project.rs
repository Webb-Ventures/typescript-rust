//! Linting a set of paths: file discovery (`.gitignore`, config ignores, nearest config per
//! file), config loading, and the parallel runner.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use rustc_hash::FxHashMap;

use crate::config::biome::{self, BIOME_CONFIG_NAMES};
use crate::config::eslint_eval::{ESLINT_CONFIG_NAMES, NodeOptions, evaluate_all};
use crate::config::glob::Glob;
use crate::config::mapping::UnsupportedReason;
use crate::config::oxlint::{self, OXLINT_CONFIG_NAMES};
use crate::config::{
    ConfigFile, ConfigKind, NotLinted, ResolveOptions, default_config, objects_from_eslint_json,
};
use crate::engine::{FileConfig, FileResult, lint_and_fix, lint_source};
use crate::output::{FileReport, file_warning};
use crate::typed::driver::{DriverOptions, TypedOutcome, TypedRequest, lint_projects};
use crate::types::{FileKey, NoTypeInfo};

/// Extensions considered when walking directories.
pub const LINTABLE_EXTENSIONS: &[&str] = &["js", "mjs", "cjs", "jsx", "ts", "tsx", "mts", "cts"];

/// Stack size for lint worker threads (deeply nested code recurses in the parser and the
/// scope builder).
pub const WORKER_STACK: usize = 256 << 20;

/// Options for [`lint_paths`].
#[derive(Debug)]
pub struct LintOptions {
    pub paths: Vec<PathBuf>,
    pub cwd: PathBuf,
    /// `--config`: one config for every file, patterns relative to `cwd`.
    pub config: Option<PathBuf>,
    pub fix: bool,
    pub threads: usize,
    /// Honour `.gitignore` files while walking directories.
    pub gitignore: bool,
    /// Warn when an explicitly named file is ignored.
    pub warn_ignored: bool,
    pub resolve: ResolveOptions,
    pub node: NodeOptions,
}

impl LintOptions {
    pub fn new(cwd: PathBuf) -> LintOptions {
        let cwd_str: std::sync::Arc<str> = cwd.to_string_lossy().into();
        LintOptions {
            paths: Vec::new(),
            cwd,
            config: None,
            fix: false,
            threads: default_threads(),
            gitignore: true,
            warn_ignored: true,
            resolve: ResolveOptions {
                cwd: Some(cwd_str),
                modules: Some(crate::modules::ModuleHost::real()),
                ..ResolveOptions::new()
            },
            node: NodeOptions::default(),
        }
    }
}

/// `min(4, cores)`.
pub fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, 4)
}

/// An unsupported rule and the config that enables it.
#[derive(Clone, Debug)]
pub struct Unsupported {
    pub config: String,
    pub rule: String,
    pub reason: UnsupportedReason,
}

/// Everything a run produced.
#[derive(Debug, Default)]
pub struct LintRun {
    pub reports: Vec<FileReport>,
    pub unsupported: Vec<Unsupported>,
    pub notes: Vec<String>,
    /// Config files that were loaded, with their kind.
    pub configs: Vec<(String, ConfigKind)>,
}

// ---------------------------------------------------------------------------------------
// .gitignore
// ---------------------------------------------------------------------------------------

#[derive(Debug)]
struct GitIgnore {
    /// Directory of the `.gitignore`, relative to the walk root, `/`-separated ("" = root).
    base: String,
    rules: Vec<(bool, Glob)>,
}

impl GitIgnore {
    fn parse(base: String, text: &str) -> GitIgnore {
        let mut rules = Vec::new();
        for line in text.lines() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (neg, pat) = match line.strip_prefix('!') {
                Some(p) => (true, p),
                None => (false, line.strip_prefix('\\').unwrap_or(line)),
            };
            let body = pat.strip_suffix('/').unwrap_or(pat);
            let anchored = body.contains('/');
            let body = body.strip_prefix('/').unwrap_or(body);
            let mut g = if anchored {
                body.to_owned()
            } else {
                format!("**/{body}")
            };
            if pat.ends_with('/') {
                g.push('/');
            }
            rules.push((neg, Glob::new(&g)));
        }
        GitIgnore { base, rules }
    }

    /// `Some(true)` ignored, `Some(false)` re-included, `None` no opinion.
    fn check(&self, rel: &str, is_dir: bool) -> Option<bool> {
        let sub = if self.base.is_empty() {
            rel
        } else {
            rel.strip_prefix(&self.base)?.strip_prefix('/')?
        };
        let mut res = None;
        for (neg, g) in &self.rules {
            let hit = if is_dir {
                g.matches_dir(sub)
            } else {
                !g.dir_only && g.matches(sub)
            };
            if hit {
                res = Some(!*neg);
            }
        }
        res
    }
}

fn gitignored(stack: &[GitIgnore], rel: &str, is_dir: bool) -> bool {
    let mut ignored = false;
    for g in stack {
        if let Some(v) = g.check(rel, is_dir) {
            ignored = v;
        }
    }
    ignored
}

// ---------------------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------------------

/// A file to lint and whether the user named it explicitly.
#[derive(Clone, Debug)]
struct Candidate {
    path: PathBuf,
    explicit: bool,
}

fn has_lintable_ext(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| LINTABLE_EXTENSIONS.contains(&e))
}

fn rel_str(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Walks `dir`, honouring `.gitignore` files and skipping `node_modules` and `.git`.
fn walk_dir(root: &Path, gitignore: bool, out: &mut Vec<Candidate>) {
    let mut stack_ign: Vec<GitIgnore> = Vec::new();
    // Parent .gitignore files between the repository root and `root` also apply.
    if gitignore {
        let mut ancestors: Vec<&Path> = root.ancestors().skip(1).collect();
        let repo_root = ancestors.iter().position(|a| a.join(".git").exists());
        if let Some(i) = repo_root {
            ancestors.truncate(i + 1);
            for a in ancestors.iter().rev() {
                if let Ok(t) = std::fs::read_to_string(a.join(".gitignore")) {
                    // Re-base onto `root`: patterns from above are matched against the path
                    // from their own directory, which we emulate by prefixing.
                    let prefix = rel_str(a, root);
                    let gi = GitIgnore::parse(String::new(), &t);
                    stack_ign.push(rebase(gi, &prefix));
                }
            }
        }
    }
    walk_inner(root, root, gitignore, &mut stack_ign, out, 0);
}

/// Wraps a parent `.gitignore` so it sees paths as `prefix/rel`.
fn rebase(gi: GitIgnore, prefix: &str) -> GitIgnore {
    if prefix.is_empty() {
        return gi;
    }
    // Keep only rules that can match below `prefix`, re-expressed relative to it.
    let mut rules = Vec::new();
    for (neg, g) in gi.rules {
        let src = g.source.clone();
        if let Some(rest) = src.strip_prefix("**/") {
            rules.push((neg, Glob::new(&format!("**/{rest}"))));
        } else if let Some(rest) = src.strip_prefix(&format!("{prefix}/")) {
            rules.push((neg, Glob::new(rest)));
        }
    }
    GitIgnore {
        base: String::new(),
        rules,
    }
}

fn walk_inner(
    root: &Path,
    dir: &Path,
    gitignore: bool,
    ign: &mut Vec<GitIgnore>,
    out: &mut Vec<Candidate>,
    depth: u32,
) {
    if depth > 200 {
        return;
    }
    let pushed = if gitignore {
        match std::fs::read_to_string(dir.join(".gitignore")) {
            Ok(t) => {
                ign.push(GitIgnore::parse(rel_str(root, dir), &t));
                true
            }
            Err(_) => false,
        }
    } else {
        false
    };
    let mut entries: Vec<(PathBuf, bool)> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(Result::ok)
            .filter_map(|e| {
                let ft = e.file_type().ok()?;
                let p = e.path();
                let is_dir = if ft.is_symlink() {
                    p.is_dir()
                } else {
                    ft.is_dir()
                };
                Some((p, is_dir))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    entries.sort();
    for (p, is_dir) in entries {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if is_dir {
            if name == "node_modules" || name == ".git" {
                continue;
            }
            let rel = rel_str(root, &p);
            if gitignore && gitignored(ign, &rel, true) {
                continue;
            }
            walk_inner(root, &p, gitignore, ign, out, depth + 1);
        } else if has_lintable_ext(&p) {
            let rel = rel_str(root, &p);
            if gitignore && gitignored(ign, &rel, false) {
                continue;
            }
            out.push(Candidate {
                path: p,
                explicit: false,
            });
        }
    }
    if pushed {
        ign.pop();
    }
}

// ---------------------------------------------------------------------------------------
// Config lookup
// ---------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct DirConfigs {
    eslint: Option<PathBuf>,
    oxlint: Option<PathBuf>,
    biome: Option<PathBuf>,
}

/// Finds the config that governs each directory (cached per directory). Public so editors
/// (`ts-r-lsp`) resolve configs exactly like `ts-r-lint`; create a new one after config files
/// are added or removed.
#[derive(Default)]
pub struct ConfigLookup {
    dirs: FxHashMap<PathBuf, Arc<DirConfigs>>,
}

impl std::fmt::Debug for ConfigLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigLookup")
            .field("dirs", &self.dirs.len())
            .finish()
    }
}

impl ConfigLookup {
    pub fn new() -> ConfigLookup {
        ConfigLookup::default()
    }

    /// The config for files in `dir`: the nearest ESLint config at or above it, else the
    /// nearest oxlint config, else the nearest Biome config, else the built-in default.
    pub fn governing(&mut self, dir: &Path) -> ConfigRef {
        let d = self.nearest(dir);
        if let Some(p) = &d.eslint {
            ConfigRef::Eslint(p.clone())
        } else if let Some(p) = &d.oxlint {
            ConfigRef::Oxlint(p.clone())
        } else if let Some(p) = &d.biome {
            ConfigRef::Biome(p.clone())
        } else {
            ConfigRef::Default
        }
    }

    fn nearest(&mut self, dir: &Path) -> Arc<DirConfigs> {
        if let Some(c) = self.dirs.get(dir) {
            return c.clone();
        }
        let mut here = match dir.parent() {
            Some(p) => (*self.nearest(p)).clone(),
            None => DirConfigs::default(),
        };
        let find = |names: &[&str]| names.iter().map(|n| dir.join(n)).find(|p| p.is_file());
        if let Some(p) = find(ESLINT_CONFIG_NAMES) {
            here.eslint = Some(p);
        }
        if let Some(p) = find(OXLINT_CONFIG_NAMES) {
            here.oxlint = Some(p);
        }
        if let Some(p) = find(BIOME_CONFIG_NAMES) {
            here.biome = Some(p);
        }
        let a = Arc::new(here);
        self.dirs.insert(dir.to_path_buf(), a.clone());
        a
    }
}

/// Which config governs a file.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum ConfigRef {
    Eslint(PathBuf),
    Oxlint(PathBuf),
    Biome(PathBuf),
    Default,
}

fn config_kind_for(path: &Path) -> ConfigRef {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if OXLINT_CONFIG_NAMES.contains(&name.as_str()) {
        ConfigRef::Oxlint(path.to_path_buf())
    } else if BIOME_CONFIG_NAMES.contains(&name.as_str()) {
        ConfigRef::Biome(path.to_path_buf())
    } else {
        ConfigRef::Eslint(path.to_path_buf())
    }
}

/// Loads every referenced config. ESLint configs are evaluated in one Node batch (through the
/// `lint-config` cache). `explicit_base` replaces each config's base directory (`--config`).
pub fn load_configs(
    refs: &[ConfigRef],
    opts: &LintOptions,
    explicit_base: Option<&Path>,
) -> Result<BTreeMap<ConfigRef, ConfigFile>, String> {
    let reg = opts.resolve.registry;
    let mut out = BTreeMap::new();
    let eslint: Vec<PathBuf> = refs
        .iter()
        .filter_map(|r| match r {
            ConfigRef::Eslint(p) => Some(p.clone()),
            _ => None,
        })
        .collect();
    if !eslint.is_empty() {
        for (p, r) in eslint.iter().zip(evaluate_all(&eslint, &opts.node)) {
            let ev = r.map_err(|e| format!("{}: {e}", p.display()))?;
            let objs = objects_from_eslint_json(&ev.config, reg)
                .map_err(|e| format!("{}: {e}", p.display()))?;
            let base = explicit_base
                .map(Path::to_path_buf)
                .unwrap_or_else(|| p.parent().unwrap_or(Path::new("/")).to_path_buf());
            let mut c = ConfigFile::new(ConfigKind::Eslint, Some(p.clone()), base, objs);
            c.deps = ev.deps;
            out.insert(ConfigRef::Eslint(p.clone()), c);
        }
    }
    for r in refs {
        match r {
            ConfigRef::Oxlint(p) => {
                let mut c = oxlint::load(p, reg)?;
                if let Some(b) = explicit_base {
                    c.base = b.to_path_buf();
                }
                out.insert(r.clone(), c);
            }
            ConfigRef::Biome(p) => {
                let mut c = biome::load(p, reg)?;
                if let Some(b) = explicit_base {
                    c.base = b.to_path_buf();
                }
                out.insert(r.clone(), c);
            }
            ConfigRef::Default => {
                out.insert(r.clone(), default_config(opts.cwd.clone(), reg));
            }
            ConfigRef::Eslint(_) => {}
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------------------

const IGNORED_MSG: &str = "File ignored because of a matching ignore pattern. Use \"--no-ignore\" to disable file ignore settings or use \"--no-warn-ignored\" to suppress this warning.";
const NO_CONFIG_MSG: &str = "File ignored because no matching configuration was supplied.";

/// Lints `opts.paths`. `Err` is a fatal configuration problem (ESLint exit code 2).
pub fn lint_paths(opts: &LintOptions) -> Result<LintRun, String> {
    let cwd = &opts.cwd;
    // 1. Discover files.
    let mut cands = Vec::new();
    let paths = if opts.paths.is_empty() {
        vec![cwd.clone()]
    } else {
        opts.paths.clone()
    };
    for p in &paths {
        let abs = if p.is_absolute() {
            p.clone()
        } else {
            cwd.join(p)
        };
        let abs = normalize(&abs);
        if abs.is_dir() {
            walk_dir(&abs, opts.gitignore, &mut cands);
        } else if abs.is_file() {
            cands.push(Candidate {
                path: abs,
                explicit: true,
            });
        } else {
            return Err(format!(
                "No files matching the pattern \"{}\" were found.",
                p.display()
            ));
        }
    }
    cands.sort_by(|a, b| a.path.cmp(&b.path));
    cands.dedup_by(|a, b| {
        if a.path == b.path {
            b.explicit |= a.explicit;
            true
        } else {
            false
        }
    });

    // 2. Decide which config governs each file.
    let explicit_cfg = opts.config.as_ref().map(|c| {
        let abs = if c.is_absolute() {
            c.clone()
        } else {
            cwd.join(c)
        };
        normalize(&abs)
    });
    let mut lookup = ConfigLookup::default();
    let mut file_refs: Vec<ConfigRef> = Vec::with_capacity(cands.len());
    for c in &cands {
        let r = match &explicit_cfg {
            Some(p) => config_kind_for(p),
            None => lookup.governing(c.path.parent().unwrap_or(Path::new("/"))),
        };
        file_refs.push(r);
    }
    let mut distinct = file_refs.clone();
    distinct.sort();
    distinct.dedup();
    let configs = load_configs(
        &distinct,
        opts,
        explicit_cfg.as_ref().map(|_| cwd.as_path()),
    )?;

    // 3. Resolve per-file configs; drop ignored files.
    let mut work: Vec<WorkItem> = Vec::new();
    let mut work_cfg: Vec<String> = Vec::new();
    let mut last_name = String::new();
    for (c, r) in cands.iter().zip(&file_refs) {
        // Work items pushed by the previous iteration belong to the previous config.
        while work_cfg.len() < work.len() {
            work_cfg.push(last_name.clone());
        }
        last_name = config_name(cwd, r);
        let Some(cfg) = configs.get(r) else { continue };
        let rel = match cfg.relative(&c.path) {
            Some(r) => r,
            None => {
                // Outside the config's base: ESLint treats such files as ignored.
                if c.explicit && opts.warn_ignored {
                    work.push((c.path.clone(), None, Some(IGNORED_MSG)));
                }
                continue;
            }
        };
        match cfg.file_config(&rel, &opts.resolve) {
            Ok(fc) => work.push((c.path.clone(), Some(fc), None)),
            Err(NotLinted::Ignored) => {
                if c.explicit && opts.warn_ignored {
                    work.push((c.path.clone(), None, Some(IGNORED_MSG)));
                }
            }
            Err(NotLinted::NoMatchingConfig) => {
                if c.explicit && opts.warn_ignored {
                    work.push((c.path.clone(), None, Some(NO_CONFIG_MSG)));
                }
            }
        }
    }

    while work_cfg.len() < work.len() {
        work_cfg.push(last_name.clone());
    }

    // 4. Files whose config enables type-aware rules: per tsconfig project, on our checker.
    let mut typed_reports: Vec<(usize, FileReport)> = Vec::new();
    let mut typed_unsupported = Vec::new();
    let mut typed_notes = Vec::new();
    if opts.resolve.types_available {
        let mut reqs = Vec::new();
        let mut req_idx = Vec::new();
        for (i, (path, cfg, _)) in work.iter().enumerate() {
            if let Some(cfg) = cfg
                && cfg.rules.has_typed()
            {
                reqs.push(TypedRequest {
                    path: path.clone(),
                    cfg: cfg.clone(),
                    config: work_cfg[i].clone(),
                });
                req_idx.push(i);
            }
        }
        if !reqs.is_empty() {
            let typed = lint_projects(
                &reqs,
                &DriverOptions {
                    threads: opts.threads,
                    fix: opts.fix,
                    node: &opts.node,
                    cwd,
                },
            );
            for (n, out) in typed.outcomes.into_iter().enumerate() {
                if let Some(TypedOutcome::Typed { result, source }) = out {
                    let path = reqs[n].path.clone();
                    if let Some(text) = &result.output {
                        let _ = std::fs::write(&path, text);
                    }
                    typed_reports.push((
                        req_idx[n],
                        FileReport {
                            path,
                            source,
                            result,
                        },
                    ));
                }
            }
            for (scope, rule, reason) in typed.unsupported {
                typed_unsupported.push(Unsupported {
                    config: scope,
                    rule,
                    reason,
                });
            }
            for p in &typed.projects {
                if p.parity == crate::typed::parity::Parity::Mismatch {
                    typed_notes.push(format!(
                        "type-aware rules not run in {}: {}",
                        p.config, p.detail
                    ));
                }
            }
            if std::env::var_os("TS_R_LINT_TIMING").is_some() {
                for p in &typed.projects {
                    eprintln!(
                        "ts-r-lint: project {}: {} program files, {} linted, load {:.0} ms, check+lint {:.0} ms, parity {} ({}{}; tsc {:.0} ms)",
                        p.config,
                        p.program_files,
                        p.linted,
                        p.load_ms,
                        p.check_ms,
                        p.parity.as_str(),
                        if p.cached { "cached; " } else { "" },
                        p.detail,
                        p.tsc_ms
                    );
                }
            }
            typed_notes.extend(typed.notes);
        }
    }

    // 5. Everything else in parallel (and typed files that fell back to untyped linting).
    let done: rustc_hash::FxHashSet<usize> = typed_reports.iter().map(|(i, _)| *i).collect();
    let rest: Vec<usize> = (0..work.len()).filter(|i| !done.contains(i)).collect();
    let mut all: Vec<(usize, FileReport)> = run_parallel(&work, &rest, opts);
    all.extend(typed_reports);
    all.sort_by_key(|(i, _)| *i);
    let reports = all.into_iter().map(|(_, r)| r).collect();

    let mut run = LintRun {
        reports,
        ..LintRun::default()
    };
    let mut seen_typed = std::collections::BTreeSet::new();
    for u in typed_unsupported {
        if seen_typed.insert((u.config.clone(), u.rule.clone())) {
            run.unsupported.push(u);
        }
    }
    run.notes.extend(typed_notes);
    let mut seen = std::collections::BTreeSet::new();
    for (r, cfg) in &configs {
        let name = config_name(cwd, r);
        run.configs.push((name.clone(), cfg.kind));
        for (rule, reason) in cfg.unsupported(opts.resolve.types_available) {
            if seen.insert((name.clone(), rule.clone())) {
                run.unsupported.push(Unsupported {
                    config: name.clone(),
                    rule,
                    reason,
                });
            }
        }
        run.notes.extend(cfg.notes());
    }
    if let Ok(n) = opts.resolve.notes.lock() {
        run.notes.extend(n.iter().cloned());
    }
    run.notes.sort();
    run.notes.dedup();
    Ok(run)
}

/// A file to lint with its config, or a file-level warning to report instead.
type WorkItem = (PathBuf, Option<Arc<FileConfig>>, Option<&'static str>);

/// The display name of a lint config in summaries.
fn config_name(cwd: &Path, r: &ConfigRef) -> String {
    match r {
        ConfigRef::Eslint(p) | ConfigRef::Oxlint(p) | ConfigRef::Biome(p) => rel_str(cwd, p),
        ConfigRef::Default => "(built-in default)".to_owned(),
    }
}

/// Lints `work[i]` for every `i` in `which` without type information.
fn run_parallel(
    work: &[WorkItem],
    which: &[usize],
    opts: &LintOptions,
) -> Vec<(usize, FileReport)> {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, FileReport)>> = Mutex::new(Vec::with_capacity(which.len()));
    let threads = opts.threads.clamp(1, 64).min(which.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..threads {
            let next = &next;
            let results = &results;
            let _ = std::thread::Builder::new()
                .stack_size(WORKER_STACK)
                .spawn_scoped(s, move || {
                    let mut local = Vec::new();
                    loop {
                        let n = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&i) = which.get(n) else {
                            break;
                        };
                        let Some((path, cfg, warn)) = work.get(i) else {
                            break;
                        };
                        local.push((i, lint_one(path, cfg.as_deref(), *warn, i, opts.fix)));
                    }
                    if let Ok(mut r) = results.lock() {
                        r.extend(local);
                    }
                });
        }
    });
    results.into_inner().unwrap_or_default()
}

fn lint_one(
    path: &Path,
    cfg: Option<&FileConfig>,
    warn: Option<&str>,
    idx: usize,
    fix: bool,
) -> FileReport {
    let Some(cfg) = cfg else {
        return FileReport {
            path: path.to_path_buf(),
            source: String::new(),
            result: FileResult {
                diagnostics: vec![file_warning(warn.unwrap_or(IGNORED_MSG))],
                ..FileResult::default()
            },
        };
    };
    let src = match std::fs::read(path) {
        Ok(b) => match String::from_utf8(b) {
            Ok(s) => s,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        },
        Err(e) => {
            let mut d = file_warning(&format!("Parsing error: {e}"));
            d.severity = crate::diagnostic::Severity::Error;
            d.fatal = true;
            return FileReport {
                path: path.to_path_buf(),
                source: String::new(),
                result: FileResult {
                    diagnostics: vec![d],
                    ..FileResult::default()
                },
            };
        }
    };
    // ESLint strips a leading BOM before linting.
    let text = src.strip_prefix('\u{feff}').unwrap_or(&src);
    let path_str = path.to_string_lossy();
    let types = NoTypeInfo;
    let key = FileKey(idx as u32);
    let mut result = if fix {
        lint_and_fix(&path_str, text, cfg, &types, key)
    } else {
        lint_source(&path_str, text, cfg, &types, key)
    };
    let source = match result.output.clone() {
        Some(out) => {
            let bom = if src.starts_with('\u{feff}') {
                "\u{feff}"
            } else {
                ""
            };
            match std::fs::write(path, format!("{bom}{out}")) {
                Ok(()) => out,
                Err(e) => {
                    // The fixes are not on disk: report the file as it is, and the failure.
                    result = lint_source(&path_str, text, cfg, &types, key);
                    let mut d = file_warning(&format!("Could not write fixes: {e}"));
                    d.severity = crate::diagnostic::Severity::Error;
                    result.diagnostics.insert(0, d);
                    text.to_owned()
                }
            }
        }
        None => text.to_owned(),
    };
    FileReport {
        path: path.to_path_buf(),
        source,
        result,
    }
}

/// Removes `.` and `..` components without touching the file system.
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitignore_rules() {
        let g = GitIgnore::parse(String::new(), "dist/\n*.log\n/build\n!keep.log\nsrc/gen\n");
        assert_eq!(g.check("dist", true), Some(true));
        assert_eq!(g.check("a/dist", true), Some(true));
        assert_eq!(g.check("dist", false), None);
        assert_eq!(g.check("x/y.log", false), Some(true));
        assert_eq!(g.check("keep.log", false), Some(false));
        assert_eq!(g.check("build", true), Some(true));
        assert_eq!(g.check("a/build", true), None);
        assert_eq!(g.check("src/gen", true), Some(true));
        let sub = GitIgnore::parse("pkg".to_owned(), "out\n");
        assert_eq!(sub.check("pkg/out", true), Some(true));
        assert_eq!(sub.check("out", true), None);
    }

    #[cfg(unix)]
    #[test]
    fn unwritable_fix_is_an_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ts-r-lint-fix-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.js");
        let src = "if (!!value) {}\n";
        std::fs::write(&file, src).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
        let writable = std::fs::OpenOptions::new().write(true).open(&file).is_ok();
        let v = serde_json::json!([{
            "files": ["**/*.js"],
            "rules": { "no-extra-boolean-cast": "error" },
        }]);
        let objs = objects_from_eslint_json(&v, crate::Registry::global()).unwrap();
        let cfg = ConfigFile::new(ConfigKind::Eslint, None, dir.clone(), objs);
        let fc = cfg.file_config("a.js", &ResolveOptions::new()).unwrap();
        let report = lint_one(&file, Some(&fc), None, 0, true);
        let _ = std::fs::remove_dir_all(&dir);
        if writable {
            return; // running as root
        }
        let d = &report.result.diagnostics;
        assert!(d[0].message.starts_with("Could not write fixes"));
        assert_eq!(d[0].severity, crate::diagnostic::Severity::Error);
        assert!(d.iter().any(|d| d.rule == Some("no-extra-boolean-cast")));
        assert_eq!(report.source, src);
    }
}
