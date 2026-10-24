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
Only the test cases are used; nothing from esbuild's implementation runs in ts-r-build.

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

cb637a3 (2026-10-24, macOS arm64, esbuild 0.28.2, `--jobs 4`): **742 of 779 compared cases
pass (95.3%)**; 291 of the 1070 cases are skipped. The esbuild test files come from
`raw.githubusercontent.com/evanw/esbuild/<commit>/internal/bundler_tests/` (data only).
33 more cases are compared than at 52b87b3: the runner now maps JSX options, entry, chunk and
asset path templates, `OutputExtensionJS`, `TSConfigPath`, and `UnsupportedJSFeatures` sets
that equal a year target of ES2022 or later (`--target=esN`). Sets older than ES2022 are
skipped as `UnsupportedJSFeatures=es(<2022)`.

| Test file | Pass | Fail | Skip |
| --- | ---: | ---: | ---: |
| bundler_css_test.go | 1 | 5 | 56 |
| bundler_dce_test.go | 108 | 3 | 10 |
| bundler_default_test.go | 213 | 9 | 81 |
| bundler_glob_test.go | 1 | 0 | 9 |
| bundler_importphase_test.go | 12 | 0 | 0 |
| bundler_importstar_test.go | 80 | 0 | 0 |
| bundler_importstar_ts_test.go | 23 | 0 | 0 |
| bundler_loader_test.go | 47 | 8 | 30 |
| bundler_lower_test.go | 16 | 0 | 68 |
| bundler_packagejson_test.go | 93 | 2 | 1 |
| bundler_splitting_test.go | 21 | 2 | 0 |
| bundler_ts_test.go | 59 | 3 | 26 |
| bundler_tsconfig_test.go | 68 | 3 | 9 |
| bundler_yarnpnp_test.go | 0 | 2 | 1 |

Remaining failures by cause:

- **Not implemented**: CSS modules imported from JS and CSS-from-JS missing-import errors
  (4), `[hash]` content hashing of a CSS bundle (`MetafileCSSBundleTwoToOne`), `data:` URL
  imports of JavaScript and JSON (4), `?query` suffixes as separate `file` loader modules,
  asset `[dir]` relative to the output base, one asset reached from two output directories,
  Yarn PnP (2), `--public-path` with splitting and a minified-identifiers splitting case.
- **Top-level await in lazily initialized modules** (4): `require()` of a module with
  top-level await through a chain, `import()` of one without code splitting and a cycle of
  them (esbuild makes the module's wrapper async). `TopLevelAwaitAllowedImportWithSplitting`
  differs because esbuild's own output throws (`require_entry is not defined`).
- **Interop and DCE details**: TDZ and class renaming (`CommonJSFromES6`, `AvoidTDZ`),
  `this` in ESM-looking files, namespace shapes of files that are neither ESM nor CommonJS,
  CommonJS `exports` warnings in an ESM bundle, pure calls with spread arguments,
  `@__NO_SIDE_EFFECTS__` on unused calls, dead `case` bodies that import a missing file.
- **TypeScript, where esbuild and tsc 7 differ** (ts-r follows tsc): `@new x()` / `@x?.[y]`
  decorators (tsc's TS1497, `TSExperimentalDecorators`), `'' + null` in const enums
  (`EnumRulesFrom_TypeScript_5_0`), computed keys of fields without an initializer under
  `useDefineForClassFields: false` (tsc never evaluates them), a package `tsconfig` field
  naming a missing file (tsc falls back to the package's `tsconfig.json`).
- **Oracle artifacts**: `TsconfigExtendsDotWithSlash` / `DotDotWithSlash` expect a warning
  where the real esbuild CLI fails ("is a directory"); ts-r matches the test's intent.
  `PackageJsonBrowserIssue2002B` depends on an esbuild quirk (a bare `sub` matched against a
  browser-map key relative to the importer), and `PackageJsonExportsAlternatives` fails in
  esbuild itself.

| Run | Cases | Compared | Pass | Fail | Skip | Pass rate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| M3, tree shaking commit | 1070 | 757 | 617 | 140 | 313 | 81.5% |
| M3, final | 1070 | 746 | 643 | 103 | 324 | 86.2% |
| transform gaps (`export =` under preserve, import/local collisions, method parameter decorators, enums merged across namespace blocks, JSX factory through namespace exports, `extends: ".."`, package `tsconfig` directory) | 1070 | 746 | 651 | 95 | 324 | 87.3% |
| main at 9f23c9d (parser grammar errors fail six inputs) | 1070 | 746 | 646 | 100 | 324 | 86.6% |
| build parity: only esbuild's errors fail a build (`ts_r_transform::build_errors`), `.js` before `.ts` inside `node_modules`, dead `catch` of an empty `try` | 1070 | 746 | 655 | 91 | 324 | 87.8% |
| strict-mode errors (`with`, `delete x` in strict code, ESM files and ESM output; tsconfig `alwaysStrict` only when set), assignments to constants and imports when bundling, top-level `return` with top-level `await` | 1070 | 746 | 667 | 79 | 324 | 89.4% |
| parallel output writes: `DuplicateEntryPoint` (one entry point given twice) now fails with "Two output files share the same path"; esbuild writes it once | 1070 | 746 | 666 | 80 | 324 | 89.3% |
| parity/bundler at cb637a3: import phases and attributes, glob entries, strict package.json, re-export cycle errors, JSON namespaces, imported scripts as CommonJS, external evaluation order, exports targets naming compiled TypeScript, exported const enums kept; runner maps JSX, path templates and modern `UnsupportedJSFeatures` | 1070 | 779 | 742 | 37 | 291 | 95.3% |
