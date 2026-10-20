# Ecosystem harness

Groundwork for the 1.0 gate "the top 1,000 npm packages build and pass their own tests" (M7).
`cargo xtask ecosystem` takes the most downloaded npm packages, runs ts-r over them, and
compares every result with an oracle: Node, tsc 7.0.2 and esbuild 0.28.2 (pinned in
[../bench/package.json](../bench/package.json)). The current numbers are in
[STATUS.md](STATUS.md); the code is `xtask/src/ecosystem/` ([../xtask/AGENTS.md](../xtask/AGENTS.md)).

```sh
# once: the pinned esbuild and tsc 7
(cd bench && ../scripts/limit.sh -- npm ci --ignore-scripts)
# top 100: parse, transform, bundle
cargo xtask ecosystem
# everything
cargo xtask ecosystem --top 1000 --stages parse,transform,bundle,check --budget-s 7200
# a few packages (leaves STATUS.md alone)
cargo xtask ecosystem --only chalk,zod --stages bundle
# the opt-in tests stage alone (first 5 eligible packages of the top 50)
cargo xtask ecosystem --top 50 --stages tests --tests --tests-top 5
# keep $TMPDIR/ts-r-ecosystem-<pid>/ to look at a failure
cargo xtask ecosystem --only minimatch --stages tests --tests --keep-tmp
# STATUS.md from the newest results; do the repros still fail?
cargo xtask ecosystem report
cargo xtask ecosystem repro [ecosystem/repros/bundle/x.mjs]
# re-pin packages.toml; delete the cache
cargo xtask ecosystem refresh --count 1000
cargo xtask ecosystem --clean
```

## Files

| Path | What |
|---|---|
| `packages.toml` | The pinned list: rank, name, exact version, downloads, tarball URL, `dist.integrity`, repository. `refresh` rebuilds it from [npm-high-impact](https://github.com/wooorm/npm-high-impact)'s download-ordered list (read as data), re-ranked with `api.npmjs.org` last-month counts and pinned to each package's `latest`. Never edit by hand. |
| `STATUS.md` | Committed summary of the last full run: pass rate per stage, bundle size versus esbuild, top failure causes with owners and repros, failing packages. Runs with `--only` or `--no-status` leave it alone. |
| `results/<date>.json` | Raw per-package, per-stage results (gitignored). |
| `repros/<stage>/<cause>.{js,ts}` | Minimized repros for the top failure causes (see Triage). |
| `js/resolve.mjs`, `js/shape.mjs` | Node helpers: resolve entry points like Node does, and print a module's exported shape. |
| `js/ts-r-hooks.cjs` | The tests stage's TypeScript loader (`module.registerHooks`, compiling with `ts-r-run --compile`), installed in place of ts-node / tsx / tap's loader. |

## What each stage checks

| Stage | Input | Pass when | Oracle |
|---|---|---|---|
| parse | every `.js/.mjs/.cjs/.jsx/.ts/.mts/.cts/.tsx/.d.ts` in the tarball | `ts_r_parser` reports no error, or the oracle rejects the file too | `node --check` for `.js/.mjs/.cjs`; tsc 7 `--noCheck --noResolve` (TS1xxx/TS8xxx/TS17xxx) otherwise |
| transform | shipped TS sources (not `.d.ts`) | `ts_r_transform` output reparses and has the same AST shape (comments ignored) as tsc's emit | tsc 7 emit with the same tsconfig (the package's own when it loads, else ES2022/ESNext/`react-jsx`) |
| bundle | each entry point: Node's resolution of every `exports` subpath without `*` (max 6), else `main` | ts-r-build builds it (unminified and `--minify`, `--platform=node`, format from the file's module type), the bundle loads in Node, and its exported shape (keys + `typeof`) equals esbuild's or the unbundled module's | esbuild 0.28.2 with the same flags; entries where esbuild's own bundle fails are skipped |
| check | `import * as m from "<pkg>"`, `strict`, `skipLibCheck: false`, `module: preserve`, `@types/node` installed | ts-r-check prints the same (file, line, column, code) set as tsc; diagnostics inside `@types/node` count only for `@types/node` itself | tsc 7 |
| tests | opt-in `--tests`: the first `--tests-top` (default 5, max 50) packages of the top 50 whose test pipeline runs tsc, tshy, tsx, ts-node or esbuild; source from the GitHub tag | the package's own `npm test` passes with those tools swapped for ts-r (below) and at least one swapped tool ran | the same suite with the package's own tools (if that fails, the package is skipped) |

Bundle sizes (minified, summed over entries) are recorded per package and summarized against
esbuild in STATUS.md.

### The tests stage

1. The repository's tarball at the version's tag (`v<version>`, `<version>`, `<name>@<version>`)
   comes from `codeload.github.com` into the cache (`repos/`). Monorepos (`workspaces`, or a
   root `package.json` with another name) and suites that depend on services or browsers
   (mongodb, redis, pg, puppeteer, playwright, karma, ...) are skipped.
2. Two copies are unpacked and installed (`npm ci` with a lockfile, else `npm install
   --before=<list date>`; `--ignore-scripts` always). Lint and format steps (xo, eslint,
   prettier, `npm run lint`, ...) are dropped from `test`/`pretest`/`posttest`; when the
   pipeline does not build and `prepare` or `build` exists, it runs first (install scripts
   were skipped).
3. Baseline: `npm test` in the first copy. ts-r: `npm test` in the second copy after the swaps:
   - `tsc` (`.bin/tsc` and `typescript/bin/tsc`, which tshy runs) → `xtask ecosystem shim tsc`:
     ts-r-check on the same project (command-line options in a temporary tsconfig that
     `extends` the project's), JavaScript and source maps from ts-r's transform one file at a
     time into `outDir`, declarations from tsc 7 `--emitDeclarationOnly --noCheck` (ts-r has no
     declaration emitter yet). Exit codes follow tsc.
   - `tsx`, `ts-node` binaries → `ts-r-run`; `esbuild` binary → `ts-r-build` (the esbuild JS
     API is not swapped).
   - ts-node, tap's ts-node fork and tsx entry points → `js/ts-r-hooks.cjs`.
   Every swapped tool appends to a log, so a pass where nothing swapped ran is a skip.
4. When ts-r-check's errors fail the run, `npm test` runs once more with them ignored
   (`TS_R_ECO_IGNORE_CHECK=1`): STATUS.md shows whether ts-r's emit and loader pass the suite.
5. Each `npm test` runs under limit.sh (2048 MB / 120 s, `TAP_JOBS=1`) inside
   `sandbox-exec` with outbound network denied except localhost on macOS (`unshare -rn` on
   Linux when allowed; otherwise the network stays open and the result says so).

## Limits and safety

- Tarballs are verified against `dist.integrity` (sha512) before they are unpacked, and
  re-verified before re-unpacking. At most `--max-downloads` (default 300) per run.
- The cache (`~/.cache/ts-r/ecosystem`, or `$TS_R_CACHE_DIR/ecosystem`) holds tarballs,
  unpacked sources and npm's cache for dependency installs. It is capped at 5 GB (least
  recently used first) at the start and end of every run.
- Package code only runs under `scripts/limit.sh`: `npm install --ignore-scripts` (2048 MB /
  240 s, dependencies resolved `--before` the list's date so runs are reproducible), and
  every (package, stage) worker at 2048 MB / 120 s for its whole process tree. Workers also
  install `ts_r_testing`'s allocator cap at 2048 MB.
- At most 4 jobs at a time, one thread each (ts-r-build `--threads=1`, esbuild
  `GOMAXPROCS=1`, tsc `--singleThreaded`, ts-r-check `--threads 1`).
- `--budget-s` (default 3600) bounds the run; stages not started in time are left out and
  STATUS.md says the budget ran out.
- Each run works in `$TMPDIR/ts-r-ecosystem-<pid>/` and deletes it at the end (unless
  `--keep-tmp`).

## Triage

Failures are grouped by cause: the diagnostic code and message for parse and check
(`missing`/`extra` relative to tsc), the node kinds at the first AST difference for
transform, the build or runtime error signature or shape difference for bundle, and
`limit: ...` / `crash: panicked at <file>` when a worker dies. Numbers and long quoted names are
blanked so the same bug groups together. Each cause gets an owning crate (`report::owner`).

For each of the top causes, minimize one example (see `.agents/skills/minimize-repro/SKILL.md`)
into `repros/<stage>/<slug>.{js,ts}`, where `<slug>` is the cause lower-cased with runs of
other characters turned into `-` and cut at 60 characters (`report::slug`), so STATUS.md links it. Each repro starts with a
comment block:

```js
// Cause:    <the cause line from STATUS.md>
// Found in: <package>@<version> <file>
// Owner:    <crate>
// Expected: <what node / tsc / esbuild do>
// Actual:   <what ts-r does>
```

`Owner:` overrides the heuristic owner in STATUS.md. A repro that needs several files uses
`// @filename: <path>` sections (TypeScript's test format). `cargo xtask ecosystem repro`
runs every repro through its stage's worker as if it were a package: for parse and transform
the files form the package directory; for bundle and check they form a package `repro` in
`node_modules/` (sections under `node_modules/` are other packages; without a
`package.json`, the first file is `main` and the first `.d.ts` is `types`). It prints whether
each repro still fails with its `Cause:`. A repro with a `// Manual:` line (it needs flags
the stage does not pass) is listed, not run.

Repros are inputs for the owning crate's agent, who turns them into fixture tests when they
fix the bug, then deletes the repro. This harness never changes compiler crates.
