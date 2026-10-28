# xtask

Repo tooling, run as `cargo xtask <command>` (alias in `.cargo/config.toml`). It is not shipped;
it keeps the repo in step with upstream TypeScript, checks agent docs, and runs benchmarks.
`cargo xtask help` prints every command and flag.

## Commands

| Command | What it does | Used by |
|---|---|---|
| `cargo xtask agents-check` | Every crate (and xtask) has `AGENTS.md`; relative links in agent docs resolve; `cargo xtask` references name real commands; every `SKILL.md` has `name`/`description` front matter matching its directory; every platform module (`windows.rs`, `unix.rs`, `macos.rs`, `linux.rs`, or that name's `mod.rs`) is gated to its platform (`src/platform_files.rs`) | CI, before finishing any task |
| `cargo xtask upstream check [--json] [--include-rc]` | Compares [`upstream/UPSTREAM.json`](../upstream/UPSTREAM.json) with npm and GitHub. Exit 1 when a newer TypeScript stable exists (`--include-rc`: also an RC) | nightly `upstream.yml` |
| `cargo xtask upstream bump [--to <ver>]` | Re-pins TypeScript (npm version, tag, commit, npm `gitHead`) and updates [`docs/state/compat.json`](../docs/state/compat.json) | bump PRs |
| `cargo xtask upstream sync [--minimal] [--commit <sha>] [--force]` | Sparse, shallow, blob-less checkout of the pinned microsoft/TypeScript commit into `target/upstream/typescript`: tests, baselines, `src/lib`, diagnostic catalogs, license files. `--minimal` = libs + diagnostics only. Records `lastSync` in compat.json | bump PRs, bench, codegen |
| `cargo xtask upstream diff <old> <new> [--out f.md] [--limit N]` | Markdown summary of new/removed/changed tests, baselines, lib files and diagnostic codes between two refs (`7.0.2`, `v7.0.2`, a SHA, or `pinned`) | bump PR descriptions |
| `cargo xtask codegen [diagnostics\|lint-globals] [--out f.rs] [--input dir] [--check]` | `diagnostics`: the diagnostics catalog (codes, categories, message templates, `lookup(code)`) from the synced catalogs; default output `target/codegen/diagnostics_generated.rs`, the diagnostics crate chooses where it lives. `lint-globals`: the global names each pinned lib file (`tsc/internal/bundled/libs/lib.*.d.ts`) declares, split into types and values, plus its `<reference lib>`s; checked in at `crates/ts_r_lint/src/lib_globals_generated.rs`. No target: all of them; `--check` without a target checks the checked-in outputs | bump PRs, CI (`--check`) |
| `cargo xtask oracle <dir>... [--out f.json] [--cmd "<tsc-like>"] [--from-output f.txt]` | Runs `npx -y -p typescript@<pinned> tsc --noEmit --pretty false -p .` in each project dir and writes sorted, normalized diagnostics (`project, file, line, col, code, category, message`). `--cmd` runs another tsc-compatible binary (ours) through the same normalizer | conformance work |
| `cargo xtask tasks <diff.json>` or `tasks --ours a.json --tsc b.json` | Groups differences (missing / extra / mismatched) by code, area and test feature into a markdown checklist. `diff.json` is `{"ours": <oracle report>, "tsc": <oracle report>}` | after `oracle` |
| `cargo xtask bench [filter...] [--list] [--save name] [--baseline f.json] [--bin-dir d] [--markdown f.md] [--no-fail] [--setup] [--memory]` | Runs [`bench/scenarios.toml`](../bench/scenarios.toml) with hyperfine, writes `bench/results/<name>.json`, prints a comparison table, exit 1 if one of our tools is more than `max_regression` and more than `noise_sigmas` combined standard deviations slower than the baseline, or failed (crashed, exited non-zero or went over its cap) where the baseline measured or has no measurement of it (the regressed ids go to `bench/results/<name>-regressions.txt`). `--compare <run.json> --baseline <f.json>` gates two saved runs without running anything (CI's confirming re-run; a failed measurement on either side keeps the regression). `--memory`: one extra run per tool under `/usr/bin/time` for the peak RSS of its largest process (a column in the table) | `bench.yml`, perf work |
| `cargo xtask bench --repos <names\|all> [--large] [--mem-mb N [--dedicated-machine]] [--time-s N] [filter...]` | Only the scenarios with `repo = "<name>"` (never part of a default run): pinned public repos from [`bench/repos.toml`](../bench/repos.toml) (`src/bench_repos.rs`), run in `target/bench-repos/<name>` with absolute `{bin}`/`{node_bin}`/`{tmp}`, each hyperfine under `scripts/limit.sh` at the repo's `mem_mb`, `TS_R_LIB_DIR` pointed at the synced libs. A scenario's `setup` runs once before each tool's runs (`hyperfine --setup`: untimed, same cap and directory), e.g. `bench/js/eslint-same-rules.mjs` for `lint-trpc-same-rules`. Repo scenario ids are `<kind>-<repo>` or `<kind>-<repo>-<variant>`. `size = "large"` repos refuse to run or fetch without `--large`. `--mem-mb` above 8192 (the shared-machine cap) is refused without `--dedicated-machine`. See [`bench/README.md`](../bench/README.md) | M7 bench repos |
| `cargo xtask bench --fetch-repos --repos <names\|all> [--no-install] [--dry-run]` / `--editor [--run]` / `--clean-repos` | Shallow (`--depth 1 --filter=blob:none`, sparse where set) checkout of each pinned commit, `install` + `post_install` under limit.sh, `<tsconfig>.ts-r-bench.json` check wrappers, the three.js x10 input. `--editor` prints (with `--run`, runs) `editors/vscode/bench/editor-latency.js` and `editor-memory.js` on the repo's file set. `--clean-repos` deletes checkouts | M7 bench repos, editor memory |
| `cargo xtask emit-repos [--only a,b] [--fresh] [--tsc <path>] [--bin <ts-r-check>] [--markdown f.md] [--no-fail]` | Emit phase E6 (design record 0012): runs tsc 7 and ts-r-check on each case of [`bench/emit-repos.toml`](../bench/emit-repos.toml) (`src/emit_repos.rs`), a fetched bench checkout or an ecosystem-cache package or GitHub tarball (copied to `target/emit-repos/src/`), one after the other in the same tree, each under `scripts/limit.sh` and `/usr/bin/time` with 3 threads. New files are moved to `target/emit-repos/out/<case>/{tsc,ours}`; compared: file sets and bytes per kind (`.tsbuildinfo` by path only), exit codes, diagnostics, peak RSS. tsc's side is cached by binary, case and source. Writes `target/emit-repos/report.md`; exit 1 unless 95% of the gated cases' files are byte-identical and ts-r's peak RSS is at or under tsc's on each gated case | E6 emit hardening |
| `cargo xtask ecosystem [--top N] [--only a,b] [--stages parse,transform,bundle,check] [--tests [--tests-top N]] [--jobs N] [--budget-s S] [--max-downloads N] [--no-build] [--no-status] [--keep-tmp] [--out f.json]` | Runs the pinned top npm packages ([`ecosystem/packages.toml`](../ecosystem/packages.toml)) through ts-r and compares with node, tsc 7 and esbuild; writes `ecosystem/results/<date>.json` and, for full runs, [`ecosystem/STATUS.md`](../ecosystem/STATUS.md). `--clean` deletes the tarball cache. See [`ecosystem/README.md`](../ecosystem/README.md) | M7 ecosystem gate |
| `cargo xtask ecosystem refresh [--count N]` / `ecosystem report [results.json]` | Re-pin `packages.toml` from npm download counts / rewrite STATUS.md from a results file | list updates, triage |
| `cargo xtask diag-codes [--out f.json]` | Dumps every diagnostic code ts-r can report as JSON (`code`, `category`, tsc's message, reporting crates): the checker's generated catalogue filtered to codes some crate emits (`dg::NAME_<code>` in `ts_r_checker`, message-table names used outside tests in `ts_r_diagnostics` and `ts_r_binder`, literal codes in `ts_r_config` and `ts_r_check`), plus the TS99xxx codes ts-r adds. Reads source text only (`src/diag_codes.rs`); default output stdout | error-code pages (`landing/data/diagnostics.json`) |
| `cargo xtask pgo [--profile release\|dist] [--target <triple>] [--repos <names\|all>] [--ecosystem <dir>] [--samples N] [--builder cargo\|zigbuild] [--use-profile <f>] [--profile-out <f>] [--no-rebuild] [--keep-profiles]` | Profile-guided build of the seven shipped binaries (`src/pgo.rs`): finds `llvm-profdata` in rustup's `llvm-tools` (adds the component only if missing), builds instrumented binaries into `target/pgo/instrumented`, trains them read-only on the fetched public bench repos (`target/bench-repos`, from `bench --fetch-repos`; `--repos` narrows them: check wrappers, lint of their sources, builds of the excalidraw and three.js x10 entries, `ts-r-run --compile`), ecosystem samples (`~/.cache/ts-r/ecosystem/src`) and repo fixtures, merges into `target/pgo/merged.profdata` and rebuilds into `target/pgo/optimized/<target>/<profile>/` (plus the bench `build_loop` example). `--use-profile` only rebuilds (same-arch cross builds); `--no-rebuild` stops after merging | release workflow, perf work |
| `cargo xtask npm-pack [--profile <p>] [--bin-dir <dir>] [--out <dir>] [--smoke] [--keep]` | Builds the npm packages for this machine (`src/npm_pack.rs`, via [`npm/scripts/build-packages.mjs`](../npm/README.md)) into `target/npm`, checks each with `npm pack --dry-run` (required files, packed-size budgets in `BUDGETS`) and prints a size table. `--smoke` packs tarballs and installs each tool package alone in a temp project (offline, `--ignore-scripts`), checks nothing else was installed, and runs `--version` plus a smoke command per tool, then the `@ts-r.dev/typescript` drop-in test: installed as `typescript` with the pinned real TypeScript as `typescript-real`, `npx tsc --noEmit` on `npm/tests/fixtures/{pass,errors,mismatch}` must match real tsc (exit code, stdout) with parity passing, passing, failing | release workflow, packaging work |

Exit codes: 0 ok, 1 check failed / bump available / regression, 2 usage or runtime error.

## Invariants

- **Data only from upstream.** `upstream sync` refuses any sparse pattern outside tests,
  baselines, `src/lib`, `tests/lib`, diagnostic catalogs and license files (`is_data_path` in
  `src/upstream.rs`). Never widen that list to compiler source. The synced tree lives in the
  gitignored `target/`, never in git.
- **`UPSTREAM.json` changes only in bump PRs** (`upstream bump` for TypeScript, Renovate for the
  rest). The `sync` / `syncMinimal` lists in it drive both `sync` and `diff`.
- **Generated files are never hand-edited.** Rerun `cargo xtask codegen`; CI runs `--check`.
  Generated output depends only on its inputs (sorted by code, no timestamps).
- **Deterministic output.** Oracle reports are sorted and use paths relative to the project dir;
  `BTreeMap` everywhere output order matters.
- **New commands** must be added to `COMMANDS` in `src/main.rs` (and sub-commands to the lists in
  `src/agents_check.rs`) or `agents-check` will flag docs that mention them.
- **Oracle and benchmark runs are capped.** The compiler under `oracle` and each `hyperfine` run
  under `bench` start through `scripts/limit.sh` (`util::limited`, or the repo's `mem_mb` cap
  for `--repos` scenarios; Windows has no limit.sh and runs them directly).
- Network goes through `curl` (`util::http_get`); set `GITHUB_TOKEN` to avoid GitHub rate limits.
  Git operations shell out to `git`.
- **Ecosystem runs never execute registry code outside `scripts/limit.sh`.** Tarballs are
  verified against `dist.integrity` before unpacking; `npm install` always gets
  `--ignore-scripts`; every (package, stage) is a separate `xtask ecosystem worker` process
  under limit.sh at 2048 MB / 120 s with `TS_R_MEM_CAP_MB=2048`; at most 4 jobs, one thread
  each. The cache (`~/.cache/ts-r/ecosystem`) stays under 5 GB; temp dirs are removed at the end.
  The tests stage runs a package's own `npm test` the same way (2048 MB / 120 s per run), inside
  `sandbox-exec` with outbound network denied on macOS.
- **Bench repos are opt-in and capped.** A default `cargo xtask bench` never fetches or runs
  them; `--repos` must name them. Every install and every benchmarked command runs under
  `scripts/limit.sh` at the repo's `mem_mb` (at most 8192 in `bench/repos.toml`; `--mem-mb`
  above 8192 is refused unless `--dedicated-machine` is also passed, which only a machine
  nobody else is using may do). Installs use `--ignore-scripts` (or Yarn's
  `--mode=skip-build`). Large repos need `--large`; `--list` and `--dry-run` never need it.
  Checkouts live in `target/bench-repos/` and are never committed.
- **The tests stage's swaps are harness code, not a compiler**: `ecosystem shim tsc` (`shim.rs`)
  only calls ts-r-check, `ts_r_checker::load_program` (which files to emit),
  `ts_r_transform`/`ts_r_codegen` and, for declarations, the reference
  tsc; `ecosystem/js/ts-r-hooks.cjs` only calls `ts-r-run --compile`. A missing feature in a
  shim must not be reported as a ts-r failure: fix the shim (as with source maps for tap's
  coverage check) and re-run.
- **The ecosystem harness reports, it never fixes**: failures become causes in STATUS.md and
  repros in `ecosystem/repros/<stage>/` for the owning crate. Workers always run as a release
  build (`<target>/release/xtask`, built by the driver, where `<target>` is `CARGO_TARGET_DIR`
  or `target/`; the ts-r binaries default to the same directory), since debug parsing would
  hit the limits.

## Release builds (M7)

npm-pack runs npm, node and every installed bin under `scripts/limit.sh` (2048 MB, 120-300 s),
removes its temp projects (unless `--keep`) and stops `ts-r-dev`'s whole process group.
The drop-in test fetches the pinned `typescript` with `--prefer-offline` (the one install that may hit the network).

`[profile.release]` is fat LTO, one codegen unit, `panic = "unwind"`; `[profile.dist]` (what the
release workflow ships) adds stripped debug info. `cargo xtask pgo` trains on read-only inputs
only: the bench checkouts are never written (outputs, caches and the lint config go to
`target/pgo/train-tmp`, deleted afterwards), every training command runs under
`scripts/limit.sh` (4096 MB, 300 s; on Windows, which has no limit.sh, a Job Object with the
same caps through `ts_r_testing::limited`), and raw profiles are deleted after merging.
ts-r-dev and ts-r-lsp are not driven (servers); they share the trained crates and the profile.

Measured on macOS arm64 (M-series, shared machine, load 15-30), hyperfine 2 interleaving the
builds, median wall time and retired instructions; relative to thin LTO, then to fat LTO. Rows
measured on a private app were dropped when training moved to the public bench repos; they
are not re-measured yet:

| workload | thin LTO | fat LTO | fat + panic=abort | fat + PGO |
|---|---:|---:|---:|---:|
| ts-r-check infra-project (1,498 files) | 198.7 ms | 192.4 ms (-3%) | 204.9 ms (+6%) | 185.9 ms (-3%; instr -5%) |
| build_loop, 20 rebuilds | 284.4 ms | 278.2 ms (-2%) | 275.9 ms (-1%) | 268.6 ms (-3%) |
| ts-r-run --no-cache hello.ts (mostly Node) | 30.6 ms | 30.5 ms | 31.3 ms | 30.9 ms |
| seven binaries, total size | 26.1 MB | 24.2 MB (-7%) | 20.7 MB (-15%) | 30.6 MB (+26%) |
| build time, -j 4 (varies with load) | 85 s | 226 s | 181 s | 217 + 107 s + 28 s training |

`cargo xtask bench` (best of three alternating runs, PGO vs plain release): version-startup
1.55 -> 1.62 ms (+4.5%, the larger binary); the app-bundle rows of that run were on the private
app and are not re-measured.
Note: `cargo xtask bench` parses hyperfine 1.x JSON; hyperfine 2.0 (Homebrew) changed the schema.

## Dependencies

- `serde` + `serde_json` (`preserve_order`): UPSTREAM.json, compat.json, oracle reports, hyperfine
  exports. `preserve_order` keeps compat.json's key order stable when we rewrite it.
- `toml`: reads `bench/scenarios.toml` and `bench/repos.toml`.
- `ts_r_parser` (workspace crate): parses the TypeScript lib files for `codegen lint-globals`,
  and package files in the ecosystem parse stage.
- `ts_r_transform`, `ts_r_codegen`, `ts_r_config` (workspace crates): the ecosystem transform
  stage (transpile with the package's tsconfig, compare AST shapes with tsc's emit) and the
  tests stage's `tsc` shim (tsconfig loading, command-line option types, per-file emit).
- `ts_r_checker` (workspace crate): the `tsc` shim loads the program (`load_program`) to emit
  every project file the root files import, as tsc does.
- `ts_r_testing`: the capped global allocator (`cap_memory!`), because `ecosystem worker` runs
  real-world input; and `limited`, the Windows Job Object caps for `pgo` training runs.
- No hashing crate: tarball integrity goes through `openssl dgst` (macOS, Linux CI).
- No HTTP client crate (we shell out to `curl`, present on all CI runners) and no CLI-parser crate
  (`src/args.rs` is 80 lines), to keep `cargo xtask` quick to build.

## Testing

`cargo test -p xtask` covers argument parsing, link/front-matter checks, version ordering, tsc
output parsing, catalog diffing, code generation, task grouping and the bench regression gate.
Tests also parse the real `upstream/UPSTREAM.json`, `bench/scenarios.toml`, `bench/repos.toml`
(validated, and every repo scenario matches a manifest entry and vice versa) and
`ecosystem/packages.toml`. Ecosystem unit tests cover integrity checks, ranking, entry-point
discovery, shape and emit comparison, failure grouping and STATUS.md rendering, and the tests
stage's script analysis (swappable tools, lint stripping, build step), GitHub URLs and the tsc
shim's command line and output paths.
