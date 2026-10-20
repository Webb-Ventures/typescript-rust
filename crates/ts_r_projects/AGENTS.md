# ts_r_projects

Project references, incremental builds and `tsc --build` for ts-r-check (design record
[0012](../../docs/design/0012-tsc-emit.md), phase E3; "Implementation notes (E3)" there).
Root rules: [../../AGENTS.md](../../AGENTS.md).

| Module | What it does |
|---|---|
| `buildinfo` | ts-r's `.tsbuildinfo`: schema, tsc's path, read/parse, deterministic text |
| `incremental` | Which outputs a build writes and the buildinfo it leaves (`plan`) |
| `references` | Referenced configs (transitive), the resolver's source-to-output map, TS6306/TS6310/TS6377/TS6307 |
| `graph` | `--build` project graph: build order, TS6202 cycles, missing projects |
| `status` | Up-to-date statuses in tsc's order, their messages, expected outputs |
| `build` | The `--build` driver: flags, status lines, touching, clean, parallel builders; `--watch` cycles (`Session`, `Cycle`, `run_watch`) |
| `roots` | `files` entries that name no file: TS6053 / TS6231 with tsc's chain and related info |
| `clock` | Local `HH:MM:SS AM` times from TZif (`$TZ`, `/etc/localtime`), UTC fallback |

The compile pipeline (load, check, emit, render) stays in ts-r-check behind
`build::Host`; this crate never prints diagnostics itself.

## Invariants

- **Our buildinfo, tsc's path.** `{"version":"ts-r x.y.z (tsc 7.0.2)", "root", "errors",
  "program"}`; tsc's or another version's buildinfo reads as `Read::Foreign` (a rebuild,
  "generated with version ..."). Paths are relative to the buildinfo's directory, lists are
  in program order or sorted, there are no timestamps and no absolute paths; option values
  are hashed with paths made relative to the config directory. Libraries are recorded only
  as one hash of their file names and contents.
  The path: `tsBuildInfoFile`, else `<config stem>.tsbuildinfo` beside the config, moved
  under `outDir` like an output when the config is inside `rootDir`.
- **A build leaves a clean build's outputs.** `incremental::plan` writes tsc's set (changed
  files, importers of a changed declaration signature while signatures keep changing,
  declarations of the other transitive importers, pending emit, every file
  when a kind's options changed) plus any output whose bytes differ from the file on disk.
  Composite projects do not rewrite a `.d.ts` whose text is unchanged when the previous
  state is known, but rewrite its `.d.ts.map` all the same; other incremental projects, and
  builds without a readable buildinfo of this version, rewrite both (observed).
  `latest_changed_dts` ignores a `//# sourceMappingURL=` line added or dropped
  (`declarationMap` toggled leaves downstream projects up to date). A new `declarationDir`
  re-emits the JavaScript too (it is in the JS option hash). Checking-only option changes
  re-check but re-emit nothing; `declaration` is not one (turning it off asks for nothing
  new, turning it on is a new output kind). A changed file that declares a const enum
  makes its transitive importers re-emit their JavaScript too (they inline its values).
  Another set of library files (names and contents, hashed as `ProgramState::libs`; `noLib`,
  `lib` or `target` toggled) affects every file, as tsc does.
  A file whose missing `.d.ts` tsc's own emit diagnostics explain (TS4094 and the like) is
  not pending, and the declaration options are stored as after a complete emit; ts-r's
  refusals (TSR codes) leave the file pending.
  The buildinfo is written only when its text changes (`--build` of a project without
  program state rewrites everything, as tsc does).
- **Stored options.** Per output kind, the hash of the options it was last emitted with and
  its map options (`sourceMap`/`inlineSourceMap`, `declarationMap`); a kind not produced by
  a build that emitted something is forgotten, so producing it again re-emits every file.
  The `package.json` paths resolution read or looked for are recorded too, by `tsc -p` as
  by `-b` (a later `-b` then rewrites nothing).
  Options equal to tsc 7's default are left out, as tsc leaves them out of its buildinfo:
  so far `strict: true` (observed: `tsbuildWatch/programUpdates/works-with-extended-source-
  files`).
- **Signatures.** A file's signature is the hash of its declaration output when the emit has
  one (E2's `.d.ts`, `FileState::signature`), else of its declared API
  (`ts_r_decls::index(..).api_hash`, `FileState::api`; text hash for `.d.ts` inputs). The
  API hash is stored next to a `.d.ts` signature, so a build without declarations compares
  it (tsc still computes the `.d.ts` signature then). Both
  empty means "the version" (tsc's used version): never computed, or reset because an
  import's signature changed and the file was not re-evaluated; the next computation is a
  change. Signatures are computed only for affected files (changed, importers of a changed
  signature while it keeps changing; under `isolatedModules` importers are never
  affected, only their declarations pending). A build that writes declarations stores the
  `.d.ts` hash of every file it wrote one for. The seam is
  `incremental::declaration_signature`: outputs are found by path suffix (`.d.ts`,
  `.d.mts`, `.d.cts`), not by `OutputKind`.
- **Status order** (observed): dependency failures under `--stopBuildOnErrors`, container
  projects (no files, and a `references` key or an explicit `files`: never built, only
  their config file's own errors printed, such as TS18002, which counts as skipped outputs;
  not the program-level option checks such as TS5108), `--force`, buildinfo missing / other version /
  errors, an output kind (or its maps) asked for that was not produced last time; when no
  root is newer than the buildinfo, a missing output and an output older than the newest
  root of a project without program state (config files count as inputs there when the
  buildinfo still holds program state, a project that stopped being incremental), and
  pending emit of one with it (observed with `noEmit` toggled in the config,
  `tsbuildWatch/noEmit`); config
  files newer, program state missing, emit pending, options changed, a root
  file dropped, program state in a non-incremental project's buildinfo, root inputs (missing; newer: incremental projects compare content, so a
  touched file only updates timestamps), non-root program files, outputs of projects without
  program state (must all exist; newest input vs oldest output), recorded `package.json`
  files (newer, or a missing one now present), referenced projects (newer
  buildinfo: a newer `latest_changed_dts` rebuilds, naming the reference's path; otherwise
  timestamps only). A root dropped is reported before a missing output. A reference built
  in this run, or with a newer buildinfo, whose recorded `.d.ts` inputs all have the content
  hashes the buildinfo stored (`buildinfo::content_hash`, which ignores a final
  `//# sourceMappingURL=` line) leaves the project up to date "with .d.ts files from its
  dependencies" ("Updating output timestamps").
- **Output** is printed in build order whatever `--builders` says (results are buffered per
  project). Status lines are tsc's: `[HH:MM:SS AM] msg` pretty, `HH:MM:SS AM - msg` plain,
  each followed by a blank line; the project list joins with `\r\n` as tsc 7 does. Dry-run
  lines name projects by absolute path, as tsc does.
- **`--watch`** (record 0012, E4): `run_watch` takes the projects whose own inputs changed
  (`Cycle::changed`, by config path; `None` on the first build). They and the projects
  referencing them (transitively) are considered; the others replay their last result
  (diagnostics, outcome) without status lines. A considered project that was only reached
  through a reference and had errors last time is `Status::HasErrors` ("is out of date
  because it has errors"): rebuilt, and touched with "Updating unchanged output
  timestamps" when nothing was written. A project whose config appeared or disappeared is
  built, not replayed. `Host::touched` reports the outputs whose timestamps a build set.
  A reference whose declarations changed reads differently in a watch build: "output
  '<oldest output>' is older than input '<the reference's config>'" instead of the
  buildinfo and the reference's path (observed: `tsbuildWatch/projectsBuilding`,
  `tsbuildWatch/reexport`, `tsbuildWatch/sample/incremental-updates-in-verbose-mode`).
- An incremental project rebuilt because a config file is newer, writing nothing (not even
  its buildinfo), gets "Updating unchanged output timestamps" (observed), like one rebuilt
  for a reference. So does a non-incremental project whose only errors are declaration
  emit errors (`Compiled::emit_errors_only`).
- A reference in error (TS6053, TS6306, TS6310, TS6377) is an option error: the program's
  own diagnostics are not reported. TS6377 (this project's buildinfo is a referenced
  project's, `--build` writing one for every project): every output is written, the
  buildinfo is not, and the outputs are touched ("Updating unchanged output timestamps").
- **`noCheck`**: the buildinfo keeps `errors` (syntax, options, emit), `semantic_errors`
  and `check_pending` apart. A `noCheck` build leaves a check pending unless nothing changed
  since a build without one (then it keeps the stored state, semantic errors included);
  `noCheck` is left out of the check-options hash. The up-to-date check reports "needs to
  report errors" for `errors`, and for semantic errors or a pending check only when the
  build checks.
- `circular: true` on a reference allows cycles through everything reached via it.
- Referenced projects keep their own format: an ES-module project's files are ES modules to
  the importer (`ts_r_resolver::ProjectReferences::mark_esm`); a referenced project's
  sources in the program (a `noEmit` reference) are never emitted
  (`references::drop_referenced_outputs`).
- A `files` entry naming no file is a global error (TS6053 with a TS extension, TS6231
  without one unless a file with one of the tried extensions exists), with the chain "The
  file is in the program because: / Part of 'files' list in <config name>" and related info
  at the entry only when the list is in the project's own config (not inherited).
- A referenced project whose config is missing is a node of the build order (it prints its
  global TS6053, "File '<config>' not found.") but not an upstream of the project
  referencing it, which reports TS6053 at the reference as an option error. Only a missing
  project named on the command line (`Node::root`) counts as skipped outputs (exit 1).
- Recursion in `graph` is bounded by the number of distinct configs (each is on the DFS
  stack once) and capped at 4096.

## Dependencies

`serde`/`serde_json` (the buildinfo JSON), `blake3` (content hashes, as ts_r_cache and
ts_r_decls), `rustc-hash`. ts_r_checker (the loaded program), ts_r_emit (output paths and
the in-memory emit), ts_r_resolver (`ProjectReferences`), ts_r_decls (API hashes),
ts_r_config, ts_r_diagnostics, ts_r_span, ts_r_ast (to find const enum declarations in a
changed file).

## Testing

Unit tests next to the code (`cargo test -p ts_r_projects`). The oracle is tsc 7's
`tsc/` and `tsbuild/` scenario baselines, run by ts_r_check's `scenario_oracle` example
(see [../ts_r_check/AGENTS.md](../ts_r_check/AGENTS.md)).
