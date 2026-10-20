# ts_r_resolver

Module and type-reference resolution matching tsc 7 (TypeScript 7.0.2): node16, nodenext and
bundler. Root rules: [../../AGENTS.md](../../AGENTS.md). Config side:
[../ts_r_config/AGENTS.md](../ts_r_config/AGENTS.md).

## API in one breath

`Resolver::new(fs, ResolverOptions::from_compiler_options(&parsed.options))`, then
`resolve_module(specifier, containing_file, ResolutionMode) -> Option<Arc<ResolvedModule>>`
(`resolved_file_name`, `original_path`, `extension`, `is_external_library_import`,
`package_id`, `resolved_using_ts_extension`) and
`resolve_type_reference(name, containing_file, mode)` (`None` for a `types` entry).
`mode_for(file, ImportKind)` picks Import/Require for an import; `file_format(file)` gives the
node16/nodenext ESM/CJS format and the deciding `package.json`. `package_json(dir)`,
`package_scope(dir)`, `conditions(mode)`, `clear_caches()`. The resolver is `Send + Sync`;
share one per option set. `resolve_module_traced` / `resolve_type_reference_traced` take a
`&mut Trace` and log the steps (see Trace below).

## Trace (`--traceResolution`, `src/trace.rs`)

- `Trace::new()` (optionally `.with_compiler_version("FakeTSVersion")` for tsc's test
  baselines) collects lines in tsc 7's wording and order; `lines()` / `take_lines()` read them.
  One `Trace` spans one program: it remembers misses ("does not exist according to earlier
  cached lookups") and `package.json` lookups ("exists according to earlier cached lookups").
- Every step reports to a `Tracer` (`Option<&mut Trace>`) through the `tr!` macro, which
  formats nothing when tracing is off: the untraced entry points pay one `None` check per step.
- **Tracing never changes a result.** A traced call bypasses the module/type-reference
  result caches (so every step is logged) but uses the same code path. The one trace-only
  branch is tsc's "trying with modern Node resolution features disabled" retry (ESM import,
  JS result from a package with `exports`): it is logged and its result discarded.
- The traced footer shows the file resolution found before a referenced project's source is
  swapped for its declaration output, as tsc shows it.
- Oracles: ts_r_check's `trace_oracle` example (tsc's `.trace.json` baselines) and the
  scenario oracle's `--traceResolution` scenarios.
- File misses are one cache for module and type-reference lookups ("according to earlier
  cached lookups" crosses between them).
- Known differences from tsc's trace: `require()` calls in JS files are not module requests
  yet (the binder does not collect them), and `libReplacement` does not replace lib files in
  the program, so a replaced lib's own lib references still load (ts_r_check's trace looks
  every lib file of the program up as `@typescript/lib-*`).
- Use the TS 7 baselines (`tsc/testdata/baselines/reference/`), not the TS 5 ones in
  `tests/baselines/reference/`: they differ in places (package IDs, cached lookups).
- A request in a file of a referenced project (or in its declaration output) prints
  "Using compiler options of project reference redirect '<config>'." right after the header,
  and the header names the source file, not the output.

## Invariants (all observed with `tsc --traceResolution`)

- **Algorithms:** TS 7 removed node10/classic; they run as bundler. Conditions are
  `import|require`, `types`, plus `node` for node16/nodenext, then `customConditions`.
  `types@<range>` conditions match against `TS_VERSION`.
- **ESM mode under node16/nodenext is strict:** no extension guessing, no directory modules,
  for relative, `paths` and package-subpath lookups. A package root still uses
  `types`/`main`, then `index` when it has a `package.json` (none: unresolved).
- **Extensions:** `.js`→`.ts,.tsx,.d.ts,.js,.jsx`; `.jsx`→`.tsx` first; `.mjs`→`.mts,.d.mts`;
  `.cjs`→`.cts,.d.cts`; `.ts`→`.ts,.tsx,.d.ts`; `.json`→`.d.json.ts`, then `.json`
  (`resolveJsonModule`); other extensions → `.d.<ext>.ts`. Extensionless: `.ts,.tsx,.d.ts,.js,.jsx`
  (never `.mts`/`.cts`/`.json`). `moduleSuffixes` loop inside each extension. JS files resolve
  without `allowJs` (the checker reports).
- **Relative specifiers ending in `/`, `.` or `..`** are directory-only.
- **node_modules:** two passes over all ancestors (`TS+declarations`, then `JS+JSON`); each
  directory tries the package, then `@types/<mangled>`. `exports` (when present) is final for
  that package (no file fallback); a target that does not exist tries the next condition.
  `typings` beats `types` with no fallback; `main` is used for declarations only when neither
  exists. A directory named by one of those fields loads its `index` only (its own
  `package.json` is ignored). `typesVersions` (first matching range) maps the entry and
  subpaths in both passes; a sub-directory without its own `package.json` maps `index`
  through the package's `typesVersions`, relative to that directory, even when it does not
  exist. A mapped subpath naming a declaration file that exists as written gets no package
  ID. A missing subpath directory is skipped without a line. In `@types/<pkg>`, the
  `<pkg>.d.ts` file and the package's `index` are declarations only, but its
  `types`/`typings`/`main` targets load TypeScript files too.
- **Custom `typeRoots` fallback:** a bare specifier node_modules does not resolve is looked
  up in each custom type root like a type reference (`<root>/<name>.d.ts`, then the
  directory; strict ESM: only a directory with a `package.json`), without a package ID.
  A root named `@types` holds scoped names mangled ("Scoped package detected"); other roots
  hold `@scope/name` as written; a missing `<root>/@scope` probes nothing. Type references
  use the same rules for their primary lookup.
- **`#` specifiers:** under node16 (`module` node16/node18/node20), `#` and `#/...` are invalid
  ("Invalid import specifier ... has no possible resolutions") and go on as package names;
  nodenext and bundler accept `#/*` keys.
- **Order for bare specifiers:** `paths` (falls through to node_modules if nothing resolves) →
  `#imports` of the nearest `package.json` → self-name via `exports` → node_modules.
  A bare `imports` target resolves again from the package directory (`#a` → `#b` → `./b.js`
  works); at most `MAX_IMPORTS_DEPTH` (32) hops, so a cycle is unresolved, not a stack
  overflow (tsc 7.0.2 overflows its stack there).
- **Real paths** only for node_modules and type-reference results (unless `preserveSymlinks`).
  `is_external_library_import` is true for every result of a node_modules lookup, also a
  workspace link whose real path leaves node_modules (tsc 7 treats it as a library: not
  emitted, no rootDir rule). The checker still loads such a link's JavaScript under `allowJs`.
- **Package IDs:** `name/sub@version[+peer@version...]` from the package root's `package.json`;
  peers sorted by name and looked up next to the package's real path. Directory modules with
  their own `package.json` take the ID from it (usually none). Relative imports inside a package
  get its ID, except directory modules.
- **Type references:** primary = type roots (custom `typeRoots` accept `<root>/<name>.d.ts`;
  default `node_modules/@types` roots hold directories only); then declarations-only
  node_modules search from the containing file, skipped for `types` entries with custom
  `typeRoots`. Primary results carry no package ID. Relative names resolve next to the file.
- **`exports`/`imports` targets:** a `null` target (or a `null` condition value) ends the
  whole lookup, not just that package (no `@types`, no other ancestors). A target with a TS
  extension is probed as written in the TS pass; in the JS pass its extension is stripped and
  the JS-pass replacements tried. A target in a directory that does not exist probes nothing
  (the "stripping it" line still prints).
- **`paths`:** a substitution whose pattern text carries an extension (`./src/*.ts`,
  `./src/*.js`, `.json`) is probed as written first (with `moduleSuffixes`); an extension that
  only the `*` capture supplies is not.
- **Module formats:** under `module: commonjs`, `.mts`/`.mjs` files resolve their imports in
  Import mode, everything else in Require mode.
- **Project references** (`references.rs`): requests in a file of a referenced project resolve
  with that project's options through a child resolver (one per project, created lazily,
  sharing this resolver's fs probe caches). A file listed by several projects goes with the
  last one recorded. Self-name and `imports` targets that point into a referenced project's
  `outDir`/`declarationDir` map back to its source (`rootDir`, else the config directory), as
  tsc does before the project is built; a declaration output counts as existing while its
  source exists, also through a symlinked package directory.
- **Ambiguous project root (TS2209/TS2210):** with `outDir` or `declarationDir` set and
  neither `rootDir` nor a tsconfig (`ResolverOptions::root_dir` is `None`), a self-name
  through the program's own package's `exports`, or an `imports` entry with a relative
  target, of a package outside `node_modules`, is not resolved. Each such entry (what the
  `*` matched, else `.`) and its `package.json` is recorded once, in order met
  (`Resolver::ambiguous_roots`); the checker reports them as global diagnostics. tsc then
  reports no semantic diagnostics; the CLI does not suppress them yet (see Gaps).
- **Caching:** top-level module results keyed by (containing directory, specifier, mode).
  Recursive `imports` targets bypass that cache because their remaining depth budget differs.
  Fs probes, `package.json` and real paths are cached too, behind `RwLock`s. Nothing is invalidated
  automatically: call `clear_caches()` after file changes (it clears the child resolvers too).

## Oracle

- `tests/fixtures/<project>/tsconfig*.json` with `expected/<config>.trace.txt`: every resolution
  tsc traced (file, mode from the printed conditions, specifier, result, package ID,
  primary). `cargo test -p ts_r_resolver --test oracle`; regenerate with
  `TS_R_TSC=/path/to/tsc cargo test -p ts_r_resolver --test oracle -- --ignored regenerate`.
  Fixture `node_modules` (incl. a pnpm layout with relative symlinks) are checked in.
- Real projects: `TS_R_EXTERNAL_PROJECTS=/repo cargo test --release -p ts_r_resolver --test oracle -- --ignored external --nocapture`
  replays every resolution in `tsc --traceResolution` for each `tsconfig*.json` and diffs.
  private-app (pnpm-installed copy): 24 configs, 63,958 resolutions, 0 mismatches.
- The harness runs tsc under `scripts/limit.sh` and caps its own memory
  (`ts_r_testing::cap_memory!()`, a dev-dependency).

## Dependencies

`ts_r_config` only (fs trait, path helpers, JSONC for `package.json`, the `exports` matcher).

## Gaps

- No failed-lookup locations (for watch invalidation) or resolution diagnostics; callers
  report TS2307 etc.
- Project references are not a gap but live apart: `references.rs`;
  `ResolveOptions::project_references` redirects a referenced project's source to its output;
  `Resolver::is_reference_esm` says when such a file has its project's ES module format;
  imports in a referenced project's declaration output resolve from its source's directory.
  `Resolver::package_json_lookups` lists every `package.json` probed, for the buildinfo.
- `resolution-mode` attributes are the caller's job (pass the mode).
- Exports targets without a known extension are not probed; peer IDs only look in the
  package's own `node_modules`; `module: preserve` uses the bundler mode rules.
- Errors would move to `ts_r_diagnostics` (there are none yet).
- TS2209/TS2210: ts-r-check still prints semantic diagnostics next to them (tsc prints
  none when a global diagnostic exists), and the warm lazy history path
  (`ts_r_history::lazy`) does not report them.
