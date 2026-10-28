# ts_r_bundler

esbuild-compatible JavaScript/TypeScript bundler: parallel module graph, ESM/CJS interop,
tree shaking, scope hoisting, code splitting, esm/cjs/iife output, loaders, source maps,
metafile, and incremental rebuilds. Driven by `ts-r-build` (crate `ts_r_build`) and the dev
server's dependency pre-bundler (feature `hmr`). Root rules: [../../AGENTS.md](../../AGENTS.md).
Design: [../../docs/design/0008-bundler.md](../../docs/design/0008-bundler.md).

Written from scratch. esbuild's docs, its end-to-end test cases (as data) and its observed output
are the oracles. esbuild's, Rollup's and other bundlers' source may be read to understand a rule,
never translated (root AGENTS.md).

## API in one breath

```rust
let ctx = BuildContext::new(BuildOptions { entry_points, bundle: true, outdir, .. }, Arc::new(RealFs::new()))?;
let r = ctx.build();              // r.outputs, r.messages, r.metafile, r.stats
ctx.invalidate(&changed_paths);   // then build() again: unchanged modules and fragments are reused
ts_r_bundler::build(opts)         // one shot on the real file system
```

## Pipeline (one file each)

`resolve.rs` (paths, packages, exports/imports, browser map, sideEffects) → `prepare.rs`
(load, loaders, parse, transform, define, fold, scopes) → `scan.rs` (records, parts,
imports/exports into `meta.rs`'s serialisable `ModuleMeta`) → `graph.rs` (parallel load,
DFS numbering) → `link.rs` (module kinds, wrappers, bindings, tree shaking) → `plan.rs`
(global names, chunks, per-module `ModulePlan`, cross-chunk imports) → `print.rs` (rewrite,
rename, wrap, compress, print one module to a `Fragment`) → `chunk.rs` (assemble chunks,
`sourcemap.rs` concatenation) → `bundle.rs` (driver, caches, writing), `metafile.rs`.
`runtime.rs` holds our interop helpers; `cjs_exports.rs` detects CommonJS export names;
`hmr.rs` (feature `hmr`) is the dev server's `DepBundler` (per-specifier `bundle` and the
code-split `batch` that `ts-r-dev` uses, with source maps whose `sources` are relative to
the served URL when `DepBundle::serve_root` is set, and CSS outputs passed through; an entry
is ESM when it has module syntax or Node would load it as a module (`.mjs`, `"type": "module"`),
and a dependency's dynamic `import()` of a stylesheet stays external as the served URL of the
original file, through `BuildOptions::dynamic_import_url`, as Vite does; dependency CSS is
read already bundled by `ts_r_hmr::css`, so `BuildOptions::css_prebundled` keeps its
`url()`s, served URLs like `/@ts-r/up/1/...`, as written instead of resolving them);
`css/` is the stylesheet side: `lexer.rs` (CSS Syntax tokens, escapes, quoting), `mod.rs`
(rules, `@import`/`url()`/`composes` records, local names for `local-css`), `print.rs`,
`color.rs` (shortest colors for `--minify-syntax`; the CSS named-color table),
`order.rs` (`@import` expansion, duplicates, layers, one stylesheet per chunk) and
`link.rs` (final local names, `composes` class lists, reference errors);
`props.rs` compiles `--mangle-props` / `--reserve-props` (the names themselves are found and
rewritten by `ts_r_minify::props`); `inject.rs` is `--inject` (exports of the injected files
replace free names, before preparation's scope pass); `data_url.rs` decodes `data:` URL modules; `fscache.rs` caches directory listings and real paths for a build; `expr.rs` is a tiny
expression builder for generated code; `pool.rs` is the std-thread pool.

## Invariants

- **A build fails where esbuild fails.** Parse errors go through
  `ts_r_transform::build_errors`: tsc's checker grammar errors that esbuild accepts
  (`if (x) ;`, `a?.#x`, `declare function f() {}`, `module M {}`, sloppy
  `for (var x = 1 in o)`) are not build errors; ECMAScript early errors (`const x;`) are.
- **esbuild's strict-mode and binding errors** (`prepare.rs`): `with` and `delete x` fail in
  strict code (a `"use strict"` prologue or class body, an ES module, ESM output, or a
  TypeScript file under a tsconfig that sets `alwaysStrict` or `strict`); writes to `const`
  and `using` bindings fail when bundling or minifying syntax, writes to imports when
  bundling; a top-level `return` fails in a file with top-level `await`.
- **Top-level await** (`link.rs` `compute_async`): a file with top-level `await` is an ES
  module. A wrapped ES module whose own body or any statically imported wrapped module
  awaits gets an `async` `init_x()`, which callers `await` and `import()` waits for
  (`__tsrEsm` returns the body's promise; a cycle back into a running wrapper returns at
  once). A `require()` that reaches one is an error naming the file that awaits.
- **Imports run in order** (`plan.rs` `module_order_with_boundaries`): an unwrapped ES
  module's `require_x()` / `init_x()` calls for the imports before an unwrapped module it
  imports are printed in front of that module's code (`Chunk::before`), so a CommonJS
  import runs between the inlined modules as written. They stay in the importer when the
  target is in another chunk or the called module is not yet defined there (a cycle).
- **Top-level `this`** (`prepare.rs`): `undefined` in a file that exports something or uses
  `import.meta`; when bundling, the `exports` object (a CommonJS file) otherwise, even in
  a file with `import` statements, as esbuild does.
- **Top-level `let`, `const` and `class` become `var`** (`print.rs` `class_to_var`) in every
  bundled module, as esbuild prints them: `var C = class {}`, or `var C = class _C {}` with
  the class's references to itself renamed `_C` (the same short name when mangling).
  Decorated classes and nested declarations stay as written; transform mode never does it.
- **Every CommonJS entry runs unwrapped** in `cjs` format without splitting when nothing
  else imports it (`plan.rs` `unwrapped_entries`), and `import * as ns` of a CommonJS file
  names its import `ns`, as esbuild does.
- **`define` keys** (`prepare.rs` `apply_defines`) match `a.b`, `a?.b` and `a["b"]` alike,
  and may start at `import.meta`; a write keeps its target unless the value is a name
  (`d = 0` with `d=ident` becomes `ident = 0`), as in esbuild.
- **tsconfig `alwaysStrict` counts only when set** (directly or through `strict`), as in
  esbuild: tsc 7's default of `true` would add `"use strict"` to every TypeScript file.
- **`alias` runs before everything else in resolution** (`resolve.rs` `alias`): only package
  paths are aliased; the longest matching key wins (`key` exactly or `key/rest`, which keeps
  `/rest`); the substitution resolves from the working directory, and `external` patterns
  see the substituted path, as esbuild's CLI does.
- **`keep_names`** (`--keep-names`, `ts_r_minify::keep_names`): after each module's renaming,
  functions and classes whose binding changed get their `name` back through the `__name`
  runtime helper (`Helper::Name`, `Object.defineProperty(target, "name", { value,
  configurable: true })`); a top-level class always does, because `var C = class _C {}`
  would be named `_C`. The calls for a top-level declaration are printed right after it.
  Transform mode uses `__name`, defined in the prelude when a call was added.
- **`drop_labels`** turns labeled statements whose label is in the set into empty statements
  right after parsing (`prepare.rs` `drop_labels`), so their imports are never resolved.
- **Implicit extensions inside `node_modules` try JavaScript first** (`javascript_first`:
  `.jsx`, `.js`, then `.tsx`, `.ts`, then the rest), as esbuild does; elsewhere the
  configured `resolveExtensions` order applies.
- **Deterministic output**: module order is DFS pre-order from the entries; every hash map is
  either keyed lookup only or sorted before it affects output. No absolute paths in output
  except where the user asked (none today); cached data holds no absolute paths or times.
  Colliding output paths are rejected before any files are written in parallel.
- **Fragments are pure functions of `(prep_key, ModulePlan)`**: that is the cache key, in
  memory and in `ts_r_cache` (namespace `build`). Anything a fragment depends on must be in
  `ModulePlan` (it derives `Hash`).
- Module preparation depends only on the file, its loader, the tsconfig-derived transform
  options, the defines, dropped labels, annotation handling and the effective output format
  (`prep_key`).
  Resolution and the tsconfig-derived transform options are never cached across a
  structural change.
- A CommonJS entry of a single-entry CommonJS build runs unwrapped (Node's own `module`,
  `exports`, `require`) only when that is safe (`plan::unwrapped_entry`: nothing requires
  it, no top-level `return`/`arguments`, no direct `eval`, its `"use strict"` suits the whole
  file) and smaller (its free `module`/`exports` bytes stay under the wrapper and helper it
  saves). It then names its own top-level bindings around every other top-level name.
- Compact (minified whitespace) output puts fragments and chunk code on one line; the chunk
  source map shifts each fragment's first-line columns by where it starts.
- A chunk source map is built in parallel: each fragment's mappings are re-encoded on the
  pool relative to its first segment (`encode_fragment`), then stitched in order with only
  that first segment patched (`ChunkMap::add_encoded`). The result is byte-identical to
  adding the fragments one by one (`add_fragment`); a unit test holds them equal.
- **Module identity includes how it is imported.** An import with a `type` attribute gets its
  own module (key `path\0with:type`, loader from the attribute; `bytes` is `Loader::Bytes`).
  Binary loaders use `FileSystem::read_bytes`, preserving invalid UTF-8 and UTF-16 BOMs.
  External imports are keyed `path\0phase\0attrs` (`import defer`/`import source` and
  attributes are kept), and keep source evaluation order (`plan::external_order`). In CommonJS
  and IIFE output an external first loaded by an unwrapped module is required where that
  import runs (before the code of the next module to finish in evaluation order), as esbuild
  does; the rest are required at the top of the chunk.
- **Module kinds** (`link::compute_kinds`): a JS file with no ESM syntax that something
  imports a binding from is CommonJS. Resolving a missing export never includes its file.
  A re-export cycle is an error (`Detected cycle while resolving import "x"`). Top-level
  `this` in a CommonJS file is `exports`.
- **JSON** imported as a namespace gives an ES namespace whose `default` is a getter
  (`__tsrToEsm(mod, 2)`); attributed JSON exposes only that default getter (mode 3),
  regardless of a JSON `__esModule` property. The loader retains original top-level object
  keys for export discovery; a `"__proto__"` key or export name is always the computed key
  `["__proto__"]`, never a prototype. package.json is strict JSON (errors, not guesses).
- **Per-file transform choices are pure functions of the file**: a TypeScript file that
  exports a const enum keeps the enum object (`preserveConstEnums`, as `isolatedModules`
  does), since its importers are transformed apart from it; JS `accessor` fields are lowered
  under an explicit `--target`; tsconfig `"jsx": "preserve"` is ignored when bundling.
- **Targets are feature sets** (`features.rs`): `--target=esN` is every esbuild feature
  newer than year N (ES5 = 2014), and `--supported:<feature>=false` adds or removes one.
  Lowering goes by year through `ts_r_transform`: the oldest unsupported feature it can lower
  sets the year (`Features::lower_year`), so output may lower more than esbuild's but runs
  the same. ES2015-level syntax esbuild won't lower (`const`/`let`, classes, destructuring,
  default/rest parameters, `for-of`, generators, async without generators, spread) and a
  top-level `await` without `top-level-await` are build errors with esbuild's messages
  (`lowering::errors`); arrows and template literals stay as written.
  A BigInt literal in a value below `bigint` is a warning, not an error: "Big integer
  literals are not available in the configured target environment and may crash at
  run-time" (`lowering::bigint_literals`); literals in types are erased and not reported.
  esbuild also prints it as `/* @__PURE__ */ BigInt("123")`, which ts-r-build doesn't yet
  (it keeps `123n`). Observed behaviour: esbuild 0.28.2 and
  its `bundler_lower_test.go`, `bundler_default_test.go`,
  `bundler_loader_test.go` expectations.
- **Feature switches that change output, not syntax** (`prepare.rs`, `plan.rs`, `print.rs`):
  without `import-meta`, `import.meta` (also the one JSX's automatic runtime builds) is one
  empty object per module (`var import_meta = {}`) with a warning per use; without
  `arbitrary-module-namespace-names`, a string import or export name that is not an
  identifier is an error only where ESM output keeps it (entries and what they `export *`,
  external imports; `lowering::string_names`); without `object-extensions` a JSON
  `"__proto__"` key stays a plain key; without `const-and-let` the transform's own
  `const`/`let` (`import x = require`) become `var`; without `using`, assigning a module-level
  `const` or `using` binding is an error (`Cannot assign to "x" because it is a constant`),
  since lowering would turn it into a write that silently works.
- **Module keys that are not plain files** (`graph.rs`): `(disabled):x`, a file plus
  `WITH_TYPE_SEP` and an import attribute type, a file plus `QUERY_SEP` and the `?query` /
  `#hash` it was imported with (each suffix is its own module, as in esbuild; the file is
  read without it), and a `data:` URL whose MIME type is `text/javascript` or
  `application/json` (`data_url.rs`: loaded from the URL, cannot import relative paths).
  Strip them (`split_with_type`, `split_query`) before using a key as a path.
- **File-loader URLs are filled in per chunk** (`chunk.rs`): the module's cached code holds a
  placeholder (`prepare::asset_placeholder`, letters and digits only). During assembly,
  only its loader fragment is copied and its whole placeholder string literal is replaced
  by the escaped URL as seen from the chunk's directory (`./a.png`, `../a.png`), or under
  `--public-path`, plus the import's query. `[dir]` in `--asset-names` is relative to the
  entries' common directory, `..` written `_.._`. The replacement can shift source map
  columns after it on the same line. `--public-path` also prefixes cross-chunk imports
  (`plan::chunk_import`).
- **`copy` files** (`graph::copy_imports_external`): an `import`, `require()` or
  `import()` of one is an external import of a placeholder path (`copy_path`), replaced in
  each chunk by the copy's URL from that chunk (`chunk::copy_url`); the copy is named with
  `--asset-names`, or is the entry's own output when the file is also an entry point.
  Import attributes on such an import are kept, never checked.
- **A module only `import()` reaches is named like a chunk** (`bundle::dynamic_file_name`):
  `--chunk-names` (default `[name]-[hash]`, `[name]` the file's stem, `[dir]` dropped), not
  `--entry-names`, as esbuild 0.28.2 does (`import('./sub/c')` gives `out/c-HASH.js`).
- **CSS is bundled like esbuild 0.28.2 does it** (`css/`): a stylesheet is a module whose
  records follow its JavaScript ones (record `i` of `Sheet` is record `i` of the module's
  `meta`); JavaScript sees an ES module stub (`export default` the local names, named
  exports for identifier names) whose strings are placeholders `css::link` fills in. Each
  chunk's stylesheet is its entry's CSS in first-import order, every `@import` expanded in
  place; an earlier copy of a file is dropped when a later one applies under the same or
  fewer conditions, leaving an `@layer` statement only where layer order needs it (the
  rules are in `css/order.rs`). External `@import`s go first; only the first file's leading
  `@layer` statements go above them. A leading `@charset` becomes `@charset "UTF-8";` at
  the top, a later one stays. `composes ... from "file"` orders that file like an
  `@import`. The `:global` / `:local` mode a selector ends in holds for its body, and a
  nested rule left with no selector (`:global { ... }`) is its parent's body.
  `--minify-syntax` shortens colors in color properties and drops a rule or declaration an
  identical later one repeats. Parsing never fails: malformed CSS gives warnings, nesting deeper than
  128 levels an error. Minified identifiers use our own name sequence, not esbuild's
  frequency order.
- **Entries**: an entry given twice with the same name is built once; an entry with `*` that
  isn't a file is a glob (sorted, bounded, skipping directories the pattern cannot match).
  Reaching a traversal size or depth limit is an error, never a partial expansion. CSS
  entries are printed without `--bundle`; a missing `--tsconfig` is an error.
- **Resolution**: a `package.json` `exports` target or import of `./x.js` that is missing
  falls back to `./x.ts`/`./x.tsx` (`.jsx` tries `.ts` first); esbuild's dual-package rule
  (require picks `main`) applies only with the default main fields.
- **`--mangle-props`** (`props.rs`, `ts_r_minify::props`): a bundle gives each matching
  property name one short name over every included module (`plan.rs`, from the per-module
  counts in `ModuleMeta::props`), so objects keep their shape across modules; `ns.x` of an
  `import * as ns` is an import, not a property. Without `--bundle` each file is its own unit,
  as esbuild links each entry point alone. The patterns are part of the preparation cache
  key. Names follow esbuild's rules where observed, including what it deliberately breaks
  (an enum member read `E.x_` is mangled, the enum's own strings are not).
- **`--inject`** (`inject.rs`): every export of an injected file (dotted string names like
  `"a.b"` and `import.meta.x` included) replaces the same free name or member chain in every
  other module, after `define` (a define with a value wins; one that names an injected export
  forwards to it). Each entry imports every injected file first, in the order given; the
  added imports don't make a CommonJS file an ES module (`Prepared::injected`). Writing to
  an injected name and a missing file are errors. Without `--bundle` the build bundles only
  the injected files (`BuildOptions::bundle_only_injected`, tree shaking off, ESM by
  default); every other import stays as written. The injected files and their exports are
  part of each file's preparation cache key.
- **CSS `--minify-syntax`** (`css/calc.rs`, `css/print.rs`): declaration values get short
  numbers (`0.50px` is `.5px`) and folded `calc()` (only results exact to 5 decimal places);
  `@keyframes` selectors `from` / `100%` become `0%` / `to`. At-rule names print as written
  except `@media` and `@layer`, as esbuild does.
- **CSS nesting** (`css/nesting.rs`) is lowered with `--supported:nesting=false` (no
  target table yet), as esbuild does: descendant or `&`-substituted selectors, `:is()` for a
  parent list, `:scope` for a top-level rule a CSS module left without a selector; `&`
  inside selector functions is replaced too. Lowering has a budget of 2^20 component values
  plus 4 per input byte (`& &` doubles a selector per level): past it the sheet gets an
  error and lowering stops. Pretty
  output prints one selector of a list per line. `Cv::Raw` / `Cv::Space` are the only
  component values not backed by source text.
- **Local names** (`local-css`) also cover `@container` names, `container` /
  `container-name`, string keyframe and animation names, and the counter style of
  `list-style-type` / a valid `list-style` (predefined styles and keywords stay). A bare
  `:global` inside a function's selector list (`:nth-child(2n of ...)`) lasts past its commas.
- **Path externals** (`--external:./x`, `/x`): matched against the resolved path, before
  extension probing and after resolution (esbuild's post-resolve externals); the import is
  written relative to the output directory, also when the chunk is in a subdirectory of it
  (`--entry-names=sub/[name]` gives `../src/lib.js` in `out/sub/main.js`, as esbuild 0.28.2
  prints it, observed). This is esbuild's behaviour, kept on purpose.
- A define also applies inside TypeScript enum initializers before the enum transform, so
  its value can name a member of that enum (`prepare::enum_initializers`).
- Never panic on user input; every problem is a `Message` (esbuild-style text).
- At most `TS_R_THREADS` worker threads (default: cores, capped at 8). While a fresh graph
  loads, one more thread reads module files ahead of the workers (`CachedFs::read_ahead`)
  as the calling thread waits: opening files is costly and contended in the kernel on
  macOS, and one reader beats every worker opening its own (three.js x10: graph 110 to
  75 ms). Each text file is still read once through the inner file system; nothing read ahead
  outlives the graph phase.
- A one-shot CLI build ends with `BuildContext::finish_and_leak` (flush the disk cache,
  leak the rest): never call it where the context or process lives on.

## Testing

- `cargo test -p ts_r_bundler` (unit tests plus `tests/bundle.rs`, which bundles in-memory
  projects and runs them with Node through `scripts/limit.sh` when Node is installed, and
  `tests/dep_bundle.rs` for the dev-server adapter; set `TS_R_REACT_ROOT` to a project with
  React installed to render the real thing).
- esbuild oracle: see [../../docs/design/0008-bundler.md](../../docs/design/0008-bundler.md#testing).

## Dependencies

Workspace: `ts_r_span`, `ts_r_ast`, `ts_r_parser`, `ts_r_transform`, `ts_r_codegen`,
`ts_r_config`, `ts_r_resolver`, `ts_r_cache`, `ts_r_minify`, `ts_r_diagnostics`, `ts_r_hmr`
(optional, feature `hmr`, for the `DepBundler` adapter; the crate's dev-dependency on itself turns the feature on for
its own tests). External: `rustc-hash` (fast maps
for ids and paths), `blake3` (content and plan hashes for cache keys and chunk names; stable
across runs, unlike `std`'s hasher), `regex-lite` (the `--mangle-props` / `--reserve-props`
patterns, Go RE2 syntax as esbuild takes them; already used by `ts_r_hmr`). Dev: `ts_r_testing`.

## Gaps

See the design record's "Known gaps" section.
