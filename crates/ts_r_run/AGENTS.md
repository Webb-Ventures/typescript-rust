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
the preload for files the graph walk missed), `--resolve <parent> <specifier>` (prints the
tsconfig `paths`/`baseUrl` target or nothing; used by the preload for plain JavaScript
parents) and `--serve` (the hook strategy's compiler).
`ts-r-run --test [node flags]` with no file runs Node's test runner on its default patterns
(which include `.ts` files, as Node strips types by default; not with `--watch`): there is no
graph to build, so the preload, which each test process inherits, compiles every file when it
is loaded (`run_tests`), as `tsx --test` does.
Env: `TS_R_RUN_THREADS`, `TS_R_CACHE_DIR`. Exit code: Node's, or 128 + the signal that ended
it; 1 for compile
errors (printed tsc-style), 2 for usage errors. Needs Node 22.15+ (`module.registerHooks`).

## How a run works (strategy "prebuild", the default)

1. `manifest::check_stamp`: if every input recorded for (build, entry, flags) still has its
   size and mtime, start Node with the recorded manifest. Nothing else runs.
2. `Project::load`: nearest `tsconfig.json` (no `include` globbing), `TransformOptions` for
   running (target ≥ ES2022, JSX runtime, no `importHelpers`), resolver in bundler mode.
   A `--tsconfig` that can't be read or whose `extends` is missing stops the run
   (`project::FATAL_CONFIG_CODES`, TS5083/TS6053, printed tsc-style, exit 1); a detected
   tsconfig with the same errors is ignored, as under tsx.
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
5. Start `node --import <hooks.mjs> <entry> args` with `TS_R_RUN_*` env vars (the preload
   deletes them so child processes don't see them) and stay its parent (signal relay below).
   With `--test` in the Node flags, `--enable-source-maps` goes first, as tsx does: Node's
   test runner takes each test's location from the stack of its `test()` call, which only
   Node's own source-map support maps to `.ts` lines (a later `--no-enable-source-maps` wins). `--import` takes a URL: on Windows the
   preload is passed as `file:///C:/...` (a bare `C:\...` is read as scheme `c:`).

Files: `lib.rs` (CLI, modes, launch), `compile.rs` (one file: format detection, transform,
print, specifiers, cache payload), `project.rs` (tsconfig, options, resolution policy),
`graph.rs` (engine with cache, parallel walk, build fingerprint), `manifest.rs` (manifest and
stamp formats), `watch.rs`, `serve.rs`, `js/hooks.mjs` (prebuild preload),
`js/preload-async.mjs` + `js/hooks-async.mjs` (hook strategy), `unix.rs` + `js/relay.mjs`
(signal relay, imported by both preloads). The JS files are embedded with
`include_str!` and written once per content to `run-graphs/hooks-<hash>/`.

## Invariants

- **Syntax errors stop a run; tsc's checker grammar errors that esbuild accepts do not**
  (`ts_r_transform::build_errors`, the same rule as ts-r-build). Only errors in files that load
  with the entry stop it: those reached through static imports and `require` from the entry
  (`graph::eager_closure`). A file reached only through an `import()` somewhere on the way is
  recorded in the manifest (`"e"`) and throws a `TransformError` ("Transform failed: ...")
  when it is loaded, so that `import()` rejects and the rest of the program runs, as under tsx.
- **Node semantics for module format**: `.mts`/`.cts`, then `package.json` `type`, then
  syntax detection (`compile::has_esm_syntax`): top-level `await` makes an ES module; a value
  `import`/`export` or `import.meta` makes one unless the file also uses a CommonJS global, in
  which case it runs as CommonJS (tsx compatibility). Also for tsx: a file in a `"type": "module"` package with no
  ES syntax that assigns `module.exports`/`exports.` runs as CommonJS, and the preload
  compiles a `.js` file with ES syntax that Node would run as CommonJS (`"type": "commonjs"`,
  or typeless and using CommonJS globals) instead of letting Node fail on it. Change both the
  doc comment and the tests when this changes.
- **Only relative specifiers, tsconfig `paths` and `baseUrl` are resolved by the graph**
  (`Project::resolve`; `baseUrl`, which tsc 7 dropped, because tsx still honours it). Bare
  package specifiers stay with Node (runtime conditions, `exports`). When Node fails, the
  preload's fallback tries what tsx tries: the `.js` → `.ts` swap, then (not for targets of
  `exports`/`imports`) added extensions, a directory's nested `package.json` `main` and its
  index, `.ts` first in local code and `.js` first inside node_modules. A plain JavaScript
  parent outside node_modules asks `--resolve` for `paths`/`baseUrl`. Declaration files are
  never resolution targets.
- **JSON imports need no import attribute** (the preload adds `type: 'json'`, as under tsx).
  A JSON object that is imported (not required) also has its keys as named exports: the
  preload turns it into a small ES module (`jsonModule`) whose `default` is the parsed object
  and which exports every key that can be an export name (not `default`). Arrays and other
  values are left to Node.
- **CommonJS output looks like tsx's** (tsx 4.23 `dist/register-*.mjs`, `dist/index-*.mjs`,
  read for behaviour):
  - `import.meta` works in CommonJS output: `compile::calls_import_meta` replaces it with a
    hoisted function that builds `{ url, filename, dirname }` once (`IMPORT_META`).
  - The `__esModule` marker is written as `("__esModule")` (`hide_esmodule_marker`): it is
    still set for `require` callers, but Node's CommonJS export detection doesn't list it, so
    an ES namespace import of the file shows only its exports.
  - `import(x)` everywhere becomes `import(x).then(m => ...)` that returns `m.default` when
    that is an object with `__esModule` (a CommonJS module that is a compiled ES module), so
    `(await import('./cjs.js')).default` is its default export (`unwrap_dynamic_imports`).
    The arrow is inline, not a helper, because tsx's suite `eval`s a function's text. A plain
    JavaScript ES module outside node_modules that calls `import(` is compiled for the same
    reason (`interopJs` in the preload).
  - A specifier with a query or hash (`./config.ts?x=1`) is a module of its own, CommonJS too:
    the preload resolves such a CommonJS file (and any file `require`d with a query) to an
    alias URL whose file name keeps the query and loads it from the real file; `import.meta.url`
    is the module's own URL (the preload rewrites `IMPORT_META_URL`).
  - `require()` of a module with top-level `await` throws a `TransformError` (Node's
    `ERR_REQUIRE_ASYNC_MODULE` renamed, its message after tsx's reason), and the preload's
    `Module.prototype.require` wrapper does only that.
- **Node never strips types from what the preload returns**: a `Module.prototype._compile`
  wrapper maps the `*-typescript` formats Node's CommonJS loader gives a `.ts` file to the
  plain ones for compiled files (Node refuses type stripping inside node_modules, and the code
  is already JavaScript).
- **Resolution details from tsx's `dist/require-*.mjs` and `register-*.mjs`**: an extensionless
  path never finds `.mts`/`.cts`; `require('pkg/file.js')` finds `pkg/file.ts` (the failed
  bare specifier is looked up in each parent `node_modules/<pkg>`); `require()` takes a
  `file:` URL; a path with a query that is a file loads it; `require('..')` and `require('.')`
  inside a package get the same fallback as `./x` (a directory's TypeScript index).
- **JSX**: with no `jsx` option JSX compiles to `React.createElement` calls (the classic
  runtime, honouring `jsxFactory`/`jsxFragmentFactory`), as esbuild does under tsx (tsx's
  smoke test); `preserve` and `react-jsx*` use the automatic runtime. `.jsx` files get the tsconfig's JSX options only with
  `allowJs`; without it they use `React.createElement` (`Project::plain_jsx`, in the cache key).
- **No absolute paths in cache payloads**: the source map's `sources` is the file's base name
  (resolved against the module URL by Node); `react-jsxdev` is downgraded to `react-jsx`.
- **Cache keys** include `graph::build_fingerprint()` (exe size + mtime), the options'
  `cache_key`, the file name, package type and source-map flag. Bump `CACHE_FORMAT` when the
  payload layout changes.
- **The manifest is immutable** once written (name = content hash; temp file + rename).
- **Nothing left running**: `--watch` children exit when their parent ts-r-run disappears
  (`TS_R_RUN_WATCH_PARENT` poll); `--serve` exits when its stdin closes; `--no-cache` runs
  delete their temp directory.
- **Signals reach the program, and a program that can't answer is stopped** (Unix, not
  `--watch`; the rule follows tsx's relay, read in tsx 4.23 `dist/cli.mjs` and
  `dist/preflight.cjs`, written here on our own design). ts-r-run spawns Node instead of
  exec'ing it and passes its pid in `TS_R_RUN_RELAY`. `js/relay.mjs` puts a listener first on
  SIGINT, SIGTERM and SIGHUP that sends SIGURG to ts-r-run ("got it") and exits with 128 +
  the signal number when the program has no listener of its own; it is hidden from
  `process.listeners()`/`listenerCount()` (counting one given listener hides it only when it is
  the one asked about) but stays in `rawListeners()`. On one of those signals ts-r-run waits
  30 ms for the report (Ctrl-C reaches the whole process group), then sends the signal on,
  waits 1 s (tsx: 30 ms; longer so a busy but responsive program isn't killed), then kills the
  child with SIGKILL and exits 128 + the signal. SIGQUIT, SIGUSR1 and SIGUSR2 are passed on
  as they are. Signals are sent with the `kill` utility (no `unsafe`, as in `watch.rs`).
  Windows waits for the child (no signals to relay).
- The preload must stay small and synchronous on the hot path (one `readFileSync` of the
  manifest, `JSON.parse` of the index only, code sliced lazily per module).

## Testing

- `cargo test -p ts_r_run`: unit tests for format detection, specifier collection, payload
  round trip, syntax errors, inline maps, CLI parsing, manifest layout, stamps,
  `parallel_map`, resolution policy.
- `tests/node.rs`: small projects run in Node under `scripts/limit.sh`, one per tsx behaviour
  the run oracle found missing (package resolution, lenient `.js`, JSON, `paths`/`baseUrl`
  from JavaScript, `module.exports` in a module package, `.jsx` without `allowJs`, a fatal
  `--tsconfig`, `--test` locations, the signal relay, `import.meta` and the `__esModule`
  marker in CommonJS, `?query` instances, syntax errors behind `import()`, `require` of
  top-level `await`, the `import()` unwrap, JSON named exports, TypeScript in packages,
  `require('..')` in a package, `--test` without files).
  Skipped when Node is missing.
- Parity oracle: [`bench/run-oracle`](../../bench/run-oracle/README.md) runs tsx's own suite and
  Node's TypeScript tests against ts-r-run (`node bench/run-oracle/run.mjs`; numbers there).
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
watching for `--watch`, same version and features as `ts_r_hmr`), `signal-hook` (Unix only,
`iterator` feature: receiving signals in a thread for the relay; std has no safe way to
install a signal handler, and the crate denies `unsafe`). Dev: `ts_r_testing`.

## Gaps

- The stamp doesn't notice a *new* file that would change a resolution (e.g. adding `a.ts` next
  to an `a/index.ts` that was resolved); edit any input or use `--no-cache`.
- Strategy "hook" doesn't see `require()` calls made inside CommonJS modules (Node's async
  hooks limitation); use the default strategy.
- Windows: tested in CI (the preload normalises `\`); `--watch` stops children with
  `Child::kill` only.
- Only the entry's tsconfig is used for every file (like tsx); per-package tsconfigs in a
  monorepo are ignored.
- From the run oracle (bench/run-oracle/README.md has the cases):
  - `require()` reaching an ES module: tsx compiles every such module to CommonJS with
    esbuild's interop (namespace keys sorted, `default` first, `.mjs` and ES packages turned
    into CommonJS); ts-r-run keeps Node's `require(esm)` and tsc's helpers. This is the two
    "package type: undefined" smoke cases (from `.js` and from `.ts`).
  - Hook strategy: the preload's resolve fallback, lenient `.js` and `--resolve` are only in
    the prebuild preload (`js/hooks.mjs`).
  - `require` isn't defined in ES output (tsx defines it).
