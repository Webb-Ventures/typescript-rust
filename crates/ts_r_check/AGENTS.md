# ts_r_check

The `ts-r-check` binary: `ts-r-check [-p tsconfig.json] [tsc 7 flags] [--threads N]
[--no-history] [--sound=...]`. Loads the project with `ts_r_config`, the lib files with
`ts_r_checker::LibStore::locate()` (`$TS_R_LIB_DIR`, the executable's directory, then
`target/upstream/typescript/tsc/internal/bundled/libs`), checks it through its saved history
(`ts_r_history::persist::check`; `--no-history`: `check_program`, cache untouched) and prints
diagnostics in tsc's format. Root rules:
[../../AGENTS.md](../../AGENTS.md). Design record:
[../../docs/design/0004-types-and-checker.md](../../docs/design/0004-types-and-checker.md).

## Behaviour (observed from tsc 7.0.2)

- **Command line** (design record 0012, E0): tsc 7.0.2's, parsed by `ts_r_config::cli`
  after ts-r's own flags (`--threads`, `--no-history`, `--sound*`) are taken out. Parse errors
  (TS5023, TS6046, TS5093/5094, ...) print without a summary (pretty: tsc's coloured
  `error TSxxxx:` prefix) and exit 1 before anything else. Then `-v` (`Version 7.0.2 (ts-r
  x.y.z)`: tsc's line for scripts matching `Version x.y.z`, plus ours; wins over `--help`),
  `--help` / `--all` (ours, in `tsc --all` order), exit 0.
- `--init` (before `--help`, `init.rs`): writes tsc 7's starter `tsconfig.json` to the cwd,
  given options replacing their template line or appended after it; TS5054 when one exists
  (exit 0 both ways).
- **Reporting flags** (`src/explain.rs`, record 0012, E4), from the command line or the
  config: `--listFiles` (absolute paths) and `--explainFiles` (paths relative to the cwd,
  each followed by why the file is in the program, three spaces in; it wins over
  `--listFiles`) print after the diagnostics and `--listEmittedFiles`, before the summary,
  for every program built (each `-b` project, each watch build). Lib files come first:
  `lib.d.ts` / `lib.es6.d.ts`, then the others in the `lib` option's order (files it does
  not name, the `.full` default libraries, last); then program order. Reasons are listed in discovery order (each
  root, then depth first its `/// <reference path|types|lib>` and imports in source
  order; then `types` entries; then libs), then the file's facts (output of a referenced
  project's source; ECMAScript/CommonJS format under `node16`/`nodenext`). `Imported via`
  keeps the specifier's quote and adds `with packageId` for bare specifiers that resolve
  into a package (resolved again with `ts_r_resolver`). `--listFilesOnly` loads the program
  and lists it, nothing checked or written, exit 0. `--diagnostics` /
  `--extendedDiagnostics` print counts and the time to stderr.
- `--traceResolution` (or `traceResolution` in the config) prints tsc 7's resolution log
  before the diagnostics, for every program built (`src/trace.rs`): the program's
  resolutions replayed through `ts_r_resolver`'s traced entry points with one `Trace`, in
  tsc's order (each root, then depth first its `/// <reference path|types>`, imports and
  the JSX runtime import; then `types` entries; with `libReplacement`, then each lib file
  by name as `@typescript/lib-*`, in CommonJS mode). Oracle: `cargo run --release -p ts_r_check
  --example trace_oracle` against tsc's `.trace.json` baselines (writes
  `target/trace-oracle.txt`). TSR2004 (a flag ts-r-check cannot honour: an error, exit 1,
  nothing checked) has no flag left today.
- **Flags not implemented** are never ignored silently: `--generateTrace`, `--generateCpuProfile`,
  `--pprofDir` and a non-`en` `--locale` are a TSR2005 warning. `--listEmittedFiles` prints
  `TSFILE: <absolute path>` per written output (`ts_r_emit::listing`). `--quiet` prints no diagnostics (exit code unchanged).
  `--checkers N` / `--singleThreaded` set the thread count (`--threads` wins).
- Without `-p`, the nearest `tsconfig.json` upwards from the cwd is used; none: the help, exit
  1 (tsc prints its help). `-p` to a missing path: TS5058 with the absolute path; to a
  directory without `tsconfig.json`: TS5081; no tsconfig with `--showConfig`: TS5081 naming
  the cwd. Files on the command line: TS5112 when a tsconfig is present (without
  `--ignoreConfig`), TS5042 with `-p`, otherwise they are the program
  (`ConfigLoader::command_line`; a missing one is TS6053 "Root file specified for
  compilation").
- `noCheck`: only syntax errors (`build::syntactic_diagnostics`: the parser's, without the
  checker-grammar codes) and emit diagnostics; outputs are written as usual. The buildinfo
  records semantic errors and a pending check apart from other errors
  (`ts_r_projects` AGENTS.md).
- With option errors, a program with syntax errors reports those and drops the option
  diagnostics (`build::syntax_before_options`, observed); without syntax errors the option
  errors are reported and nothing is checked, except TS2318 for missing core global types
  (`noLib`), which is global and reported with them.
- Option errors (a missing referenced config among them) also keep the rootDir program
  diagnostics (TS6059 at an import, TS6307) out of a `--build` project (observed:
  `tsbuildWatch/sample/watches-config-files-that-are-not-present`).
- Config diagnostics are `ParsedConfig::all_diagnostics()`: parse errors plus the program-level
  option checks (TS5052, TS5069, TS5102, TS5055, ...). Any option-check error means the
  program is not checked at all (tsc then reports no syntactic or semantic errors); parse
  errors (TS5023, TS5024, TS6046) do not stop the check. Config diagnostics merge into the
  program's by absolute path (global ones first), so `a.ts` errors print before
  `tsconfig.json` ones and `z.ts` after.
- `--pretty false`: `path(line,col): error TSxxxx: message` with chain lines indented two
  spaces per level, no summary line. Pretty output (the default on a terminal and with
  `--pretty`): code frames, a blank line after located diagnostics (none after global ones),
  then `Found 1 error in a.ts:3` / `Found N errors in the same file, starting at: a.ts:3` /
  `Found N errors in M files.` plus the `Errors  Files` table (rows sorted by the displayed
  file name, also for `-b`), the `:line` in grey, and a closing blank line. Global diagnostics count as errors but not as files.
- Paths are shown relative to the cwd. Output is sorted by path, then position.
- **Emit** (record 0012, E1): unless `noEmit`, the program goes through `ts_r_emit` (JS and
  source maps, `Layout::Tsc`, the checker as `EmitResolver`) into memory; outputs are
  written only after every diagnostic is known, and not at all with `noEmitOnError` and
  errors. The writer refuses to overwrite a program input (TS5055 is also checked before).
  Write failures are TS5033 errors. When nothing the check finds can stop the writes (no
  `noEmitOnError`, not incremental, more than one thread), the emit, with its own checker,
  and the writes run on a scoped thread next to the check (`early_emit`); its diagnostics
  are merged, and its listing and TS5033 printed, exactly where the sequential path does.
  This costs a second checker's memory during the check (date-fns `pkgs/core` peak RSS
  116 → 129 MB, typeorm 202 → 221 MB) for the emit's wall time.
- **Exit code** (record 0012, invariant 3): 0 with no errors; 2 with errors when every output
  was written; 1 with errors when any output was skipped (`noEmit`, `noEmitOnError`, TS5055,
  a write failure, or a file ts-r cannot emit yet: TSR2002 resolver unknown, TSR2003 target
  below ES2022, TSR2001 a declaration with an unresolved type, TSR2006 declarations of a
  JavaScript input). So 2 is never claimed without the outputs.
- Command-line options are merged before file discovery (`CommandLine::apply_to`, `null`
  clears), as tsc merges them, so root files, default exclusions, option checks such as
  TS5096 and TS5055, `--showConfig` and the history key see them. Reporting-only flags
  (hidden, no `affects` class: `--pretty`,
  `--checkers`, ...) are not merged, so they do not change the history key.
- `--threads` defaults to 4 and is clamped to 1..=4.
- Pretty output defaults to on for a terminal; `FORCE_COLOR` (any value) turns it on,
  `NO_COLOR` wins over both (observed with tsc 7).
- **Project references and incremental** (record 0012, E3; `ts_r_projects`): `-p` loads the
  referenced configs, redirects imports of their sources to their outputs
  (`ts_r_resolver::ProjectReferences`) and reports TS6306/TS6310 at the reference, TS6307
  for composite programs and TS6305 (checker) for an unbuilt reference. `incremental` /
  `composite` projects write through `build::write_project` (`ts_r_projects::incremental`):
  only the outputs tsc would write plus any whose bytes differ, and ts-r's own buildinfo.
- **`--build`** (`src/build.rs`, the `ts_r_projects::build::Host`): every `-b` flag
  (`--verbose`, `--dry`, `--force`, `--clean`, `--stopBuildOnErrors`, `--builders`).
  Each project compiles like `-p` (command-line options merged into each config). Exit 4 on
  a reference cycle (TS6202), 0 with no errors, 1 when a project's outputs were skipped,
  else 2. Non-pretty `-b` prints no summary; pretty prints one summary over all projects.
- **`--watch`** (`src/watch.rs`, record 0012, E4), for `-p` and `-b`: every file-system
  question a build asks goes through `watch::TrackingFs`, which records the answer and who
  asked (a project's config load, its root list, its program); a change is a question whose
  answer differs now (an edit, a touch: the stamp holds the modification time, a file
  created or deleted, a directory listing that changed). What a build writes or touches is
  answered again right after it, so a build never triggers the next. `-p` rebuilds with
  the session's `SourceCache` and the previous build's program state in memory
  (`build::compile` with `memory`), so only what changed and what depends on it is
  re-emitted; history and sound mode are not used. `-b` rebuilds the projects whose own
  questions changed and the projects referencing them (`ts_r_projects::build::run_watch`);
  the others print their last diagnostics again. Output: the screen cleared (pretty
  output only, not with `--preserveWatchOutput`, `--diagnostics`,
  `--extendedDiagnostics`), `Starting compilation in watch mode...` / `File change detected.
  Starting incremental compilation...`, the diagnostics (`-p`: the pretty summary too),
  `Found N errors. Watching for file changes.`. A look that finds no change prints nothing.
  A deleted `-p` config prints TS5083 and `Found 1 error.` without clearing. A rebuild for a
  directory change alone keeps the config's last TS18003 state (observed). The session's
  exit status: 0 for `-p`, the first build's for `-b`. Changes arrive through `ts_r_watch`
  (the working directory and the project directories, recursively) and a poll every
  `--watchInterval` ms (default 2000) for what lies outside them. `TS_R_WATCH_DRIVER=stdin`
  (hidden, for tests): each stdin line is one look, end of input ends the session, and each
  look ends with the line `\x01ts-r-watch-cycle <status>`.
- History (design record 0005, "Implementation notes"): the run's history goes to the ts-r
  cache (namespace `check`, `TS_R_CACHE_DIR` to move it). The next run prints the stored
  output when no file, resolution, lib or option changed (nothing parsed; unchanged files are
  recognised by stat fingerprints), or re-checks only the files an edit can affect, parsing
  only the edited files and what the re-checked files reach. Output always equals
  `--no-history`. `Outcome::sources` holds text only for files a diagnostic points into.
  `TS_R_CHECK_STATS=1` prints the mode, counts and phase times to stderr.
- History is not used when the program is emitted, `incremental`, has project references
  or `noCheck` (they need the loaded program). A `rootDir` alone does not keep it out: the
  run uses history, then loads the program for the TS6059 check only when `Outcome::files`
  holds a file that could be emitted (not an external library, not a declaration) outside
  `rootDir`. Until 2026-10-26 any `rootDir` meant a full check, so tRPC's and date-fns's
  bench configs (both inherit a `rootDir`) never had a warm run: macOS arm64, release,
  median of 10, warm 104 -> 23 ms (tRPC `packages/server`) and 247 -> 59 ms (date-fns
  `pkgs/core`); `--no-history` is 110 and 230 ms.
- TS6059 (tsc 7.0.2, observed): a file outside `rootDir` that only an import brings in is
  reported at that import, and the checker's diagnostics stay (sorted in by position, the
  TS6059 first at one position). A root file outside `rootDir` is a global option error,
  and the program's syntactic and semantic diagnostics are dropped
  (`build::rooted_program_diagnostics`).

## Measurements (a private monorepo's copy, historical; release, macOS arm64, in-process time)

Min of 9 runs (shared machine); "one file edited" appends a statement to the file (a new
edit each run, so the file is always read and hashed). Before: every edited run loaded and
parsed the whole program.

| project | `--no-history` | warm, no change | one file edited, before | one file edited, now |
|---|---|---|---|---|
| infra-project (1498 files, 18 checked) | 190 ms | 22 ms | 211 ms | 42 ms |
| apps/web (104 checked) | 50 ms | 5.2 ms | 26 ms | 12 ms |
| domains/moderation (37) | 33 ms | 1.5 ms | 20-24 ms | 5.8 ms |
| packages/providers (40) | 35 ms | 1.4 ms | 34 ms | 11 ms |

All 25 projects: cold, warm, body-edited, export-edited and reverted outputs identical to
`--no-history`. What an edited run still pays: scanning every file (stat, resolution
checks; 20 ms on infra-project), parsing the libs it needs (lib.dom alone is 6 ms, skipped when the
re-checked files cannot see DOM names), the check, and rewriting the manifest.

## Sound mode (design record 0007, crate `ts_r_sound`)

- After the regular check, `ts_r_sound::check_sound` runs over the same program. Its
  configuration is `--sound-config <file>` (a `ts-r.toml`, a `tsrconfig.json` or a tsconfig
  with a `"ts-r"` key, by `ts_r_sound::config::parse_file`), else the one of `ts-r.toml` or
  `tsrconfig.json` (nearest upwards from the tsconfig) or the tsconfig's top-level
  `"ts-r": { "sound": ... }` key (more than one: TSR0020 naming every file), else none, where
  only `// @ts-r <level>` pragmas turn files on.
- `--sound=strict|sound|contracts` puts every project file at that level (pragmas still win);
  `--sound=off` skips sound mode entirely (tsc parity runs).
- TSR diagnostics merge into the same sorted stream; the pretty summary counts them separately:
  `Found 7 errors (3 sound-mode) in 2 files.` Configuration errors print as
  `ts-r.toml(1,1): error TSR0020: ...`. `code-prefix = "TS"` prints `TS100201` for `TSR0201`.
- `--sound-report[=json]` prints the coverage report before the diagnostics.
- History and sound mode: sound mode needs the loaded program, which a warm history run never
  builds, and history does not save sound results. So history is skipped whenever sound mode
  may apply: `--sound=<level>` other than `off`, `--sound-config`, or a discovered `ts-r.toml`,
  `tsrconfig.json` or `"ts-r"` key. Without any of those, a history run still happens; if any project source
  contains `@ts-r` (a pragma could turn a file on), the run falls back to a full check plus
  sound mode. History runs answer that with `Outcome::pragma` (the manifest keeps a per-file
  flag), since they leave unchanged files unread and their `sources` texts empty. A warm run therefore never misses sound diagnostics. `--sound=off` keeps history.

## Scenario oracle (design record 0012, E3, E4)

`examples/scenario_oracle.rs` replays tsc 7's `tsc/`, `tsbuild/`, `tscWatch/` and
`tsbuildWatch/` scenario baselines
(`testdata/baselines/reference/{tsc,tsbuild,tscWatch,tsbuildWatch}`): it writes each step's files, runs
`target/release/ts-r-check` with the step's arguments (and `FORCE_COLOR` as tsc's harness
does), and compares the output, the exit status and the written files (`*new*`,
`*modified*`, `*rewrite with same content*`, `*mTime changed*`, `*deleted*`; buildinfo
contents are not compared, only that they were written). The ts-r version in buildinfo
paths and messages is normalised to `FakeTSVersion`. Mismatches go to
`target/scenario-oracle.txt` with buckets (E2 `.d.ts`, E4 flags, other). A watch scenario
is one session under the stdin driver: the first step starts it (its status is the
sentinel's), each `Edit [n]::` applies its changes, asks for one look and compares what
that build printed and wrote (no exit status); the `Watch Registrations::` trailer is not
compared, nor a step's `<config>::` / `SemanticDiagnostics::` trailer (a run without a
config has no project line: the trailer starts at `SemanticDiagnostics::`). Every wait has a 60 s timeout and the session is killed at the end. The two gate
lines are `Matched` (tsc/ + tsbuild/, gate 85%) and `Watch matched` (gate 80%).

```text
scripts/limit.sh --mem-mb 4096 --time-s 1200 -- cargo build --release -j 3 -p ts_r_check \
  --bin ts-r-check --example scenario_oracle
scripts/limit.sh --mem-mb 4096 --time-s 600 -- \
  target/release/examples/scenario_oracle --threads 3 [--show] [--verbose] [filter...]
```

`SCENARIO_KEEP=1` keeps the work directories under `target/e3-scenarios/`.

`tests/incremental_emit.rs` is design record 0005's differential test extended to emit (the
E3 gate): random edits to a copy of `ts_r_history`'s `tests/fixtures/app`, built after every
edit with `--incremental` (`-p`), with `-b`, and with `composite` + `declarationMap` (`-b`;
only once `.d.ts` emit exists, i.e. no TSR2004) in trees carried across the edits. Each
carried tree's outputs must be byte-equal to a clean build's (outputs of deleted sources may
stay, as with tsc; the buildinfo is not compared) and its diagnostics equal. Defaults: 30
edits, a fixed seed; `TS_R_DIFF_STEPS`, `TS_R_DIFF_SEED` override. Seeds 1-10 x 200 edits
pass, with E2's `.d.ts` emit too. `watch_emit_equals_clean_build` (E4) does the same with one
`ts-r-check -w` session per variant (`-p`, `-p --incremental`, `-b`, and `composite` once
`.d.ts` emit exists) carried across the edits under the stdin driver: after each edit and
look, outputs and diagnostics (status lines removed) equal a clean build's.

## Emit oracle (design record 0012, E0)

`examples/emit_oracle.rs` compares emitted files with tsc 7's emit baselines: every test of
`tests/cases/{compiler,conformance}` (submodule baselines) and tsc 7's own
`tsc/testdata/tests/cases/{compiler,conformance}` (`tsc7/` rows) with a `.js` baseline, one
run per variant. Per output it reports `bytes` (identical), `shape` (same
`ts_r_codegen::shape` AST; comments and layout ignored), `differs`, `missing` and `extra`, for
JS, d.ts, maps (`.js.map` JSON bytes and `.sourcemap.txt` segments) and JSON copies, with
rows ALL / ES2022+ (effective target) / areas and cause buckets; mismatches go to
`target/emit-oracle.txt`, `--show` prints both texts, `--show-maps` the differing map
segments (`-` tsc only, `+` ours only).

```text
cargo build --release -j 4 -p ts_r_check --example emit_oracle
RAYON_NUM_THREADS=4 scripts/limit.sh --mem-mb 4096 --time-s 1800 -- \
  target/release/examples/emit_oracle --threads 4 [--emitter check|transpile] [--verbose] [--show] [filter...]
```

- Baseline format (observed): after the title, one section per test unit except
  `tsconfig.json`, then the outputs, named by base name (full path with `@fullEmitPaths`).
  Outputs of one group (JS, then declarations) are concatenated directly; the harness adds
  `\r\n\r\n` before the first declaration and before `DtsFileErrors` (not compared). The
  harness writes CRLF unless the test sets `newLine`, so emitters get that newline. A
  `tsconfig.json` unit is extended by the harness config (its files and options apply,
  directives on top). Most tests set `@target: es2015`, so ES2022+ is the smaller row.
- Emitters: `check` (default) is the E0 reference, nothing; `transpile` is per-file
  `ts_r_transform::transpile` next to each TypeScript root; `program` is E1's emitter
  (`ts_r_emit::emit` over the loaded program with the checker's `EmitResolver`, as
  ts-r-check writes). Later phases never lower these rates.
- `EMIT_ORACLE_TSR=1` prints each TSR2001 refusal (path, line, message, source line) to
  stderr, for sorting the `.d.ts` types the checker cannot write yet.
- With `--emitter program` the E1 gate block prints shape and bytes for ES2022+ JS outputs (gates: shape 100% with
  `examples/emit_known_gaps.txt` listing the rest, bytes >= 90%) and map segments on
  byte-exact files (>= 90%). `--write-gaps` rewrites the gaps file from a full run.
- Normalization (observed with tsc 7.0.2, not a weakening): tsc ends a file at
  `//# sourceMappingURL=...` without a newline; the baseline harness adds one, so a single
  newline after a final URL line is dropped before comparing.
- Measured at E0 (2026-10-19, 11,925 test variants, 17,347 outputs: 14,476 JS, 2,635 d.ts,
  201 maps, 33 JSON, 2 other): `check` 0 bytes-exact (0.0%) everywhere, ES2022+ included
  (3,206 outputs, 2,159 JS). `transpile`: ALL 5,802 bytes (33.4%) / 11,604 shape (66.9%);
  JS 40.1% bytes / 80.1% shape; ES2022+ JS 911 / 2,159 bytes (42.2%) / 1,643 shape (76.1%);
  d.ts, JSON 0% (not emitted); maps 0 bytes, 4 shape. Largest JS causes: shape-equal layout
  (line breaks and spacing 3,923, comments 1,811), missing outside-root outputs (`outDir`, JS
  inputs) 701, CommonJS 307, lowering temporaries 287, static blocks 127, `__awaiter` 121.
- Measured at E1 (2026-10-19, `--emitter program`): ES2022+ JS 1,946 / 2,159 bytes
  (90.1%) / 2,012 shape (93.2%); map segments equal on 0 of 17 byte-exact files with maps
  (tsc maps node ends and more tokens). Remaining ES2022+ JS causes: `using` lowering
  (TSR2003) 33, CommonJS 19, lowering temporaries 18, import/export 11, classes 8,
  `__setFunctionName` 5, static blocks 4, JSX 4, extra code 4, not emitted 3, decorators 2,
  other 27; shape-equal comment and spacing families are listed in the codegen AGENTS.md.
- Measured at E1 close (`m8/emit-maps`, `--emitter program --layout tsc`):
  - ES2022+ JS: 1,996 / 2,159 bytes (92.5%), 2,034 shape (94.2%).
  - KNOWN_GAPS: 125, with 0 unlisted.
  - Map segments: 17 of 18 byte-exact ES2022+ files with maps; 152 of 156 for all
    targets emitted at ES2022.
- The E2 block ends with a `declaration maps` line: `.d.ts.map` outputs whose segments
  equal tsc's, over the bytes-exact `.d.ts` outputs with a map baseline (and over all), with
  up to 10 misses listed. Measured on `m8/dts-followup`: 16 / 16 (100%), 16 / 20 over all
  (before: 15 / 16).

## Dependencies

`ts_r_span`, `ts_r_config`, `ts_r_diagnostics` (rendering), `ts_r_checker`, `ts_r_history`
(persisted history), `ts_r_sound` (sound mode), `ts_r_watch` (`--watch` file events), `ts_r_resolver` (`--explainFiles`: type reference
and `/// <reference>` targets and package identities, as the program found them). Dev: `ts_r_testing` (memory cap), and for the
emit oracle `ts_r_ast`, `ts_r_parser`, `ts_r_codegen` (shapes, source maps) and
`ts_r_transform` (the `transpile` emitter).

## Tests

`tests/cli.rs` runs the binary on `tests/fixtures/<project>/` (both `--pretty` modes, exit
code) against stored tsc 7.0.2 output in `expected/`; regenerate with
`TS_R_TSC=<tsc> cargo test -p ts_r_check --test cli -- --ignored regenerate` (tsc runs on a
temp copy). Fixtures that check a program need the lib files and are skipped without them
(`NEEDS_LIBS`). Compare against tsc on a real project with
`scripts/limit.sh --mem-mb 4096 -- target/release/ts-r-check -p <dir>/tsconfig.json --pretty false`
and `npx -y -p typescript@7.0.2 tsc -p <dir> --pretty false`.
