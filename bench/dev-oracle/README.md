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

e07eafa (2026-10-24, Apple M4 Max, darwin 25.6.0, Vite 8.3.4, Chrome 156.0.8078.47, `--jobs 1`):
**274 of 322 compared tests pass (85.1%)**; 77 tests are skipped. Earlier runs: 202 of 322 at
58a940e; 101 of 254 at 8c14f29, with an older harness that loaded fewer specs.

| Playground | Pass | Compared | Skip |
| --- | ---: | ---: | ---: |
| assets | 82 | 83 | 14 |
| assets-sanitize | 2 | 2 | 0 |
| base-conflict | 1 | 1 | 0 |
| build-old | 3 | 3 | 0 |
| chunk-importmap | 1 | 10 | 2 |
| csp | 7 | 7 | 2 |
| css-codesplit | 0 | 1 | 13 |
| css-codesplit-cjs | 0 | 0 | 3 |
| css-lightningcss | 7 | 9 | 4 |
| css-lightningcss-root | 1 | 1 | 0 |
| css-no-codesplit | 1 | 1 | 1 |
| data-uri | 2 | 2 | 2 |
| define | 5 | 6 | 0 |
| dynamic-import | 18 | 24 | 5 |
| dynamic-import-inline | 0 | 0 | 1 |
| env | 16 | 16 | 1 |
| env-nested | 3 | 3 | 0 |
| glob-import | 24 | 25 | 0 |
| hmr | 49 | 56 | 1 |
| hmr-root | 1 | 1 | 0 |
| html | 42 | 45 | 18 |
| import-attribute | 0 | 2 | 0 |
| lazy-compilation | 0 | 0 | 2 |
| minify | 0 | 0 | 3 |
| module-graph | 0 | 0 | 1 |
| nested-deps | 1 | 1 | 0 |
| object-hooks | 1 | 1 | 0 |
| preserve-symlinks | 2 | 2 | 0 |
| proxy-bypass | 1 | 2 | 2 |
| transform-plugin | 0 | 3 | 0 |
| tsconfig-json | 4 | 5 | 1 |
| wasm | 0 | 10 | 1 |

Remaining failures by cause (48):

- **HMR edge cases** (hmr, 7): invalidating a virtual module (2); a missing import on server
  startup, and a file deleted then restored (the page's `load` event times out, 2);
  `this.addWatchFile` (a no-op in the sidecar); deduplicating a server-rendered link
  stylesheet; reusing a server-rendered `<style>`.
- **Not implemented**: importmap (chunk-importmap, 9), wasm imports and `?url` (10), import
  attributes (2), CSS modules: class names aren't scoped or mapped (css-lightningcss, 2),
  plugin `moduleType: 'json'`, hook filters and `addWatchFile` (transform-plugin, 3), an ESM
  `vite.config.js` in a `"type": "commonjs"` package (css-codesplit-cjs: ts-r-dev exits).
- **Dynamic imports** (dynamic-import, 6): one instance for static and dynamic imports of
  `.mxd2`/`.mxdjson` files (2); variable imports through an alias, `#` subpath imports and
  `?worker` (3); CSS in a package (1).
- **HTML** (html, 3): `%VITE_NUMBER%` from env, the raw HTML served, invalidating an inline
  proxy module on reload.
- **One each**: the injected `@vite/client` script (assets; design record 0010), glob and
  dynamic-import-vars output visible to post plugins (glob-import), style order of JS-inserted
  tags (css-codesplit), the proxy error page (proxy-bypass), `define` string (flaky: it passed
  when define was rerun alone, 6 of 6).
- **Transform** (owned by ts_r_transform, not ts-r-dev): per-tsconfig `compilerOptions`
  (tsconfig-json, 1).
