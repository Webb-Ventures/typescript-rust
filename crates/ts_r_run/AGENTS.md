# ts_r_run

The `ts-r-run` binary (`[[bin]] name = "ts-r-run"`): runs a TypeScript file on the Node the
user already has, replacing tsx and ts-node. `ts-r run` (the [`ts_r_cli`](../ts_r_cli/AGENTS.md)
launcher) execs it with arguments untouched. Root rules: [../../AGENTS.md](../../AGENTS.md).
Design and measurements: [../../docs/design/0002-transform-and-codegen.md](../../docs/design/0002-transform-and-codegen.md).

## CLI

`ts-r-run [--watch] [--tsconfig <file>] [--no-source-maps] [--no-cache] [--strategy
prebuild|hook] [--node <bin>] [node flags] <file> [args...]`. Unknown flags before the file
go to Node (`-r`, `--require`, `--import`, `--env-file`, `-C`, `--conditions`, ... take the next
argument). Internal modes: `--compile <file>` (prints `esm`/`cjs`, a newline, the code; used by
the preload for files the graph walk missed) and `--serve` (the hook strategy's compiler).
Env: `TS_R_RUN_THREADS`, `TS_R_CACHE_DIR`. Exit code: Node's (exec on Unix); 1 for compile
errors (printed tsc-style), 2 for usage errors. Needs Node 22.15+ (`module.registerHooks`).

## How a run works (strategy "prebuild", the default)

1. `manifest::check_stamp`: if every input recorded for (build, entry, flags) still has its
   size and mtime, exec Node with the recorded manifest. Nothing else runs.
2. `Project::load`: nearest `tsconfig.json` (no `include` globbing), `TransformOptions` for
   running (target ≥ ES2022, JSX runtime, no `importHelpers`), resolver in bundler mode.
3. `graph::build`: breadth-first from the entry, each level compiled in parallel; every file
   through `ts_r_cache` (namespace `run`, `graph::CACHE_FORMAT`). Specifiers come from the
   transformed tree (`compile::collect_specs`): type-only imports are never followed.
   A file importing `@ts-r.dev/guard` is first expanded by `ts_r_guard::Expander` (validators
   inlined; record 0007): the expanded text is what is compiled and cached, guard errors
   (TSR1501, ...) are printed tsc-style and stop the run, and the declaration files the
   validators came from become inputs of the stamp, so editing a type re-generates them.
   `[sound.guard]` (`ts_r_sound::config::guard_options` for the project's tsconfig) is read
   once per run; it is in the cache salt and its file is a stamp input.
4. `manifest::write` (`<cache root>/run-graphs/<blake3>.graph`, written once per content) and
   `write_stamp`.
5. Exec `node --import <hooks.mjs> <entry> args` with `TS_R_RUN_*` env vars (the preload
   deletes them so child processes don't see them). `--import` takes a URL: on Windows the
   preload is passed as `file:///C:/...` (a bare `C:\...` is read as scheme `c:`).

Files: `lib.rs` (CLI, modes, launch), `compile.rs` (one file: format detection, transform,
print, specifiers, cache payload), `project.rs` (tsconfig, options, resolution policy),
`graph.rs` (engine with cache, parallel walk, build fingerprint), `manifest.rs` (manifest and
stamp formats), `watch.rs`, `serve.rs`, `js/hooks.mjs` (prebuild preload),
`js/preload-async.mjs` + `js/hooks-async.mjs` (hook strategy). The JS files are embedded with
`include_str!` and written once per content to `run-graphs/hooks-<hash>/`.

## Invariants

- **Node semantics for module format**: `.mts`/`.cts`, then `package.json` `type`, then
  syntax detection (`compile::has_esm_syntax`); ES syntax mixed with CommonJS globals runs as
  CommonJS (tsx compatibility). Change both the doc comment and the tests when this changes.
- **Only relative specifiers and tsconfig `paths` are resolved by ts-r-run**; bare package
  specifiers stay with Node (runtime conditions, `exports`). Declaration files are never
  resolution targets.
- **No absolute paths in cache payloads**: the source map's `sources` is the file's base name
  (resolved against the module URL by Node); `react-jsxdev` is downgraded to `react-jsx`.
- **Cache keys** include `graph::build_fingerprint()` (exe size + mtime), the options'
  `cache_key`, the file name, package type and source-map flag. Bump `CACHE_FORMAT` when the
  payload layout changes.
- **The manifest is immutable** once written (name = content hash; temp file + rename).
- **Nothing left running**: `--watch` children exit when their parent ts-r-run disappears
  (`TS_R_RUN_WATCH_PARENT` poll); `--serve` exits when its stdin closes; `--no-cache` runs
  delete their temp directory.
- The preload must stay small and synchronous on the hot path (one `readFileSync` of the
  manifest, `JSON.parse` of the index only, code sliced lazily per module).

## Testing

- `cargo test -p ts_r_run`: unit tests for format detection, specifier collection, payload
  round trip, syntax errors, inline maps, CLI parsing, manifest layout, stamps,
  `parallel_map`, resolution policy.
- `tests/guard.rs`: `parse<T>` end to end (the `ts_r_guard` fixture app run in Node under
  `scripts/limit.sh`, compared with its `expected.txt`; guard errors; a type edit reaching a
  cached run). Skips the Node parts when Node is missing.
- Manual end-to-end checks used for M1 (no Node in CI yet): ESM + CJS + `.cts` + tsconfig
  `paths` + enums/namespaces/parameter properties; TC39 decorators (class replacement,
  field/accessor/method decorators, `addInitializer`, metadata); typeless packages; syntax errors; stack
  traces mapped to `.ts` lines; `--watch` restart on edit, recovery after a syntax error,
  SIGTERM to the child, no leftover processes.
- Benchmark: `cargo build --release -p ts_r_run --bin ts-r-run --example bench && TS_R_RUN_THREADS=4
  scripts/limit.sh -- target/release/examples/bench [--modules N] [--runs N] [--tsx <path>]`.

## Performance (Node 24.21.0, Apple M-series, 4 threads, median of 15)

| | hello.ts | 1,000 modules |
|---|---|---|
| `node -e 0` (floor) | 23 ms | 24 ms |
| `node --experimental-strip-types` | 50 ms | 215 ms |
| tsx 4.23 (binary; `npx tsx` adds ~400 ms) | 79 ms | 270 ms |
| ts-r-run warm | 29 ms | 101 ms |
| ts-r-run, entry touched (cache warm) | 30 ms | 116 ms |
| ts-r-run, empty cache | 32 ms | 137 ms |
| ts-r-run `--no-source-maps` | 29 ms | 81 ms |
| ts-r-run `--strategy hook` | 48 ms | 390 ms |

## Dependencies

`ts_r_span`, `ts_r_ast`, `ts_r_parser`, `ts_r_diagnostics`, `ts_r_codegen`,
`ts_r_transform`, `ts_r_config`, `ts_r_resolver`, `ts_r_cache`, `ts_r_guard` (`parse<T>`
call sites), `ts_r_sound` (only `config::guard_options`: `[sound.guard]`), `rustc-hash`, `notify` (file
watching for `--watch`, same version and features as `ts_r_hmr`). Dev: `ts_r_testing`.

## Gaps

- The stamp doesn't notice a *new* file that would change a resolution (e.g. adding `a.ts` next
  to an `a/index.ts` that was resolved); edit any input or use `--no-cache`.
- Strategy "hook" doesn't see `require()` calls made inside CommonJS modules (Node's async
  hooks limitation); use the default strategy.
- Windows: tested in CI (the preload normalises `\`); `--watch` stops children with
  `Child::kill` only.
- Only the entry's tsconfig is used for every file (like tsx); per-package tsconfigs in a
  monorepo are ignored.
