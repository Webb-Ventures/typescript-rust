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
- `jsdoc.rs`: shared machinery for the `jsdoc/*` rules (see "jsdoc rules" below).
- `esquery.rs`: esquery selectors (parser and matcher, generic over a `Tree`); `estree.rs`:
  the typescript-estree view of a file they match; `jsdoc_tree.rs`: the `JsdocBlock` view of
  one comment for jsdoc `comment` contexts (see "Selectors" below).

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

## Status (v1 lint round 3, `wip/v1-lint3`)

- 158 rules, 51 of them type-aware. Rule oracle: 15,746 / 15,766 cases. The non-type-aware
  cases are 8,846 / 8,846 and the type-aware cases 6,900 / 6,920 (2026-10-26); the 20
  type-aware failures are a subset of the 46 listed in design record 0006
  (`tests/oracle/baseline.txt` has the current per-rule counts).
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

## jsdoc rules

`jsdoc/require-param-description`, `jsdoc/require-returns` and `jsdoc/escape-inline-tags`
(eslint-plugin-jsdoc 63.3.3; its tests are the oracle, `gen-rule-oracle.mjs --jsdoc-tests
<plugin>/test/rules/assertions`). What we learned, in our words:

- **Which comment is a node's block.** Expressions (function and class expressions, arrows,
  object literals, body-less functions) borrow the comment of the nearest enclosing
  statement-like node (variable declaration, expression statement, property, class member,
  `export default`, `return`, a parameter with a default), unless they are a call or `new`
  argument or a comment sits right before them. The block is the last comment before the node
  (or its first decorator, or its parent's) skipping line comments and one `(`; it must be
  `/**` + whitespace and end 0 to 1 lines (`minLines`/`maxLines`) above the node.
- **ESTree shape.** An exported declaration is visited twice, first as its `export` wrapper
  (`Target::Export`), as ESLint sees `ExportNamedDeclaration` before the declaration. Contexts
  name ESTree types (`Finder::es_type` maps our nodes); `any` (or an object without a context)
  switches to "every block once": the first node in pre-order that finds an unclaimed block
  claims it, and blocks no node claims are checked with no node. With `any`, object entries
  with a `comment` filter decide per block: the first entry with no `comment`, or whose
  `comment` matches the block, checks it; no such entry, no check.
- **Selector contexts.** When a context is an esquery selector, or any entry has a `comment`,
  the rule runs as listeners: every ESTree node in traversal order is tried against each
  distinct context in specificity order (the plugin registers one listener per context
  string), and each matching entry checks that node's block, so two entries matching one
  node report twice. Synthetic ESTree nodes have no block. A `comment` selector is matched,
  without ancestry, against the block's `JsdocBlock` tree; an entry applies to a node when
  its context matches it (or is `any`) and its `comment` matches.
- **Function types.** A `TSFunctionType` borrows the comment of what it types: through
  parenthesized types up to an (exported) type alias, a property or method signature, a
  body-less function's return type, a method's return type, a variable declarator's type
  (the declaration's comment), or a parameter's type (the function's, or for an arrow in a
  variable, the declaration's).
- **Block model.** `Block::parse` reproduces the `comment-parser` tokens (start, delimiter,
  tag, type, name, description, end, line end per line; tag sections split on `@name` outside
  fenced code) so a fix can rebuild the comment exactly: lines live in an arena shared by the
  block and its tags, and every fix replaces the whole comment with the mutated block.
  Reports on a tag go to column 0 of its line (an empty span); others cover the comment.
- **Settings** (`settings.jsdoc`): `tagNamePreference` (a `false` or message-only entry
  blocks a tag and reports its use), `ignorePrivate`/`ignoreInternal` skip blocks, the
  `*ReplacesDocs` flags exempt blocks (or their class's block) with `@ignore`, `@override`,
  `@implements` (and `@augments`/`@extends` only when asked), `mode` (escape-inline-tags runs
  only in `typescript`), `exemptDestructuredRootsFromChecks`, `contexts`.
- **require-param-description**: each `@param` with a blank description is reported; a root
  parameter that is destructured may instead be exempted or get the default description.
- **require-returns**: a function whose body returns a value (or a `new Promise` whose
  executor calls its resolver with an argument, or any Promise with `forceReturnsWithAsync`)
  needs `@returns`; constructors, setters, abstract/`@type`/`@interface` blocks and
  `@inheritDoc` are exempt; two `@returns` report once. `publicOnly` uses a simplified export
  test (export modifiers, `export default x`, `export { x }`, `module.exports`/`exports.x`,
  `window.x`).
- **escape-inline-tags**: an `@word` at the start of a description line or after whitespace
  is reported unless allowed, inside a markdown code span, or the scoped-package part of a
  `{@link @scope/pkg}`. Descriptions are scanned first, then each tag but `@example`, one
  report per remaining occurrence; the fixer escapes with `\@` or backticks.

Read to understand the rules (not ported): eslint-plugin-jsdoc `src/iterateJsdoc.js`
(iterateAllJsdocs, findContext), `src/jsdocUtils.js` (getContextObject),
`src/jsdocUtils.js`, `src/utils/hasReturnValue.js`, `src/rules/requireParamDescription.js`,
`src/rules/requireReturns.js`, `src/rules/escapeInlineTags.js`; `@es-joy/jsdoccomment`
`src/jsdoccomment.js` (getReducedASTNode, getTSFunctionComment), `src/parseComment.js`,
`src/commentParserToESTree.js`, `src/commentHandler.js`, `src/parseInlineTags.js`; `comment-parser` `src/parser/{index,
source-parser,block-parser,spec-parser}.ts`, `src/parser/tokenizers/{tag,type,name,
description}.ts`, `src/util.ts`, `src/stringifier/index.ts`.

## Selectors

`no-restricted-syntax` and jsdoc `contexts` use esquery 1.7.0 selectors. What we learned:

- **Syntax.** Compound (`A[x]:not(B)`), descendant (` `), child (`>`), sibling (`~`, `+`),
  `,` lists, `*`, attributes (`[a]`, `[a.b=1]`, `=~`/`!=` with `/re/flags`, `type(...)`,
  `<`/`<=`/`>`/`>=`, quoted or bare values), `:not/:matches/:is/:has`, `:first-child`,
  `:last-child`, `:nth-child(n)`, `:nth-last-child(n)`, and the class pseudos
  (`:statement`, `:declaration`, `:pattern`, `:expression`, `:function`). Type names match
  case-insensitively. A bare attribute value compares as a string against the property's
  JavaScript string form; a number compares as a number; a regex only matches strings.
- **ESLint on top.** ESLint parses each listener key once, strips `:exit`, buckets selectors
  by the node types they can match (case-sensitive) and, on each node, runs matching
  selectors in order of attribute count, then identifier count, then the selector text. A
  repeated selector keeps the last option's message. Sibling and `:nth-*` selectors look at
  the node's position in its parent's list field; `:has` searches the subtree, with the
  path back to the subject as its ancestry.
- **ESTree view.** `EsTree::build` lays out the typescript-estree tree once (types, field
  names, visitor-key order, ranges): export wrappers, `ChainExpression`, `TSTypeAnnotation`,
  `ClassBody`, `TSInterfaceBody`, `TemplateElement`, `Property` shorthand values and
  similar have no AST node of their own (`Origin::Synthetic`). Checked against
  typescript-estree on the oracle snippets (14,439 of 14,439 matched type, range and parent).
- **JsdocBlock view.** `JsdocTree` mirrors jsdoccomment's compact ESTree: one
  `JsdocDescriptionLine` per description line, `JsdocTag` with its `JsdocTypeLine`s, and
  `JsdocInlineTag`s (`{@link x}`, `{@link x|text}`, `{@link x text}`, `[text]{@link x}`).
  Types are not parsed (`parsedType` is `null`).

Read to understand (not ported): esquery `dist/esquery.esm.js`; ESLint
`lib/linter/esquery.js`, `lib/linter/source-code-traverser.js`. The ESTree shape comes from
typescript-estree's output (types, ranges, parents) and `@typescript-eslint/visitor-keys`.

## Patterns

- typescript-eslint extension rules reuse the core rule: make the core rule type
  `pub(crate)` or add `pub(crate) fn create_ts`, and keep the `ts_*` module to its `RuleDef`.
- Fixes that rewrite import/export lists build a list of `(Span, String)` edits and fold them
  into one `Fix` (see `ts_consistent_type_imports.rs`); keep edits non-overlapping.
