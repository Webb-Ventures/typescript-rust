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

61c9d42 (2026-10-26, Apple M4 Max, darwin 25.6.0, Vite 8.3.4, Chrome 156.0.8078.47, `--jobs 1`):
**312 of 322 compared tests pass (96.9%)**; 77 tests are skipped. Earlier runs: 274 of 322 at
e07eafa; 202 of 322 at 58a940e; 101 of 254 at 8c14f29, with an older harness that loaded fewer
specs.

| Playground | Pass | Compared | Skip |
| --- | ---: | ---: | ---: |
| assets | 82 | 83 | 14 |
| assets-sanitize | 2 | 2 | 0 |
| base-conflict | 1 | 1 | 0 |
| build-old | 3 | 3 | 0 |
| chunk-importmap | 10 | 10 | 2 |
| csp | 7 | 7 | 2 |
| css-codesplit | 0 | 1 | 13 |
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
| hmr | 51 | 56 | 1 |
| hmr-root | 1 | 1 | 0 |
| html | 45 | 45 | 18 |
| import-attribute | 1 | 2 | 0 |
| lazy-compilation | 0 | 0 | 2 |
| minify | 0 | 0 | 3 |
| module-graph | 0 | 0 | 1 |
| nested-deps | 1 | 1 | 0 |
| object-hooks | 1 | 1 | 0 |
| preserve-symlinks | 2 | 2 | 0 |
| proxy-bypass | 2 | 2 | 2 |
| transform-plugin | 2 | 3 | 0 |
| tsconfig-json | 5 | 5 | 1 |
| wasm | 10 | 10 | 1 |

Remaining failures by cause (10):

- **HMR edge cases** (hmr, 5): a file deleted then restored (the page's `load` event times
  out); CSS HMR through `this.addWatchFile` (a no-op in the sidecar); deduplicating a
  server-rendered link stylesheet; reusing a server-rendered `<style>` (it is rewritten once
  more than in Vite); `acceptExports([])`'s `used:foo0` log (order-sensitive, see below).
- **Not implemented**: `this.addWatchFile` re-running a transform (transform-plugin, 1);
  `import.meta.glob` and dynamic-import-vars output visible to post plugins (glob-import, 1);
  an ESM `vite.config.js` in a `"type": "commonjs"` package (css-codesplit-cjs: ts-r-dev
  exits, no compared tests).
- **One each**: the injected `@vite/client` script under a nested base path (assets; design
  record 0010); style order of JS-inserted tags (css-codesplit).
- **Harness**: import-attribute "from source code" failed with `CDP: Cannot find context with
  specified id` (the page's context went away mid-evaluate); it passed 3 of 3 rerun alone.

Flaky and order-sensitive tests: `define > string`, `nested-deps` and `acceptExports([])`
have each passed and failed between full runs with no change to the code they test. The
`acceptExports([])` one reads the console log in order, and ts-r-dev's client connects (and
logs) before the page's modules run, so `used:foo0` can land outside the window the test
reads. All three passed in the run before this one (312 of 326 at 6a39428).
Tests that fail against Vite in a run are skipped, so the compared count moves with Vite's own
flakes: css-codesplit's first three tests (the page isn't styled yet when Vite is tested) and
one wasm test. ts-r-dev passes the css-codesplit ones (the CSS module one since 61c9d42).
