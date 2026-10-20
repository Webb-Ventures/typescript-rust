//! `cargo xtask pgo`: profile-guided optimisation of the shipped binaries.
//!
//! 1. Finds `llvm-profdata` in rustup's `llvm-tools` component (installs it only if missing).
//! 2. Builds the seven binaries instrumented (`-Cprofile-generate`) into `target/pgo/instrumented`.
//! 3. Runs a read-only training workload: ts-r-check, ts-r-lint, ts-r-build and ts-r-run over
//!    the pinned public bench repos that are fetched (`target/bench-repos`, from `cargo xtask
//!    bench --fetch-repos`; never written to), ecosystem package samples
//!    (`~/.cache/ts-r/ecosystem/src`, from `cargo xtask ecosystem`), and the repo's own fixtures
//!    and synced TypeScript tests. Every command goes through `scripts/limit.sh` (4096 MB, 300 s);
//!    on Windows, through a Job Object with the same caps (`ts_r_testing::limited`).
//! 4. Merges the raw profiles with `llvm-profdata merge`.
//! 5. Rebuilds with `-Cprofile-use` into `target/pgo/optimized/<target>/<profile>/`.
//!
//! ts-r-dev and ts-r-lsp are servers and are not driven directly; they are built from the same
//! crates (parser, transform, checker, bundler) and use the same merged profile.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Instant;

use crate::args::Args;
use crate::bench_repos::{self, RepoPin};
use crate::util::{Result, rel, root, run_capture, run_status};

/// Cargo packages holding the shipped binaries, and the binary each one builds.
pub const BINARIES: &[(&str, &str)] = &[
    ("ts_r_cli", "ts-r"),
    ("ts_r_run", "ts-r-run"),
    ("ts_r_dev", "ts-r-dev"),
    ("ts_r_build", "ts-r-build"),
    ("ts_r_check", "ts-r-check"),
    ("ts_r_lint_cli", "ts-r-lint"),
    ("ts_r_lsp", "ts-r-lsp"),
];

/// Where the TypeScript lib files are synced (`cargo xtask upstream sync --minimal`).
pub const LIB_DIR: &str = "target/upstream/typescript/tsc/internal/bundled/libs";

const HELP: &str = "\
cargo xtask pgo [--profile release|dist] [--target <triple>] [--jobs N] [--repos <names|all>]
                [--ecosystem <dir>] [--samples N] [--builder cargo|zigbuild]
                [--use-profile <file.profdata>] [--profile-out <file>] [--no-rebuild]
                [--keep-profiles]

Builds instrumented binaries, trains them on the fetched public bench repos (read-only;
default: every repo of bench/repos.toml fetched into target/bench-repos, `--repos` narrows it),
ecosystem samples and repo fixtures, merges the profiles with llvm-profdata and rebuilds with
-Cprofile-use into
target/pgo/optimized/<target>/<profile>/. --use-profile skips straight to the rebuild with an
existing profile (for builds of the same architecture: musl, or an older glibc with
--builder zigbuild --target x86_64-unknown-linux-gnu.2.17); --no-rebuild stops after merging.
";

struct Options {
    profile: String,
    target: String,
    jobs: String,
    /// `--repos` (names or `all`); `None` = every fetched repo.
    repos: Option<String>,
    ecosystem: Option<PathBuf>,
    samples: usize,
    builder: String,
    use_profile: Option<PathBuf>,
    profile_out: PathBuf,
    rebuild: bool,
    keep_profiles: bool,
}

pub fn run(mut args: Args) -> Result<ExitCode> {
    if args.flag("help") {
        print!("{HELP}");
        return Ok(ExitCode::SUCCESS);
    }
    let root = root();
    let opts = Options {
        profile: args.opt("profile")?.unwrap_or_else(|| "release".into()),
        target: match args.opt("target")? {
            Some(t) => t,
            None => host_target()?,
        },
        jobs: args.opt("jobs")?.unwrap_or_else(|| "4".into()),
        repos: args.opt("repos")?,
        ecosystem: args.opt("ecosystem")?.map(PathBuf::from).or_else(|| {
            let home = std::env::var_os("HOME")?;
            let d = PathBuf::from(home).join(".cache/ts-r/ecosystem/src");
            d.is_dir().then_some(d)
        }),
        samples: args
            .opt("samples")?
            .map(|s| s.parse().map_err(|_| format!("--samples: bad number {s}")))
            .transpose()?
            .unwrap_or(40),
        builder: args.opt("builder")?.unwrap_or_else(|| "cargo".into()),
        use_profile: args.opt("use-profile")?.map(PathBuf::from),
        profile_out: args
            .opt("profile-out")?
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("target/pgo/merged.profdata")),
        rebuild: !args.flag("no-rebuild"),
        keep_profiles: args.flag("keep-profiles"),
    };
    args.finish()?;
    if !matches!(opts.builder.as_str(), "cargo" | "zigbuild") {
        return Err(format!("--builder must be cargo or zigbuild, not {}", opts.builder).into());
    }
    let pgo = root.join("target/pgo");
    std::fs::create_dir_all(&pgo)?;

    let profdata = match &opts.use_profile {
        Some(p) => {
            if !p.is_file() {
                return Err(format!("--use-profile {}: no such file", p.display()).into());
            }
            p.canonicalize()?
        }
        None => {
            let llvm_profdata = find_llvm_profdata()?;
            let raw = pgo.join("profiles");
            let _ = std::fs::remove_dir_all(&raw);
            std::fs::create_dir_all(&raw)?;

            let t = Instant::now();
            let flags = format!("-Cprofile-generate={}", raw.display());
            build(&opts, &pgo.join("instrumented"), &flags, false)?;
            eprintln!(
                "pgo: instrumented build in {:.0} s",
                t.elapsed().as_secs_f64()
            );

            let bin = pgo
                .join("instrumented")
                .join(triple_dir(&opts.target))
                .join(&opts.profile);
            let t = Instant::now();
            let stats = train(&opts, &bin, &raw)?;
            eprintln!(
                "pgo: training ran {} commands ({} exited non-zero, {} killed or crashed) in {:.0} s",
                stats.runs,
                stats.nonzero,
                stats.crashed,
                t.elapsed().as_secs_f64()
            );

            let mut inputs: Vec<PathBuf> = std::fs::read_dir(&raw)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e == "profraw"))
                .collect();
            inputs.sort();
            if inputs.is_empty() {
                return Err("training wrote no .profraw files".into());
            }
            let list = pgo.join("profraw-files.txt");
            let text: Vec<String> = inputs.iter().map(|p| p.display().to_string()).collect();
            std::fs::write(&list, text.join("\n") + "\n")?;
            run_status(
                Command::new(&llvm_profdata)
                    .arg("merge")
                    .arg("-o")
                    .arg(&opts.profile_out)
                    .arg(format!("--input-files={}", list.display())),
            )?;
            let size = std::fs::metadata(&opts.profile_out)?.len();
            eprintln!(
                "pgo: merged {} raw profiles into {} ({} KB)",
                inputs.len(),
                rel(&opts.profile_out),
                size / 1024
            );
            let _ = std::fs::remove_file(&list);
            if !opts.keep_profiles {
                std::fs::remove_dir_all(&raw)?;
            }
            opts.profile_out.canonicalize()?
        }
    };

    if opts.rebuild {
        let t = Instant::now();
        // Functions the training never reached, or code that differs between targets of the
        // same architecture (--use-profile), are optimised normally instead of warning.
        let flags = format!(
            "-Cprofile-use={} -Cllvm-args=-no-pgo-warn-mismatch",
            profdata.display()
        );
        build(&opts, &pgo.join("optimized"), &flags, true)?;
        let out = pgo
            .join("optimized")
            .join(triple_dir(&opts.target))
            .join(&opts.profile);
        eprintln!(
            "pgo: optimized build in {:.0} s: {}\n     compare: cargo xtask bench --bin-dir {}",
            t.elapsed().as_secs_f64(),
            rel(&out),
            rel(&out)
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Cargo's output directory for a target. cargo-zigbuild accepts a glibc version suffix
/// (`x86_64-unknown-linux-gnu.2.17`) but builds into the plain triple's directory.
pub fn triple_dir(target: &str) -> &str {
    match target.split_once("-gnu.") {
        Some((base, _)) => &target[..base.len() + 4],
        None => target,
    }
}

fn host_target() -> Result<String> {
    let out = run_capture(Command::new("rustc").arg("-vV").current_dir(root()))?;
    out.lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(|s| s.trim().to_string())
        .ok_or_else(|| "rustc -vV printed no host".into())
}

/// `llvm-profdata` from the active toolchain's `llvm-tools` component, matching rustc's LLVM.
fn find_llvm_profdata() -> Result<PathBuf> {
    let sysroot = run_capture(
        Command::new("rustc")
            .args(["--print", "sysroot"])
            .current_dir(root()),
    )?;
    let host = host_target()?;
    let exe = if cfg!(windows) {
        "llvm-profdata.exe"
    } else {
        "llvm-profdata"
    };
    let path = Path::new(sysroot.trim())
        .join("lib/rustlib")
        .join(&host)
        .join("bin")
        .join(exe);
    if path.is_file() {
        return Ok(path);
    }
    eprintln!("pgo: installing rustup component llvm-tools (llvm-profdata)");
    let added = run_status(
        Command::new("rustup")
            .args(["component", "add", "llvm-tools"])
            .current_dir(root()),
    );
    if added.is_err() {
        run_status(
            Command::new("rustup")
                .args(["component", "add", "llvm-tools-preview"])
                .current_dir(root()),
        )?;
    }
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "llvm-profdata not found at {} after install",
            path.display()
        )
        .into())
    }
}

fn build(
    opts: &Options,
    target_dir: &Path,
    rustflags: &str,
    with_bench_example: bool,
) -> Result<()> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(root())
        .arg(if opts.builder == "zigbuild" {
            "zigbuild"
        } else {
            "build"
        })
        .args(["--profile", &opts.profile, "-j", &opts.jobs])
        .args(["--target", &opts.target])
        .arg("--target-dir")
        .arg(target_dir);
    for (pkg, _) in BINARIES {
        cmd.args(["-p", pkg]);
    }
    cmd.arg("--bins");
    if with_bench_example {
        // `cargo xtask bench`'s build-incremental scenario runs this example from --bin-dir.
        cmd.args(["-p", "ts_r_bundler", "--example", "build_loop"]);
    }
    // With an explicit --target, RUSTFLAGS reaches only the target crates, not build scripts.
    let mut flags = std::env::var("RUSTFLAGS").unwrap_or_default();
    if !flags.is_empty() {
        flags.push(' ');
    }
    flags.push_str(rustflags);
    cmd.env("RUSTFLAGS", flags);
    run_status(&mut cmd)
}

#[derive(Default)]
struct Stats {
    runs: usize,
    nonzero: usize,
    crashed: usize,
}

struct Trainer<'a> {
    bin: &'a Path,
    env: Vec<(String, PathBuf)>,
    limit: Option<PathBuf>,
    stats: Stats,
}

impl Trainer<'_> {
    /// Runs one of our binaries under scripts/limit.sh (4096 MB, 300 s), output discarded.
    /// Windows has no limit.sh: there the run gets the same caps in a Job Object
    /// (`ts_r_testing::limited`).
    fn run(&mut self, tool: &str, args: &[&str], cwd: &Path) {
        let exe = self
            .bin
            .join(format!("{tool}{}", std::env::consts::EXE_SUFFIX));
        self.stats.runs += 1;
        let status = if cfg!(windows) {
            let mut cmd = ts_r_testing::limited(&exe, 4096, 300);
            cmd.args(args)
                .current_dir(cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            for (k, v) in &self.env {
                cmd.env(k, v);
            }
            cmd.status()
        } else {
            let mut cmd = match &self.limit {
                Some(limit) => {
                    let mut c = Command::new("bash");
                    c.arg(limit)
                        .args(["--mem-mb", "4096", "--time-s", "300", "--"])
                        .arg(&exe);
                    c
                }
                None => Command::new(&exe),
            };
            cmd.args(args)
                .current_dir(cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            for (k, v) in &self.env {
                cmd.env(k, v);
            }
            cmd.status()
        };
        match status {
            Ok(s) if s.success() => {}
            // 1/2: diagnostics or lint errors (expected on real code); 101: a Rust panic,
            // 124/137: limit.sh (or the Windows job) killed it; no code: a signal.
            Ok(s) if matches!(s.code(), Some(1 | 2)) => self.stats.nonzero += 1,
            Ok(s) => {
                self.stats.crashed += 1;
                eprintln!("pgo: {tool} {} ({}) -> {s}", args.join(" "), cwd.display());
            }
            Err(e) => {
                self.stats.crashed += 1;
                eprintln!("pgo: could not start {}: {e}", exe.display());
            }
        }
    }
}

fn train(opts: &Options, bin: &Path, raw: &Path) -> Result<Stats> {
    let root = root();
    let libs = root.join(LIB_DIR);
    if !libs.join("lib.es5.d.ts").is_file() {
        return Err(
            "no TypeScript lib files: run `cargo xtask upstream sync --minimal` first".into(),
        );
    }
    let tmp = root.join("target/pgo/train-tmp");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("cache"))?;
    let limit = root.join("scripts/limit.sh");
    let mut t = Trainer {
        bin,
        env: vec![
            ("LLVM_PROFILE_FILE".into(), raw.join("%m-%p.profraw")),
            ("TS_R_CACHE_DIR".into(), tmp.join("cache")),
            ("TS_R_LIB_DIR".into(), libs),
            // The bundler defaults to up to 8 workers; corpus runs use at most 4 threads.
            ("TS_R_THREADS".into(), "4".into()),
        ],
        limit: (!cfg!(windows) && limit.is_file()).then_some(limit),
        stats: Stats::default(),
    };
    let out_js = tmp.join("out.js");
    let out_js = out_js.to_string_lossy().into_owned();
    let lint_report = tmp.join("lint.json");
    let lint_report = lint_report.to_string_lossy().into_owned();

    // An ESLint flat config turning on every built-in rule (no imports, so it loads anywhere).
    let rules = run_capture(
        Command::new(bin.join(format!("ts-r-lint{}", std::env::consts::EXE_SUFFIX)))
            .arg("--list-rules")
            .env("LLVM_PROFILE_FILE", raw.join("%m-%p.profraw")),
    )?;
    let names: Vec<String> = rules
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(|r| format!("{r:?}"))
        .collect();
    let lint_config = tmp.join("eslint.config.mjs");
    std::fs::write(
        &lint_config,
        format!(
            "export default [{{ ignores: ['**/dist/**', '**/.next/**', '**/node_modules/**', '**/generated/**'] }},\n  {{ files: ['**/*.{{js,mjs,cjs,jsx,ts,tsx,mts,cts}}'], rules: Object.fromEntries([{}].map((r) => [r, 'error'])) }}];\n",
            names.join(", ")
        ),
    )?;
    let lint_config = lint_config.to_string_lossy().into_owned();

    t.run("ts-r", &["--version"], &root);
    t.run("ts-r", &["doctor"], &root);

    // Type-check projects: the fetched bench repos' check wrappers first (noEmit, written by
    // the fetch), then the repo's tsconfig fixtures.
    let manifest = bench_repos::load()?;
    let repos = training_repos(&manifest, opts.repos.as_deref())?;
    if repos.is_empty() {
        eprintln!(
            "pgo: no bench repos fetched; training without them (cargo xtask bench --fetch-repos --repos date-fns,trpc,three-x10,excalidraw)"
        );
    } else {
        let names: Vec<&str> = repos.iter().map(|r| r.name.as_str()).collect();
        eprintln!("pgo: training on bench repos {}", names.join(", "));
    }
    let mut projects = Vec::new();
    for r in &repos {
        for p in &r.projects {
            let wrapper = r.dir().join(bench_repos::wrapper_path(p));
            if wrapper.is_file() {
                projects.push(wrapper);
            }
        }
    }
    let mut fixtures = Vec::new();
    for c in ["ts_r_history", "ts_r_guard", "ts_r_config", "ts_r_resolver"] {
        find_named(
            &root.join("crates").join(c).join("tests/fixtures"),
            "tsconfig.json",
            &mut fixtures,
            4,
        );
    }
    projects.extend(fixtures);
    for (i, cfg) in projects.iter().enumerate() {
        let dir = cfg.parent().unwrap_or(&root);
        let cfg = cfg.to_string_lossy();
        t.run(
            "ts-r-check",
            &["-p", &cfg, "--noEmit", "--pretty", "false", "--no-history"],
            dir,
        );
        // History: a cold run writes it, a warm run reads it back.
        t.run("ts-r-check", &["-p", &cfg, "--noEmit"], dir);
        t.run("ts-r-check", &["-p", &cfg, "--noEmit"], dir);
        if i < 6 {
            t.run(
                "ts-r-check",
                &["-p", &cfg, "--noEmit", "--sound=strict", "--sound-report"],
                dir,
            );
        }
    }

    // Lint: all rules over the bench repos' sources, the ecosystem samples and the repo's JS.
    for r in &repos {
        let dirs = source_dirs(r);
        if dirs.is_empty() {
            continue;
        }
        for format in ["json", "stylish"] {
            let mut a: Vec<&str> = vec![
                "--no-gitignore",
                "-c",
                &lint_config,
                "--threads",
                "4",
                "-f",
                format,
                "-o",
                &lint_report,
            ];
            a.extend(dirs.iter().map(String::as_str));
            t.run("ts-r-lint", &a, &r.dir());
        }
    }
    // `landing` is not in the public repo (the release workflow trains there).
    for dir in ["editors/vscode", "landing", "bench/js", "ecosystem/js"] {
        if !root.join(dir).is_dir() {
            continue;
        }
        t.run(
            "ts-r-lint",
            &[dir, "-c", &lint_config, "-f", "json", "-o", &lint_report],
            &root,
        );
    }

    // Bundle: the bench repos' app and bundler inputs (excalidraw's harness entry, three.js
    // x10), cold and with the cache, minified and not; then the ecosystem samples' entries.
    let outdir = format!("--outdir={}", tmp.join("out").display());
    for r in &repos {
        let dir = r.dir();
        let (entry, extra): (&str, &[&str]) = match r.prepare.as_deref() {
            Some("dev-harness") => (
                "ts-r-bench-dev/main.tsx",
                &[
                    "--format=esm",
                    "--jsx=automatic",
                    "--loader:.woff2=file",
                    "--loader:.scss=empty",
                ],
            ),
            Some("three-x10") => ("bench/entry.js", &["--format=iife", "--global-name=THREE"]),
            _ => continue,
        };
        if !dir.join(entry).is_file() {
            continue;
        }
        let variants: [&[&str]; 4] = [
            &["--minify", "--no-cache"],
            &["--no-cache"],
            &["--minify", "--sourcemap"],
            &["--minify", "--sourcemap"],
        ];
        for v in variants {
            let mut a: Vec<&str> = vec![entry, "--bundle", "--log-level=error", &outdir];
            a.extend_from_slice(extra);
            a.extend_from_slice(v);
            t.run("ts-r-build", &a, &dir);
        }
    }
    if let Some(eco) = &opts.ecosystem {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(eco)
            .map(|rd| {
                rd.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default();
        dirs.sort();
        // Spread the samples over the (download-ranked) list instead of taking the first N.
        let step = (dirs.len() / opts.samples.max(1)).max(1);
        for dir in dirs.iter().step_by(step).take(opts.samples) {
            if let Some(entry) = package_entry(dir) {
                let e = entry.to_string_lossy();
                for extra in ["--minify", "--sourcemap"] {
                    t.run(
                        "ts-r-build",
                        &[
                            &e,
                            "--bundle",
                            "--platform=node",
                            extra,
                            "--no-cache",
                            "--log-level=error",
                            &format!("--outfile={out_js}"),
                        ],
                        dir,
                    );
                }
            }
            t.run(
                "ts-r-lint",
                &[
                    ".",
                    "--no-gitignore",
                    "-c",
                    &lint_config,
                    "-f",
                    "json",
                    "-o",
                    &lint_report,
                ],
                dir,
            );
        }
    } else {
        eprintln!(
            "pgo: no ecosystem samples (run `cargo xtask ecosystem` once, or pass --ecosystem)"
        );
    }

    // Transpile: ts-r-run --compile prints the compiled module without running it.
    let mut sources = Vec::new();
    for r in &repos {
        for d in source_dirs(r) {
            find_sources(&r.dir().join(d), &mut sources, 60);
        }
    }
    find_sources(
        &root.join("target/upstream/typescript/tests/cases/compiler"),
        &mut sources,
        300,
    );
    find_sources(
        &root.join("crates/ts_r_guard/tests/fixtures"),
        &mut sources,
        50,
    );
    for src in &sources {
        let s = src.to_string_lossy();
        t.run(
            "ts-r-run",
            &["--compile", &s],
            src.parent().unwrap_or(&root),
        );
    }
    let hello = root.join("bench/fixtures/hello.ts");
    // Cold (no cache), then a cache fill and a warm run.
    let variants: [&[&str]; 3] = [&["--no-cache"], &[], &[]];
    for extra in variants {
        let mut a: Vec<&str> = extra.to_vec();
        let h = hello.to_string_lossy();
        a.push(&h);
        t.run("ts-r-run", &a, &root);
    }

    let _ = std::fs::remove_dir_all(&tmp);
    Ok(t.stats)
}

/// The bench repos to train on: `--repos` (names or `all`, which must be fetched), else every
/// repo of the manifest that is fetched at its pinned commit. Never fetches.
fn training_repos<'a>(
    manifest: &'a bench_repos::ReposFile,
    spec: Option<&str>,
) -> Result<Vec<&'a RepoPin>> {
    match spec {
        Some(spec) => {
            let picked = bench_repos::select(manifest, spec, true)?;
            if let Some(r) = picked.iter().find(|r| !bench_repos::is_fetched(r)) {
                return Err(format!(
                    "--repos: {} is not fetched (cargo xtask bench --fetch-repos --repos {})",
                    r.name, r.name
                )
                .into());
            }
            Ok(picked)
        }
        None => Ok(manifest
            .repos
            .iter()
            .filter(|r| bench_repos::is_fetched(r))
            .collect()),
    }
}

/// A bench repo's source directories (its editor set's `src_dirs`, else the excalidraw
/// harness's app and packages), relative to the checkout and present on disk.
fn source_dirs(r: &RepoPin) -> Vec<String> {
    let mut dirs: Vec<String> = r
        .editor
        .as_ref()
        .map(|e| e.src_dirs.clone())
        .unwrap_or_default();
    if dirs.is_empty() && r.prepare.as_deref() == Some("dev-harness") {
        dirs = vec!["excalidraw-app".into(), "packages".into()];
    }
    dirs.retain(|d| r.dir().join(d).is_dir());
    dirs
}

/// Files called `name` under `dir` (sorted, skipping node_modules, dot dirs and `target`),
/// at most `max_depth` levels down.
fn find_named(dir: &Path, name: &str, out: &mut Vec<PathBuf>, max_depth: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        let file = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if max_depth > 0 && file != "node_modules" && file != "target" && !file.starts_with('.')
            {
                find_named(&p, name, out, max_depth - 1);
            }
        } else if file == name {
            out.push(p);
        }
    }
}

/// Up to `limit` TypeScript sources under `dir` (not declarations, not node_modules).
fn find_sources(dir: &Path, out: &mut Vec<PathBuf>, limit: usize) {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>, left: &mut usize) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for p in entries {
            if *left == 0 {
                return;
            }
            let file = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if p.is_dir() {
                if file != "node_modules" && file != "dist" && !file.starts_with('.') {
                    walk(&p, out, left);
                }
            } else if (file.ends_with(".ts") || file.ends_with(".tsx")) && !file.ends_with(".d.ts")
            {
                out.push(p);
                *left -= 1;
            }
        }
    }
    let mut left = limit;
    walk(dir, out, &mut left);
}

/// A package's main entry: `main` from package.json, else `index.js`.
fn package_entry(dir: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let main = json["main"].as_str().unwrap_or("index.js");
    let mut p = dir.join(main);
    if p.is_dir() {
        p = p.join("index.js");
    }
    if p.is_file() {
        return Some(p);
    }
    for ext in ["js", "cjs", "mjs"] {
        let with = dir.join(format!("{main}.{ext}"));
        if with.is_file() {
            return Some(with);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triple_dir_drops_glibc_suffix() {
        assert_eq!(
            triple_dir("x86_64-unknown-linux-gnu.2.17"),
            "x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            triple_dir("aarch64-unknown-linux-musl"),
            "aarch64-unknown-linux-musl"
        );
        assert_eq!(
            triple_dir("x86_64-pc-windows-msvc"),
            "x86_64-pc-windows-msvc"
        );
    }

    #[test]
    fn package_entry_follows_main() {
        let dir = std::env::temp_dir().join(format!("xtask-pgo-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("package.json"), r#"{"main": "lib/main"}"#).unwrap();
        std::fs::write(dir.join("lib/main.js"), "module.exports = 1").unwrap();
        assert_eq!(package_entry(&dir), Some(dir.join("lib/main.js")));
        std::fs::write(dir.join("package.json"), r#"{"main": "lib"}"#).unwrap();
        std::fs::write(dir.join("lib/index.js"), "module.exports = 2").unwrap();
        assert_eq!(package_entry(&dir), Some(dir.join("lib/index.js")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_binary_has_a_package() {
        let root = root();
        for (pkg, bin) in BINARIES {
            let dir = std::fs::read_dir(root.join("crates"))
                .unwrap()
                .filter_map(|e| e.ok())
                .find(|e| e.file_name() == *pkg)
                .unwrap_or_else(|| panic!("no crates/{pkg}"));
            let manifest = std::fs::read_to_string(dir.path().join("Cargo.toml")).unwrap();
            assert!(
                manifest.contains(&format!("name = \"{bin}\"")),
                "{pkg} lacks bin {bin}"
            );
        }
    }
}
