# ts-r rules checklist

Mirrors the root `AGENTS.md`. When the repo's own `AGENTS.md` and this file disagree, the
repo's file wins; quote it, not this one. Each rule lists how a breach shows up in a diff.

## The from-scratch rule

> Every layer is written here: lexer, parser, AST, binder, checker, transformer, codegen,
> minifier, resolver, bundler, linter, dev server, language server.

- **No other compiler, bundler or linter as a dependency**: swc, oxc, biome, tree-sitter,
  rolldown, esbuild, typescript, ts-rust/tsc-rs crates or their WASM builds. Look at every
  `Cargo.toml` and `package.json` change, and `build.rs` / scripts that shell out to these at
  run time. Using them as *oracles* in tests, benches and `xtask` is allowed (that is how
  parity is measured: `bench/package.json` pins tsc, esbuild, oxc-parser, tsx).
- **No copied or translated source** from tsc, esbuild or any other tool. Reading their source
  to understand a rule is allowed, and citing the file read (`tsc: tsc/internal/checker/relater.go`)
  is fine. Signs of porting: comments claiming a port ("port of `checkTypeRelatedTo`"), upstream
  identifier spellings (`getTypeOfSymbol`, `isTypeAssignableTo` in camelCase), license headers,
  structure that mirrors an upstream file line by line.
- **Nothing from Rust ports of TypeScript** (ts-rust/tsc-rs, stc, ezno): any sign their source
  or design notes were used is a finding.
- **Small general-purpose crates** are fine (rustc-hash, memchr, smallvec, bumpalo, rkyv,
  memmap2, blake3, serde/serde_json, notify, rayon, crossbeam) **with a reason in the crate's
  AGENTS.md**. A new dependency without that note is a finding.

## Workspace hygiene

- Dependencies go in the crate's own `Cargo.toml`, not the root one.
- A new crate under `crates/` lands with its `Cargo.toml` **and** `src/lib.rs` or
  `src/main.rs` in the same change (the workspace globs `crates/*`; a manifest without a
  target breaks `cargo` for everyone).
- Every crate has an `AGENTS.md`; when an invariant changes, the crate's AGENTS.md changes in
  the same PR (agents-check only checks existence and links, not accuracy, so this is ours).
- Edition 2024. `unsafe` is denied workspace-wide; a crate that needs it allows it locally,
  with a `// SAFETY:` comment on **each** block and a note in its AGENTS.md.
- Never committed: `target/`, benchmark results, caches, binaries. (`node_modules` inside
  `crates/*/tests/fixtures/` is committed on purpose.)

## Never panic on user input

> Malformed source, config or JSON produces diagnostics.

Look for `unwrap()`, `expect()`, indexing (`s[i]`, `&text[a..b]`), `unreachable!()`,
integer overflow on `u32` offsets, and slicing at non-char boundaries on any path fed by
source text, tsconfig/package.json/ESLint config, CLI args, LSP messages or HTTP requests.
An `unwrap` on an invariant the code itself established (just-checked `Some`, arena index it
created) is fine.

## Determinism

> Output is deterministic: no `HashMap` iteration order in output, no timestamps or absolute
> paths in cached data.

- Iterating a `HashMap`/`HashSet`/`FxHashMap` into diagnostics, emitted code, JSON output,
  metafiles, cache entries or hashes without sorting.
- Parallel results (rayon, threads) collected in completion order.
- `SystemTime::now()`, `std::env::current_dir()`, absolute paths, PIDs or random seeds in
  cache keys, cached data or output.

## Performance is a feature

- Index-based data (`u32` ids into arrays) over `Rc`/`Box` graphs; interned strings; no
  per-node heap allocation in hot paths; no `clone` of large structures (ASTs, type stores,
  module graphs, file text).
- Benchmarks gate merges: a tool more than 5% slower than `main` blocks the PR
  (`bench-compare` skill). A change on a measured path whose PR or commits claim a speed-up or
  "no regression" without numbers is worth a finding only when the code clearly adds work on
  the hot path.

## Positions

> Positions are UTF-8 byte offsets (`u32`). Convert to UTF-16 or line/column only at the edges
> (LSP, diagnostics output).

UTF-16 or char counts stored in spans, cache, AST or binder data; conversions in the middle of
the pipeline; `chars().count()` used as an offset.

## Resource limits (shared 48 GB / 16-core machine)

- Anything that can loop or grow without bound (fuzzing, mutation tests, corpus runs,
  benchmarks, oracle runs, `npx`/`node`/`tsc` on real projects, long-running servers) runs
  through `scripts/limit.sh [--mem-mb N] [--time-s N] -- <cmd>`. Caps never above 8192 MB.
- Rust test, example and harness binaries over generated or real-world input call
  `ts_r_testing::cap_memory!()` (and have `ts_r_testing` as a dev-dependency).
- Fuzzing: bounded iterations, bounded input size, per-input time limit; no `loop {}` without a
  bound; nothing left running in the background.
- Every loop in the lexer, parser, binder and checker consumes input or bails out. Recursion
  has a depth limit that produces a diagnostic, not a stack overflow. Pathological inputs (deep
  nesting, huge files, lone `#`, unterminated constructs) get regression tests.
- At most 4 threads for corpus and benchmark runs (`RAYON_NUM_THREADS=4`, `--test-threads=4`).

## Design records and agent docs

- Records in `docs/design/` are the spec. Code that contradicts an **Accepted** record without
  updating it is a finding. Code that depends on a **Proposed** record must not merge before
  the record is Accepted.
- A change that sets or changes an invariant other code relies on (AST layout, type ids, cache
  keys, spans), adds a public interface (CLI flags, config options, diagnostics format, LSP
  behaviour), or deliberately differs from tsc 7 / esbuild / Vite / ESLint needs a record.
- Changes to design records, `AGENTS.md` files and skills are reviewed by a person. Check them
  for accuracy: commands that exist (`cargo xtask help`), paths that resolve, rules that do not
  contradict the root file.

## Branches and commits

- Branch names `<milestone>/<track>` (`m5/checker-parity`, `l4/editor-actions`) or
  `site/...`, `chore/...`. Never push to `main`; never rewrite pushed history.
- Commits only touch paths the track owns. A PR from one track that edits another track's
  crate without saying why is worth a cross-track (lens F) look, not automatically a finding.
