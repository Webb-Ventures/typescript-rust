//! `cargo xtask emit-repos`: tsc 7 and ts-r-check emit on real repositories, compared file by
//! file (design record 0012, E6).
//!
//! Each case in [`MANIFEST`] names a source (a fetched bench checkout from `bench/repos.toml`,
//! or a package or GitHub tarball from the ecosystem cache), a directory and the command line
//! both tools get. The two tools run one after the other in the same tree, each under
//! `scripts/limit.sh` and `/usr/bin/time`. After each run every file that appeared in the tree
//! (outside `node_modules` and `.git`) is moved into `target/emit-repos/out/<case>/<tool>/`, so
//! the second tool starts from the same clean tree and both see the same paths (source maps
//! name sources relative to the output, so the outputs must be written where tsc writes them).
//!
//! Compared per case: the written file sets and bytes per kind (`.js`, `.js.map`, `.d.ts`,
//! `.d.ts.map`, JSON; `.tsbuildinfo` by path only, since ts-r writes its own schema), the exit
//! codes, the diagnostics as (file, line, column, code), and the peak RSS of each tool's largest
//! process. The tsc side is cached under its key (binary, case, source), so re-runs only run
//! ts-r-check unless `--fresh` is given. Output is sorted and holds no absolute paths.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::args::Args;
use crate::bench::parse_max_rss_mb;
use crate::bench_repos::{self, fnv1a, limit_cmd};
use crate::util::{Result, read_to_string, rel, root};

pub const MANIFEST: &str = "bench/emit-repos.toml";
const OUT_DIR: &str = "target/emit-repos";
/// The shared machine's per-command cap (root AGENTS.md).
const MAX_MEM_MB: u32 = 4096;
/// Threads for both tools (the shared-machine budget): `GOMAXPROCS` for tsc, `--threads` for
/// ts-r-check.
const THREADS: &str = "3";
/// The byte-identical share of all files the E6 gate asks for.
const GATE_IDENTICAL: f64 = 0.95;

#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(rename = "case")]
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
struct Case {
    /// Short name: the `--only` key and the output directory.
    name: String,
    /// What the case covers (module kind, declarations, JSX, references, target).
    style: String,
    /// A checkout in `target/bench-repos/<bench>` (`cargo xtask bench --fetch-repos`).
    #[serde(default)]
    bench: Option<String>,
    /// An unpacked npm package in the ecosystem cache (`src/<name@version>/package`), copied.
    #[serde(default)]
    ecosystem: Option<String>,
    /// A GitHub source tarball in the ecosystem cache (`repos/<name@version>.tgz`), unpacked.
    #[serde(default)]
    ecosystem_repo: Option<String>,
    /// Ecosystem packages (`name@version`) copied into the copy's `node_modules`.
    #[serde(default)]
    deps: Vec<String>,
    /// Paths deleted from an ecosystem copy when it is prepared (a package's shipped `dist/`).
    #[serde(default)]
    remove: Vec<String>,
    /// Working directory inside the source.
    #[serde(default)]
    dir: String,
    /// The command line both tools get (`--pretty false` is added).
    args: Vec<String>,
    /// Files written into `dir` before the runs (generated configs) and removed after.
    #[serde(default)]
    files: BTreeMap<String, String>,
    #[serde(default = "default_mem")]
    mem_mb: u32,
    #[serde(default = "default_time")]
    time_s: u32,
    /// Counted in the gate. A case where tsc itself reports errors is reported but left out
    /// (its exit code and emit follow the errors, not the emitter).
    #[serde(default = "default_gate", skip_serializing)]
    gate: bool,
}

fn default_mem() -> u32 {
    MAX_MEM_MB
}
fn default_time() -> u32 {
    600
}
fn default_gate() -> bool {
    true
}

/// One tool's run of one case.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
struct Run {
    key: String,
    exit: Option<i32>,
    rss_mb: Option<f64>,
    seconds: f64,
    stdout: String,
    /// Pre-existing files the tool changed (they are not restored; the run is suspect).
    modified: Vec<String>,
}

#[derive(Debug, Default, Clone, Copy)]
struct KindCount {
    identical: usize,
    differ: usize,
    tsc_only: usize,
    ours_only: usize,
}

impl KindCount {
    fn total(&self) -> usize {
        self.identical + self.differ + self.tsc_only + self.ours_only
    }
}

struct CaseResult {
    name: String,
    style: String,
    gate: bool,
    kinds: BTreeMap<&'static str, KindCount>,
    tsc: Run,
    ours: Run,
    diags_same: usize,
    diags_tsc_only: Vec<String>,
    diags_ours_only: Vec<String>,
    differing: Vec<String>,
}

pub fn run(mut args: Args) -> Result<ExitCode> {
    let only = args.opt("only")?;
    let fresh = args.flag("fresh");
    let no_fail = args.flag("no-fail");
    let markdown = args.opt("markdown")?;
    let tsc = match args.opt("tsc")? {
        Some(t) => PathBuf::from(t),
        None => default_tsc()?,
    };
    let ours = match args.opt("bin")? {
        Some(b) => PathBuf::from(b),
        None => root().join("target/release/ts-r-check"),
    };
    args.finish()?;
    if !ours.is_file() {
        return Err(format!(
            "{} is missing: cargo build --release -j 3 -p ts_r_check",
            ours.display()
        )
        .into());
    }
    let tsc = native_tsc(&tsc);
    let manifest: Manifest = toml::from_str(&read_to_string(&root().join(MANIFEST))?)
        .map_err(|e| format!("{MANIFEST}: {e}"))?;
    let only: Option<BTreeSet<String>> =
        only.map(|o| o.split(',').map(|s| s.trim().to_string()).collect());
    let tsc_id = binary_id(&tsc);
    let mut results = Vec::new();
    let mut failed = Vec::new();
    for case in &manifest.cases {
        if only.as_ref().is_some_and(|o| !o.contains(&case.name)) {
            continue;
        }
        if case.mem_mb > MAX_MEM_MB {
            return Err(format!("{}: mem_mb above {MAX_MEM_MB}", case.name).into());
        }
        match run_case(case, &tsc, &tsc_id, &ours, fresh) {
            Ok(Some(r)) => results.push(r),
            Ok(None) => {}
            Err(e) => {
                eprintln!("emit-repos: {}: {e}", case.name);
                failed.push(case.name.as_str());
            }
        }
    }
    let (table, pass) = report(&results, &failed);
    print!("{table}");
    std::fs::create_dir_all(root().join(OUT_DIR))?;
    std::fs::write(root().join(OUT_DIR).join("report.md"), &table)?;
    if let Some(path) = markdown {
        std::fs::write(&path, &table)?;
    }
    Ok(if pass || no_fail {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// `bench/node_modules/typescript/bin/tsc` (the pinned tsc 7, `bench/package.json`).
fn default_tsc() -> Result<PathBuf> {
    let p = root().join("bench/node_modules/typescript/bin/tsc");
    if p.is_file() {
        Ok(p)
    } else {
        Err("no tsc 7: run `(cd bench && ../scripts/limit.sh -- npm ci --ignore-scripts)` or pass --tsc <path>".into())
    }
}

/// tsc 7's `bin/tsc` is a Node wrapper that starts the native binary: measure the binary
/// itself (`@typescript/typescript-<os>-<arch>/lib/tsc` next to the package) when it is there.
fn native_tsc(tsc: &Path) -> PathBuf {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        o => o,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        a => a,
    };
    // .../node_modules/typescript/bin/tsc -> .../node_modules
    if let Some(nm) = tsc.parent().and_then(Path::parent).and_then(Path::parent) {
        let native = nm.join(format!("@typescript/typescript-{os}-{arch}/lib/tsc"));
        if native.is_file() {
            return native;
        }
    }
    tsc.to_path_buf()
}

/// Identifies a binary by size and contents of its head and tail (cheap, stable).
fn binary_id(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    let head = &bytes[..bytes.len().min(1 << 20)];
    let tail = &bytes[bytes.len().saturating_sub(1 << 20)..];
    format!("{}-{:016x}-{:016x}", bytes.len(), fnv1a(head), fnv1a(tail))
}

fn ecosystem_cache() -> PathBuf {
    if let Some(d) = std::env::var_os("TS_R_CACHE_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d).join("ecosystem");
    }
    match std::env::var_os("HOME").filter(|d| !d.is_empty()) {
        Some(home) => PathBuf::from(home).join(".cache/ts-r/ecosystem"),
        None => root().join("target/ecosystem-cache"),
    }
}

/// The source tree of a case, prepared if needed, and a string naming its version.
fn source(case: &Case) -> Result<Option<(PathBuf, String)>> {
    if let Some(name) = &case.bench {
        let file = bench_repos::load()?;
        let Some(pin) = file.repos.iter().find(|r| &r.name == name) else {
            return Err(format!("no bench repo `{name}`").into());
        };
        if !bench_repos::is_fetched(pin) {
            eprintln!(
                "emit-repos: {}: skipped, run `cargo xtask bench --fetch-repos --repos {name}`",
                case.name
            );
            return Ok(None);
        }
        return Ok(Some((pin.dir(), format!("bench:{name}@{}", pin.commit))));
    }
    let cache = ecosystem_cache();
    let (key, origin) = match (&case.ecosystem, &case.ecosystem_repo) {
        (Some(pkg), None) => (pkg.clone(), cache.join("src").join(pkg.replace('/', "+"))),
        (None, Some(repo)) => (
            format!("repo-{repo}"),
            cache.join("repos").join(format!("{repo}.tgz")),
        ),
        _ => {
            return Err("a case needs exactly one of bench, ecosystem, ecosystem_repo".into());
        }
    };
    if !origin.exists() {
        eprintln!(
            "emit-repos: {}: skipped, {} is not in the ecosystem cache",
            case.name,
            origin.display()
        );
        return Ok(None);
    }
    let mut deps = case.deps.clone();
    deps.sort();
    let id = format!(
        "{key} deps={} remove={}",
        deps.join(","),
        case.remove.join(",")
    );
    // One copy per source and dependency set: cases that share them share the copy.
    let dir_name = format!(
        "{}-{:08x}",
        key.replace(['/', '@'], "_"),
        fnv1a(id.as_bytes()) as u32
    );
    let work = root().join(OUT_DIR).join("src").join(&dir_name);
    let stamp = root()
        .join(OUT_DIR)
        .join("src")
        .join(format!("{dir_name}.stamp"));
    if std::fs::read_to_string(&stamp).ok().as_deref() != Some(id.as_str()) || !work.is_dir() {
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work)?;
        if case.ecosystem.is_some() {
            let from = package_dir(&origin)
                .ok_or_else(|| format!("{} holds no package", origin.display()))?;
            copy_tree(&from, &work)?;
        } else {
            let status = Command::new("tar")
                .args(["xzf"])
                .arg(&origin)
                .args(["--strip-components", "1", "-C"])
                .arg(&work)
                .status()?;
            if !status.success() {
                return Err(format!("tar failed on {}", origin.display()).into());
            }
        }
        for gone in &case.remove {
            let _ = std::fs::remove_dir_all(work.join(gone));
        }
        for dep in &deps {
            let (name, _) = dep
                .rsplit_once('@')
                .filter(|(n, _)| !n.is_empty())
                .ok_or_else(|| format!("dep `{dep}` is not name@version"))?;
            let from = package_dir(&cache.join("src").join(dep.replace('/', "+")))
                .ok_or_else(|| format!("dep {dep} is not in the ecosystem cache"))?;
            copy_tree(&from, &work.join("node_modules").join(name))?;
        }
        std::fs::write(&stamp, &id)?;
    }
    Ok(Some((work, format!("ecosystem:{id}"))))
}

/// The unpacked package in an ecosystem cache entry: `package/`, or the one directory a
/// tarball holds (`@types/*` tarballs unpack into `node/`, `react/`, ...).
fn package_dir(entry: &Path) -> Option<PathBuf> {
    let package = entry.join("package");
    if package.is_dir() {
        return Some(package);
    }
    let dirs: Vec<PathBuf> = std::fs::read_dir(entry)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    match dirs.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    let mut entries: Vec<_> = std::fs::read_dir(from)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let ty = e.file_type()?;
        let dest = to.join(e.file_name());
        if ty.is_dir() {
            copy_tree(&e.path(), &dest)?;
        } else if ty.is_file() {
            std::fs::copy(e.path(), &dest)?;
        }
    }
    Ok(())
}

/// Files and directories of a tree (outside `node_modules` and `.git`), with size and mtime.
type Snapshot = BTreeMap<PathBuf, Option<(u64, std::time::SystemTime)>>;

fn snapshot(dir: &Path, out: &mut Snapshot) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.filter_map(|e| e.ok()) {
        let path = e.path();
        let Ok(ty) = e.file_type() else { continue };
        let name = e.file_name();
        if ty.is_dir() {
            if name == "node_modules" || name == ".git" {
                continue;
            }
            out.insert(path.clone(), None);
            snapshot(&path, out);
        } else if let Ok(m) = std::fs::symlink_metadata(&path) {
            out.insert(
                path,
                Some((m.len(), m.modified().unwrap_or(std::time::UNIX_EPOCH))),
            );
        }
    }
}

/// Moves what a run added under `tree` into `dest` (same relative paths) and removes the
/// directories it created. Returns the pre-existing files whose size or mtime changed.
fn collect_outputs(tree: &Path, before: &Snapshot, dest: &Path) -> Result<Vec<String>> {
    let mut after = Snapshot::new();
    snapshot(tree, &mut after);
    let mut modified = Vec::new();
    for (path, meta) in &after {
        let rel_path = path.strip_prefix(tree).unwrap_or(path);
        match (before.get(path), meta) {
            (None, Some(_)) => {
                let to = dest.join(rel_path);
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if std::fs::rename(path, &to).is_err() {
                    std::fs::copy(path, &to)?;
                    std::fs::remove_file(path)?;
                }
            }
            (Some(Some(old)), Some(new)) if old != new => {
                modified.push(slash(rel_path));
            }
            _ => {}
        }
    }
    // Deepest first, so nested new directories go before their parents.
    for (path, meta) in after.iter().rev() {
        if meta.is_none() && !before.contains_key(path) {
            let _ = std::fs::remove_dir(path);
        }
    }
    Ok(modified)
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn run_case(
    case: &Case,
    tsc: &Path,
    tsc_id: &str,
    ours: &Path,
    fresh: bool,
) -> Result<Option<CaseResult>> {
    let Some((tree, version)) = source(case)? else {
        return Ok(None);
    };
    let cwd = tree.join(&case.dir);
    let out = root().join(OUT_DIR).join("out").join(&case.name);
    let case_toml = serde_json::to_string(case)?;
    let tsc_key = format!(
        "{:016x}",
        fnv1a(format!("{tsc_id}\n{case_toml}\n{version}").as_bytes())
    );
    // Generated files go in before the snapshot, so they never count as outputs.
    let mut written = Vec::new();
    for (name, text) in &case.files {
        let p = cwd.join(name);
        if p.exists() && std::fs::read_to_string(&p).ok().as_deref() != Some(text.as_str()) {
            return Err(format!("{} already exists with other contents", p.display()).into());
        }
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, text)?;
        written.push(p);
    }
    let result = (|| -> Result<CaseResult> {
        let cached: Option<Run> = std::fs::read_to_string(out.join("tsc.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .filter(|r: &Run| r.key == tsc_key && out.join("tsc").is_dir());
        let tsc_run = match cached {
            Some(r) if !fresh => r,
            _ => {
                let _ = std::fs::remove_dir_all(out.join("tsc"));
                let mut cmd = timed(case);
                cmd.arg(tsc).env("GOMAXPROCS", THREADS);
                let mut r = run_tool(case, &tree, &cwd, cmd, &out.join("tsc"))?;
                r.key = tsc_key.clone();
                std::fs::write(out.join("tsc.json"), serde_json::to_string_pretty(&r)?)?;
                r
            }
        };
        let _ = std::fs::remove_dir_all(out.join("ours"));
        let mut cmd = timed(case);
        let cache_dir = root().join(OUT_DIR).join("ts-r-cache");
        let _ = std::fs::remove_dir_all(&cache_dir);
        cmd.arg(ours)
            .args(["--threads", THREADS])
            .env("RAYON_NUM_THREADS", THREADS)
            .env("TS_R_CACHE_DIR", &cache_dir)
            .env(
                "TS_R_LIB_DIR",
                root().join("target/upstream/typescript/tsc/internal/bundled/libs"),
            );
        let ours_run = run_tool(case, &tree, &cwd, cmd, &out.join("ours"))?;
        std::fs::write(
            out.join("ours.json"),
            serde_json::to_string_pretty(&ours_run)?,
        )?;
        Ok(compare(case, &out, tsc_run, ours_run))
    })();
    for p in written {
        let _ = std::fs::remove_file(p);
    }
    let r = result?;
    let mut diff = String::new();
    for d in &r.differing {
        let _ = writeln!(diff, "{d}");
    }
    for d in &r.diags_tsc_only {
        let _ = writeln!(diff, "diagnostic only from tsc: {d}");
    }
    for d in &r.diags_ours_only {
        let _ = writeln!(diff, "diagnostic only from ts-r: {d}");
    }
    std::fs::write(out.join("diff.txt"), diff)?;
    Ok(Some(r))
}

fn timed(case: &Case) -> Command {
    let mut cmd = limit_cmd(case.mem_mb, case.time_s);
    cmd.arg("/usr/bin/time").arg(if cfg!(target_os = "macos") {
        "-l"
    } else {
        "-v"
    });
    cmd
}

fn run_tool(case: &Case, tree: &Path, cwd: &Path, mut cmd: Command, dest: &Path) -> Result<Run> {
    let mut before = Snapshot::new();
    snapshot(tree, &mut before);
    cmd.args(&case.args)
        .args(["--pretty", "false"])
        .current_dir(cwd)
        .env("NO_COLOR", "1");
    let start = Instant::now();
    let output = cmd.output()?;
    let seconds = start.elapsed().as_secs_f64();
    let stderr = String::from_utf8_lossy(&output.stderr);
    std::fs::create_dir_all(dest)?;
    let modified = collect_outputs(tree, &before, dest)?;
    let stdout = String::from_utf8_lossy(&output.stdout).replace(&slash(cwd), "<cwd>");
    for line in stderr.lines() {
        if line.starts_with("limit.sh: killed") || line.contains("panicked") {
            eprintln!("emit-repos: {}: {line}", case.name);
        }
    }
    Ok(Run {
        key: String::new(),
        exit: output.status.code(),
        rss_mb: parse_max_rss_mb(&stderr),
        seconds,
        stdout,
        modified,
    })
}

const KINDS: &[&str] = &[
    "js",
    "js.map",
    "d.ts",
    "d.ts.map",
    "json",
    "tsbuildinfo",
    "other",
];

fn kind_of(path: &str) -> &'static str {
    let ends = |s: &[&str]| s.iter().any(|x| path.ends_with(x));
    if ends(&[".d.ts.map", ".d.mts.map", ".d.cts.map"]) {
        "d.ts.map"
    } else if ends(&[".d.ts", ".d.mts", ".d.cts"]) {
        "d.ts"
    } else if ends(&[".js.map", ".mjs.map", ".cjs.map", ".jsx.map"]) {
        "js.map"
    } else if ends(&[".js", ".mjs", ".cjs", ".jsx"]) {
        "js"
    } else if ends(&[".tsbuildinfo"]) {
        "tsbuildinfo"
    } else if ends(&[".json"]) {
        "json"
    } else {
        "other"
    }
}

fn files_under(dir: &Path) -> BTreeSet<String> {
    let mut snap = Snapshot::new();
    snapshot(dir, &mut snap);
    snap.into_iter()
        .filter(|(_, m)| m.is_some())
        .map(|(p, _)| slash(p.strip_prefix(dir).unwrap_or(&p)))
        .collect()
}

fn compare(case: &Case, out: &Path, tsc: Run, ours: Run) -> CaseResult {
    let (a, b) = (out.join("tsc"), out.join("ours"));
    let fa = files_under(&a);
    let fb = files_under(&b);
    let mut kinds: BTreeMap<&'static str, KindCount> = BTreeMap::new();
    let mut differing = Vec::new();
    for f in fa.union(&fb) {
        let kind = kind_of(f);
        let k = kinds.entry(kind).or_default();
        match (fa.contains(f), fb.contains(f)) {
            (true, false) => {
                k.tsc_only += 1;
                differing.push(format!("only from tsc: {f}"));
            }
            (false, true) => {
                k.ours_only += 1;
                differing.push(format!("only from ts-r: {f}"));
            }
            _ => {
                let x = std::fs::read(a.join(f)).unwrap_or_default();
                let y = std::fs::read(b.join(f)).unwrap_or_default();
                if kind == "tsbuildinfo" || x == y {
                    k.identical += 1;
                } else {
                    k.differ += 1;
                    differing.push(format!("differs: {f} ({})", first_difference(&x, &y)));
                }
            }
        }
    }
    let da = diagnostics(&tsc.stdout);
    let db = diagnostics(&ours.stdout);
    let mut same = 0;
    let mut tsc_only = Vec::new();
    let mut ours_only = Vec::new();
    for (d, n) in &da {
        let m = db.get(d).copied().unwrap_or(0);
        same += (*n).min(m);
        for _ in m..*n {
            tsc_only.push(d.clone());
        }
    }
    for (d, n) in &db {
        let m = da.get(d).copied().unwrap_or(0);
        for _ in m..*n {
            ours_only.push(d.clone());
        }
    }
    CaseResult {
        name: case.name.clone(),
        style: case.style.clone(),
        gate: case.gate,
        kinds,
        tsc,
        ours,
        diags_same: same,
        diags_tsc_only: tsc_only,
        diags_ours_only: ours_only,
        differing,
    }
}

/// `line N: tsc «..» / ts-r «..»` for the first differing line.
fn first_difference(a: &[u8], b: &[u8]) -> String {
    let a = String::from_utf8_lossy(a);
    let b = String::from_utf8_lossy(b);
    let (mut la, mut lb) = (a.split('\n'), b.split('\n'));
    let mut n = 1;
    loop {
        match (la.next(), lb.next()) {
            (Some(x), Some(y)) if x == y => n += 1,
            (x, y) => {
                let cut = |s: Option<&str>| {
                    let s = s.unwrap_or("<end>");
                    s.chars().take(160).collect::<String>()
                };
                return format!("line {n}: tsc «{}» / ts-r «{}»", cut(x), cut(y));
            }
        }
    }
}

/// Diagnostics of `--pretty false` output as `file(line,col): CODE` (global: `CODE`), counted.
fn diagnostics(stdout: &str) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for line in stdout.lines() {
        if line.starts_with(' ') {
            continue;
        }
        let key = if let Some(rest) = line.strip_prefix("error ") {
            rest.split(':').next().map(str::to_string)
        } else if let Some(i) = line.find("): error ") {
            let code = line[i + 9..].split(':').next().unwrap_or("");
            Some(format!("{} {code}", &line[..=i]))
        } else {
            None
        };
        if let Some(k) = key {
            *out.entry(k).or_default() += 1;
        }
    }
    out
}

fn mb(v: Option<f64>) -> String {
    v.map_or("-".into(), |v| format!("{v:.0}"))
}

fn exit(v: Option<i32>) -> String {
    v.map_or("killed".into(), |c| c.to_string())
}

/// The per-case and per-kind tables, and whether the gate passed.
fn report(results: &[CaseResult], failed: &[&str]) -> (String, bool) {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "| Case | Style | Files identical | js | js.map | d.ts | d.ts.map | other | Exit tsc / ts-r | Diagnostics same / tsc only / ts-r only | Peak RSS MB tsc / ts-r | Time s tsc / ts-r |"
    );
    let _ = writeln!(s, "|---|---|---|---|---|---|---|---|---|---|---|---|");
    let mut all = KindCount::default();
    let mut per_kind: BTreeMap<&str, KindCount> = BTreeMap::new();
    let mut rss_ok = true;
    for r in results {
        let mut t = KindCount::default();
        for (k, c) in &r.kinds {
            t.identical += c.identical;
            t.differ += c.differ;
            t.tsc_only += c.tsc_only;
            t.ours_only += c.ours_only;
            if !r.gate {
                continue;
            }
            let e = per_kind.entry(k).or_default();
            e.identical += c.identical;
            e.differ += c.differ;
            e.tsc_only += c.tsc_only;
            e.ours_only += c.ours_only;
        }
        if r.gate {
            all.identical += t.identical;
            all.differ += t.differ;
            all.tsc_only += t.tsc_only;
            all.ours_only += t.ours_only;
        }
        let cell = |k: &str| {
            r.kinds
                .get(k)
                .map_or("-".into(), |c| format!("{}/{}", c.identical, c.total()))
        };
        let other = {
            let mut c = KindCount::default();
            for k in ["json", "tsbuildinfo", "other"] {
                if let Some(x) = r.kinds.get(k) {
                    c.identical += x.identical;
                    c.differ += x.differ + x.tsc_only + x.ours_only;
                }
            }
            if c.total() == 0 {
                "-".into()
            } else {
                format!("{}/{}", c.identical, c.total())
            }
        };
        let ok = match (r.tsc.rss_mb, r.ours.rss_mb) {
            (Some(a), Some(b)) => b <= a,
            _ => false,
        };
        rss_ok &= ok || !r.gate;
        let tsr = r
            .diags_ours_only
            .iter()
            .filter(|d| d.contains("TSR"))
            .count();
        let _ = writeln!(
            s,
            "| {} | {} | {}/{} ({:.1}%) | {} | {} | {} | {} | {} | {} / {} | {} / {} / {}{} | {} / {}{} | {:.2} / {:.2} |",
            if r.gate {
                r.name.clone()
            } else {
                format!("{} (not gated)", r.name)
            },
            r.style,
            t.identical,
            t.total(),
            pct(t.identical, t.total()),
            cell("js"),
            cell("js.map"),
            cell("d.ts"),
            cell("d.ts.map"),
            other,
            exit(r.tsc.exit),
            exit(r.ours.exit),
            r.diags_same,
            r.diags_tsc_only.len(),
            r.diags_ours_only.len(),
            if tsr > 0 {
                format!(" ({tsr} TSR)")
            } else {
                String::new()
            },
            mb(r.tsc.rss_mb),
            mb(r.ours.rss_mb),
            if ok { "" } else { " (over)" },
            r.tsc.seconds,
            r.ours.seconds,
        );
        if !r.tsc.modified.is_empty() || !r.ours.modified.is_empty() {
            let _ = writeln!(
                s,
                "|   | warning: changed {} / {} existing files (tsc / ts-r), e.g. {} | | | | | | | | | | |",
                r.tsc.modified.len(),
                r.ours.modified.len(),
                r.tsc
                    .modified
                    .first()
                    .or(r.ours.modified.first())
                    .map_or("", |s| s.as_str())
            );
        }
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "| Kind | Identical | Differ | Only tsc | Only ts-r |");
    let _ = writeln!(s, "|---|---|---|---|---|");
    for k in KINDS {
        if let Some(c) = per_kind.get(k) {
            let _ = writeln!(
                s,
                "| {k} | {}/{} ({:.1}%) | {} | {} | {} |",
                c.identical,
                c.total(),
                pct(c.identical, c.total()),
                c.differ,
                c.tsc_only,
                c.ours_only
            );
        }
    }
    let gated: Vec<&CaseResult> = results.iter().filter(|r| r.gate).collect();
    let rate = all.identical as f64 / all.total().max(1) as f64;
    let pass = rate >= GATE_IDENTICAL && rss_ok && !gated.is_empty() && failed.is_empty();
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "Gated cases, all files: {}/{} byte-identical ({:.1}%, gate {:.0}%); peak RSS at or under tsc 7 on {}/{} cases. Gate: {}.",
        all.identical,
        all.total(),
        rate * 100.0,
        GATE_IDENTICAL * 100.0,
        gated
            .iter()
            .filter(|r| matches!((r.tsc.rss_mb, r.ours.rss_mb), (Some(a), Some(b)) if b <= a))
            .count(),
        gated.len(),
        if pass { "met" } else { "not met" }
    );
    if !failed.is_empty() {
        let _ = writeln!(s, "Failed cases: {}.", failed.join(", "));
    }
    let _ = writeln!(
        s,
        "Differences per case: {}/out/<case>/diff.txt.",
        rel(&root().join(OUT_DIR))
    );
    (s, pass)
}

fn pct(a: usize, b: usize) -> f64 {
    if b == 0 {
        100.0
    } else {
        a as f64 * 100.0 / b as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_error_fails_gate_and_preserves_partial_report() {
        let results = [CaseResult {
            name: "passing".into(),
            style: "CommonJS".into(),
            gate: true,
            kinds: BTreeMap::from([(
                "js",
                KindCount {
                    identical: 1,
                    ..KindCount::default()
                },
            )]),
            tsc: Run {
                rss_mb: Some(10.0),
                ..Run::default()
            },
            ours: Run {
                rss_mb: Some(5.0),
                ..Run::default()
            },
            diags_same: 0,
            diags_tsc_only: Vec::new(),
            diags_ours_only: Vec::new(),
            differing: Vec::new(),
        }];
        assert!(report(&results, &[]).1);
        let (table, pass) = report(&results, &["failed"]);
        assert!(!pass);
        assert!(table.contains("| passing |"));
        assert!(table.contains("Failed cases: failed."));
        assert!(table.contains("Gate: not met."));
        assert!(!report(&[], &["failed"]).1);
    }

    #[test]
    fn manifest_parses() {
        let m: Manifest = toml::from_str(&read_to_string(&root().join(MANIFEST)).unwrap())
            .expect("bench/emit-repos.toml parses");
        let mut names = BTreeSet::new();
        for c in &m.cases {
            assert!(names.insert(c.name.clone()), "duplicate case {}", c.name);
            assert!(c.mem_mb <= MAX_MEM_MB, "{}", c.name);
            let sources = [
                c.bench.is_some(),
                c.ecosystem.is_some(),
                c.ecosystem_repo.is_some(),
            ];
            assert_eq!(sources.iter().filter(|x| **x).count(), 1, "{}", c.name);
        }
    }

    #[test]
    fn kinds() {
        assert_eq!(kind_of("dist/a.d.ts.map"), "d.ts.map");
        assert_eq!(kind_of("dist/a.d.mts"), "d.ts");
        assert_eq!(kind_of("dist/a.cjs.map"), "js.map");
        assert_eq!(kind_of("dist/a.js"), "js");
        assert_eq!(kind_of(".ts/tsconfig.tsbuildinfo"), "tsbuildinfo");
        assert_eq!(kind_of("dist/data.json"), "json");
    }

    #[test]
    fn diagnostic_keys() {
        let out = "src/a.ts(1,2): error TS2322: Type 'x'\n  more\nerror TS5011: The common\nsrc/a.ts(1,2): error TS2322: again\n";
        let d = diagnostics(out);
        assert_eq!(d.get("src/a.ts(1,2) TS2322"), Some(&2));
        assert_eq!(d.get("TS5011"), Some(&1));
    }

    #[test]
    fn first_difference_names_the_line() {
        assert_eq!(
            first_difference(b"a\nb\nc", b"a\nx\nc"),
            "line 2: tsc «b» / ts-r «x»"
        );
    }
}
