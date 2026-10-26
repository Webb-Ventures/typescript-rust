# Run oracle for ts-r-run

`run.mjs` treats two test suites as data and compares `ts-r-run` with the tools they test:

- **tsx**: tsx 4.23.15's own spec files (`target/upstream/tsx/tests/specs/*.ts`, pinned in
  `upstream/UPSTREAM.json`, fetched by `cargo xtask upstream sync`). They run unchanged under
  Node's type stripping. The oracle's stand-ins in `tsx-suite/` replace `manten` (the suite's
  test runner) and `tests/utils/tsx` (how the suite spawns the tool). Each spec runs twice:
  first with the subject set to tsx 4.23.15 (`bench/node_modules/tsx`, the reference), then
  with `ts-r-run`. Only tests that pass with tsx are compared, and a compared test passes when
  it also passes with `ts-r-run`. Tests that run a file through tsx's loader flags (`node
  --import tsx <file>`, `node --require tsx/cjs <file>`) keep those flags for tsx and run
  `ts-r-run <file>` for `ts-r-run`, which has one way to run a file.
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
`TS_R_CACHE_DIR` and deletes it when it finishes. `TS_R_ORACLE_KEEP=<dir>` copies each tsx
test's fixture to `<dir>/<n>` before it runs, to reproduce a failure by hand (with
`--filter`, so that only the failing tests are copied).

Tests are skipped when they fail with tsx itself in this setup, or when they test something
`ts-r-run` does not offer by design: the Node API (`tsx/esm/api`, `register()`), the REPL,
watch mode, `--eval`/`--print`/stdin, the pty shell, `--version`/`--help` text, and
assertions on tsx's own transform output, cache or error text. `process-interact.ts` tests
the suite's own helper and never runs tsx. In the node source, cases are skipped when Node
and tsx disagree (type stripping only, versus a full transform), or when the arguments are
not literal.
`results.json` lists every skip reason.

## Results

30863e8 (2026-10-26, macOS arm64, Node 24.21.0, tsx 4.23.15, `--jobs 4`):

- tsx: **100 of 100 compared cases pass (100%)**; 35 skipped.
- node: **36 of 36 compared cases pass (100%)**; 53 skipped.

| tsx spec | Pass | Fail | Skip |
| --- | ---: | ---: | ---: |
| smoke | 13 | 0 | 0 |
| tsconfig | 19 | 0 | 0 |
| cli | 13 | 0 | 10 |
| commonjs-mode-contracts | 8 | 0 | 0 |
| resolution-priority | 9 | 0 | 0 |
| version-sensitive | 36 | 0 | 15 |
| loaders | 2 | 0 | 0 |
| api, api-declarations, esm-hook-resolve, node-capabilities, process-interact, pty-shell, repl, transform, transform-cache, watch | 0 | 0 | 10 |

| Node test file | Pass | Fail | Skip |
| --- | ---: | ---: | ---: |
| test-typescript.mjs | 18 | 0 | 13 |
| test-typescript-commonjs.mjs | 4 | 0 | 10 |
| test-typescript-eval.mjs | 0 | 0 | 24 |
| test-typescript-module.mjs | 7 | 0 | 3 |
| test-typescript-transform.mjs | 7 | 0 | 3 |

In the node source, 29 skips are `--eval`, `--print` or stdin cases and 24 are cases where Node
and tsx disagree. In 20 of those 24 `ts-r-run` matches tsx and in 4 it matches Node.

No tsx case fails. The 35 skips were reviewed again: they test tsx's API, its REPL, `tsx
watch`, its `--eval`/`--print`, `--version` and `--help` text, an interactive pty, or tsx's own
source, and one ("require(esm) support controls extensionless .mjs resolution") fails with tsx
itself here. The 53 node skips are `--eval`/`--print`/stdin input or cases where Node and tsx
disagree. None of them can be compared with the same command.

| Run | tsx compared | tsx pass | tsx skip | tsx rate | node compared | node pass | node skip |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| harness only, no `ts_r_run` changes | 90 | 52 | 45 | 57.8% | 36 | 36 | 53 |
| resolution inside packages (TypeScript swap and nested `main` in `node_modules`, `exports` get the swap only), tsconfig `paths`/`baseUrl` for JavaScript parents with `.json` targets, JSON without import attributes, ES syntax in CommonJS-scope `.js`, `module.exports` in TypeScript in a `type: module` package, `.jsx` without `allowJs` uses `React.createElement`, fatal `--tsconfig` errors, empty directories as missing modules | 86 | 75 | 49 | 87.2% | 36 | 36 | 53 |
| signal relay with a listener hidden from `listenerCount`, `--enable-source-maps` for `--test` | 86 | 79 | 49 | 91.9% | 36 | 36 | 53 |
| format detection (only top-level `await` forces ESM), `import.meta` and a hidden `__esModule` marker in CommonJS output, `?query` instances, `import()` default unwrap, syntax errors behind `import()` deferred to load time, `TransformError` for `require` of top-level `await`, JSON named exports, no type stripping of preload output in `node_modules` | 86 | 84 | 49 | 97.7% | 36 | 36 | 53 |
| the loader-flag cases (`--import tsx`, `--require tsx/cjs`) and `tsx --test` brought in; `ts-r-run --test` without a file, `require('..')` to a package's TypeScript index | 100 | 98 | 35 | 98.0% | 36 | 36 | 53 |
| `require()` of ES modules compiled to CommonJS (sorted exports, `default` first in `__importStar`), one instance for a file both required and imported, native `import()` in CommonJS output, `?query` after a plain import in CommonJS, `require` through an `imports` map | 100 | 100 | 35 | 100% | 36 | 36 | 53 |
