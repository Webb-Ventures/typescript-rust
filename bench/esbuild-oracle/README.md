# esbuild oracle for ts-r-build

`run.mjs` treats esbuild's end-to-end bundler tests as data: it reads the Go test files in
`target/upstream/esbuild/internal/bundler_tests/` (esbuild v0.28.2, commit
`609683d892977362a0f99026cb74b96263d728a9`; fetched by `cargo xtask upstream sync`), and for
every case whose options map to CLI flags it:

1. writes the case's files to a temporary directory,
2. builds it with esbuild 0.28.2 (`bench/node_modules/.bin/esbuild`, pinned in
   `bench/package.json`) and with `ts-r-build`, using the same flags,
3. runs every entry output under Node and compares build success, stdout and the thrown
   error (content hashes in file names and numeric suffixes of generated names are
   normalized, because they are implementation-defined).

A case passes when both tools fail to build, or both build and the outputs behave the same.
We never read esbuild's Go implementation; only the test cases are used.

```sh
cargo build --release -p ts_r_build -j 4
(cd bench && ../scripts/limit.sh -- npm ci)
scripts/limit.sh --mem-mb 4096 --time-s 1800 -- node bench/esbuild-oracle/run.mjs --jobs 4
node bench/esbuild-oracle/fails.mjs [REGEX]   # list failures from target/esbuild-oracle/results.json
```

Cases are skipped when they need options the CLI cannot express or that ts-r-build does not
implement (syntax lowering via `UnsupportedJSFeatures`, `MangleProps`, inject, plugins, CSS
bundling, custom JSX factories through Go structs, ...). `results.json` lists every skip
reason; cases that print `Math.random()` results are skipped as nondeterministic.

## Results

M3 plus `m1/transform-gaps` (macOS arm64, esbuild 0.28.2, `--jobs 4`): **651 of 746 compared
cases pass (87.3%)**; 324 of the 1070 cases are skipped. The esbuild test files come from
`raw.githubusercontent.com/evanw/esbuild/<commit>/internal/bundler_tests/` (data only).

| Test file | Pass | Fail | Skip |
| --- | ---: | ---: | ---: |
| bundler_css_test.go | 1 | 4 | 57 |
| bundler_dce_test.go | 104 | 7 | 10 |
| bundler_default_test.go | 185 | 22 | 96 |
| bundler_glob_test.go | 0 | 1 | 9 |
| bundler_importphase_test.go | 0 | 12 | 0 |
| bundler_importstar_test.go | 77 | 3 | 0 |
| bundler_importstar_ts_test.go | 23 | 0 | 0 |
| bundler_loader_test.go | 34 | 14 | 37 |
| bundler_lower_test.go | 13 | 1 | 70 |
| bundler_packagejson_test.go | 89 | 6 | 1 |
| bundler_splitting_test.go | 20 | 2 | 1 |
| bundler_ts_test.go | 50 | 12 | 26 |
| bundler_tsconfig_test.go | 55 | 9 | 16 |
| bundler_yarnpnp_test.go | 0 | 2 | 1 |

Remaining failures by cause:

- **Not implemented**: `import defer` / `import source` phases (12), glob imports
  (`require('./src/' + x)`), CSS bundling details (`@import` inlining, CSS modules from JS),
  `data:` URL imports of JavaScript and JSON, `with { type: ... }` loader overrides, Yarn PnP,
  `--line-limit`.
- **Missing diagnostics** (esbuild fails the build, we don't): assigning to imports and
  constants, `with` in strict mode, `delete` of a bare identifier in ESM output, export cycles
  (`export {a as b}` / `export {b as a}`), `require()` of a module with top-level await
  through a chain, strict JSON for `package.json` (comments, trailing commas) and BigInt in
  JSON.
- **TypeScript, where esbuild and tsc 7 differ** (ts-r follows tsc): `@new x()` / `@x?.[y]`
  decorators (tsc's TS1497), `'' + null` in const enums, computed keys of fields without an
  initializer under `useDefineForClassFields: false` (tsc never evaluates them), a package
  `tsconfig` field naming a missing file (tsc falls back to the package's `tsconfig.json`).
- **TypeScript, needs the bundler**: cross-file const enum inlining (`TSConstEnumComments`,
  `TSMinifyEnumPropertyNames`: the bundler must hand imported enum values to the transform),
  missing re-exports from TS files (`TSExportMissingES6`), `jsx: preserve` in tsconfig ignored
  when bundling (`TsconfigPreserveJSX`).
- **Interop details**: namespace objects of empty or side-effect-free files, CommonJS files
  that also use ESM syntax, the dual-package rule with `module` before `main`, `require.main`
  and `require.cache`, `__proto__` keys in JSON, `AvoidTDZ` class renaming.
- **Shapes we print differently**: JSON namespace `default` is a value rather than a getter;
  `--public-path` with splitting; asset paths for one file reached from two output
  directories; `?query` suffixes on `file` loader URLs.

| Run | Cases | Compared | Pass | Fail | Skip | Pass rate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| M3, tree shaking commit | 1070 | 757 | 617 | 140 | 313 | 81.5% |
| M3, final | 1070 | 746 | 643 | 103 | 324 | 86.2% |
| transform gaps (`export =` under preserve, import/local collisions, method parameter decorators, enums merged across namespace blocks, JSX factory through namespace exports, `extends: ".."`, package `tsconfig` directory) | 1070 | 746 | 651 | 95 | 324 | 87.3% |
