# 0005: Check history: format, invalidation and the correctness test

Status: Proposed
Owners: `ts_r_decls` (per-file declaration index), `ts_r_history` (levels, invalidation,
history store; checker hooks in `ts_r_checker::layers`); storage through `ts_r_cache`

## Context

The plan's "Incremental type-checking" section: `ts-r-check` saves a history after every run
(file hashes, exported-signature hashes, which symbols each file uses, the errors found) and the
next run, in a watcher, a fresh terminal, after `git checkout` or in CI with a restored cache,
re-checks only changed files, stopping early unless an exported signature changed, and then
only files that use the changed symbol. Targets: <300 ms for a 10-file PR on 50k modules; zero
differences from a full check over 1,000 random edit sequences (required).

tsc's `--incremental` tracks dependencies per file (any change to an imported file's
declaration output re-checks all importers). We track per symbol.

## Decision

### Units

- **Semantic file hash**: BLAKE3 over the file's token stream (kind + text of each token, no
  trivia), plus the source kind. Comment and whitespace edits keep it unchanged.
- **Top-level symbol**: a declaration name in a file's module exports, in its global
  contribution (script top level, `declare global`), or a non-exported top-level declaration
  that an exported one references. Members and nested namespace exports belong to their
  top-level symbol. Identified by `(relative file path, Name string, meaning)`.
- **Signature hash** of a top-level symbol (BLAKE3, 32 bytes in `ts_r_decls`; the manifest
  stores the first 16):
  - *explicit*: computed by `ts_r_decls` from the AST of the declaration's type surface
    (type annotations, parameters, type parameters, heritage clauses, members with their
    annotations, enum member initializers, `declare`/`export`/modifiers), excluding bodies and
    initializers of annotated declarations, constructor and setter bodies, decorators and the
    types of private members. Positions, whitespace and comments never enter it. A signature
    that references a non-exported declaration of the same file (`export function f(): Local`)
    folds that declaration's surface in. Purely syntactic, before any checking.
  - *inferred*: when part of the surface is missing an annotation (unannotated `const`, function
    without a return type, class property without a type), the declaration is marked
    `explicit = false`; `ts_r_decls` then also hashes the parts the type is inferred from
    (the body or initializer), which is conservative, and the checker may replace it with a
    **structural type hash** of the inferred
    type (hash over type content: kinds, literal values, member names, and for named types their
    `(file, name)` identity, never `TypeId`s).
  - Merged symbols (interfaces/namespaces across files, module augmentations, `declare global`)
    hash the sorted list of their per-declaration hashes.
- **Used-symbol set** of a file: every top-level symbol of *another* file whose declaration
  information the checker read while checking this file, plus global names looked up (found or
  not; a later global declaration can change resolution) and every module specifier's
  resolution result. Recorded at one choke point in the checker (`deps.record(symbol)` on
  each access to declared/inferred type, members, signatures, base types, alias targets,
  enum values) **at access time**, even when the value was cached by another file's check.
  The binder's free-name list and import aliases seed it.

### On-disk format

Stored in `ts_r_cache` namespace `check`, format version `HISTORY_VERSION`.

- **Manifest key**: `KeyBuilder("ts-r/history")` over tool version, the resolved compiler options
  (canonical form), lib file semantic hashes, and the config path relative to the project root.
  No absolute paths, so CI on another machine finds it.
- **Manifest payload** (little-endian, length-prefixed, sorted by relative path):

  ```text
  header: magic "TSRH", version u32, file_count u32, string table
  per file: path (str idx), semantic hash [16], decls hash [16],
            exports: [(name str idx, meaning u8, explicit u8, sig hash [16])]  sorted by name
            uses:    [(file idx, name str idx, meaning u8)] + [global name str idx] sorted
            resolutions: [(specifier str idx, resolved file idx | NONE)]
            result key [32]  -> per-file result entry
  ```

- **Per-file result entry** (content-addressed by `(semantic hash, options, uses-hash)`):
  diagnostics anchored to tokens: `(code, category, start token idx, start offset in token,
  end token idx, end offset, message template args)` where args that quote source text are
  stored as token ranges and re-rendered from the current text. Token anchoring makes cached
  diagnostics survive whitespace/comment edits (line and column are recomputed on replay).
- Writes after a run: changed result entries, then the manifest (atomic via the cache's
  temp-and-rename); a crash leaves the previous manifest valid.

### Invalidation algorithm

```text
load manifest M (miss → full check, write history)
for each file f in the program: h = semantic_hash(f)
dirty = { f | f new, or h != M[f].hash, or a resolution of f changed, or f uses a removed file }
repeat until dirty is empty:
    check all files in dirty in parallel (with dependency recording)
    for each f in dirty: compute export signature hashes S'(f) (decls explicit + checker inferred)
    changed = { (f, name) | S'(f)[name] != M[f].exports[name] }  (added/removed exports included)
    dirty = { g not yet checked | g.uses ∩ changed ≠ ∅ }        (reverse index of uses)
            ∪ { g | g looked up a global name whose merged hash changed }
reuse stored diagnostics for every file not checked; write the new manifest
```

Global or option changes (tool version, options, lib hashes) change the manifest key: a full
check. Package upgrades change the semantic hashes of the `.d.ts` files involved, which flow
through the same algorithm. A file whose check fails internally (panic guard, budget) is marked
"no history" and always rechecked.

### Correctness test

`ts_r_checker/tests/history_fuzz.rs` (lands with the checker):

1. Generate a seeded random multi-file project (modules and scripts, interfaces merged across
   files, namespaces, generics, inferred and annotated exports, re-exports, `export *`,
   `declare global`, module augmentation, circular imports).
2. Apply a random sequence of edits (body change, signature change, add/remove/rename export,
   add/delete file, change an import, comment/whitespace-only edit, swap annotation for
   inference, branch switch = jump to an older snapshot).
3. After each edit run (a) the history-based check carrying the history forward and (b) a full
   check from scratch; compare the sorted, normalized diagnostics. Any difference fails with the
   seed and the minimized edit sequence.
4. CI runs 1,000 sequences (gate: zero differences); nightly runs more and `ts-r-check --verify`
   on real repos (full check vs history; a difference is a bug and falls back to full check).

## Invariants agents must keep

1. History never changes results: a history-based run's output equals a full check's.
2. Signature and semantic hashes never include `TypeId`s, absolute paths, timestamps or trivia.
3. Every cross-file read in the checker records a dependency (single choke point).
4. A missing, corrupt or version-mismatched history means a full check, never an error.
5. The decls hash of a declaration depends only on its own AST surface and the surfaces of the
   same file's non-exported declarations it references; cross-file inputs are recorded as
   dependencies (`Dep::Import`, `Dep::Global`), never hashed in.

## Alternatives

- **tsc-style per-file dependency (re-check every importer when a file's d.ts changes).**
  Simple and safe, but a widely imported module re-checks most of the repo for any signature
  change; symbol-level uses cut that to the files that touch the symbol.
- **Merkle "deep" signature hashes instead of access-time recording.** Would avoid the
  recorder, but over-invalidates (every transitive mention) and misses augmentation effects
  unless merged at program level anyway. Access-time recording is exact and simpler to verify
  with the fuzz test.
- **Byte-offset diagnostics.** Break on every whitespace edit; token anchoring costs one lex
  per replayed file.

## Implementation notes (M5 history, `ts_r_history`)

What was built differs from the decision above in these points; review them here.

- **Levels (editor and shared state).** Programs are split into up to 4 levels (lib,
  dependencies, stable project files, hot files) with a type store and per-symbol state per
  level (`ts_r_checker::layers`, `ts_r_history::levels`/`layout`). Lower levels are bound and
  typed once per process and shared by every checker built on them (other projects, later
  edits), found by content key in a weak registry. The closure rule (no file depends on a
  higher level) is planned from imports, references, augmentations, global names and ambient
  modules; a merge the plan missed makes the checker refuse the split.
- **Deep (Merkle) hashes instead of access-time recording.** The alternative rejected above
  was chosen: recording needs a choke point in every checker module, and this track may not
  restructure them. Each file gets a *shape* (`ts_r_history::shape`): its exports, globals,
  ambient modules and augmentations from `ts_r_decls` with their dependencies, and what it
  uses (imports, its own module, free names, declared globals, ambient specifiers, lib-merging
  globals, `JSX`/`React`, all globals for `globalThis`). A use's deep hash covers everything
  reachable from it; cycles are hashed per strongly connected component. Over-invalidation
  stays modest because bodies are not part of signatures. An import uses one export of its
  module: `ts_r_decls` records references between exports of a file (`Dep::Sibling`), so a
  change to one export reaches the users of the exports whose signatures name it and no
  one else. Known coarsenesses: namespace imports and `export *` use a module's whole API,
  `node_modules` files are hashed per file (text hash), and a free name nobody declares
  depends on every global name ("Did you mean").
- **No token anchoring.** The file hash is the BLAKE3 of its text, and stored diagnostics keep
  byte offsets: a file is re-checked when its text changed (comments included) or when one of
  its diagnostics points into a file whose text changed. Dependents are still spared when the
  edit leaves the API alone.
- **One manifest, no result entries.** `ts-r-check` stores one manifest per project
  (`ts_r_history::persist`, namespace `check`): libs (`lib:<name>` and hash), roots, `types`
  resolutions, per file its text hash, binding options and every request's resolution, the
  output, and per-file history states (shape, used deep hashes, diagnostics). Paths are
  relative to the config directory. The key covers the tool version, the executable's build
  (a rebuilt checker never reuses old diagnostics), the config path and the options. A warm
  run first verifies the manifest without parsing (libs, roots, `types`, then each file's
  text, binding options and resolutions, in parallel) and prints the stored output when all
  match.
- **Lazy loading (M5 history, part 2).** A warm run after an edit no longer loads the whole
  program (`ts_r_history::lazy`). Unchanged files are recognised by stat fingerprints (size,
  mtime, ctime, inode; trusted only when older than a 2 s racy window), kept in a sidecar
  cache entry bound to the manifest's digest so the manifest itself stays timestamp-free.
  The program's file list is replayed from saved requests (only new files are parsed), shapes
  are computed for edited files only, and the re-checked files are checked on a sub-program
  of the libs plus the files their uses reach, re-linked files and their merge partners. The
  DOM libs join the sub-program only when a name they declare is reachable. Lib global names
  and references are cached per lib hash. Deep hashes write paths in the saved (relative)
  form and keep merged parts in program order (a global variable's first declaration fixes
  its type). `ts_r_decls` now records references inside expressions of hashed parts
  (inferred initializers), which the sub-program relies on to include the files they name.
- **Editor memory: closed files are released.** A language-server session keeps everything
  its checker computed (function bodies included) until an edit ends it. After files close
  and a quiet time, a project restarts its session on a fresh checker and a rebuilt stable
  level, or drops its whole program when none of its files is open; results are recomputed
  on demand (`ts_r_lsp`, `tsr/project/release.rs`). On a private monorepo (historical), all 649 files open then
  closed down to 10: physical footprint 350 -> 215 MB (a fresh daemon with those 10: ~135 MB).
- **Blocking syntax errors** produce `check_program`'s syntax-only output and leave the history
  as it was.
- **Correctness test**: `ts_r_history/tests/differential.rs` applies random edits to a fixture
  project copy (word swaps, export toggles, inserted declarations including augmentations,
  `declare global`, `export *` and imports of new files, deleted lines, created and deleted
  files, reverts) and compares the in-memory history and the persisted history (through a
  temp cache) with a fresh check after every edit. `cargo test` runs 80 edits; 40 seeds x 300
  edits pass locally.

## Status

Extended by [0012](0012-tsc-emit.md) (2026-10-19): the history key uses only `CHECK`-class options, so emit-only options never invalidate checking.

Proposed 2026-10-14 by the M5 binder agent. `ts_r_decls` implements the explicit signature
hashes and the per-file index. Implemented 2026-10-15 by the M5 history track in
`ts_r_history` (levels, shapes and deep hashes, `History`, the persisted manifest, the
differential test) with the deviations in "Implementation notes".
