---
name: add-lint-rule
description: Implement an ESLint or typescript-eslint rule in ts-r-lint from its docs page, its published tests (as data) and observed ESLint output, with options, fixes and suggestions, and prove it with the rule oracle. Use when adding a rule to crates/ts_r_lint or fixing a rule's mismatches.
---

# Add a lint rule

ts-r-lint rules must behave exactly like ESLint's: same id, messages, message ids, positions,
options, fixes and suggestions. Read [crates/ts_r_lint/AGENTS.md](../../../crates/ts_r_lint/AGENTS.md)
and [docs/design/0006-lint.md](../../../docs/design/0006-lint.md) first.

Use the docs page, the rule's test file (valid/invalid cases) and observed output. The rule's
implementation (ESLint, typescript-eslint, oxlint, Biome) may be read to understand an edge
case; **never port it**: write our own on our semantic model.

## 1. Get the oracle data

1. Check `crates/ts_r_lint/tests/oracle/rules/<name>.json` (`ts-<name>.json` for
   typescript-eslint). If it exists, skip to step 2.
2. Otherwise add the rule to `crates/ts_r_lint/tests/oracle/gen/rules.txt` and regenerate:
   - In a temp dir, `npm i eslint@<pinned> typescript-eslint@<pinned> globals` (versions in
     the generator header).
   - Download the test files only: `npm pack eslint@<pinned>` and
     `npm pack @typescript-eslint/eslint-plugin@<pinned>` (or the GitHub tarballs) and keep
     only `tests/lib/rules` / `packages/eslint-plugin/tests`.
   - Run, under the memory cap:
     `scripts/limit.sh --mem-mb 4096 -- node crates/ts_r_lint/tests/oracle/gen/gen-rule-oracle.mjs --tools <tmp> --eslint-tests <dir> --tseslint-tests <dir> --out crates/ts_r_lint/tests/oracle/rules <rule>`
   - The generator stubs every rule module; it records what the real `Linter` reports.
3. Read the rule's docs page for options and their defaults.

## 2. Write the rule

1. Create `crates/ts_r_lint/src/rules/<snake_name>.rs` with `pub static RULE: RuleDef`:
   - `meta(name, docs, presets, category)`, plus `.fixable()` / `.suggestions()`.
   - Presets: `ESLINT_REC`, `TS_REC`, `TS_STRICT`, `TS_ESLINT_REC` or `NO_PRESET`.
   - `create: create_default::<T>` for rules without options, or `create::<T>` with a
     `FromOptions` impl that parses the options array (unknown keys are ignored; wrong types
     return `Err`).
2. Implement `Rule`:
   - Return the node kinds you need from `kinds()` / `exit_kinds()`; handle them in
     `enter` / `exit`. Use `run_file` only for whole-file work (scopes, comments).
   - Scope questions go through `cx.semantic()` (the `Semantic` trait), never a private walk.
   - Type questions go through `cx.file.types`; set `requires_types` in the meta if the rule
     cannot work without them.
   - Report with `cx.report(span, message).id("messageId")`, add `.fix(Fix::replace(..))` or
     `.suggest(id, desc, fix)` as the docs describe. Copy message text from the oracle data.
   - Shared helpers are in `ast_util.rs`; add new ones there if two rules need them.
3. Add the module to `rules/mod.rs` (`mod` line and the `ALL` list, alphabetical).
4. If oxlint or Biome have the rule under another name, add it to `config/mapping.rs`.
5. For a typescript-eslint extension rule (same name as a core rule), share the core
   implementation: make the core rule type `pub(crate)` (or add `pub(crate) fn create_ts`
   with a TypeScript switch) and give `ts_<name>.rs` only the `RuleDef`. Check both oracles.
6. If the test file's cases depend on tester-level `languageOptions` (source type, parser
   options), check that the oracle JSON records them before debugging the rule; extend
   `pickParserOptions` in the generator and the harness's `config_for` if not.

## 3. Prove it

1. `TS_R_LINT_ORACLE_RULE=<name> TS_R_LINT_ORACLE_VERBOSE=20 cargo test -p ts_r_lint --test rule_oracle -- --nocapture`
   shows each mismatching case with expected and actual messages and fix output.
2. Iterate until every case passes, or the remaining ones are explained (parser differences,
   ESLint features we do not have). Never special-case test inputs.
3. Bless the baseline: `TS_R_LINT_ORACLE_BLESS=1 cargo test -p ts_r_lint --test rule_oracle`.
   The diff of `tests/oracle/baseline.txt` must only go up.
4. Run `cargo test -p ts_r_lint`, `cargo clippy -p ts_r_lint --all-targets -- -D warnings`,
   `cargo fmt --check`.
5. Lint a real project and compare with ESLint on the same files: the scripts in
   `crates/ts_r_lint/tests/oracle/project/` copy a repo to a temp dir, enable every ts-r-lint
   rule, and print per-rule match/miss/extra. Every "extra" is a bug; explain every "miss".
