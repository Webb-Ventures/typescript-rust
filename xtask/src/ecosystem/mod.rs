//! `cargo xtask ecosystem`: build and check the most downloaded npm packages with ts-r and
//! compare against node, tsc 7 and esbuild. See `ecosystem/README.md`.
//!
//! The driver fetches verified tarballs into the cache, installs dependencies (scripts off)
//! for the bundle and check stages, and runs every (package, stage) pair as a separate
//! worker process under `scripts/limit.sh` (2048 MB / 120 s), at most 4 at a time. Results
//! go to `ecosystem/results/<date>.json`; a full run also rewrites `ecosystem/STATUS.md`.

mod cache;
mod packages;
mod proc;
mod report;
mod repro;
mod shim;
mod tests_stage;
mod worker;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::args::Args;
use crate::util::{Result, read_to_string, root, run_capture, today};
use packages::Package;
use report::{PackageResult, RunFile, StageResult};

/// Stages that run without `--tests`.
const DEFAULT_STAGES: &[&str] = &["parse", "transform", "bundle"];
const STAGE_MEM_MB: u32 = 2048;
const STAGE_TIME_S: u32 = 120;
const MAX_JOBS: usize = 4;

pub fn run(mut args: Args) -> Result<ExitCode> {
    // Sub-commands first (`worker` is internal: one package, one stage).
    match args
        .subcommand(&["refresh", "worker", "report", "repro", "shim", "run"])
        .as_deref()
    {
        Some("refresh") => return packages::refresh(args),
        Some("repro") => return repro::run(args),
        Some("worker") => return worker::run(args),
        Some("report") => return report_cmd(args),
        Some("shim") => return shim::run(args.take_all()),
        _ => {}
    }
    if args.flag("clean") {
        args.finish()?;
        cache::clean()?;
        return Ok(ExitCode::SUCCESS);
    }
    let opts = Options::parse(&mut args)?;
    args.finish()?;
    drive(opts)
}

#[derive(Debug)]
struct Options {
    top: usize,
    only: Vec<String>,
    stages: Vec<String>,
    tests: bool,
    tests_top: usize,
    jobs: usize,
    budget: Duration,
    max_downloads: usize,
    bin_dir: String,
    no_build: bool,
    /// Keep the run's temporary directory (for debugging a failure).
    keep_tmp: bool,
    write_status: bool,
    out: Option<String>,
}

impl Options {
    fn parse(args: &mut Args) -> Result<Options> {
        let top = args.opt("top")?.map_or(Ok(100), |s| s.parse())?;
        let only: Vec<String> = args
            .opt("only")?
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let tests = args.flag("tests");
        let mut stages: Vec<String> = match args.opt("stages")? {
            Some(s) => s
                .split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect(),
            None => DEFAULT_STAGES.iter().map(|s| s.to_string()).collect(),
        };
        for s in &stages {
            if !report::STAGES.contains(&s.as_str()) {
                return Err(format!(
                    "unknown stage `{s}` (stages: {})",
                    report::STAGES.join(", ")
                )
                .into());
            }
        }
        if tests && !stages.iter().any(|s| s == "tests") {
            stages.push("tests".into());
        }
        if stages.iter().any(|s| s == "tests") && !tests {
            return Err("the tests stage is opt-in: pass --tests".into());
        }
        stages.sort_by_key(|s| report::STAGES.iter().position(|x| x == s));
        let jobs: usize = args.opt("jobs")?.map_or(Ok(MAX_JOBS), |s| s.parse())?;
        let budget_s: u64 = args.opt("budget-s")?.map_or(Ok(3600), |s| s.parse())?;
        Ok(Options {
            top,
            only,
            stages,
            tests,
            tests_top: args.opt("tests-top")?.map_or(Ok(5), |s| s.parse())?.min(50),
            jobs: jobs.clamp(1, MAX_JOBS),
            budget: Duration::from_secs(budget_s),
            max_downloads: args.opt("max-downloads")?.map_or(Ok(300), |s| s.parse())?,
            bin_dir: args.opt("bin-dir")?.unwrap_or_else(default_bin_dir),
            no_build: args.flag("no-build"),
            keep_tmp: args.flag("keep-tmp"),
            write_status: !args.flag("no-status"),
            out: args.opt("out")?,
        })
    }

    fn has(&self, stage: &str) -> bool {
        self.stages.iter().any(|s| s == stage)
    }
}

/// Removes the run's temporary directory however the run ends.
struct TempRoot(PathBuf);

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn drive(opts: Options) -> Result<ExitCode> {
    let started = Instant::now();
    let deadline = started + opts.budget;
    let list = packages::load()?;
    let selected: Vec<Package> = list
        .packages
        .iter()
        .filter(|p| p.rank as usize <= opts.top)
        .filter(|p| opts.only.is_empty() || opts.only.contains(&p.name))
        .cloned()
        .collect();
    if selected.is_empty() {
        return Err("no package selected (check --top / --only)".into());
    }
    let worker_exe = prepare_binaries(&opts)?;
    let tools = worker::Tools::new(Path::new(&opts.bin_dir));
    for (what, path) in [("esbuild", &tools.esbuild), ("tsc", &tools.tsc)] {
        if !path.exists() {
            return Err(format!(
                "{what} not found at {}; install the pinned reference tools: (cd bench && ../scripts/limit.sh -- npm ci --ignore-scripts)",
                path.display()
            )
            .into());
        }
    }

    let size = cache::enforce_cap(cache::CACHE_CAP_BYTES);
    let bin_dir = root().join(&opts.bin_dir);
    eprintln!(
        "ecosystem: list of {} ({})",
        list.meta.generated, list.meta.source
    );
    eprintln!(
        "ecosystem: {} packages, stages {}, {} jobs, budget {} s, cache {:.0} MB",
        selected.len(),
        opts.stages.join(","),
        opts.jobs,
        opts.budget.as_secs(),
        size as f64 / 1e6
    );

    // 1. Fetch (bounded number of downloads per run).
    let downloads = AtomicUsize::new(0);
    let fetched: Vec<std::result::Result<PathBuf, String>> =
        packages::parallel(&selected, opts.jobs, |p| {
            let allow = downloads.load(Ordering::Relaxed) < opts.max_downloads;
            match cache::ensure(p, allow) {
                Ok((dir, cache::Fetched::Downloaded)) => {
                    downloads.fetch_add(1, Ordering::Relaxed);
                    Ok(dir)
                }
                Ok((dir, cache::Fetched::Cached)) => Ok(dir),
                Err(e) => Err(e.to_string()),
            }
        });
    eprintln!(
        "ecosystem: fetched ({} downloaded)",
        downloads.load(Ordering::Relaxed)
    );

    // The tests stage runs on the first `--tests-top` packages of the top 50 whose test
    // pipeline uses a tool ts-r can stand in for.
    let tests_set: Vec<String> = if opts.has("tests") {
        selected
            .iter()
            .zip(&fetched)
            .filter(|(p, _)| p.rank <= tests_stage::MAX_RANK)
            .filter(|(_, dir)| {
                dir.as_ref().is_ok_and(|d| {
                    std::fs::read_to_string(d.join("package.json"))
                        .ok()
                        .and_then(|t| serde_json::from_str(&t).ok())
                        .is_some_and(|pj| !tests_stage::swappable(&pj).is_empty())
                })
            })
            .take(opts.tests_top)
            .map(|(p, _)| p.name.clone())
            .collect()
    } else {
        Vec::new()
    };
    if opts.has("tests") {
        eprintln!("ecosystem: tests stage for {}", tests_set.join(", "));
    }

    // 2. Stages.
    let tmp = TempRoot(std::env::temp_dir().join(format!("ts-r-ecosystem-{}", std::process::id())));
    std::fs::create_dir_all(&tmp.0)?;
    let budget_hit = AtomicBool::new(false);
    let done = AtomicUsize::new(0);
    let log = Mutex::new(());
    let jobs: Vec<(Package, std::result::Result<PathBuf, String>)> =
        selected.into_iter().zip(fetched).collect();
    let env = RunEnv {
        opts: &opts,
        worker_exe: &worker_exe,
        tmp_root: &tmp.0,
        deadline,
        budget_hit: &budget_hit,
        types_node: &list.meta.types_node,
        before: format!("{}T23:59:59Z", list.meta.generated),
        tests_set: &tests_set,
        bin_dir: &bin_dir,
        tsc: &tools.tsc,
        lib_dir: tools.lib_dir.as_deref(),
    };
    let total = jobs.len();
    let results: Vec<PackageResult> = packages::parallel(&jobs, opts.jobs, |(p, dir)| {
        let t = Instant::now();
        let r = run_package(&env, p, dir);
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        let _guard = log.lock();
        let parts: Vec<String> = opts
            .stages
            .iter()
            .filter_map(|s| {
                let st = serde_json::to_string(&r.stages.get(s)?.status).ok()?;
                Some(format!("{s} {}", st.trim_matches('"')))
            })
            .collect();
        eprintln!(
            "[{n}/{total}] {}@{} {} ({:.1} s)",
            p.name,
            p.version,
            parts.join(", "),
            t.elapsed().as_secs_f64()
        );
        r
    });
    if opts.keep_tmp {
        eprintln!("kept {}", tmp.0.display());
        std::mem::forget(tmp);
    } else {
        drop(tmp);
    }
    let size = cache::enforce_cap(cache::CACHE_CAP_BYTES);

    // 3. Results and status.
    let run = RunFile {
        date: today(),
        git: git_head(),
        list_generated: list.meta.generated.clone(),
        top: opts.top.min(list.packages.len()),
        stages: opts.stages.clone(),
        seconds: started.elapsed().as_secs_f64(),
        budget_exhausted: budget_hit.load(Ordering::Relaxed),
        tools: tool_versions(&tools),
        packages: results,
    };
    let out = opts.out.clone().map(PathBuf::from).unwrap_or_else(|| {
        root()
            .join("ecosystem/results")
            .join(format!("{}.json", run.date))
    });
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&out, serde_json::to_string_pretty(&run)?)?;
    eprintln!("wrote {}", out.display());
    if opts.write_status && opts.only.is_empty() {
        let status = root().join("ecosystem/STATUS.md");
        std::fs::write(&status, report::status_markdown(&run))?;
        eprintln!("wrote {}", status.display());
    }
    println!("{}", report::summary_line(&run));
    for c in report::group(&run).iter().take(10) {
        println!(
            "  {:>3} pkgs {:>4} items  {:<9} {}  [{}]",
            c.packages.len(),
            c.files,
            c.stage,
            report::truncate(&c.cause, 90),
            report::owner(&c.stage, &c.cause)
        );
    }
    eprintln!(
        "cache: {:.0} MB (cap {} MB)",
        size as f64 / 1e6,
        cache::CACHE_CAP_BYTES / 1024 / 1024
    );
    Ok(ExitCode::SUCCESS)
}

struct RunEnv<'a> {
    opts: &'a Options,
    worker_exe: &'a Path,
    tmp_root: &'a Path,
    deadline: Instant,
    budget_hit: &'a AtomicBool,
    types_node: &'a str,
    before: String,
    /// Packages the tests stage runs on.
    tests_set: &'a [String],
    /// `--bin-dir`, absolute.
    bin_dir: &'a Path,
    /// The reference tsc.
    tsc: &'a Path,
    /// The reference tsc's lib directory.
    lib_dir: Option<&'a Path>,
}

impl RunEnv<'_> {
    fn worker(&self) -> Worker<'_> {
        Worker {
            exe: self.worker_exe,
            bin_dir: &self.opts.bin_dir,
        }
    }

    fn out_of_time(&self) -> bool {
        if Instant::now() >= self.deadline {
            self.budget_hit.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
}

fn run_package(
    env: &RunEnv<'_>,
    p: &Package,
    dir: &std::result::Result<PathBuf, String>,
) -> PackageResult {
    let mut stages: BTreeMap<String, StageResult> = BTreeMap::new();
    let result = |stages: BTreeMap<String, StageResult>| PackageResult {
        rank: p.rank,
        name: p.name.clone(),
        version: p.version.clone(),
        stages,
    };
    let pkg_dir = match dir {
        Ok(d) => d.clone(),
        Err(e) => {
            for s in &env.opts.stages {
                stages.insert(s.clone(), StageResult::skip(format!("not fetched: {e}")));
            }
            return result(stages);
        }
    };
    let tmp = env.tmp_root.join(p.slug());
    let _ = std::fs::create_dir_all(&tmp);

    for stage in ["parse", "transform"] {
        if env.opts.has(stage) && !env.out_of_time() {
            stages.insert(
                stage.into(),
                run_worker(env.worker(), &p.name, stage, &pkg_dir, None, &tmp),
            );
        }
    }
    if (env.opts.has("bundle") || env.opts.has("check")) && !env.out_of_time() {
        let install = tmp.join("install");
        match npm_install(env, p, &install) {
            Ok(()) => {
                for stage in ["bundle", "check"] {
                    if env.opts.has(stage) && !env.out_of_time() {
                        stages.insert(
                            stage.into(),
                            run_worker(
                                env.worker(),
                                &p.name,
                                stage,
                                &pkg_dir,
                                Some(&install),
                                &tmp,
                            ),
                        );
                    }
                }
            }
            Err(e) => {
                for stage in ["bundle", "check"] {
                    if env.opts.has(stage) {
                        stages.insert(
                            stage.into(),
                            StageResult::skip(format!("npm install failed: {e}")),
                        );
                    }
                }
            }
        }
    }
    if env.opts.tests && env.opts.has("tests") && !env.out_of_time() {
        let r = if env.tests_set.contains(&p.name) {
            tests_stage::run(&tests_stage::Env {
                pkg: p,
                tmp: &tmp,
                bin_dir: env.bin_dir,
                worker_exe: env.worker_exe,
                tsc: env.tsc,
                lib_dir: env.lib_dir,
                before: &env.before,
            })
        } else if p.rank > tests_stage::MAX_RANK {
            StageResult::skip(format!(
                "tests run on the top {} only",
                tests_stage::MAX_RANK
            ))
        } else {
            StageResult::skip(format!(
                "not among the first {} packages whose tests use tsc/tshy/tsx/ts-node/esbuild",
                env.opts.tests_top
            ))
        };
        stages.insert("tests".into(), r);
    }
    if !env.opts.keep_tmp {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    result(stages)
}

fn limit_sh() -> PathBuf {
    root().join("scripts/limit.sh")
}

/// How to start a stage worker.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Worker<'a> {
    pub exe: &'a Path,
    pub bin_dir: &'a str,
}

/// Runs one stage for one package in a worker process under limit.sh.
pub(crate) fn run_worker(
    w: Worker<'_>,
    name: &str,
    stage: &str,
    pkg_dir: &Path,
    install: Option<&Path>,
    tmp: &Path,
) -> StageResult {
    let out = tmp.join(format!("{stage}.json"));
    let work = tmp.join(format!("work-{stage}"));
    let mut cmd = Command::new(limit_sh());
    cmd.arg("--mem-mb")
        .arg(STAGE_MEM_MB.to_string())
        .arg("--time-s")
        .arg(STAGE_TIME_S.to_string())
        .arg("--")
        .arg(w.exe)
        .args(["ecosystem", "worker", stage, "--name", name])
        .arg("--pkg-dir")
        .arg(pkg_dir)
        .arg("--tmp")
        .arg(&work)
        .arg("--out")
        .arg(&out)
        .arg("--bin-dir")
        .arg(w.bin_dir)
        .env("TS_R_MEM_CAP_MB", STAGE_MEM_MB.to_string())
        .current_dir(root());
    if let Some(i) = install {
        cmd.arg("--install-dir").arg(i);
    }
    let o = proc::run(&mut cmd, Duration::from_secs(u64::from(STAGE_TIME_S) + 30));
    let _ = std::fs::remove_dir_all(&work);
    let last_at = o
        .stderr
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("at "))
        .unwrap_or("")
        .to_string();
    if o.ok()
        && let Ok(text) = std::fs::read_to_string(&out)
        && let Ok(r) = serde_json::from_str::<StageResult>(&text)
    {
        return r;
    }
    match o.code {
        Some(137) => StageResult::error(
            &format!("limit: memory over {STAGE_MEM_MB} MB"),
            &last_at,
            "",
        ),
        Some(124) => {
            StageResult::error(&format!("limit: time over {STAGE_TIME_S} s"), &last_at, "")
        }
        _ if o.timed_out => {
            StageResult::error(&format!("limit: time over {STAGE_TIME_S} s"), &last_at, "")
        }
        _ => {
            let panic = o
                .stderr
                .lines()
                .find(|l| l.contains("panicked at"))
                .map(String::from);
            let abort = o.stderr.contains("memory cap exceeded");
            let (cause, detail) = match (panic, abort) {
                (_, true) => (
                    format!("limit: memory over {STAGE_MEM_MB} MB"),
                    String::new(),
                ),
                (Some(line), _) => {
                    let msg = o
                        .stderr
                        .lines()
                        .skip_while(|l| !l.contains("panicked at"))
                        .nth(1)
                        .unwrap_or("")
                        .to_string();
                    (
                        format!("crash: {}", panic_site(&line)),
                        format!("{line} {msg}"),
                    )
                }
                (None, false) => (
                    format!("crash: exit {:?}", o.code),
                    report::truncate(&o.stderr, 400),
                ),
            };
            StageResult::error(&cause, &last_at, &detail)
        }
    }
}

/// `thread 'main' panicked at crates/ts_r_parser/src/expr.rs:12:5:` → `panicked at crates/ts_r_parser/src/expr.rs`.
fn panic_site(line: &str) -> String {
    let after = line.split_once("panicked at ").map_or(line, |(_, r)| r);
    let file = after.split(':').next().unwrap_or(after);
    format!("panicked at {file}")
}

fn npm_install(env: &RunEnv<'_>, p: &Package, dir: &Path) -> std::result::Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tgz = cache::tarball_path(p);
    let mut deps = serde_json::Map::new();
    deps.insert(p.name.clone(), format!("file:{}", tgz.display()).into());
    if p.name != "@types/node" {
        deps.insert("@types/node".into(), env.types_node.into());
    }
    let pj = serde_json::json!({ "name": "ts-r-ecosystem-probe", "private": true, "dependencies": deps });
    std::fs::write(dir.join("package.json"), pj.to_string()).map_err(|e| e.to_string())?;
    let attempt = |legacy: bool| {
        let mut cmd = Command::new(limit_sh());
        cmd.args(["--mem-mb", "2048", "--time-s", "240", "--"])
            .arg(crate::util::npm_tool("npm"))
            .args([
                "install",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--no-package-lock",
                "--prefer-offline",
                "--loglevel=error",
            ])
            .arg(format!("--before={}", env.before))
            .env("npm_config_cache", cache::npm_cache())
            .env("npm_config_update_notifier", "false")
            .current_dir(dir);
        if legacy {
            cmd.arg("--legacy-peer-deps");
        }
        proc::run(&mut cmd, Duration::from_secs(270))
    };
    let mut o = attempt(false);
    if !o.ok() && (o.stderr.contains("ERESOLVE") || o.stderr.contains("peer")) {
        o = attempt(true);
    }
    if o.ok()
        && dir
            .join("node_modules")
            .join(&p.name)
            .join("package.json")
            .is_file()
    {
        Ok(())
    } else {
        let line = o
            .stderr
            .lines()
            .find(|l| l.contains("ERR!") && !l.trim_end().ends_with("ERR!"))
            .map(String::from)
            .unwrap_or_else(|| o.first_line());
        Err(report::truncate(line.trim(), 200))
    }
}

/// Builds the release worker (xtask) and the ts-r binaries the stages need. Returns the
/// worker executable: always a release build, because parsing real packages in a debug
/// build would hit the time limit.
fn prepare_binaries(opts: &Options) -> Result<PathBuf> {
    build_binaries(opts.no_build, &opts.stages)
}

/// Cargo's target directory for builds started from the repo root: `CARGO_TARGET_DIR` when
/// it is set and not empty (a relative value resolves against the repo root, as cargo does
/// for a build run there), otherwise `<root>/target`.
fn target_dir_in(root: &Path, env: Option<std::ffi::OsString>) -> PathBuf {
    match env.filter(|v| !v.is_empty()) {
        Some(v) => {
            let p = PathBuf::from(v);
            if p.is_absolute() { p } else { root.join(p) }
        }
        None => root.join("target"),
    }
}

/// Where `cargo build --release` puts the worker and the ts-r binaries.
pub(crate) fn release_dir() -> PathBuf {
    target_dir_in(&root(), std::env::var_os("CARGO_TARGET_DIR")).join("release")
}

/// The default `--bin-dir`: the release directory of the cargo target dir.
pub(crate) fn default_bin_dir() -> String {
    release_dir().to_string_lossy().into_owned()
}

/// Builds the release worker plus the ts-r binaries `stages` need (unless `no_build`).
pub(crate) fn build_binaries(no_build: bool, stages: &[String]) -> Result<PathBuf> {
    let has = |s: &str| stages.iter().any(|x| x == s);
    let release_xtask = release_dir().join(format!("xtask{}", std::env::consts::EXE_SUFFIX));
    if no_build {
        if !release_xtask.is_file() {
            return Err(format!("--no-build: {} is missing", release_xtask.display()).into());
        }
        return Ok(release_xtask);
    }
    let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.args(["build", "--release", "-j", "4", "--quiet", "-p", "xtask"]);
    if has("bundle") || has("tests") {
        cmd.args(["-p", "ts_r_build"]);
    }
    if has("check") || has("tests") {
        cmd.args(["-p", "ts_r_check"]);
    }
    if has("tests") {
        cmd.args(["-p", "ts_r_run"]);
    }
    cmd.current_dir(root());
    eprintln!("ecosystem: building release binaries");
    crate::util::run_status(&mut cmd)?;
    Ok(release_xtask)
}

fn git_head() -> String {
    run_capture(
        Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(root()),
    )
    .map(|s| s.trim().to_string())
    .unwrap_or_default()
}

fn tool_versions(t: &worker::Tools) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    let v = |cmd: &mut Command| {
        proc::run(cmd, Duration::from_secs(20))
            .stdout
            .trim()
            .trim_start_matches("Version ")
            .to_string()
    };
    m.insert("node".into(), v(Command::new(&t.node).arg("--version")));
    m.insert(
        "esbuild".into(),
        v(Command::new(&t.esbuild).arg("--version")),
    );
    m.insert("tsc".into(), v(Command::new(&t.tsc).arg("--version")));
    m
}

/// `cargo xtask ecosystem report [<results.json>]`: rewrite STATUS.md from a results file
/// (default: the newest in `ecosystem/results/`).
fn report_cmd(mut args: Args) -> Result<ExitCode> {
    let file = args.positional();
    args.finish()?;
    let path = match file {
        Some(f) => PathBuf::from(f),
        None => {
            let dir = root().join("ecosystem/results");
            let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
                .map_err(|e| format!("{}: {e}", dir.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e == "json"))
                .collect();
            files.sort();
            files.pop().ok_or("no results in ecosystem/results")?
        }
    };
    let run: RunFile = serde_json::from_str(&read_to_string(&path)?)?;
    let status = root().join("ecosystem/STATUS.md");
    std::fs::write(&status, report::status_markdown(&run))?;
    println!("{}", report::summary_line(&run));
    eprintln!("wrote {}", status.display());
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_dir_follows_cargo_target_dir() {
        let root = Path::new("/repo");
        assert_eq!(target_dir_in(root, None), Path::new("/repo/target"));
        assert_eq!(
            target_dir_in(root, Some("".into())),
            Path::new("/repo/target")
        );
        assert_eq!(
            target_dir_in(root, Some("build/out".into())),
            Path::new("/repo/build/out")
        );
        let abs = std::env::temp_dir().join("ts-r-target");
        assert_eq!(target_dir_in(root, Some(abs.clone().into())), abs);
    }

    #[test]
    fn options() {
        let mut a = Args::new(vec![
            "--stages".into(),
            "bundle,parse".into(),
            "--jobs".into(),
            "16".into(),
        ]);
        let o = Options::parse(&mut a).unwrap();
        assert_eq!(o.stages, vec!["parse", "bundle"]);
        assert_eq!(o.jobs, 4);
        let mut a = Args::new(vec!["--stages".into(), "tests".into()]);
        assert!(Options::parse(&mut a).is_err());
        let mut a = Args::new(vec!["--stages".into(), "nope".into()]);
        assert!(Options::parse(&mut a).is_err());
    }

    #[test]
    fn panic_sites() {
        assert_eq!(
            panic_site("thread 'main' panicked at crates/ts_r_parser/src/expr.rs:12:5:"),
            "panicked at crates/ts_r_parser/src/expr.rs"
        );
    }
}
