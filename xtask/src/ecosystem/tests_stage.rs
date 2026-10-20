//! The opt-in tests stage (`--tests`): a package's own test suite at its git tag, run twice —
//! once with its own tooling (the baseline) and once with its TypeScript / bundling tools
//! swapped for ts-r — under `scripts/limit.sh` with the network blocked where the OS allows it.
//!
//! Swaps, all inside the ts-r copy's `node_modules`:
//! - `tsc` (`.bin/tsc` and `typescript/bin/tsc`, which tshy runs): `xtask ecosystem shim tsc`
//!   (ts-r-check plus ts-r's transform; see `shim.rs`).
//! - `tsx`, `ts-node` binaries: `ts-r-run`.
//! - ts-node / tsx / tap's TypeScript loader entry points: `ecosystem/js/ts-r-hooks.cjs`
//!   (compiles each TypeScript file with `ts-r-run --compile`).
//! - `esbuild` binary: `ts-r-build` (the JS API is not swapped).
//!
//! A package passes when both runs pass and at least one swapped tool ran; when the baseline
//! fails the stage is skipped (the suite needs something this machine lacks).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::packages::Package;
use super::report::{self, StageResult};
use super::{cache, limit_sh, proc, shim};
use crate::util::root;

/// Per test run (the baseline and the ts-r run each get this much).
const TEST_TIME_S: u32 = 120;
const TEST_MEM_MB: u32 = 2048;
const INSTALL_TIME_S: u32 = 240;
/// Only the top 50 packages are candidates.
pub const MAX_RANK: u32 = 50;

/// The tools the stage knows how to swap.
const SWAPPABLE: &[&str] = &["tsc", "tshy", "tsx", "ts-node", "esbuild"];
/// Test-script steps that are linting or formatting, not tests.
const LINTERS: &[&str] = &[
    "xo", "eslint", "prettier", "standard", "oxlint", "biome", "tslint", "dprint", "attw",
    "publint",
];
/// Dependencies that mean the suite needs a service or a browser.
const SERVICES: &[&str] = &[
    "mongodb",
    "mongoose",
    "redis",
    "ioredis",
    "pg",
    "mysql",
    "mysql2",
    "testcontainers",
    "puppeteer",
    "playwright",
    "@playwright/test",
    "karma",
    "selenium-webdriver",
    "webdriverio",
    "cypress",
];

pub struct Env<'a> {
    pub pkg: &'a Package,
    pub tmp: &'a Path,
    /// Directory with ts-r-run, ts-r-check and ts-r-build (absolute).
    pub bin_dir: &'a Path,
    /// The release xtask (runs the tsc shim).
    pub worker_exe: &'a Path,
    /// The reference tsc 7 (declarations only).
    pub tsc: &'a Path,
    /// The reference tsc's lib directory (`TS_R_LIB_DIR` for ts-r-check).
    pub lib_dir: Option<&'a Path>,
    /// `--before` for installs without a lockfile.
    pub before: &'a str,
}

/// Swappable tools a package's test pipeline mentions (from its package.json scripts).
pub fn swappable(pkg_json: &Value) -> Vec<&'static str> {
    let text = pipeline_text(pkg_json);
    let words = words(&text);
    SWAPPABLE
        .iter()
        .copied()
        .filter(|t| {
            words.iter().any(|w| {
                w == t || w.starts_with(&format!("{t}/")) || w.ends_with(&format!("/.bin/{t}"))
            })
        })
        .collect()
}

fn scripts(pkg_json: &Value) -> BTreeMap<String, String> {
    pkg_json
        .get("scripts")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| {
        c.is_whitespace() || matches!(c, '=' | ';' | '&' | '|' | '(' | ')' | '"' | '\'')
    })
    .filter(|w| !w.is_empty())
    .map(String::from)
    .collect()
}

/// The text of every script the test pipeline runs: pretest, test, posttest and what they
/// call through `npm run`, `run-s` / `run-p` / `npm-run-all` (with `:*` globs), plus
/// prepare/build when the pipeline doesn't run them.
fn pipeline_text(pkg_json: &Value) -> String {
    let s = scripts(pkg_json);
    let mut seen: Vec<String> = Vec::new();
    let mut queue: Vec<String> = vec!["pretest".into(), "test".into(), "posttest".into()];
    if let Some(pre) = build_step(&s) {
        queue.push(pre.trim_start_matches("npm run ").to_string());
    }
    let mut out = String::new();
    while let Some(name) = queue.pop() {
        if seen.contains(&name) || seen.len() > 64 {
            continue;
        }
        seen.push(name.clone());
        let Some(body) = s.get(&name) else { continue };
        out.push_str(body);
        out.push('\n');
        let w = words(body);
        for (i, word) in w.iter().enumerate() {
            let next = w.get(i + 1).map(String::as_str);
            match word.as_str() {
                "run" | "run-script"
                    if i > 0 && matches!(w[i - 1].as_str(), "npm" | "pnpm" | "yarn") =>
                {
                    if let Some(n) = next {
                        queue.push(n.to_string());
                    }
                }
                "run-s" | "run-p" | "npm-run-all" => {
                    for arg in w[i + 1..].iter().take_while(|a| !a.contains("&&")) {
                        if arg.starts_with('-') {
                            continue;
                        }
                        if let Some(prefix) = arg.strip_suffix('*') {
                            queue.extend(s.keys().filter(|k| k.starts_with(prefix)).cloned());
                        } else {
                            queue.push(arg.clone());
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// `npm run prepare` / `npm run build` when the test pipeline needs a build that `npm
/// install --ignore-scripts` skipped and the scripts don't run it themselves.
fn build_step(s: &BTreeMap<String, String>) -> Option<String> {
    let pipeline: String = ["pretest", "test"]
        .iter()
        .filter_map(|k| s.get(*k))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let w = words(&pipeline);
    let runs = |name: &str| w.iter().any(|x| x == name);
    if runs("prepare") || runs("build") {
        return None;
    }
    if s.contains_key("prepare") {
        Some("npm run prepare".into())
    } else if s.contains_key("build") {
        Some("npm run build".into())
    } else {
        None
    }
}

/// Drops lint and format steps from a `&&` chain.
fn without_linters(script: &str) -> String {
    script
        .split("&&")
        .map(str::trim)
        .filter(|seg| {
            let w = words(seg);
            let mut it = w.iter().map(String::as_str).skip_while(|x| *x == "npx");
            match it.next() {
                Some(first) if LINTERS.contains(&first) => false,
                Some("npm" | "pnpm" | "yarn") => {
                    let rest: Vec<&str> = it.collect();
                    let target = match rest.first() {
                        Some(&"run") | Some(&"run-script") => rest.get(1).copied(),
                        other => other.copied(),
                    }
                    .unwrap_or("");
                    !["lint", "format", "prettier", "eslint", "fmt", "xo"]
                        .iter()
                        .any(|p| target.starts_with(p))
                }
                _ => true,
            }
        })
        .collect::<Vec<_>>()
        .join(" && ")
}

/// `owner/repo` for a GitHub repository URL from package.json.
pub fn github_repo(url: &str) -> Option<String> {
    let rest = url
        .split_once("github.com")
        .map(|(_, r)| r.trim_start_matches([':', '/']))
        .or_else(|| url.strip_prefix("github:"))
        .or_else(|| {
            // Bare `owner/repo` shorthand.
            (url.matches('/').count() == 1 && !url.contains(':')).then_some(url)
        })?;
    let mut parts = rest.split('/');
    let owner = parts.next()?.to_string();
    let repo = parts
        .next()?
        .split('#')
        .next()?
        .trim_end_matches(".git")
        .to_string();
    (!owner.is_empty() && !repo.is_empty()).then(|| format!("{owner}/{repo}"))
}

fn tag_candidates(p: &Package) -> Vec<String> {
    let bare = p.name.rsplit('/').next().unwrap_or(&p.name);
    let mut v = vec![
        format!("v{}", p.version),
        p.version.clone(),
        format!("{}@{}", p.name, p.version),
        format!("{bare}@{}", p.version),
        format!("{bare}-v{}", p.version),
    ];
    v.dedup();
    v
}

/// Downloads (once) the repository's source tarball at the version's tag into the cache.
fn fetch_source(p: &Package) -> Result<PathBuf, String> {
    let repo = github_repo(&p.repo).ok_or_else(|| format!("source not on GitHub ({})", p.repo))?;
    let dest = cache::dir().join("repos").join(format!("{}.tgz", p.slug()));
    if dest.is_file() {
        return Ok(dest);
    }
    std::fs::create_dir_all(dest.parent().unwrap_or(Path::new("."))).map_err(|e| e.to_string())?;
    let part = dest.with_extension("tgz.part");
    for tag in tag_candidates(p) {
        let url = format!("https://codeload.github.com/{repo}/tar.gz/refs/tags/{tag}");
        let o = proc::run(
            Command::new("curl")
                .args([
                    "-fsSL",
                    "--retry",
                    "2",
                    "--max-time",
                    "120",
                    "--max-filesize",
                    "268435456",
                    "-o",
                ])
                .arg(&part)
                .arg(&url),
            Duration::from_secs(150),
        );
        if o.ok() {
            std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
            return Ok(dest);
        }
    }
    let _ = std::fs::remove_file(&part);
    Err(format!(
        "no tag {} in github.com/{repo}",
        tag_candidates(p).join(" / ")
    ))
}

fn unpack(tgz: &Path, dest: &Path) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(dest);
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let o = proc::run(
        Command::new("tar")
            .args(["-xzf"])
            .arg(tgz)
            .args(["--strip-components", "1", "--no-same-owner", "-C"])
            .arg(dest),
        Duration::from_secs(120),
    );
    if o.ok() {
        Ok(())
    } else {
        Err(format!("tar: {}", o.first_line()))
    }
}

/// The command prefix that blocks the network (localhost stays open), if the OS has one.
fn network_jail() -> (Vec<String>, &'static str) {
    if cfg!(target_os = "macos") && Path::new("/usr/bin/sandbox-exec").exists() {
        let profile = "(version 1)(allow default)(deny network-outbound (remote ip))\
                       (allow network-outbound (remote ip \"localhost:*\"))";
        return (
            vec!["/usr/bin/sandbox-exec".into(), "-p".into(), profile.into()],
            "sandbox-exec",
        );
    }
    if cfg!(target_os = "linux")
        && Command::new("unshare")
            .args(["-rn", "true"])
            .status()
            .is_ok_and(|s| s.success())
    {
        return (vec!["unshare".into(), "-rn".into()], "unshare");
    }
    (Vec::new(), "open")
}

/// `npm <args>` in `dir` under limit.sh (and `jail`), with `extra` environment variables.
fn npm(
    dir: &Path,
    args: &[&str],
    time: u32,
    jail: &[String],
    extra: &[(&str, &std::ffi::OsStr)],
) -> proc::Output {
    let mut cmd = Command::new(limit_sh());
    cmd.envs(extra.iter().copied());
    let mem = TEST_MEM_MB;
    cmd.arg("--mem-mb")
        .arg(mem.to_string())
        .arg("--time-s")
        .arg(time.to_string())
        .arg("--")
        .args(jail)
        .arg(crate::util::npm_tool("npm"))
        .args(args)
        .env("npm_config_cache", cache::npm_cache())
        .env("npm_config_update_notifier", "false")
        .env("npm_config_fund", "false")
        .env("npm_config_audit", "false")
        .env("CI", "true")
        .env_remove("NO_COLOR")
        .env_remove("FORCE_COLOR")
        .env("TAP_COLORS", "0")
        .env("TAP_JOBS", "1")
        .current_dir(dir);
    proc::run(&mut cmd, Duration::from_secs(u64::from(time) + 30))
}

fn install(env: &Env<'_>, dir: &Path) -> Result<(), String> {
    let lock = dir.join("package-lock.json").is_file() || dir.join("npm-shrinkwrap.json").is_file();
    let before = format!("--before={}", env.before);
    let base: Vec<&str> = if lock {
        vec!["ci", "--ignore-scripts", "--loglevel=error"]
    } else {
        vec![
            "install",
            "--ignore-scripts",
            "--no-package-lock",
            "--loglevel=error",
            &before,
        ]
    };
    let mut o = npm(dir, &base, INSTALL_TIME_S, &[], &[]);
    if !o.ok()
        && (o.stderr.contains("ERESOLVE")
            || o.stderr.contains("peer")
            || o.stderr.contains("EUSAGE"))
    {
        // A lockfile npm ci rejects (old format, out of sync): fall back to a plain install.
        let mut args = vec![
            "install",
            "--ignore-scripts",
            "--no-package-lock",
            "--loglevel=error",
            "--legacy-peer-deps",
        ];
        if !lock {
            args.push(&before);
        }
        o = npm(dir, &args, INSTALL_TIME_S, &[], &[]);
    }
    if o.ok() {
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

/// Rewrites the scripts: lint steps dropped, the skipped install-time build added.
fn prepare_scripts(dir: &Path) -> Result<String, String> {
    let path = dir.join("package.json");
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut pj: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let s = scripts(&pj);
    let test = without_linters(s.get("test").map_or("", String::as_str));
    if test.trim().is_empty() {
        return Err("test script is lint only".into());
    }
    let test = match build_step(&s) {
        Some(pre) => format!("{pre} && {test}"),
        None => test,
    };
    let obj = pj
        .get_mut("scripts")
        .and_then(Value::as_object_mut)
        .ok_or("no scripts")?;
    obj.insert("test".into(), Value::String(test.clone()));
    for hook in ["pretest", "posttest"] {
        if let Some(body) = s.get(hook) {
            let kept = without_linters(body);
            if kept.trim().is_empty() {
                obj.remove(hook);
            } else {
                obj.insert(hook.into(), Value::String(kept));
            }
        }
    }
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&pj).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(test)
}

fn write_exe(path: &Path, body: &str) -> std::io::Result<()> {
    if path.symlink_metadata().is_ok() {
        std::fs::remove_file(path)?;
    }
    std::fs::write(path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn sh_quote(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', r"'\''"))
}

/// Installs the swaps into `dir/node_modules`. Returns what was swapped.
fn install_swaps(env: &Env<'_>, dir: &Path) -> std::io::Result<Vec<String>> {
    let nm = dir.join("node_modules");
    let bin = nm.join(".bin");
    let mut swapped = Vec::new();
    let tsc_sh = format!(
        "#!/bin/sh\nexec {} ecosystem shim tsc \"$@\"\n",
        sh_quote(env.worker_exe)
    );
    let logged = |tool: &str, exe: &Path| {
        format!(
            "#!/bin/sh\n{} \"$@\"\ncode=$?\nprintf '{tool}\\t%s\\t%s\\n' \"$code\" \"$*\" >> \"$TS_R_ECO_SHIM_LOG\"\nexit $code\n",
            sh_quote(exe)
        )
    };
    let run = env.bin_dir.join("ts-r-run");
    let build = env.bin_dir.join("ts-r-build");
    if let Ok(rd) = std::fs::read_dir(&bin) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let body = match name.as_str() {
                "tsc" => tsc_sh.clone(),
                "tsx" => logged("tsx", &run),
                n if n.starts_with("ts-node") => logged("ts-node", &run),
                "esbuild" => logged("esbuild", &build),
                _ => continue,
            };
            write_exe(&e.path(), &body)?;
            swapped.push(format!(".bin/{name}"));
        }
    }
    // tshy and others run `node node_modules/typescript/bin/tsc`.
    let ts_bin = nm.join("typescript/bin/tsc");
    if ts_bin.is_file() {
        let js = format!(
            "#!/usr/bin/env node\nconst r = require('node:child_process').spawnSync({}, ['ecosystem', 'shim', 'tsc', ...process.argv.slice(2)], {{ stdio: 'inherit' }});\nprocess.exit(r.status ?? 1);\n",
            serde_json::to_string(&env.worker_exe.to_string_lossy()).unwrap_or_default()
        );
        write_exe(&ts_bin, &js)?;
        swapped.push("typescript/bin/tsc".into());
    }
    // Loader entry points: every `exports` / `main` target of ts-node (and tap's fork of it)
    // and tsx becomes a stub that registers ts-r's hooks.
    let hooks = root().join("ecosystem/js/ts-r-hooks.cjs");
    let hooks_js = serde_json::to_string(&hooks.to_string_lossy()).unwrap_or_default();
    for pkg in ["ts-node", "@isaacs/ts-node-temp-fork-for-pr-2009", "tsx"] {
        let pdir = nm.join(pkg);
        let Ok(text) = std::fs::read_to_string(pdir.join("package.json")) else {
            continue;
        };
        let Ok(pj) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let esm_pkg = pj.get("type").and_then(Value::as_str) == Some("module");
        let mut targets = Vec::new();
        collect_targets(pj.get("exports").unwrap_or(&Value::Null), "", &mut targets);
        if let Some(m) = pj.get("main").and_then(Value::as_str) {
            targets.push(m.to_string());
        }
        targets.sort();
        targets.dedup();
        let mut n = 0;
        for t in targets {
            let t = t.trim_start_matches("./");
            if t.contains("..") || t.ends_with(".d.ts") || t.ends_with(".json") || t.contains('*') {
                continue;
            }
            let f = pdir.join(t);
            if !f.is_file() {
                continue;
            }
            let esm = t.ends_with(".mjs") || (t.ends_with(".js") && esm_pkg);
            let body = if esm {
                format!("import {hooks_js};\n")
            } else {
                format!("require({hooks_js});\n")
            };
            std::fs::write(&f, body)?;
            n += 1;
        }
        if n > 0 {
            swapped.push(format!("{pkg} loader ({n} entry files)"));
        }
    }
    Ok(swapped)
}

/// Leaf file targets of an `exports` value (skipping the `./package.json` subpath).
fn collect_targets(v: &Value, key: &str, out: &mut Vec<String>) {
    match v {
        Value::String(s) if key != "./package.json" => out.push(s.clone()),
        Value::Object(m) => {
            for (k, x) in m {
                let key = if k.starts_with('.') { k.as_str() } else { key };
                collect_targets(x, key, out);
            }
        }
        Value::Array(a) => {
            for x in a {
                collect_targets(x, key, out);
            }
        }
        _ => {}
    }
}

/// The first line of test output that looks like a failure.
fn failure_line(o: &proc::Output) -> String {
    let markers = [
        "not ok",
        "✖",
        "✗",
        "✘",
        "FAIL",
        "AssertionError",
        "Error:",
        "failing",
        "ERR!",
    ];
    for text in [&o.stdout, &o.stderr] {
        if let Some(l) = text
            .lines()
            .map(str::trim)
            .find(|l| markers.iter().any(|m| l.contains(m)) && !l.starts_with("# fail 0"))
        {
            return report::truncate(l, 300);
        }
    }
    o.first_line()
}

struct ShimEvent {
    tool: String,
    code: i32,
    detail: String,
}

fn read_log(path: &Path) -> Vec<ShimEvent> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut p = l.splitn(3, '\t');
            Some(ShimEvent {
                tool: p.next()?.to_string(),
                code: p.next()?.parse().ok()?,
                detail: p.next().unwrap_or("").to_string(),
            })
        })
        .collect()
}

/// The cause for a failing ts-r run: the first swapped tool that failed, else the tests.
fn tsr_cause(events: &[ShimEvent], tools: &str, o: &proc::Output) -> (String, String) {
    if let Some(e) = events.iter().find(|e| e.code != 0) {
        let cause = match e.tool.as_str() {
            "tsc" if e.detail.starts_with("check:") => {
                let code = e
                    .detail
                    .split_whitespace()
                    .find(|w| w.starts_with("TS") && w.len() > 2)
                    .unwrap_or("TS?")
                    .trim_end_matches(':');
                format!("tsc → ts-r-check: errors tsc does not report ({code})")
            }
            "tsc" => format!("tsc → ts-r shim: {}", report::signature(&e.detail)),
            "ts-r-run" => "loader → ts-r-run --compile failed".to_string(),
            "esbuild" => "esbuild → ts-r-build failed".to_string(),
            t => format!("{t} → ts-r-run failed"),
        };
        return (cause, e.detail.clone());
    }
    if o.timed_out || o.code == Some(124) {
        return (format!("tests time out with ts-r ({tools})"), String::new());
    }
    if o.code == Some(137) {
        return (
            format!("tests over {TEST_MEM_MB} MB with ts-r ({tools})"),
            String::new(),
        );
    }
    let all_pass =
        !o.stdout.contains("not ok") && !o.stdout.contains("✖") && !o.stdout.contains("# fail ");
    if all_pass && o.stdout.contains("% Stmts") {
        return (
            format!("coverage below the package's threshold with ts-r ({tools})"),
            failure_line(o),
        );
    }
    (format!("tests fail with ts-r ({tools})"), failure_line(o))
}

pub fn run(env: &Env<'_>) -> StageResult {
    let started = Instant::now();
    let mut r = run_inner(env);
    r.ms = started.elapsed().as_millis() as u64;
    r
}

fn run_inner(env: &Env<'_>) -> StageResult {
    let p = env.pkg;
    let tgz = match fetch_source(p) {
        Ok(t) => t,
        Err(e) => return StageResult::skip(e),
    };
    let dir = env.tmp.join("tests");
    let base = dir.join("base");
    let tsr = dir.join("ts-r");
    for d in [&base, &tsr] {
        if let Err(e) = unpack(&tgz, d) {
            return StageResult::error("setup: cannot unpack the source", "", &e);
        }
    }
    let pj: Value = match std::fs::read_to_string(base.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
    {
        Some(v) => v,
        None => return StageResult::skip("no package.json at the repository root"),
    };
    if pj.get("name").and_then(Value::as_str) != Some(p.name.as_str())
        || pj.get("workspaces").is_some()
    {
        return StageResult::skip("monorepo: workspace installs are not supported yet");
    }
    let deps: Vec<String> = ["dependencies", "devDependencies"]
        .iter()
        .filter_map(|k| pj.get(*k).and_then(Value::as_object))
        .flat_map(|m| m.keys().cloned())
        .collect();
    if let Some(s) = deps.iter().find(|d| SERVICES.contains(&d.as_str())) {
        return StageResult::skip(format!("needs a service or browser ({s})"));
    }
    let tools = swappable(&pj);
    let test_cmd = match prepare_scripts(&base).and_then(|t| prepare_scripts(&tsr).map(|_| t)) {
        Ok(t) => t,
        Err(e) => return StageResult::skip(e),
    };
    for d in [&base, &tsr] {
        if let Err(e) = install(env, d) {
            return StageResult::skip(format!("npm install failed: {e}"));
        }
    }
    let (jail, network) = network_jail();
    let mut r = StageResult::default();
    r.metrics.insert(
        "network_blocked".into(),
        f64::from(u8::from(network != "open")),
    );

    // Baseline: the package's own tools.
    let t = Instant::now();
    let b = npm(&base, &["test"], TEST_TIME_S, &jail, &[]);
    r.metrics
        .insert("baseline_ms".into(), t.elapsed().as_millis() as f64);
    if !b.ok() {
        let why = if b.timed_out || b.code == Some(124) {
            format!("over {TEST_TIME_S} s")
        } else {
            failure_line(&b)
        };
        return StageResult::skip(format!("suite fails with its own tooling: {why}"));
    }

    // ts-r.
    let swapped = match install_swaps(env, &tsr) {
        Ok(s) => s,
        Err(e) => return StageResult::error("setup: cannot install the swaps", "", &e.to_string()),
    };
    let log = dir.join("shim.log");
    let _ = std::fs::write(&log, "");
    let run_bin = env.bin_dir.join("ts-r-run");
    let mut extra = vec![
        (shim::ENV_LOG, log.as_os_str()),
        (shim::ENV_BIN_DIR, env.bin_dir.as_os_str()),
        (shim::ENV_TSC, env.tsc.as_os_str()),
        ("TS_R_ECO_RUN_BIN", run_bin.as_os_str()),
    ];
    if let Some(lib) = env.lib_dir {
        // ts-r-check's lib files: the reference tsc's, like the check stage.
        extra.push(("TS_R_LIB_DIR", lib.as_os_str()));
    }
    let t = Instant::now();
    let o = npm(&tsr, &["test"], TEST_TIME_S, &jail, &extra);
    r.metrics
        .insert("ts_r_ms".into(), t.elapsed().as_millis() as f64);
    let events = read_log(&log);
    let mut counts: BTreeMap<&str, u32> = BTreeMap::new();
    for e in &events {
        *counts.entry(e.tool.as_str()).or_default() += 1;
    }
    for (tool, n) in &counts {
        r.metrics.insert(format!("ran_{tool}"), f64::from(*n));
    }
    let ran: Vec<String> = counts.iter().map(|(t, n)| format!("{t} ×{n}")).collect();
    let tools_s = if ran.is_empty() {
        tools.join(", ")
    } else {
        ran.join(", ")
    };
    r.note = format!(
        "`npm test` = `{}`; swapped {}; network {network}",
        report::truncate(&test_cmd, 120),
        if swapped.is_empty() {
            "nothing".into()
        } else {
            swapped.join(", ")
        }
    );
    if o.ok() {
        if events.is_empty() {
            r.note = format!("no swapped tool ran; {}", r.note);
            r.status = report::Status::Skip;
            return r;
        }
        r.checked = 1;
        r.finish();
        return r;
    }
    r.checked = 1;
    let (cause, detail) = tsr_cause(&events, &tools_s, &o);
    let check_blocked = cause.starts_with("tsc → ts-r-check");
    r.fail(cause, "npm test".to_string(), detail);
    if check_blocked && !o.timed_out {
        // Again with ts-r-check's errors ignored: does ts-r's emit / loader pass the suite?
        let _ = std::fs::write(&log, "");
        extra.push((shim::ENV_IGNORE_CHECK, std::ffi::OsStr::new("1")));
        let again = npm(&tsr, &["test"], TEST_TIME_S, &jail, &extra);
        let events = read_log(&log);
        // `ran_*`: the most each tool ran in either ts-r run (the first stops at the check).
        let mut again_counts: BTreeMap<String, f64> = BTreeMap::new();
        for e in &events {
            *again_counts.entry(format!("ran_{}", e.tool)).or_default() += 1.0;
        }
        for (k, n) in again_counts {
            let v = r.metrics.entry(k).or_default();
            *v = v.max(n);
        }
        let passed = again.ok();
        r.metrics
            .insert("passes_ignoring_check".into(), f64::from(u8::from(passed)));
        if passed {
            r.note = format!("passes with ts-r-check's errors ignored; {}", r.note);
        } else {
            let (cause, detail) = tsr_cause(&events, &tools_s, &again);
            r.note = format!("fails with ts-r-check's errors ignored too; {}", r.note);
            r.fail(cause, "npm test (check ignored)".to_string(), detail);
        }
    }
    r.finish();
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_swappable_tools_through_npm_run() {
        let pj = serde_json::json!({ "scripts": {
            "pretest": "npm run prepare", "prepare": "tshy", "test": "tap",
            "check:type:ts": "tsc -p tsconfig.ts.json",
        }});
        assert_eq!(swappable(&pj), ["tshy"]);
        let pj = serde_json::json!({ "scripts": {
            "test": "node --test && npm run check:type:ts", "check:type:ts": "tsc -p x.json",
        }});
        assert_eq!(swappable(&pj), ["tsc"]);
        let pj = serde_json::json!({ "scripts": {
            "test": "run-p test:*", "test:a": "mocha -r ts-node/register", "test:b": "esbuild x",
        }});
        assert_eq!(swappable(&pj), ["ts-node", "esbuild"]);
        let pj = serde_json::json!({ "scripts": { "test": "xo && ava" } });
        assert!(swappable(&pj).is_empty());
    }

    #[test]
    fn drops_lint_steps() {
        assert_eq!(without_linters("xo && ava && tsd"), "ava && tsd");
        assert_eq!(
            without_linters("npm run lint && npm run build && node --test"),
            "npm run build && node --test"
        );
        assert_eq!(without_linters("npx eslint . && tap"), "tap");
        assert_eq!(without_linters("xo"), "");
    }

    #[test]
    fn adds_the_skipped_install_build() {
        let s: BTreeMap<String, String> = [
            ("build".to_string(), "tsc".to_string()),
            ("test".into(), "node --test".into()),
        ]
        .into();
        assert_eq!(build_step(&s).as_deref(), Some("npm run build"));
        let s: BTreeMap<String, String> = [
            ("prepare".to_string(), "tshy".to_string()),
            ("pretest".into(), "npm run prepare".into()),
        ]
        .into();
        assert_eq!(build_step(&s), None);
    }

    #[test]
    fn github_urls() {
        for (url, want) in [
            (
                "git+https://github.com/npm/node-semver.git",
                Some("npm/node-semver"),
            ),
            (
                "git+ssh://git@github.com/isaacs/minimatch.git",
                Some("isaacs/minimatch"),
            ),
            ("github:chalk/chalk", Some("chalk/chalk")),
            ("https://github.com/a/b#main", Some("a/b")),
            ("sindresorhus/ky", Some("sindresorhus/ky")),
            ("https://gitlab.com/a/b", None),
        ] {
            assert_eq!(github_repo(url).as_deref(), want, "{url}");
        }
    }

    #[test]
    fn loader_targets() {
        let exports = serde_json::json!({
            ".": { "import": "./dist/esm.mjs", "require": "./dist/index.js" },
            "./register": "./register/index.js",
            "./package.json": "./package.json",
        });
        let mut out = Vec::new();
        collect_targets(&exports, "", &mut out);
        out.sort();
        assert_eq!(
            out,
            ["./dist/esm.mjs", "./dist/index.js", "./register/index.js"]
        );
    }
}
