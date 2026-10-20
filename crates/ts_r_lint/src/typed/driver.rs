//! Type-aware linting of a set of files, one tsconfig project at a time.
//!
//! [`lint_projects`] groups the files whose config enables a type-aware rule by the tsconfig
//! typescript-eslint would use for them, then for each project: loads and checks it once
//! (shared lib and `node_modules` levels are reused across projects), lints its files with
//! types, and decides from the [`parity`](super::parity) check whether those results may be
//! reported. A project whose diagnostics differ from the user's tsc gets its files linted
//! without the type-aware rules, and the rules are reported as unsupported there, once. The
//! user's tsc runs on background threads while our checker works (one tsc process per thread,
//! as many threads as `threads`, at most 4), and its verdict is cached.
//!
//! Nothing here writes files: with `fix`, fixed text is returned in
//! [`FileResult::output`] and the caller writes it. Parity always compares the check of the
//! files as they are on disk (the first pass), not of the fixed text.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Instant;

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_checker::{Layer, LibStore};
use ts_r_config::{DirEntry, FileSystem, RealFs};

use super::parity::{self, Parity, ParityResult, TsInstall};
use super::{LoadedProject, ProjectRun, TypedFile, check_and_lint, load_project};
use crate::config::eslint_eval::NodeOptions;
use crate::config::mapping::UnsupportedReason;
use crate::engine::{FileConfig, FileResult, MAX_FIX_PASSES};
use crate::fix::apply_fixes;

/// A file whose config enables type-aware rules.
#[derive(Clone, Debug)]
pub struct TypedRequest {
    pub path: PathBuf,
    pub cfg: Arc<FileConfig>,
    /// The lint config's display name (for the unsupported summary).
    pub config: String,
}

/// What became of a [`TypedRequest`].
#[derive(Debug)]
pub enum TypedOutcome {
    /// Linted with types (the result includes the untyped rules too).
    Typed { result: FileResult, source: String },
    /// Lint it without type information (the caller does).
    Untyped,
}

/// Options for [`lint_projects`].
#[derive(Debug)]
pub struct DriverOptions<'a> {
    /// Threads for checking and linting a project (at most 4 are used).
    pub threads: usize,
    pub fix: bool,
    pub node: &'a NodeOptions,
    /// Paths in the summary are shown relative to this directory.
    pub cwd: &'a Path,
}

/// One project's numbers.
#[derive(Clone, Debug)]
pub struct ProjectSummary {
    pub config: String,
    pub program_files: usize,
    pub linted: usize,
    pub parity: Parity,
    pub detail: String,
    pub cached: bool,
    pub load_ms: f64,
    pub check_ms: f64,
    pub tsc_ms: f64,
}

/// What [`lint_projects`] produced.
#[derive(Debug, Default)]
pub struct TypedRun {
    /// One per request, in order.
    pub outcomes: Vec<Option<TypedOutcome>>,
    /// `(scope, rule, reason)`: the scope is a tsconfig or lint config.
    pub unsupported: Vec<(String, String, UnsupportedReason)>,
    pub projects: Vec<ProjectSummary>,
    pub notes: Vec<String>,
}

/// A finished run of the user's tsc: comparison keys (or why not) and its time.
type TscResult = (Result<BTreeMap<String, usize>, String>, f64);

/// The user's tsc, run on background threads in the order projects are queued.
struct TscPool {
    tx: Mutex<Option<mpsc::Sender<(String, TsInstall, bool)>>>,
    done: Mutex<FxHashMap<String, TscResult>>,
    ready: Condvar,
}

impl TscPool {
    fn queue(&self, config: &str, ts: &TsInstall, incremental: bool) {
        if let Ok(tx) = self.tx.lock()
            && let Some(tx) = tx.as_ref()
        {
            let _ = tx.send((config.to_owned(), ts.clone(), incremental));
        }
    }

    fn wait(&self, config: &str) -> TscResult {
        let mut done = self.done.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(r) = done.remove(config) {
                return r;
            }
            done = self.ready.wait(done).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn close(&self) {
        if let Ok(mut tx) = self.tx.lock() {
            tx.take();
        }
    }
}

/// Closes the tsc queue when dropped (also while unwinding).
struct CloseOnDrop<'a>(&'a TscPool);

impl Drop for CloseOnDrop<'_> {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// The text of a caught panic.
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".to_owned()
    }
}

/// A project to lint: its tsconfig and the requests it covers.
struct Group {
    tsconfig: String,
    reqs: Vec<usize>,
    ts: Option<TsInstall>,
    key: Option<String>,
    cached: Option<ParityResult>,
    queued: bool,
}

/// Lints `reqs` with types. See the module docs.
pub fn lint_projects(reqs: &[TypedRequest], opts: &DriverOptions<'_>) -> TypedRun {
    let mut run = TypedRun {
        outcomes: reqs.iter().map(|_| None).collect(),
        ..TypedRun::default()
    };
    let mut unsupported: BTreeMap<(String, String), UnsupportedReason> = BTreeMap::new();
    let mut mark = |scope: &str, cfg: &FileConfig, reason: UnsupportedReason| {
        for name in cfg.rules.typed_names() {
            unsupported
                .entry((scope.to_owned(), name.to_owned()))
                .or_insert(reason);
        }
    };

    // 1. Group by tsconfig.
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, r) in reqs.iter().enumerate() {
        let path = r.path.to_string_lossy().replace('\\', "/");
        let tsconfig = r
            .cfg
            .env
            .project_lib
            .as_ref()
            .map(|p| p.tsconfig_for(&path));
        match tsconfig {
            None => {
                mark(&r.config, &r.cfg, UnsupportedReason::NoProject);
                run.outcomes[i] = Some(TypedOutcome::Untyped);
            }
            Some(None) => {
                mark(&r.config, &r.cfg, UnsupportedReason::NotInProject);
                run.outcomes[i] = Some(TypedOutcome::Untyped);
            }
            Some(Some(ts)) => groups.entry(ts).or_default().push(i),
        }
    }
    if groups.is_empty() {
        run.unsupported = flatten(unsupported);
        return run;
    }

    // 2. The TypeScript lib files: ours, else the user's TypeScript's.
    let first_dir = groups
        .keys()
        .next()
        .and_then(|c| Path::new(c).parent())
        .unwrap_or(Path::new("/"))
        .to_path_buf();
    let libs = LibStore::locate().or_else(|| {
        let ts = parity::find_typescript(&first_dir)?;
        let dir = ts.tsc.parent()?.parent()?.join("lib");
        dir.join("lib.es5.d.ts")
            .is_file()
            .then(|| LibStore::new(dir))
    });
    let Some(libs) = libs else {
        for idx in groups.values().flatten() {
            mark(
                &reqs[*idx].config,
                &reqs[*idx].cfg,
                UnsupportedReason::NoLibFiles,
            );
            run.outcomes[*idx] = Some(TypedOutcome::Untyped);
        }
        run.unsupported = flatten(unsupported);
        return run;
    };

    // 3. Parity: cached verdicts, and the user's tsc queued for the rest.
    let mode = parity::mode();
    let parity_off = mode.as_deref() == Some("off");
    let require = mode.as_deref() == Some("require");
    let fs = RealFs::new();
    let mut gs: Vec<Group> = Vec::new();
    for (tsconfig, reqs) in groups {
        let mut g = Group {
            tsconfig,
            reqs,
            ts: None,
            key: None,
            cached: None,
            queued: false,
        };
        if !parity_off {
            let dir = Path::new(&g.tsconfig)
                .parent()
                .unwrap_or(Path::new("/"))
                .to_path_buf();
            g.ts = parity::find_typescript(&dir);
            let parsed = ts_r_config::load_tsconfig(&fs, &g.tsconfig);
            let mut inputs = vec![parsed.config_path.clone()];
            inputs.extend(parsed.extended_config_paths.iter().cloned());
            g.key = parity::cache_key(&g.tsconfig, &inputs, g.ts.as_ref());
            if opts.node.use_cache {
                g.cached = g.key.as_deref().and_then(parity::read_cached);
            }
        }
        gs.push(g);
    }

    let (tx, rx) = mpsc::channel::<(String, TsInstall, bool)>();
    let pool = TscPool {
        tx: Mutex::new(Some(tx)),
        done: Mutex::new(FxHashMap::default()),
        ready: Condvar::new(),
    };
    for g in &mut gs {
        if g.cached.is_none()
            && let Some(ts) = &g.ts
        {
            let incremental = tsconfig_incremental(&fs, &g.tsconfig);
            pool.queue(&g.tsconfig, ts, incremental);
            g.queued = true;
        }
    }

    // Shared levels stay alive for the whole run so later projects reuse them.
    let mut keep: Vec<Arc<Layer>> = Vec::new();
    let queued = gs.iter().filter(|g| g.queued).count();
    let jobs = opts.threads.clamp(1, 4).min(queued.max(1));
    let rx = Mutex::new(rx);
    std::thread::scope(|s| {
        let pool = &pool;
        let rx = &rx;
        let node = opts.node;
        let mut started = 0;
        for _ in 0..jobs {
            let worker = std::thread::Builder::new().spawn_scoped(s, move || {
                loop {
                    // One worker waits on the queue at a time; the job runs unlocked.
                    let next = rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
                    let Ok((config, ts, incremental)) = next else {
                        break;
                    };
                    let t0 = Instant::now();
                    // Always answer: a project waits for this result.
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        parity::run_tsc(&node.node, &node.wrapper, &ts, &config, incremental)
                    }))
                    .unwrap_or_else(|p| Err(format!("internal error: {}", panic_message(&*p))));
                    let ms = t0.elapsed().as_secs_f64() * 1000.0;
                    let mut done = pool.done.lock().unwrap_or_else(|e| e.into_inner());
                    done.insert(config, (r, ms));
                    pool.ready.notify_all();
                }
            });
            if worker.is_ok() {
                started += 1;
            }
        }
        if started == 0 {
            pool.close();
        }

        // However the projects end, the tsc workers must see the queue close, or the scope
        // waits for them forever.
        let _close = CloseOnDrop(pool);
        for g in &mut gs {
            // A checker bug (a panic) must not take the run down: the project's files are
            // linted without types and the failure is reported as an internal error.
            let summary = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                lint_group(g, reqs, opts, &libs, pool, &mut keep, &mut run, require)
            }));
            let summary = match summary {
                Ok(s) => s,
                Err(payload) => {
                    let scope = display(opts.cwd, &g.tsconfig);
                    let msg = panic_message(&*payload);
                    run.notes.push(format!(
                        "internal error: the type checker failed on {scope} ({msg}); its files \
                         were linted without type-aware rules"
                    ));
                    for &i in &g.reqs {
                        mark(&scope, &reqs[i].cfg, UnsupportedReason::InternalError);
                        run.outcomes[i] = Some(TypedOutcome::Untyped);
                    }
                    Some(ProjectSummary {
                        config: scope,
                        program_files: 0,
                        linted: 0,
                        parity: Parity::Mismatch,
                        detail: format!("internal error: {msg}"),
                        cached: false,
                        load_ms: 0.0,
                        check_ms: 0.0,
                        tsc_ms: 0.0,
                    })
                }
            };
            if let Some(sum) = summary {
                if sum.parity == Parity::Mismatch {
                    let scope = display(opts.cwd, &g.tsconfig);
                    for &i in &g.reqs {
                        mark(&scope, &reqs[i].cfg, UnsupportedReason::ProjectMismatch);
                    }
                }
                run.projects.push(sum);
            }
        }
        pool.close();
    });
    drop(keep);
    run.unsupported = flatten(unsupported);
    run
}

/// Loads, checks and lints one project and decides its parity. Fills `run.outcomes` for the
/// group's requests.
#[allow(clippy::too_many_arguments)]
fn lint_group(
    g: &mut Group,
    reqs: &[TypedRequest],
    opts: &DriverOptions<'_>,
    libs: &LibStore,
    pool: &TscPool,
    keep: &mut Vec<Arc<Layer>>,
    run: &mut TypedRun,
    require: bool,
) -> Option<ProjectSummary> {
    let threads = opts.threads.clamp(1, 4);
    let t0 = Instant::now();
    let fs = RealFs::new();
    let mut project = load_project(&fs, &g.tsconfig, libs, threads);
    remember(keep, &project);
    let load_ms = t0.elapsed().as_secs_f64() * 1000.0;

    // Requests in the program; the rest are linted without types.
    let mut files: Vec<(usize, usize)> = Vec::new();
    for &i in &g.reqs {
        let path = reqs[i].path.to_string_lossy().replace('\\', "/");
        let fi = project
            .file_index(&path)
            .or_else(|| project.file_index(&fs.realpath(&path)));
        match fi {
            Some(fi) if !project.program.files[fi].text.starts_with('\u{feff}') => {
                files.push((i, fi))
            }
            Some(_) => {
                run.notes.push(format!(
                    "{}: starts with a byte order mark; type-aware rules not run on it",
                    display(opts.cwd, &path)
                ));
                run.outcomes[i] = Some(TypedOutcome::Untyped);
            }
            None => {
                run.notes.push(format!(
                    "{}: not part of the program of {}; type-aware rules not run on it",
                    display(opts.cwd, &path),
                    display(opts.cwd, &g.tsconfig)
                ));
                run.outcomes[i] = Some(TypedOutcome::Untyped);
            }
        }
    }

    let lint: Vec<TypedFile> = files
        .iter()
        .map(|&(i, fi)| TypedFile {
            file: fi,
            cfg: reqs[i].cfg.clone(),
        })
        .collect();
    let t1 = Instant::now();
    let (prun, texts) = if opts.fix {
        let (p, r, texts) = fix_passes(project, &lint, libs, threads, keep);
        project = p;
        (r, texts)
    } else {
        (check_and_lint(&project, &lint, threads), Vec::new())
    };
    let check_ms = t1.elapsed().as_secs_f64() * 1000.0;

    // Parity.
    let ours_hash = parity::keys_hash(&prun.keys);
    let (mut verdict, tsc_ms) = if !prun.failed.is_empty() {
        (
            ParityResult {
                result: Parity::Mismatch,
                detail: format!("our checker failed on {} file(s)", prun.failed.len()),
                typescript: String::new(),
                cached: false,
                tsc_ms: 0.0,
                ours: ours_hash.clone(),
            },
            0.0,
        )
    } else {
        decide(g, &prun, pool, &ours_hash)
    };
    if require && verdict.result == Parity::Unknown {
        verdict.result = Parity::Mismatch;
    }

    let failed: FxHashSet<usize> = prun.failed.iter().copied().collect();
    let mut results = prun.results.into_iter();
    for (n, &(i, fi)) in files.iter().enumerate() {
        let mut result = results.next().unwrap_or_default();
        if verdict.result == Parity::Mismatch || failed.contains(&fi) {
            run.outcomes[i] = Some(TypedOutcome::Untyped);
            continue;
        }
        let source = match texts.get(n).cloned().flatten() {
            Some(t) => {
                result.output = Some(t.clone());
                t
            }
            None => project.program.files[fi].text.clone(),
        };
        run.outcomes[i] = Some(TypedOutcome::Typed { result, source });
    }
    Some(ProjectSummary {
        config: display(opts.cwd, &g.tsconfig),
        program_files: project.program.files.len(),
        linted: files.len(),
        parity: verdict.result,
        detail: verdict.detail,
        cached: verdict.cached,
        load_ms,
        check_ms,
        tsc_ms,
    })
}

/// The parity verdict for a checked project: the cached one while our diagnostics are
/// unchanged, else a fresh comparison with the user's tsc (written to the cache).
fn decide(g: &mut Group, prun: &ProjectRun, pool: &TscPool, ours: &str) -> (ParityResult, f64) {
    let unknown = |detail: String| ParityResult {
        result: Parity::Unknown,
        detail,
        typescript: String::new(),
        cached: false,
        tsc_ms: 0.0,
        ours: ours.to_owned(),
    };
    if parity::mode().as_deref() == Some("off") {
        return (
            unknown("parity check off (TS_R_LINT_PARITY=off)".into()),
            0.0,
        );
    }
    let Some(ts) = g.ts.clone() else {
        return (
            unknown("no TypeScript installed for this project; using ts-r's checker".into()),
            0.0,
        );
    };
    if let Some(c) = &g.cached
        && c.ours == ours
    {
        return (c.clone(), 0.0);
    }
    if !g.queued {
        let incremental = tsconfig_incremental(&RealFs::new(), &g.tsconfig);
        pool.queue(&g.tsconfig, &ts, incremental);
        g.queued = true;
    }
    let (r, ms) = pool.wait(&g.tsconfig);
    match r {
        Ok(theirs) => {
            let mut v = parity::compare(&prun.keys, &theirs, &ts.version);
            v.tsc_ms = ms;
            if let Some(k) = &g.key {
                parity::write_cached(k, &v);
            }
            (v, ms)
        }
        Err(e) => (
            unknown(format!("TypeScript {} could not run: {e}", ts.version)),
            ms,
        ),
    }
}

/// `--fix` with types: check and lint the project, apply every fix, and repeat on the fixed
/// text (up to ESLint's 10 passes). Returns the last project, its run and the fixed texts
/// (`None` for unchanged files), in `lint` order.
fn fix_passes(
    mut project: LoadedProject,
    lint: &[TypedFile],
    libs: &LibStore,
    threads: usize,
    keep: &mut Vec<Arc<Layer>>,
) -> (LoadedProject, ProjectRun, Vec<Option<String>>) {
    let paths: Vec<String> = lint
        .iter()
        .map(|t| project.program.files[t.file].path.clone())
        .collect();
    let mut overlay: FxHashMap<String, String> = FxHashMap::default();
    let mut lint: Vec<TypedFile> = lint.to_vec();
    let mut prun = check_and_lint(&project, &lint, threads);
    // Parity compares with tsc on the files as they are on disk: keep the first pass's keys
    // (later passes check the fixed text, where positions moved).
    let keys = prun.keys.clone();
    for _ in 0..MAX_FIX_PASSES {
        let mut changed = false;
        for (n, r) in prun.results.iter().enumerate() {
            if r.diagnostics.iter().any(|d| d.fatal) {
                continue;
            }
            let fixes: Vec<&crate::diagnostic::Fix> = r
                .diagnostics
                .iter()
                .filter_map(|d| d.fix.as_ref())
                .collect();
            if fixes.is_empty() {
                continue;
            }
            let text = &project.program.files[lint[n].file].text;
            let (next, applied) = apply_fixes(text, &fixes);
            if applied > 0 && next != *text {
                overlay.insert(paths[n].clone(), next);
                changed = true;
            }
        }
        if !changed {
            break;
        }
        let fs = Overlay {
            base: RealFs::new(),
            files: &overlay,
        };
        project = load_project(&fs, &project.config_path, libs, threads);
        remember(keep, &project);
        let mut ok = true;
        for (n, t) in lint.iter_mut().enumerate() {
            match project.file_index(&paths[n]) {
                Some(fi) => t.file = fi,
                None => ok = false,
            }
        }
        if !ok {
            break;
        }
        prun = check_and_lint(&project, &lint, threads);
    }
    let texts = paths.iter().map(|p| overlay.get(p).cloned()).collect();
    prun.keys = keys;
    (project, prun, texts)
}

fn remember(keep: &mut Vec<Arc<Layer>>, project: &LoadedProject) {
    if let Some(p) = &project.parent
        && !keep.iter().any(|k| Arc::ptr_eq(k, p))
    {
        keep.push(p.clone());
    }
}

fn tsconfig_incremental(fs: &RealFs, config: &str) -> bool {
    let parsed = ts_r_config::load_tsconfig(fs, config);
    parsed.options.incremental() == Some(true) || parsed.options.composite() == Some(true)
}

fn display(cwd: &Path, p: &str) -> String {
    match Path::new(p).strip_prefix(cwd) {
        Ok(r) => r.to_string_lossy().replace('\\', "/"),
        Err(_) => p.to_owned(),
    }
}

fn flatten(
    m: BTreeMap<(String, String), UnsupportedReason>,
) -> Vec<(String, String, UnsupportedReason)> {
    m.into_iter().map(|((s, r), why)| (s, r, why)).collect()
}

/// The real file system with some files replaced (fixed text during `--fix`).
struct Overlay<'a> {
    base: RealFs,
    files: &'a FxHashMap<String, String>,
}

impl FileSystem for Overlay<'_> {
    fn read_file(&self, path: &str) -> Option<String> {
        match self.files.get(path) {
            Some(t) => Some(t.clone()),
            None => self.base.read_file(path),
        }
    }
    fn file_exists(&self, path: &str) -> bool {
        self.files.contains_key(path) || self.base.file_exists(path)
    }
    fn directory_exists(&self, path: &str) -> bool {
        self.base.directory_exists(path)
    }
    fn read_dir(&self, path: &str) -> Vec<DirEntry> {
        self.base.read_dir(path)
    }
    fn realpath(&self, path: &str) -> String {
        self.base.realpath(path)
    }
    fn case_sensitive(&self) -> bool {
        self.base.case_sensitive()
    }
    fn cwd(&self) -> String {
        self.base.cwd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_text_keeps_the_parity_keys_of_the_files_on_disk() {
        let Some(libs) = LibStore::locate() else {
            eprintln!("skipped: lib files missing (cargo xtask upstream sync --minimal)");
            return;
        };
        let dir = std::env::temp_dir().join(format!("ts-r-lint-fix-parity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = ts_r_testing::canonical(&dir).unwrap();
        std::fs::write(
            dir.join("tsconfig.json"),
            r#"{"compilerOptions": {"strict": true, "target": "es2022", "noEmit": true}}"#,
        )
        .unwrap();
        let code = "declare const b: boolean;\nconst c = b === true; const e: string = 123;\n\
                    export { c, e };\n";
        std::fs::write(dir.join("a.ts"), code).unwrap();
        let config = dir
            .join("tsconfig.json")
            .to_string_lossy()
            .replace('\\', "/");
        let path = dir.join("a.ts").to_string_lossy().replace('\\', "/");
        let project = load_project(&RealFs::new(), &config, &libs, 1);
        let cfg = crate::typed::file_config_for_test(
            r#"{"@typescript-eslint/no-unnecessary-boolean-literal-compare": "error"}"#,
        )
        .unwrap();
        let lint = [TypedFile {
            file: project.file_index(&path).unwrap(),
            cfg,
        }];
        let on_disk = check_and_lint(&project, &lint, 1).keys;
        assert!(on_disk.keys().any(|k| k.contains("TS2322")), "{on_disk:?}");
        let mut keep = Vec::new();
        let (_, prun, texts) = fix_passes(project, &lint, &libs, 1, &mut keep);
        assert!(
            texts[0]
                .as_deref()
                .is_some_and(|t| t.contains("const c = b;")),
            "{texts:?}"
        );
        assert_eq!(prun.keys, on_disk);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
