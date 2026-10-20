//! Pinned public repositories for `cargo xtask bench` (`bench/repos.toml`): loading and
//! selecting them, fetching each at its pinned commit into `target/bench-repos/<name>`,
//! installing its dependencies under `scripts/limit.sh`, preparing derived inputs (three.js
//! x10) and printing or running the editor-memory harnesses on them.
//!
//! Nothing here runs in a default `cargo xtask bench`: fetching is opt-in (`--fetch-repos`) and
//! repo scenarios only run when `--repos` names them. Repos marked `size = "large"` also need
//! `--large`, because their installs and checks do not fit the shared machine's memory caps.
//!
//! A repeat fetch does no work when nothing changed: a checkout already at the pinned commit is
//! kept, an install is skipped while its stamp (commit, install commands, lockfile hashes, Node
//! version) matches, and the three.js x10 input is only rebuilt when its stamp changes. Package
//! managers share one offline-first cache under [`cache_dir`] (`{cache}` in install commands).

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::util::{Result, read_to_string, rel, root, run_capture, run_status};

pub const MANIFEST: &str = "bench/repos.toml";
/// Where checkouts go (inside the gitignored `target/`).
pub const REPOS_DIR: &str = "target/bench-repos";
/// Install and prepare stamps, next to (not inside) the checkouts.
pub const STAMPS_DIR: &str = "target/bench-repos/.stamps";
/// The highest memory cap the manifest may ask for (see the root AGENTS.md resource limits).
pub const MAX_MEM_MB: u32 = 8192;

/// Scenario kinds a repo can take part in. `editor-memory` and `dev` are harnesses outside
/// `scenarios.toml` (`editors/vscode/bench/`, `bench/dev/`).
pub const KINDS: &[&str] = &["check", "lint", "build", "rebuild", "editor-memory", "dev"];

/// Lockfiles whose contents decide whether an install is still current.
pub const LOCKFILES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lock",
];

/// The shared package-manager cache: `$TS_R_BENCH_CACHE`, else `~/.cache/ts-r/bench`.
/// Install commands name it as `{cache}`; [`pm_cache_env`] points every package manager at it.
pub fn cache_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("TS_R_BENCH_CACHE").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    match std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|d| !d.is_empty())
    {
        Some(home) => PathBuf::from(home).join(".cache/ts-r/bench"),
        None => root().join("target/bench-cache"),
    }
}

/// Points npm, pnpm, Yarn (1 and berry) and corepack at the shared cache, offline-first. The
/// install commands in `repos.toml` also pass the flags explicitly; these cover the rest.
pub fn pm_cache_env(cmd: &mut Command, cache: &Path) {
    let p = |sub: &str| cache.join(sub);
    cmd.env("npm_config_cache", p("npm"))
        .env("npm_config_prefer_offline", "true")
        .env("pnpm_config_store_dir", p("pnpm-store"))
        .env("YARN_CACHE_FOLDER", p("yarn"))
        .env("YARN_GLOBAL_FOLDER", p("yarn-berry"))
        .env("YARN_ENABLE_GLOBAL_CACHE", "true")
        .env("YARN_ENABLE_TELEMETRY", "0")
        .env("COREPACK_HOME", p("corepack"));
}

/// Replaces `{cache}` in an install command with the (shell-quoted) cache directory.
pub fn expand_install(step: &str, cache: &Path) -> String {
    step.replace("{cache}", &quote(&cache.to_string_lossy()))
}

/// FNV-1a, 64 bits: a stable content hash for stamps (no hashing crate in xtask).
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// `node --version`, or `none`.
pub fn node_version() -> String {
    run_capture(Command::new("node").arg("--version"))
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|_| "none".into())
}

/// What an install depends on: commit, commands, Node version and the lockfiles' hashes. The
/// install is skipped while the stamp written after the last successful one is equal.
pub fn install_stamp(r: &RepoPin, install_dir: &Path, node: &str) -> String {
    let mut s = format!(
        "commit {}\ninstall {}\npost_install {}\nnode {node}\n",
        r.commit,
        r.install.as_deref().unwrap_or(""),
        r.post_install.as_deref().unwrap_or(""),
    );
    for f in LOCKFILES {
        if let Ok(bytes) = std::fs::read(install_dir.join(f)) {
            s.push_str(&format!("{f} {:016x}\n", fnv1a(&bytes)));
        }
    }
    s
}

fn stamp_path(r: &RepoPin, what: &str) -> PathBuf {
    root().join(STAMPS_DIR).join(format!("{}.{what}", r.name))
}

fn stamp_matches(path: &Path, want: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|s| s == want)
}

fn write_stamp(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text)?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReposFile {
    #[serde(rename = "repo")]
    pub repos: Vec<RepoPin>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Size {
    Small,
    Medium,
    Large,
}

impl Size {
    pub fn as_str(self) -> &'static str {
        match self {
            Size::Small => "small",
            Size::Medium => "medium",
            Size::Large => "large",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoPin {
    /// Short name: the checkout directory and the `--repos` key.
    pub name: String,
    /// GitHub `owner/name`.
    pub repo: String,
    /// The default branch the commit was taken from.
    pub branch: String,
    /// Pinned commit (40 hex digits).
    pub commit: String,
    /// When the pin was taken (`git ls-remote` of the default branch).
    pub date: String,
    /// `large-app`, `monorepo`, `ts7` (TypeScript 7's benchmark set), `bundler` or `app`.
    pub group: String,
    /// `large` repos need a dedicated machine and `--large`.
    pub size: Size,
    /// Sparse-checkout patterns (`--no-cone`); empty = the whole tree.
    #[serde(default)]
    pub sparse: Vec<String>,
    /// Shell command that installs dependencies, run in `install_dir`.
    #[serde(default)]
    pub install: Option<String>,
    /// Directory (relative to the checkout) to run `install` in; default the checkout.
    #[serde(default)]
    pub install_dir: Option<String>,
    /// Shell command run after `install` (same dir and caps), e.g. building a workspace
    /// package whose `exports` point at build output the check needs.
    #[serde(default)]
    pub post_install: Option<String>,
    /// Memory cap (MB) for the install and for each benchmarked command.
    #[serde(default = "default_mem")]
    pub mem_mb: u32,
    /// Time cap (s) for the install.
    #[serde(default = "default_install_time")]
    pub install_time_s: u32,
    /// The TypeScript projects the check scenarios use (tsconfig paths in the checkout).
    #[serde(default)]
    pub projects: Vec<String>,
    /// Extra `compilerOptions` for the check wrappers (see [`write_check_wrappers`]).
    #[serde(default)]
    pub check_options: toml::Table,
    /// The linter the repo itself uses (the lint reference), e.g. `eslint`, `oxlint`, `biome`.
    #[serde(default)]
    pub linter: Option<String>,
    /// Derived input to generate after checkout: `three-x10`, or `dev-harness` (copy
    /// `bench/dev/<name>/` to `ts-r-bench-dev/` in the checkout).
    #[serde(default)]
    pub prepare: Option<String>,
    /// Which kinds of scenario use this repo (`check`, `lint`, `build`, `editor-memory`).
    pub scenarios: Vec<String>,
    #[serde(default)]
    pub editor: Option<EditorSet>,
    #[serde(default)]
    pub notes: Option<String>,
}

/// Files the editor-memory runs open, and the directories `--open-all` walks.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorSet {
    pub files: Vec<String>,
    #[serde(default)]
    pub src_dirs: Vec<String>,
}

fn default_mem() -> u32 {
    4096
}

fn default_install_time() -> u32 {
    900
}

impl RepoPin {
    pub fn dir(&self) -> PathBuf {
        root().join(REPOS_DIR).join(&self.name)
    }

    pub fn url(&self) -> String {
        format!("https://github.com/{}.git", self.repo)
    }
}

pub fn load() -> Result<ReposFile> {
    let path = root().join(MANIFEST);
    let text = read_to_string(&path)?;
    let file: ReposFile = toml::from_str(&text).map_err(|e| format!("{MANIFEST}: {e}"))?;
    let problems = validate(&file);
    if !problems.is_empty() {
        return Err(format!("{MANIFEST}:\n  {}", problems.join("\n  ")).into());
    }
    Ok(file)
}

/// Manifest invariants (also run as a unit test).
pub fn validate(file: &ReposFile) -> Vec<String> {
    let mut problems = Vec::new();
    let mut names: Vec<&str> = file.repos.iter().map(|r| r.name.as_str()).collect();
    names.sort_unstable();
    for w in names.windows(2) {
        if w[0] == w[1] {
            problems.push(format!("duplicate repo name {}", w[0]));
        }
    }
    for r in &file.repos {
        let n = &r.name;
        if n.is_empty() || !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            problems.push(format!("{n}: names are [a-z0-9-]"));
        }
        if r.commit.len() != 40 || !r.commit.chars().all(|c| c.is_ascii_hexdigit()) {
            problems.push(format!("{n}: commit must be a full 40-digit SHA"));
        }
        if r.repo.split('/').count() != 2 {
            problems.push(format!("{n}: repo must be owner/name"));
        }
        if r.mem_mb > MAX_MEM_MB {
            problems.push(format!(
                "{n}: mem_mb {} is above {MAX_MEM_MB} (raise it per run with --mem-mb N --dedicated-machine on a dedicated machine)",
                r.mem_mb
            ));
        }
        for s in &r.scenarios {
            if !KINDS.contains(&s.as_str()) {
                problems.push(format!("{n}: unknown scenario kind {s}"));
            }
        }
        if r.scenarios.iter().any(|s| s == "check") && r.projects.is_empty() {
            problems.push(format!("{n}: check needs projects"));
        }
        if r.scenarios.iter().any(|s| s == "lint") && r.linter.is_none() {
            problems.push(format!("{n}: lint needs linter"));
        }
        let wants_editor = r.scenarios.iter().any(|s| s == "editor-memory");
        match &r.editor {
            Some(e) if e.files.is_empty() => problems.push(format!("{n}: editor.files is empty")),
            None if wants_editor => {
                problems.push(format!("{n}: editor-memory needs [repo.editor]"))
            }
            Some(_) if !wants_editor => {
                problems.push(format!("{n}: [repo.editor] without editor-memory"))
            }
            _ => {}
        }
        if let Some(p) = &r.prepare
            && p != "three-x10"
            && p != "dev-harness"
        {
            problems.push(format!("{n}: unknown prepare step {p}"));
        }
        if let Some(i) = &r.install
            && let Some(missing) = offline_first_problem(i)
        {
            problems.push(format!("{n}: install {missing}"));
        }
    }
    problems
}

/// Installs must use the shared cache offline-first (see [`cache_dir`]): npm and pnpm with
/// `--prefer-offline` and `{cache}`, Yarn 1 with `--prefer-offline` and `--cache-folder
/// {cache}/...`. Yarn berry (`--immutable`) reads its global cache from the environment.
pub fn offline_first_problem(install: &str) -> Option<String> {
    let berry = install.contains("--immutable");
    if berry {
        return None;
    }
    let mut missing = Vec::new();
    if !install.contains("--prefer-offline") {
        missing.push("--prefer-offline");
    }
    if !install.contains("{cache}") {
        missing.push("a {cache} directory");
    }
    (!missing.is_empty()).then(|| format!("needs {}", missing.join(" and ")))
}

/// Resolves `--repos a,b` (`all` = every repo that fits this machine; with `large`, every
/// repo). Naming a large repo without `large` is an error.
pub fn select<'a>(file: &'a ReposFile, spec: &str, large: bool) -> Result<Vec<&'a RepoPin>> {
    let mut out: Vec<&RepoPin> = Vec::new();
    for name in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if name == "all" {
            for r in &file.repos {
                if (large || r.size != Size::Large) && !out.iter().any(|o| o.name == r.name) {
                    out.push(r);
                }
            }
            continue;
        }
        let Some(r) = file.repos.iter().find(|r| r.name == name) else {
            let known: Vec<&str> = file.repos.iter().map(|r| r.name.as_str()).collect();
            return Err(format!("unknown repo `{name}` (known: {})", known.join(", ")).into());
        };
        if r.size == Size::Large && !large {
            return Err(format!(
                "`{name}` is a large repo: its install and checks exceed this machine's memory caps. \
                 Run it on a dedicated machine with --large (see bench/README.md); \
                 --list and --dry-run work without it"
            )
            .into());
        }
        if !out.iter().any(|o| o.name == r.name) {
            out.push(r);
        }
    }
    if out.is_empty() {
        return Err("--repos selected nothing".into());
    }
    Ok(out)
}

fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    cmd
}

fn head(dir: &Path) -> Option<String> {
    run_capture(git(dir).args(["rev-parse", "--verify", "--quiet", "HEAD"]))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Whether the checkout exists at the pinned commit.
pub fn is_fetched(r: &RepoPin) -> bool {
    head(&r.dir()).as_deref() == Some(r.commit.as_str())
}

/// The `scripts/limit.sh` prefix for a command under `mem_mb` / `time_s`.
pub fn limit_cmd(mem_mb: u32, time_s: u32) -> Command {
    let mut cmd = Command::new(root().join("scripts/limit.sh"));
    cmd.args([
        "--mem-mb",
        &mem_mb.to_string(),
        "--time-s",
        &time_s.to_string(),
        "--",
    ]);
    cmd
}

pub struct FetchOptions {
    pub install: bool,
    pub dry_run: bool,
    /// Overrides the manifest's memory cap (dedicated machines).
    pub mem_mb: Option<u32>,
    /// Install even when the stamp says nothing changed.
    pub reinstall: bool,
}

/// Shallow, blob-less fetch of the pinned commit, then install and prepare. Each step is
/// skipped when it is already done: the checkout is at the commit, the install stamp matches,
/// the prepared input's stamp matches.
pub fn fetch(r: &RepoPin, o: &FetchOptions) -> Result<()> {
    let dir = r.dir();
    let mem = o.mem_mb.unwrap_or(r.mem_mb);
    let cache = cache_dir();
    let install_dir = match &r.install_dir {
        Some(d) => dir.join(d),
        None => dir.clone(),
    };
    let steps: Vec<String> = [&r.install, &r.post_install]
        .into_iter()
        .flatten()
        .map(|s| expand_install(s, &cache))
        .collect();
    if o.dry_run {
        println!("{} ({}@{}):", r.name, r.repo, &r.commit[..12]);
        println!(
            "    git init {} && git remote add origin {}",
            rel(&dir),
            r.url()
        );
        if !r.sparse.is_empty() {
            println!(
                "    git sparse-checkout set --no-cone {}",
                r.sparse.join(" ")
            );
        }
        println!(
            "    git fetch --depth 1 --filter=blob:none --no-tags origin {} && git checkout --detach {}",
            r.commit, r.commit
        );
        if o.install {
            for step in &steps {
                println!(
                    "    scripts/limit.sh --mem-mb {mem} --time-s {} -- sh -c 'cd {} && {step}'",
                    r.install_time_s,
                    rel(&install_dir),
                );
            }
            if !steps.is_empty() {
                println!(
                    "    (skipped while {} matches; package caches in {})",
                    rel(&stamp_path(r, "install")),
                    cache.display()
                );
            }
        }
        for p in &r.projects {
            println!("    write {} (extends {p})", wrapper_path(p));
        }
        if let Some(p) = &r.prepare {
            println!("    prepare: {p}");
        }
        return Ok(());
    }

    if !dir.join(".git").exists() {
        std::fs::create_dir_all(&dir)?;
        run_capture(git(&dir).args(["init", "--quiet"]))?;
        run_capture(git(&dir).args(["remote", "add", "origin", &r.url()]))?;
    }
    if !r.sparse.is_empty() {
        let current = run_capture(git(&dir).args(["sparse-checkout", "list"])).unwrap_or_default();
        if !current
            .lines()
            .map(str::trim)
            .eq(r.sparse.iter().map(String::as_str))
        {
            run_capture(
                git(&dir)
                    .args(["sparse-checkout", "set", "--no-cone"])
                    .args(&r.sparse),
            )?;
        }
    }
    if is_fetched(r) {
        eprintln!("{}: checkout already at {}", r.name, &r.commit[..12]);
    } else {
        eprintln!("{}: fetching {}@{} ...", r.name, r.repo, &r.commit[..12]);
        run_status(git(&dir).args([
            "fetch",
            "--quiet",
            "--depth",
            "1",
            "--filter=blob:none",
            "--no-tags",
            "origin",
            &r.commit,
        ]))?;
        run_status(git(&dir).args([
            "-c",
            "advice.detachedHead=false",
            "-c",
            "core.longpaths=true",
            "checkout",
            "--quiet",
            "--force",
            "--detach",
            &r.commit,
        ]))?;
    }
    if o.install && !steps.is_empty() {
        let stamp_file = stamp_path(r, "install");
        let stamp = install_stamp(r, &install_dir, &node_version());
        if !o.reinstall
            && install_dir.join("node_modules").is_dir()
            && stamp_matches(&stamp_file, &stamp)
        {
            eprintln!(
                "{}: install up to date ({}), skipped",
                r.name,
                rel(&stamp_file)
            );
        } else {
            let _ = std::fs::remove_file(&stamp_file);
            std::fs::create_dir_all(&cache)?;
            for step in &steps {
                eprintln!(
                    "{}: running `{step}` under {mem} MB / {} s ...",
                    r.name, r.install_time_s
                );
                let mut cmd = limit_cmd(mem, r.install_time_s);
                cmd.args(["sh", "-c", step])
                    .current_dir(&install_dir)
                    .env("CI", "1")
                    .env("COREPACK_ENABLE_DOWNLOAD_PROMPT", "0")
                    .env("HUSKY", "0");
                pm_cache_env(&mut cmd, &cache);
                run_status(&mut cmd)?;
            }
            write_stamp(&stamp_file, &stamp)?;
        }
    }
    write_check_wrappers(r)?;
    if r.prepare.as_deref() == Some("three-x10") {
        let stamp_file = stamp_path(r, "prepare");
        let stamp = format!("commit {}\ncopies {THREE_COPIES}\n", r.commit);
        if dir.join("bench/entry.js").is_file() && stamp_matches(&stamp_file, &stamp) {
            eprintln!("{}: three.js x10 input up to date, skipped", r.name);
        } else {
            prepare_three_x10(&dir)?;
            write_stamp(&stamp_file, &stamp)?;
        }
    }
    if r.prepare.as_deref() == Some("dev-harness") {
        let from = root().join(DEV_DIR).join(&r.name);
        if !from.is_dir() {
            return Err(format!("{}: no harness in {}", r.name, rel(&from)).into());
        }
        let to = dir.join(DEV_HARNESS);
        if to.exists() {
            std::fs::remove_dir_all(&to)?;
        }
        copy_tree(&from, &to)?;
        eprintln!("{}: copied {} to {}", r.name, rel(&from), rel(&to));
    }
    Ok(())
}

/// Per-app dev-server and app-bundle harnesses (`bench/dev/<repo>/`).
pub const DEV_DIR: &str = "bench/dev";
/// Where `prepare = "dev-harness"` copies `bench/dev/<repo>/` inside the checkout.
pub const DEV_HARNESS: &str = "ts-r-bench-dev";

/// The wrapper tsconfig the check scenarios use for `project`:
/// `dir/tsconfig.json` -> `dir/tsconfig.ts-r-bench.json`.
pub fn wrapper_path(project: &str) -> String {
    match project.strip_suffix(".json") {
        Some(stem) => format!("{stem}.ts-r-bench.json"),
        None => format!("{project}.ts-r-bench.json"),
    }
}

/// The wrapper's text: extends the project and turns emit, incremental builds and composite
/// off (a cold check that writes nothing, the same for every tool), plus the repo's
/// `check_options`.
pub fn wrapper_json(project: &str, extra: &toml::Table) -> Result<String> {
    let file = Path::new(project)
        .file_name()
        .and_then(|f| f.to_str())
        .ok_or_else(|| format!("bad project path {project}"))?;
    let mut opts = serde_json::Map::new();
    opts.insert("noEmit".into(), true.into());
    opts.insert("incremental".into(), false.into());
    opts.insert("composite".into(), false.into());
    for (k, v) in extra {
        opts.insert(k.clone(), serde_json::to_value(v)?);
    }
    let mut root = serde_json::Map::new();
    root.insert("extends".into(), format!("./{file}").into());
    root.insert("compilerOptions".into(), opts.into());
    Ok(serde_json::to_string_pretty(&serde_json::Value::Object(root))? + "\n")
}

/// Writes the check wrappers next to each project (in the checkout, never in git).
pub fn write_check_wrappers(r: &RepoPin) -> Result<()> {
    for p in &r.projects {
        let target = r.dir().join(wrapper_path(p));
        if target.parent().is_some_and(Path::is_dir) {
            std::fs::write(&target, wrapper_json(p, &r.check_options)?)?;
        } else {
            eprintln!("{}: {p}: directory missing, no wrapper written", r.name);
        }
    }
    Ok(())
}

/// Number of three.js copies in the bundler scenario.
pub const THREE_COPIES: usize = 10;

/// The three.js x10 input: `bench/copy1` .. `bench/copy10` are copies of three.js's `src`,
/// and `bench/entry.js` imports every copy as a namespace and re-exports them, so a bundler
/// has to parse, link, tree-shake and minify ten independent copies of the same library.
pub fn prepare_three_x10(dir: &Path) -> Result<()> {
    let src = dir.join("src");
    if !src.join("Three.js").is_file() {
        return Err(format!("{}: no src/Three.js", rel(dir)).into());
    }
    let out = dir.join("bench");
    if out.exists() {
        std::fs::remove_dir_all(&out)?;
    }
    for i in 1..=THREE_COPIES {
        copy_tree(&src, &out.join(format!("copy{i}")))?;
    }
    std::fs::write(out.join("entry.js"), three_x10_entry(THREE_COPIES))?;
    eprintln!(
        "three-x10: wrote {} ({THREE_COPIES} copies of src)",
        rel(&out)
    );
    Ok(())
}

pub fn three_x10_entry(copies: usize) -> String {
    let mut s = String::from("// Generated by `cargo xtask bench --fetch-repos`.\n");
    for i in 1..=copies {
        s.push_str(&format!("import * as copy{i} from './copy{i}/Three.js';\n"));
    }
    let names: Vec<String> = (1..=copies).map(|i| format!("copy{i}")).collect();
    s.push_str(&format!("export {{ {} }};\n", names.join(", ")));
    s
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    let mut entries: Vec<_> = std::fs::read_dir(from)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let ty = e.file_type()?;
        let target = to.join(e.file_name());
        if ty.is_dir() {
            copy_tree(&e.path(), &target)?;
        } else if ty.is_file() {
            std::fs::copy(e.path(), &target)?;
        }
    }
    Ok(())
}

/// Deletes the checkouts of the selected repos.
pub fn clean(repos: &[&RepoPin]) -> Result<()> {
    for r in repos {
        let dir = r.dir();
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
            eprintln!("removed {}", rel(&dir));
        }
        for what in ["install", "prepare"] {
            let _ = std::fs::remove_file(stamp_path(r, what));
        }
    }
    Ok(())
}

/// One editor-memory measurement: a label and the command line.
pub struct EditorRun {
    pub label: String,
    pub argv: Vec<String>,
    /// The ts-r daemon to stop afterwards.
    pub stop_daemon: bool,
}

/// The editor harness runs for one repo: the scripted-client harness
/// (`editors/vscode/bench/editor-latency.js`: ts-r-lsp, the repo's tsserver, TS 7's LSP) with
/// every source open, and the real VS Code harness (`editor-memory.js`: stock vs ts-r).
pub fn editor_runs(r: &RepoPin, bin_dir: &str, lib_dir: Option<&Path>) -> Vec<EditorRun> {
    let Some(e) = &r.editor else {
        return Vec::new();
    };
    let repo = rel(&r.dir());
    let first = e.files[0].clone();
    // Every source open needs more than the default cap on the big repos.
    let latency_mem = r.mem_mb.max(4096).to_string();
    let vscode_mem = r.mem_mb.max(6144).to_string();
    let latency = |kind: &str, server: String| {
        let mut argv: Vec<String> = [
            "scripts/limit.sh",
            "--mem-mb",
            &latency_mem,
            "--time-s",
            "900",
            "--",
            "env",
            "TS_R_LSP_PARITY=off",
            "node",
            "editors/vscode/bench/editor-latency.js",
            kind,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // The daemon runs in the repo, where the synced libs are not found relative to the cwd.
        if let Some(libs) = lib_dir {
            argv.insert(8, format!("TS_R_LIB_DIR={}", libs.display()));
        }
        argv.extend([server, repo.clone(), first.clone()]);
        argv.extend(
            [
                "--keys",
                "30",
                "--samples",
                "20",
                "--open-all",
                "--settle",
                "5000",
            ]
            .map(String::from),
        );
        if !e.src_dirs.is_empty() {
            argv.extend(["--src-dirs".to_string(), e.src_dirs.join(",")]);
        }
        argv
    };
    // The harness starts servers with the repo as cwd: their paths must be absolute.
    let abs = |p: &str| -> String {
        let p = Path::new(p);
        if p.is_absolute() {
            p.display().to_string()
        } else {
            root().join(p).display().to_string()
        }
    };
    let tsgo = format!(
        "bench/node_modules/@typescript/typescript-{}-{}/lib/tsc",
        match std::env::consts::OS {
            "macos" => "darwin",
            "windows" => "win32",
            o => o,
        },
        match std::env::consts::ARCH {
            "aarch64" => "arm64",
            "x86_64" => "x64",
            a => a,
        }
    );
    let mut runs = vec![
        EditorRun {
            label: "latency-ts-r".into(),
            argv: latency("ts-r", abs(&format!("{bin_dir}/ts-r-lsp"))),
            stop_daemon: true,
        },
        EditorRun {
            label: "latency-tsserver".into(),
            argv: latency(
                "tsserver",
                abs(&format!(
                    "{repo}/{}node_modules/typescript/lib/tsserver.js",
                    r.install_dir
                        .as_deref()
                        .map(|d| format!("{d}/"))
                        .unwrap_or_default()
                )),
            ),
            stop_daemon: false,
        },
        EditorRun {
            label: "latency-tsgo".into(),
            argv: latency("tsgo", abs(&tsgo)),
            stop_daemon: false,
        },
    ];
    for mode in ["stock", "ts-r"] {
        let mut argv: Vec<String> = [
            "scripts/limit.sh",
            "--mem-mb",
            &vscode_mem,
            "--time-s",
            "900",
            "--",
            "node",
            "editors/vscode/bench/editor-memory.js",
            "--project",
            &repo,
            "--mode",
            mode,
            "--files",
            &e.files.join(","),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // The harness defaults to target/release; point it at the selected build.
        if mode == "ts-r" {
            let bin = format!("TS_R_LSP_BIN={}", abs(&format!("{bin_dir}/ts-r-lsp")));
            argv.splice(6..6, ["env".to_string(), bin]);
        }
        runs.push(EditorRun {
            label: format!("vscode-{mode}"),
            argv,
            stop_daemon: false,
        });
    }
    runs
}

/// Shell-quotes one argument for printing.
pub fn quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=,:@+".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_is_valid() {
        let file = load().unwrap();
        assert!(validate(&file).is_empty());
        assert!(file.repos.len() >= 15);
    }

    #[test]
    fn selection_guards_large_repos() {
        let file = load().unwrap();
        let small = select(&file, "all", false).unwrap();
        assert!(small.iter().all(|r| r.size != Size::Large));
        assert!(select(&file, "all", true).unwrap().len() == file.repos.len());
        assert!(select(&file, "vscode", false).is_err());
        assert!(select(&file, "vscode", true).is_ok());
        assert!(select(&file, "nope", true).is_err());
        let two = select(&file, "trpc,date-fns,trpc", false).unwrap();
        assert_eq!(two.len(), 2);
    }

    #[test]
    fn three_entry_imports_every_copy() {
        let e = three_x10_entry(3);
        assert!(e.contains("import * as copy3 from './copy3/Three.js';"));
        assert!(e.contains("export { copy1, copy2, copy3 };"));
    }

    #[test]
    fn editor_runs_cover_both_harnesses() {
        let file = load().unwrap();
        let r = file.repos.iter().find(|r| r.editor.is_some()).unwrap();
        let runs = editor_runs(r, "target/dist", Some(Path::new("/libs")));
        assert!(runs[0].argv.contains(&"TS_R_LIB_DIR=/libs".to_string()));
        assert_eq!(runs[0].argv[6], "env");
        let labels: Vec<&str> = runs.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "latency-ts-r",
                "latency-tsserver",
                "latency-tsgo",
                "vscode-stock",
                "vscode-ts-r"
            ]
        );
        assert!(runs.iter().all(|r| r.argv[0] == "scripts/limit.sh"));
        let vscode = &runs[4].argv;
        assert_eq!(vscode[6], "env");
        assert_eq!(
            vscode[7],
            format!(
                "TS_R_LSP_BIN={}",
                root().join("target/dist/ts-r-lsp").display()
            )
        );
        assert!(!runs[3].argv.iter().any(|a| a.starts_with("TS_R_LSP_BIN=")));
    }

    #[test]
    fn check_wrappers() {
        assert_eq!(
            wrapper_path("packages/a/tsconfig.json"),
            "packages/a/tsconfig.ts-r-bench.json"
        );
        let mut extra = toml::Table::new();
        extra.insert("moduleResolution".into(), "node16".into());
        extra.insert("composite".into(), true.into());
        let text = wrapper_json("packages/a/tsconfig.build.json", &extra).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["extends"], "./tsconfig.build.json");
        assert_eq!(v["compilerOptions"]["noEmit"], true);
        assert_eq!(v["compilerOptions"]["incremental"], false);
        assert_eq!(v["compilerOptions"]["composite"], true);
        assert_eq!(v["compilerOptions"]["moduleResolution"], "node16");
    }

    #[test]
    fn installs_are_offline_first() {
        assert!(offline_first_problem("npm ci --ignore-scripts").is_some());
        assert!(
            offline_first_problem("npm ci --prefer-offline --cache {cache}/npm --ignore-scripts")
                .is_none()
        );
        assert!(offline_first_problem("corepack yarn install --immutable").is_none());
        let cache = Path::new("/c d/bench");
        assert_eq!(
            expand_install("pnpm i --store-dir {cache}/pnpm-store", cache),
            "pnpm i --store-dir '/c d/bench'/pnpm-store"
        );
    }

    #[test]
    fn install_stamp_tracks_lockfiles_and_node() {
        let file = load().unwrap();
        let r = file.repos.iter().find(|r| r.install.is_some()).unwrap();
        let dir = std::env::temp_dir().join(format!("xtask-stamp-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pnpm-lock.yaml"), "a").unwrap();
        let a = install_stamp(r, &dir, "v24.0.0");
        assert!(a.contains(&r.commit) && a.contains("pnpm-lock.yaml"));
        assert_eq!(a, install_stamp(r, &dir, "v24.0.0"));
        assert_ne!(a, install_stamp(r, &dir, "v22.0.0"));
        std::fs::write(dir.join("pnpm-lock.yaml"), "b").unwrap();
        assert_ne!(a, install_stamp(r, &dir, "v24.0.0"));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
    }

    #[test]
    fn quoting() {
        assert_eq!(quote("a/b.ts"), "a/b.ts");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), "'it'\\''s'");
    }
}
