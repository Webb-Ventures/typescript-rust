//! Repo tooling for ts-r: `cargo xtask <command>`.
//!
//! Every command is hand-rolled on purpose (no clap) so the tool builds fast and stays small.
//! See `xtask/AGENTS.md` for the command list and the rules this tool follows.

mod agents_check;
mod args;
mod bench;
mod bench_repos;
mod codegen;
mod diag_codes;
mod ecosystem;
mod emit_repos;
mod lint_globals;
mod npm_pack;
mod oracle;
mod pgo;
mod platform_files;
mod tasks;
mod upstream;
mod util;

use std::process::ExitCode;

use args::Args;

// `ecosystem worker` parses and transforms real-world packages: abort at a known size
// (TS_R_MEM_CAP_MB, set to the stage cap by the driver) instead of exhausting memory.
ts_r_testing::cap_memory!();

const HELP: &str = "\
cargo xtask <command> [options]

Commands:
  agents-check                 Check AGENTS.md / SKILL.md / docs links and front matter
  upstream check [--json] [--include-rc]
                               Compare pins in upstream/UPSTREAM.json with the latest releases
                               (exit 1 when a TypeScript stable bump is available)
  upstream bump [--to <ver>]   Re-pin TypeScript in UPSTREAM.json (default: latest stable)
  upstream sync [--minimal] [--commit <sha>] [--force]
                               Fetch upstream TypeScript test/lib/diagnostic data files into
                               target/upstream/typescript (data only, never compiler source)
  upstream diff <old> <new> [--out <file>]
                               Markdown summary of what changed between two TypeScript pins
  codegen [diagnostics|lint-globals] [--out <file>] [--input <dir>] [--check]
                               Generate Rust data from synced upstream files (no target:
                               all; with --check, the checked-in ones)
  diag-codes [--out <file>]    Dump the diagnostic codes ts-r can report (code, category, tsc's
                               message, reporting crates) as JSON, for the docs site
  oracle <project-dir>... [--out <file>] [--cmd <tsc-like command>]
                               Run tsc at the pinned version and write normalized diagnostics JSON
  tasks <diff.json> | tasks --ours <a.json> --tsc <b.json> [--out <file>]
                               Group diagnostic differences into a markdown task list
  bench [<filter>...] [--list] [--save <name>] [--baseline <file>] [--bin-dir <dir>]
        [--markdown <file>] [--no-fail] [--memory]
                               Run bench/scenarios.toml with hyperfine and compare results
                               (--memory: also the peak RSS of one extra run per tool)
  bench --compare <results.json> --baseline <file> [--markdown <file>]
                               Gate two saved runs against each other without running
  bench --repos <names|all> [--large] [--mem-mb N [--dedicated-machine]] [--time-s N]
        [<filter>...] [--list]
                               Only the scenarios of pinned public repos (bench/repos.toml),
                               in target/bench-repos/<name>, under scripts/limit.sh; large
                               repos need --large (a dedicated machine); --mem-mb above 8192
                               also needs --dedicated-machine (nobody else on the machine)
  bench --fetch-repos --repos <names|all> [--no-install] [--reinstall] [--dry-run] [--large]
                               Fetch the pinned commits (shallow, blob-less), install deps
                               under limit.sh, write check wrappers, build three.js x10;
                               unchanged checkouts and installs are reused (install stamps,
                               package caches in ~/.cache/ts-r/bench)
  bench --editor --repos <names> [--run]
                               Print (or run) the editor-memory harnesses on the repos
  bench --clean-repos --repos <names|all> [--large]
                               Delete the checkouts
  ecosystem [--top N] [--only a,b] [--stages parse,transform,bundle,check] [--tests]
            [--tests-top N] [--jobs N] [--budget-s S] [--max-downloads N] [--bin-dir <dir>]
            [--no-build] [--no-status] [--keep-tmp] [--out <file>] | ecosystem --clean
                               Build and check the top npm packages (ecosystem/packages.toml)
                               against node, tsc 7 and esbuild; write ecosystem/results/ and
                               ecosystem/STATUS.md
  ecosystem refresh [--count N] Re-pin ecosystem/packages.toml from npm download counts
  ecosystem report [<results.json>]
                               Rewrite ecosystem/STATUS.md from a results file
  emit-repos [--only a,b] [--fresh] [--tsc <path>] [--bin <ts-r-check>] [--markdown <file>]
             [--no-fail]
                               tsc 7 and ts-r-check emit on the real repos of
                               bench/emit-repos.toml: written files per kind, exit codes,
                               diagnostics and peak RSS; exit 1 when the E6 gate fails
  pgo [--profile release|dist] [--target <triple>] [--repos <names>] [--ecosystem <dir>]
      [--use-profile <file>] [--no-rebuild] [--keep-profiles]
                               Profile-guided build: instrument, train on the fetched bench
                               repos, ecosystem samples and fixtures, merge, rebuild into
                               target/pgo/optimized/<target>/<profile>/
  npm-pack [--profile <p>] [--bin-dir <dir>] [--out <dir>] [--smoke] [--keep]
                               Build the npm packages for this machine, check contents and sizes
                               (npm pack --dry-run); --smoke installs each from tarballs in a temp
                               project and runs its bin, plus the @ts-r.dev/typescript drop-in test

Exit codes: 0 ok, 1 check failed / bump available / regression, 2 usage or runtime error.
";

fn main() -> ExitCode {
    let mut args = Args::from_env();
    let Some(cmd) = args.positional() else {
        eprint!("{HELP}");
        return ExitCode::from(2);
    };
    let result = match cmd.as_str() {
        "agents-check" => agents_check::run(args),
        "upstream" => upstream::run(args),
        "codegen" => codegen::run(args),
        "diag-codes" => diag_codes::run(args),
        "oracle" => oracle::run(args),
        "tasks" => tasks::run(args),
        "bench" => bench::run(args),
        "ecosystem" => ecosystem::run(args),
        "emit-repos" => emit_repos::run(args),
        "pgo" => pgo::run(args),
        "npm-pack" => npm_pack::run(args),
        "help" | "-h" | "--help" => {
            print!("{HELP}");
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command `{other}`\n\n{HELP}").into()),
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("xtask: error: {err}");
            ExitCode::from(2)
        }
    }
}

/// The command names `agents-check` accepts in `cargo xtask <cmd>` references.
pub const COMMANDS: &[&str] = &[
    "agents-check",
    "upstream",
    "codegen",
    "diag-codes",
    "oracle",
    "tasks",
    "bench",
    "ecosystem",
    "emit-repos",
    "pgo",
    "npm-pack",
    "help",
];
