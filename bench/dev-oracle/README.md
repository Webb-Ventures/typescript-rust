# Vite playground oracle for ts-r-dev

`run.mjs` treats Vite's playground end-to-end specs as data: it reads the `__tests__/*.spec.ts`
files in `target/upstream/vite/playground/` (Vite 8.3.4, fetched by `cargo xtask upstream
sync`), and for every playground it can serve it:

1. copies the playground to `target/dev-oracle/work/<server>/<name>`,
2. for each spec file, starts a dev server on the copy: Vite 8.3.4 (`vite-serve.mjs`, Vite
   pinned in `bench/package.json`) or `ts-r-dev`, each under `scripts/limit.sh`,
3. opens the server's URL in headless Chrome (driven over the DevTools protocol by
   `cdp.mjs`, not Playwright), and runs the spec's tests in order through small stand-ins for
   vitest and Vite's `test-utils` (`shim/`), the way Vite's playground setup does,
4. records pass, fail or skip per test and server in `target/dev-oracle/results.json`.

A test is compared only when it passes against Vite; it passes when it passes against
ts-r-dev too. Only the specs are used; nothing from Vite's implementation runs in ts-r-dev.

```sh
cargo build --release -p ts_r_dev -j 4
(cd bench && ../scripts/limit.sh -- npm ci)
scripts/limit.sh --mem-mb 4096 --time-s 3600 -- node bench/dev-oracle/run.mjs --jobs 1 \
  --tsr target/release/ts-r-dev     # --playgrounds assets,hmr reruns some, merged into results.json
node bench/dev-oracle/fails.mjs [--playground hmr] [--skips]   # list failures
```

Whole playgrounds are skipped when they have no browser page (SSR), start a custom server
(`__tests__/serve.ts`), need npm packages that `bench/node_modules` doesn't have (vue, sass,
react, ...), only have variant specs, test web workers, are build-only (legacy) or use
`experimental.bundledDev` (hmr-full-bundle-mode). Tests are skipped when they are build-only
(`isBuild`), assert on the dev server's terminal output, or use Vite's server object.
Full runs use `--jobs 1`: two playgrounds at once got Chrome killed at its
2048 MB cap during hmr.

## Results

7cba502 (2026-10-26, Apple M4 Max, darwin 25.6.0, Vite 8.3.4, Chrome 156.0.8078.47, `--jobs 1`):
**321 of 322 compared tests pass (99.7%)**; 77 tests are skipped. The same in three full runs
in a row. Earlier runs: 312 of 322 at 61c9d42; 274 of 322 at e07eafa; 202 of 322 at 58a940e;
101 of 254 at 8c14f29, with an older harness that loaded fewer specs.

| Playground | Pass | Compared | Skip |
| --- | ---: | ---: | ---: |
| assets | 83 | 83 | 14 |
| assets-sanitize | 2 | 2 | 0 |
| base-conflict | 1 | 1 | 0 |
| build-old | 3 | 3 | 0 |
| chunk-importmap | 10 | 10 | 2 |
| csp | 7 | 7 | 2 |
| css-codesplit | 1 | 1 | 13 |
| css-codesplit-cjs | 0 | 0 | 3 |
| css-lightningcss | 9 | 9 | 4 |
| css-lightningcss-root | 1 | 1 | 0 |
| css-no-codesplit | 1 | 1 | 1 |
| data-uri | 2 | 2 | 2 |
| define | 6 | 6 | 0 |
| dynamic-import | 24 | 24 | 5 |
| dynamic-import-inline | 0 | 0 | 1 |
| env | 16 | 16 | 1 |
| env-nested | 3 | 3 | 0 |
| glob-import | 24 | 25 | 0 |
| hmr | 56 | 56 | 1 |
| hmr-root | 1 | 1 | 0 |
| html | 45 | 45 | 18 |
| import-attribute | 2 | 2 | 0 |
| lazy-compilation | 0 | 0 | 2 |
| minify | 0 | 0 | 3 |
| module-graph | 0 | 0 | 1 |
| nested-deps | 1 | 1 | 0 |
| object-hooks | 1 | 1 | 0 |
| preserve-symlinks | 2 | 2 | 0 |
| proxy-bypass | 2 | 2 | 2 |
| transform-plugin | 3 | 3 | 0 |
| tsconfig-json | 5 | 5 | 1 |
| wasm | 10 | 10 | 1 |

Remaining failure (1):

- **Not implemented**: `import.meta.glob` and dynamic-import-vars output visible to post
  plugins (glob-import). ts-r-dev expands globs and resolves template-literal imports when it
  rewrites a module's imports, after the cached transform and its post plugins have run, so a
  post `transform` hook still sees `import.meta.glob(...)` and the untouched `import(`./${x}.js`)`
  (Vite runs its glob and dynamic-import-vars plugins as normal transforms ahead of user post
  plugins: vite: packages/vite/src/node/plugins/importMetaGlob.ts, dynamicImportVars.ts).
  Moving both into the transform stage is a pipeline change for a later round.

Flaky tests: `nested-deps > handle nested package` failed once (`expected '' to be
'F@2.0.0'`) in one full run before the 7cba502 merge and passed in every other run, and 3 of
3 rerun alone. `define > string` has passed in every run since 61c9d42. The order-sensitive
`acceptExports([])` log no longer flakes: the client imports `/@vite/env` like Vite's, so the
page's modules run in the order Vite runs them. HMR's file-deletion tests ("delete file should
not break hmr", "should hmr when file is deleted and restored") passed in every full run and in
three hmr-only runs after the watcher change (crates/ts_r_watch/AGENTS.md).
Tests that fail against Vite in a run are skipped, so the compared count moves with Vite's own
flakes: css-codesplit's first three tests (the page isn't styled yet when Vite is tested) and
one wasm test. ts-r-dev passes the css-codesplit ones (the CSS module one since 61c9d42, the
style-order one since 7cba502).
