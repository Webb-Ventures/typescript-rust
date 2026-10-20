# 0012: ts-r-check as a full tsc: emit, declarations, --build and --watch

Status: Accepted 2026-10-19 by Riley Webb
Owners: `ts_r_emit` (new: program emit, output paths, writer, declaration emit),
`ts_r_projects` (new: project-reference graph, up-to-date checks, `.tsbuildinfo`, the `--build`
driver), `ts_r_watch` (new, extracted from `ts_r_hmr`), `ts_r_check` (CLI);
changes in `ts_r_transform`, `ts_r_codegen`, `ts_r_checker`, `ts_r_config`, `ts_r_history`

## Context

`ts-r-check` today is `tsc --noEmit`: it checks, prints tsc's diagnostics and exits 0/1/2, but
never writes a file (its AGENTS.md says so, and exit 2 currently claims "outputs generated" when
none were). The plan's tool table promises more: "the same CLI flags, `.js`, `.d.ts` and source
maps". Libraries need `.d.ts` files, monorepos run `tsc -b`, and many apps still ship `tsc`
output. Until `ts-r-check` emits, no project can drop tsc.

The specification is **tsc 7.0.2's observed behaviour** (`bench/node_modules/.bin/tsc`, or
`npx -y -p typescript@7.0.2 tsc`), never its source. The pieces we reuse already exist:
`ts_r_transform` (all TS-specific transforms, ES2022+), `ts_r_codegen` (printer and V3 maps),
`ts_r_decls` (explicit declaration surfaces), `ts_r_checker` (types and the type printer),
`ts_r_history` (0005), `ts_r_config` (`--showConfig`, option checks) and the dev server's
watcher.

### Observed (tsc 7.0.2, probes run 2026-10-19 under `scripts/limit.sh`)

| Area | Observation |
|---|---|
| Targets | `es2015` (`es6`) to `es2025` and `esnext` accepted. `es5`: TS5108, checking skipped, **emit still happens** (lowered like ES2015, CommonJS), exit 2. `es3`: TS6046, exit 1, nothing written. |
| Lowering | At `es2015`, tsc lowers `async`/`await` (`__awaiter` plus generators), `for await` (`__asyncValues`), class fields into the constructor, `#private` (`WeakMap` plus `__classPrivateFieldGet`), `??` and `?.` (temporaries), object spread (`Object.assign`) and `**` (`Math.pow`). BigInt below ES2020 reports TS2737 and is emitted unchanged. |
| Modules | `commonjs`, `es2015`/`es2020`/`es2022`/`esnext`, `node16`/`node18`/`node20`/`nodenext`, `preserve`. `amd`/`umd`/`system`: TS5108. `none`: TS6046. |
| `outFile` | TS5102, and emit continues **as if it were unset** (one `.js` per input), exit 2. |
| `rootDir` | Defaults to the tsconfig's directory. If the common source directory differs and `rootDir` is not set, tsc reports TS5011 ("... must be explicitly set ... aka.ms/ts6") and still emits, with paths relative to the config directory (`out/src/a.js`). |
| Extensions | `.ts`→`.js`, `.mts`→`.mjs`, `.cts`→`.cjs`, `.tsx`→`.jsx` under `jsx: preserve`; declarations `.d.ts`/`.d.mts`/`.d.cts`. |
| Writer | `emitBOM` writes `EF BB BF`. `newLine: crlf` applies to the whole file. The `//# sourceMappingURL=` line has no trailing newline. Blank lines from the source are dropped. **Line breaks inside lists are kept from the source** (`{ x: 1,\n    y: 2 }` stays on two lines). Literals keep their raw text. A comment in front of an erased declaration is dropped. |
| Source maps | `{"version":3,"file":"a.js","sourceRoot":"","sources":["../a.ts"],"names":[],"mappings":…}`. `sourceRoot` gains a trailing `/` and makes `sources` relative to the root directory. `mapRoot` changes only the URL (`/mr/src/a.js.map`). `inlineSourceMap` writes a base64 data URL, and `inlineSources` adds `sourcesContent`. |
| Listing | `--listEmittedFiles` prints `TSFILE: <abs path>` per file, in program order, and for each file in the order `.js.map`, `.js`, `.d.ts.map`, `.d.ts`. |
| Errors and emit | Semantic and syntax errors do not stop emit (exit 2). `noEmitOnError`: nothing is written, exit 1. `--noEmit` with errors: exit 1. |
| Declarations | Declaration diagnostics (TS4094 and the rest) are reported **even under `--noEmit`** when `declaration` is on. The file's `.d.ts` is skipped but its `.js` is written, and the exit code is 1. `isolatedDeclarations` reports TS9007/9010/9013/9016/9039 and skips those `.d.ts` files. `noCheck` still prints inferred types (`readonly [number, "x"]`). |
| `--build` | Flags: `--verbose/-v --dry --force/-f --clean --builders --stopBuildOnErrors`. `-p`: TS5072. `--clean --verbose`: TS6370. A command-line `--noEmitOnError`: TS5094. A reference cycle: TS6202, exit 4. A missing project: TS6053, exit 1. By default, downstream projects **still build when upstream has errors** (exit 2). With `--stopBuildOnErrors` they are skipped ("can't be built because its dependency 'core' has errors"), still exit 2. |
| Up to date | Status messages (verbose and dry): missing output, "output … is older than input …", "newest input … is older than output …", "up to date with .d.ts files from its dependencies" plus "Updating output timestamps", "up to date but needs to update timestamps of output files that are older than input files" (mtime changed but the hash did not), "buildinfo … indicates that program needs to report errors" / "… change in compilerOptions" / "… some of the changes were not emitted", and a version mismatch. |
| `.tsbuildinfo` | JSON: `version` ("7.0.2"), `root`, `fileNames`, `fileInfos` (32-hex text hashes; `signature` = hash of the file's `.d.ts`; `affectsGlobalScope`, `impliedNodeFormat`), `fileIdsList`, `referencedMap`, `options`, `affectedFilesPendingEmit`, `emitSignatures`, `latestChangedDtsFile`, `errors`, `checkPending`. Written under `outDir` (`out/tsconfig.tsbuildinfo`) for `incremental` and `composite`. |
| Reporting | `--diagnostics` and `--extendedDiagnostics` print the same block (Files, Lines, Identifiers, Symbols, Types, Instantiations, Memory used/allocs, Config/BuildInfo read/Parse/Bind/Check/Emit/Changes compute/Total time). `--listFiles` prints absolute paths in program order, libs included. `--listFilesOnly` lists the files and exits 0 without checking. `--explainFiles` prints paths relative to the cwd with indented reasons. Watch mode prints `ESC[2J ESC[3J ESC[H` (unless `--preserveWatchOutput`), then `HH:MM:SS AM - Starting compilation in watch mode...`, `File change detected. Starting incremental compilation...`, `Found N errors. Watching for file changes.` |
| Exit codes | 0 success; 1 diagnostics and outputs skipped; 2 diagnostics and outputs generated; 4 reference cycle. Upstream baselines tally 788 / 301 / 345 / 2. |

### Oracles available (`target/upstream/typescript/tsc/testdata/baselines/reference`)

- `submodule/{compiler,conformance}/*.js`: 11,714 emit baselines (input files followed by tsc 7's
  outputs; CRLF and LF mixed), of which 1,894 contain `.d.ts` output. There are also 140
  `.js.map` and 145 `.sourcemap.txt` baselines. `submoduleAccepted/` holds 368 `.js.diff` files
  where tsc 7 deliberately differs from 5.x, and tsc 7's output wins. `compiler/` and
  `conformance/` add 217 tsc-7-only tests.
- Scenario baselines: `tsc/` (191), `tsbuild/` (186), `tscWatch/` (41), `tsbuildWatch/` (65).
  Each lists the input virtual file system, the command line, `ExitStatus::`, the output text,
  and every file written (with a readable dump of `.tsbuildinfo`).
- Real projects: the transform's project oracle (private-app, 339 files), `bench/repos.toml`
  and the ecosystem harness's top packages (`cargo xtask ecosystem`).

## Decision

### 1. JS emit

**Pipeline per program.** Load and check exactly as today, then run `emit(program, &EmitOptions)`
in `ts_r_emit`. For every emittable source file (a root or included file that is not a
declaration, not from `node_modules` (`is_external_library_import`) and not JSON unless the
JSON case below applies), the emitter:

1. copies the file's AST arena (one `Vec` copy; the checker's AST stays immutable),
2. runs `ts_r_transform::transform` with options from the tsconfig and an `EmitResolver`,
3. prints with `ts_r_codegen` in the new `Layout::Tsc`, adding a source map when asked,
4. hands the bytes to the writer.

**`EmitResolver`** is a new trait in `ts_r_transform`. It holds the cross-file facts tsc's emit
needs from the checker:

```rust
pub trait EmitResolver: Sync {
    fn is_value_alias(&self, file: FileId, alias: NodeId) -> Answer;      // import/export elision
    fn const_enum_value(&self, file: FileId, access: NodeId) -> Option<ConstValue>; // cross-file inlining
    fn metadata_type(&self, file: FileId, ty: NodeId) -> MetadataType;      // emitDecoratorMetadata
    fn is_referenced_alias(&self, file: FileId, decl: NodeId) -> Answer;   // import x = N.y
}
pub enum Answer { Yes, No, Unknown }
```

`SyntacticResolver` (today's behaviour) stays the default for ts-r-run, the dev server and the
bundler. `ts_r_checker` implements the trait on `Program`. If the resolver answers `Unknown`
(a checker gap), the file's output is **not written**: TSR2002 names the construct
and the exit code is 1. We never guess, because a wrongly elided or kept import fails at
runtime.

**Output paths** (`ts_r_emit::paths`, pure functions over `CompilerOptions`):

- `rootDir`, explicit or else the config directory. The common source directory is computed
  only to report TS5011, and emit continues.
- `outDir` and `declarationDir` rebase relative to `rootDir`. Without `outDir`, outputs go
  next to the source, and TS5055 comes from `emit_overwrite_diagnostics()` as today.
- Extensions follow the table above.
- `outFile` is treated as unset, after TS5102 from `ts_r_config`.
- A JSON file in the program is copied to `outDir` only when the output path differs from the
  input (to confirm by fixture before implementing).
- A failed write reports TS5033 at the end, like tsc.

**Options.**

- `module`, `moduleDetection`, `verbatimModuleSyntax`, `isolatedModules`, `preserveConstEnums`,
  JSX modes (`preserve`, `react-native`, `react`, `react-jsx`, `react-jsxdev`) and
  `importHelpers`/`noEmitHelpers` go through `TransformOptions::from_compiler_options`.
- Under `preserve` and `react-native`, codegen prints JSX as written.
- Two known gaps must close for emit: `importHelpers` with CommonJS (`tslib_1.__x`), and the
  `module: preserve` `export {}` marker. The bundler keeps the marker through a new
  `TransformOptions::bundler_markers` field, set by `ts_r_bundler` only.
- `removeComments`, `newLine` and `emitBOM` are writer and codegen options.
- `sourceMap`, `inlineSourceMap`, `inlineSources`, `sourceRoot` and `mapRoot` extend
  `SourceMapOptions` with tsc's path rules from the table above.

**Downlevel targets.** tsc 7 lowers down to ES2015, so ES2015 to ES2021 are in scope (open decision 3
below). `downlevel.rs` becomes one pass per feature year, run after the TS transforms. Each
lowering uses tsc's exact helper text (captured from output, as `helpers.rs` already does).
A row's syntax is lowered when the target is below that year:

| Year | Syntax lowered |
|---|---|
| ES2016 | `**` |
| ES2017 | `async` functions |
| ES2018 | async generators, `for await`, object rest and spread |
| ES2019 | optional `catch` binding |
| ES2020 | `?.` and `??` |
| ES2021 | logical assignment |
| ES2022 | class fields, `#private`, static blocks, `#x in o` |
| ESNext | `using` |

Each pass lands with fixtures and with the matching `(target=…)` baseline variants. Until a
pass lands, its target reports TSR2003 and writes nothing (never silently unlowered
output). 0002's "ES2022 and later only" and `downlevel::MIN_TARGET` move with each pass.

**Equivalence gate.** Users diff `dist/`, publish it and review `.d.ts` changes, so the
target is **byte equality** with tsc's output. The only normalisation is line endings of the
baseline file itself, to the test's `newLine`. Shape equality (today's oracle) is the floor.
To reach bytes, `ts_r_codegen` gets `Layout::Tsc`:

- 4-space indent.
- Blank lines dropped.
- List line breaks taken from the source: a list prints multi-line where the original node
  had a newline between its open token and first element, or between elements. This is
  computed from spans and the line table, with no new AST field.
- Synthesized nodes use fixed per-construct rules (`{ value: true }` on one line, enum IIFEs
  multi-line), each proven by a fixture.
- tsc's comment attachment: leading and detached comments are kept, and comments of erased
  nodes are dropped.

`Readable` and `Compact` are unchanged for the other tools.

**Writing.**

- Outputs are buffered per file, and written only after all diagnostics are known
  (`noEmitOnError`). When no diagnostic can stop the writes (no `noEmitOnError`, not an
  incremental build), ts-r-check may emit and write next to the check (E2 follow-up).
- Writes are direct, without temp-and-rename, so watchers of `dist/` see one change.
- Every emitted file is written even when its bytes are unchanged, as tsc does, because
  make-like tools rely on mtimes. In incremental and `--build` modes only affected files are
  emitted, as tsc does.

### 2. Declaration emit

`ts_r_emit::dts` builds a **new** declaration AST from the original (untransformed) tree and
prints it with `ts_r_codegen` (`Syntax::TypeScript`, `Layout::Tsc`). It has two layers:

1. **Syntactic.** Explicit surfaces are copied: signatures, annotations, heritage, enum
   members (constant-folded), `declare` and modifiers. Bodies, initializers of typed
   declarations and private member types are dropped. `#private` collapses to `#private;`,
   and `export {}` is added where tsc adds it. `stripInternal` removes `/** @internal */`.
   This is the whole emitter under `isolatedDeclarations`, which also reports TS9005 to TS9039
   at the spots `ts_r_decls` marks `explicit = false`. That crate's notion of "explicit" is
   reused, so the two cannot drift (95% of private-app's entries are explicit).
2. **Inferred.** For each missing annotation the emitter asks the checker:
   `program.type_to_type_node(ty, enclosing: (FileId, NodeId), DeclFlags, &mut AstBuilder)
   -> Result<NodeId, NotNameable>`. This is a new function next to `print.rs`, sharing its
   symbol-accessibility code: the shortest accessible entity name, `typeof x`,
   `import("./m").T` when no import is in scope, literal widening for mutable locations,
   `readonly` tuples for `as const`, `unique symbol`, and no truncation. A
   `NotNameable { code, args }` result becomes tsc's diagnostic: TS4xxx (private name,
   anonymous class with private members, TS4094) or TS2742 (inferred type cannot be named
   without a non-portable reference).

The checker must give these diagnostics **whenever `declaration` is on, `--noEmit`
included**. That fixes a check-parity gap: ts-r-check reports none today.

**Hard rule.** If any type the emitter would print is `TypeId::UNRESOLVED` (0004's silent
fallback), the file's `.d.ts` is **not written** and TSR2001 says where. A wrong `.d.ts`
breaks every consumer, so printing `any` in its place is never acceptable.

**Options.**

- `declaration` and `emitDeclarationOnly`.
- `declarationDir`, through the same path rules.
- `declarationMap` maps declaration names and signatures to their source positions. Inferred
  type nodes carry no mapping, matching tsc's maps (`"mappings":"AACA,eAAO,MAAM,CAAC,IAAI,CAAC"`).
- Declaration diagnostics skip only that file's `.d.ts` and make the exit code 1.

**Oracle.** The 1,894 baselines with `.d.ts` output (byte equality), the `tsbuild/declarationEmit*`
scenarios, and real projects. On real projects, besides bytes, a **consumer test**: check each
importing project once against tsc's `.d.ts` files and once against ours. The diagnostics
must be identical.

### 3. `--incremental`, `--build`, project references, `.tsbuildinfo`

**Proposed: keep our own state, honour tsc's file locations and up-to-date semantics, and do
not read or write tsc's tsbuildinfo schema.**

- **Location.** ts-r writes a `.tsbuildinfo` exactly where tsc would (`tsBuildInfoFile`, or
  `<outDir or config dir>/<config name>.tsbuildinfo`), for `incremental` and `composite`
  projects. Tools that cache `dist/` (Turborepo, Nx, CI artifact caches) therefore carry it,
  and `--clean` removes it.
- **Contents.** Our own JSON schema with `"version": "ts-r <x.y.z> (tsc 7.0.2)"`:
  - roots and file names relative to the buildinfo's directory, each with its BLAKE3 text hash,
  - the `.d.ts` signature hash per file (BLAKE3 of the emitted `.d.ts` bytes),
  - the hash of the check- and emit-affecting options,
  - `errors`, `pendingEmit` (files whose outputs were not written), `latestChangedDtsFile`,
  - the output list (for `--clean`),
  - the 0005 history manifest key.

  Keys come in a fixed order and lists are sorted. There are no timestamps: mtimes are read
  from the file system, never stored.
- **Interop.** tsc reading our file sees a foreign `version` and rebuilds the project once.
  This path is defined and tested upstream (`rebuilds-completely-when-version-in-tsbuildinfo-doesnt-match-ts-version`).
  ts-r reading tsc's file (`"version":"7.0.2"`) treats the project as out of date, also once.
  Mixed teams pay one rebuild per tool switch, never a wrong skip.
- **Up-to-date check** (`ts_r_projects::status`), in tsc's order and with tsc's message text:
  1. missing buildinfo,
  2. foreign version,
  3. options hash changed,
  4. `errors` or `pendingEmit` set,
  5. an input newer than the buildinfo:
     - if its text hash is unchanged, the status is "up to date but needs to update
       timestamps", and the outputs are touched;
     - otherwise rebuild.
  6. an upstream `latestChangedDtsFile` newer than our buildinfo means rebuild. If upstream
     rebuilt but its `.d.ts` files did not change, the status is "up to date with .d.ts files
     from its dependencies" and the outputs are touched.

  Inputs are all loaded source and declaration files (including roots and imported
  dependencies), the tsconfig and its `extends` chain.
- **Check history.** A project that is out of date is checked through 0005's history, so
  `-b` gets per-symbol incremental checking, which tsc's per-file `referencedMap` cannot.
  When the ts-r cache is missing (a fresh CI machine with restored `dist/`), the project gets
  a full check. That is correct and only slower.
- **Driver** (`ts_r_projects::build`):
  - Load the reference graph from the given projects, find cycles (TS6202, exit 4), and order
    the projects topologically. Missing projects report TS6053.
  - Build independent projects in parallel. `--builders N` caps it (default 4, max 4 under
    the machine rules).
  - Downstream projects still build after upstream errors unless `--stopBuildOnErrors`.
  - References resolve to the upstream's output `.d.ts` files, which needs the resolver's
    `outDir`→source mapping (a listed gap). `composite` adds TS6304/TS6307 and the
    "file not listed" checks.
  - `--verbose` prints the "Projects in this build" block and each status line. `--dry`
    prints "A non-dry build would …" and writes nothing.
  - `--force` ignores the status. `--clean` deletes the recorded outputs and the buildinfo,
    and refuses `--verbose` (TS6370).
- **Option classes.** `ts_r_config`'s static `OPTIONS` table gains an `affects` bitset
  (`CHECK`, `EMIT`, `DECL_EMIT`, `OUTPUT_PATHS`, `BUILDINFO`), taken from the
  `tsbuild/*/when-*-option-changes` scenarios and probes. 0005's history key then uses only
  `CHECK` options, so changing `outDir` or `sourceMap` no longer throws away check history.
  Emit cache keys use the emit classes.

### 4. `--watch` and the reporting flags

- **Watcher.** `ts_r_hmr/src/watch.rs` (notify, the pure `Debouncer`, the macOS kqueue fast
  path, dropped FSEvents replay) moves to a new `ts_r_watch` crate. `ts_r_hmr`, `ts_r_run`
  and `ts_r_lsp`'s `tsr/watch.rs` use it too, so `ts_r_check` does not link an HTTP server.
- **What `-w` watches:**
  - the directories of program files,
  - the include wildcard directories (re-globbed on create and delete),
  - the tsconfig and its `extends` chain,
  - the `package.json` files that decided module formats,
  - resolution failure locations, once `ts_r_resolver` records them (a listed gap).
- **On change.** Re-check in memory, keeping the `Program`'s unchanged ASTs and `BoundFile`s
  (as the LSP does), then re-emit only the affected files. The output follows the watch
  messages above. `-b -w` watches every project in the graph.
- **`--listFiles`, `--listFilesOnly`, `--listEmittedFiles`, `--explainFiles`, `--showConfig`**
  (already in `ts_r_config`), `--diagnostics` and `--extendedDiagnostics` print tsc's exact
  formats. Only formats and labels are gated. Counts such as Types and Symbols are ours and
  will differ.
- **`--traceResolution`** needs trace output in `ts_r_resolver` (a later phase).
- **Flag surface.** We accept every flag `tsc --all` lists, with tsc's aliases. A new
  `ts_r_config::cli` module holds tsc's command-line parser: types, `--flag false`, lists,
  build-mode aliases where `-v` and `-d` change meaning, and TS5023/TS5072/TS5094/TS6046/TS6369
  errors. `--showConfig`, the option checks and history keys all see the merged options, as
  today.
- **Flags with no ts-r implementation:**
  - Profiling flags (`--generateTrace`, `--generateCpuProfile`, `--pprofDir`) print one TSR2005
    warning and are otherwise ignored.
  - `--checkers`, `--builders` and `--singleThreaded` map to our thread count.
  - `--locale` other than `en` gives TSR2005.
  - `--init` writes tsc 7's template (captured output).
  - Any flag that would change output and is not implemented is a **TSR2004 error**, never
    ignored.

### 5. Crates, threading, caching, determinism

| Crate | Role |
|---|---|
| `ts_r_emit` (new) | `emit(&Program, &EmitOptions, &dyn Writer) -> EmitResult`, plus `paths`, `dts`, `writer` (BOM, newline, `MemoryWriter` for tests), `listing` (TSFILE order), and the emit cache. Depends on `ts_r_transform`, `ts_r_codegen`, `ts_r_checker`, `ts_r_decls`, `ts_r_config` and `ts_r_cache`. |
| `ts_r_projects` (new) | Reference graph, status, buildinfo codec, the build and clean driver. Depends on `ts_r_emit`, `ts_r_history` and `ts_r_config`. |
| `ts_r_watch` (new) | The watcher and debouncer moved from `ts_r_hmr`. The `notify` dependency moves with it. |
| `ts_r_check` | CLI only: argument parsing through `ts_r_config::cli`, choice of mode (`-p`, `-b`, `-w`), reporting. |
| `ts_r_transform` | `EmitResolver`, downlevel passes, `bundler_markers`. |
| `ts_r_codegen` | `Layout::Tsc`, `SourceMapOptions` path fields. |
| `ts_r_checker` | Implements `EmitResolver`; `type_to_type_node`; declaration diagnostics. |

- **Threading.**
  - Emit runs after the check on the same worker count (≤ 4, `--checkers`).
  - Files are handed out by an atomic counter, as in `check_program`. JS and `.d.ts` emit of
    one file run in the same task.
  - The checker's shared state is read-only by then, apart from the write-once caches.
  - One writer thread drains finished files in any order. Listing and diagnostics are sorted
    afterwards.
- **Emit cache** (`ts_r_cache` namespace `emit`, versioned). It matters because a warm history
  run never checks unchanged files, yet a non-incremental `-p` must still write every output.
  - JS key: tool build, emit-option key, the relative output path, the source text hash, and
    a digest of the `EmitResolver` answers the transform asked for (recorded during the
    transform).
  - `.d.ts` key: tool build, decl-option key, the relative output path, the text hash, and
    the 0005 deep hashes of the file's uses.

  Payloads contain no absolute paths, except where tsc's own output does (`mapRoot`).
- **Determinism.**
  - Output bytes depend only on the inputs, the options and the tool build.
  - One thread and four threads give identical bytes over all baselines (a CI test).
  - Diagnostics and `TSFILE:` lines come in program order.
  - The buildinfo is sorted and has no timestamps.
  - Watch-mode timestamps (`HH:MM:SS AM`, local time) appear only in terminal output, never
    in anything cached.
- **Diagnostic codes.** Emit takes `TSR20xx` from the range 0007 reserves (`TSR16xx` to
  `TSR99xx`), in the same `100_000 + n` encoding:

  | Code | Category | Meaning |
  |---|---|---|
  | TSR2001 | error | inferred type unresolved in a declaration; `.d.ts` skipped |
  | TSR2002 | error | `EmitResolver` answered `Unknown`; output skipped |
  | TSR2003 | error | lowering for this target not implemented |
  | TSR2004 | error | output-changing flag not implemented |
  | TSR2005 | warning | flag accepted and ignored (profiling, `--locale`); exit code unchanged |
  | TSR2006 | error | `.d.ts` for a JavaScript input not supported |

## Invariants agents must keep

1. tsc 7.0.2's observed output is the spec. A behaviour change needs a fixture or baseline
   proving tsc does it. Byte equality is the target; shape equality never regresses
   (`KNOWN_GAPS` only shrinks).
2. Never write a wrong output silently. An `Unknown` resolver answer, an `UNRESOLVED` type in
   a declaration, an unimplemented lowering or an unimplemented output-changing flag skips
   that output with a TSR20xx error and exit 1.
3. Exit codes are tsc's: 0, 1 (outputs skipped), 2 (outputs generated), 4 (reference cycle).
   Exit 2 is only returned after outputs were actually written.
4. `noEmitOnError` with any error writes nothing. `--dry` writes nothing. `--clean` removes
   only files ts-r recorded or tsc's rules name.
5. Declaration diagnostics are reported whenever `declaration` is on, `--noEmit` included.
6. Emit never mutates the checker's ASTs. It works on a copy.
7. Emit and buildinfo output are identical with 1 and N threads. The buildinfo contains no
   timestamps or absolute paths.
8. ts-r never writes a buildinfo that claims tsc's `version`. A tsc-written buildinfo never
   makes ts-r skip work.
9. History keys use `CHECK`-class options only. Every emit cache key covers every option in
   its class.

## Alternatives

- **Read and write tsc's `.tsbuildinfo` exactly.** It gives zero rebuilds when switching
  between tsc and ts-r, but costs a lot:
  - We would have to match tsc's unidentified 128-bit text hash.
  - `signature` is a hash of tsc's `.d.ts` bytes, so interop only works once our `.d.ts`
    output is byte-exact.
  - tsc trusts `semanticDiagnosticsPerFile`, `affectedFilesPendingEmit` and `errors` from the
    file, so one wrong field makes tsc skip needed work.
  - The schema changes between tsc versions.

  This could come back after `.d.ts` output is byte-exact (open decision 1).
- **Keep all state in `~/.cache/ts-r` and write no buildinfo.** Simpler, but `tsc -b`
  semantics (the up-to-date stamp travelling with `dist/`, `--clean`, tools that look for the
  file) break.
- **Accept shape equality as the gate (as in 0002).** Cheaper, but `dist/` diffs and API
  reports (`.d.ts` reviews) would show noise on every migration, and users would not trust
  the switch.
- **Declaration emit from type strings** (`type_to_string` pasted into text). It fails on
  nested multi-line object types (tsc indents them at the use site) and on declaration maps.
  Building nodes lets codegen own layout.
- **Put emit inside `ts_r_check` or `ts_r_checker`.** The LSP (emit on save, "go to source"
  maps) and `ts-r ci` need emit as a library, and the checker should not depend on
  transform and codegen.
- **Reuse `ts_r_hmr` for watching as a dependency.** It links an HTTP/WebSocket server into
  `ts-r-check`. Extracting it moves one 747-line file (`watch.rs`).

## Plan (milestone M8; one branch per phase, stacked as AGENTS.md describes)

| Phase | Branch | Delivers | Gate |
|---|---|---|---|
| E0 | `m8/emit-harness` | `emit_oracle` harness over the submodule baselines (bytes and shape per file, `--filter`, cause buckets); `ts_r_config::cli` with the full flag table; truthful exit codes; option `affects` classes | Harness reports today's byte-exact rate at ES2022+. Every `tsc --all` flag parses. The `ts_r_check` CLI tests pass. |
| E1 | `m8/emit-js` | `ts_r_emit` JS: paths, writer, maps, `noEmit`/`noEmitOnError`/`emitDeclarationOnly`, `--listEmittedFiles`, `EmitResolver` on the checker, `Layout::Tsc` | Supported JS baselines: shape 100% (`KNOWN_GAPS` listed), bytes ≥ 90%. `.sourcemap.txt` decoded segments equal on ≥ 90% of byte-exact files. private-app: identical set of written files, 0 shape mismatches. 1-vs-4-thread test passes. |
| E2 | `m8/emit-dts` | Syntactic `.d.ts` plus `isolatedDeclarations` (TS9xxx), then `type_to_type_node`, TS4xxx/TS2742, `declarationMap`, `declarationDir`, `stripInternal` | Bytes ≥ 85% of the 1,894 `.d.ts` baselines. Consumer test identical on private-app and 20 ecosystem packages. 0 `.d.ts` files written with an unresolved type. |
| E3 | `m8/tsbuild` | `ts_r_projects`: buildinfo, `--incremental`, `-b` with all build flags, statuses, parallel builders, `ts_r_resolver` `outDir` mapping | ≥ 85% of the `tsc/` and `tsbuild/` scenarios match (output text with timestamps normalised, exit status, set and content of written files; buildinfo content excluded). The 0005 differential test extended to emit: after random edits, outputs are byte-equal to a clean build. |
| E4 | `m8/watch` | `ts_r_watch` extraction; `-w` for `-p` and `-b`; `--listFiles`, `--listFilesOnly`, `--explainFiles`, `--diagnostics`, `--extendedDiagnostics`; `--traceResolution` | ≥ 80% of the `tscWatch/` and `tsbuildWatch/` scenarios match. `ts_r_hmr` tests unchanged. |
| E5 | `m8/downlevel` | ES2021 → ES2015 passes, newest year first | Shape ≥ 95% on the `(target=es20xx)` variants. Lowered fixtures run under Node and print the same output as the ESNext originals. |
| E6 | `m8/emit-hardening` | Real repos: `bench/repos.toml` and the ecosystem packages that build with `tsc -b` | `dist/` byte-identical for ≥ 95% of files. Cold `-p` with emit ≤ 1.25× our check-only time. Warm no-change `-b` on a 20-project monorepo < 50 ms. Peak RSS ≤ tsc 7's. |

The thresholds are proposals for the owner to set. Later phases never lower an earlier
phase's gate. Each phase records its measured numbers in this record's implementation notes
and in the owning crates' AGENTS.md.

## Risks

- **Printer long tail.** Comment attachment and list layout are where byte equality gets
  hard. Mitigation: the shape gate first, then byte rates tracked per cause bucket in the
  harness. Shape is never traded for bytes.
- **Type printing for declarations** depends on checker coverage (58% of conformance
  variants exact today). Gaps show up as skipped `.d.ts` files with TSR2001, which is safe
  but blocks adoption for libraries. E2's syntactic layer covers explicit-API and
  `isolatedDeclarations` projects first.
- **Elision needs type information** outside `isolatedModules`/`verbatimModuleSyntax`
  (re-exported types, `import x = N.y`, metadata). Projects using those options never ask the
  resolver.
- **Mixed tsc/ts-r teams** pay one rebuild per switch (alternative 1 removes that later).
  Third-party tools that parse tsc's buildinfo fields would not understand ours. None are
  known in the ecosystem list; E6 checks.
- **Downlevel scope** is large (async generators, private fields with helpers). E5 is last,
  and each target is refused until its pass lands.
- **Writes are irreversible.** `--clean` and output paths get fixture tests for symlinked
  outDirs, `rootDir` outside the project, and outputs that would overwrite inputs (TS5055).
- **`allowJs` input emit and JS→`.d.ts`** (JSDoc declaration emit) are not covered here.
  Until a follow-up record covers them, JS inputs are emitted by the JS path only and their
  `.d.ts` is refused with TSR2006.

## Decisions (2026-10-19, Riley Webb)

1. **Buildinfo:** our own schema at tsc's path, with tsc's up-to-date rules. Switching between
   tsc and ts-r costs one rebuild. The 0005 history stays in the ts-r cache, not in the buildinfo
   (revisit in E6 if CI caches of `dist/` need it).
2. **Gate level:** byte equality is the merge target, with the thresholds in the Plan table.
3. **Downlevel:** implement ES2021 down to ES2015 as E5. Until each pass lands, its targets are
   refused with TSR2003.
4. **Drop-in name:** an opt-in `tsc` alias, shipped separately from `@ts-r/check`, so users
   enable it deliberately. `-v` prints the ts-r version plus the tsc version it matches.
5. **Crate split:** three new crates (`ts_r_emit`, `ts_r_projects`, `ts_r_watch`), as proposed.
6. **Phase order:** as proposed, `--build` (E3) before downlevel (E5).

## Status

Proposed 2026-10-19 (branch `m8/emit-design`) from probes of tsc 7.0.2 and the upstream
baselines listed above. No code depends on it yet. On acceptance, three records get a pointer
here:

- [0002](0002-transform-and-codegen.md): the ES2022 floor and the shape-only gate.
- [0004](0004-types-and-checker.md): the query API gains `type_to_type_node` and `EmitResolver`.
- [0005](0005-history.md): the history key uses `CHECK`-class options only.

Accepted 2026-10-19 by Riley Webb, with the decisions above.

## Implementation notes (E0, `m8/emit-harness`, 2026-10-19)

**Emit oracle.** `crates/ts_r_check/examples/emit_oracle.rs` (no new crate in E0; ts_r_check
gains `ts_r_emit` in E1). It runs every test of `tests/cases/{compiler,conformance}` and tsc
7's own `tsc/testdata/tests/cases/{compiler,conformance}` that has a `.js` baseline, one run
per variant, and compares each output the baseline lists: JS and d.ts by bytes and by AST
shape, maps by `.js.map` JSON bytes and `.sourcemap.txt` segments, JSON copies by bytes.
Observed baseline format: after the title, one section per unit except `tsconfig.json`, then
the outputs (base names, full paths with `@fullEmitPaths`); outputs of a group are
concatenated directly and the harness adds `\r\n\r\n` before the first declaration and before
`DtsFileErrors`; it writes CRLF unless the test sets `newLine`. Most tests set
`@target: es2015`, so the ES2022+ row is about a fifth of the outputs.

| Emitter | Outputs | Bytes-exact | Shape-equal | ES2022+ bytes-exact |
|---|---|---|---|---|
| `check` (what ts-r-check writes today) | 17,347 | 0 (0.0%) | 0 (0.0%) | 0 / 3,206 (0.0%) |
| `transpile`, all kinds | 17,347 | 5,802 (33.4%) | 11,604 (66.9%) | 911 / 3,206 (28.4%) |
| `transpile`, JS only | 14,476 | 5,802 (40.1%) | 11,600 (80.1%) | 911 / 2,159 (42.2%) |

11,925 variants (1,711 tests have no `.js` baseline, 8 baselines are UTF-16). d.ts (2,635),
JSON (33) and map bytes are 0% for both emitters. The `transpile` emitter (per-file
`ts_r_transform::transpile`, outputs next to their roots) is E1's starting point, not a
gate: its largest JS causes are shape-equal layout (line breaks and spacing 3,923, comments
1,811), outputs outside the root's directory (`outDir`, JS inputs) 701, CommonJS 307,
lowering temporaries 287, class static blocks 127 and `__awaiter` 121 (the last three are
below-ES2022 lowering, E5).

**Command line.** `ts_r_config::cli`: `FLAGS` holds the 137 flags `tsc --all` lists (order,
sections, aliases; checked against tsc's own output in
`crates/ts_r_config/tests/fixtures/tsc-7.0.2-all.txt`) plus `--ignoreDeprecations`, which tsc
accepts without listing. Every flag parses in every spelling (`--x`, `-x`, any case, aliases)
with every listed value. Observed and implemented: no `--flag=value` form (TS5023 names the
argument as written), booleans take only an exact `true`/`false`, strings take the next
argument whatever it is, lists skip a dashed or empty one, `null` clears any option,
`paths`/`plugins`/`rootDirs` only take `null` (TS6064), the last occurrence wins, `@file`
response files (TS5083), `--build` only first (TS6369; a later `-b` is TS5023), build-mode
aliases `-v`/`-d`/`-f` (TS5072, TS5073, TS5094; build flags outside it TS5093), TS5002,
TS5080, TS6044, TS6046, TS6048, and TS6370 after the parse errors. A command-line error
prints and exits 1 before anything else, as tsc does.

**`affects` classes.** Every option in `OPTIONS` carries `CHECK`, `EMIT`, `DECL_EMIT`,
`OUTPUT_PATHS` and `BUILDINFO` bits, from three probe passes against tsc 7 (does an
incremental re-check after flipping the option redo type work; does it change the bytes of
the `.js`/map, `.d.ts` or the set of files written; is it recorded in the `.tsbuildinfo`)
plus conservative reasoning for program-structure options (`moduleResolution`, `paths`,
`types`, ...: CHECK). A missing bit is a stale cache, an extra one only a miss, so doubtful
cases get the bit. `DECL_EMIT` does not include inferred types: a declaration cache key
includes `CHECK` too. The history key still hashes every option; switching it to
`CHECK`-class options (the 0005 pointer) is left to the phase that adds the emit cache (E1),
and keeping more options in the key is the safe direction.

**ts-r-check.** Arguments go through `ts_r_config::cli` after ts-r's own flags are taken
out. Exit codes are 0 without errors and 1 with errors: nothing is written until E1, so 2
is never returned (the CLI fixtures map tsc's stored 2 to 1). `-v` prints
`Version 7.0.2 (ts-r 0.0.0)`: tsc's line first, so scripts matching `Version x.y.z` keep
working under the opt-in `tsc` alias (decision 4), then the ts-r version; it wins over
`--help`. Not implemented yet and a TSR2004 error (exit 1, nothing checked): `--build`
(E3), `--watch`, `--listFiles`, `--listFilesOnly`, `--explainFiles`, `--diagnostics`,
`--extendedDiagnostics`, `--traceResolution` (E4), `--init` and `--noCheck`, and files named
on the command line (TS5112 / TS5042 are reported first, as tsc does). TSR2005 warnings:
`--generateTrace`, `--generateCpuProfile`, `--pprofDir`, a non-`en` `--locale`. `--quiet`,
`--showConfig`, `--checkers`, `--singleThreaded` and `--listEmittedFiles` (prints nothing)
work. TS5058 now prints the absolute path and a directory without `tsconfig.json` is TS5081,
both as observed; without any project ts-r-check prints its help and exits 1 like tsc.
These flags count as given only on the command line; the same reporting options in a
tsconfig are still accepted silently.

## Implementation notes (E1, `m8/emit-js`, 2026-10-19)

**What landed.** Crate `ts_r_emit`:
- output paths (`outDir`, `rootDir`, `declarationDir`, TS5011, TS6059, TS5055);
- source maps (`maps.rs`: location, `sources`, `sourceRoot`, `mapRoot`, inline forms);
- the `Writer` trait with `FsWriter` and `MemoryWriter`;
- `noEmit`, `noEmitOnError`, `emitDeclarationOnly` and `--listEmittedFiles` (`TSFILE:` lines);
- `CheckerResolver`, the `EmitResolver` on the checker (`Checker::alias_is_value` in `ts_r_checker/src/emit.rs`);
- JS printed with `Layout::Tsc`.

ts-r-check emits unless `noEmit`. Outputs are written only once every diagnostic is known. The
exit code is tsc's: 0 with no errors; 2 with errors when every output was written; 1 with
errors when any output was skipped. A target below ES2022 is refused with TSR2003 (whole
program), as is `using` that needs lowering (per file). A resolver `Unknown` is TSR2002, and
declaration outputs are TSR2004 until E2. Each of these skips the output, so the run exits 1.
The emit oracle's `--emitter program` runs the same emitter over the loaded program.

**Writes.**
- An output path equal to an input is TS5055.
- `FsWriter` also refuses a path whose canonical form is a program input (a symlinked
  `outDir` pointing back into the sources) with TS5033. tsc 7.0.2 overwrites the input in
  that case; this is a deliberate divergence.
- Fixture tests in `ts_r_check/tests/cli.rs` cover a symlinked `outDir`, `rootDir` outside
  the project, outputs over inputs, `noEmitOnError` and `--listEmittedFiles`.
- `ts_r_emit/tests/emit.rs` has the 1-vs-4-thread determinism test and the `tsc_layout`
  project fixtures (tsc 7.0.2 outputs, byte for byte).

**Gates, measured.** Emit oracle, ES2022+ JavaScript outputs (2,159).

| Emitter / state | Bytes-exact | Shape-equal |
|---|---|---|
| E0 `transpile` | 911 (42.2%) | 1,643 (76.1%) |
| `program`, first run (Readable layout) | 1,090 (50.5%) | 1,910 (88.5%) |
| `program` + `Layout::Tsc` (merge of `m8/emit-layout`) | 1,782 (82.5%) | 2,005 (92.9%) |
| `program`, `m8/emit-js` | 1,946 (90.1%) | 2,012 (93.2%) |
| `program`, `m8/emit-maps` (E1 close) | **1,996 (92.5%)** | **2,034 (94.2%)** |

- **Bytes ≥ 90%: met** (92.5%).
- **Shape: 94.2%.** Every one of the 125 outputs that is not shape-equal (147 at
  `m8/emit-js`) is listed with its cause in `crates/ts_r_check/examples/emit_known_gaps.txt`
  (0 unlisted). The gate's 100% holds only over the supported set minus that list. The list
  may only shrink.
- **Source-map segments: met.** Segments are equal on 17 of the 18 byte-exact ES2022+
  outputs that have a map baseline (94.4%). Emitting earlier targets at ES2022, they are
  equal on 152 of 156 (97.4%). `Layout::Tsc` maps follow tsc's observed points: node starts
  and ends, a few tokens, and the transforms' ranges for synthesized code
  (`TransformResult::map_ranges`). The misses are:
  - the TC39 decorator transform (esDecorators-classDeclaration-sourceMap);
  - three JSX-factory tests, where tsc counts generated lines differently.
- **Private-app row, by stand-in.** The private app was not available, so a private
  pnpm/turbo monorepo of the same style stood in (25 tsconfig projects,
  apps/domains/packages, Next.js TSX, CommonJS Pulumi, source maps on).
  - Both tools ran with `--noEmit false`, declarations off, and `--outDir` per project.
  - The written files match: 752 by tsc, 752 by ts-r-check, the same paths, all
    byte-identical (JS and `.js.map`). Shape mismatches: 0.
  - The workspace's shared config packages were not installed, so both tools reported
    TS6053 on `extends` and ran with defaults. The exit codes differ on one project:
    an infrastructure project exits 0 with tsc and 2 with ts-r-check, from a checker false
    positive (TS2322 on a conditional spread into a `fetch` init).
  - The layout rules this found have fixtures in `ts_r_emit/tests/tsc_layout/app_style`:
    - numeric separators printed by value without a `target`;
    - trailing commas in import and export lists;
    - indentation inside JSX expression containers;
    - comments before a broken `.` and before spread elements.
- **Determinism:** `ts_r_emit/tests/emit.rs::one_and_four_threads_agree` passes. 1 and 4
  threads write the same files with the same bytes.
- **Checker gates held:** conformance ALL 12769 / 8729 exact / 236 with false positives
  (unchanged). The transform's `baseline_oracle` is 1,672 match / 100 mismatch.

**Fixes by cause, run 5 (after the layout merge) to final.** Counts are ES2022+ JS outputs
that are not byte-exact. A bucket is named after the first differing line, so a fix can move
an output into another bucket: lowering temporaries 13 → 18 and static blocks 0 → 4 are moves
of this kind. No output that matched in an earlier run stops matching.

| Cause | Run 5 | Final | Fix |
|---|---|---|---|
| shape-equal: comments or tokens | 124 | 55 | Stand-ins keep the same-line comment of the statement they replace; triple-slash directives survive an erased first declaration; empty function and namespace bodies drop comments on the `{` line; a space after a trailing comma before a comment |
| shape-equal: line breaks and spacing | 98 | 9 | Transform layout hints (`single_line` / `multi_line`): accessor pairs, CJS export getters, `_classThis` / `__setFunctionName` static blocks, private forwarders, `__decorate([` lists. Also: tag-template space, `async (x) =>`, `export \n@dec`, trailing comma in named imports, escaped names as written |
| CommonJS | 24 | 19 | `exports.C = C;` after a decorated exported class, `exports.C = C = __decorate(...)` |
| decorators | 7 | 2 | Helper order (`__runInitializers` first when instance members are decorated) |
| JSX | 4 | 2 | A spread that sets `__proto__` is not flattened |
| harness | n/a | n/a | tsc writes no newline after a final `//# sourceMappingURL=`. The baseline harness adds one, so the oracle drops a single one before comparing |

**Remaining causes (final).**
- **At E1 close (`m8/emit-maps`).** Not shape-equal, 125 outputs:
  - `using` lowering (E5): 33
  - other: 25
  - lowering temporaries: 18
  - import/export on invalid syntax: 9
  - classes: 8
  - `__setFunctionName`: 5
  - static blocks: 4
  - CommonJS: 3
  - extra code at the end: 3
  - smaller causes: the rest

  Shape-equal but not byte-exact, 38 outputs:
  - comments or tokens: 28
  - line breaks and spacing: 9
  - other: 1

  The `m8/emit-maps` fixes were:
  - CommonJS: 19 → 3.
    - `module.exports = x;` moves to the end of the file.
    - No `export {}` for `.cts`/`.cjs` under `module: preserve`.
    - A re-exported namespace import is `exports.ns = ns;`.
  - Comments or tokens: 55 → 28.
    - Comments between and after decorators.
    - Decorators print as written.
    - Comments around binary operators and after `...`.
    - Triple-slash directives before a lone `export {}`.
    - `(x as T)` drops its parentheses around inline comments.
    - Bigint literals are normalised.
- **Not shape-equal (147, at `m8/emit-js`):**
  - `using` that needs lowering (TSR2003, E5): 33
  - other: 27
  - CommonJS: 19
  - lowering temporaries: 18
  - import/export on invalid syntax: 11
  - classes: 8
  - `__setFunctionName`: 5
  - static blocks: 4
  - extra code at the end: 4
  - not emitted: 3
  - JSX: 2
  - decorators: 2
  - one or two each:
    - TS5055 inputs: 2
    - missing code: 2
    - one each: const enum inlining, TSR2002, `"use strict"`, enum IIFE, `__classPrivateFieldIn`, arrow functions, missing CommonJS code
- **Shape-equal but not byte-exact (66):**
  - comments: 55. On decorators and between class-expression decorators; before binary
    operators; `/*c*/ x` spacing; stand-ins moved into constructors; triple-slash directives
    before an erased `export {}`.
  - line breaks and spacing: 9
  - indentation: 1
  - line endings: 1

## Implementation notes (E3, `m8/tsbuild`, 2026-10-19)

**What landed.** Crate `ts_r_projects` (its AGENTS.md lists the invariants):
- ts-r's own `.tsbuildinfo` at tsc's path;
- `incremental::plan`, which decides what a build writes;
- project references (TS6306, TS6310, TS6307, TS6377, TS6053) and the resolver's
  source-to-output map;
- the `--build` graph (TS6202 cycles, exit 4);
- tsc's up-to-date statuses with their message text;
- the build driver with every `-b` flag: `--verbose`, `--dry`, `--force`, `--clean`,
  `--stopBuildOnErrors` and `--builders` (parallel builders; output is buffered per project
  and printed in build order).

In ts-r-check:
- `-p` honours `incremental`/`composite` and references;
- files on the command line are the program (`ConfigLoader::command_line`);
- `--noCheck` and `--init` work.

`scenario_oracle` (ts_r_check example) runs tsc 7's `tsc/` and `tsbuild/` scenario
baselines step by step and compares output, exit status, the set of written files and
their contents. Our buildinfo is checked by path only.

**Deviations from the proposal above.**
- **Buildinfo schema.** It has `version`, `root`, `errors`, `semantic_errors`,
  `check_pending` and `program`. `program` holds:
  - option hashes per output kind, with the map options;
  - files with text hash, signature, `global` and dependency indexes;
  - `pending` emit;
  - `latest_changed_dts`;
  - the `package.json` files resolution read, or looked for and did not find.

  The proposed output list and history key are not stored: `--clean` deletes the expected
  outputs computed from the config (`status::expected_outputs`).
- **Signatures.** The hash of the file's `.d.ts` output when one is emitted. Otherwise the
  hash of the declared API (`ts_r_decls` `api_hash`), and the text hash for `.d.ts` inputs.
- **Status order.** It is longer than the six steps proposed. The observed order is in
  ts_r_projects/AGENTS.md. Notable points:
  - an output kind (or its maps) not produced last time is "change in compilerOptions", and
    comes before a newer config file;
  - a non-incremental project reports a dropped root before stored program state;
  - a project with no inputs and either references or an explicit `files` is never built.
    Its config errors (TS18002) still count, as skipped outputs.
- **Composite and `.d.ts` rewrites.** Composite projects leave an unchanged `.d.ts` alone;
  other incremental projects rewrite it (both observed).
- **Checking-only option changes** re-check but re-emit nothing.
- **`noCheck`** reports syntax errors only: the parser's, minus the codes the checker
  reports (`CHECKER_GRAMMAR_CODES`). The buildinfo keeps `errors`, `semantic_errors` and
  `check_pending` apart, and `noCheck` is not part of the check-options hash.
- **Option errors.** Syntax errors replace option errors when both are present (tsc 7's
  order). A reference in error is an option error.
- **TS6377** (a project's buildinfo is a referenced project's). Every output is written, the
  buildinfo is not, and the outputs are touched.

**Gates, measured.**
- **Scenario oracle: not met.**

  | Tree | Matched |
  |---|---|
  | `m8/tsbuild` alone | 120 / 377 (31.8%); 192 failures need `.d.ts` output |
  | Trial merge with `m8/emit-dts` and `m8/emit-maps` | 193 / 377 (51.2%) |

  The 184 trial failures split as: 121 that tag E2, 25 that use E4 reporting flags, and 38
  other.
- **0005 differential test extended to emit: met.** `ts_r_check/tests/incremental_emit.rs`
  makes random edits to ts_r_history's fixture app, carries the tree across the edits, and
  compares every output byte for byte, and the diagnostics, with a clean build. It runs
  three variants:
  - `--incremental -p` with `sourceMap`;
  - `-b` with `removeComments`;
  - `-b` with `composite` + `declarationMap`, once `.d.ts` emit exists.

  Seeds 1-8 × 200 edits pass on the branch. Seeds 1-10 × 200 pass in the trial merge, with
  about 10,000 outputs and 1,300 `.d.ts` per seed.

**Remaining scenario failures by owner (trial merge).**
- **E2.** `.d.ts.map` mappings (about 23 content failures). TS4094 and other declaration
  diagnostics, also under `noEmit`/`noCheck` (about 20). `.d.ts` content (8), including:
  - `preserve="true"` triple-slash references kept with a rewritten path;
  - synthesized module specifiers.

  Also: JS declaration emit (TSR2006, 3), TSR2001 (1), and `--listEmittedFiles` order (a
  `.d.ts.map` is listed before its `.d.ts`).
- **E5.** `target: es5` is removed in tsc 7 (TS5108), which emits ES2015, so about 11
  scenarios need ES2015 output. One more needs ES2017.
- **E4.** `--explainFiles`, `--listFiles` (also from a tsconfig), `--traceResolution` and
  `--extendedDiagnostics` (25).
- **Checker.**
  - No related information yet: TS2551 "declared here" (about 12) and the TS2554 argument note.
  - TS2554's span.
  - TS2318 under `noLib` (also reported next to option errors).
  - TS6053 for a missing `/// <reference path>`.
  - TS1149 and TS1261 casing.
  - TS2613, TS2748, TS2878, TS2769 and TS7016 for `react/jsx-runtime`.
  - TSR2002 on const-enum imports (3; the EmitResolver).
- **Help text (12).** tsc's `--help`, `--all`, `-b --help`, the bare `tsc` banner and the
  localized `--locale cs` text. ts-r-check prints its own help (E0's decision), so these
  stay unmatched unless that decision changes.
- **Accepted deviations.**
  - `tsbuildinfo-has-fewer-fileInfos-than-fileNames` edits tsc's own buildinfo schema.
  - `rebuilds-when-absolute-non-root-dependency-is-updated` uses Windows drive paths
    (`C:/`, `D:/`), which the harness cannot map on a POSIX file system.

**Seams with E2.**
- `ts_r_check/src/build.rs` builds `EmitConfig` without E2's `specifiers` field. The merge
  must add `specifiers: Some(&specifiers)` (with `ts_r_checker::typenode::specifier_host`),
  as `main.rs` does on `m8/emit-dts`.
- After E2 lands, `Effective::declaration()` should become
  `declaration == Some(true) || composite == Some(true)`. An explicit `"declaration": false`
  does not stop a composite project's `.d.ts` files (observed: TS6304, and the files are
  still written). The trial merge carries this change.
- `incremental::declaration_signature` finds `.d.ts` outputs by path suffix, so E2's
  output kinds need no change here.

## Implementation notes (E5, `m8/downlevel`, 2026-10-19)

**What landed.** ts-r-check emits ES2015 to ES2021 (ES5 is lowered like ES2015, as tsc 7
does). A file is refused with TSR2003 only when its transformed tree still holds syntax newer
than the target (`TransformResult::unlowered`, which names the construct); the rest of the
program is written. The lowering lives in `ts_r_transform::downlevel`, after the TypeScript
transform on the same tree:
- `lower.rs`, the one walk: function contexts, hoisted temporaries (one `var` per transformer
  after the prologue, named in print order per function and skipping the file's names),
  unique names (`_1` suffixes), class temporaries reserved in nested functions.
- ES2021, ES2019 and ES2016 (`operators.rs`): logical assignment, numeric separators,
  optional catch bindings, `**` / `**=`.
- ES2020 (`es2020.rs`): `?.` (with `this` kept for calls, `delete`, parenthesized chains) and
  `??`.
- ES2018 (`es2018.rs`, `rest.rs`, `template.rs`): async generators and `for await`
  (`__asyncGenerator`, `__await`, `__asyncValues`, `__asyncDelegator`), object spread
  (`Object.assign`), object rest (`__rest`, including defaults around a nested rest), JSX
  spread props, tagged templates with invalid escapes (`__makeTemplateObject`, cached in
  `templateObject_N` in modules).
- ES2017 (`es2017.rs`): async functions and arrows (`__awaiter` with generators, `_super` /
  `_superIndex`, `arguments_1`).
- ES2022 (`es2022.rs`, `private.rs`): class fields as `Object.defineProperty` or assignments,
  statics after the class (aliases, `Reflect.get` / `Reflect.set` for `super` in static
  code), static blocks, computed key captures, private names through
  `__classPrivateFieldGet/Set/In` with `WeakMap` / `WeakSet` storage, private destructuring
  targets as setter objects, `accessor` storage.
- ESNext `using` / `await using` (`using.rs`): `__addDisposableResource` /
  `__disposeResources`.

Helpers are requested where they are used and printed in tsc's order (fixed priorities first,
then request order). Codegen got three small `Layout::Tsc` changes: comma sequences broken
after every comma on request, synthesized strings double-quoted with non-ASCII escaped, and
legacy octal / leading-zero numbers printed as their value.

**Gates, measured.** Emit oracle (`--emitter program --layout tsc`), JavaScript outputs.

| Outputs | E1 (all refused) | First E5 run (refusal per file) | Final |
|---|---|---|---|
| `(target=es20xx)` variants, 1,575 | 0 | 996 shape / 969 bytes | **1,498 shape (95.1%)** / 1,454 bytes (92.3%) |
| es2015, 11,859 | 0 | | 11,036 shape (93.1%) / 10,623 bytes (89.6%) |
| es2016, 8 | 0 | | 8 / 8 (100%) |
| es2017, 130 | 0 | | 117 shape (90.0%) / 108 bytes |
| es2018, 63 | 0 | | 61 shape (96.8%) / 60 bytes |
| es2019, 17 | 0 | | 16 / 16 (94.1%) |
| es2020, 124 | 0 | | 114 shape (91.9%) / 110 bytes |
| es2021, 116 | 0 | | 114 shape (98.3%) / 113 bytes |
| All below ES2022, 12,317 | 0 | 10,330 shape / 9,978 bytes | 11,466 shape (93.1%) / 11,038 bytes (89.6%) |
| ES2022+ (the E1 gate), 2,159 | 2,029 shape | 2,029 | 2,053 shape (95.1%), 0 unlisted gaps |

- **Shape ≥ 95% on the variants: met** (1,498; the gate needs 1,497).
- **Lowered fixtures run under Node and print what ESNext prints: met.**
  `crates/ts_r_transform/tests/runtime.rs` runs five fixtures (async and rest, classes,
  operators, private names, `using`) at ES2015 to ES2021 and at ESNext through
  `scripts/limit.sh`, and compares stdout.
- **TSR2003 left in the variants: 4.** `using` in `usingDeclarations.1` (es2015, es2017), `#x in
  o` outside its class (`privateNameInInExpressionTransform`, es2020), and a computed key
  inside a pattern nested under an object rest (`destructuringEvaluationOrder`).
- 41 entries of `emit_known_gaps.txt` now pass; the list can shrink.

**Remaining causes (variants not shape-equal, 77).** CommonJS 27 (mostly error cases:
exports of invalid declarations), other 13 (parser: top-level `await` followed by a line
break in a module ×4, an unterminated template in a labeled statement ×2, `await` as an
async arrow's parameter), decorators 10 (`design:type` metadata that needs the checker:
imported, BigInt and JSDoc types), lowering temporaries 7 (decorator temporaries: TC39
comment-preservation and source-map tests, metadata name collisions), import/export 3, `__awaiter` 2 (parser), `"use strict"` 2
(parser: `this is` type predicate on an accessor), const enum inlining 2, classes 2, one
each elsewhere.

**Observed rules worth keeping** (all from tsc 7.0.2 outputs):
- Temporaries a transformer needs are numbered in print order of their hoisted `var`, which
  lists them in creation order: a `super` write makes key, result, then value temporaries.
- With `useDefineForClassFields`, an uninitialized static field defines nothing below
  ES2022, but its computed key is still captured. Without it, a removed field still
  evaluates a computed key that may have effects, together with the other key captures in
  one `static { }` block (one statement after the class below ES2022).
- An object literal holding an object-rest destructuring assignment (outside functions and
  nested literals) is wrapped in `Object.assign({ .. })`, field descriptors included.
- `export default class C` with statics becomes `class C`, the statics, then
  `export default C;`.

## Implementation notes (E2, `m8/emit-dts`, 2026-10-19)

**What landed.** `ts_r_emit::dts`, with these modules:
- `mod.rs`: statements, classes, and the choice of what to write;
- `types.rs`: type nodes and member lists;
- `reuse.rs`: reusing source type nodes and literal initializers;
- `isolated.rs`: the TS9xxx rules;
- `expando.rs`: namespaces for `f.x = 1`.

In the checker, `typenode.rs` holds `type_to_type_node`, and `typenode/specifier.rs` builds
module specifiers for `import("…")` types.

ts-r-check writes `.d.ts` files under `declaration`. The following work as observed in tsc
7.0.2:
- `emitDeclarationOnly`;
- `declarationDir`;
- `stripInternal`;
- `removeComments`;
- `newLine`;
- `isolatedDeclarations`.

A probe project compared tsc and ts-r over output paths, file sets, `.d.ts` text and the map
`sources`, under:
- `outDir` with `declarationDir`;
- `declarationDir` inside `outDir`;
- `emitDeclarationOnly`;
- `declarationMap` on and off.

Everything matched except the map `mappings`.

**Departures from section 2.**
- **Own printer.** Declarations are not printed by `ts_r_codegen`. Their layout differs
  from JS output in many places:
  - type literals are always multi-line;
  - list and declarator comments are kept;
  - accessor pairs, overload groups, `#private;`;
  - multi-line tuples as written.
  The emitter writes text directly, with one small printer that both reused source nodes
  and checker-built nodes go through.
- **Reuse before inference.** A source annotation is reused as written when every name in
  it resolves to the same symbol at the declaration (`reuse.rs`). Otherwise the emitter
  asks the checker. Simple literal initializers print their source text: `1`, `'x' as const`,
  `-1`, template literals without substitutions.
- **Specifiers are verified by resolution.** An `import("…")` specifier, whether a package
  name, a subpath export, a relative path or a path without `/index`, is only used if
  resolving it from the output file reaches the declaring file. If none does, the result is
  TS2883 (tsc 7's code for TS2742).
- **Declaration maps** have a segment at the start of each written declaration and at the
  start and end of each name copied from the source. go-to-definition through the map
  lands on the name. They do not have tsc's other segments: declaration lists, parameters,
  type nodes, statement ends. Map bytes therefore differ from tsc's, and the oracle does not
  gate them.

**Refusals.** Nothing is written with an unresolved type: 0 such files in the oracle and in
the consumer packages. Instead:
- TSR2001: a declaration needs a type the checker left `UNRESOLVED`. That file's `.d.ts`
  only is skipped: 40 outputs in the oracle.
- TSR2006: JavaScript inputs. JSDoc declaration emit is not implemented: 552 outputs.
- tsc's own refusals:
  - TS2883: non-portable type;
  - TS4094: private or protected member of an exported anonymous class;
  - TS9005..TS9039: `isolatedDeclarations`, reported at the node tsc reports.
- Each refusal skips the output, so the run exits 1.

**Observed rules worth knowing** (probes, each encoded in a test):
- Expando assignments (`f.x = 1`) become `declare namespace f { … }`. An assigned
  identifier is re-exported as `export { x };`.
- Object-literal members keep their JSDoc.
- Class members whose computed name cannot be bound are hoisted after `#private;`, in this
  order:
  - static members first;
  - then string keys, then number keys, then symbol keys;
  - methods become function-type properties;
  - interface and type-literal members with such names are dropped.
- Shadowing type parameters are renamed `T_1`. `typeof` of a body local expands its
  structure. An anonymous type cycle prints `/*elided*/ any`.
- JSON modules under node16..nodenext are `{ default: V }`.
- An unannotated generator infers `Generator<Y, R, unknown>`. This also fixed checker
  conformance case `arrayFromAsync`: 9,618 → 9,619.
- `stripInternal` honours any leading comment that contains `@internal`, but not a trailing
  comment on the previous line.
- An import whose bindings are all elided stays as `import "m";` only when `m` augments the
  importing module.

**Gates, measured.** Emit oracle, `--emitter program`, run 56.

| Measure | Result |
|---|---|
| Baselines without JavaScript inputs, every `.d.ts` bytes-exact (the gate's denominator: JSDoc declaration emit is out of scope, TSR2006) | **1,127 / 1,320 (85.4%), gate 85%: met** |
| All baselines with `.d.ts` output | 1,127 / 1,639 (68.8%) |
| `.d.ts` outputs bytes-exact | 1,858 / 2,640 (70.4%) |
| `.d.ts` outputs shape-equal | 1,870 / 2,640 (70.8%) |
| `.d.ts` written with an unresolved type | **0** |

Progress on outputs bytes-exact: 0% → 52.0% (first run) → 66.1% → 70.4%.

- **Consumer test** (`import * as m` of each package's entry, `export type Api = typeof m`,
  checked by tsc against tsc's `.d.ts` and against ours).
  - Packages: brace-expansion 5.0.12, lru-cache 11.5.3, minimatch 10.2.6 (tshy configs), and
    rxjs 7.8.2 (`src/tsconfig.types.json`).
  - **261 / 261 `.d.ts` files bytes-exact; 4 / 4 consumers identical** (same exit code,
    diagnostics and consumer `.d.ts`).
  - The record asks for 20 packages. The pinned ecosystem cache holds only these four that
    ship TypeScript sources with a tsconfig.
  - private-app was not available in this session.
- **Not done: declaration diagnostics under `--noEmit`.** tsc reports TS4094 and friends
  with `declaration` on even when not emitting. ts-r-check's `--noEmit` path runs through
  `ts_r_history` without a loaded program, so it reports none. Closing this needs either a
  full program load under `declaration` (slower check-only runs) or declaration
  diagnostics saved in history.

**Remaining `.d.ts` causes** (outputs, excluding TSR2006):

| Cause | Outputs |
|---|---|
| import types (specifier choice, `import("./x")` vs a local name) | 49 |
| TSR2001 (types the checker leaves unresolved) | 40 |
| other types | 24 |
| imports kept or elided | 23 |
| outputs tsc does not write | 13 |
| unions | 13 |
| class members | 13 |
| function types | 12 |
| `typeof` | 9 |
| shape-only (comments, line breaks) | 12 |
| others (modifiers, object literals, synthesized declarations, literal initializers, …) | 35 |

## Implementation notes (E4, `m8/watch`, 2026-10-19)

**What landed.**
- **`ts_r_watch`.** The watcher moved out of `ts_r_hmr` (re-exported there as
  `ts_r_hmr::watch`; `ts_r_hmr`'s tests are unchanged) and gained `watch_filtered`, because
  ts-r-check must see `node_modules` changes. `ts_r_run` and `ts_r_lsp` do not use it yet.
- **`ts-r-check -w`** for `-p` and `-b` (`ts_r_check/src/watch.rs`). The proposal's list of
  what to watch became one rule: every file-system question a build asks goes through a
  tracking file system that records the answer and who asked (a config load, a root list,
  a program). A change is a question whose answer differs now: an edit, a touch, a file
  created or deleted, a directory listing that changed. Resolution failure locations are
  covered by the same rule, with no resolver change. What a build writes is answered again
  right after it, so a build never triggers the next. Events come from `ts_r_watch` on the
  working directory and the project directories, plus a poll every `--watchInterval` ms
  for what lies outside them.
- **Rebuilds.**
  - `-p` keeps the session's `SourceCache` and the previous build's program state in
    memory, so only changed files and their dependents are re-checked and re-emitted.
  - `-b` rebuilds the projects whose own questions changed and the projects that
    reference them (`ts_r_projects::build::run_watch`). The others print their last
    diagnostics again.
- **Output.** tsc 7's watch messages, screen clearing (pretty output only, not with
  `--preserveWatchOutput` or the stats flags) and `Found N errors. Watching for file
  changes.`. A look that finds no change prints nothing.
- **Reporting flags** (`ts_r_check/src/explain.rs`): `--listFiles`, `--listFilesOnly`,
  `--explainFiles` (also from a tsconfig), `--diagnostics` and `--extendedDiagnostics`. The
  `--explainFiles` reason order and texts are in ts_r_check/AGENTS.md. The stats flags print
  to stderr; tsc's baselines show nothing for them on stdout, so only their acceptance is
  gated.
- **Test driver.** `TS_R_WATCH_DRIVER=stdin` (hidden): each stdin line is one look, and each
  look ends with a sentinel line carrying the status. `scenario_oracle` replays the
  `tscWatch/` and `tsbuildWatch/` sessions with it, with a 60 s timeout on every wait.

**Watch-only behaviour (observed).**
- A reference whose declarations changed is reported as "output '<the project's oldest
  output>' is older than input '<the reference's config>'". A non-watch build names the
  buildinfo and the reference's path.
- A rebuild for a directory change alone keeps the config's last TS18003 state.
- A deleted `-p` config prints TS5083 and `Found 1 error.` without clearing the screen.

**`-b` changes found through the watch scenarios** (they apply to plain `-b` too):
- Config files are inputs of a non-incremental project's oldest-output check only while its
  buildinfo still has program state.
- `strict: true` (tsc 7's default) is left out of the options hash.
- A missing referenced config is a build-order node that prints its global TS6053. It is not
  an upstream, and only a missing root project counts as skipped.

**Deviations.**
- `--traceResolution` (and `traceResolution` in a config) landed after E4 on
  `m8/trace-resolution`; it is no longer TSR2004. `ts_r_resolver` has an optional trace sink
  that costs one `None` check per step when off and never changes a result. ts-r-check
  replays the program's resolutions through it in tsc's order and prints the log before
  the diagnostics.
  - Trace oracle (`ts_r_check` example `trace_oracle`, tsc's 146 `.trace.json` baselines):
    121 tests exact, 6,237 / 8,558 lines (72.9%).
  - Scenarios: `tsc/` + `tsbuild/` 226 → 236 / 377 and watch 64 → 67 / 106 (on top of
    `m8/watch` with E2 merged). Checker conformance is unchanged at 9,619 / 12,750.
  - What is left comes from resolution differences, kept on purpose; they are listed in
    `ts_r_resolver`'s AGENTS.md. Example: imports of a referenced project's output resolved
    with that project's options ("Using compiler options of project reference redirect").

**Gates, measured.**

| Tree | Watch (`tscWatch/` + `tsbuildWatch/`) | `tsc/` + `tsbuild/` |
|---|---|---|
| Before E4 | 0 / 106 | 120 / 377 |
| `m8/watch` alone | 48 / 106 (45%) | 130 / 377 |
| Trial merge with `m8/emit-dts` | 64 / 106 (60%) | 220 / 377 (was 194) |

- **Watch gate (80%): not met**, mostly because of E2.
- **Differential test: met.** `incremental_emit.rs`'s `watch_emit_equals_clean_build`
  carries one watch session per variant (`-p`, `-p --incremental`, `-b`, and `composite`
  once `.d.ts` emit exists) across random edits. After every look, each output and the
  diagnostics equal a clean build's.

**Remaining watch failures by owner (trial merge, 42).**
- **E2 (33).**
  - `.d.ts.map` mappings (`tsbuildWatch/sample` and others).
  - TS4094's "Add a type annotation" related information (`noEmitOnError`,
    `programUpdates` declaration-emit errors).
  - Declaration errors under `noEmit`.
- **E4 (2).** `--traceResolution` (`tsbuildWatch/moduleResolution`).
- **Checker.**
  - TS7016 for an untyped `.js` left behind by a deleted or renamed source. A clean build
    misses it too (4).
  - The `@types` package elaboration (1).
  - Cross-file related information, and TS2554's span and note (also in plain `-b`).
- **Resolver.** The external flag of a symlinked `node_modules` package (1).
- **Accepted.** The help text (`Parse-watch-interval…`).

In `tsc/` and `tsbuild/`, the 15 E4-tagged failures left need:
- `--traceResolution` (4, and about 12 more that also need E2 set it in their tsconfig);
- E2's TS4023 (4);
- checker casing errors TS1149 (3);
- `rootDir` with JSON imports (TS6059/TS5011, 2);
- E5 (1);
- profiling (`generateTrace`, 2, accepted).

The `--explainFiles` and `--listFiles` text itself matches in every one of them. Two more
need other owners:
- `tsbuild/transitiveReferences` needs imports in a referenced project's `.d.ts` resolved
  with that project's options.
- `tsbuild/solution/when-solution-is-referenced-indirectly` needs a project with no inputs
  and `references: []` to be skipped without TS18003.

## Implementation notes (E6, `m8/emit-hardening`, 2026-10-19)

**What landed.** `cargo xtask emit-repos` (`xtask/src/emit_repos.rs`, cases in
`bench/emit-repos.toml`). It runs tsc 7.0.2 and ts-r-check with the same command line in the
same tree, one after the other, each under `scripts/limit.sh` with 3 threads. It compares:
- the written files per kind (bytes; `.tsbuildinfo` by path only);
- exit codes and diagnostics;
- peak RSS.

The rest of E6 is fixes found by it, in `ts_r_codegen`, `ts_r_transform`, `ts_r_emit` and
small parts of the checker and resolver.

**The repositories.** All are public: the pinned bench checkouts of `bench/repos.toml` and
ecosystem-cache packages. There are 14 cases, 13 gated:
- trpc (server package; all packages with `paths` and JSX);
- rxjs 8 (ESM, tshy layout);
- lru-cache 11.5.3, minimatch 10.2.6 (ESM and CJS) and brace-expansion 5.0.12, from GitHub
  tarballs with tshy configs;
- typeorm (CommonJS, legacy decorators with metadata, importHelpers);
- rxjs 7.8.2: ES2015 ESM, CJS with downlevelIteration, and composite types;
- date-fns: `tsc -b` over 5 composite projects, and `pkgs/core` alone with 1,000+ files;
- excalidraw (JSX, declarations only). It is not gated because 60 of its files hit
  TSR2001.

**Gate: met.** Bytes identical across the 13 gated cases: **7,047 / 7,279 files (96.8%,
gate 95%)**, up from 5,189 / 7,587 (68.4%) at the start. The totals differ because the
harness also counts files only one tool writes. Peak RSS is at or under tsc 7's on
**13 / 13** cases (before: 12 / 13, typeorm 419 MB vs 317 MB).

| Case | Files identical, before → after | Exit tsc / ts-r | Peak RSS MB, tsc / ts-r before / ts-r after |
|---|---|---|---|
| trpc-server | 140/267 (52.4%) → 235/267 (88.0%) | 2 / 1 | 225 / 98 / 98 |
| trpc-packages | 258/498 (51.8%) → 438/498 (88.0%) | 2 / 1 | 325 / 135 / 133 |
| rxjs8-esm | 344/500 (68.8%) → 494/500 (98.8%) | 0 / 2 | 135 / 66 / 65 |
| lru-cache-esm | 8/12 → 11/12 | 2 / 2 | 91 / 50 / 49 |
| minimatch-esm | 15/24 → 21/24 | 0 / 2 | 91 / 48 / 48 |
| minimatch-cjs | 8/24 → 19/24 | 0 / 2 | 91 / 48 / 48 |
| brace-expansion-cjs | 2/4 → 4/4 | 0 / 0 | 51 / 24 / 24 |
| typeorm | 1214/1488 (81.6%) → 1477/1488 (99.3%) | 2 / 2 (was 2 / 1) | 317 / **419** / 168 |
| rxjs7-esm-es2015 | 416/503 (82.7%) → 486/503 (96.6%) | 2 / 2 | 156 / 54 / 55 |
| rxjs7-cjs-es5 | 392/501 (78.2%) → 462/501 (92.2%) | 2 / 2 | 108 / 40 / 40 |
| rxjs7-types | 252/501 (50.3%) → 494/501 (98.6%) | 2 / 2 | 160 / 54 / 54 |
| date-fns-build | 1309/2035 (64.3%) → 1696/1727 (98.2%) | 0 / 1 | 670 / 267 / 265 |
| date-fns-core-dist | 831/1230 (67.6%) → 1210/1230 (98.4%) | 0 / 2 | 276 / 111 / 112 |
| excalidraw (not gated) | 326/461 → 331/461 | 2 / 1 | 867 / 283 / 289 |

| Kind | Before | After |
|---|---|---|
| `.js` | 1,254 / 1,393 (90.0%) | 1,370 / 1,393 (98.3%) |
| `.js.map` | 725 / 1,138 (63.7%) | 1,084 / 1,138 (95.3%) |
| `.d.ts` | 3,201 / 4,397 (72.8%; 308 only ts-r) | 3,999 / 4,089 (97.8%) |
| `.d.ts.map` | 1 / 646 (0.2%) | 586 / 646 (90.7%) |
| `.tsbuildinfo` (by path) | 8 / 8 | 8 / 8 |
| `.json` | 0 / 5 (only ts-r) | 0 / 5 (only ts-r) |

**The record's other E6 measures.**
- **Cold `-p` with emit vs check-only:** 3 runs each, an empty `TS_R_CACHE_DIR` each time,
  median. Met on typeorm (JS, d.ts, maps): 0.665 s vs 0.583 s = **1.14×**. Unmet on
  date-fns `pkgs/core`, which is declarations only: 0.336 s vs 0.253 s = **1.33×**, above
  1.25×.
- **Warm no-change `-b` on a 20-project monorepo:** not measured here. It is E3's
  (`ts_r_projects`). date-fns has 5 projects.
- **Wall time** (from the harness, not gated): ts-r-check is slower than tsc 7 on typeorm
  (0.90 s vs 0.52 s), date-fns-build (1.45 s vs 0.97 s) and excalidraw (2.88 s vs 1.35 s).
  It is at or under tsc 7 on the small cases.

**Regression checks.**
- Emit oracle, `--emitter program`:
  - E1 bytes 93.7% (was 93.6%), shape 95.4% (was 95.3%);
  - E5 variants bytes 93.1% (was 93.0%), shape 95.4%;
  - E2 1,128 / 1,320 (85.5%, was 1,127);
  - 0 tests newly failing, 75 fixed.
- `emit_known_gaps.txt` is rewritten from that run.
- Transpile emitter: 0 newly failing, 58 fixed.
- Checker conformance: 9,619 / 12,750, unchanged.

**Observed rules added.** Each is from tsc 7.0.2 output on these repositories and on probes.
Fixtures: `ts_r_emit/tests/tsc_layout/{e6_real_repos,module_preserve}`.

- **JS source maps.**
  - `?.` maps its start and end.
  - A tagged template maps the template's start and end, and each `}` before a middle or
    tail.
  - `new.target` maps at the name. A generator method maps its `*`.
  - CommonJS:
    - preamble statements map nothing;
    - `export { x } from` maps the `require` to the declaration, and each getter and its
      value's property to the specifier;
    - `export * as ns` maps `exports.ns` to the name.
  - A static field's `static { this.x = v; }` maps nothing on the block. The statement
    ends at the field's end. The assignment ends at the key's end only in a class that also
    moves instance initializers into its constructor.
  - `this["k"]` maps the key.
  - In codegen, `NO_MAP` marks one side of a range as unmapped.
- **JS comments.**
  - A field initializer moved into the constructor takes the field's comments
    (`PrintHints::moved_comments`).
  - Comments between `if (...)` and an embedded statement lead the statement.
  - Inside `import { }` / `export { }`, a specifier's own-line comments print after `, ` on
    their own line (`export { a, \n// c\nb, }`). A comment before the `,` stays
    (`b /* c */, c`).
- **File-level JS.**
  - The file's detached comments are only those up to the first blank line.
  - Under `module: preserve`, tsc never writes the `export {};` marker: a module whose
    statements were all erased is an empty file. Program emit turns off
    `TransformOptions::preserve_module_marker`. The bundler keeps the marker.
- **Declarations.**
  - A hashbang is kept.
  - Export lists keep the source's trailing comma; import lists, which are rebuilt, do not.
  - Specifier comments are kept on the same line in any form. On their own line they are
    kept only when they are `/** */` or `/*! */`.
- **Declaration maps** now have tsc's segments:
  - the start and end of every node kept from the source;
  - modifiers and `?`;
  - comments.

  Nodes the emitter builds map nothing. The 0.2% → 90.7% `.d.ts.map` change is this.
- **Declaration names** (checker, `typenode.rs`):
  - a symbol reachable through a namespace import of the file prints `ns.Name`;
  - `import("…")` may go through a program module that re-exports the symbol, when that
    module's relative path has fewer segments;
  - a relative specifier keeps the `.js`/`.ts` ending the file's own imports use.
- **Libraries.** A workspace package linked into `node_modules` is a library (not emitted,
  no rootDir rule) even though its real path is outside `node_modules`: the resolver's
  `is_external_library_import`. Its JavaScript is still loaded under `allowJs`.

**Checker and resolver edits** (kept small; listed for merges):
- `ts_r_checker/src/typenode.rs`:
  - `namespace_import_entity`, wrapping `entity_of_inner`;
  - `shorter_reexport` and `reexporters`;
  - `relative_ending`.
- `typenode/specifier.rs`: `Ending::Ts`.
- `checker.rs`: `CachedTypes::reexporters`.
- `program.rs`: allowJs inclusion of linked workspace JavaScript.
- `ts_r_resolver/src/resolver.rs`: `is_external_library_import`.

**Left** (by owner):
- **Checker:** the ts-r-only diagnostics that change exit codes on 8 cases:
  - TS2345/TS2322 on rxjs 7 and 8 generics;
  - `RegExpMatchArray` tuples in minimatch;
  - the lru-cache branded index;
  - `Console` members in date-fns;
  - trpc spreads and generics.

  Also the TSR refusals, mostly TSR2001 on types the checker leaves unresolved: 60 in
  excalidraw, 11 in trpc.
- **Emit / codegen (this track):**
  - `.d.ts.map`: 60 not identical, 49 of them in trpc, mostly import types and inferred
    types;
  - rxjs 7 ES2015 maps of downlevelled code: 44;
  - the date-fns `tsc -b` copies of `package.json` that tsc 7 does not write: 5;
  - a comment after the last kept specifier when a type-only specifier follows it;
  - BSON-style `exports.X = void 0` for a `declare namespace` that holds a value
    `export {}`;
  - class-body detached comments that tsc gives the synthesized constructor;
  - declaration-only emit cost (1.33× check-only; 1.24× after the E2 follow-up).
- **E3:** the warm `-b` measure.

## Implementation notes (E2 follow-up, `m8/dts-followup`, 2026-10-20)

**Declaration maps.** tsc 7.0.2 writes parts of inferred types from the source's syntax,
and those parts map to the source (observed with `declarationMap` probes):
- the annotation of a parameter, return type, property, getter or setter in a signature
  being written, when it writes the same type and its names mean the same at the
  declaration (an instantiated `Promise<T>` is rebuilt; `Response` is kept);
- object literal property initializers: `x: e as T` writes `T`; string, template, number
  and bigint literals of a `const` context are written as written (`'a'`, `-0`, `1n`), not
  `true`/`null`; `as const` arrays write their elements from syntax;
- the keys of an object literal written from its syntax: a variable initializer, the only
  `return` of a function, or a nested property value. Not when the literal has a shorthand
  or spread member, a computed key that is not a literal, a nested literal that is not
  written from syntax, a value whose type the object widens (`a: s` with `const s = 1`,
  `!s`), or a function with an inferred type predicate;
- a private method is a new property: no end mark, and no start mark when a modifier is
  dropped (`private async m()`).

Another file's annotation is copied with its text and nothing mapped: literals as written
(`'a' | 'b'`, `Array<string>`), names rewritten as the enclosing file can write them
(`import("./b").Q`, `import("./id").Id<{}>` for an import type). The same holds for its
object literal values (`readonly a: 'x'`), but not its `as const` tuples, which tsc writes
from their type. A number spelled otherwise (`0x10`) is written from its value and mapped
to the literal. A reused node that declares a type parameter the written type renames
(`infer T` written `infer T_1`) is written from its type: tsc keeps such a node and renames
inside it, so its text matches now and its map segments do not.

**Default-exported functions.** `export default (a) => a` (or a function expression, or
`export =`) is written as `export default _default;` then `declare function _default(…)`
with the statement's comments; a named function expression keeps its name. Only the
parameters map. `/** */` comments after the last statement are kept (followed by a space
when no line break follows them); a multi-line parameter JSDoc is re-indented; a `//`
comment after a parameter's comma is kept and the next parameter starts a new line.

**Names.** A local declaration that its module exports through an export declaration is
named by that export: `import("./Types").Suit` (`export { Suit }`), `import("./color").default`
(`export default Color`), `import("./foo")` (`export = Conn`), or `y.Member` through a
namespace import of the module. A module the file imports that exports the symbol
(explicitly or by `export *`) names it when its specifier ranks before the declaring
module's: a package specifier before a relative one, then fewer path segments, the
declaring module first among equals (`import("typescript-fsa").A` over
`"typescript-fsa/src/impl"`). A `.ts` specifier of a `.d.ts` file does not make relative
specifiers keep `.ts`. A default import of a module without a default export
(`allowSyntheticDefaultImports`) names its members (`http.Server`). `infer` type
parameters get names unique in the whole written type (`T`, `T_1`, `T_2`), the same
parameter the same name.

**Declaration diagnostics.** TS4094 is reported once per private or protected member, with
the related TS9027 "Add a type annotation to the variable x." on variables. TS4023 is
reported on a variable whose type recurs through another module's unexported alias
(`reports-dts-generation-errors`).

**Symbol-keyed properties.** A property of a variable's type literal is named through the
variable (`typeof Op.or`, `uniqueSymbolPropertyDeclarationEmit`). A mapped type's property
keyed by a unique symbol is TS4118 ("its property '[timestampSymbol]' cannot be
serialized"), and a unique-symbol key that another module declares and the file cannot name
is TS4023; tsc writes no `.d.ts` for either file. A spread's copy of an optional property
keeps its declaration's annotation (no added `| undefined`), and a single unlabeled rest
element `[...T[]]` is written `T[]`.

**Builds.** A `.d.ts.map` is written (and listed) before its `.d.ts`; a composite project
rewrites an unchanged `.d.ts.map`, and rewrites unchanged `.d.ts` files too when it has no
previous state (another version's buildinfo); `declarationMap` toggled does not make
downstream projects out of date; a new `declarationDir` re-emits the JavaScript.

| Measure | Before | Maps and diagnostics | Cross-file reuse and names | Symbol keys, parser, emit cost |
|---|---|---|---|---|
| Emit oracle `.d.ts` baselines without JS inputs | 1,129 / 1,320 (85.5%) | 1,135 / 1,320 (86.0%) | 1,146 / 1,319 (86.9%) | 1,151 / 1,318 (87.3%) |
| Emit oracle `.d.ts` outputs bytes-exact | 1,860 / 2,640 | 1,867 / 2,640 | 1,880 / 2,640 | 1,885 / 2,640 |
| Emit oracle declaration maps (bytes-exact `.d.ts`) | 15 / 16 | 16 / 16 | 17 / 17 | 17 / 17 |
| Emit oracle `.d.ts` import-type differences | 49 | 49 | 38 | 38 |
| Emit oracle TSR2001 (type not written) | 39 | 39 | 39 | 38 |
| Scenarios (`tsc/`, `tsbuild/`) | 256 / 377 | 269 / 377 | 270 / 377 | 270 / 377 |
| Watch scenarios | 71 / 106 | 78 / 106 | 78 / 106 | 78 / 106 |
| emit-repos, all files (gated) | 7,046 / 7,279 (96.8%) | 7,077 / 7,279 (97.2%) | 7,102 / 7,279 (97.6%) | 7,103 / 7,279 (97.6%) |
| emit-repos `.d.ts.map` | 586 / 646 (90.7%) | 612 / 646 (94.7%) | 623 / 646 (96.4%) | 623 / 646 (96.4%) |
| Checker conformance | 9,926 | 9,926 (0 newly failing) | 9,926 (0 newly failing) | 9,926 (0 newly failing) |

Two baselines left the `.d.ts` denominator: ts-r no longer writes a `.d.ts` that tsc does not
(`declarationEmitObjectAssignedDefaultExport`, `declarationEmitMappedTypeTemplateTypeofSymbol`).

**Declaration-only emit cost.** The E6 measure (cold `-p` on date-fns `pkgs/core` with an
empty `TS_R_CACHE_DIR`, declarations only vs `--noEmit`, medians) was 1.33×. Re-measured
first, it was worse than recorded and not comparable: `@types/node` 26 writes
`S extends T ? | A | B : never`, which the parser took for a JSDoc postfix `T?` (TS1005), so
check-only stopped at syntax errors and checked nothing. With the parser fixed (`?` before a
token that starts a type is the conditional's), check-only was 0.19 s and declarations
0.41 s (2.1×). A time profile (xctrace) of the difference: writing the 1,230 outputs
(create, mkdir, write, close) about 125 ms on one thread, the writer canonicalizing all
1,471 inputs 31 ms, and the declaration emit itself 42 ms of CPU (about 15 ms over 4
threads). The writer now reads input identities only when an output already exists (by
device and inode), creates directories only on NotFound, and writes on up to 4 threads;
and ts-r-check emits and writes next to the check when nothing can stop the writes.

| date-fns `pkgs/core`, 9 alternating runs, medians | check-only | declarations | ratio |
|---|---|---|---|
| Parser fixed, before the writer changes | 0.19 s | 0.41 s | 2.1× |
| Writer: lazy identities, parallel writes | 0.19 s | 0.27–0.33 s | 1.4–1.7× |
| Emit and writes next to the check, fresh `outDir` | 0.192 s | 0.237 s | **1.24×** |
| Same, outputs already present | 0.174 s | 0.186 s | **1.07×** |

typeorm (JS, declarations, maps) is now 0.50 s with emit vs 0.54 s check-only (the
`--noEmit` run also saves history). The overlap holds the emit's checker during the check:
peak RSS 116 → 129 MB on date-fns, 202 → 221 MB on typeorm. The bench's check cases use
`noEmit` configurations and are unaffected. The machine was shared with other agents, so
single timings vary by about ±10%.

**Remaining `.d.ts` causes by owner** (emit oracle outputs, excluding TSR2006's 552
JavaScript inputs: 148 differ, 41 missing, 11 extra).
- **Declaration emit (`ts_r_emit`, `typenode/`):**
  - malformed import types in error tests (28 of the 38 import-type outputs:
    `nodeModulesImport*DeclarationEmitErrors*`, `parseAssertEntriesError`,
    `parseImportAttributesError`): tsc writes its recovered node
    (`import("pkg", { with: {} })`);
  - `export =` namespace members named through the module (`import("./x").B`, not `A.B`);
  - a package's entry over a subpath for a re-exported type
    (`reactTransitiveImportHasValidDeclaration`);
  - a symlinked package named by its package path (`declarationEmitReexportedSymlinkReference2`);
  - three TSR2001 "too deep" refusals (`declarationsWithRecursiveInternalTypesProduceUniqueTypeParams`,
    `mappedTypeWithAsClauseAndLateBoundProperty2`, a `this.http.request()` return).
- **Checker (`ts_r_checker` outside `typenode/`):**
  - the alias of an interface instantiation (`type Al = I<string>`) is lost;
  - alias type arguments are not kept as written (`Component<any, {}, {}>` for `Component`);
  - a self-referential function expression is an error type (tsc writes
    `() => () => /*elided*/ any`);
  - narrowing and inference differences (`NonNullable`, inferred type predicates);
  - about 35 TSR2001 refusals where the checker leaves a type unresolved: reverse mapped
    types, instantiation expressions, dynamic `import()` types, `this` in object literals,
    variadic tuples, circular bases;
  - the `exactOptionalPropertyTypes` mapped `foo?: string`.
- **Resolver:** a package's self-name through `exports` with an `outDir`.
- **Out of scope here:** JSDoc declaration emit for JavaScript inputs (TSR2006), and
  declaration diagnostics under `--noEmit`.

## Implementation notes (scenario parity, `m8/scenarios`, 2026-10-20)

Work from the scenario baselines' observed behaviour; each rule names its scenario in the
crate's AGENTS.md.

**Incremental state (`ts_r_projects`).**
- Signatures propagate as tsc's do: changed files are evaluated, their direct importers
  while signatures keep changing, and the other transitive importers only get their
  declarations pending (signature reset). A changed file that declares a const enum makes
  those importers re-emit their JavaScript too.
- Emit is pending per file and kind. A file whose missing `.d.ts` tsc's own emit
  diagnostics explain (TS4094 and the like) is not pending, and the declaration options
  are stored as after a complete emit (a rebuild writes nothing).
- `declaration` is not a checking option: turning it off asks for nothing new. The API
  hash is kept next to a `.d.ts` signature so a build without declarations can compare
  shapes (tsc computes the `.d.ts` signature there).
- `tsc -p` records the `package.json` lookups as `-b` does.
- `.d.ts` content hashes ignore a final `//# sourceMappingURL=` line; a reference built in
  this run whose latest changed `.d.ts` is unchanged leaves its dependents up to date
  "with .d.ts files from its dependencies". A dropped root is reported before a missing
  output. A non-incremental project whose only errors are declaration emit errors updates
  its output timestamps.
- Another set of library files (`noLib`, `lib`, `target` toggled) affects every file.
- A container project (no inputs and a `references` key, even an empty one; no TS18003)
  prints only its config file's own errors, not program-level option checks (TS5108).

**Emit.** A JSON file that is only imported and lies outside `rootDir` is not written and
is no source of the layout (no TS6059, TS6307, nor part of the common source directory).
Cross-file const enum accesses are inlined (`1 /* E.A */`) through
`EmitResolver::const_enum_value`, answered by the checker; the import is then elided.
Under `isolatedModules` nothing is inlined. Two gaps: with `preserveConstEnums` an import
used only by inlined accesses is kept, and so is a namespace import used only as `ns.E.A`
(tsc elides both; the output runs the same). `export default X` of a type-only import is
erased.

**Checker.** TS2318 for missing core global types (`noLib`), reported with option errors
too; TS2726/TS2727 for an unknown `/// <reference lib>`; TS7016 for a relative import of
JavaScript outside the program (the `@types` hint only for a package with a package id)
and for an untyped automatic JSX runtime; related information on arity, overload, spelling
and elaboration errors.

**Reporting.** The Errors/Files summary table is sorted by file name; `--listFiles` puts
`lib.d.ts` first.

| Measure | Start | After |
|---|---|---|
| Scenarios (`tsc/`, `tsbuild/`) | 271 / 377 (71.9%) | 321 / 377 (85.1%) |
| Watch scenarios | 78 / 106 (73.6%) | 91 / 106 (85.8%; one `projectsBuilding` scenario is flaky under load) |
| Checker conformance | 10,958 exact, 95 FP | 10,968 exact, 95 FP (0 newly failing) |
| Emit oracle ES2022+ JS bytes / variants shape / `.d.ts` | 2,023 / 1,502 / 1,154 | unchanged |

Left unmatched (build): help and locale text (E0's decision), declaration diagnostics under
`--noEmit` (an open decision), JSDoc declaration emit (TSR2006), forceConsistentCasing
(TS1149/TS1261), a few checker diagnostics (TS2748, TS2878, TS2613, TS2783, TS1479), `.d.ts`
specifier choices, `generateTrace` and `libReplacement`.

## Implementation notes (E2 leftovers, `m8/dts-leftovers`, 2026-10-20)

This round closes the declaration emit causes that the E2 follow-up assigned to `ts_r_emit`
and `typenode/`, plus the resolver's self-name case. Each rule was observed in tsc 7.0.2
probes and is listed in the owning crate's AGENTS.md.

**Ambiguous project root (TS2209/TS2210, `ts_r_resolver`).** Take a program with `outDir` or
`declarationDir` but neither `rootDir` nor a tsconfig. It cannot resolve its own package's
self-name through `exports`, nor an `imports` entry with a relative target, when the
package is outside `node_modules`. tsc reports each such entry once, as a global
diagnostic, and the import stays unresolved. The trpc emit-repos cases depended on this: their tests import `@trpc/server`, and
tsc gives TS2307 there. Their extra TS2883 for `WebSocket` came from the `export =` rule
below. Both now match tsc: exit 2 / 2, diagnostics identical. tsc's own harness passes
test directives as options, not as a config file. The conformance and emit oracles now
do the same: they drop the wrapper's config path, so no tsconfig directory stands in for
`rootDir`.

**Import attributes (`ts_r_parser`).** Import attributes are a delimited list (TS1478 for a
bad entry). An `import()` type whose second argument is not an object with `with` or
`assert` gets empty attributes. Declaration emit writes that as tsc's recovered node,
`import("pkg", { with: {} })`. An `as` type that holds an `assert` import type is not
reused: it is written from the checker.

**Specifiers (`typenode/specifier.rs`).**
- A member of a module's `export =` namespace is `import("m").B`, even in the module's own
  file.
- A package's `exports` condition branch names its file when it is `default` or an active
  condition.
- A type in a chunk that a same-package entry re-exports is named through the re-exporter.
  Ranking: a package specifier before a relative one, then fewer path segments. When the
  declaring module has no specifier at all, any re-exporter will do (this covers vitest's
  hashed chunks).
- A symlinked package is named by its package path.
- ts-r-check gives the specifier host the project references, so a package specifier
  resolves to a referenced project's output `.d.ts`
  (`inferredTypeFromMonorepoReference`, `synthesized-module-specifiers` ×2).

**Writing types from structure (`typenode.rs`).**
- An instantiation of an anonymous type that is nested inside 11 open instantiations of
  the same declaration is written `/*elided*/ any`. The declared, uninstantiated type does
  not count. Instantiated object literals now carry `ObjectFlags::INSTANTIATED`, so the
  checker can tell them apart.
- A signature written from a type keeps its type-parameter defaults.
- An optional `?` parameter written from its type gets `| undefined`, even without
  `strict`.
- TS7056: tsc stops at an approximate serialized length of 1,000,000.
  `MAX_SERIALIZED_LENGTH` keeps a running count:
  - members, parameters and type parameters count their name + 2;
  - keywords count their text;
  - references count their name plus argument separators;
  - unions count 3 per separator;
  - function types count 6 + 2 per parameter;
  - import types count specifier + qualifier + 8;
  - annotations reused as written count their subtree.

  This is calibrated on a spread chain: 66,500 members are written and 66,750 give TS7056.
  Past the limit the declaration gets TS7056 at its name
  (`hugeDeclarationOutputGetsTruncatedWithError`,
  `declarationEmitPrivatePromiseLikeInterface`).

**Declaration content (`ts_r_emit`).**
- Fresh literal annotations: consider an initialized `const`, or a `readonly` property,
  whose annotation is a `typeof import()` (directly or through non-generic aliases) that
  still gives a fresh literal. It is written `= 1` rather than with its annotation.
  `typeof k` of a local `const k = 1` keeps its annotation: tsc's is the regular literal.
  The checker keeps the fresh one because its `switch` narrowing compares `TypeId`s.
  Regularizing it in `declared.rs` cost three TS2345 errors per trpc case, so the rule
  stays syntactic.
- `[Symbol.x]` is kept when `SymbolConstructor` has `x`, even one typed `symbol`. It is
  dropped like other unbindable keys when `x` is missing (TS2339). These two rules cover
  `change-to-type-used-as-global` ×2.

| Measure | Before | After |
|---|---|---|
| Checker conformance | 11,073 exact, 94 FP | 11,087 exact, 94 FP (0 newly failing) |
| Emit oracle ES2022+ JS bytes / variants shape | 2,023 / 1,503 | 2,023 / 1,503 |
| Emit oracle `.d.ts` baselines without JS inputs | 1,155 / 1,318 | 1,172 / 1,317 (89.0%) |
| Emit oracle `.d.ts` outputs bytes-exact | 1,889 / 2,640 | 1,938 / 2,640 |
| Emit oracle `.d.ts` import-type differences | 38 | 1 |
| Emit oracle "imports" differences | 17 | 5 |
| Emit oracle TSR2001 (type not written) | 28 | 26 |
| Scenarios (`tsc/`, `tsbuild/`) | 321 / 377 | 326 / 377 (86.5%) |
| Watch scenarios | 91 / 106 | 91 / 106 |
| Trace oracle | 142 / 146 | 142 / 146 |
| emit-repos, all files (gated) | 7,113 / 7,279 (97.7%) | 7,120 / 7,279 (97.8%) |
| emit-repos cases with identical diagnostics | 11 / 13 | 13 / 13 (trpc exit 2 / 2) |

One baseline left the `.d.ts` denominator because ts-r no longer writes a `.d.ts` that tsc
does not (TS7056).

**What remains, by owner.**

The checker (`ts_r_checker` outside `typenode/`):
- The alias of an indexed access type is lost (`FlatArray`).
- `updateIfChanged`'s circular inference gives `any`.
- Alias type arguments are not kept as written (react's `Component<any, {}, {}>`, the one
  import-type difference left).
- `toLocaleString` overloads are written in a different order.
- tsc doubles a tuple in some cases, and ts-r does not reproduce it.
- `typeof k` freshness, as above.
- About 25 TSR2001 refusals for unresolved types.

These cover `declarationsWithRecursiveInternalTypesProduceUniqueTypeParams` and
`mappedTypeWithAsClauseAndLateBoundProperty2`: they are now written, but still differ.

ts-r-check:
- With TS2209/TS2210 present, semantic diagnostics are still printed (tsc prints none when
  a global diagnostic exists).
- The warm lazy history path does not report TS2209/TS2210.

Out of scope here: JSDoc declaration emit for JavaScript inputs (TSR2006), and declaration
diagnostics under `--noEmit` (an open decision).
