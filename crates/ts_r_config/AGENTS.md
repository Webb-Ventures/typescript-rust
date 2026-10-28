# ts_r_config

tsconfig.json loading for every ts-r tool: a JSONC parser with byte offsets, `extends`
chains, typed compiler options with tsc 7's defaults, file discovery (`files`/`include`/
`exclude`), project references, and the config diagnostics tsc 7 reports. Matches
TypeScript 7.0.2. Root rules: [../../AGENTS.md](../../AGENTS.md).

## API in one breath

`load_tsconfig(&fs, path) -> ParsedConfig` (or `ConfigLoader::new(&fs).load(path)` to share the
raw-file cache across projects). `ParsedConfig` has `options: CompilerOptions`, `file_names`,
`include`/`exclude` (`SpecList`), `references` (each with its resolved `config_path` and the
reference's byte `span`, where TS6306/TS6310 are reported), `watch_options`, `type_acquisition`,
`extended_config_paths` and `diagnostics` (`all_diagnostics()` adds option checks).
`ConfigLoader::load_with_overrides(path, callback)` applies CLI option overrides after
inheritance and before file discovery, default exclusions and empty-input diagnostics.
`ConfigLoader::command_line(names, callback)` is the project of files named on the command
line (`tsc a.ts`, `--ignoreConfig`): defaults plus overrides, the names as `files` and roots,
`command_line` set and `config_path` `<cwd>/tsconfig.json`, which is never read. A
`SpecList` keeps its non-string entries in `invalid` (TS5024 each): `files: [1]` is not
empty (no TS18002), and TS18003 shows them as written (`'[1]'`). TS18003 quotes `include`
and `exclude` as written, `${configDir}` unexpanded (the default `outDir` exclude is absolute). Empty inputs: TS18002 for
an empty `files` list unless references are listed; TS18003 unless the config has a
`references` key at all (`declares_references`, an empty list too: a container project).
`CompilerOptions::get(OptionId)` / typed accessors (`o.strict()`) give explicit values;
`o.effective()` applies defaults (`module()`, `module_resolution()`, `resolution_algorithm()`,
`resolve_json_module()`, `lib_files()`, ...). `show_config(&parsed)` prints exactly what
`tsc --showConfig` prints. `find_config_file` / `resolve_project_arg` do project discovery
(nearest `tsconfig.json` upwards, `-p` file-or-directory). `FileSystem` (with `RealFs`,
`MemoryFs`) is the fs abstraction the resolver reuses (`RealFs::read_file` decodes through
`fs::decode_text`: a UTF-16 byte order mark, either endianness, means UTF-16 as in tsc;
otherwise UTF-8 with U+FFFD for invalid bytes; `read_bytes` preserves original bytes in
`RealFs` and defaults to UTF-8 contents for text-backed implementations; `stamp(path)`: length and
modification time without a read, `RealFs` only; the default `None` makes callers read and
compare, so a wrapper that changes contents must not forward it; `may_be_symlink(path)`:
`false` only when the path is known not to be a symlink, so globbing derives a directory's
real path from its parent's instead of asking the file system; `disk_name(dir, name)`: an
entry's stored case, which include globs use on case-insensitive file systems instead of
listing each ancestor, so a recording wrapper need not treat it as a dependency); `exports` holds the package.json
`exports`/`imports` matcher shared with `ts_r_resolver`; `path` has string path helpers.
`cli::parse_command_line(&args, cwd, &fs) -> CommandLine` parses tsc's command line
(`cli::FLAGS` is every flag `tsc --all` lists, in its order, plus `--ignoreDeprecations`);
`CommandLine::apply_to(&mut options)` merges it over a tsconfig's. Every option in `OPTIONS`
carries `affects` classes (`options::affects::{CHECK, EMIT, DECL_EMIT, OUTPUT_PATHS,
BUILDINFO}`, design record 0012) that cache keys are built from.

## Invariants

- **tsc 7 is the spec.** Behaviour comes from running `tsc` (see oracle below) and from docs,
  never from TypeScript's source. When the oracle disagrees with a comment here, the oracle wins.
- **TS 7 defaults:** `target` ES2025; `module` from `target`; `moduleResolution` nodenext /
  node16 / bundler from `module`; `strict` true; `types` `[]`; `resolveJsonModule` on for
  node20/nodenext and bundler. Removed options/values (`baseUrl`, `outFile`,
  `downlevelIteration`, ES5, AMD/UMD/System, node10/classic, `esModuleInterop: false`, ...) are
  reported (TS5102/TS5108/TS5095) and treated as their TS 7 replacements (`node10` → bundler).
  TS5102's hint for `baseUrl` is `paths` `*` relative to the leaf config's directory, `./`-
  prefixed unless it starts with `../` (`..` gives `./../*`).
- **Paths are absolute, normalised, `/`-separated strings.** Path-valued options are stored
  absolute; `paths` substitutions stay as written with `CompilerOptions::paths_base_path`.
- **Positions are UTF-8 byte offsets** (`(u32, u32)` spans). UTF-16 columns only when rendering.
- **Never panic on input.** Malformed JSON, wrong option types and bad `extends` give diagnostics.
- **Deterministic:** option output follows the static `OPTIONS` table order; globbing lists each
  directory's files before its subdirectories in ordinal order, like tsc.
  Globbing reads the directories it can reach ahead of the walk, a level at a time on up
  to 4 threads (`prefetch`); that is only a cache, the serial walk decides what is listed.
- `extends` specs are rebased one level at a time; `${configDir}` is substituted only as a
  prefix; package `extends` uses conditions node/require/types/default and real paths.
  A package with no `node_modules` copy is looked up in the nearest Yarn PnP
  `.pnp.data.json` (`pnp.rs`: owner by longest `packageLocation`, whole directory names compared with the
  file system's case rule, its `packageDependencies`,
  aliases, `fallbackPool`), then resolved as usual (`exports`, `tsconfig`). This goes past
  tsc 7, which has no PnP and reports TS6053; it only applies where tsc would fail, and
  matches esbuild (`TsconfigPackageJsonExportsYarnPnP`, `TsconfigStackOverflowYarnPnP`).
  Every manifest read for that (whether it resolved the name or not, inherited from bases)
  is listed in `ParsedConfig::pnp_manifest_paths`: tools that watch config inputs watch it
  too, next to `extended_config_paths`.
- **Malformed JSON is reported as tsc 7 reports it** (`jsonc.rs` module and `parse` docs;
  observed with `tsc -p`, 220 inputs in `tests/fixtures/malformed`). tsc parses the file as
  one expression, so: parse errors keep one per position (TS1005, TS1109 "Expression
  expected." for a missing value, TS1136/TS1137 for a token a list cannot use, TS1012, and the
  scanner's TS1002/1010/1121/1124/1125/1127/1160/1161/1177/1178/1198/1199/1351/1489/6188/6189);
  conversion errors are a separate list: TS1327 for a key (or value) that is not a
  double-quoted string, TS1328 once over a value that is not JSON (an identifier, a missing
  value, `a + b`, `f()`, a regex, a template), TS1136 for a shorthand or spread member
  (`Member::key_form`). The option pass then reports such a value again at its *full start*:
  TS5024 where an option type applies, else TS1328 (TS1136 for a shorthand) under unknown
  keys, unknown compiler options (which then get no TS5023), `watchOptions`, plugin entries
  and `paths` entries (`Diags::free_form`). Non-JSON list elements are dropped (a list of
  only those is not set), and a later element's error is anchored at the written element
  whose index it has in what is left (`list_anchor`). A root without `{` parses as an object
  after "'{' expected."; tokens after the root are TS1012 once and parsed too; the first
  object literal is the config. A bad escape keeps its text (`"\u12"`). Number options are
  truncated to integers, saturating (`1.5` is 1), as tsc 7 stores them.
  A BigInt value (`123n`, `-1n`) is one of those TS1328s, but `ParsedJson::errors_except_bigints`
  leaves it out: ts_r_bundler reads JSON errors from there and reports a BigInt in esbuild's
  words itself (`Unexpected "123n" in JSON`). Fixture outputs must not depend on where the
  checkout is: an absolute path in an input becomes a `../../..` path in showConfig (and has
  no drive on Windows), which the oracle refuses.
- **Two kinds of config diagnostics.** `diagnostics` are parse errors (JSON, unknown option
  TS5023, wrong type TS5024, bad value TS6046); tsc still checks the program after them.
  `option_diagnostics()` (`verify.rs`) and `emit_overwrite_diagnostics()` (TS5055) are tsc's
  program-level option checks; after any of them tsc reports no syntactic or semantic errors.
  `verify.rs` covers TS5051/5052/5053/5059/5067/5069/5074/5089/5091/5095/5096/5102/5108/5109/5110,
  TS6304/6379 and TS18035. Anchors (observed): the option's key, or its value for
  TS5095/5096/5108/5109/5110; an option set in a base config, or not set at all, is anchored
  at the leaf's `compilerOptions` key (no location when the leaf has none); TS5052/5053/5091
  use whichever of the two options comes first; TS6379 uses the `declaration` key if present.
  `checkJs` implies `allowJs`, so `checkJs` is TS5052 only beside an explicit `allowJs: false`.
  TS5074 (no location): an explicit `incremental: true` (not implied by `composite`) with
  neither a config file nor `tsBuildInfoFile` (`tsc: tsc/internal/compiler/program.go`).
  `sort_and_dedupe` orders like tsc: file, start, end, code, message.
- **Command line (`cli.rs`) follows observed tsc 7.0.2 behaviour**, summarised in the module
  docs: flags case-insensitive with one or two dashes, no `--flag=value` form, booleans take
  only an exact `true`/`false`, strings always take the next argument, lists skip a dashed
  one, `null` clears, last occurrence wins, `--build` only first, build-mode aliases and
  TS5072/5093/5094, diagnostics in argument order then TS6370. `table_matches_tsc_all`
  compares `FLAGS` with `tests/fixtures/tsc-7.0.2-all.txt` (tsc's own `--all` output); update
  both together when the pinned tsc moves.
- **`affects` is conservative.** A class is set when the option can change that output
  (re-check and output-diff probes against tsc 7, record 0012): CHECK = diagnostics,
  EMIT = `.js`/maps bytes, DECL_EMIT = `.d.ts` bytes beyond what the checker infers (a
  declaration key must include CHECK too), OUTPUT_PATHS = which files are written where,
  BUILDINFO = recorded in `.tsbuildinfo`. When unsure, set the class: a missing bit is a
  stale cache, an extra bit only a miss.

## Oracle

- Fixtures: `tests/fixtures/<project>/tsconfig*.json`, expected `--showConfig` text and config
  diagnostics in `expected/`, tsc version in `tests/fixtures/TSC_VERSION.txt`.
  `cargo test -p ts_r_config --test oracle` compares; regenerate with
  `TS_R_TSC=/path/to/tsc cargo test -p ts_r_config --test oracle -- --ignored regenerate`
  (default `npx -y -p typescript@7.0.2 tsc`).
- Real projects: `TS_R_EXTERNAL_PROJECTS=/repo1:/repo2 cargo test -p ts_r_config --test oracle -- --ignored external`
  compares every `tsconfig*.json` (outside `node_modules`) with `tsc --showConfig` and diagnostics.
  Public bench repos (2026-10-26, `b5938d0`, tsc 7.0.2; `bench --fetch-repos` checkouts of
  trpc, date-fns, rxjs, typeorm and playwright): 114 configs, 114 `--showConfig` outputs
  identical, 114 with the same diagnostics (the last 3, TS18003 quoting `${configDir}`
  specs as written and TS5102's `paths` hint relative to the leaf config, fixed in
  `b5938d0`).
- The harness runs tsc under `scripts/limit.sh` and caps its own memory
  (`ts_r_testing::cap_memory!()`, a dev-dependency).
- Fixture `node_modules` are checked in (`tests/fixtures/.gitignore` re-includes them) and one
  is a symlink; keep it a relative link.

## Dependencies

None. No serde: tsconfig needs comments, trailing commas, byte offsets on every key and value,
duplicate-key handling and tsc's exact error codes (TS1005, TS1327, ...), which a hand parser
gives directly.

## Gaps

- Diagnostics use the local `ConfigDiagnostic` (code, message, file, byte span); they move to
  `ts_r_diagnostics` once that crate settles.
- No TS5011/TS6059 (`rootDir` must contain all sources) or other checks that need the program.
- JSONC error recovery follows tsc's expression grammar only as far as JSON mistakes need:
  methods and accessors in objects (`{ a() {} }`), `new`, arrow bodies, JSX and the
  missing-expression position after a decorator (`@x }`, where tsc points at the trivia) can
  report different follow-on errors. Past 256 levels of nesting each token is one TS1328.
- Wildcard directories for watch mode are not exposed yet; `watchOptions` is parsed but unused.
- The command line does not reject some well-formed `--locale` tags tsc has no messages for
  (`xx`); only malformed tags give TS6048.
