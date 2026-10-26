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
implement (Go plugins, browser targets, custom JSX factories through Go structs, ...). `results.json` lists every skip reason; cases that print `Math.random()`
results are skipped as nondeterministic. `UnsupportedJSFeatures` maps to `--target=esN` when
the set equals a year target and to `--supported:<feature>=false` flags otherwise.

## Results

v1 build6 at c0449fb (2026-10-27, macOS arm64, esbuild 0.28.2, `--jobs 4`):
**1,002 of 1,028 compared cases pass (97.5%)**; 42 of the 1070 cases are skipped. Of the 26 failures, 14 are
intended differences where ts-r-build follows tsc 7 (listed below) and 8 are oracle
artifacts; the other 4 are gaps. The esbuild test files come from
`raw.githubusercontent.com/evanw/esbuild/<commit>/internal/bundler_tests/` (data only).
Since af89821 (938 of 964): ts-r-build has `--mangle-props`, `--mangle-quoted`,
`--reserve-props`, `--inject`, path externals matched after resolution, CSS nesting
lowering (`--supported:nesting=false`), `calc()` folding and short numbers under
`--minify-syntax`, and more CSS module names (`@container`, counter styles, string keyframe
names); `define` applies inside enum initializers and to JSX factories. The runner maps
`MangleProps`, `MangleQuoted`, `ReserveProps`, `InjectPaths`, `External.PostResolve`,
`OriginalTargetEnv` and `UnsupportedCSSFeatures` (nesting), so 64 more cases are compared.

| Test file | Pass | Fail | Skip |
| --- | ---: | ---: | ---: |
| bundler_css_test.go | 55 | 4 | 3 |
| bundler_dce_test.go | 117 | 0 | 4 |
| bundler_default_test.go | 293 | 1 | 9 |
| bundler_glob_test.go | 1 | 0 | 9 |
| bundler_importphase_test.go | 12 | 0 | 0 |
| bundler_importstar_test.go | 80 | 0 | 0 |
| bundler_importstar_ts_test.go | 23 | 0 | 0 |
| bundler_loader_test.go | 80 | 0 | 5 |
| bundler_lower_test.go | 73 | 11 | 0 |
| bundler_packagejson_test.go | 93 | 2 | 1 |
| bundler_splitting_test.go | 22 | 1 | 0 |
| bundler_ts_test.go | 76 | 3 | 9 |
| bundler_tsconfig_test.go | 75 | 4 | 1 |
| bundler_yarnpnp_test.go | 2 | 0 | 1 |

### Intended differences (14): ts-r-build follows tsc 7

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
- **`importsNotUsedAsValues: "preserve"`** (`TsconfigPreserveUnusedImports`): tsc 7 no
  longer has the option, so ts-r-build drops an import whose names are only used as types;
  esbuild keeps it as `import "./foo"`.

### Oracle artifacts (8)

- `TsconfigExtendsDotWithSlash` / `DotDotWithSlash` expect a warning where the real esbuild
  CLI fails ("is a directory"); ts-r matches the test's intent.
- `PackageJsonBrowserIssue2002B` depends on an esbuild quirk (a bare `sub` matched against a
  browser-map key relative to the importer), and `PackageJsonExportsAlternatives` fails in
  esbuild itself.
- `TopLevelAwaitAllowedImportWithSplitting`: esbuild's own output throws
  (`require_entry is not defined`).
- `LowerStaticSuperES2021NoBundle` and `LowerStaticAsyncSuperES2021NoBundle`: esbuild's output
  is a SyntaxError (`'super' keyword unexpected here`; in the async case it leaves
  `await super.foo?.(1, 2)` in an arrow moved out of the class); ts-r's runs as tsc 7's does
  and throws a ReferenceError, so the thrown errors differ. These count as one artifact each.
- `SplittingMinifyIdentifiersCrashIssue437`: the output differs only in which short name the
  minifier picks (`o` vs `f`), which shows in a thrown error.

### Gaps (4)

- **CSS** (4): with `--minify-identifiers`, esbuild names local CSS classes by how often
  they are used, ts-r-build in source order (`ImportLocalCSSFromJSMinifyIdentifiers`,
  `...AvoidGlobalNames`, `...MultipleEntryPoints`); esbuild escapes non-ASCII characters in
  CSS strings (`"\1f44d"`, its default ASCII charset) and ts-r-build keeps them
  (`ImportCSSFromJSLocalAtCounterStyle`).

### Skipped (42)

Nondeterministic (12), Go-only TypeScript options (10), cases sharing a Go variable (6), no
output path (4, one of them writes to stdout), Windows paths (2), and one each for a browser
target with CSS features (`CSSNestingOldBrowser`), `UnsupportedCSSFeatures` other than
nesting (`InlineStyle`), `Defines` with call flags (`--pure`), a non-string file, the two JSX
options `Fragment` and `SideEffects`, absolute node paths and `Plugins` (a Go plugin).

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
| v1 build parity at e5b1e2c: DCE of dead `switch` cases and `@__NO_SIDE_EFFECTS__`, async lazy modules, CommonJS imports in order, top-level `this`, `let`/`const`/`class` to `var`, unwrapped CommonJS entries, `define` on `import.meta` / optional chains; runner maps `Defines` | 1070 | 881 | 851 | 30 | 189 | 96.6% |
| v1 emit parity at 9293c3c: async `super` below ES2017 (optional `super` calls, `super` in nested class keys) | 1070 | 881 | 852 | 29 | 189 | 96.7% |
| v1 resolve parity at 45ab2ad: tsconfig `extends` of a package through Yarn PnP's `.pnp.data.json` | 1070 | 881 | 854 | 27 | 189 | 96.9% |
| v1 build3 at 8a3356f: `--alias`, `--drop-labels`; runner maps `PackageAliases` and `DropLabels` | 1070 | 885 | 858 | 27 | 185 | 96.9% |
| v1 build4 at 955b2a9: CSS bundling and CSS modules, `copy` / `local-css` / `global-css` loaders; runner compares CSS and maps those loaders | 1070 | 957 | 931 | 26 | 113 | 97.3% |
| v1 build5 at af89821: `--keep-names`; runner maps `KeepNames` | 1070 | 964 | 938 | 26 | 106 | 97.3% |
| v1 build6 at c0449fb: `--mangle-props` / `--mangle-quoted` / `--reserve-props`, `--inject`, path externals, CSS nesting lowering, `calc()` folding, more CSS module names, `define` in enums and JSX factories; runner maps those options | 1070 | 1028 | 1002 | 26 | 42 | 97.5% |
