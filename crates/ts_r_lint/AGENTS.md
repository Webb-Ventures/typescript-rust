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
- `rules/this_binding.rs`: where a `this` is bound and whether code is strict (core rules).
- `jsdoc.rs`: shared machinery for the `jsdoc/*` rules (see "jsdoc rules" below).
- `jsdoc_util.rs`: helpers the round-3 `jsdoc/*` rules share (the `contexts` option,
  preferred-tag lookup, "replaces docs" exemptions, generator/`yield`/`throw` questions).
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
- **An explicit location wins where positions are printed.** `Diagnostic::loc` (ESLint's
  1-based line and column, possibly past the end of a line) is what `output::Positions::
  start_of`/`end_of` give every format and the oracle; `None` shows the span. Rules set it
  only when the span can't say ESLint's position.
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

## Status (v1 lint round 4, `wip/v1-lint4`)

- 292 rules (134 core ESLint, 95 typescript-eslint, 40 eslint-plugin-jsdoc, 23
  eslint-plugin-unicorn), 51 of them type-aware. Rule oracle: 25,042 / 25,060 cases. The
  non-type-aware cases are 18,140 / 18,140 and the type-aware cases 6,902 / 6,920
  (2026-10-26); the 18 type-aware failures are a subset of the 46 listed in design record
  0006 (`tests/oracle/baseline.txt` has the current per-rule counts).
- private-app against ESLint with every configured rule (round 3 run): all 23 projects
  pass the parity gate. The result is 26,578 match, 0 miss, 0 extra.
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
- `tests/presets.rs` checks every ESLint and typescript-eslint rule's `Preset` tags against
  the published presets (`tests/oracle/presets.txt`, from `gen/gen-presets.mjs --tools <dir>`),
  fails if one of their rules is missing, and prints how many rules of each preset (jsdoc and
  unicorn too) are implemented (`-- --nocapture`).
- Regenerating oracle data needs ESLint and typescript-eslint installed in a temp dir; see the
  header of `tests/oracle/gen/gen-rule-oracle.mjs`. Run it under `scripts/limit.sh`. The
  generator records tester-level `sourceType` and the parser options that change scope
  analysis (`jsxPragma`, `jsxFragmentName`, decorator metadata, `lib`); the harness passes
  them on (also `isolatedDeclarations`, which `consistent-generic-constructors` reads from
  `LintEnv::isolated_declarations`). Testers load as ESM: put `{"type":"module"}` in a
  `package.json` next to the typescript-eslint test files.
- eslint-plugin-unicorn data comes from `--unicorn-tests <plugin>/test` (the generator stubs
  the plugin's test helpers and records a case's own globals; cases for other languages,
  CSS/JSON/Markdown/Vue, are skipped). A data file's top-level `globals` (builtin, node and
  browser, as the plugin's tester sets them) are merged into every case by the harness.
- Project oracle (real code, ESLint vs ts-r-lint, per-rule match/miss/extra):
  `tests/oracle/project/` (`setup.py` copies a repo to a temp dir and stubs missing plugins,
  `all_rules_config.py` writes a config enabling every ts-r-lint rule, `eslint_packages.py`
  runs ESLint per package, `compare.py` diffs JSON reports, `bench.py` times runs). Results
  for private-app are in design record 0006. For the all-rules run, lint the whole copy
  once (`eslint -c oracle.config.mjs -f json -o <out>/ALL.json .`, and `ts-r-lint
  --no-gitignore -c oracle.config.mjs -f json`); `compare.py` treats `ALL.json` as every
  package.

## Core ESLint rules (round 3)

Round 3 adds 57 core rules, each at 100% of its oracle (`no-return-assign` through
`no-useless-rename` at the end of `tests/oracle/gen/rules.txt`).

- **Shared pieces.** `rules/this_binding.rs` (no `RULE`) answers where a `this` is bound and
  whether that binding is the default one (the global object in sloppy code, and never
  under `impliedStrict` or in a module); `no-eval`, `no-alert`, `no-extra-bind` and
  `no-lone-blocks` use it. `rules::max_option` reads a `max-*` limit given as a
  number or as `{ max }` / `{ maximum }`. `LintEnv::implied_strict` carries
  `parserOptions.ecmaFeatures.impliedStrict` (all code strict), next to `global_return`.
- **yoda** decides "is this a range test" with JavaScript's `<=`: two strings compare by
  UTF-16 code units, anything else through ToNumber, and a regex literal compares as its
  source text.
- **no-useless-rename** reports but does not fix when removing the key would drop a comment,
  or when the target with a default is wrapped in parentheses.
- Round 4 adds `no-promise-executor-return`, `grouped-accessor-pairs` and `no-else-return`,
  each at 100% of its oracle.
- **no-promise-executor-return** checks the first argument of `new Promise(...)` (the
  global `Promise`): a `return` with a value or an arrow's expression body. With
  `allowVoid` the suggestion prepends `void` (adding parentheses below unary precedence);
  an expression body also suggests braces unless it is an anonymous function or class.
- **grouped-accessor-pairs** matches keys by static name, else by tokens (a private name
  never equals a public one); a key with two getters or two setters is skipped; static and
  instance members are separate lists. Reports go on the latter accessor's head, whose `(`
  is searched for after the key (a computed key can hold parentheses).
- **no-else-return** checks `if`s directly in a statement list. Its fix unwraps the `else`
  unless a name the `else` block declares is declared in, captured by a reference in, or
  hoisted into the surrounding scope (a function's implicit `arguments` counts), or ASI
  could merge statements; the fix's range is the enclosing function (or the file), so one
  applies per function per pass.
- `function_name_with_kind` shows private members bare (`private getter #x`), as ESLint.
- **no-empty-function** (and the typescript-eslint version) picks the `allow` name from the
  function's kind (arrow, function, method, getter, setter, constructor, with `async`/
  `generator` prefixes); an empty body without comments is reported at the body, with a
  ` /* empty */ ` suggestion. The TS version also allows `private`/`protected`
  constructors, constructors with parameter properties, and (by option) decorated and
  `override` methods. The message names a function only by its own name or key.

Read to understand (not ported), ESLint `lib/rules/`: `no-eval.js`, `no-lonely-if.js`,
`no-alert.js`, `eqeqeq.js`, `no-throw-literal.js`, `radix.js`, `no-extra-bind.js`,
`no-lone-blocks.js`, `prefer-numeric-literals.js`, `no-promise-executor-return.js`,
`grouped-accessor-pairs.js`, `no-else-return.js` (and `utils/fix-tracker.js`), `no-empty-function.js`, `no-extend-native.js`,
`prefer-object-has-own.js`, `no-useless-computed-key.js`, `no-restricted-properties.js`,
`no-restricted-globals.js`, `yoda.js`, `no-useless-rename.js`, and `utils/ast-utils.js`
(the default-`this` test, when braces are needed, same-reference and static property names).

## typescript-eslint rules (round 4)

Round 4 adds 13 typescript-eslint rules, each at 100% of its oracle; all are tagged
`TS_STYLISTIC` (upstream's plain `stylistic` set, `Preset::TsStylistic`).

- **array-type**: `array`, `generic` or `array-simple` (`T[]` only for simple element
  types: names, keywords, `this`, arrays, references without type arguments), with a
  separate `readonly` style; `readonly T[]` pairs with `ReadonlyArray<T>` and
  `Readonly<T[]>`. A local declaration named `Array`/`ReadonlyArray`/`Readonly` turns the
  generic check off. The fix parenthesizes union, function, intersection, operator, infer,
  constructor and conditional element types.
- **consistent-indexed-object-style**: `record` (default) reports interfaces and type
  literals with exactly one index signature, and mapped types whose key isn't used and whose
  constraint isn't a bare `keyof`; circular types (followed through aliases and members
  with a visited set) are skipped. The fix becomes a suggestion when comments would be
  lost; no fix for an interface with `extends` or under `export default`.
  `index-signature` reports two-argument `Record<K, V>`.
- **prefer-function-type**: an interface (no `extends`, or only `Function`) or type literal
  whose single member is a call or construct signature with a return type; a `this` type in
  the interface is reported instead, without a fix. The fix moves comments next to the
  member and parenthesizes inside union, intersection and array types.
- **consistent-generic-constructors**: declarators, class fields and defaulted parameters of
  functions with a body; the callee must be a plain identifier matching the annotation's
  name; global typed arrays are skipped. `constructor` mode is off under
  `isolatedDeclarations`.
- **consistent-type-assertions**: style `as` / `angle-bracket` / `never` (`as const` still
  allowed), object/array literal checks only on assertions in the chosen style, skipping
  `any`, `unknown` and `const`; `allow-as-parameter` positions are call/`new` parts,
  `throw`, defaults, JSX expressions and tagged-template parts. Only `<T>x` -> `x as T` is
  auto-fixed; "annotation" and "satisfies" suggestions follow upstream's precedence quirks
  (`==` ranks with assignment, `satisfies` and instantiations have none).
- **no-inferrable-types**: per type, the values that make it obvious (`number`: number
  literals, `Infinity`, `NaN`, `Number()`, after `+`/`-`; `bigint`; `boolean`: `!x`,
  literals, `Boolean()`; `string`; `symbol`; `null`; `undefined`/`void x`; `RegExp`);
  parameters only on functions with a body; optional, `readonly` and `abstract` properties
  skipped. The fix deletes from `:` (and a `?` or `!`) to the end of the type.
- **prefer-for-of**: `for (let i = 0; i < x.length; i++ | ++i | i += 1 | i = i + 1)` whose
  body uses `i` only as `x[i]` (same object text, not `this`) and never writes `x[i]`
  (assignment targets, `delete`, `++`/`--`, destructuring elements, and properties of an
  assigned object literal, through TS wrappers).
- **adjacent-overload-signatures**: member lists of programs, blocks, namespaces, classes,
  interfaces and type literals; a member is keyed by name, name kind (plain, private,
  quoted, computed text), `static` and call-signature-ness; any other member resets.
  Helpers: `member_key_name`, `NameKind`, `requires_quoting`.
- **class-literal-property-style**: `fields` reports class getters returning a literal
  (unless a setter of the same static name exists); `getters` reports readonly literal
  fields not assigned in the constructor (assignment, `delete`, `++`/`--`, array or rest
  destructuring, through TS wrappers; the nearest class wins).
- **consistent-type-definitions**: `interface` turns `type T = { ... }` (through
  parentheses) into an interface; `type` turns interfaces into aliases, appending
  ` & Base` per `extends`; no fix inside `declare global`.
- **ban-tslint-comment**: `tslint:enable`/`tslint:disable` with optional `-line` /
  `-next-line`; the fix removes the comment with one character on each side (the one before
  only when the comment doesn't start a line).
- **no-confusing-non-null-assertion**: `=`, `==`, `===`, `in`, `instanceof` with a left side
  ending in an unparenthesized `!`; suggestions remove the `!` (when the left side is the
  assertion) or wrap the left side.

Read to understand (not ported), typescript-eslint `packages/eslint-plugin/src/rules/`:
`array-type.ts`, `consistent-indexed-object-style.ts`, `prefer-function-type.ts`,
`consistent-generic-constructors.ts`, `consistent-type-assertions.ts`,
`no-inferrable-types.ts`, `prefer-for-of.ts`, `adjacent-overload-signatures.ts`,
`no-empty-function.ts`, `class-literal-property-style.ts`,
`consistent-type-definitions.ts`, `ban-tslint-comment.ts`,
`no-confusing-non-null-assertion.ts`, and `src/util/` (`misc.ts`, `isAssignee.ts`,
`scopeUtils.ts`, `getOperatorPrecedence.ts`, `getWrappedCode.ts`,
`getTextWithParentheses.ts`).

## eslint-plugin-unicorn rules

23 rules from eslint-plugin-unicorn 79.0.0, each at 100% of its oracle, all `NO_PRESET`.
Rule files are `rules/unicorn_*.rs`; shared helpers are in `rules/unicorn_util.rs`. Configs
name them `unicorn/<rule>`, in ESLint and oxlint configs alike (oxlint's `jsdoc/` names map
the same way). What we learned, in our words:

- **Shared helpers** (`unicorn_util.rs`). `global_path` names the global an expression
  refers to (`Intl.Collator`): an unshadowed global (never written in the file), or a member
  of `globalThis`/`global`/`self`/`window`, followed through `?:`, `||`/`&&`/`??`,
  sequences, parentheses, TS wrappers, local variable inits and plain assignments, and
  object-pattern keys. `call_to_new` / `new_to_call` switch `Foo()` and `new Foo()`, adding
  parentheses (`new (a().b)()`, a `new` callee, a `return`/`throw`/`yield` argument on a
  later line), keyword spacing and a leading `;` where the statement would join the previous
  one. `is_error_name` is `(Upper[alnum]*)*Error`.
- **Positions.** A `TemplateElement` is reported with its delimiters
  (`ast_util::template_element_span`); a `Directive`'s literal is
  `ast_util::string_literal_span` (its span also covers the `;`). A keyword glued to a
  rewritten literal (`return.5`, `return+new Date`) gets a space.
- **prefer-node-protocol**: specifiers of imports, re-exports, `import()`, `import("x")`
  types, `require(x)` and `process.getBuiltinModule(x)` that are builtins both bare and with
  `node:` (the plugin's static `builtin-modules` list).
- **escape-case**, **number-literal-case**, **numeric-separators-style**,
  **no-zero-fractions**, **text-encoding-identifier-case**: work on the literal's raw text
  (an escape counts after an odd run of backslashes; `utf8`/`utf-8` is fixed only as the
  second argument of `x.readFile[Sync]()`, else suggested; `<meta charset>`, `<form
  accept-charset>` and `new TextDecoder()` always take `utf-8`; separators group from the
  right, fractions from the left only with `fractionGroupLength`).
- **prefer-optional-catch-binding**: a binding with a default counts as used (the
  initialising write is a reference); computed pattern keys keep it.
- **no-console-spaces**, **no-nested-ternary**, **no-lonely-if**, **no-null**,
  **no-this-assignment**, **no-unreadable-iife**, **no-document-cookie**,
  **empty-brace-spaces**, **no-xor-as-exponentiation**: see each file's header. no-lonely-if
  rebuilds the merged `if` as one replacement, moving comments and adding `;` against ASI;
  no-document-cookie follows `document` aliases through variable inits.
- **prefer-date-now**: `new Date()` (with or without `()`) under `.getTime()`/`.valueOf()`,
  `Number()`, `BigInt()`, unary `+`/`-`, arithmetic operators and their compound
  assignments; no fix when a comment is inside the replaced node.
- **no-instanceof-builtins**: `x instanceof Array` (and `Error` with `useErrorIsError`)
  becomes `Array.isArray(x)`; `Function` becomes `typeof x === 'function'`; primitive
  wrappers get that as a suggestion; other builtins only under `strategy: "strict"` or
  `include`. The `typeof` form is parenthesized where the parent binds tighter than `===`.
- **no-typeof-undefined**: `typeof x === "undefined"` (any equality, string or plain
  template), skipping ambient names and, unless `checkGlobalVariables`, globals (then a
  suggestion); the fix keeps comments and parenthesizes operands that would start a
  statement, arrow body, `export default` or `for` init.
- **new-for-builtins**: which builtins need `new` and which must not have it; `Date()`
  becomes `String(new Date())`; `Object(x) ===` comparisons and optional chains are allowed.
- **throw-new-error**: error-named calls (plain or member property) not used as a callee,
  tag or decorator, not optional, not `Data.TaggedError()`; no oracle case needs types, so
  the plugin's type-information check is left out.
- **no-new-array**: `new Array(x)` is fixed to `[x]` only when `x` is statically known and
  not a number (literals, known templates/operators/sequences/literals, `const` bindings
  declared before); a spread gets a `[...x]` suggestion.
- **prefer-type-error**: `throw new Error()` as the whole consequent of an `if` whose test
  is a type check (`typeof` other than existence checks, `instanceof` a non-error,
  lodash/underscore `is*` calls with arguments, `isNaN`/`isFinite`, `!` of one, a binary
  with one on either side, a logical with one on both sides).

Read to understand (not ported), eslint-plugin-unicorn `rules/`: the 23 rule files, and
`fix/{switch-call-expression-to-new-expression,switch-new-expression-to-call-expression,
fix-space-around-keywords,add-parentheses-to-return-or-throw-expression,
replace-node-or-token-and-spaces-before}.js`, `utils/{global-reference-tracker,
needs-semicolon,get-unparenthesized-keyword-argument-parent,
is-in-unparenthesized-new-expression-callee,get-static-value,builtins,numeric,
should-add-parentheses-to-comparison,should-add-parentheses-to-unary-expression,
should-add-parentheses-to-expression-statement-expression,comments}.js`, `ast/` helpers;
`@eslint-community/eslint-utils` (ReferenceTracker); `is-builtin-module`.

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

Round 3 adds 32 more rules (`src/rules/jsdoc_*.rs`; shared helpers in `src/jsdoc_util.rs`):
`require-param-{name,type}`, `require-property-{name,type,description}`, `require-{returns,throws,yields}-{type,description}`,
`require-next-{type,description}`, `require-throws`, `require-yields`, `require-yields-check`,
`require-returns-check`, `check-access`, `check-alignment`, `check-property-names`,
`empty-tags`, `implements-on-classes`, `multiline-blocks`, `no-blank-blocks`,
`no-blank-block-descriptions`, `no-defaults`, `no-multi-asterisks`, `no-types`,
`reject-any-type`, `reject-function-type`, `require-hyphen-before-param-description`,
`tag-lines`. Round 4 adds `check-param-names`, `require-property`,
`require-asterisk-prefix` and `no-missing-syntax`, which needed explicit report locations.
What we learned:

- **Report positions.** A report given a comment-relative line lands at column 0 of that
  line; one given a line and `column: 0` lands at the comment's own column on that line.
  ESLint doesn't clamp a rule's `loc`: it can report past the end of a line (`1:2` on an
  empty first line, `5:9` on a blank line) or past the end of the comment. A byte span can't
  say that, so such reports set `Diagnostic::loc` (`Report::loc`, `Doc::report_at`); the
  span still decides directives, sorting and the editor range. Rules that report at a fixed
  `1:2` (missing options, contradictory `structuredTags`) do the same.
- **Settings errors.** `settings.jsdoc.structuredTags` with a tag `required` to have a
  `name` or `type` it sets to `false` makes every rule report the plugin's error once at
  `1:2` and check nothing (`jsdoc::settings_load`, called by the block iterators).
- **check-param-names** pairs the `n`th undotted `@param` with the `n`th parameter
  (skipping a leading `this` unless the tag is named `this`); the first problem stops the
  block. A destructured root (or a parameter typed with a type literal) expects every
  flattened property path under the tag's name; a missing one is reported on the line
  before the first tag whose path doesn't start with it, counted from the root tag's line
  (so it can land past the comment). Properties of a tag whose type isn't object-like
  (`checkTypesPattern`, a `/re/flags` string) and rest properties (unless
  `checkRestProperty`) aren't required. In the "names to be" message a destructured
  parameter shows the tag at its position and a rest parameter `...name`.
- **no-missing-syntax** counts blocks per (context, `comment`) they were found through and
  reports the first entry short of its `minimum` at `1:2`. Its iteration is additive (the
  plugin's `matchContext`): under object `any` entries every block is visited once per
  entry whose `comment` matches it (`Contexts::additive`). A string entry is checked with
  an empty comment while its blocks are counted with none, so it always reports.
- **Fixes on one block accumulate.** A rule that reports several times on a block mutates the
  same parsed block each time and every fix rewrites the whole comment, so the fixes overlap
  and only the first in source order is applied per pass (tag-lines shows both mutations in a
  later report's fix text). Removing a tag or inserting lines renumbers every line after it;
  inserted lines belong to no tag; lines made by replacing the block description are
  numbered 0 and the first line always has an empty start.
- **Contexts.** Rules that declare no default contexts ignore the `contexts` option. Rules
  that do (`require-yields`, the returns rules) check only blocks whose node matches one of
  the listed contexts.
- **Yield and return walks.** The yield search skips nested functions, class bodies, call
  callees, `new` and `await` operands; with `next` only a yield used directly as an
  initializer counts. A `MethodDefinition` itself never "has a yield value". The returns and
  throws checks need *every* branch to end in the statement (`if` with both arms, `switch`
  with a default, `try` with `finally`).
- **Undefined-ish types.** A `@returns`/`@yields` type is "maybe undefined" when its
  top-level union mentions `undefined` or `void`, or it names a `@typedef` whose type does;
  those blocks don't need a matching return or yield. Type checks in reject-*-type and here
  use a light scanner of the type text, not a full jsdoc type parser.
- **no-restricted-syntax** reports a block found through an entry of its own `contexts`
  (`settings.jsdoc.contexts` is ignored; with no options it visits the four function types
  and reports the missing option on each block). The block carries the `comment` selector it
  was found through (`Doc::info_comment`: the entry's under listeners, the first matching
  `comment` when iterating every block, none under a plain `any`). An entry applies when it is
  a string whose selector matches the node, or an object whose `context` matches the node (or
  is missing or `any`) and whose `comment` is that same selector; the first such entry gives
  the message (`message`, or `Syntax is restricted: {{context}}` plus ` with {{comment}}`).

Read to understand the rules (not ported): eslint-plugin-jsdoc `src/iterateJsdoc.js`
(iterateAllJsdocs, findContext), `src/jsdocUtils.js` (getContextObject),
`src/jsdocUtils.js`, `src/utils/hasReturnValue.js`, `src/rules/requireParamDescription.js`,
`src/rules/requireReturns.js`, `src/rules/escapeInlineTags.js`, and for round 3
`src/rules/{requireParamName,requireParamType,requireReturnsDescription,requireReturnsType,
requirePropertyName,checkAccess,emptyTags,implementsOnClasses,noDefaults,noTypes,
checkAlignment,noMultiAsterisks,noBlankBlocks,noBlankBlockDescriptions,
requireHyphenBeforeParamDescription,multilineBlocks,requireThrows,requireYields,
requireYieldsCheck,requireReturnsCheck,checkPropertyNames,tagLines,noRestrictedSyntax}.js`,
and for round 4 `src/rules/{checkParamNames,requireProperty,requireAsteriskPrefix,
noMissingSyntax}.js` and `src/jsdocUtils.js` (`getFunctionParameterNames`,
`overrideTagStructure`, `getRegexFromString`),
`src/buildRejectOrPreferRuleDefinition.js`, `src/buildForbidRuleDefinition.js`,
`src/getDefaultTagStructureForMode.js`, `src/index-cjs.js`; `@es-joy/jsdoccomment`
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
  Tag types are parsed for a subset of jsdoc-type-pratt-parser's typescript mode
  (`parse_type`): names, numbers, quoted strings, `null`, `undefined`, `*`, `?`, property
  paths (`JsdocTypeNamePath` with a `JsdocTypeProperty` right side), generics (`A<B>`,
  `A.<B>`, and `B[]` as a `square` generic of `Array`), object types (`JsdocTypeObject` of
  `JsdocTypeObjectField`s, whose `key` is a plain string), parentheses and unions. Anything
  else gets `parsedType: null`, as a type that fails to parse does. Children follow the type
  parser's visitor keys (`left`, `right`, `element`, `elements`); nested plain objects such
  as `meta.quote` / `meta.brackets` / `meta.dot` / `meta.separator` are attributes.

Read to understand (not ported): esquery `dist/esquery.esm.js`; ESLint
`lib/linter/esquery.js`, `lib/linter/source-code-traverser.js`. The type shapes come from
observed `jsdoc-type-pratt-parser` output (`parse(type, 'typescript')`) and its
`visitorKeys`. The ESTree shape comes from
typescript-estree's output (types, ranges, parents) and `@typescript-eslint/visitor-keys`.

## Patterns

- typescript-eslint extension rules reuse the core rule: make the core rule type
  `pub(crate)` or add `pub(crate) fn create_ts`, and keep the `ts_*` module to its `RuleDef`.
- Fixes that rewrite import/export lists build a list of `(Span, String)` edits and fold them
  into one `Fix` (see `ts_consistent_type_imports.rs`); keep edits non-overlapping.

## eslint-plugin-unicorn rules (round 5, lead)

`prefer-blob-reading-methods`, `consistent-date-clone`, `no-invalid-remove-event-listener`,
`prefer-structured-clone`, `prefer-event-target`, `no-useless-error-capture-stack-trace`,
`no-negation-in-equality-check`, `no-exports-in-scripts`, `consistent-tuple-labels`,
`no-leading-empty-lines`, `prefer-add-event-listener-options` (each at 100% of its oracle), `no-await-in-promise-methods`
(110 of 111) and `no-unnecessary-await` (63 of 66): their last cases need the parser's
module goal, see below. Shared call shapes are
in `rules/unicorn_ast.rs`: `method_call` (a call of `obj.name` with an identifier property,
parentheses transparent as in ESTree), `promise_method_call`, `matches_path` (`_.cloneDeep`,
`new.target.x`, `this.y`), `call_tokens` (a call's `(`, `)` and trailing `,`),
`would_remove_comments` (a comment in the replaced range but outside every kept range) and
`skip_spaces` (JavaScript's `\s`).

- **no-await-in-promise-methods** walks up from each `await` to an array that is the single
  argument of `Promise.all/allSettled/any/race`, stopping at a function or an outer `await`;
  only an `await` that is the element itself gets the "Remove `await`" suggestion.
  `Promise.all([\n await\n promise])` in a `.js` module needs the module goal (see
  "Parser options").
- **consistent-date-clone** reports `.getTime()` in `new Date(x.getTime())` from the method
  name to the end of the call; the fix deletes from the end of the receiver's parentheses to
  the end of the member, and from the end of the parenthesized callee to the end of the call.
- **no-unnecessary-await** fixes only an `await` that is the last thing its function or
  module evaluates (`is_last_evaluated`: the end of an arrow body, function body or
  program, through `if` arms, declarations, `return`/`throw` and expression statements).
  `await +x` and `await\n[]` at the top level of a `.js` module need the module goal.
- **no-negation-in-equality-check** and the fix helpers it shares with
  `no-typeof-undefined` (`keyword_argument_parent`, `start_needs_parens`) and
  `no-instanceof-builtins` (`starts_continued_statement`).
- **no-useless-error-capture-stack-trace**: the nearest function or class around the call
  must be the constructor of a class whose `extends` is a global built-in error;
  `new.target`, `this.constructor` and the class's own single-declaration name count as
  the class.
- **prefer-event-target**: `EventEmitter` as superclass or `new` callee, unless declared by
  an import of (or a `const` destructuring / `.EventEmitter` of `require`/`await import`
  of) `@angular/core` or `eventemitter3`.
- **no-exports-in-scripts** (a `#!` first line), **consistent-tuple-labels**,
  **no-leading-empty-lines**, **prefer-add-event-listener-options**: see each file's header.
- **A leading BOM** is stripped before linting, by the CLI (`project::lint_one`) and by the
  oracle harness, which puts it back in front of fixed output; `gen-rule-oracle.mjs` applies
  fix ranges to the BOM-less text the same way (ESLint's ranges index text without it).
- **prefer-structured-clone**'s JSON suggestion deletes `JSON.stringify` with each
  parenthesis around it, the inner call's `(`, `)` and trailing comma; the clone-function
  form replaces the callee inside its parentheses.
- **prefer-global-this**, **no-named-default**: see each file's header.
- **prefer-string-repeat** skips literals the syntax needs (`unicorn_ast::is_string_literal_required`:
  non-computed keys and any key of an abstract member or signature, module sources and
  specifiers, import attributes, JSX attribute values, enum members, module names,
  `require` references, any child of a type) and `expect(x).toMatchInlineSnapshot(lit)`.
- **remove_argument** (`unicorn_ast.rs`) removes one call argument with its parentheses
  and one comma, keeping comments in the removed range (no-unnecessary-array-flat-depth).
- **no-magic-array-flat-depth** ignores a disable/enable directive in the argument list;
  any other comment there explains the depth and silences it.
- **no-unnecessary-array-flat-depth**, **consistent-template-literal-escape**,
  **no-break-in-nested-loop**, **no-useless-re-export**, **prefer-global-number-constants**
  (`is_left_hand_side` in `unicorn_ast.rs`), **max-nested-calls**,
  **prefer-short-escape-sequences**: see each file's header.
- **no-unnecessary-slice-end** and **no-unnecessary-array-splice-count** share
  `unicorn_length_or_infinity.rs`; **no-array-reverse** and **no-array-sort** share
  `unicorn_array_mutate.rs`. `contains_node` (`unicorn_ast.rs`) walks a subtree.
- **no-async-promise-finally** uses type information only to skip a receiver known not to
  be a promise (every non-nullable union member lacks a callable `then`); `any`/`unknown`
  stay reported.
- **remove_statements** (`unicorn_ast.rs`, `removeStatement`): removes statements with their
  own lines when nothing else shares them, or writes `;` when the previous token would
  otherwise run into the next statement (no-useless-continue).
- **no-useless-continue**, **no-array-concat-in-loop**, **no-duplicate-set-values**,
  **no-optional-chaining-on-undeclared-variable** (scopes walked outwards; type-only
  declarations and imports don't declare a runtime name), **prefer-response-static-json**:
  see each file's header.
- **prefer-promise-static-methods** withholds its fix when the executor has typed
  parameters, a return type or type parameters, when `new Promise<T>` would hand its type
  arguments to `reject`, or when a comment would be lost.
- **no-useless-boolean-cast** treats parentheses as the end of an optional chain
  (`(a?.b).c` is not an optional-chain result), as ESTree's `ChainExpression` does.
  **no-duplicate-logical-operands**: see the file's header.
- **prefer-block-statement-over-iife** treats `script` and `commonjs` alike for block-level
  function declarations. **prefer-number-coercion** finds calls with `global_path`.
- **prefer-promise-try** reuses `concise_arrow_body_text` (`unicorn_no_useless_boolean_cast.rs`).
- **no-useless-override** does not yet read types: the plugin reports `async` forwards of a
  promise-returning call and bare `super.m()` statements of a `void` call only with type
  information, so here those stay unreported even in typed runs (no oracle case has types).
- **prefer-class-fields** writes a new private field with its `#` (the plugin writes the bare
  name, which would declare a public field; no oracle case covers it).

Read to understand (not ported), eslint-plugin-unicorn `rules/`: `prefer-blob-reading-methods.js`,
`no-await-in-promise-methods.js`, `consistent-date-clone.js`,
`no-invalid-remove-event-listener.js`, `prefer-structured-clone.js`, `ast/is-method-call.js`,
`ast/call-or-new-expression.js`, `utils/is-node-matches.js`, `utils/comments.js`,
`fix/remove-method-call.js`, `fix/remove-spaces-after.js`, `no-unnecessary-await.js`,
`no-negation-in-equality-check.js`, `no-useless-error-capture-stack-trace.js`,
`prefer-event-target.js`, `no-exports-in-scripts.js`, `consistent-tuple-labels.js`,
`no-leading-empty-lines.js`, `prefer-add-event-listener-options.js`, `utils/needs-semicolon.js`,
`fix/add-parentheses-to-return-or-throw-expression.js`, `prefer-global-this.js`,
`no-named-default.js`, `prefer-string-repeat.js`, `utils/is-string-literal-required.js`,
`no-unnecessary-array-flat-depth.js`, `no-magic-array-flat-depth.js`, `fix/remove-argument.js`,
`utils/comments.js`, `utils/eslint-directive.js`, `consistent-template-literal-escape.js`,
`no-break-in-nested-loop.js`, `no-useless-re-export.js`, `prefer-global-number-constants.js`,
`utils/is-left-hand-side.js`, `max-nested-calls.js`, `prefer-short-escape-sequences.js`,
`no-async-promise-finally.js`, `utils/is-promise-type.js`, `utils/types.js`,
`shared/no-unnecessary-length-or-infinity-rule.js`, `no-unnecessary-slice-end.js`,
`no-unnecessary-array-splice-count.js`, `utils/contains-node.js`,
`shared/no-array-mutate-rule.js`, `utils/is-node-value-not-function.js`,
`no-useless-continue.js`, `fix/remove-statement.js`, `utils/needs-semicolon.js`,
`no-array-concat-in-loop.js`, `shared/array-concat-in-loop.js`, `no-duplicate-set-values.js`,
`utils/get-duplicate-array-elements.js`, `no-optional-chaining-on-undeclared-variable.js`,
`utils/imports.js`, `prefer-response-static-json.js`, `prefer-queue-microtask.js`,
`utils/is-node-contains-lexical-this.js`, `utils/is-value-not-usable.js`,
`prefer-location-assign.js`, `no-constant-zero-expression.js`,
`prefer-identifier-import-export-specifiers.js`, `consistent-existence-index-check.js`,
`utils/should-add-parentheses-to-comparison.js`, `utils/get-precedence.js`,
`no-unnecessary-nested-ternary.js`, `utils/should-add-parentheses-to-logical-expression-child.js`,
`utils/should-add-parentheses-to-conditional-expression-child.js`, `require-css-escape.js`,
`utils/is-node-value-not-dom-node.js`, `explicit-timer-delay.js`,
`require-array-sort-compare.js`, `fix/append-argument.js`, `fix/replace-argument.js`,
`prefer-array-slice.js`, `prefer-promise-static-methods.js`, `no-useless-boolean-cast.js`,
`utils/boolean.js`, `utils/function-body.js`, `utils/get-concise-arrow-body-text.js`,
`no-duplicate-logical-operands.js`, `utils/comparison.js`, `utils/is-reference-context.js`,
`prefer-block-statement-over-iife.js`, `ast/is-direct-eval-call.js`, `prefer-number-coercion.js`,
`prefer-unary-minus.js`, `ast/is-negative-one.js`, `utils/should-add-parentheses-to-unary-expression.js`,
`prefer-promise-try.js`, `no-invalid-fetch-options.js`, `no-misrefactored-assignment.js`,
`no-useless-compound-assignment.js`, `no-unsafe-string-replacement.js`,
`utils/get-const-variable-initializer.js`, `no-double-comparison.js`,
`utils/get-comparison-operand-text.js`, `prefer-array-from-range.js`, `shared/array-range.js`,
`no-useless-override.js`, `utils/member-expression.js`, `fix/replace-node-or-token-and-spaces-before.js`,
`prefer-class-fields.js`, `utils/get-indent-string.js`.

## eslint-plugin-unicorn type guessing (round 5)

Read to understand (not ported), eslint-plugin-unicorn 79.0.0 `rules/utils/`:
`type-helpers.js`, `type-check.js`, `jsdoc.js`, `jsdoc-type.js`, `parse-jsdoc-type.js`,
`get-static-value.js`, `unwrap-typescript-expression.js`, `get-visitor-child-nodes.js`,
`is-array.js`, `is-string.js`, `is-number.js`, `is-map.js`, `is-set.js`, `is-bigint.js`,
`is-reg-exp.js`, `is-weak-map.js`, `is-weak-set.js`, `should-skip-known-non-array-receiver.js`,
`global-reference-tracker.js`, `is-left-hand-side.js`, `shared/typed-array.js`; the rules
`prefer-number-properties.js`, `prefer-string-trim-start-end.js`, `error-message.js`; and
`@eslint-community/eslint-utils` (`getStaticValue`, `ReferenceTracker`).

Many unicorn rules ask whether an expression is an array, string, number, `Map`, ... with
no type checker. Two modules answer the way the plugin does, so the rules stay at 100% with
`NoTypeInfo`:

- **`unicorn_static.rs`: static values.** `static_value` is ESLint's `getStaticValue`
  (literals, templates, operators, `const` bindings that are never mutated, a fixed list of
  pure built-in calls and members). `static_value_no_side_effects` refuses expressions that
  have side effects (calls, `new`, assignments, updates, `await`, `yield`, `delete`, also
  inside the `const`s they read) or read a member that could have changed;
  `Object.freeze`/`seal`/`preventExtensions` of a static value pass their argument through.
  `static_value_control_flow` also needs every binding on an evaluated path to be a `const`
  declared before the use. Evaluated children skip functions, classes, type nodes,
  non-computed member names and plain property keys.
- **`unicorn_types.rs`: the guessing engine.** A `Checker` lists the target type names, the
  names known not to be it, aliases (`ReadonlyMap` is `Map`), target imports, constructor and
  call names, and hooks for node shapes, annotations, JSDoc types and static values. It
  answers `Target`, `NonTarget`, `Nullish` or `Unknown`, in this order: `satisfies` looks
  through; an `as`/`<T>` annotation if it says something; otherwise a JSDoc type on the node
  (which wins, unknown included); then the expression (a variable's annotation or `const`
  initializer, a local function's annotated or documented return type, `=`, `this`/`super`/
  `new` from class declarations when `class_syntax` is on, the last of a sequence,
  `?:`/`||`/`&&`/`??` as unions); then a static value (not for identifiers, and unknown when
  the expression reads a JSDoc-typed binding); then the node-shape hooks. Unions are the
  target when every member is (any member with `mixed_union_target`), non-target when every
  member is non-target (any with `mixed_union_non_target`); `null`/`undefined` count as
  non-target unless `nullish_in_mixed_union` drops them. Intersections are unknown if any
  member is, else the target if any member is. Type references resolve through local type
  aliases, interfaces (`extends`), type parameters (their constraint), classes (`extends`,
  when `class_heritage` is on, the default) and imports; `prefer_definitions` resolves a
  local definition before trusting the name. Every walk keeps a visited list and a depth
  limit.
- **JSDoc.** Only `@param`/`@arg`/`@argument`, `@type` and `@return(s)` tags are read, and only
  at line starts. The comment is the one directly before the first owner node that has a
  comment (for a function: the function, its TS wrappers, then the declarator, the
  single-declarator declaration, `x = fn` statement, property or method), and it must be a
  `/**` block. Any `@typedef`, `@callback` or `@overload` means no tags. Exactly one tag must
  match. The type reader understands names, primitives (with boxed aliases), unions,
  intersections, `?T`, `T=`, `T[]`, tuples, records (non-primitive objects), functions
  (`() => T`, `function(): T`) and literals; anything else is unknown.
- **Checkers.** `is_array` / `is_known_non_array` (definitions first, no class heritage,
  rest parameters, `Array.from`/`of`, `Object.keys`-style calls, array-returning methods on
  arrays), `is_indexed_collection` (arrays and typed arrays), `is_string` (also `+` with a
  string, string-returning methods on strings, `join` on indexed collections, `Date`
  string methods, `RegExp` `.source`/`.flags`), `is_number` (also arithmetic, `Math.*`,
  `.length` of strings/indexed collections/functions, `.size` of maps and sets, number
  methods on strings/arrays/dates), `is_bigint`, `is_map`, `is_set`, `is_weak_map`,
  `is_weak_set`, `is_regexp`, `is_date`. A literal of another type is not "known non-X":
  only annotations, JSDoc and the node hooks say non-target.
- **Global references.** `prefer-number-properties` reports what ESLint's reference
  tracker yields: direct reads of an unmodified global, and properties of the global object
  (`globalThis.parseInt`, through `const g = globalThis`). Reads through an alias of the
  function itself (`const p = parseInt; p()`) are not reported.
- **Fix text** (`unicorn_text.rs`). Parentheses as ESTree sees them (a node's own `Paren`
  parents), comments a replacement would drop, `getMemberExpressionObjectText`,
  `shouldAddParenthesesToUnaryExpressionReplacement`, `needsSemicolon` (read from the token
  before the replaced node and the deepest node ending there), `isSameReference`,
  `hasPossibleSideEffect`.
- **Known parser gap.** `unicorn/prefer-includes` is 243/247: the 4 failing cases are
  `x < 0 as any ** 2` and `x < 0 satisfies boolean as any ** 2`. tsc and typescript-estree
  keep parsing tighter binary operators after `as T` / `satisfies T`, but ts_r_parser stops
  there and reports "';' expected". The fix belongs in the parser, not in the rule.

## eslint-plugin-unicorn rules (round 5, h4)

Current: catch-error-name 120/120, consistent-function-scoping 170/170, no-array-reduce
482/482, no-await-expression-member 71/72, no-empty-file 58/58, no-for-each 431/431,
no-negated-condition 134/134, no-object-as-default-parameter 62/62, no-process-exit 58/58,
no-static-only-class 91/91, no-thenable 178/178, no-useless-fallback-in-spread 61/61,
no-useless-length-check 165/165, no-useless-promise-resolve-reject 121/121,
no-useless-spread 283/283, no-useless-switch-case 34/34, no-useless-undefined 271/271,
prefer-export-from 158/158, prefer-module 152/152, prefer-switch 68/68,
prefer-top-level-await 228/228, switch-case-braces 56/56.

Shared helpers of this round live in `unicorn_util_h4.rs` (method calls, identifier
validity, free names, renames, ESTree identifier spans, same-line trailing comments,
indentation, line breaks, `hasSideEffect` with the getter/conversion options, switch-case
heads, keyword-argument parents, token neighbours, `fix_of`, `is_typescript_path`).

- **Oracle data.** The generator writes RegExp-valued options as `{regex, flags}` (they
  used to serialize as `{}`), and turns `reportUnusedDisableDirectives` off: a test with a
  stray `eslint-disable` comment otherwise expects ESLint's directive-removal autofix as
  the rule's output.
- **Fix edit order.** Several fixes put more than one insertion at the same offset (the
  `case` labels and `{` of prefer-switch, `break;` and `}` after a body). ESLint merges a
  fix's edits sorted by range, keeping the order they were produced in for equal ranges;
  `fix_of` does the same (a stable sort), so produce edits in the plugin's order.
- **Extending a fix range.** no-for-each makes its fix cover the call's parent so two
  nested fixes don't interleave; empty insertions at both ends of the parent do that.
- **catch-error-name.** The new name comes from `available_name` (the wanted name, then
  `_` appended until no scope in the catch clause's chain declares or reads it); the
  rename edits every reference with ESTree spans (a shorthand property gets `key: name`).
- **consistent-function-scoping.** The scopes a function "sits in" are the scope owning
  its block (a function, catch clause, static block...), plus the loop's scope and each
  bare block's scope when the block is a loop body. A variable used by the function makes
  it stay when it is declared in one of those scopes (unless it is just a function's own
  name), referenced from one, a parameter of the owning function, or a sibling function
  declaration. Arrows that read `this`/`super`/`new.target` stay; so do React hook
  callbacks' inner functions, IIFE bodies, and `jest.mock`/`vi.mock`/`vi.hoisted`
  factories.
- **no-await-expression-member: 71/72.** The missing case is a file without
  `import`/`export` holding `const foo = (await -x)[0];`. ts_r_parser follows tsc's script
  rule and reads `await - x` as a binary expression; ESLint parses the file as a module.
  The fix belongs in the parser (module goal), not the rule.
- **no-empty-file.** A file with only a BOM is reported at 1:1-1:1: the BOM is not part
  of ESLint's text, so locations start after it.
- **no-for-each.** The fix needs the receiver to be known as an array (`is_array`), one
  plain or two simple parameters, and that no identifier in the call with a parameter's
  name resolves outside the callback (or is unresolved). `return x;` becomes
  `x; continue;`, braced when the `return` is an `if` branch, with a `;` first when the
  previous token would join it.
- **no-array-reduce.** The `Math.sumPrecise` suggestion ignores parameters whose annotation
  can't be a number (`a: string`); h3's `is_known_non_number` only reads references, not a
  parameter's own annotation. The loop fix's indentation unit is the most common step
  between consecutive space-indented lines when spaces dominate, else a tab.
- **no-useless-undefined.** Removing `undefined` takes its parentheses and the whitespace
  before each, keeping a line break only when that whitespace starts with one. A default
  `= undefined` on a typed parameter becomes `a?: T`; in a `.ts`-family file an untyped one
  becomes `a?`.
- **no-useless-spread.** Unwrapping `[...x]` adds parentheses when `x` binds looser than a
  member access, gives up for a multi-line `yield`, and parenthesizes a multi-line
  `return`/`throw` argument instead of fixing keyword spacing.
- **prefer-switch.** A chain ends at the first `if` whose comparisons don't share the
  current discriminant candidates; the candidates kept are the ones before that `if`.
  Member references compare their computed keys after folding literals (`a["b" + ""]` is
  `a.b`), which `is_same_reference` alone doesn't do.
- **prefer-top-level-await.** Without type information `isKnownNonPromise` is always
  false, so every unawaited promise-like top-level call is reported.
- **prefer-export-from.** A new `export ... from` goes at the end of the source text (the
  `Program` end, trailing comments and whitespace included). A type-only import or export
  joins a `type` export of the same source if there is one, else a value one with an
  inline `type`.
- **no-thenable.** Under espree a shorthand `export {then}` is one identifier node playing
  both roles, so it is reported twice; typescript-eslint makes two nodes and reports once.

Read to understand (not ported), all from eslint-plugin-unicorn 79.0.0 `rules/`: the 24
rule files of the list above, plus `utils/get-available-variable-name.js`,
`fix/rename-variable.js`, `fix/replace-reference-identifier.js`,
`utils/get-switch-case-head-location.js`, `utils/get-indent-string.js`,
`utils/get-linebreak.js`, `utils/comments.js`,
`fix/replace-node-or-token-and-spaces-before.js`, `utils/eslint-directive.js`,
`utils/needs-semicolon.js`, `utils/get-unparenthesized-keyword-argument-parent.js`,
`fix/add-parentheses-to-return-or-throw-expression.js`,
`utils/should-add-parentheses-to-expression-statement-expression.js`,
`utils/is-logical-expression.js`, `ast/is-method-call.js`,
`ast/call-or-new-expression.js`, `ast/is-empty-node.js`, `ast/is-undefined.js`,
`utils/unwrap-typescript-expression.js`, `fix/remove-spaces-after.js`,
`fix/remove-parentheses.js`, `utils/escape-string.js`,
`utils/is-in-unparenthesized-new-expression-callee.js`,
`utils/get-enclosing-function.js`, `utils/is-known-non-iterable.js`,
`fix/replace-member-expression-property.js`, `utils/numeric.js`,
`utils/get-class-head-location.js`, `utils/get-call-argument-text.js`,
`utils/get-concise-arrow-body-text.js`, `utils/get-expression-text.js`,
`utils/get-precedence.js`, `utils/should-add-parentheses-to-await-expression-argument.js`,
`utils/single-statement.js`, `utils/is-same-reference.js`,
`utils/has-possible-side-effect.js`, `utils/is-side-effect-free-function.js`,
`utils/reindent-text.js`, `utils/block-scope.js`, `utils/is-promise-type.js`,
`utils/is-call-expression-value-discarded-with-void.js`, `fix/remove-specifier.js`,
`utils/imports.js`, `utils/is-node-contains-lexical-this.js`, `utils/get-references.js`,
`utils/comparison.js`, `utils/is-reference-context.js`, `utils/is-typescript-file.js`,
`fix/remove-argument.js`, `fix/fix-space-around-keywords.js`,
`utils/is-strong-precedence-node.js`, `utils/is-function-self-used-inside.js`,
`ast/is-reference-identifier.js`, `utils/function-body.js`,
`utils/is-node-value-not-function.js`, `utils/array-or-object-prototype-property.js`,
`utils/has-optional-chain-element.js`, `utils/get-const-variable-initializer.js`,
`utils/get-indent-unit.js`, `utils/get-line-indent.js`; and from
`@eslint-community/eslint-utils`, `hasSideEffect`, `getFunctionHeadLocation` and
`getFunctionNameWithKind`.
