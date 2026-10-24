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
implement (`MangleProps`, inject, plugins, CSS bundling, custom JSX factories through Go
structs, ...). `results.json` lists every skip reason; cases that print `Math.random()`
results are skipped as nondeterministic. `UnsupportedJSFeatures` maps to `--target=esN` when
the set equals a year target and to `--supported:<feature>=false` flags otherwise.

## Results

b2d0178 plus 07d2cdd (2026-10-24, macOS arm64, esbuild 0.28.2, `--jobs 4`): **832 of 872
compared cases pass (95.4%)**; 198 of the 1070 cases are skipped. Of the 40 failures, 13 are
intended differences where ts-r-build follows tsc 7 (listed below) and 7 are oracle
artifacts; the other 20 are gaps. The esbuild test files come from
`raw.githubusercontent.com/evanw/esbuild/<commit>/internal/bundler_tests/` (data only).
93 more cases are compared than at cb637a3: ts-r-build now takes `--target=es5..esnext` and
`--supported:<feature>=bool` (any `UnsupportedJSFeatures` set maps), bundles `data:` URL
modules, keeps `?query` / `#hash` suffixes as separate modules, writes `file` loader URLs per
chunk, and applies `--public-path` to chunk imports.

| Test file | Pass | Fail | Skip |
| --- | ---: | ---: | ---: |
| bundler_css_test.go | 1 | 5 | 56 |
| bundler_dce_test.go | 110 | 3 | 8 |
| bundler_default_test.go | 227 | 9 | 67 |
| bundler_glob_test.go | 1 | 0 | 9 |
| bundler_importphase_test.go | 12 | 0 | 0 |
| bundler_importstar_test.go | 80 | 0 | 0 |
| bundler_importstar_ts_test.go | 23 | 0 | 0 |
| bundler_loader_test.go | 57 | 0 | 28 |
| bundler_lower_test.go | 72 | 12 | 0 |
| bundler_packagejson_test.go | 93 | 2 | 1 |
| bundler_splitting_test.go | 22 | 1 | 0 |
| bundler_ts_test.go | 66 | 3 | 19 |
| bundler_tsconfig_test.go | 68 | 3 | 9 |
| bundler_yarnpnp_test.go | 0 | 2 | 1 |

### Intended differences (13): ts-r-build follows tsc 7

ts-r-build lowers TypeScript and JavaScript with the same transform as `ts-r-check` emit, so
where tsc 7 and esbuild disagree it gives tsc's result. These cases are not changed.

- **Private names in optional chains below ES2022** (5: `LowerPrivateFieldOptionalChain2019NoBundle`,
  `LowerPrivateFieldOptionalChain2020NoBundle`, `TSLowerPrivateFieldOptionalChain2015NoBundle`,
  `LowerPrivateMethod2019`, `LowerPrivateMethod2020`): tsc 7 reports TS18030 ("An optional
  chain cannot contain private identifiers") when it would have to lower `a?.#x`, so the
  build fails; esbuild lowers it.
- **`super` in a private method below ES2022** (`LowerPrivateSuperES2021`) and **`super` in
  a static block of a class without `extends`** (`StaticClassBlockES2021`): tsc 7's lowering
  moves the body out of the class and leaves `super` where it is no longer valid, so
  ts-r-build reports "Transforming ... is not supported yet" instead of emitting broken code;
  esbuild rewrites `super`.
- **An async arrow before `super()`** (`LowerAsync2016NoBundle`): tsc captures `this` for
  the lowered arrow at its definition, so calling the constructor throws "Must call super
  constructor" exactly as tsc 7's own output does; esbuild defers the capture.
- **A class named `WeakMap` with private members** (`TSLowerPrivateFieldAndMethodAvoidNameCollision2015`):
  tsc's lowering refers to the global `WeakMap`, which the class shadows, so the output
  throws as tsc 7's does; esbuild renames the class.
- **TypeScript rules** (4): `@new x()` / `@x?.[y]` decorators are tsc's TS1497
  (`TSExperimentalDecorators`); `'' + null` in a const enum is not a constant in tsc
  (`EnumRulesFrom_TypeScript_5_0`); computed keys of fields without an initializer under
  `useDefineForClassFields: false` are never evaluated by tsc
  (`TSComputedClassFieldUseDefineFalse`); a package `tsconfig` field naming a missing file
  falls back to the package's `tsconfig.json`, as tsc resolves it
  (`TsconfigJsonNodeModulesTsconfigPathBad`).

### Oracle artifacts (7)

- `TsconfigExtendsDotWithSlash` / `DotDotWithSlash` expect a warning where the real esbuild
  CLI fails ("is a directory"); ts-r matches the test's intent.
- `PackageJsonBrowserIssue2002B` depends on an esbuild quirk (a bare `sub` matched against a
  browser-map key relative to the importer), and `PackageJsonExportsAlternatives` fails in
  esbuild itself.
- `TopLevelAwaitAllowedImportWithSplitting`: esbuild's own output throws
  (`require_entry is not defined`).
- `LowerStaticSuperES2021NoBundle`: esbuild's output is a SyntaxError (`'super' keyword
  unexpected here`); ts-r's throws a ReferenceError, so the thrown errors differ.
- `SplittingMinifyIdentifiersCrashIssue437`: the output differs only in which short name the
  minifier picks (`o` vs `f`), which shows in a thrown error.

### Gaps (20)

- **CSS** (5): CSS modules imported from JS, CSS-from-JS missing-import errors, and the
  `[ext]` placeholder in entry names with CSS bundles (`MetafileCSSBundleTwoToOne`).
- **Top-level await in lazily initialized modules** (3): `require()` of a module with
  top-level await through a chain, `import()` of one without code splitting and a cycle of
  them (esbuild makes the module's wrapper async).
- **Interop and DCE details** (8): TDZ and class renaming (`CommonJSFromES6`, `AvoidTDZ`),
  `this` in ESM-looking files, namespace shapes of files that are neither ESM nor CommonJS,
  CommonJS `exports` warnings in an ESM bundle, pure calls with spread arguments,
  `@__NO_SIDE_EFFECTS__` on unused calls, dead `case` bodies that import a missing file.
- **Lowering** (2): `super` in static async methods at ES2016 (`LowerStaticAsyncSuperES2016NoBundle`)
  and `super[key]?.()` in a lowered async method (`LowerAsyncSuperES2016NoBundle`).
- **Yarn PnP** (2).

### Skipped (198)

CSS bundling entries (44), `Loader=copy` (19), `Loader=localcss` / `globalcss` (19),
`MangleProps` / `MangleQuoted` / `ReserveProps` (35), `InjectPaths` (17), `Defines` (15),
nondeterministic (11), Go-only TypeScript options (10), `External.PostResolve` (8),
`KeepNames` (8), `UnsupportedCSSFeatures` (7), cases sharing a Go variable (6),
`OriginalTargetEnv` (5), no output path (4), and 1-2 each for `DropLabels`, `PackageAliases`,
Windows paths, stdout, JSX options and absolute node paths. `Plugins` (1) needs a Go plugin
and stays skipped.

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
| parity/bundler-2 at 9348931: `--target=es5..esnext`, `--supported:F=bool`; runner maps every `UnsupportedJSFeatures` set | 1070 | 872 | 823 | 49 | 198 | 94.4% |
| parity/bundler-2 at 07d2cdd: `data:` URL modules, `?query` suffixes, per-chunk `file` URLs, `--public-path` for chunks, `import()`-only modules named like chunks | 1070 | 872 | 832 | 40 | 198 | 95.4% |
