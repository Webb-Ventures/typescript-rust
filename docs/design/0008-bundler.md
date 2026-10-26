# 0008: Bundler, minifier and ts-r-build

Status: Proposed (drafted for M3; awaiting review)
Owners: `ts_r_bundler`, `ts_r_minify`, `ts_r_build`; consumers: `ts_r_hmr` / `ts-r-dev`
(dependency pre-bundling through the `DepBundler` seam)

## Context

M3 ships `ts-r-build`, a replacement for esbuild (and for Rollup in Vite builds) in our own
projects. Its behaviour is specified by **esbuild 0.28.2**: its documented options, its
end-to-end bundler tests (`internal/bundler_tests/*.go` at commit
`609683d892977362a0f99026cb74b96263d728a9`, used as data only) and its observed output on real
projects (excalidraw and three.js, pinned in `bench/repos.toml`). The source of esbuild, Rollup, Rolldown,
webpack, terser, swc or oxc may be read to understand a rule, never ported.

We match esbuild's **semantics** (what the bundle does when run, which modules and exports
survive, which errors are reported), not its exact bytes. Our runtime helpers, generated names
and formatting are our own. Syntax newer than `--target` / `--supported` is lowered by the
same per-year down-level passes as tsc emit (`ts_r_transform`), ES2015 at the lowest; ES2015
syntax that an ES5 target forbids is an error, as in esbuild (`features.rs`, `lowering.rs`).

## Decision

### Pipeline

1. **Resolve** (`resolve.rs`). Relative and absolute paths with extension probing
   (`resolveExtensions`, `.js` → `.ts` rewriting), tsconfig `paths`/`baseUrl` through
   `ts_r_resolver`, `node_modules` lookup with `exports`/`imports` conditions
   (`import`/`require`, `browser`/`node`, `default`, plus `--conditions`), main fields
   (`browser,module,main` on the browser, `main,module` on node), the `browser` field map
   (including `false` → empty module), `sideEffects`, realpath for symlinked packages, node
   built-ins external on `--platform=node`, `--external` patterns with `*`, and
   `--packages=external`. Results are cached per `(spec, dir, kind)` until a structural change.
2. **Prepare** (`prepare.rs`). Load with the loader for the extension (`js jsx ts tsx json
   text file copy css local-css global-css empty base64 dataurl binary`; other non-JS
   loaders become small CommonJS modules, stylesheets are parsed by `css/` into a `Sheet`,
   and imports of a `copy` file stay imports, of its copy),
   parse, run `ts_r_transform` with module format `Preserve` (types, enums, decorators, JSX),
   apply `--define` (plus esbuild's automatic `process.env.NODE_ENV` for browser bundles), fold
   constants (`ts_r_minify::fold`, so dead `NODE_ENV` branches never reach the import scan),
   and analyse scopes. Binary loaders use `FileSystem::read_bytes`: the real file system
   returns original bytes without text decoding; the default implementation supports
   text-backed file systems by encoding their text as UTF-8. JSON loading retains original
   top-level object keys for export discovery before rewriting `__proto__` as a computed
   JavaScript property. JSON namespaces always expose the complete value through a default
   getter; a `type: 'json'` import attribute limits the namespace to that default export,
   even if the value has an `__esModule` property.
3. **Scan** (`scan.rs`) into `ModuleMeta` (`meta.rs`): import records (static, re-export,
   `require`, `import()`; with "inside try" flags), top-level *parts* (one per statement with
   the symbols it declares and uses, namespace member uses and side-effect flag), imports,
   exports, star exports, and flags (ESM syntax, CommonJS `module`/`exports` use, top-level
   await, `import.meta`, direct `eval`). `ModuleMeta` has a binary codec so it can be cached.
4. **Graph** (`graph.rs`). Modules load in parallel on a std-thread pool (at most
   `TS_R_THREADS`), then get ids in **DFS pre-order** from the entries, which is the
   evaluation order and makes everything downstream deterministic.
5. **Link** (`link.rs`). Each module is `Esm` or `Cjs` (a stylesheet is an `Esm` stub whose
   exports are its local names). A module is CommonJS if it has
   no ESM exports and uses `module`/`exports`, or is `require`d, or is a non-JS loader. CJS
   modules are wrapped (`__tsrCjs`); ESM modules reached by `require()` or a non-splitting
   `import()` are lazily wrapped (`__tsrEsm`), which propagates to their static ESM
   dependencies. Imports are bound to a symbol in another module, a property of a CJS
   module's exports, a namespace object, an external import or `undefined` (with esbuild's
   missing-export errors, silenced for TypeScript files where the name may be a type). Tree
   shaking is a worklist over parts: entry parts and parts with side effects (respecting
   `sideEffects` and `/*#__PURE__*/`) are live, and a live part makes the declarations of its
   used symbols live. Lazily wrapped ES modules are shaken the same way; only modules that
   are `require()`d or imported as a namespace keep every export (for their namespace object).
6. **Plan** (`plan.rs`). Allocates bundle-wide names (module-scope symbols keep their names
   unless they collide; then `name2`, `name3`; minified builds use short names), builds
   chunks, and writes one `ModulePlan` per included module: final names, import rewrites,
   wrapper, live parts, record replacements and printing options.
7. **Print** (`print.rs`). Each module is printed on its own from its prepared tree: imports
   are replaced by the bound names, `require`/`import()` records by wrapper calls or chunk
   imports, nested scopes are deconflicted (or mangled; a CommonJS wrapper's `exports` and
   `module` parameters are mangled with the module's own locals), import/export syntax is removed,
   and with `--minify-syntax` the module is compressed. The result is a `Fragment` (code plus
   a single-source map).
8. **Assemble** (`chunk.rs`). Chunk = hashbang, banner, format wrapper, external imports,
   cross-chunk imports, runtime helpers, transform preludes, namespace objects, fragments in
   order, entry exports, footer. Fragment maps are concatenated with line offsets
   (`sourcemap.rs`); they already point at the original `.ts` text.

### Interop rules

- `import x from "cjs"` gives `module.exports` when the importer is in node mode (`.mjs`,
  `.mts`, or a `"type": "module"` package), otherwise `exports.default` when
  `__esModule` is set, else `module.exports`. Named imports read properties of
  `module.exports` at use time.
- `require("esm")` returns a cached namespace object with `__esModule: true`.
- Entry exports: ESM output uses `export { .. }`; CJS output assigns
  `module.exports = __tsrToCjs(ns)`; IIFE with `--global-name` returns the namespace.

### Code splitting

ESM only. Each user entry and each `import()` target is an entry point. A module goes to the
chunk keyed by the set of entries that reach it; entry chunks hold modules reached only from
them, and shared chunks are named `chunk-<HASH>.js` where the hash covers the member modules'
`prep_key`s. Chunks import the names they need from each other; a chunk also imports (for
side effects) every chunk whose entry set is a strict superset of its own, so shared code
runs first. `import()` of a split entry becomes `import("./entry.js")`.

### Minifier

`--minify-whitespace` is the compact codegen layout. `--minify-identifiers` mangles nested
scopes per module (most-used bindings get the shortest names) and gives top-level symbols
short bundle-wide names. `--minify-syntax` runs `compress` (statement merging, `if` to
`&&`/`?:`, boolean and `undefined` shortening, unreachable code removal) after folding.
`ts_r_minify` is independent of the bundler and also runs on single files.

### Incremental builds and caching

- In memory (`BuildContext`): prepared modules keyed by `prep_key` = blake3 of file contents,
  loader, transform options cache key, defines, annotation handling and effective output
  format (including strict-mode validation); printed fragments keyed by
  `blake3(prep_key, ModulePlan)`. A rebuild re-reads changed files, re-links (cheap), and
  reprints only modules whose plan changed. Fragments not used by the latest build are
  evicted.
- On disk (`ts_r_cache`, namespace `build`, versioned): encoded `ModuleMeta` per `prep_key`
  (so a cold process skips the scan for unchanged files, but still parses when a fragment
  must be printed) and encoded fragments per fragment key. No absolute paths or times are
  stored.
- File system: `CachedFs` lists each directory once and answers existence and real-path
  queries from the listings (on macOS this replaced thousands of `open`/`getattrlist` calls
  per build). `BuildContext::invalidate(changed)` forgets the listings of the changed files'
  directories and remembers the set; the next build reuses every other cached module
  without reading it. Added or removed files, `package.json` and `tsconfig.json` changes
  are structural: resolution and listings start over and every file is re-hashed.

### CLI (`ts-r-build`)

Entry globs skip directories the pattern cannot match and fail with a diagnostic when a
traversal size or depth limit prevents complete expansion. Entry deduplication uses resolved
path and output name, preserving the order of the first occurrence.

esbuild flag syntax for: `--bundle --outdir --outfile --format --platform --target --minify
--minify-whitespace --minify-identifiers --minify-syntax --sourcemap[=linked|inline|external|both]
--sources-content --splitting --external:X --packages=external --define:K=V --loader:.ext=L
--metafile=F --global-name --main-fields --conditions --resolve-extensions --tsconfig --jsx
--jsx-factory --jsx-fragment --jsx-import-source --entry-names --chunk-names --asset-names
--public-path --banner:js --footer:js --out-extension:.js --tree-shaking --ignore-annotations
--log-level --watch`. There is no JavaScript config file and no plugin API.

### Dev-server seam

With feature `hmr`, `ts_r_bundler::hmr::DepBundle` implements `ts_r_hmr::DepBundler`.
`ts_r_hmr` itself is unchanged; `ts-r-dev` opts in with
`config.dep_bundler = Arc::new(ts_r_bundler::hmr::DepBundle::new())`. Per request:

- The dependency entry plus everything it reaches through relative imports becomes one ESM
  module. Every bare specifier stays external (`packages_external`), so each package is
  loaded once and the server's normal import rewriting points it at `/@modules/...`.
- `require("peer")` inside a dependency becomes a hoisted `import * as ns from "peer"` and
  `__tsrToCjs(ns)` (`BuildOptions::require_to_import`, ESM output only).
- A CommonJS entry gets a generated ESM facade: `default` (with `__esModule` interop), one
  named export per statically detected name (`cjs_exports.rs`: `exports.x =`,
  `Object.defineProperty(exports, "x")`, `module.exports = {..}`, following
  `module.exports = require("./x")` chains like React's `index.js`), and
  `"module.exports"`, the raw exports object. `__tsrToCjs` returns a namespace's
  `"module.exports"` export when present (Node's `require(esm)` rule), so peers that
  `require("react")` get the same object as importers of `react`.
- `process.env.NODE_ENV` is `"development"` and the `development` condition is on.

`tests/dep_bundle.rs` links React-shaped CommonJS fixtures and an ESM package the way the
server serves them and runs them under Node; with `TS_R_REACT_ROOT` set to a project with
React installed it renders React 19 through `react-dom/server.browser` and loads
`react-dom/client`. The per-specifier call bundles two entry points of one package that
share internal files with a copy each; the batch call below avoids that.

Batch mode (`DepBundle::batch`, the seam's `bundle_batch`): `ts_r_hmr` scans the app's
dependencies from `index.html` up front and asks for all of them at once. One build with
`splitting` and packages bundled in produces one output per entry (named after the
specifier) plus shared `chunk-<hash>.js` files, so every module of every package exists
once. `ts-r-dev` serves these under `/@ts-r/deps/` and caches the batch (see
`ts_r_hmr`'s AGENTS.md). `tests/dep_bundle.rs` checks sharing, determinism and that the
linked batch runs under Node.

## Testing

- `cargo test -p ts_r_bundler`: resolver, codec and source-map unit tests; `tests/bundle.rs`
  bundles in-memory projects in every format and runs them under Node via `scripts/limit.sh`.
- `cargo test -p ts_r_minify`: per-pass tests, and `tests/semantics.rs`, which minifies
  tricky programs (shadowing, `eval`, `with`, labels, TDZ, Annex B catch redeclarations,
  ASI) and compares their Node output with the originals'.
- esbuild oracle: `scripts/limit.sh -- node bench/esbuild-oracle/run.mjs` ports the
  end-to-end cases of esbuild's bundler tests (as data) to temporary projects, builds them
  with `ts-r-build`, runs the outputs under Node where the case is runnable, and checks
  expected errors. Results are in `bench/esbuild-oracle/README.md`.
- Real app: excalidraw (pinned in `bench/repos.toml`; see `bench/README.md`). `cargo xtask
  bench --repos excalidraw` runs `build-excalidraw` (one process, minified ESM, with and
  without the persistent cache, against esbuild 0.28.2 pinned in `bench/package.json`) and
  `rebuild-excalidraw` (first build plus 20 edit-and-rebuild cycles in one process:
  `examples/build_loop.rs --edit` vs `bench/js/esbuild-rebuild.mjs`).
- three.js x10: `build-three-x10`, ten copies of three.js bundled at once (the benchmark
  esbuild and Bun publish).

### Results (M3, Apple M-series, 4 threads)

- esbuild oracle: 938 of 964 compared cases pass (97.3%), 13 of the 26 failures intended
  differences where ts-r-build follows tsc 7; see the oracle README.
- A private app's server-rendered smoke entry (1983 modules; historical, no longer run):
  ts-r-build 65 ms without and 55 ms with the persistent cache, esbuild 86 ms; +2.7% size.

### Results on public repos (2026-10-16, Apple M-series shared laptop, 4 threads)

- excalidraw, minified ESM (`build-excalidraw`, median of 10): ts-r-build 254.8 ms without
  and 131.9 ms with the persistent cache, esbuild 200.9 ms. ts-r-build does not bundle
  template-literal `import()`s yet, so it leaves out the 61 locale chunks esbuild emits (7.2
  MB against 9.3 MB of output): the cold comparison flatters ts-r-build, and it is still
  slower than esbuild there.
- excalidraw, first build plus 20 rebuilds (`rebuild-excalidraw`, median of 5): ts-r-build
  1,357.5 ms vs esbuild 3,457.1 ms.
- three.js x10 (`build-three-x10`, median of 10): ts-r-build 377.7 ms without and 240.5 ms
  with the persistent cache, esbuild 224.7 ms; 7.88 MB vs 7.71 MB of output.

## Known gaps

- CSS is bundled (`@import` order, duplicates, `@layer`/`@supports`/`@media` conditions,
  `url()` assets, `local-css`/`global-css` and `composes`), but without esbuild's syntax
  lowering and minification rewrites beyond dropping repeated rules, and with our own
  minified local names. No plugins, no `--watch` serve mode, no legal-comment extraction.
- No lowering below ES2015: `const`, classes, destructuring and the like are errors at
  `--target=es5`, as in esbuild; arrows and template literals are kept.
- IIFE output is not re-indented; runtime helpers are repeated per chunk.
- With several entries and no splitting, tree-shaking liveness is shared across entries.
- `var` declarations nested in blocks of a lazily wrapped ESM module stay inside its closure.
- No `--mangle-props`, no `--drop`, no `--pure`.
- Not implemented: `import defer`/`import source`, glob `require`/`import()` of template
  strings, Yarn PnP module resolution (only a tsconfig `extends` goes through PnP). Missing
  diagnostics: assignments to imports and constants, export cycles, strict `package.json`
  JSON. The oracle README lists the remaining failures by cause.
- Minified output is 2.9% smaller than esbuild's in total over the top npm packages (median
  per-package ratio 0.993, `ecosystem/STATUS.md`), but larger on 277 of 914 packages: no
  cross-module constant inlining, no `--minify-syntax` rewrites of classes and templates yet.

## Alternatives considered

- **One shared AST for the whole bundle** (print once): simpler renaming, but no per-module
  fragment caching, so rebuilds would reprint everything. Rejected in favour of fragments.
- **Lowering CommonJS to ESM** instead of wrapping: smaller output but changes evaluation
  order and breaks circular `require`s; esbuild's observed behaviour is to wrap.
