# Benchmarks

`cargo xtask bench` runs [scenarios.toml](scenarios.toml) with hyperfine and compares our
tools with pinned reference tools ([package.json](package.json): esbuild 0.28.2, tsc 7.0.2,
oxc-parser, tsx, Vite 8.3.4 with @vitejs/plugin-react 6.1.2). Every benchmark runs on public
code: micro inputs in the repo and the pinned open-source repos below. The PR workflow and the 5% regression gate (above the noise, confirmed by a re-run) are in
[../.agents/skills/bench-compare/SKILL.md](../.agents/skills/bench-compare/SKILL.md); the
command reference is in [../xtask/AGENTS.md](../xtask/AGENTS.md).

| Path | What |
|---|---|
| `scenarios.toml` | Every scenario: the micro scenarios (default run) and the pinned-repo scenarios (only with `--repos`) |
| `repos.toml` | The pinned public repositories: commit, install command, projects, linter, editor file set |
| `package.json` | Pinned reference tools, installed into `bench/node_modules` |
| `dev/` | The dev-server harness (`dev.mjs`: ts-r-dev against Vite in headless Chrome) and each app's entry files (`dev/excalidraw/`) |
| `esbuild-oracle/` | esbuild's bundler tests as an oracle for ts-r-build ([README](esbuild-oracle/README.md)) |
| `js/` | Small Node helpers the scenarios call |

hyperfine 1.x is required (`cargo xtask bench` parses its 1.x JSON). Without one on `PATH`,
`cargo install --locked --root target/tools hyperfine@1.20.0` keeps it inside the repo and
the bench finds it there. `--memory` adds one extra run per tool under `/usr/bin/time` and
reports the peak RSS of its largest process (not the sum of a process tree).

## Pinned public repos

Chosen by the project owner for the benchmark suite and the before-and-after editor-memory
runs. Each is pinned in [repos.toml](repos.toml) to its default branch's head on 2026-10-15
(`git ls-remote`), checked out shallow and blob-less into `target/bench-repos/<name>`
(gitignored), and never part of a default `cargo xtask bench`.

| Name | Repo | Commit | Group | Size | Scenarios |
|---|---|---|---|---|---|
| `vscode` | microsoft/vscode | `640a33ebdea0` | large app | large | check, lint, editor |
| `sentry` | getsentry/sentry | `2c72ecdf192b` | large app | large | check, lint (oxlint), editor |
| `grafana` | grafana/grafana | `872635d5545f` | large app | large | check, lint, editor |
| `supabase` | supabase/supabase | `c8c12a87b46a` | large app | large | check, lint, editor |
| `backstage` | backstage/backstage | `50407b790c6f` | monorepo | large | check, lint (pending), editor |
| `n8n` | n8n-io/n8n | `ed2500ffb2cb` | monorepo | large | check, lint, editor |
| `cal-com` | calcom/cal.com | `54343aa685ae` | monorepo | large | check, lint (Biome), editor |
| `next-js` | vercel/next.js | `6dda4371effe` | monorepo | large | check, lint, editor |
| `react` | facebook/react | `098d2e87a1f7` | monorepo | large | check (React Compiler), editor |
| `playwright` | microsoft/playwright | `a038fe7e0eb9` | TS 7 set | medium | check, lint, editor |
| `trpc` | trpc/trpc | `10e2ef98f784` | TS 7 set | medium | check, lint, editor |
| `date-fns` | date-fns/date-fns | `717ce0a807ea` | TS 7 set | small | check, lint (oxlint), editor |
| `rxjs` | ReactiveX/rxjs | `54796b38a57e` | TS 7 set | medium | check, editor |
| `typeorm` | typeorm/typeorm | `fe9239428751` | TS 7 set | medium | check, lint, editor |
| `three-x10` | mrdoob/three.js | `8d159b79b3f2` | bundler | small | build |
| `excalidraw` | excalidraw/excalidraw | `4c37956ba5f0` | app | medium | build, rebuild, dev server |

The full SHAs, install commands, projects and per-repo notes are in `repos.toml`.

```sh
# what would run, and the exact commands (works for every repo, large ones included)
cargo xtask bench --list --repos all
cargo xtask bench --fetch-repos --repos all --dry-run

# fetch, install and prepare the small and medium repos (each install under limit.sh)
cargo xtask bench --fetch-repos --repos trpc,date-fns,rxjs,typeorm,playwright,three-x10,excalidraw
# run their scenarios with peak memory, then compare against a saved run as usual
cargo build --release -j 4 -p ts_r_check -p ts_r_lint_cli -p ts_r_build -p ts_r_lsp -p ts_r_dev
cargo build --release -j 4 -p ts_r_bundler --example build_loop
cargo xtask upstream sync --minimal          # lib files for ts-r-check / ts-r-lint
RAYON_NUM_THREADS=4 cargo xtask bench --repos trpc,date-fns,rxjs,typeorm,playwright,three-x10,excalidraw \
    --memory --save repos
# one scenario kind or one repo: filters still match scenario ids
cargo xtask bench --repos all check-
# delete the checkouts afterwards
cargo xtask bench --clean-repos --repos all
```

How the repo scenarios run:

- Commands run in the checkout. `{repo}` is its path; `{bin}`, `{node_bin}` and `{tmp}`
  (`target/bench-tmp`) become absolute. `TS_R_LIB_DIR` points at the synced TypeScript libs.
- Each tool's hyperfine process runs under `scripts/limit.sh` at the repo's `mem_mb` (4096 for
  small and medium repos, 8192 for large ones) and the scenario's `time_s` (`--time-s`
  overrides it). A tool that is killed shows as `failed` in the table, with the reason.
- A scenario whose repo is not fetched shows as `skipped` with the fetch command.
- Installs run with `--ignore-scripts` (Yarn: `--mode=skip-build`): no dependency lifecycle
  scripts run. `post_install` is the one exception, for rxjs, which builds its own
  `@rxjs/observable-polyfill` because the package's `exports` point at build output.
- **Check wrappers.** The fetch writes `<tsconfig>.ts-r-bench.json` next to each project. It
  extends the project and sets `noEmit: true`, `incremental: false`, `composite: false` (plus
  the repo's `check_options`), so every tool does the same cold check and writes nothing.
  `ts-r-check` runs with `--no-history` (cold, like tsc); `ts-r-check-warm` uses its saved
  history. `tsc-7` is the pinned TypeScript 7.0.2; `tsc-repo` is the repo's own TypeScript.
  The pinned repos do not type-check cleanly under TypeScript 7, so all check tools run with
  `ignore_failure`; whether the diagnostics agree is checked separately (smoke runs below).
- Lint scenarios compare `ts-r-lint` (type-aware rules on, and with `--no-type-check`) with
  the repo's own linter: ESLint, oxlint (date-fns, sentry) or Biome (cal.com). ts-r-lint does
  not read legacy `.eslintrc*` configs, so backstage's lint tool is pending and rxjs and react
  have no lint scenario. Those runs are not like-for-like: the repo's ESLint also runs plugin
  rules ts-r-lint does not have.
- **Like-for-like lint (`lint-trpc-same-rules`).** The scenario's `setup` runs
  [js/eslint-same-rules.mjs](js/eslint-same-rules.mjs) with the ts-r-lint under test: it reads
  the binary's `--list-rules` (its rule registry) and writes trpc's `eslint.config.js` with
  every other rule removed, next to it, as `eslint.ts-r-same-rules.config.mjs` (type-aware rules
  included) and `eslint.ts-r-same-rules-untyped.config.mjs` (only rules that need no type
  information, by the plugin's `meta.docs.requiresTypeChecking`, and `project`/`projectService`
  off). Both tools get the same file with `-c` and the same paths. The untyped pair is the
  comparison: `ts-r-lint --no-type-check` (config cached, and `TS_R_LINT_NO_CONFIG_CACHE=1`,
  where Node evaluates the config as ESLint does) against ESLint. The typed pair runs ts-r-lint
  with an empty cache every run and `TS_R_LINT_PARITY=off`, because its parity gate turns the
  type-aware rules off on trpc today (one TS2322 that tsc 7 does not report); with the gate off
  it reports 11 problems ESLint does not, so that pair is not like-for-like.

### three.js x10

The bundler benchmark that esbuild and Bun publish: bundle ten copies of three.js at once, so
the bundler parses, links, tree-shakes and minifies about ten times the library. Our version:
the fetch takes three.js's `src/` at the pinned commit (sparse checkout), copies it to
`bench/copy1` ... `bench/copy10` inside the checkout and writes `bench/entry.js`, which imports
each copy's `Three.js` as a namespace and re-exports all ten. `build-three-x10` bundles that
entry minified, with source maps, as one IIFE with a global name, with `ts-r-build` (no cache,
and with its disk cache) and with esbuild 0.28.2 using the same flags. (esbuild's published
numbers use an older three.js release; ours uses the pinned commit, so absolute times differ.)

### excalidraw: app bundle, rebuilds and the dev server

excalidraw is the public Vite + React app: about 500 TS/TSX modules of its own (the app and its
`packages/*` workspaces, which Vite resolves from source) plus a typical dependency set (jotai,
Radix, Sentry, Firebase, CodeMirror, Mermaid). The fetch copies [dev/excalidraw/](dev/excalidraw/)
into `<checkout>/ts-r-bench-dev/`: an `index.html`, a `main.tsx` that renders
`excalidraw-app/App`, and a `vite.config.mjs` with the source aliases of excalidraw's own config.
SCSS is not compiled (ts-r-dev has no preprocessors yet): every `.scss` import resolves to an
empty stylesheet for both servers, so the page renders unstyled.

- `build-excalidraw` bundles `ts-r-bench-dev/main.tsx` minified as ESM with ts-r-build (no
  cache, and cached) and esbuild, same flags (`.woff2` as files, `.scss` empty).
  ts-r-build does not bundle template-literal `import()`s yet, so it leaves out the 61 locale
  JSON chunks esbuild emits (7.2 MB against 9.3 MB): the comparison flatters ts-r-build.
- `rebuild-excalidraw` is 20 rebuilds after editing one component, with `build_loop` (ts-r's
  incremental bundler) and esbuild's `context().rebuild()`.
- The dev-server run is not a hyperfine scenario (it needs a browser). `dev.mjs` starts each
  server under `scripts/limit.sh` (2048 MB), loads the page in headless Chrome over the
  DevTools protocol (a fresh Chrome per load, 3072 MB), and measures cold start (no caches),
  warm start, the time from a file write to the change in the DOM over 50 edits of the
  welcome-screen heading, whether React state survived (the edited DOM node is kept), and the
  servers' memory:

```sh
cd bench && npm ci && cd ..                  # Vite and the React plugin
cargo build --release -j 4 -p ts_r_dev
scripts/limit.sh --mem-mb 1024 --time-s 1800 -- node bench/dev/dev.mjs --app excalidraw \
    --server both --mode bench --out target/dev-excalidraw.json
```

`--mode check` (the default) does one load and three edits per server and prints console
errors and the server log; `TS_R_CHROME` points at another Chrome.

### Editor memory

The editor harnesses live in `editors/vscode/bench/`: `editor-latency.js` (a scripted LSP
client: first diagnostics, hover/definition/completion, keystrokes, RSS with one file and with
every source open; for ts-r-lsp, the repo's tsserver and TypeScript 7's LSP) and
`editor-memory.js` (a real VS Code, stock versus the ts-r extension). Each repo's
`[repo.editor]` names the files to open and the source directories `--open-all` walks.

```sh
cargo xtask bench --editor --repos trpc            # print the five commands, with caps
cargo xtask bench --editor --repos trpc --run      # run them; JSON in bench/results/editor/
```

## Caches

Nothing is downloaded or installed twice when nothing changed:

- **Checkouts.** A checkout already at the pinned commit is reused (no fetch); a changed
  `commit` in `repos.toml` fetches just that commit.
- **Install stamps.** After an install, `target/bench-repos/.stamps/<name>` records the commit,
  the install and `post_install` commands, the Node version and a hash of each lockfile. The
  next `--fetch-repos` skips the install when the stamp matches and `node_modules` exists.
  `--reinstall` ignores the stamps. three.js x10's copies have their own stamp.
- **Package caches.** Every install is offline-first against one shared cache,
  `~/.cache/ts-r/bench/` (or `$TS_R_BENCH_CACHE`): `npm/` (npm `--prefer-offline --cache`),
  `pnpm-store/` (pnpm `--prefer-offline --store-dir`), `yarn/` (Yarn 1 `--cache-folder`),
  `yarn-berry/` (Yarn 2+ global cache) and `corepack/` (package-manager downloads). The
  installs in `repos.toml` spell this out with `{cache}`; `cargo xtask bench` rejects an install
  that isn't offline-first.
- **Reference tools.** `bench/node_modules` is installed with `npm ci --prefer-offline` from
  npm's own cache (`~/.npm`, which CI's `setup-node` caches) and stamped
  (`bench/node_modules/.ts-r-bench-stamp`: Node version and the hashes of `package.json` and
  `package-lock.json`); `--setup` reinstalls.

A first fetch of the small and medium repos takes about 100 s and leaves about 7 GB in the
cache (pnpm 3.3 GB, Yarn 3.2 GB); a repeat takes about a second, and a fetch after
`--clean-repos` with a warm cache about 50 s (no downloads).

PR benches run on the review box (`bench.yml` hands them over; see
[the plugin README](../.agents/plugins/code-review/README.md#benches)), which keeps the same
directories, the crates and both release builds in `/srv/ts-r-review/bench` between runs. It
runs one bench per slot (`BENCH_SLOTS`; one by default), each slot on a cpuset of its own with
the PR and its base built and measured on the same cores, and by default measures one slot at a
time while the others build. It builds only the binaries `scenarios.toml` runs, and reuses the
reference tools' numbers from an earlier run while `bench/`, `xtask/`, `upstream/` and
`scripts/` are unchanged (the comment says so); our tools are measured on both sides every
time, so the gate is the same as a local `--baseline` run's.

To clear: `rm -rf ~/.cache/ts-r/bench` (package caches), `cargo xtask bench --clean-repos
--repos all` (checkouts and stamps), `rm -rf bench/node_modules` (reference tools), or
`--reinstall` to redo installs without deleting anything.

## Large repos: a dedicated machine

vscode, sentry, grafana, supabase, backstage, n8n, cal.com, next.js and react are wired up
(manifest, scenarios, editor sets, `--list` and `--dry-run`) but have **not been run**: their
installs (1-3 GB of `node_modules`, native builds skipped) and full checks with tsc or ESLint
exceed the 4096 MB cap of this shared machine, and tsserver with every source open needs more
still. `--repos` refuses them without `--large`.

One exception, its check only: react's `check-react` (the React Compiler,
`compiler/packages/babel-plugin-react-compiler/tsconfig.ts-r-bench.json`) fits under the cap
(peak RSS tsc 7.0.2 311 MB, ts-r-check 169 MB); its diagnostics are under
[Diagnostics against tsc 7](#diagnostics-against-tsc-7-2026-10-24). Its time was not measured
on a quiet machine and is not quoted.

Run them on a machine or CI runner with at least 32 GB of RAM and 50 GB of free disk, one repo
at a time:

```sh
cargo xtask bench --fetch-repos --repos vscode --large
RAYON_NUM_THREADS=4 cargo xtask bench --repos vscode --large --memory --save vscode
cargo xtask bench --editor --repos vscode --large --run
cargo xtask bench --clean-repos --repos vscode --large
```

`mem_mb` in `repos.toml` stays at 8192 (the most any command may use on the shared machine);
pass `--mem-mb 16384 --dedicated-machine` (or more) there, for example
`cargo xtask bench --repos vscode --large --mem-mb 16384 --dedicated-machine`. `--mem-mb` above
8192 without `--dedicated-machine` is refused: only pass it on a machine nobody else is using. Expect the first run to need command adjustments (for
example n8n's workspace packages may need `pnpm build` before their check, and cal.com's
Yarn 4 install needs corepack).

## Diagnostics against tsc 7 (2026-10-24)

Each `check-*` scenario's project at `ea41eac`, `--pretty false`, ts-r-check with
`--no-history`, compared with tsc 7.0.2 by file, line, column and code. Playwright and RxJS
were measured again on 2026-10-26 at `687b025` (the v1 checker round); the React Compiler row
is still from `ea41eac`:

| Project | tsc 7 | ts-r-check | Only ts-r-check | Only tsc |
|---|---:|---:|---:|---:|
| tRPC `packages/server` | 0 | 0 | 0 | 0 |
| date-fns `pkgs/core` | 0 | 0 | 0 | 0 |
| TypeORM `packages/typeorm` | 31 | 31 | 0 | 0 |
| Playwright | 13 | 13 | 0 | 0 |
| RxJS `packages/rxjs` | 21,908 | 21,903 | 0 | 5 |
| React Compiler | 156 | 227 | 71 | 0 |

4 of the 6 match tsc 7 exactly (tRPC, date-fns, TypeORM, Playwright). RxJS has none only
ours; its 5 missing are TS2344 in `*.spec.ts` from expect-type's `toEqualTypeOf`. The React Compiler's 71 extra are 65 TS4111
(property access on `env.config.*`, whose type is zod 4's `z.infer` of a schema), 4 TS7022,
1 TS7006 and 1 TS2345.

## Dedicated Linux run (2026-10-24)

The review box's bench of pull request #42 (bench.yml, on four dedicated cores of its bench
machine, linux-x86_64; the CPU model is not recorded): `d8a6a97` against main at `0697824`,
11 minutes. `d8a6a97` is the tree #42 merged as `8b6c630`; it differs from main at `0697824`
(and from `a74e016`) only in `crates/ts_r_run`, `bench/run-oracle`, `Cargo.lock` and two site
files, and no scenario below runs ts-r-run, so these are main's numbers. They are the README's
`bench:main` table (`node scripts/readme-bench.mjs main`) and the site's Linux sections
(`landing/data/bench.json`: `check-linux`, `build-linux`, `lint-linux`). Medians of the
`runs` in `scenarios.toml` (30 for version-startup, 10 for build-three-x10, 3 for lint-trpc, 5
for the rest); peak memory was not measured, and problems were not compared.

| Scenario | ts-r | ts-r, second variant | Reference |
|---|---:|---:|---:|
| version-startup | 2.03 ms | | esbuild 3.93 ms; tsc 7 61.3 ms |
| check-trpc | 474.4 ms | warm 401.0 ms | tsc 7 979.0 ms; repo tsc 5,176.7 ms |
| check-date-fns | 998.4 ms | warm 1,020.4 ms | tsc 7 1,980.1 ms; repo tsc 1,970.2 ms |
| check-rxjs | 1,202.1 ms | warm 140.9 ms | tsc 7 6,513.9 ms; repo tsc 14,249.3 ms |
| build-three-x10 | 962.9 ms | cached 484.6 ms | esbuild 1,409.8 ms |
| lint-trpc | 838.1 ms | no types 43.8 ms | ESLint 11,278.7 ms |
| lint-trpc-same-rules | 30.6 ms | cold config 2,850.7 ms; typed, empty cache 3,588.1 ms | ESLint 4,005.4 ms; typed 10,790.1 ms |
| lint-date-fns | 186.6 ms | | oxlint 1,103.3 ms |
| parse-dom-dts | pending (no `--parse-only`) | | esbuild 40.6 ms; oxc-parser 94.7 ms; tsc 7 165.0 ms |
| run-startup | not run (was pending) | | node --experimental-strip-types 112.1 ms; tsx 216.4 ms |

The gate flagged check-rxjs's warm run (+24.8% against the base); the confirming re-run, PR
first, gave 1,150.4 ms cold and 124.2 ms warm (-5.0% and -3.7%), so it passed. The saved
history barely helped on tRPC and not at all on date-fns here, where the laptop run below had
23 and 78 ms: not investigated yet.

## Headline run (2026-10-16)

The shared-laptop numbers on the site (`landing/data/bench.json`) and in
[docs/metrics.md](../docs/metrics.md). Shared macOS arm64 laptop (16 cores, 48 GB, other jobs
running), release build of `m7/bench-oss`, 4 threads, every command under `scripts/limit.sh`.
`RAYON_NUM_THREADS=4 cargo xtask bench --repos trpc,date-fns,rxjs,typeorm,playwright,three-x10,excalidraw --memory`
took 293 s; medians, peak RSS of one extra run:

| Scenario | ts-r | ts-r, second variant | Reference |
|---|---:|---:|---:|
| check-trpc | 108 ms, 117 MB | warm 23 ms, 15 MB | tsc 7: 189 ms, 294 MB; repo tsc: 1,130 ms |
| check-date-fns | 226 ms, 216 MB | warm 78 ms, 27 MB | tsc 7: 323 ms, 558 MB; repo tsc: 324 ms |
| check-rxjs | 462 ms, 458 MB | warm 57 ms, 33 MB | tsc 7: 1,280 ms, 1,168 MB; repo tsc: 2,468 ms |
| check-typeorm | 548 ms, 377 MB | warm 126 ms, 37 MB | tsc 7: 567 ms, 826 MB; repo tsc: 3,915 ms |
| check-playwright | 592 ms, 463 MB | warm 40 ms, 26 MB | tsc 7: 685 ms, 1,094 MB; repo tsc: 4,754 ms |
| lint-trpc | 186 ms, 148 MB | no types 20 ms, 9 MB | ESLint: 2,566 ms, 992 MB |
| lint-date-fns | 76 ms, 14 MB | | oxlint: 348 ms, 301 MB |
| lint-typeorm | 810 ms, 380 MB | no types 194 ms, 26 MB | ESLint: 6,718 ms, 1,748 MB |
| lint-playwright | 725 ms, 465 MB | no types 59 ms, 17 MB | ESLint: 5,441 ms, 2,341 MB |
| build-three-x10 | 378 ms, 459 MB | cached 241 ms, 275 MB | esbuild: 225 ms, 695 MB |
| build-excalidraw | 255 ms, 317 MB | cached 132 ms, 152 MB | esbuild: 201 ms, 624 MB |
| rebuild-excalidraw (first build + 20) | 1,358 ms, 515 MB | | esbuild: 3,457 ms, 292 MB |

Dev server (`bench/dev/dev.mjs --app excalidraw --server both --mode bench`): ts-r-dev cold
939 ms, warm 538 ms, save to DOM p50/p95 8.6/9.0 ms, 151-153 MB; Vite 8.3.4 cold 1,107 ms,
warm 1,032 ms, 35.0/37.2 ms, 364-376 MB. Both kept state on 53/53 edits with 0 reloads (details
in `crates/ts_r_hmr/AGENTS.md`).

Editor (`editor-latency.js`, the commands `--editor` prints; one run each):

| Repo | Tool | First diagnostics | Keystroke p50 / p95 | RSS one file / all open |
|---|---|---:|---:|---:|
| date-fns (1,641 files) | ts-r-lsp | 416 ms | 14.1 / 14.7 ms | 164 / 232 MB |
| | tsserver | timed out (120 s) | | |
| | TS 7 LSP | 168 ms | 5.0 / 5.8 ms | 469 / 710 MB |
| RxJS (231 files) | ts-r-lsp | 155 ms | 12.2 / 12.8 ms | 138 / 377 MB |
| | tsserver | 953 ms | 28.6 / 35.1 ms | 840 / 1,091 MB |
| | TS 7 LSP | 108 ms | 13.3 / 17.2 ms | 397 / 820 MB |
| tRPC (154 files) | ts-r-lsp | 224 ms | 9.4 / 10.5 ms | 95 / 165 MB |
| | tsserver | 800 ms | 24.3 / 29.3 ms | 571 / 757 MB |
| | TS 7 LSP | 58 ms | 6.2 / 7.4 ms | 477 / 480 MB |

ts-r-lsp reported 2 diagnostics on the opened RxJS file and 3 on tRPC's where tsc's LSP
reported 0 and 2. Lint keystrokes on tRPC's `resolveResponse.ts` (`lint-keystrokes.js
--open-all --src-dirs packages/server/src,packages/client/src`): ts-r-lsp 15.6/15.8 ms (10 ms
debounce included), 10.6 MB, with 35 configured rules not run in the editor; the ESLint
extension's server 3.0.34: 53.3/58.6 ms, 2,296 MB. Two editor windows on tRPC
(`crates/ts_r_lsp/examples/two_windows.rs`, TypeScript 6.0.3 backend): ts-r-lsp 698 MB, two plain
tsservers 838 MB, VS Code's default setup 1,451 MB.

Engine throughput on tRPC with its `node_modules`: `parse_corpus` 60,419 files, 456.5 MB at
225 MB/s on one thread (4 files report syntax errors); the binder's `corpus`, 11,937 files on 4
threads at 44,585 files/s, 0 panics.

Not re-run on public repos yet: the VS Code `editor-memory.js` runs and the large repos.

## Like-for-like lint run (2026-10-18)

`RAYON_NUM_THREADS=4 cargo xtask bench --repos trpc lint-trpc --memory` on the same shared
laptop (load average 5-7), ts-r-lint release build of the crates at `a7210058`, ESLint 10.10.0
and typescript-eslint 8.70.0 from tRPC's lockfile, Node 24.21.0. Median of 5 runs (lint-trpc:
3), peak RSS of one extra run:

| Scenario / tool | Median | Peak RSS | Problems from the rules |
|---|---:|---:|---|
| lint-trpc-same-rules: ts-r-lint (48 rules, no types, config cached) | 15.5 ms | 9 MB | 0 |
| ts-r-lint-cold-config (same, Node evaluates the config) | 733 ms | 336 MB | 0 |
| eslint (same 48 rules, no TypeScript program) | 1,015 ms | 515 MB | 0 |
| ts-r-lint-typed-cold (80 rules, parity gate off, empty cache) | 952 ms | 335 MB | 11 that ESLint does not report |
| eslint-typed (80 rules, projectService) | 2,605 ms | 973 MB | 0 |
| lint-trpc: ts-r-lint / no types / ESLint, own config | 228 / 19 / 2,781 ms | 167 / 10 / 986 MB | |

On this run ts-r-lint's parity gate turned the type-aware rules off in both tRPC projects
(ts-r-check reports a TS2322 at `stream/sse.ts:279` that tsc 7 does not), so `lint-trpc`'s
ts-r-lint row is a type check plus the untyped rules.

## three.js x10 bundler run (2026-10-21)

After the `a3/build-parity` speed work (parallel source-map assembly, interned export names in
the linker, reading module files ahead on one extra thread, fewer copies in codegen's source
maps). `RAYON_NUM_THREADS=4 cargo xtask bench --repos three-x10 --memory`, three invocations in
a row on the same shared laptop (Apple M4 Max, load average 11-14), ts-r-build release build at
`2fb25557` with `--threads=4`, esbuild 0.28.2 with its default threads. Median of 10 runs each,
peak RSS of one extra run:

| Invocation | ts-r-build, no cache | ts-r-build, cached | esbuild |
|---|---:|---:|---:|
| 1 | 231.8 ms, 422 MB | 128.1 ms, 246 MB | 262.7 ms, 696 MB |
| 2 | 233.9 ms, 420 MB | 134.2 ms, 243 MB | 238.5 ms, 695 MB |
| 3 | 255.0 ms, 422 MB | 132.2 ms, 244 MB | 246.2 ms, 695 MB |

Without the cache ts-r-build was faster in two invocations and slower in the third; the site
shows the third. The headline run above had 378 / 241 ms against 225 ms. Besides its 4 worker
threads ts-r-build runs one file-reading thread while the module graph loads. The outputs
still agree: both IIFEs load in Node with 10 copies x 443 exports, and ts-r's is 7,877,810
bytes against esbuild's 7,705,245 (+2.2%).

## Smoke runs (2026-10-15)

Shared macOS arm64 machine (16 cores, other agents running), `--memory`, hyperfine medians.
Peak RSS is the largest process. First run: release build of `m1/transform-run`. After the
fixes in `m5/bench-crashes` (known issues 1-4 and 7 below), the check and lint scenarios were
re-run; "Before" is the first run.

| Scenario | Tool | Before | Before RSS | After | After RSS |
|---|---|---:|---:|---:|---:|
| check-date-fns | ts-r-check | 192.4 ms * | 141 MB | 253.5 ms | 220 MB |
| | ts-r-check-warm | 99.8 ms * | 24 MB | 113.2 ms | 28 MB |
| | tsc 7 | 364.7 ms | 573 MB | 335.6 ms | 533 MB |
| | repo's tsc | 388.9 ms | 564 MB | 339.5 ms | 577 MB |
| lint-date-fns | ts-r-lint | 92.9 ms | 14 MB | 93.9 ms | 14 MB |
| | oxlint | 381.3 ms | 300 MB | 385.6 ms | 298 MB |
| check-rxjs | ts-r-check | 588.1 ms | 399 MB | 472.8 ms | 451 MB |
| | ts-r-check-warm | 57.7 ms | 32 MB | 58.9 ms | 32 MB |
| | tsc 7 | 1,317 ms | 1,143 MB | 1,301 ms | 1,136 MB |
| | repo's tsc | 2,433 ms | 762 MB | 2,407 ms | 763 MB |
| check-typeorm | ts-r-check | 236.1 ms * | 175 MB | 558.5 ms | 376 MB |
| | ts-r-check-warm | 143.2 ms * | 27 MB | 143.7 ms | 36 MB |
| | tsc 7 | 586.8 ms | 827 MB | 588.7 ms | 835 MB |
| | repo's tsc | 4,038 ms | 906 MB | 3,954 ms | 908 MB |
| lint-typeorm | ts-r-lint | killed: over 4096 MB | | 869.8 ms | 374 MB |
| | ts-r-lint-no-types | 181.1 ms | 27 MB | 176.4 ms | 25 MB |
| | eslint | 7,027 ms | 1,752 MB | 6,980 ms | 2,013 MB |
| check-trpc | ts-r-check | killed: over 4096 MB | | 108.1 ms | 117 MB |
| | ts-r-check-warm | killed: over 4096 MB | | 25.0 ms | 14 MB |
| | tsc 7 | 189.2 ms | 294 MB | 182.0 ms | 296 MB |
| | repo's tsc | 1,158 ms | 558 MB | 1,160 ms | 558 MB |
| lint-trpc | ts-r-lint | killed: over 300 s | | 194.5 ms | 146 MB |
| | ts-r-lint-no-types | 21.3 ms | 10 MB | 20.2 ms | 9 MB |
| | eslint | 2,740 ms | 971 MB | 2,711 ms | 978 MB |
| check-playwright | ts-r-check | 1,847 ms | 1,319 MB | 575.2 ms | 463 MB |
| | ts-r-check-warm | 45.6 ms | 26 MB | 45.5 ms | 25 MB |
| | tsc 7 | 700.4 ms | 1,128 MB | 671.1 ms | 1,128 MB |
| | repo's tsc | 4,729 ms | 1,273 MB | 4,742 ms | 1,308 MB |
| lint-playwright | ts-r-lint | killed: over 300 s | | 712.4 ms | 461 MB |
| | ts-r-lint-no-types | 57.7 ms | 17 MB | 56.9 ms | 16 MB |
| | eslint | 6,123 ms | 2,344 MB | 5,929 ms | 2,253 MB |
| build-three-x10 | ts-r-build | 383.4 ms | 461 MB | not re-run | |
| | ts-r-build (cached) | 254.1 ms | 276 MB | | |
| | esbuild | 268.3 ms | 695 MB | | |

\* Parse only: syntax errors stopped the semantic check (known issues 3 and 4, fixed), so
these were not comparable with tsc's times; the "After" runs check the whole program.

The type-aware lint runs report the ts-r/tsc diagnostic differences below and then lint
without type-aware rules (the parity gate), so they measure the type check plus untyped rules.
Gate status on 2026-10-16 (`m5/bench-parity`): trpc passes (type-aware rules run, 225 ms);
typeorm fails on one diagnostic, a TS1192 for `import chai from "chai"` under the repo's
`moduleResolution: node` (removed in TypeScript 7, TS5108; ts-r resolves it like `bundler`,
TypeScript 5.9 allows the synthetic default there); playwright fails on its 24 extras
(issue 6).

Editor (`editor-latency.js ts-r ... --open-all`, the first command of `--editor ... --run`):

| Repo | ts-r-lsp first diagnostics | RSS, one file | RSS, all sources open |
|---|---:|---:|---:|
| date-fns (1,641 files) | 410 ms (before: none, issue 11) | 162.6 MB | 229.3 MB |
| rxjs (231 files) | 186 ms (before: none, issue 11) | 137.8 MB | 375.6 MB |

TypeScript 7's LSP on date-fns (first run) showed first diagnostics in 211 ms, 463.5 MB with
one file open and 715.5 MB with all 1,641 sources open; the repo's tsserver timed out on
date-fns under the harness's 120 s wait. Those and the VS Code (`editor-memory.js`) runs were
not repeated after the fixes.

Diagnostics were compared by (file, line, column, code) with tsc 7, and lint problems by
(file, rule, line, column) with the repo's linter, from one run of each tool.

### Known issues found by the smoke runs

Issues 1-4, 7 and 11 are fixed on `m5/bench-crashes` (root causes below); the rest are open
and recorded for the owning crates. The diagnostic counts of issues 1 and 3-6 were then cut on
`m5/bench-parity` (status of 2026-10-16 in each item; fixtures in
`crates/ts_r_checker/tests/parity/`).

Checker (`ts_r_checker`, `ts_r_check`, `ts_r_config`, `ts_r_parser`):

1. **Fixed: trpc `packages/server`: ts-r-check ran out of memory** (killed at 4096 MB). Root
   cause: speculative checks (candidate inference, context-sensitive callbacks) cache nothing,
   and re-checked work multiplied: unannotated locals of a callback were re-typed at every
   reference, callbacks of nested generic calls were re-checked by every inference pass, and
   relating big generic interfaces (rxjs `Observable`) expanded structurally. Locals are now
   typed once per callback check, callback types are memoised per contextual type, relations
   have a work budget, and a speculation budget turns any remaining blow-up into one TS2589
   per file. Now 0.11 s / 117 MB. The 78 diagnostics tsc did not report (`packages/client`:
   86) are gone: both packages now match tsc 7 exactly (none on either side), 144 ms cold.
2. **Fixed: a command-line `--noEmit` did not count for TS5096.** The flag is now merged into
   the loaded options before option checks, as tsc does.
3. **Fixed: parser read `(readonly string[])` as a function type** (TS1005 in typeorm's
   `ForeignKey.ts`). typeorm is now checked fully, and now matches tsc 7 exactly (31
   diagnostics, none only ours or only tsc's; was 149 only ours and 1 only tsc's), 0.93 s.
4. **Fixed: JSON files in `include` were parsed as TypeScript** (7,460 TS1005 in date-fns's
   `pkgs/core/package.json`). `.json` files (included only with `resolveJsonModule` and an
   explicit `.json` pattern, as before) now parse as one JSON value and are not type-checked.
   date-fns is now checked fully and matches tsc 7 exactly (was 27 only ours). The type of an
   imported JSON module is now modelled (its widened value), except under the Node `module`
   kinds, where it is still `any`.
5. **rxjs `packages/rxjs`:** 21,887 diagnostics agree; 31 only ours (TS7006 11, TS2345 4,
   TS2769 4, TS2532 3, TS2322 3), 21 only tsc's (TS2345 9, TS2344 5, TS2684 4); was 21,007,
   1,333 and 901. Most of the rest come from relating rxjs's globally augmented `Observable`
   against generic constraints (variance through deep structures). Measured again on
   2026-10-24 at `087afc9`: tsc 21,908, ts-r 21,909; 21,903 agree, 6 only ours (TS2339 in
   `packages/test/src/assertions.ts`), 5 only tsc's (TS2344 in `*.spec.ts`). Measured again on
   2026-10-26 at `687b025`: tsc 21,908, ts-r 21,903, all agree, 0 only ours (`x.constructor
   === C` now narrows through C's prototype only), the same 5 only tsc's.
6. **playwright:** all 13 of tsc's diagnostics (missing generated files, TS2307) match, plus
   24 only ours (TS2322 9, TS2769 5, TS7053 2; was 814): overload resolution in `clock.ts`,
   `program.ts` and `network.ts`, `ElementHandle<T>` and `CommandReturnValues[T]` indexed
   returns, readonly tuples under `satisfies`, and a few JSX contexts. With the whole program
   checked the cold check now takes 2.34 s against tsc 7's 0.73 s (it was 575 ms when most
   bodies were not checked). Measured again on 2026-10-24 at `087afc9`: all 13 of tsc's match,
   plus 2 only ours (TS2604 on a `Story` JSX element in `packages/html-reporter/tests/main.tsx`
   and `packages/web/playwright/gallery/main.tsx`). tRPC, date-fns and TypeORM match tsc 7
   exactly (0, 0 and 31 diagnostics). Measured again on 2026-10-26 at `687b025`: all 13 of
   tsc's match and none only ours (JSX value tags are narrowable references, so `Story` is
   narrowed as tsc narrows it).

Linter (`ts_r_lint`):

7. **Fixed: type-aware linting panicked and then hung** on trpc and playwright
   (`index out of bounds ... 4294967071` at `crates/ts_r_checker/src/symbols.rs:301`). Root
   cause: with history levels, augmentation redirects listed for the whole program were
   merged again in a level above the one holding both ends (node_modules), with
   level-relative indexes. They are now filtered per level like other merges. The hang was a
   tsc parity worker pool that the panicking project never closed; the pool now closes on
   every exit path, and a checker panic in a project is caught and reported (the project is
   linted without type-aware rules). On typeorm the type-aware run exceeded 4096 MB from the
   same speculation blow-up as issue 1, made worse by condition chains in callbacks (now
   reused under unchanged speculative assumptions); it now takes 0.87 s / 374 MB.
8. **oxlint `no-unused-vars` flags parameters of function types and call signatures**
   (`type F = (date: Date) => void`, `(arg2: Arg2, arg1: Arg1): Result;`): 117 problems on
   date-fns `pkgs/core/src` that oxlint does not report (oxlint reports none). ts-r-lint also
   notes that oxlint's `env` is not mapped.
9. **typeorm, `--no-type-check`:** 2,174 of ESLint's 7,392 problems match and 2 are only ours;
   most of the 5,218 missing are `jsdoc/*` plugin rules (4,928) and type-aware rules (off).

Bundler (`ts_r_build`):

10. **three.js x10 bundles agree.** Both IIFEs load in Node with 10 copies x 443 exports and the
    same export names and `typeof`s; ts-r-build's output is 7.88 MB against esbuild's 7.71 MB
    (+2.2%), source maps 33.1 MB against 33.4 MB. Cold, ts-r-build was slower than esbuild in
    this run; with its disk cache it was level (see the table). Later runs: level cold on the
    laptop (three.js x10 bundler run) and 1.5x faster on the dedicated Linux run.

Language server (`ts_r_lsp`):

11. **Fixed: ts-r-lsp published no diagnostics on date-fns or rxjs.** The project thread
    panicked at the same `symbols.rs:301` underflow as in 7. Fixed with 7; the project thread
    now also survives a panic while arranging levels or building the checker (it falls back to
    one level, then publishes one internal-error diagnostic per file and rebuilds on the next
    change). First diagnostics: date-fns 410 ms, rxjs 186 ms (see the editor table).
