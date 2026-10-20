//! `cargo xtask bench`: run `bench/scenarios.toml` with hyperfine, store results as JSON in
//! `bench/results/` and print a comparison table (optionally against a baseline run).
//!
//! Placeholders in commands: `{bin}` (our release binaries, `--bin-dir`, default
//! `target/release`), `{node_bin}` (`bench/node_modules/.bin`, pinned reference tools) and
//! `{input}` (the scenario's input file). Commands run from the workspace root.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde::{Deserialize, Serialize};

use crate::args::Args;
use crate::bench_repos::{self, RepoPin};
use crate::util::{
    Result, has_program, limited, npm_tool, read_to_string, rel, root, run_capture, run_status,
};

pub const SCENARIOS: &str = "bench/scenarios.toml";
pub const RESULTS_DIR: &str = "bench/results";

#[derive(Debug, Deserialize)]
pub struct BenchFile {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default, rename = "scenario")]
    pub scenarios: Vec<Scenario>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub warmup: u32,
    pub runs: u32,
    /// Allowed slowdown of our tools vs the baseline (0.05 = 5%).
    pub max_regression: f64,
    /// A slowdown also has to exceed this many standard deviations of the two runs combined,
    /// so the noise of short timings on shared runners doesn't fail the gate.
    pub noise_sigmas: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            warmup: 3,
            runs: 10,
            max_regression: 0.05,
            noise_sigmas: 2.0,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Scenario {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub measures: Option<String>,
    /// Input file; scenarios report MB/s when set.
    #[serde(default)]
    pub input: Option<String>,
    /// How to get the input if it is missing.
    #[serde(default)]
    pub input_hint: Option<String>,
    /// Run without an intermediate shell (`hyperfine -N`), for very short commands.
    #[serde(default)]
    pub no_shell: bool,
    #[serde(default)]
    pub runs: Option<u32>,
    #[serde(default)]
    pub warmup: Option<u32>,
    /// A pinned repo from `bench/repos.toml`: the scenario runs in its checkout
    /// (`target/bench-repos/<name>`, `{repo}`), only when `--repos` selects it, under
    /// `scripts/limit.sh` with the repo's memory cap.
    #[serde(default)]
    pub repo: Option<String>,
    /// Time cap (s) for all runs of one tool in a repo scenario (default 1200).
    #[serde(default)]
    pub time_s: Option<u32>,
    /// Shell command run once before each tool's runs (`hyperfine --setup`: untimed, in the
    /// same directory and under the same cap), for inputs derived from the binaries under test,
    /// such as the restricted ESLint config of `lint-trpc-same-rules`. Placeholders allowed.
    #[serde(default)]
    pub setup: Option<String>,
    #[serde(default, rename = "tool")]
    pub tools: Vec<Tool>,
}

#[derive(Debug, Deserialize)]
pub struct Tool {
    pub name: String,
    pub cmd: String,
    /// One of ours: gated against the baseline.
    #[serde(default)]
    pub ours: bool,
    /// Why this tool is not run yet (e.g. the binary doesn't exist until M1).
    #[serde(default)]
    pub pending: Option<String>,
    /// Paths (placeholders allowed) that must exist before running.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Pass `--ignore-failure` (tools that exit non-zero by design, e.g. tsc reporting errors).
    #[serde(default)]
    pub ignore_failure: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultsFile {
    pub name: String,
    pub git: String,
    pub os: String,
    pub arch: String,
    pub scenarios: Vec<ScenarioResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioResult {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub input_bytes: Option<u64>,
    pub tools: Vec<ToolResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub name: String,
    pub ours: bool,
    /// `ok`, `pending`, `skipped` or `failed`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<Stats>,
    /// Peak resident set size (MB) of the largest process of one extra run (`--memory`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rss_mb: Option<f64>,
}

/// Seconds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Stats {
    pub median: f64,
    pub mean: f64,
    pub stddev: f64,
    pub min: f64,
    pub max: f64,
    pub p95: f64,
    pub runs: usize,
}

pub fn load_scenarios() -> Result<BenchFile> {
    let path = root().join(SCENARIOS);
    let text = read_to_string(&path)?;
    toml::from_str(&text).map_err(|e| format!("{SCENARIOS}: {e}").into())
}

/// Parses `--mem-mb`. The root AGENTS.md caps any command at
/// [`bench_repos::MAX_MEM_MB`] on the shared machine; a higher value needs
/// `--dedicated-machine`, which says nobody else is using this one.
fn parse_mem_mb(value: Option<&str>, dedicated_machine: bool) -> Result<Option<u32>> {
    let Some(v) = value else {
        return Ok(None);
    };
    let mb = v.parse::<u32>().map_err(|e| format!("--mem-mb {v}: {e}"))?;
    if mb > bench_repos::MAX_MEM_MB && !dedicated_machine {
        return Err(format!(
            "--mem-mb {mb} is above the {} MB cap for this shared machine; pass \
             --dedicated-machine too, and only on a machine nobody else is using",
            bench_repos::MAX_MEM_MB
        )
        .into());
    }
    Ok(Some(mb))
}

pub fn run(mut args: Args) -> Result<ExitCode> {
    let list = args.flag("list");
    let no_fail = args.flag("no-fail");
    let setup = args.flag("setup");
    let memory = args.flag("memory");
    let fetch_repos = args.flag("fetch-repos");
    let no_install = args.flag("no-install");
    let dry_run = args.flag("dry-run");
    let large = args.flag("large");
    let editor = args.flag("editor");
    let run_editor = args.flag("run");
    let clean_repos = args.flag("clean-repos");
    let reinstall = args.flag("reinstall");
    let repos_spec = args.opt("repos")?;
    let dedicated_machine = args.flag("dedicated-machine");
    let mem_override = parse_mem_mb(args.opt("mem-mb")?.as_deref(), dedicated_machine)?;
    let time_override = args
        .opt("time-s")?
        .map(|v| v.parse::<u32>().map_err(|e| format!("--time-s {v}: {e}")))
        .transpose()?;
    let save = args.opt("save")?.unwrap_or_else(|| "latest".into());
    let baseline = args.opt("baseline")?;
    let compare = args.opt("compare")?;
    let markdown = args.opt("markdown")?;
    let bin_dir = args
        .opt("bin-dir")?
        .unwrap_or_else(|| "target/release".into());
    let filters = args.rest();
    args.finish()?;

    // Compare two saved runs without running anything (the CI confirmation of a regression).
    if let Some(path) = &compare {
        let Some(baseline) = &baseline else {
            return Err("--compare needs --baseline <file>".into());
        };
        let settings = load_scenarios()?.settings;
        let results = load_results(path)?;
        return report(
            &results,
            Some(baseline),
            &settings,
            markdown.as_deref(),
            None,
            no_fail,
            true,
        );
    }

    // Pinned repos: only touched when --repos names them.
    let manifest = match &repos_spec {
        Some(_) => Some(bench_repos::load()?),
        None => None,
    };
    // Only fetching and the editor harness have a print-only mode; anything else would run.
    if dry_run && !(fetch_repos || editor) {
        return Err("--dry-run applies only to --fetch-repos and --editor".into());
    }
    let needs_repos = fetch_repos || editor || clean_repos;
    if needs_repos && manifest.is_none() {
        return Err("--fetch-repos, --editor and --clean-repos need --repos <names|all>".into());
    }
    // Listing and printing never run anything, so large repos are allowed there.
    let listing = list || dry_run || (editor && !run_editor);
    let repos: Vec<&RepoPin> = match (&manifest, &repos_spec) {
        (Some(m), Some(spec)) => bench_repos::select(m, spec, large || listing)?,
        _ => Vec::new(),
    };
    if clean_repos {
        bench_repos::clean(&repos)?;
        return Ok(ExitCode::SUCCESS);
    }
    if fetch_repos {
        let o = bench_repos::FetchOptions {
            install: !no_install,
            dry_run,
            mem_mb: mem_override,
            reinstall,
        };
        for r in &repos {
            bench_repos::fetch(r, &o)?;
        }
        return Ok(ExitCode::SUCCESS);
    }
    if editor {
        return run_editor_harness(&repos, &bin_dir, &filters, run_editor && !dry_run);
    }

    let file = load_scenarios()?;
    let selected: Vec<&Scenario> = file
        .scenarios
        .iter()
        .filter(|s| match &s.repo {
            None => repos_spec.is_none(),
            Some(name) => repos.iter().any(|r| &r.name == name),
        })
        .filter(|s| filters.is_empty() || filters.iter().any(|f| s.id.contains(f.as_str())))
        .collect();
    if selected.is_empty() {
        return Err(format!(
            "no scenario matches {filters:?}{}",
            repos_spec
                .as_deref()
                .map(|r| format!(" for --repos {r}"))
                .unwrap_or_default()
        )
        .into());
    }
    let root = root();
    let base_vars = Vars {
        bin: bin_dir,
        node_bin: "bench/node_modules/.bin".into(),
        repo: String::new(),
        tmp: "target/bench-tmp".into(),
    };
    let vars_for = |s: &Scenario| -> Vars {
        match &s.repo {
            Some(name) => base_vars.in_repo(&root, &root.join(bench_repos::REPOS_DIR).join(name)),
            None => base_vars.clone(),
        }
    };
    let pin_for = |s: &Scenario| -> Option<&RepoPin> {
        let name = s.repo.as_ref()?;
        repos.iter().copied().find(|r| &r.name == name)
    };

    if list {
        for s in &selected {
            let vars = vars_for(s);
            println!("{}  {}", s.id, s.title);
            if let Some(m) = &s.measures {
                println!("    measures: {m}");
            }
            if let Some(setup) = &s.setup {
                println!("    setup: $ {}", vars.expand(setup, s.input.as_deref()));
            }
            if let Some(r) = pin_for(s) {
                println!(
                    "    repo: {} @ {} ({} on {}; {}, {}; cap {} MB; {})",
                    r.repo,
                    &r.commit[..12],
                    r.branch,
                    r.date,
                    r.group,
                    r.size.as_str(),
                    mem_override.unwrap_or(r.mem_mb),
                    if bench_repos::is_fetched(r) {
                        "fetched"
                    } else {
                        "not fetched"
                    },
                );
                if let Some(n) = &r.notes {
                    println!("    notes: {n}");
                }
            }
            for t in &s.tools {
                let state = match &t.pending {
                    Some(why) => format!("pending: {why}"),
                    None => "active".into(),
                };
                println!(
                    "    {:<18} {:<9} {}",
                    t.name,
                    if t.ours { "ours" } else { "reference" },
                    state
                );
                println!(
                    "                       $ {}",
                    vars.expand(&t.cmd, s.input.as_deref())
                );
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    let Some(hyperfine) = find_hyperfine(&root) else {
        eprintln!(
            "xtask bench needs hyperfine (https://github.com/sharkdp/hyperfine):\n  \
             macOS: brew install hyperfine\n  \
             Debian/Ubuntu: sudo apt install hyperfine\n  \
             Windows: winget install sharkdp.hyperfine\n  \
             any: cargo install --locked hyperfine (or --root target/tools to keep it local)\n\
             Use `cargo xtask bench --list` to see the scenarios without running them."
        );
        return Ok(ExitCode::from(2));
    };

    let uses_node = |c: &str| c.contains("{node_bin}") || c.contains("bench/js/");
    let needs_node = selected.iter().any(|s| {
        let active = s.tools.iter().filter(|t| t.pending.is_none());
        active.clone().any(|t| uses_node(&t.cmd))
            || (active.count() > 0 && s.setup.as_deref().is_some_and(uses_node))
    });
    if needs_node {
        install_reference_tools(&root, setup)?;
    }

    let mut results = ResultsFile {
        name: save.clone(),
        git: run_capture(Command::new("git").arg("-C").arg(&root).args([
            "rev-parse",
            "--short",
            "HEAD",
        ]))
        .map(|s| s.trim().to_string())
        .unwrap_or_default(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        scenarios: Vec::new(),
    };
    let tmp = root.join("target/bench-tmp");
    std::fs::create_dir_all(&tmp)?;
    // Repo scenarios run outside the workspace, where ts-r-check and ts-r-lint cannot find the
    // synced lib files relative to the working directory: point them there (unless set).
    let lib_dir = std::env::var_os("TS_R_LIB_DIR")
        .is_none()
        .then(|| root.join(LIB_DIR))
        .filter(|d| d.is_dir());

    for s in &selected {
        eprintln!("== {} ({})", s.id, s.title);
        let vars = vars_for(s);
        let pin = pin_for(s);
        let cwd = match pin {
            Some(r) => r.dir(),
            None => root.clone(),
        };
        let input_path = s.input.as_ref().map(|i| root.join(i));
        let input_bytes = input_path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len());
        let mut tools = Vec::new();
        for t in &s.tools {
            let mut result = ToolResult {
                name: t.name.clone(),
                ours: t.ours,
                status: "ok".into(),
                reason: None,
                stats: None,
                max_rss_mb: None,
            };
            if let Some(why) = &t.pending {
                result.status = "pending".into();
                result.reason = Some(why.clone());
            } else if let Some(r) = pin.filter(|r| !bench_repos::is_fetched(r)) {
                result.status = "skipped".into();
                result.reason = Some(format!(
                    "{} is not fetched at {} (cargo xtask bench --fetch-repos --repos {})",
                    rel(&r.dir()),
                    &r.commit[..12],
                    r.name
                ));
            } else if let (Some(input), None) = (&s.input, input_bytes) {
                result.status = "skipped".into();
                result.reason = Some(format!(
                    "missing input {input}{}",
                    s.input_hint
                        .as_deref()
                        .map(|h| format!(" ({h})"))
                        .unwrap_or_default()
                ));
            } else if let Some(missing) = t
                .requires
                .iter()
                .map(|r| vars.expand(r, s.input.as_deref()))
                .find(|r| !root.join(r).exists())
            {
                result.status = "skipped".into();
                result.reason = Some(format!("missing {}", rel(Path::new(&missing))));
            } else {
                let cmd = vars.expand(&t.cmd, s.input.as_deref());
                let json = tmp.join(format!("{}-{}.json", s.id, sanitize(&t.name)));
                // limit.sh caps the whole run, outside the timed commands: repo scenarios at the
                // repo's cap, the rest at limit.sh's defaults.
                let limit = pin.map(|r| {
                    (
                        mem_override.unwrap_or(r.mem_mb),
                        time_override.or(s.time_s).unwrap_or(DEFAULT_REPO_TIME_S),
                    )
                });
                let mut hf = match limit {
                    Some((mem, time)) => {
                        let mut c = bench_repos::limit_cmd(mem, time);
                        c.arg(&hyperfine);
                        c
                    }
                    None => limited(&hyperfine),
                };
                if pin.is_some()
                    && let Some(libs) = &lib_dir
                {
                    hf.env("TS_R_LIB_DIR", libs);
                }
                hf.current_dir(&cwd)
                    .args([
                        "--style",
                        "basic",
                        "--warmup",
                        &s.warmup.unwrap_or(file.settings.warmup).to_string(),
                    ])
                    .args(["--runs", &s.runs.unwrap_or(file.settings.runs).to_string()])
                    .arg("--export-json")
                    .arg(&json)
                    .args(["-n", &t.name]);
                if s.no_shell {
                    hf.arg("-N");
                }
                if t.ignore_failure {
                    hf.arg("--ignore-failure");
                }
                if let Some(setup) = &s.setup {
                    hf.arg("--setup")
                        .arg(vars.expand(setup, s.input.as_deref()));
                }
                hf.arg(&cmd);
                let outcome = match limit {
                    // limit.sh exits 137 (memory) or 124 (time) when it kills the tree.
                    Some((mem, time)) => match hf.status() {
                        Ok(st) if st.success() => read_stats(&json),
                        Ok(st) => Err(match st.code() {
                            Some(137) => format!("killed by scripts/limit.sh: over {mem} MB"),
                            Some(124) => format!("killed by scripts/limit.sh: over {time} s"),
                            _ => format!("hyperfine failed ({st})"),
                        }
                        .into()),
                        Err(e) => Err(format!("failed to start hyperfine: {e}").into()),
                    },
                    None => run_status(&mut hf).and_then(|()| read_stats(&json)),
                };
                match outcome {
                    Ok(stats) => result.stats = Some(stats),
                    Err(e) => {
                        result.status = "failed".into();
                        result.reason = Some(e.to_string());
                    }
                }
                if memory && result.stats.is_some() {
                    let libs = lib_dir.as_deref().filter(|_| pin.is_some());
                    match peak_rss_mb(&cmd, &cwd, limit, libs) {
                        Ok(mb) => result.max_rss_mb = Some(mb),
                        Err(e) => eprintln!("  peak RSS of {}: {e}", t.name),
                    }
                }
            }
            tools.push(result);
        }
        results.scenarios.push(ScenarioResult {
            id: s.id.clone(),
            title: s.title.clone(),
            input_bytes,
            tools,
        });
    }

    let out_dir = root.join(RESULTS_DIR);
    std::fs::create_dir_all(&out_dir)?;
    let out_path = out_dir.join(format!("{}.json", sanitize(&save)));
    std::fs::write(&out_path, serde_json::to_string_pretty(&results)? + "\n")?;
    eprintln!("wrote {}", rel(&out_path));

    let ids_path = out_dir.join(format!("{}-regressions.txt", sanitize(&save)));
    report(
        &results,
        baseline.as_deref(),
        &file.settings,
        markdown.as_deref(),
        Some(&ids_path),
        no_fail,
        false,
    )
}

fn load_results(path: &str) -> Result<ResultsFile> {
    serde_json::from_str(&read_to_string(Path::new(path))?)
        .map_err(|e| format!("{path}: {e}").into())
}

/// Prints the table (and writes it to `markdown`), and gates on regressions against
/// `baseline`. `ids_path` gets the regressed scenario ids, one per line, for a confirming
/// re-run (`cargo xtask bench <ids>`); it is removed when there are none. `confirm`: see
/// [`render_table`].
fn report(
    results: &ResultsFile,
    baseline: Option<&str>,
    settings: &Settings,
    markdown: Option<&str>,
    ids_path: Option<&Path>,
    no_fail: bool,
    confirm: bool,
) -> Result<ExitCode> {
    let base = baseline.map(load_results).transpose()?;
    let (table, regressions) = render_table(results, base.as_ref(), settings, confirm);
    print!("{table}");
    if let Some(md) = markdown {
        crate::util::write_output(Some(md), &table)?;
    }
    if let Some(p) = ids_path {
        let _ = std::fs::remove_file(p);
        if !regressions.is_empty() {
            let mut ids: Vec<&str> = regressions.iter().map(|r| r.scenario.as_str()).collect();
            ids.dedup();
            std::fs::write(p, ids.join("\n") + "\n")?;
        }
    }
    if !regressions.is_empty() {
        eprintln!(
            "bench: {} regression(s) (above {:.0}% and the noise, or failed): {}",
            regressions.len(),
            settings.max_regression * 100.0,
            regressions
                .iter()
                .map(|r| r.label.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        if !no_fail {
            return Ok(ExitCode::from(1));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// One of our tools slower than the baseline by more than the gate allows.
pub struct Regression {
    pub scenario: String,
    /// `<scenario>/<tool> +x.y%`.
    pub label: String,
}

/// The synced TypeScript lib files (`cargo xtask upstream sync --minimal`).
const LIB_DIR: &str = "target/upstream/typescript/tsc/internal/bundled/libs";

/// Default time cap for all runs of one tool in a repo scenario.
const DEFAULT_REPO_TIME_S: u32 = 1200;

/// `hyperfine` on PATH, else `target/tools/bin/hyperfine` (`cargo install --root target/tools`).
fn find_hyperfine(root: &Path) -> Option<PathBuf> {
    if has_program("hyperfine") {
        return Some(PathBuf::from("hyperfine"));
    }
    let local = root.join("target/tools/bin/hyperfine");
    local.is_file().then_some(local)
}

/// Runs `cmd` once more under `/usr/bin/time` and returns the peak RSS (MB) of its largest
/// process (`ru_maxrss` of the waited-for children: not the sum of a process tree).
fn peak_rss_mb(
    cmd: &str,
    cwd: &Path,
    limit: Option<(u32, u32)>,
    lib_dir: Option<&Path>,
) -> Result<f64> {
    let flag = if cfg!(target_os = "macos") {
        "-l"
    } else {
        "-v"
    };
    let mut c = match limit {
        Some((mem, time)) => {
            let mut c = bench_repos::limit_cmd(mem, time);
            c.arg("/usr/bin/time");
            c
        }
        None => Command::new("/usr/bin/time"),
    };
    if let Some(libs) = lib_dir {
        c.env("TS_R_LIB_DIR", libs);
    }
    c.current_dir(cwd).args([flag, "sh", "-c", cmd]);
    let out = c
        .output()
        .map_err(|e| format!("failed to start /usr/bin/time: {e}"))?;
    parse_max_rss_mb(&String::from_utf8_lossy(&out.stderr))
        .ok_or_else(|| "no maximum resident set size in /usr/bin/time output".into())
}

/// Reads the peak RSS from `/usr/bin/time -l` (macOS, bytes) or `-v` (GNU, kilobytes).
pub fn parse_max_rss_mb(stderr: &str) -> Option<f64> {
    for line in stderr.lines() {
        let line = line.trim();
        if let Some(n) = line.strip_suffix("maximum resident set size") {
            let bytes: f64 = n.trim().parse().ok()?;
            return Some(bytes / (1024.0 * 1024.0));
        }
        if let Some(n) = line.strip_prefix("Maximum resident set size (kbytes):") {
            let kb: f64 = n.trim().parse().ok()?;
            return Some(kb / 1024.0);
        }
    }
    None
}

/// `--editor`: print (or with `--run`, run) the editor-memory harnesses on the repos. Filters
/// match the run labels (`latency-ts-r`, `latency-tsserver`, `latency-tsgo`, `vscode-stock`,
/// `vscode-ts-r`).
fn run_editor_harness(
    repos: &[&RepoPin],
    bin_dir: &str,
    filters: &[String],
    execute: bool,
) -> Result<ExitCode> {
    let root = root();
    let out_dir = root.join(RESULTS_DIR).join("editor");
    let mut failed = 0;
    for r in repos {
        let lib_dir = std::env::var_os("TS_R_LIB_DIR")
            .is_none()
            .then(|| root.join(LIB_DIR))
            .filter(|d| d.is_dir());
        let runs = bench_repos::editor_runs(r, bin_dir, lib_dir.as_deref());
        if runs.is_empty() {
            eprintln!(
                "{}: no editor-memory set in {}",
                r.name,
                bench_repos::MANIFEST
            );
            continue;
        }
        println!("# {} ({} @ {})", r.name, r.repo, &r.commit[..12]);
        for run in runs
            .iter()
            .filter(|r| filters.is_empty() || filters.iter().any(|f| r.label.contains(f.as_str())))
        {
            let line: Vec<String> = run.argv.iter().map(|a| bench_repos::quote(a)).collect();
            println!("{}", line.join(" "));
            if run.stop_daemon {
                println!("{bin_dir}/ts-r-lsp stop --root {}", rel(&r.dir()));
            }
            if !execute {
                continue;
            }
            if !bench_repos::is_fetched(r) {
                return Err(format!(
                    "{} is not fetched (cargo xtask bench --fetch-repos --repos {})",
                    rel(&r.dir()),
                    r.name
                )
                .into());
            }
            let mut c = Command::new(root.join(&run.argv[0]));
            c.args(&run.argv[1..]).current_dir(&root);
            let out = c.output()?;
            if run.stop_daemon {
                let _ = Command::new(root.join(bin_dir).join("ts-r-lsp"))
                    .args(["stop", "--root"])
                    .arg(r.dir())
                    .current_dir(&root)
                    .output();
            }
            std::fs::create_dir_all(&out_dir)?;
            let path = out_dir.join(format!("{}-{}.json", r.name, run.label));
            std::fs::write(&path, &out.stdout)?;
            if out.status.success() {
                eprintln!("wrote {}", rel(&path));
            } else {
                failed += 1;
                let err_path = path.with_extension("err.txt");
                std::fs::write(&err_path, &out.stderr)?;
                let stderr = String::from_utf8_lossy(&out.stderr);
                let first = stderr
                    .lines()
                    .find(|l| l.contains("Error") || l.contains("killed"))
                    .unwrap_or("");
                eprintln!(
                    "{} {} failed ({}): {first} (stderr in {})",
                    r.name,
                    run.label,
                    out.status,
                    rel(&err_path)
                );
            }
        }
    }
    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

#[derive(Clone)]
struct Vars {
    bin: String,
    node_bin: String,
    /// The repo checkout (`{repo}`), empty outside repo scenarios.
    repo: String,
    tmp: String,
}

impl Vars {
    fn expand(&self, s: &str, input: Option<&str>) -> String {
        s.replace("{bin}", &self.bin)
            .replace("{node_bin}", &self.node_bin)
            .replace("{repo}", &self.repo)
            .replace("{tmp}", &self.tmp)
            .replace("{input}", input.unwrap_or(""))
    }

    /// Repo scenarios run in the checkout: every path becomes absolute.
    fn in_repo(&self, root: &Path, dir: &Path) -> Vars {
        // These go into `sh -c` command lines: `/` separators on every OS (`C:/x` works in
        // Windows shells; a `\` would be an escape).
        let slashes = |p: &Path| {
            let s = p.to_string_lossy();
            if cfg!(windows) {
                s.replace('\\', "/")
            } else {
                s.into_owned()
            }
        };
        let abs = |p: &str| {
            let p = Path::new(p);
            if p.is_absolute() {
                slashes(p)
            } else {
                slashes(&root.join(p))
            }
        };
        Vars {
            bin: abs(&self.bin),
            node_bin: abs(&self.node_bin),
            repo: slashes(dir),
            tmp: abs(&self.tmp),
        }
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Installs the pinned reference tools into `bench/node_modules` (`npm ci`, offline-first from
/// the shared bench cache). Skipped while the stamp in `bench/node_modules` (lockfile and
/// manifest hashes, Node version) matches, unless `force` (`--setup`).
fn install_reference_tools(root: &Path, force: bool) -> Result<()> {
    let bench = root.join("bench");
    let stamp_file = bench.join("node_modules/.ts-r-bench-stamp");
    let mut stamp = format!("node {}\n", bench_repos::node_version());
    for f in ["package.json", "package-lock.json"] {
        let bytes = std::fs::read(bench.join(f)).unwrap_or_default();
        stamp.push_str(&format!("{f} {:016x}\n", bench_repos::fnv1a(&bytes)));
    }
    if !force && std::fs::read_to_string(&stamp_file).is_ok_and(|s| s == stamp) {
        return Ok(());
    }
    let ci = bench.join("package-lock.json").is_file();
    eprintln!(
        "installing pinned reference tools into bench/node_modules (npm {}, offline-first) ...",
        if ci { "ci" } else { "install" },
    );
    // npm's own cache (~/.npm), not the bench repos' one: CI's setup-node caches it, keyed on
    // bench/package-lock.json.
    let mut cmd = Command::new(npm_tool("npm"));
    cmd.current_dir(&bench)
        .arg(if ci { "ci" } else { "install" })
        .args([
            "--prefer-offline",
            "--no-audit",
            "--no-fund",
            "--loglevel=error",
        ]);
    run_status(&mut cmd)?;
    std::fs::write(&stamp_file, stamp)?;
    Ok(())
}

fn read_stats(path: &Path) -> Result<Stats> {
    let v: serde_json::Value = serde_json::from_str(&read_to_string(path)?)?;
    let r = &v["results"][0];
    let mut times: Vec<f64> = r["times"]
        .as_array()
        .map(|a| a.iter().filter_map(|t| t.as_f64()).collect())
        .unwrap_or_default();
    times.sort_by(f64::total_cmp);
    let f = |k: &str| r[k].as_f64().unwrap_or(f64::NAN);
    Ok(Stats {
        median: f("median"),
        mean: f("mean"),
        stddev: f("stddev"),
        min: f("min"),
        max: f("max"),
        p95: percentile(&times, 0.95).unwrap_or(f("max")),
        runs: times.len(),
    })
}

/// Nearest-rank percentile of sorted data.
pub fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (p * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

fn ms(s: f64) -> String {
    if s < 0.01 {
        format!("{:.2} ms", s * 1000.0)
    } else {
        format!("{:.1} ms", s * 1000.0)
    }
}

/// Markdown comparison table plus the list of regressions (ours, slower than baseline by more
/// than `max_regression` and by more than `noise_sigmas` combined standard deviations, or
/// failed: where the baseline measured, or with nothing to compare against). `confirm` is a
/// re-run of flagged scenarios: there a failed measurement on either side also counts, since
/// it confirms nothing.
pub fn render_table(
    results: &ResultsFile,
    baseline: Option<&ResultsFile>,
    gate: &Settings,
    confirm: bool,
) -> (String, Vec<Regression>) {
    let mut md = String::new();
    let mut regressions = Vec::new();
    let _ = writeln!(
        md,
        "### Benchmarks ({} @ {}, {}-{})\n",
        results.name, results.git, results.os, results.arch
    );
    let _ = writeln!(
        md,
        "| Scenario | Tool | Median | p95 | ± | Throughput | Peak RSS | vs fastest | vs {} |",
        baseline.map(|b| b.name.as_str()).unwrap_or("baseline")
    );
    let _ = writeln!(md, "|---|---|---:|---:|---:|---:|---:|---:|---:|");
    for s in &results.scenarios {
        let fastest = s
            .tools
            .iter()
            .filter_map(|t| t.stats.map(|st| st.median))
            .fold(f64::INFINITY, f64::min);
        let base_scenario = baseline.and_then(|b| b.scenarios.iter().find(|x| x.id == s.id));
        for t in &s.tools {
            let name = if t.ours {
                format!("**{}**", t.name)
            } else {
                t.name.clone()
            };
            let base_tool = base_scenario.and_then(|b| b.tools.iter().find(|x| x.name == t.name));
            // A measurement that failed (over its cap, or the command failed) can't clear the
            // gate: ours failing where the baseline measured is a regression, and a
            // confirming re-run (`confirm`) needs both sides measured.
            let unmeasured = match (t.stats, base_tool) {
                (None, Some(b)) => t.status == "failed" && (b.stats.is_some() || confirm),
                (Some(_), Some(b)) => confirm && b.status == "failed",
                _ => false,
            };
            if t.ours && unmeasured {
                regressions.push(Regression {
                    scenario: s.id.clone(),
                    label: format!("{}/{} not measured", s.id, t.name),
                });
            }
            let Some(st) = t.stats else {
                // One of ours that crashed or exited non-zero fails the gate too, also with no
                // baseline measurement to compare against (with one, it is `not measured` above).
                if t.ours && t.status == "failed" && base_tool.is_none() {
                    regressions.push(Regression {
                        scenario: s.id.clone(),
                        label: format!("{}/{} failed", s.id, t.name),
                    });
                }
                let _ = writeln!(
                    md,
                    "| {} | {name} | {} | | | | | | |",
                    s.id,
                    match &t.reason {
                        Some(r) => format!("{}: {}", t.status, r.replace('|', "/")),
                        None => t.status.clone(),
                    }
                );
                continue;
            };
            let throughput = s
                .input_bytes
                .map(|b| format!("{:.1} MB/s", b as f64 / st.median / 1e6))
                .unwrap_or_default();
            let vs_fastest = format!("{:.2}x", st.median / fastest);
            let base = base_tool.and_then(|x| x.stats);
            let vs_base = match base {
                Some(b) => {
                    let change = (st.median - b.median) / b.median;
                    let noise = gate.noise_sigmas * st.stddev.hypot(b.stddev);
                    if t.ours && change > gate.max_regression && st.median - b.median > noise {
                        regressions.push(Regression {
                            scenario: s.id.clone(),
                            label: format!("{}/{} +{:.1}%", s.id, t.name, change * 100.0),
                        });
                    }
                    format!("{:+.1}%", change * 100.0)
                }
                None => String::new(),
            };
            let rss = t
                .max_rss_mb
                .map(|mb| format!("{mb:.0} MB"))
                .unwrap_or_default();
            let _ = writeln!(
                md,
                "| {} | {name} | {} | {} | {} | {throughput} | {rss} | {vs_fastest} | {vs_base} |",
                s.id,
                ms(st.median),
                ms(st.p95),
                ms(st.stddev),
            );
        }
    }
    md.push('\n');
    if baseline.is_some() {
        let _ = writeln!(
            md,
            "Gate: our tools may not fail or be more than {:.0}% slower than the baseline \
             median (counted when the slowdown is also above {} combined standard deviations). \
             {}\n",
            gate.max_regression * 100.0,
            gate.noise_sigmas,
            if regressions.is_empty() {
                "No regressions.".to_string()
            } else {
                let labels: Vec<&str> = regressions.iter().map(|r| r.label.as_str()).collect();
                format!("Regressions: {}.", labels.join(", "))
            }
        );
    }
    (md, regressions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_never_runs_or_deletes() {
        // An unknown repo name: before the check, selection failed instead (nothing removed).
        for argv in [
            &["--repos", "no-such-repo", "--dry-run"][..],
            &["--clean-repos", "--repos", "no-such-repo", "--dry-run"],
        ] {
            let args = Args::new(argv.iter().map(|s| s.to_string()).collect());
            let err = run(args).unwrap_err().to_string();
            assert!(err.contains("--dry-run applies only to"), "{argv:?}: {err}");
        }
    }

    #[test]
    fn mem_mb_above_the_shared_cap_needs_dedicated_machine() {
        assert_eq!(parse_mem_mb(None, false).unwrap(), None);
        assert_eq!(parse_mem_mb(Some("8192"), false).unwrap(), Some(8192));
        assert_eq!(parse_mem_mb(Some("2048"), true).unwrap(), Some(2048));
        let err = parse_mem_mb(Some("8193"), false).unwrap_err().to_string();
        assert!(err.contains("--dedicated-machine"), "{err}");
        assert_eq!(parse_mem_mb(Some("16384"), true).unwrap(), Some(16384));
        assert!(parse_mem_mb(Some("lots"), true).is_err());
        // Rejected before any repo is selected, fetched or run.
        let args = Args::new(
            ["--repos", "no-such-repo", "--mem-mb", "16384"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        );
        let err = run(args).unwrap_err().to_string();
        assert!(err.contains("--dedicated-machine"), "{err}");
    }

    #[test]
    fn scenarios_file_parses() {
        let file = load_scenarios().unwrap();
        assert!(!file.scenarios.is_empty());
        let mut ids: Vec<&str> = file.scenarios.iter().map(|s| s.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len(),
            file.scenarios.len(),
            "scenario ids must be unique"
        );
        for s in &file.scenarios {
            assert!(s.tools.iter().any(|t| t.ours), "{} has no ts-r tool", s.id);
            assert!(
                s.tools.iter().any(|t| !t.ours),
                "{} has no reference tool",
                s.id
            );
            // A setup that runs one of the bench/js helpers names a file that exists.
            for word in s.setup.iter().flat_map(|c| c.split_whitespace()) {
                if let Some((_, script)) = word.rsplit_once("/js/") {
                    assert!(
                        root().join("bench/js").join(script).is_file(),
                        "{}: setup runs a missing bench/js/{script}",
                        s.id
                    );
                }
            }
        }
    }

    #[test]
    fn percentiles() {
        let v: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(percentile(&v, 0.95), Some(19.0));
        assert_eq!(percentile(&v, 0.5), Some(10.0));
        assert_eq!(percentile(&[], 0.5), None);
    }

    fn result(name: &str, median: f64) -> ResultsFile {
        let st = Stats {
            median,
            mean: median,
            stddev: 0.001,
            min: median,
            max: median,
            p95: median,
            runs: 10,
        };
        ResultsFile {
            name: name.into(),
            git: "abc".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            scenarios: vec![ScenarioResult {
                id: "parse".into(),
                title: "Parse".into(),
                input_bytes: Some(2_000_000),
                tools: vec![
                    ToolResult {
                        name: "ts-r".into(),
                        ours: true,
                        status: "ok".into(),
                        reason: None,
                        stats: Some(st),
                        max_rss_mb: Some(42.4),
                    },
                    ToolResult {
                        name: "esbuild".into(),
                        ours: false,
                        status: "ok".into(),
                        reason: None,
                        stats: Some(Stats { median: 0.2, ..st }),
                        max_rss_mb: None,
                    },
                    ToolResult {
                        name: "later".into(),
                        ours: true,
                        status: "pending".into(),
                        reason: Some("M1".into()),
                        stats: None,
                        max_rss_mb: None,
                    },
                ],
            }],
        }
    }

    #[test]
    fn regression_gate() {
        let base = result("main", 0.100);
        let gate = Settings::default();
        let (_, ok) = render_table(&result("pr", 0.104), Some(&base), &gate, false);
        assert!(ok.is_empty());
        let (md, bad) = render_table(&result("pr", 0.110), Some(&base), &gate, false);
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].scenario, "parse");
        assert!(md.contains("+10.0%"));
        assert!(md.contains("20.0 MB/s") || md.contains("18.2 MB/s"));
        assert!(md.contains("pending: M1"));
        assert!(md.contains("| 42 MB |"));
    }

    #[test]
    fn failed_measurements_do_not_clear_the_gate() {
        let failed = |name: &str| {
            let mut r = result(name, 0.100);
            let t = &mut r.scenarios[0].tools[0];
            t.status = "failed".into();
            t.reason = Some("killed by scripts/limit.sh: over 300 s".into());
            t.stats = None;
            r
        };
        let gate = Settings::default();
        // Ours over its cap where the baseline measured: a regression, in any run.
        for confirm in [false, true] {
            let (_, bad) = render_table(&failed("pr"), Some(&result("main", 0.1)), &gate, confirm);
            assert_eq!(bad.len(), 1, "confirm={confirm}");
            assert_eq!(bad[0].label, "parse/ts-r not measured");
        }
        // The baseline failing: an improvement in the main run, unconfirmed in a re-run.
        let (_, ok) = render_table(&result("pr", 0.1), Some(&failed("main")), &gate, false);
        assert!(ok.is_empty());
        let (_, bad) = render_table(&result("pr", 0.1), Some(&failed("main")), &gate, true);
        assert_eq!(bad.len(), 1);
        // Both failing: nothing to compare in the main run, unconfirmed in a re-run.
        let (_, ok) = render_table(&failed("pr"), Some(&failed("main")), &gate, false);
        assert!(ok.is_empty());
        let (_, bad) = render_table(&failed("pr"), Some(&failed("main")), &gate, true);
        assert_eq!(bad.len(), 1);
    }

    #[test]
    fn noise_does_not_fail_the_gate() {
        // 10 ms slower on a 40 ms timing (+25%) with ±9 ms per run is noise; with ±1 ms it isn't.
        let noisy = |name: &str, median: f64, stddev: f64| {
            let mut r = result(name, median);
            for t in &mut r.scenarios[0].tools {
                if let Some(st) = &mut t.stats {
                    st.stddev = stddev;
                }
            }
            r
        };
        let gate = Settings::default();
        let (_, ok) = render_table(
            &noisy("pr", 0.050, 0.009),
            Some(&noisy("main", 0.040, 0.007)),
            &gate,
            false,
        );
        assert!(ok.is_empty());
        let (_, bad) = render_table(
            &noisy("pr", 0.050, 0.001),
            Some(&noisy("main", 0.040, 0.001)),
            &gate,
            false,
        );
        assert_eq!(bad.len(), 1);
    }

    #[test]
    fn max_rss_parsing() {
        let mac = "        1.20 real         0.90 user         0.20 sys\n           123731968  maximum resident set size\n";
        assert_eq!(parse_max_rss_mb(mac), Some(118.0));
        let gnu =
            "\tCommand being timed: \"sh -c x\"\n\tMaximum resident set size (kbytes): 2048\n";
        assert_eq!(parse_max_rss_mb(gnu), Some(2.0));
        assert_eq!(parse_max_rss_mb("nothing"), None);
    }

    #[test]
    fn repo_scenarios_match_the_manifest() {
        let file = load_scenarios().unwrap();
        let repos = bench_repos::load().unwrap();
        for s in &file.scenarios {
            let Some(name) = &s.repo else { continue };
            let r = repos
                .repos
                .iter()
                .find(|r| &r.name == name)
                .unwrap_or_else(|| panic!("{}: unknown repo {name}", s.id));
            let kind = s.id.split('-').next().unwrap_or("");
            assert!(
                r.scenarios.iter().any(|k| k == kind),
                "{}: {name} does not list `{kind}` in its scenarios",
                s.id
            );
            let id = format!("{kind}-{name}");
            assert!(
                s.id == id || s.id.starts_with(&format!("{id}-")),
                "{}: repo scenario ids are <kind>-<repo> or <kind>-<repo>-<variant>",
                s.id
            );
        }
        for r in &repos.repos {
            for kind in r
                .scenarios
                .iter()
                .filter(|k| *k != "editor-memory" && *k != "dev")
            {
                let id = format!("{kind}-{}", r.name);
                assert!(
                    file.scenarios
                        .iter()
                        .any(|s| s.id == id && s.repo.as_ref() == Some(&r.name)),
                    "{} lists `{kind}` but {SCENARIOS} has no scenario {id}",
                    r.name
                );
            }
        }
    }

    #[test]
    fn failed_tool_fails_the_gate() {
        let mut pr = result("pr", 0.100);
        let ts_r = &mut pr.scenarios[0].tools[0];
        ts_r.status = "failed".into();
        ts_r.reason = Some("hyperfine failed (exit status: 1)".into());
        ts_r.stats = None;
        let gate = Settings::default();
        // Where the baseline measured it is labelled `not measured` (one regression, not two).
        let (md, bad) = render_table(&pr, Some(&result("main", 0.100)), &gate, false);
        let labels: Vec<&str> = bad.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["parse/ts-r not measured"]);
        assert!(md.contains("Regressions: parse/ts-r not measured."), "{md}");
        // Without a baseline too, and a failed reference tool is not ours to gate.
        let (_, bad) = render_table(&pr, None, &gate, false);
        let labels: Vec<&str> = bad.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["parse/ts-r failed"]);
        assert_eq!(bad[0].scenario, "parse");
        pr.scenarios[0].tools[0].ours = false;
        assert!(render_table(&pr, None, &gate, false).1.is_empty());
    }

    #[test]
    fn expand_placeholders() {
        let v = Vars {
            bin: "target/release".into(),
            node_bin: "bench/node_modules/.bin".into(),
            repo: String::new(),
            tmp: "target/bench-tmp".into(),
        };
        assert_eq!(
            v.expand(
                "{bin}/ts-r parse {input} && {node_bin}/esbuild",
                Some("a.ts")
            ),
            "target/release/ts-r parse a.ts && bench/node_modules/.bin/esbuild"
        );
        let r = v.in_repo(Path::new("/w"), Path::new("/w/target/bench-repos/x"));
        assert_eq!(
            r.expand("cd {repo} && {bin}/ts-r-check --out={tmp}/o", None),
            "cd /w/target/bench-repos/x && /w/target/release/ts-r-check --out=/w/target/bench-tmp/o"
        );
    }
}
