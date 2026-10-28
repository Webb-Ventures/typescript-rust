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

`--modules DIR` (or `TS_R_BENCH_MODULES`) takes the npm packages from another
`node_modules` directory instead of `bench/node_modules` (Vite itself, its version, the
packages playgrounds need and the `target/dev-oracle/node_modules` link), so a checkout can
reuse an installed one without running npm.

Whole playgrounds are skipped when they have no browser page (SSR), start a custom server
(`__tests__/serve.ts`), need npm packages the node_modules directory doesn't have (vue, sass,
react, ...), are build-only (legacy) or use `experimental.bundledDev` (hmr-full-bundle-mode).
Variant specs (`__tests__/<variant>/*.spec.ts`) run with the playground's
`vite.config-<variant>.js` (`--config` for ts-r-dev, the third argument of `vite-serve.mjs`),
each on a fresh copy, unless the variant has its own `serve.ts`. Workers run as pages do
(Chrome runs them). The worker sets `NODE_ENV=test` when unset, as vitest does. Tests are
skipped when they are build-only (`isBuild`), assert on the dev server's terminal output, or
use Vite's server object.
Full runs use `--jobs 1`: two playgrounds at once got Chrome killed at its
2048 MB cap during hmr.

## Results

dafb1fa (2026-10-27, Apple M4 Max, darwin 25.6.0, Vite 8.3.4, Chrome 156.0.8078.47,
`--jobs 1`, `--modules` naming a shared install):
**512 of 512 compared tests pass (100.0%)**; 97 tests are skipped, unchanged since c0449fb. Of the 97: 74 are build-only
(`runIf(false)` in serve), 8 assert on the dev server's terminal output, 4 use Vite's server
object, 4 are `test.skip` in the spec and 7 failed against Vite in the run; none of them is
a ts-r-dev gap. Since
b5d9371 (509 of 509, 100 skipped): the shim has `text=` selectors and
`page.frame(name).content()`, so html's "invalid > should reload when fixed" and
proxy-bypass's `async-proxy-bypass` are compared and pass, and `wasm > should work when
inlined` passed against Vite in this run. At b5d9371, three full runs in a row gave 509 of
509 twice and 510 of 510 once (the wasm test is a Vite-side flake, skipped when it fails
there). Since
2bd07f1 (322 of 322, 77 skipped): variant specs run (assets' encoded and relative bases, env's
NODE_ENV variant, the worker playground's es, iife and relative-base variants and others),
workers are no longer skipped, and the shim has `page.goto`'s response, `page.keyboard.press`,
element `getProperty`, `boundingBox` and shadow-root `$$eval`, so 187 more tests are compared.
What made them pass in ts-r-dev: `--config`, `new Worker(new URL(...))` URLs with
`worker_file&type=...`, classic workers, `build.assetsInlineLimit` as a function,
`optimizeDeps.exclude`, an encoded base in the server URL, `srcset` URLs with spaces, `t=` on
asset URLs after a hot update and `html.cspNonce` (`crates/ts_r_hmr/AGENTS.md`).
Earlier runs: 512 of 512 at c0449fb; 509 of 509 at b5d9371; 322 of 322 at 2bd07f1; 321 of 322 at 7cba502; 312 of 322 at 61c9d42; 274 of 322
at e07eafa; 202 of 322 at 58a940e; 101 of 254 at 8c14f29, with an older harness that loaded
fewer specs.

| Playground | Pass | Compared | Skip |
| --- | ---: | ---: | ---: |
| assets | 212 | 212 | 26 |
| assets-sanitize | 2 | 2 | 0 |
| base-conflict | 1 | 1 | 0 |
| build-old | 3 | 3 | 0 |
| chunk-importmap | 10 | 10 | 2 |
| csp | 9 | 9 | 0 |
| css-codesplit | 1 | 1 | 13 |
| css-codesplit-cjs | 0 | 0 | 3 |
| css-lightningcss | 9 | 9 | 4 |
| css-lightningcss-root | 1 | 1 | 0 |
| css-no-codesplit | 1 | 1 | 1 |
| data-uri | 3 | 3 | 1 |
| define | 6 | 6 | 0 |
| dynamic-import | 24 | 24 | 5 |
| dynamic-import-inline | 0 | 0 | 1 |
| env | 17 | 17 | 0 |
| env-nested | 3 | 3 | 0 |
| glob-import | 25 | 25 | 0 |
| hmr | 58 | 58 | 1 |
| hmr-root | 1 | 1 | 0 |
| html | 47 | 47 | 16 |
| import-attribute | 2 | 2 | 0 |
| lazy-compilation | 0 | 0 | 2 |
| minify | 0 | 0 | 3 |
| module-graph | 0 | 0 | 1 |
| nested-deps | 1 | 1 | 0 |
| object-hooks | 1 | 1 | 0 |
| preserve-symlinks | 2 | 2 | 0 |
| proxy-bypass | 3 | 3 | 1 |
| transform-plugin | 4 | 4 | 0 |
| tsconfig-json | 5 | 5 | 1 |
| wasm | 11 | 11 | 0 |
| worker | 50 | 50 | 16 |

No failures remain. The last one was glob-import's "import.meta.glob and dynamic import vars
transformations should be visible to post transform plugins": `enforce: 'post'` and
`order: 'post'` `transform` hooks now run in a late stage after glob expansion and the
dynamic-import-vars rewrite (`crates/ts_r_hmr/AGENTS.md`).

Flaky tests: `nested-deps > handle nested package` failed once (`expected '' to be
'F@2.0.0'`) in one full run before the 7cba502 merge and passed in every other run, and 3 of
3 rerun alone. `define > string` has passed in every run since 61c9d42. The order-sensitive
`acceptExports([])` log ("doesn't accept itself if any of its exports is imported") failed
once in six full runs during round 5 and passed in the others. HMR's file-deletion tests ("delete file should
not break hmr", "should hmr when file is deleted and restored") passed in every full run and in
three hmr-only runs after the watcher change (crates/ts_r_watch/AGENTS.md).
Tests that fail against Vite in a run are skipped, so the compared count moves with Vite's own
flakes: css-codesplit's first three tests (the page isn't styled yet when Vite is tested) and
one wasm test. Others fail against Vite in every run: html's three `invalid` tests (Vite 8.3.4
serves `invalid.html` with status 200 and no overlay). The worker iife variant's "sourcemap is correct after env is injected"
needed `page.frame`; with the shim's `page.frame` it fails against Vite in the c0449fb run (its
`waitForEvent("response")` times out). ts-r-dev passes the css-codesplit ones (the CSS module one since 61c9d42, the
style-order one since 7cba502).
