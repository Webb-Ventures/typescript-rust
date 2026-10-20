# ts_r_bundler

esbuild-compatible JavaScript/TypeScript bundler: parallel module graph, ESM/CJS interop,
tree shaking, scope hoisting, code splitting, esm/cjs/iife output, loaders, source maps,
metafile, and incremental rebuilds. Driven by `ts-r-build` (crate `ts_r_build`) and the dev
server's dependency pre-bundler (feature `hmr`). Root rules: [../../AGENTS.md](../../AGENTS.md).
Design: [../../docs/design/0008-bundler.md](../../docs/design/0008-bundler.md).

Written from scratch. Never read or port esbuild, Rollup, Rolldown, webpack, swc or oxc source.
esbuild's docs, its end-to-end test cases (as data) and its observed output are the oracles.

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
the served URL when `DepBundle::serve_root` is set, and CSS outputs passed through);
`fscache.rs` caches directory listings and real paths for a build; `expr.rs` is a tiny
expression builder for generated code; `pool.rs` is the std-thread pool.

## Invariants

- **Deterministic output**: module order is DFS pre-order from the entries; every hash map is
  either keyed lookup only or sorted before it affects output. No absolute paths in output
  except where the user asked (none today); cached data holds no absolute paths or times.
- **Fragments are pure functions of `(prep_key, ModulePlan)`**: that is the cache key, in
  memory and in `ts_r_cache` (namespace `build`). Anything a fragment depends on must be in
  `ModulePlan` (it derives `Hash`).
- Module preparation depends only on the file, its loader, the tsconfig-derived transform
  options and the defines (`prep_key`). Resolution and the tsconfig-derived transform
  options are never cached across a structural change.
- A CommonJS entry of a single-entry CommonJS build runs unwrapped (Node's own `module`,
  `exports`, `require`) only when that is safe (`plan::unwrapped_entry`: nothing requires
  it, no top-level `return`/`arguments`, no direct `eval`, its `"use strict"` suits the whole
  file) and smaller (its free `module`/`exports` bytes stay under the wrapper and helper it
  saves). It then names its own top-level bindings around every other top-level name.
- Compact (minified whitespace) output puts fragments and chunk code on one line; the chunk
  source map shifts each fragment's first-line columns by where it starts.
- Never panic on user input; every problem is a `Message` (esbuild-style text).
- At most `TS_R_THREADS` worker threads (default: cores, capped at 8).

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
across runs, unlike `std`'s hasher). Dev: `ts_r_testing`.

## Gaps

See the design record's "Known gaps" section.
