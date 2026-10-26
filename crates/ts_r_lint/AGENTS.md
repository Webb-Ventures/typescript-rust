# ts_r_lint

The ts-r-lint engine: ESLint-compatible rules, config loading (ESLint flat config, oxlint,
Biome), `eslint-disable` directives, fixes and output formats. The binary lives in
[../ts_r_lint_cli](../ts_r_lint_cli/AGENTS.md). Root rules: [../../AGENTS.md](../../AGENTS.md).
Design: [../../docs/design/0006-lint.md](../../docs/design/0006-lint.md). Adding a rule:
[../../.agents/skills/add-lint-rule/SKILL.md](../../.agents/skills/add-lint-rule/SKILL.md).

## API in one breath

```rust
let mut opts = ts_r_lint::project::LintOptions::new(cwd);
opts.paths = vec!["src".into()];
let run = ts_r_lint::project::lint_paths(&opts)?;          // Err = fatal config problem
let text = ts_r_lint::output::render(Format::Stylish, &run.reports, &cwd, Registry::global());
// One file with an already-resolved config:
let r = ts_r_lint::lint_source(path, src, &file_config, &NoTypeInfo, FileKey(0));
// Editors (ts-r-lsp): the CLI's config lookup and loader, one file at a time:
let r = lookup.governing(dir);                          // project::ConfigLookup -> ConfigRef
let cfgs = ts_r_lint::project::load_configs(&[r], &opts, None)?;
```

Files:
- `rule.rs`: the rule API (`RuleDef`, `Rule`, `Ctx`, `FileCtx`, `Report`).
- `engine.rs`: one traversal per file with per-`NodeKind` dispatch; directives; fix loop.
- `rules/`: one module per rule; `rules/mod.rs` lists them in `ALL`.
- `semantic/`: the `Semantic` trait and `local.rs`, the **temporary** scope analysis.
- `types.rs`: the `TypeInfo` trait type-aware rules ask (`NoTypeInfo` answers nothing).
- `typed/`: type-aware linting on our checker. `mod.rs` loads a tsconfig project on the
  shared `ts_r_history` levels and checks + lints it; `ck_types.rs` implements `TypeInfo` over
  a checker worker; `parity.rs` compares our diagnostics with the user's tsc (cached);
  `driver.rs` groups files by tsconfig, runs projects, the parity verdict and `--fix` passes.
- `type_util.rs`: type helpers shared by type-aware rules (any/error kinds, constraints,
  builtin-symbol checks, thenables).
- `lib_globals.rs`: TypeScript lib globals for TypeScript-parsed files (which lib files, from
  `parserOptions.lib`, the tsconfig with `project`/`projectService`, or `esnext`), cached per
  lib set and per tsconfig (reloaded when the tsconfig or a config it extends changes; the
  nearest-tsconfig lookup is not cached, so created and deleted tsconfigs count). The same
  tsconfig supplies `emitDecoratorMetadata`/`experimentalDecorators` when the parser options
  leave them unset, as typescript-eslint reads them from the program. `lib_globals_generated.rs` is the per-lib-file name table,
  **generated** by `cargo xtask codegen lint-globals` from the pinned TypeScript lib files;
  never edit it by hand (`cargo xtask codegen --check` verifies it).
- `directives.rs`: `eslint-disable*`, `/* eslint */`, `/* global */`, unused-directive reports.
- `config/`: flat config (`eslint_eval.rs` runs Node once, cached), `mapping.rs` (rule-name
  mapping and unsupported reasons), `oxlint.rs`, `biome.rs`, `glob.rs` (minimatch subset).
- `project.rs`: file discovery (`.gitignore`, ignores, nearest config), parallel runner.
- `output.rs`: stylish, json, sarif, github.
- `ast_util.rs`: helpers shared by rules.

## Invariants

- **From scratch.** Behaviour comes from ESLint's and typescript-eslint's docs, their published
  rule *tests* (as data, via `tests/oracle/gen/gen-rule-oracle.mjs`) and observed output. Their
  source (and oxlint's or Biome's) may be read to understand a rule, never ported (root AGENTS.md).
- **Typed lint of a JavaScript file sees the source as written**: `lint_program_file` lints a
  clone of the checker's tree without its JSDoc-desugared types (`Ast::strip_jsdoc`; node ids
  are unchanged, so type queries still work). ESLint never sees JSDoc as syntax.
- **Rule ids are ESLint names** (`no-debugger`, `@typescript-eslint/no-explicit-any`), and
  messages, message ids, positions (1-based line, UTF-16 column), fixes and suggestions match
  ESLint's byte for byte. `tests/rule_oracle.rs` checks this; per-rule pass counts must not
  drop below `tests/oracle/baseline.txt`.
- **Spans are UTF-8 byte offsets on character boundaries**, with one exception: start + 2
  of a 4-byte character means "between its UTF-16 surrogates" (regex rules without `u`
  report code units there); `output::Positions` maps it to the low surrogate's column and
  the LSP rounds it down to the character.
- **Rules are stateless.** `Rule` objects are shared across threads and files; per-file state
  lives in locals of `run_file`, or is derived from `cx.file.parents` / `cx.semantic()`.
- **One walk per file.** Rules register node kinds in `kinds()` / `exit_kinds()`; whole-file
  rules use `run_file` and should walk only what they need.
- **The semantic model is lazy** (`FileCtx::semantic`) and behind the `Semantic` trait.
  `semantic/local.rs` is temporary: replace it with an adapter over `ts_r_binder` without
  touching rules.
- **Type-aware rules** set `requires_types` (`meta(..).typed()`). They run only on files of a
  tsconfig project whose diagnostics match the user's tsc (or with no TypeScript installed);
  otherwise the file is linted without them and the rule is listed as unsupported with the
  reason. Design record 0006, "Type information". The user's tsc runs for those parity checks
  on up to `threads` (max 4) background threads, one process each.
- **Parity compares `file(line,col): TScode` multisets** (`typed/parity.rs`, cache version
  `PARITY_VERSION`; bump it when the comparison changes). Our checker follows tsc 7. When the
  user's TypeScript is older than 7, its TS2345 headline counts as the TS2741 / TS2739 /
  TS2740 that TypeScript 7 reports for the same argument. The first elaboration line ("Property
  'x' is missing in type ..." / "Type ... is missing the following properties ...") picks the
  code. Nothing else is paired.
- **`TypeInfo` grows additively**: every new query has a default that answers nothing
  (`NoTypeInfo` and partial implementations stay valid) and its `CkTypes` implementation calls
  public checker functions only. `type_at` answers for `Param` nodes (the parameter's type,
  used for destructured parameters) and for a type parameter's declaration (the type
  parameter). `non_optional_type_at` is a chain link's type without the `undefined` the
  optional chain's short-circuit adds (`prefer-optional-chain` needs the link's own type).
- **The error type is not `any` to typescript-eslint everywhere** (observed): it is unsafe
  as a value (`no-unsafe-*` "error typed"), an error-typed receiver accepts an argument's
  `as` but no nullable `!`, an error-typed property of an intersection receives anything,
  and only an `unknown` receiver (not `any`) makes assigning `any` safe in
  `no-unsafe-assignment`.
- **Split runs**: `FileConfig::typed_only` / `RuleSet::typed_only` give the type-aware half of
  a file's rules (no unused-directive reports, and inline `/* eslint */` config adds no
  untyped rules to it), for the editor, which runs that half on its type backend's checker
  and the rest without types.
- **`TypeId::UNRESOLVED` never causes a report.** A type our checker did not compute is
  "don't know" in every `type_util` helper; rules must keep that (use `any_kind`, not raw
  flag tests, for any-ness).
- **Typed linting uses the program's own AST** (`lint_parsed` with the checker's
  `SourceFile`), so node ids match the checker's. Lint threads for a project hold one
  checker worker each; never share a `CkTypes` across threads.
- **Dependencies on the checker crates** (`ts_r_checker`, `ts_r_types`, `ts_r_binder`,
  `ts_r_history`, `ts_r_diagnostics`) exist for `typed/` only; rules see types only through
  `TypeInfo`.
- **Fixes never overlap** in one pass (ESLint's rule: a fix starting at or before the previous
  fix's end is skipped); `lint_and_fix` re-lints up to 10 passes.
- **Never panic on input.** Parse errors produce one fatal "Parsing error" diagnostic.
  tsc's checker grammar errors that the parser reports (`ParseResult::grammar`,
  `SourceFile::grammar`: `set x() {}`, `class C<> {}`) are not parse errors here, as in
  typescript-eslint.
- **A checker failure never stops or hangs a run.** `typed::driver` catches a panic in a
  project's check (`lint_group`) or in a tsc parity worker (`run_tsc`): the project's files
  are linted without type-aware rules, with a note and `UnsupportedReason::InternalError`,
  and the tsc pool is closed on every exit path (`CloseOnDrop`) so no worker thread is left
  waiting.
- **Parse through `engine::parse_for_lint`.** With the TypeScript parser, `.js`/`.jsx` files
  use tsc's JavaScript grammar with TypeScript syntax allowed (`TsInJs::Allow`: JSX on, no
  expression type arguments, no TS8xxx errors), as typescript-eslint does; never parse them
  as `.ts`.
- **TypeScript lib globals follow typescript-eslint's scope analysis** (observed): on files
  parsed with the TypeScript parser, a lib name with a type meaning is a global variable; it
  satisfies type references, and value references only when it also has a value meaning
  (`Map` yes, `PropertyKey` as a value no); a value-only lib name (`document`) is not
  defined. Configured globals satisfy both. `builtinGlobals` counts every lib global
  variable (`LintEnv::is_builtin_global`). Core `no-undef` reports unresolved type
  references too.

## Status (type-aware round 3, `m4/type-aware-3`)

- 154 rules, 51 of them type-aware. Rule oracle: 15,495 / 15,517 cases. The non-type-aware
  cases are 8,597 / 8,597 and the type-aware cases 6,898 / 6,920 (2026-10-24, `087afc9`); the
  22 failures are a subset of the 46 listed in design record 0006 (`tests/oracle/baseline.txt`
  has the current per-rule counts).
- private-app against ESLint with every configured rule: all 23 projects pass the parity
  gate. The result is 26,578 match, 0 miss, 0 extra.
- Speed at 4 threads: cold 4.4 s and 406 MB; warm 0.52 s and 170 MB. ESLint takes 19.6 s.

## Testing

- `cargo test -p ts_r_lint` runs unit tests and the rule oracle. The oracle replays rule files
  on up to 4 threads and reports in file order, so its output stays deterministic.
- `TS_R_LINT_ORACLE_RULE=no-unused TS_R_LINT_ORACLE_VERBOSE=5 cargo test -p ts_r_lint --test
  rule_oracle -- --nocapture` shows mismatches for one rule.
- Type-aware cases (`typed` in the data, `tests/oracle/rules/ts-*.json`) run against
  `tests/oracle/typed/` (typescript-eslint's test fixtures, mounted in memory at `/fixtures`)
  through `typed::load_project` + `check_and_lint`, as the CLI does; they need the lib files
  (`cargo xtask upstream sync`) and are skipped without them. Generate them with
  `gen-rule-oracle.mjs --fixtures <typescript-eslint>/packages/eslint-plugin/tests/fixtures`.
- After improving rules: `TS_R_LINT_ORACLE_BLESS=1 cargo test -p ts_r_lint --test
  rule_oracle` rewrites the baseline.
- Regenerating oracle data needs ESLint and typescript-eslint installed in a temp dir; see the
  header of `tests/oracle/gen/gen-rule-oracle.mjs`. Run it under `scripts/limit.sh`. The
  generator records tester-level `sourceType` and the parser options that change scope
  analysis (`jsxPragma`, `jsxFragmentName`, decorator metadata, `lib`); the harness passes
  them on. Testers load as ESM: put `{"type":"module"}` in a `package.json` next to the
  typescript-eslint test files.
- Project oracle (real code, ESLint vs ts-r-lint, per-rule match/miss/extra):
  `tests/oracle/project/` (`setup.py` copies a repo to a temp dir and stubs missing plugins,
  `all_rules_config.py` writes a config enabling every ts-r-lint rule, `eslint_packages.py`
  runs ESLint per package, `compare.py` diffs JSON reports, `bench.py` times runs). Results
  for private-app are in design record 0006. For the all-rules run, lint the whole copy
  once (`eslint -c oracle.config.mjs -f json -o <out>/ALL.json .`, and `ts-r-lint
  --no-gitignore -c oracle.config.mjs -f json`); `compare.py` treats `ALL.json` as every
  package.

## Patterns

- typescript-eslint extension rules reuse the core rule: make the core rule type
  `pub(crate)` or add `pub(crate) fn create_ts`, and keep the `ts_*` module to its `RuleDef`.
- Fixes that rewrite import/export lists build a list of `(Span, String)` edits and fold them
  into one `Fix` (see `ts_consistent_type_imports.rs`); keep edits non-overlapping.
