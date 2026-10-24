# Run oracle for ts-r-run

`run.mjs` treats two test suites as data and compares `ts-r-run` with the tools they test:

- **tsx**: tsx 4.23.15's own spec files (`target/upstream/tsx/tests/specs/*.ts`, pinned in
  `upstream/UPSTREAM.json`, fetched by `cargo xtask upstream sync`). They run unchanged under
  Node's type stripping. The oracle's stand-ins in `tsx-suite/` replace `manten` (the suite's
  test runner) and `tests/utils/tsx` (how the suite spawns the tool). Each spec runs twice:
  first with the subject set to tsx 4.23.15 (`bench/node_modules/tsx`, the reference), then
  with `ts-r-run`. Only tests that pass with tsx are compared, and a compared test passes when
  it also passes with `ts-r-run`.
- **node**: every `spawnPromisified(process.execPath, [...])` call in Node 24.21.0's
  `test/es-module/test-typescript*.mjs`. Its arguments are evaluated against a copy of
  `test/fixtures/typescript/`, and the case runs under Node, under tsx and under `ts-r-run` with
  the same arguments. When Node and tsx agree, the case is compared: `ts-r-run` passes when its
  exit code, stdout and error kind (syntax, type, module not found, ...) match theirs.

Only the tests and the reference outputs are used. Nothing from tsx's or Node's implementation
runs in `ts-r-run`, and the oracle never loads tsx's `src/`.

```sh
cargo build --release -p ts_r_run -j 4
(cd bench && ../scripts/limit.sh -- npm ci)
(cd bench/run-oracle && ../../scripts/limit.sh -- npm ci)
scripts/limit.sh --mem-mb 4096 --time-s 2400 -- node bench/run-oracle/run.mjs --source all --jobs 4
node bench/run-oracle/fails.mjs [REGEX]          # failures from target/run-oracle/results.json
node bench/run-oracle/fails.mjs --skips [REGEX]  # skips with their reasons
```

`--source tsx|node`, `--spec NAME` and `--filter REGEX` narrow a run. Every process runs under
`scripts/limit.sh`, at most four tests at a time, each with a timeout. The run uses its own
`TS_R_CACHE_DIR` and deletes it when it finishes.

Tests are skipped when they fail with tsx itself in this setup, or when they test something
`ts-r-run` does not offer by design: the Node API (`tsx/esm/api`, `register()`), the REPL,
watch mode, `--eval`/`--print`/stdin, the pty shell, and assertions on tsx's own transform
output, cache or error text. In the node source, cases are skipped when Node and tsx disagree
(type stripping only, versus a full transform), or when the arguments are not literal.
`results.json` lists every skip reason.

## Results

8699d15 (2026-10-24, macOS arm64, Node 24.21.0, tsx 4.23.15, `--jobs 4`):

- tsx: **75 of 86 compared cases pass (87.2%)**; 49 skipped.
- node: **36 of 36 compared cases pass (100%)**; 53 skipped.

| tsx spec | Pass | Fail | Skip |
| --- | ---: | ---: | ---: |
| smoke | 9 | 4 | 0 |
| tsconfig | 19 | 0 | 0 |
| cli | 8 | 4 | 11 |
| commonjs-mode-contracts | 8 | 0 | 0 |
| resolution-priority | 5 | 0 | 4 |
| version-sensitive | 26 | 3 | 22 |
| loaders | 0 | 0 | 2 |
| api, api-declarations, esm-hook-resolve, node-capabilities, process-interact, pty-shell, repl, transform, transform-cache, watch | 0 | 0 | 10 |

| Node test file | Pass | Fail | Skip |
| --- | ---: | ---: | ---: |
| test-typescript.mjs | 18 | 0 | 13 |
| test-typescript-commonjs.mjs | 4 | 0 | 10 |
| test-typescript-eval.mjs | 0 | 0 | 24 |
| test-typescript-module.mjs | 7 | 0 | 3 |
| test-typescript-transform.mjs | 7 | 0 | 3 |

In the node source, 29 skips are `--eval`, `--print` or stdin cases and 23 are cases where Node
and tsx disagree. In 14 of those 23 `ts-r-run` matches tsx and in 9 it matches Node.

Remaining tsx failures by cause:

- **No signal relay** (`ts_r_run`, 3, cli): `ts-r-run` hands the process over to Node rather than
  running a parent that relays signals, so "Kills child when unresponsive" (exit 130),
  "Doesn't kill child when responsive" and "Relay signal handler does not affect listenerCount
  overload" fail.
- **`--test` locations** (`ts_r_run`, 1, cli): "maps test locations" expects `test at
  test.ts:4:1` in the test runner's output.
- **Syntax errors in modules reached only by `import()`** (`ts_r_run`, 2, smoke "from .ts" in
  `type: module` and untyped packages): the graph compiles every reachable file before running,
  so `broken-syntax.ts`'s TS1005 stops the run, where tsx raises it when the `import()` runs.
- **`require` in ES output** (`ts_r_transform`, 2, smoke "from .js"): `import-from-js.js` uses
  `import.meta` and `() => require('./cjs/index')`, so it is ES and fails with "require is not
  defined" (only `import x = require()` gets a `require`). In `type: module` it also fails the
  CommonJS-context assertion in `mjs/index.mjs:7`.
- **CommonJS output details** (`ts_r_transform`, 2, version-sensitive):
  - "ESM imports preserve CommonJS-classified TypeScript contracts": `import.meta.url` is kept
    in CommonJS output (`named/config.ts` in a `type: commonjs` package), so Node's export
    detection finds no exports ("does not provide an export named 'defineConfig'").
  - "CJS namespace import shape": the `Object.defineProperty(exports, "__esModule", ...)` marker
    shows up as an extra `__esModule: true` in the ES namespace.
- **Top-level `await` behind `require()`** (`ts_r_run`, 1, version-sensitive): `config.js` with
  top-level `await` in a `type: module` package, required from `.cjs`, fails with Node's
  `ERR_REQUIRE_ASYNC_MODULE`; tsx throws a `TransformError` there.

| Run | tsx compared | tsx pass | tsx skip | tsx rate | node compared | node pass | node skip |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| harness only, no `ts_r_run` changes | 90 | 52 | 45 | 57.8% | 36 | 36 | 53 |
| resolution inside packages (TypeScript swap and nested `main` in `node_modules`, `exports` get the swap only), tsconfig `paths`/`baseUrl` for JavaScript parents with `.json` targets, JSON without import attributes, ES syntax in CommonJS-scope `.js`, `module.exports` in TypeScript in a `type: module` package, `.jsx` without `allowJs` uses `React.createElement`, fatal `--tsconfig` errors, empty directories as missing modules | 86 | 75 | 49 | 87.2% | 36 | 36 | 53 |
