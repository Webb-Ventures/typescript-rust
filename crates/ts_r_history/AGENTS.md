# ts_r_history

Incremental, history-based re-checking (track M5 "history"): which files can be checked once
and shared, and which must be re-checked after an edit. Root rules:
[../../AGENTS.md](../../AGENTS.md). Design record:
[../../docs/design/0005-history.md](../../docs/design/0005-history.md) (read its
"Implementation notes"); checker side: `ts_r_checker/src/layers.rs` and the "Levels" section
of [../ts_r_checker/AGENTS.md](../ts_r_checker/AGENTS.md).

## API in one breath

```rust
// Editor: order the program by level and build the session's checker on shared levels.
let arranged = ts_r_history::arrange(&mut program, &hot_file_indices);
let checker = Checker::layered(&program, arranged.parent.as_ref())
    .unwrap_or_else(|_| Checker::new(&program));
// arranged.perm[old] = new file index; arranged.ends = (level, end) per kept level.
// Whole program, layered (tests, examples): equals check_program up to file ids.
let (arranged_program, result) = ts_r_history::check_layered(&program, &hot, CheckRun::default());
```

```rust
// Which other files an edit can affect (editor):
let mut shapes = ProgramShapes::build(&program, 2);       // before the edit
let dependents = shapes.update(&edited_program, &changed_paths, &candidate_paths);
// Whole-program re-check with history (tests, ts-r-check):
let mut history = History::default();
let (diagnostics, stats) = history.check(&program, CheckRun::default(), Checker::new);
```

```rust
// ts-r-check: through the persisted history (cache None: plain check).
let out = ts_r_history::persist::check(&fs, persist::open_cache().as_ref(), &parsed_config, libs, 4);
// out.sources (per file id), out.diagnostics, out.mode (Cold / Unchanged / Incremental)
```

Files: `levels.rs` (the "must be at or below" graph and the level plan), `layout.rs`
(reordering, the process-wide layer registry, content keys), `shape.rs` (export shapes,
uses, deep hashes), `incremental.rs` (`History`: per-file state and the selective re-check),
`persist.rs` (the manifest in the ts-r cache, the full-load path), `lazy.rs` (the fast path:
a warm run that parses only what it re-checks), `stat.rs` (stat fingerprints), `libinfo.rs`
(lib global names and references cached per lib hash), `codec.rs` (length-prefixed binary
encoding), `lib.rs`. `TS_R_HISTORY_DEBUG=1` prints the fast path's plan to stderr.

## Invariants

- **Closure rule.** A file never depends on a file of a higher level. `levels::Graph`
  encodes every way one file's checking can see another's: imports and references (the
  target moves down), module augmentations (both in one level), global names (a declaring
  file moves to the lowest declarer or free referencer; `declare global` in a project file
  that augments `Array` lands in the lib level) and ambient modules (declarers move to the
  lowest importer that does not resolve the specifier to a file). Levels only decrease, so
  `settle` terminates. When you teach the binder or checker a new cross-file mechanism, add
  it here first.
- **Levels are keyed by content**: the key below, the compiler options as checking sees
  them (`layout::options_key`: every set option except file locations, emit and tooling
  ones, without origins or the config path), and per file path, flags, text hash (blake3,
  memoized per `Arc`), import resolutions and reference targets. A found level's
  `SourceFile`s replace the program's own (equal content), which is what lets checkers of
  other projects build on it. If an option in `NOT_CHECKING` ever influences checking, move
  it out of the list (a too-coarse key silently mixes results).
- `layout::share_sources` makes programs share equal `SourceFile`s (same path, text, binding
  inputs) through a weak process-wide map, before arranging.
- The registry holds `Weak<Layer>`s: whoever builds a checker keeps its levels alive (the
  LSP keeps the session base between sessions). Dead entries are pruned on insert.
- A merge across levels the plan did not see makes `Checker::layered` refuse; `arrange` then
  stops at the levels below and the rest is one level. Results never depend on the split.
- At most 4 levels (`ts_r_types::MAX_LEVELS`): lib, dependencies, stable, hot.
- **Shapes over-approximate.** A file's `uses` must name everything its checking can read
  from another file: imported names (per export; namespace imports, `import()` types,
  side-effect imports and `export *` targets as the module's whole API), augmentations of
  its own module (`Ref::Augs`), program edges that are not imports (`/// <reference>`),
  free names (with every global name when undeclared: "Did you mean"), declared globals,
  ambient modules matching bare or unresolved specifiers, lib-merging globals,
  `JSX`/`React`, and every global for `globalThis`/`window`.
- **Per-export deep hashes.** `Ref::Export(t, n)` stands for export `n` of `t` alone: its
  entry (whose deps include sibling exports it names, `ts_r_decls::Dep::Sibling`), the
  module's augmentations, and when `t` does not declare `n` every `export *` target's
  answer; an import that resolves nowhere also depends on `t`'s export names (the error
  suggests names) and on its top-level local names (`FileShape::locals`: TS2459/TS2460 when
  `t` declares the name locally, TS2305 when not). Dependency files (`node_modules`), JSON and `export =` modules count as
  a whole (`Node::All`); a dependency file's entry also depends on the global names it reads
  (a package can name a project's global type), and an inferred export on the values it is
  inferred from (`ts_r_decls` deps: `export const y = x` depends on `x`).
  A deep hash covers a node and everything reachable from it (cycles hashed per strongly
  connected component), so a signature change anywhere along a type's dependencies reaches
  its users. When the checker learns a new cross-file lookup, add a use for it here and a
  mutation for it in `tests/differential.rs`.
- **Persisted history**: paths relative to the config directory (`lib:<name>` for lib files),
  no absolute paths or timestamps in the payload; the key holds the executable's size and
  mtime so a rebuilt checker never reuses old results. The manifest also keeps the requested
  lib files that were missing and whether a lib store was found: a missing lib that appears
  means a real check. Bump `persist::FORMAT_VERSION` when the manifest layout, a hash rule
  or shape rule changes. Decoding never panics; anything unreadable is a cold run.
  Each saved request keeps its separate resolution mode (when different from the
  specifier's first unattributed mode) and result; lazy replay restores both resolution
  maps. Shapes conservatively include alternate targets in exported dependencies because
  declaration dependencies identify imports by specifier only.
- `History::check` equals `check_program` exactly. It also re-checks a file when one of its
  diagnostics points into a file whose text changed (positions), and treats blocking syntax
  errors like `check_program` (nothing checked, history unchanged). The parser's checker
  grammar errors (`SourceFile::grammar`) never block: a `FileState`'s `syntax` leaves them
  out and its `diags` carry them, so they are reported only for checked files, as
  `check_program` does (`incremental::syntax_only`, `Ck::check_source_file`). Octal escapes
  in strings (`SourceFile::lenient`) are in `grammar` but are syntax errors to tsc:
  `syntax_only` keeps them, through `ts_r_parser::tsc_syntax_errors`, like the driver, and
  adds TS1084 for a malformed `/// <reference />` (`invalid_reference_diagnostics`: a
  syntax error to tsc, so it blocks).
  Reference diagnostics are stored separately from syntax and semantic diagnostics:
  full checks and lazy replay use the checker's `reference_diagnostics` against the full
  program's paths, plus TS2688 for a `/// <reference types>` that resolves nowhere
  (`type_reference_diagnostics`: the full path takes the names from
  `ProgramFiles::missing_type_references`, the fast path from the saved references without
  a target), refreshing them when the source or its resolutions change.
  Link diagnostics go through `driver::link_diagnostic_reported`, as in `check_program`:
  a checked file's, unless `@ts-ignore` / `@ts-expect-error` governs the line
  (@colors/colors). Lib-side ones stay out (ts-r-check prints lib diagnostics without a path).
- **Deep hashes are path-form independent**: `History::path_key` (set by `persist`) makes
  deep hashes write paths as saved (relative, `lib:<name>`, `?<spec>` for unresolved
  specifiers), so the full path (absolute paths) and the fast path (saved paths) compute the
  same `used` hashes. Parts are hashed in program order (the first declaration of a global
  variable fixes its type).
- **The fast path (`lazy.rs`) decides exactly like `History::check`** (same targets, same
  states) and replays `load_program` from saved requests: same file order (BFS waves for
  discovery, DFS post-order from `types` then roots), same `node_modules` flags, same lib
  order (`libinfo::walk`; the manifest keeps libs in program order). Anything it does not
  handle (inconsistent manifest after blocking syntax errors, lib or option changes, a
  missing lib file that appeared, new blocking syntax errors, a program whose libs lack
  required core globals, reported as TS2318) returns `None` and the caller
  does the full load. Re-checked files
  are checked on a sub-program: the libs, the files their uses reach (deep reach), re-linked
  files (those sharing a global, ambient-module or augmentation key with an edited or removed
  file, and augmentations of modules that re-export from one, transitively: an augmentation
  merges with its module's re-exports), the files their uses reach, and their merge partners.
- **DOM libs are optional in the sub-program** (`libinfo::DOM_GROUP`): parsed only when a
  global name the check can see is one they declare, every global is reached, a re-checked
  file is a script (top-level `this`), or a sub-program file uses `import.meta`. No other lib
  is ever left out (the checker looks up their types by itself).
- **Stat fingerprints** (len, mtime, ctime, inode) live in a sidecar entry tied to a digest of
  the manifest bytes, so the manifest stays timestamp-free. A fingerprint is trusted only
  when older than the racy window (2 s); otherwise the file is read and hashed.

## Dependencies

`ts_r_span`, `ts_r_ast`, `ts_r_binder`, `ts_r_config`, `ts_r_diagnostics`, `ts_r_checker`,
`ts_r_types`, `ts_r_decls` (signature hashes and dependencies), `ts_r_cache` and
`ts_r_resolver` (persisted history), `rustc-hash`, `blake3` (content keys; already used by
`ts_r_cache` and `ts_r_decls`). Dev: `ts_r_testing` (memory cap).

## Tests and harnesses

- `cargo test -p ts_r_history`: `tests/layers.rs` checks `tests/fixtures/app` (imports, a
  `node_modules` package, `declare global` augmenting `Array`, an ambient `*.svg` module, a
  module augmentation, script globals, deliberate errors) flat and layered with every choice
  of hot file and requires identical diagnostics; it also checks the level plan and that a
  second program with the same files reuses the first one's levels by content.
- `tests/differential.rs`: random edit sequences (fixed seed and count; `TS_R_DIFF_STEPS`,
  `TS_R_DIFF_SEED` override) on a temp copy of `tests/fixtures/app`: word swaps, `export`
  toggles, inserted declarations (augmentations, `declare global`, `export *`, imports of
  new files, DOM globals, `import.meta`, `Window` augmentation), deleted lines, created,
  deleted and renamed files, reverts. After every edit `History::check` and `persist::check`
  (temp cache, racy window 0, unique past mtimes) must equal `check_program`, and the
  persisted run's file list must equal `load_program`'s. Seeds 1-60 x 300 edits at `5e68992`
  (26 Oct 2026): 60 of 60 pass (37 at `ea41eac`: after `banner` was renamed in
  `src/util.ts` the incremental run kept TS2459 where a fresh check reports TS2305, fixed by
  `FileShape.locals`); seeds 61-200 x 300 and 1001-1040 x 1000 pass too. Most persisted
  runs take the fast path, some with the DOM libs left out.
- `tests/shapes.rs`: body edits affect nobody; an export change reaches its users and not
  the rest (not even other importers of the same file, unless a signature names it); a
  global reaches the files that name it.
- `examples/closure.rs`: `closure <tsconfig> [file...]`, what re-checking one file needs
  (files and bytes reached, parse time), per-lib parse times and the verification costs.
- `examples/compare.rs`: `compare <tsconfig> [hot file...]`, flat vs layered on a real project
  under `scripts/limit.sh`; prints `PUBLISH_MISSES` (`COMPARE_FILES=1`: per-file times).
  private-app, all 25 projects, no hot file and one hot file each: identical
  diagnostics, 0 publish misses.
- `examples/probe.rs`: `probe <tsconfig> <file> [iters] [flat|layered]`, per-keystroke phase
  times. apps/web `card-form.tsx`: flat check 24.9 ms, layered 0.37 ms (arrange 0.6 ms, link
  0.6 ms).

## Measurements (a private monorepo, historical; release, macOS arm64)

Re-check precision (`ts-r-check`, scripted: for each of the first 15 non-test `src` files of
7 projects, add an export, change its type, revert; files re-checked per step summed over
the 91 files): whole-module imports 1040 → per-export 124 (kernel 18 → 12, contracts
48 → 25, providers 293 → 15, moderation 244 → 20, listings 158 → 26, api 236 → 15,
worker 43 → 11). Every step's output equals `--no-history`. What remains is the edited
file itself plus barrels (`export *`) and namespace importers.

Editor memory after closing files (`ts_r_lsp`, `tsr/project/release.rs`): all 649 files
open, then all but 10 closed: physical footprint 350 → 215 MB (fresh daemon with the same 10
open: ~135 MB; all 649 open: ~410 MB either way).

Editor (`editors/vscode/bench/editor-latency.js`, parity off), before → after levels:

| | before | after |
|---|---|---|
| card-form.tsx keystroke p50 / p95 | 25.6 / 26.8 ms | 2.8 / 3.5 ms |
| schema.gen.ts (largest) keystroke p50 / p95 | 6.6 / 16.7 ms | 4.7 / 5.4 ms |
| RSS, all 649 files open | 580 MB | 386 MB |
| open all 649 to diagnostics | 284 ms | 334 ms |

All-open memory: with levels keyed by the full options `Debug` form (origins, config path)
every project built its own lib level (25 live, 96 MB of level tables); keyed by
`options_key` 3 remain (26 MB) and all-open RSS went 465 → 386 MB. `examples/workspace.rs`
loads every project in one process like the daemon, prints live levels and compares each
project's layered diagnostics with a flat check (private-app: 25 projects, identical).
