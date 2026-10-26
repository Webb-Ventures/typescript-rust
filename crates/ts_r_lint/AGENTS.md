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

## Status (v1 lint round 5, `wip/v1-lint5`)

- 473 rules (134 core ESLint, 95 typescript-eslint, 48 eslint-plugin-jsdoc, 196
  eslint-plugin-unicorn), 51 of them type-aware. Rule oracle: 43,855 / 43,874 cases. The
  non-type-aware cases are 36,953 / 36,954 and the type-aware cases 6,902 / 6,920
  (2026-10-27); the 18 type-aware failures are a subset of the 46 listed in design record
  0006, and the one other is unicorn/require-module-specifiers (`declare export type {} from
  "foo"` in a `.js` file, a parser gap). `tests/oracle/baseline.txt` has the per-rule counts.
  Every rule of ESLint's, typescript-eslint's and eslint-plugin-jsdoc's recommended presets
  is implemented; unicorn's `recommended` has 196 of 338.
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
Rule files are `rules/unicorn_*.rs`; shared helpers are in `rules/unicorn_util.rs` (one module:
round 6 folded the per-helper `unicorn_util_h1/h2/h4/h5` modules into it, keeping one copy
of each helper), `unicorn_text.rs` (fix text: parentheses, comments, expression and member
object text, `needsSemicolon`), `unicorn_ast.rs` (call shapes, statement removal, line
breaks), `unicorn_static.rs` and `unicorn_types.rs`. Configs
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

## eslint-plugin-import rules (round 6)

21 of eslint-plugin-import 2.32.0's 46 rules, the ones that need no module resolution or
export maps, all `NO_PRESET`: consistent-type-specifier-style, dynamic-import-chunkname,
exports-last, first, max-dependencies, newline-after-import, no-absolute-path, no-amd,
no-anonymous-default-export, no-commonjs, no-duplicates, no-dynamic-require,
no-empty-named-blocks, no-import-module-exports, no-mutable-exports, no-named-default,
no-namespace, no-nodejs-modules, no-unassigned-import, no-webpack-loader-syntax and
prefer-default-export. Rule files are `rules/import_*.rs`; `rules/import_util.rs` holds the
module visitor. Configs name them `import/<rule>` (ESLint and oxlint configs alike). The
oracle data comes from the plugin's own tests (`gen-rule-oracle.mjs --import-tests`, see its
header). What we learned, in our words:

- **Module visitor** (`import_util::module_refs`). The specifiers a file names: the source
  of `import` declarations, `export ... from` and `export * from`, and `import("x")` with a
  string literal (ES modules, on unless `esmodule: false`); `require("x")` with exactly one
  string-literal argument (with `commonjs`; the importer is the call); and for AMD (`amd`)
  each string element of the array of a two-argument `require([...], f)` / `define([...],
  f)` other than the magic `require` and `exports` (the importer is the element).
  `import x = require("y")` is not visited. Parentheses don't count (ESTree drops them).
- **Scopes.** `no-amd` only reports where the call's innermost scope is the module scope
  (so never in scripts or functions). `no-commonjs` reports `require` only when the
  `var` scope is the module scope, and lets a `require` under an `if`, `try`, `?:` or a
  logical expression through unless `allowConditionalRequire: false`; `exports.x` is
  reported unless the innermost scope itself declares `exports`. `no-mutable-exports` looks
  up the names of `export { a }` and `export default a` in the export statement's scope
  only. `no-import-module-exports` looks at the last scope (in creation order) that declares
  `module`/`exports`: anything but the module scope, or an import binding, means it is not
  CommonJS.
- **Positions and fixes.** `newline-after-import` reports a point at the import's last line,
  at its start column when it fits one line and column 1 otherwise, and counts lines to the
  next statement (a decorated class from its first decorator) or, with
  `considerComments`, to a comment starting within `count + 1` lines (top-level only).
  `first` moves misplaced imports in one edit on the last import that no earlier code
  references; the ones before it get an empty fix, the later ones none.
  `no-duplicates` merges named specifiers by splitting the brace text on `,` (whitespace
  and comments kept), skipping names the first import already has.
- **Not done here.** The plugin skips its package's entry point in
  `no-import-module-exports`, and keys `no-duplicates` by the resolved path; both need a
  resolver, so specifiers are compared as written (the oracle's 26 `no-duplicates` misses
  are all resolver cases, most of them a "Resolve error" the generator recorded for the
  webpack and TypeScript resolvers it did not have). Of the plugin's `recommended` eight
  only no-duplicates is here; no-unresolved, named, namespace, default, export,
  no-named-as-default and no-named-as-default-member need resolution and export maps, and
  so do extensions, order, no-cycle, no-deprecated, no-internal-modules,
  no-relative-packages, no-relative-parent-imports, no-restricted-paths, no-self-import,
  no-useless-path-segments, no-extraneous-dependencies and no-unused-modules.
  enforce-node-protocol-usage, group-exports, no-default-export, no-named-export,
  unambiguous and imports-first (a deprecated alias of first) have no oracle data yet: their
  tests did not load or throw under ESLint 10 (`context.parserOptions`).

Read to understand (not ported): `eslint-module-utils/moduleVisitor.js`;
`src/rules/{no-amd,no-commonjs,no-dynamic-require,max-dependencies,exports-last,
no-mutable-exports,no-absolute-path,first,newline-after-import,no-duplicates,
no-empty-named-blocks,no-namespace,no-named-default,no-nodejs-modules,no-unassigned-import,
no-webpack-loader-syntax,no-anonymous-default-export,consistent-type-specifier-style,
prefer-default-export,no-import-module-exports,dynamic-import-chunkname}.js` and
`src/core/{importType,staticRequire}.js` of eslint-plugin-import 2.32.0; its
`tests/src/utils.js` and `tests/src/rule-tester.js` for the generator.

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
- **Parser options.** `engine::parse_for_lint` sets `ParseOptions::module` for
  `sourceType: "module"` (top-level `await` is the operator, as espree reads it) and
  `assertion_continues` (`x < 0 as any ** 2` is `((x < 0) as any) ** 2`, as TypeScript 5
  and typescript-estree parse it; tsc 7 rejects it, microsoft/TypeScript#63527).

## jsdoc rules (round 5: require-param, require-jsdoc, check-tag-names, ts-no-empty-object-type)

eslint-plugin-jsdoc 63.3.3; its tests are the oracle. What we learned, in our words:

- **require-param** (`src/rules/jsdoc_require_param.rs`). Runs with the plugin's
  `noTracking` iteration (`Contexts::no_tracking`): under `any`, every node that finds a
  block checks it, even a block another node already claimed. Blocks the plugin's
  `avoidDocs` skips (replacing tags, constructors unless `checkConstructors`, accessors
  unless asked, `exemptedBy`) or that carry `@type` are left alone; `this` never needs a
  tag. A destructured root uses its own name, the name of the plain `@param` in its
  position, or the next generated name (`unnamedRootBase` entries, then the last one plus a
  counter from `autoIncrementBase`); every flattened property path under it is required
  too (rest properties only with `checkRestProperty`, nothing under a tag whose type is not
  an object type per `checkTypesPattern`). With `interfaceExemptsParamsCheck`, a parameter with
  its own type annotation (and no default) counts as a root with no property paths. The fixer inserts one
  line per missing tag into the same mutated block: a root that fills an unnamed `@param`
  replaces it; anything else goes before the first tag documenting a later parameter, else
  after the last `@param`. The plugin computes that position as a count of tag lines and
  then uses it as a tag index; we keep that arithmetic so fixes match. A one-line block is
  made multi-line first. An arrow in a variable initializer whose return type is a function
  type borrows the declaration's comment.
- **require-jsdoc** (`src/rules/jsdoc_require_jsdoc.rs`, `src/jsdoc_export.rs`). Runs on
  the ESTree view: built-in listeners for the six `require` types (arrows and function
  expressions only where assigned, default-exported, initialising a variable or a property
  value, unless `checkAllFunctionExpressions`), plus `contexts` (selectors or
  `{context, inlineCommentBlock, minLineCount}`; naming a built-in type turns its check
  on). A node passes when it has a block (with `skipInterveningOverloadedDeclarations`,
  the block before the overload signatures above it), is shorter than `minLineCount`, is an
  overloaded implementation (`exemptOverloadedImplementations`), is an accessor or
  constructor `exemptSpecial`/`checkGetters`/`checkSetters`/`checkConstructors` leave out
  (paired vs. unpaired accessors), an empty function or constructor with
  `exemptEmptyFunctions`/`exemptEmptyConstructors` (no parameters, no returned value), or
  a method implementing an interface member that has a block. With `publicOnly` only
  exported nodes are reported; the export test mirrors the plugin's export parser: a
  symbol table of the top-level statements (bounded steps and depth) following `export`
  modifiers, `export default`/`export {}`, `module.exports`/`exports.x` (cjs) and
  `window.x` (window), where finding a node's own declaration only follows `body` fields.
  The report spans the node's first line (to column 0 of the next); the fix inserts an
  empty block (or `/** msg */` with `inlineCommentBlock`) before the node's reduced form
  or its first decorator, indented like that line.
- **check-tag-names** (`src/rules/jsdoc_check_tag_names.rs`). Every block once. A tag is
  known when it is a name or alias in the mode's table (`jsdoc`; `typescript` = jsdoc plus
  TSDoc, TypeDoc and `import`/`internal`/`jsx`/`overload`/`satisfies`/`template`;
  `closure`/`permissive` = typescript minus `inheritdoc`/`internal`/`overload`/`returns`/
  `satisfies` plus Closure's own tags, with `return` taking `returns` as alias; an unknown
  mode warns once per file at 1:2 and uses jsdoc) or comes from `definedTags`,
  `tagNamePreference` keys or truthy values (an object's `replacement`), `structuredTags`
  keys, or `template` with `typed`. The preferred name: a name that is a preference value
  stays, a preference key (with or without a `tag ` prefix; the last such key wins) gives
  its value, an alias gives its canonical tag. `false` or an object without `replacement`
  blocks the tag (its message or ``Blacklisted tag found (`@x`)``, no fix); another falsy
  value accepts it; a different name is reported with a fix that renames the first `@x`
  ending at a word boundary in the original comment text. A preference value that is not
  falsy, a string or an object reports a settings error at 1:2 for every block. With
  `typed`: tags always redundant under a type system, tags redundant outside an ambient
  context (not when the block has no node, not for a top-level node of a `.d.ts`, not when
  the node or an ancestor declaration has `declare`; at the Program, the file must be a
  `.d.ts`), and `@template` without a name are reported; the fixer (unless `enableFixer`
  is off) clears the type (and for always-redundant tags the tag) when the tag has a
  description, else removes the tag with `removeEmptyBlock` (the comment's lines may be
  dropped rather than blanked; an emptied block is deleted). These fixes mutate the block
  in place, so later reports in the same block see shifted line numbers, as upstream.
  Inline tags not in `inlineTags` are reported without a trailing period: the block
  description's at the first line containing `{@name` (else the last line), then each
  tag's at its first such line (else line 0).
- **ts-no-empty-object-type** (`src/rules/jsdoc_ts_no_empty_object_type.rs`). Only in
  `typescript` mode; tags that may carry a type (typescript mode's `typeAllowed: false`
  list, overridden by `structuredTags`: a required type always may, `type: false` may not),
  except `@import`. One report per member-less object type in a type that parses. We
  approximate "parses" by balanced brackets and closed quotes and find `{` + whitespace +
  `}` outside quotes, instead of building the type tree (so `Foo<{}` with an unbalanced
  `<` is skipped, but some types the type parser rejects for other reasons would still
  be checked).

Read to understand (not ported): eslint-plugin-jsdoc `src/rules/requireParam.js`,
`src/rules/requireJsdoc.js`, `src/rules/checkTagNames.js`, `src/rules/tsNoEmptyObjectType.js`,
`src/exportParser.js`, `src/tagNames.js`, `src/getDefaultTagStructureForMode.js`,
`src/jsdocUtils.js` (getFunctionParameterNames, flattenRoots, exemptSpeciaMethods,
hasAccessorPair, getIndent, enforcedContexts, getContextObject, getPreferredTagName,
getPreferredTagNameSimple, isValidTag, getTagNamesForMode, getInlineTags,
tagMightHaveTypePosition, overrideTagStructure), `src/iterateJsdoc.js` (iterateAllJsdocs
and noTracking, makeMultiline, addLine, emptyTokens, changeTag, removeTag, reportJSDoc,
reportSettings, makeReport, avoidDocs), `src/utils/hasReturnValue.js`;
`@es-joy/jsdoccomment` `src/jsdoccomment.js` (getJSDocComment with checkOverloads,
findJSDocComment, getTSFunctionComment, getReducedASTNode, the overload sibling helpers),
`src/parseComment.js` (default no-type / no-name tags), `src/parseInlineTags.js`.

## eslint-plugin-unicorn rules (round 5, h1)

`no-useless-recursion`, `no-array-from-fill`, `no-duplicate-if-branches`,
`switch-case-break-position`, `consistent-assert`, `no-top-level-assignment-in-function`,
`prefer-single-array-predicate`, `no-global-object-property-assignment`,
`require-module-attributes`, `consistent-empty-array-spread`, `prefer-promise-with-resolvers`,
`prefer-type-literal-last`, `consistent-class-member-order`, `consistent-optional-chaining`
(each at 100% of its oracle). Each file's header states the rule as we learned it; the
points below are the ones that are easy to get wrong. More helpers live in
`rules/unicorn_util.rs` (round 6 folded the per-helper modules into it): `skip_wrappers`
(parentheses and TS wrappers, as ESTree sees through them), `is_same_reference` (same static
string, `this`, `super`, name, equal literals, or member accesses with the same object and the
same static or same computed property), `logical_operands` / `is_outermost_logical` (flatten an
`&&`/`||` chain), `token_runs` (non-space runs of a span outside comments) and `delete_all`.

- **no-useless-recursion**: `return f(...)` inside the named function `f` (a plain
  identifier resolving to the function's own name) is a loop in disguise; generators, calls
  inside a `try`, and calls after a `using` / `await using` (whose cleanup runs after the
  call) are left alone.
- **no-duplicate-if-branches** compares a branch with the previous one in the chain by
  tokens and tree shape (literals by raw text), ignoring comments, layout, empty statements
  and a trailing `;`.
- **switch-case-break-position**: a `break`/`continue`/`return`/`throw` after a case's only
  block belongs inside it; only `break` and `continue` are fixed, moved before the block's
  `}` at the indentation of its last statement, and not when a comment sits in between, the
  block is on one line, or the statement has a trailing comment. The oracle harness
  (`tests/rule_oracle.rs`, `config_for`) sets `reportUnusedDisableDirectives: "warn"` when a
  case's expected messages include ESLint's "Unused eslint-disable directive" report (null
  `ruleId`), because that report's fix shapes ESLint's output there.
- **no-top-level-assignment-in-function** runs once per file over the Program's global and
  module scopes: a write (not a declaration's initialiser) to a top-level binding from inside
  a function, arrow or method is reported; `/* global */` comments declare nothing, and
  class field initialisers and static blocks are not functions.
- **no-global-object-property-assignment**: writes include compound and logical assignment,
  update, destructuring targets and `for-in`/`of` heads; `delete` is not a write.
- **prefer-promise-with-resolvers**: `new Promise<T>(...)` in a `.js` file is a comparison
  (`new Promise < T > (...)`), so it has no argument and is not reported. The fix needs the
  promise in a plain (not exported) single-declarator `const` in a statement list, preceded
  only by the `let`s it replaces; a named function expression executor whose name is the
  target resolves the assignment to that name, so the outer `let` does not match and there
  is no fix.
- **prefer-type-literal-last**: `null`/`undefined` stay last and unions/intersections are
  skipped when looking for "another type", but in the fix a union or intersection member is
  an ordinary other type. A comment between the token before the union and the token after
  it, outside every member, drops the fix.
- **consistent-class-member-order**: only the first out-of-order member is reported. The
  suggestion needs comment-free bodies with every member starting its line, and refuses to
  move a static field past a static block (or back), since both run in order.
- **consistent-optional-chaining** reports the right side only; with `&&` any `?.` on the
  right is unnecessary, with `||` the right must match the left.

Read to understand (not ported): eslint-plugin-unicorn `rules/no-useless-recursion.js`,
`rules/no-array-from-fill.js`, `rules/no-duplicate-if-branches.js`,
`rules/switch-case-break-position.js`, `rules/consistent-assert.js`,
`rules/no-top-level-assignment-in-function.js`, `rules/prefer-single-array-predicate.js`,
`rules/no-global-object-property-assignment.js`, `rules/require-module-attributes.js`,
`rules/consistent-empty-array-spread.js`, `rules/prefer-promise-with-resolvers.js`,
`rules/prefer-type-literal-last.js`, `rules/consistent-class-member-order.js`,
`rules/consistent-optional-chaining.js`, and the utils `get-enclosing-function.js`,
`is-same-statement.js`, `is-same-tokens.js`, `get-indent-string.js`, `comments.js`
(getLastTrailingCommentOnSameLine, getCommentSafeProblem), `is-same-reference.js`,
`is-logical-expression.js`, `unwrap-typescript-expression.js`, `is-global-identifier.js`,
`type-arguments.js`, `member-expression.js` (getMemberAccessOperatorRange),
`fix/remove-object-property.js`, `ast/literal.js`, `ast/function-types.js`,
`ast/call-or-new-expression.js`, `ast/matches-name-constraint.js`.
- **Lengths that are not collection sizes** (`explicit-length-check`, from
  `unicorn: rules/explicit-length-check.js`). `x.length` is skipped when its static value
  isn't a safe non-negative integer, or `x` is a single-declaration variable initialised
  with an object literal whose `length` isn't one. References to `x` before the read in the
  same function, and every reference in another function or in conditional code (an `if`
  branch the test doesn't decide, loop bodies, the right of `&&`/`||`/`??`, defaults,
  `catch`, optional calls), are classified: a `for (x.length in ...)` head or an unknown
  write makes it unknown (skip); a write of a known integer (`x.length = 3`,
  `Object.defineProperty(x, "length", {value: 3})`, `Object.assign(x, {length: 3})`)
  makes it a size (report); an accessor descriptor makes it unknown. A destructuring
  default (`({length: x.length = 1} = y)`) is not a known value.
- **DOM collections** (`prefer-at`, from `unicorn: rules/prefer-at.js`). `childNodes`,
  `children` and the `getElementsBy*` / `querySelectorAll` calls (optional chains
  included), and values typed `NodeList`/`NodeListOf`/`HTMLCollection`/`HTMLCollectionOf`
  (nullish union members ignored), are not given `.at()`. Reports sit on the index without
  its parentheses, as ESTree ranges exclude them.
- **Regex patterns as strings** (`prefer-string-replace-all`, from
  `unicorn: rules/prefer-string-replace-all.js`) use the crate's own `regex.rs` tree. A
  fixed-string pattern keeps each character as written (`\n`, `\x41`, `\u{..}` stay; an
  identity escape loses its backslash; `\cX` becomes `\r`/`\n`/`\t`/`\u{X}`; `\0`-`\7`
  octal and null become `\u{X}`; `\8` is `8`; a lone Annex-B backslash, as in `\c_`,
  becomes `\\`).
- **Static regexes** (`unicorn_static::static_regexp`, `getStaticRegExp`): a regex value
  without side effects, or a regex literal / `new RegExp(...)` of static primitive or regex
  arguments, directly or through a never-reassigned variable, then evaluated in full.
  `prefer-regexp-test` (from `unicorn: rules/prefer-regexp-test.js`) has no type checker,
  so its "type information" step always answers unknown; the rule leans on annotations,
  `const` initializers and static values. A parenthesized optional chain counts as closed
  (ESTree makes it a whole `ChainExpression`).
- **Arrays for spreading** (`prefer-spread`, from `unicorn: rules/prefer-spread.js` and
  its `shared/` helpers). A receiver is an array by annotation (`T[]`, tuples,
  `Array`/`ReadonlyArray`, `readonly`, aliases and constraints followed), an array literal
  or an `Array(...)` call; not one by literals, templates, binary expressions,
  class-like names, `.join()`, `String()`-style calls, `Buffer.concat`, `new X()`, the
  non-array branch of an `Array.isArray` test, or class/interface/non-array annotations.
  Without a type checker, every spread fix in a TypeScript file is a suggestion.
  `mutable_member_access` walks into functions and classes (ESLint's evaluated children
  only skip names), so `const f = () => f.concat(2)` has no fix.
- **Known harness gap.** `unicorn/prefer-spread` is 625/626: the one failure has an
  unused `eslint-disable-next-line` comment, and the expected output is ESLint's fix for
  the unused directive. The oracle harness turns off `reportUnusedDisableDirectives`, so
  that fix can't happen there; the rule's own report matches.

## jsdoc type rules (round 5: valid-types, check-types, no-undefined-types, check-values)

- **The type parser (`jsdoc_type.rs`).** Our own Pratt parser for JSDoc types in the
  `jsdoc`, `closure` and `typescript` modes (`permissive` tries typescript, then closure,
  then jsdoc). Each mode is a list of prefix and infix rules with binding powers; the
  closure and jsdoc modes use the loose lexer (names may contain `-`, `/`, `:` and
  similar), typescript the strict one. Results are a flat `TypeTree` (`N` nodes indexed by
  `Id`) whose `type_name()`, child order (`children`, `walk`) and `stringify()` follow the
  reference parser's output. `parse(text, mode, class_context)` rejects reserved words as
  root types (`this` only outside class context), as the reference's default strict mode
  does; the rules call it with `class_context` false except valid-types.
  `parse_name_path(.., include_special)` and `parse_name` are the namepath and name
  grammars. Recursion stops at depth 64 (`MAX_DEPTH`) with a failed parse; a regression
  test checks deep nesting. Checked differentially against jsdoc-type-pratt-parser 8.0.0
  on 635 type strings (every type in the jsdoc oracle data plus hand-written ones): tree,
  stringify, namepath and name results agree in all 5715 comparisons (mode x entry point).
  `JsdocTree`'s selector view still uses its own smaller parser (`parse_type`); switching it
  over is left for a later round.
- **Tag structure (`jsdoc_tags.rs`).** What each tag may or must carry per mode
  (`namepathRole`, `typeAllowed`, `nameRequired`, `typeRequired`, `typeOrNameRequired`),
  transcribed from the plugin's default structure for each mode, with
  `settings.jsdoc.structuredTags` replacing a tag's entry. `Modes` answers "might/must" in
  the current mode and says when only another mode would say yes, which valid-types turns
  into `in "<mode>" mode` message suffixes.
- **comment-parser problems.** `jsdoc::Tag::problems` holds the critical problems the
  tokenizers stop at (`unpaired curlies`, `unpaired brackets`, `empty name`,
  `empty default value`, `invalid default value syntax`); valid-types reports them as
  `Invalid name: a; b` and checks nothing else on that tag.
- **valid-types.** Per tag, in this order: problems; `@borrows` needs `x as y`; closure
  `@suppress` names come from a fixed list; a name where the structure allows none; a type
  where none is allowed; a missing required name (not for param/property tags, nor `@see`
  with a `{@link`); a missing required type; a missing type-or-namepath; the type parses
  (with class context) and, in closure/typescript mode, no object field's value is a
  postfix-nullable type; namepaths parse (`@memberof x#`, `@module`/`@requires` without
  `module:`, numeric jsdoc-mode properties and `@borrows .x` get one retry); in typescript
  mode a name-defining tag's name parses as a name; inline `{@link}`-style tags have content.
- **check-types.** Every name node of every typed tag (the type must parse) is looked up in
  `preferredTypes`, on top of the mode's `Object` defaults (typescript: `object<>` and
  `Object.<>` and their spellings report a "use object shorthand" message; other modes
  prefer `Object<>`) unless the user maps any `Object` spelling. A name inside an angle
  generic also matches the key with `.`, `.<>` or `<>` appended, and inside any generic the
  bare `[]`/`<>`/`.<>` keys, but only with `unifyParentAndChildTypeChecks` (option or per
  preference) or when it is the generic's own name; a preference keyed by the plain name
  matches only at the root, unless unified. A preference that is a value of another
  preference is not matched. Then `structuredTags` type lists, then (unless `noDefaults`)
  native casing (`String` -> `string`, `array` -> `Array`; `object` only when the name has
  a truthy preference and it isn't an element of `Object<..>`). The fix rewrites the
  matched node (and its parent generic's brackets/dot for postfix preferences) and replaces
  the first `{type}` in the comment with the reprinted tree. Non-string, non-object, truthy
  preferences report the settings error at 1:2 and are skipped.
- **no-undefined-types.** A name is defined when it is in: the global scope (configured
  and built-in globals, `/* global */` comments, script-mode top-level declarations, lib
  globals with the TS parser); the module scope (classes add `Class.member` for keyed
  members and are "closed"; interfaces add `I.member`; namespaces add their type aliases,
  classes, interfaces and nested namespaces and are closed; imports and `require()` are
  noted as imports); names declared in any TS module scope; extra built-ins; namepath-
  defining tags across the file; typescript-mode `@import` bindings; `definedTypes`;
  preferredTypes replacements; for a method, its class's members (bare and `Class.m`);
  top-level `Closed.x = ...` assignments; the block node's scope, the scopes above it and
  the scopes one, three, five ... levels below it (that is what the plugin collects); and,
  outside jsdoc mode, typescript utility globals and the `@template` names of the node and
  its ancestors (of every block, for a block no node claims). A name followed by
  `.prop` path parts is checked as the joined path unless the base is an import, global,
  typedef or built-in; a path whose root is defined and not closed is fine. Type
  parameters of function types and `infer X` don't count. `checkUsedTypedefs` reports,
  after the file, every block whose first name-defining tag's name the *last* checked
  block didn't use (the plugin keeps only the last block's list).
- **check-values.** `@version`/`@since` are strict semver (`v` prefix allowed, no leading
  zeros, numbers up to 2^53-1, at most 256 characters). `@variation` (with
  `numericOnlyVariation`) follows JavaScript `Number()` (hex/octal/binary, exponents,
  `Infinity`). `@license` runs `licensePattern` (`/src/flags` or a bare source with `v`,
  always global) over the description; each match's group 1 (or the whole match) is the
  license, and an empty match after a non-empty one ends the tag. Licenses are SPDX
  expressions: tokens are case-insensitive `WITH`/`AND`/`OR` prefixes, `( ) : +`,
  `DocumentRef-`/`LicenseRef-` ids and known license/exception ids (the id scanner searches
  ahead rather than anchoring, as the reference does); `OR` binds looser than `AND`;
  `+` may not follow a space. The id lists are data in `rules/jsdoc_check_values_spdx.txt`
  (spdx-license-ids 3.0.24 with its deprecated list, spdx-exceptions 2.5.0). A typescript-
  mode `@import` must read as an import statement ending in a quoted module name.
- **Regex with captures (`regex_exec.rs`).** `match_all` runs a `crate::regex` tree with
  capture groups, backreferences, lookahead, lookbehind (tried from every earlier start),
  and the `i`/`m`/`s`/`g`/`y` flags, like `String.prototype.matchAll`. Quantified single
  characters loop instead of recursing; other nesting stops at depth 400 and each attempt at
  200,000 steps, giving "no match".

Read to understand (not ported): eslint-plugin-jsdoc 63.3.3 `src/rules/validTypes.js`,
`checkTypes.js`, `noUndefinedTypes.js`, `checkValues.js`,
`src/buildRejectOrPreferRuleDefinition.js`, `src/getDefaultTagStructureForMode.js`,
`src/jsdocUtils.js`, `src/iterateJsdoc.js`; jsdoc-type-pratt-parser 8.0.0 `src/` (lexer,
grammars, parslets, stringify, visitor keys); @es-joy/jsdoccomment 0.91.0
`src/parseInlineTags.js`; comment-parser `src/parser/tokenizers/`; spdx-expression-parse
5.0.0 `scan.js`, `parse.js`; semver 7 `internal/re.js`. Behaviour was confirmed against
their output, not translated.

## eslint-plugin-unicorn rules (round 5, h2)

`no-asterisk-prefix-in-documentation-comments`, `no-canvas-to-image`, `no-transition-all`,
`no-ineffective-csp-directives`, `prefer-array-last-methods`, `prefer-then-catch`,
`prefer-object-destructuring-defaults`, `no-undeclared-class-members`,
`prefer-flat-math-min-max`, `no-this-outside-of-class`, `no-confusing-array-with`,
`no-late-current-target-access` (each at 100% of its oracle). Helpers these rules share are in `rules/unicorn_util.rs`: `skip_awaits`, `direct_or_single_use_const` (an expression, or
the initialiser of the plain `const` it names when that is declared once before the use and
read only there; the declaration's initialising write is one of its two references) and
`decode_jsx_entities` (JSX attribute strings with `&#45;`, `&#x2D;` and the HTML 4 names
plus `&apos;` decoded, as ESTree parsers give their value; our AST keeps the raw text) and
`remove_method_call` (the edits turning `obj.name(...)` into `obj`: from the end of the
parenthesized object to the end of the member, and from the end of the parenthesized callee
to the end of the call), `has_side_effect_strict` (eslint-utils' `hasSideEffect` with
`considerGetters` and `considerImplicitTypeConversion`: every member access, converting
unary/binary operators unless all operands are literals, non-literal computed keys; tagged
templates are not side effects there, and function bodies are not entered), `plain_key_name`,
`line_start`/`line_indent`, and `indent_unit` (the plugin's `getIndentUnit` over
`detect-indent`: the most used indentation change between consecutive non-empty lines, single
spaces ignored unless nothing else is found, continuation lines of comments, strings,
template chunks and JSX text blanked; a tab when the winner is not spaces or there is none).

- **no-asterisk-prefix-in-documentation-comments** looks at `/**` (not `/***`) block comments
  that span lines and start a line (only spaces and tabs before them). Every line after the
  first that is `<indent><spaces>*` loses the asterisk and one following space (` */` becomes
  `*/`); the indentation all non-blank lines then share, when it is deeper than the comment's,
  is moved back to the comment's, which also empties whitespace-only lines. When only some
  content lines have the asterisk nothing is reported: they may be Markdown list markers.
  Lines split at `\n`, `\r`, U+2028 and U+2029 (`\r\n` counts as a break and an empty line).
- **no-canvas-to-image** reports the first argument of a plain `x.drawImage(...)` that is
  (through `await`s and a single-use `const`) `loadImage(<canvas>.toDataURL())`, where the
  receiver is an identifier or a `.name` whose name contains `canvas` (any case), and of
  `x.putImageData(...)` that is `y.getImageData(...)`; optional chaining anywhere opts out.
- **no-transition-all** reports only with type information: the receiver of `.transition =`,
  `.transitionProperty =` or `.setProperty('transition' | 'transition-property', value,
  priority?)` must be the lib's `CSSStyleDeclaration`/`CSSStyleProperties` (every non-nullish
  member of a union), the value a static string whose top-level CSS components include the
  identifier `all` (escapes decoded, any case; not inside a function such as `var()`; `!`
  means it is not a plain value), and the priority missing, `''`, `important` or not a
  literal. Its oracle cases carry no type information, so they all expect nothing; the CSS
  file half of the plugin rule needs a CSS language.
- **no-ineffective-csp-directives** checks JSX only (the HTML half needs an HTML language):
  `<meta>` (lowercase identifier) without spreads, exactly one `httpEquiv`/`http-equiv` and
  exactly one `content`, each a string or a container whose control-flow static value is a
  string; header `content-security-policy` in any case; each of `frame-ancestors`, `sandbox`,
  `report-uri` reported once per element on the `content` value, in policy order.
- **prefer-array-last-methods** reports the property of a plain `.find`/`.findIndex`/
  `.indexOf`/`.reduce` call on a plain argument-less `.reverse()`/`.toReversed()` call; the
  suggestion renames it (`findLast`, `findLastIndex`, `lastIndexOf`, `reduceRight`) and
  removes the reversing call, unless a comment lies in that call outside its receiver.
- **prefer-then-catch** reports `.then` of a plain two-argument `then` call without spreads
  where neither handler is nullish (`null`, `void x`, the unshadowed `undefined`, through TS
  wrappers). With type information the non-nullable receiver must be `any`/`unknown`/an error
  type, or the lib's `Promise` (some declaration in a lib file, so a user augmentation still
  counts) whose `then` result has a callable `catch` on every union member. The suggestion
  needs an identifier or function handler: it removes the comma before it (and a trailing
  comma) and appends `.catch(<handler with its parentheses>)` after the call; a comment
  outside the handler in the removed range, or right after the handler or its comma, drops it.
- **prefer-object-destructuring-defaults** reports a declarator `{a, b: c} = {a: 1, b: 2,
  ...rest}` (no type annotation; init an object literal through parentheses): every pattern
  property a non-computed key with a plain identifier binding and no default, no
  `Object.prototype` name; every object property but the last a non-computed `key: value` or
  shorthand with a strictly side-effect-free value, names unique; the last a side-effect-free
  spread; one default per pattern property, none referring to a binding name. The suggestion
  (`{a = 1, b: c = 2} = {...rest}`, defaults with their parentheses) is dropped when the
  declarator holds a comment.
- **no-undeclared-class-members** runs once per file. A `this.x` belongs to the class whose
  member list contains it on the way up, unless a function that is not a class method's value
  (object-literal methods count as functions) comes first; heritage and class decorators pass
  through to the outer class. Classes with `extends` are skipped. Declared: `constructor`,
  non-static methods/accessors/fields by static key name (identifier, string, expression-free
  template, also computed), parameter properties, and `this.x =` in the constructor body
  outside nested functions and classes. Not reported in static members or blocks, keys and
  decorators. The suggestion (first plain `=` target per name, not in the constructor) inserts
  `<indent>x;` at the start of the line of the first member or the comment just before it,
  unless that is the `{` line; the indent is the whitespace before the first member, or the
  class line's indent plus `indent_unit`.
- **prefer-flat-math-min-max** reports the outermost plain `Math.min(...)`/`Math.max(...)`
  (`Math` an identifier, parentheses transparent) with an argument that is the same method's
  call, and not itself an argument of one. The fix writes the callee text (without its
  parentheses) and every argument of the flattened nest with its parentheses, and spaces a
  glued keyword (`return(Math.max)(...)`); a comment outside the callee and the kept
  arguments cancels it.
- **no-this-outside-of-class** walks up from `this` past arrows: the first non-arrow function
  decides (a class method's value, or a function with a `this` parameter, is allowed); before
  that, a static block or a class field's value allows it. Object-literal `MethodDef`s are
  functions here, so their `this` is reported. `this` named by a `typeof` type query
  (through qualified names) is skipped.
- **no-confusing-array-with** reports `.with` of a plain `x.with(i, ...)` with a non-spread
  first argument that is a number literal under any `+`/`-` signs and TS wrappers whose
  truncation is negative (`negative-index`), or `x.length` of the same reference
  (`length-index`), unless `should_skip_known_non_array_receiver` says the receiver isn't an
  array.
- **no-late-current-target-access** checks `<name>.currentTarget` (object an identifier
  through TS wrappers; `e`, `event`, `evt` or `[a-z][A-Za-z0-9]*Event`) whose binding has a
  parameter declaration, unless the type guesser says it is not an event (an `EVENT`
  `Checker` with the DOM event interfaces and React's event names, definitions preferred).
  Inside a function nested in the declaring one it is `in-nested-function`; in the handler
  itself it is `after-suspension` when an `await`, `yield` or `for await` of the handler
  (not of nested functions) ends before the access starts (ESLint marks them on exit), or
  when the access is in a loop part that repeats (`for` test/update/body, `for-in`/`for-of`
  except the iterated expression, `while`, `do`) and that loop contains a suspension point.

Read to understand (not ported), eslint-plugin-unicorn 79.0.0 `rules/`:
`no-asterisk-prefix-in-documentation-comments.js`, `no-canvas-to-image.js`,
`no-transition-all.js`, `no-ineffective-csp-directives.js`, `prefer-array-last-methods.js`,
`prefer-then-catch.js`, `fix/remove-method-call.js`, `fix/replace-member-expression-property.js`,
`fix/remove-argument.js`, `prefer-object-destructuring-defaults.js`,
`no-undeclared-class-members.js`, `shared/class-this.js`, `prefer-flat-math-min-max.js`,
`no-this-outside-of-class.js`, `ast/is-in-type-query.js`, `no-confusing-array-with.js`,
`utils/numeric.js`, `utils/comparison.js`, `no-late-current-target-access.js`,
`shared/late-event-handler.js`, `utils/is-event.js`, `utils/contains-suspension-point.js`,
`utils/get-enclosing-function.js`, `utils/get-line-start.js`,
`utils/reindent-text.js`, `utils/types.js`, `utils/comments.js`, `utils/get-references.js`,
`utils/get-indent-string.js`, `utils/get-indent-unit.js`, `utils/get-line-indent.js`;
@eslint-community/eslint-utils 4.10.1 `hasSideEffect`; detect-indent 7.0.2 `index.js`. The JSX entity names are the HTML 4 list plus `apos` (what espree's
JSX parser decodes); only numeric references appear in the oracle.

## eslint-plugin-unicorn rules (round 5, h5)

Rules from eslint-plugin-unicorn 79.0.0's recommended set, each at 100% of its oracle, all
`NO_PRESET`. Shared helpers are in `rules/unicorn_util.rs` and `rules/unicorn_text.rs`: `would_remove_comments` (a
comment wholly inside the replaced range and outside every kept range), `expression_text` /
`call_argument_text` (a node's text without redundant parentheses unless those hold a
comment; a sequence is wrapped when it becomes one argument), `call_parens` /
`call_arguments_text` (the argument list's `(`, `)` and inner text), `operator_after`,
`has_side_effect` (eslint-utils' default: calls, `new`, writes, `await`, `yield`, `delete`;
member reads don't count). What we learned, in our words:

- **require-number-to-fixed-digits-argument**: a `.toFixed()` call without arguments (not
  an optional call, not on a `new` expression) is reported from `(` to `)`; the fix puts `0`
  before the `)`, after any comment.
- **prefer-math-trunc**: `x | 0`, `>> 0`, `<< 0`, `^ 0` (any spelling of a zero literal,
  quoted raw in the message), their compound assignments, the innermost `~~`, and
  `[Number.]parseInt(String(x), 10)` with the globals. All suggestions; withheld when a
  comment outside the copied operand would be lost (compound assignments copy the left side
  into `Math.trunc(...)` and replace the operator by `=`, so they also need it free of side
  effects).
- **prefer-code-point**: `charCodeAt` calls are suggested `codePointAt` unless the result is
  used as a number (climbing parentheses, logical operators, TS wrappers and `?:` branches,
  the parent is arithmetic/bitwise, unary `+ - ~`, or an arithmetic compound assignment's
  right side); `String.fromCharCode` always gets `fromCodePoint`.
- **prefer-reflect-apply**: `fn.apply(null | this, [..] | arguments)` and
  `Function.prototype.apply.call(fn, ...)` (non-optional calls of non-optional members;
  static string keys count) become `Reflect.apply(fn, recv, list)`. Literal, array/object
  literal and `super` receivers are skipped, as is a receiver chain with an optional link
  outside parentheses (`a?.b.apply`). The fix is dropped when a comment lies outside the
  target, receiver and list.
- **prefer-prototype-methods**: the first argument of `Reflect.apply` or the object of
  `.apply/.bind/.call`: `[].x`/`{}.x` (key evaluated with `const`s and well-known symbols,
  `Symbol(Symbol.iterator)`), or a global read of an `Object.prototype` method (configured
  global, or a member of the global object via `unicorn_util::is_global_object`, with a
  key static without variables, since the reference tracker evaluates in the global
  scope). No fix with a comment inside the replaced node.
- **prefer-native-coercion-functions**: `v => String(v)` (also `Number`, `BigInt`,
  `Boolean`, `Symbol`, written as the callee, TS wrappers dropped) and identity callbacks
  of `.some()`-like methods (`Boolean`). Names and heads follow eslint-utils, not ESLint
  (`utils_function_name_with_kind`: class fields holding functions are methods, an object
  property's function is a method, `export default` is `'default'`;
  `utils_function_head_span`: a computed key's function starts at its property). Fixes:
  `foo: Name` for object methods, ` = Name;` for class methods (suggestion), a `;` when a
  token on a later line, outside the parent, could continue the identifier.
- **prefer-dom-node-append**: `x.appendChild(y)` with one non-spread argument, where
  neither side is a value that can't be a node (`is_value_not_dom_node`: literals,
  functions, classes, arrays, objects, templates, `undefined`), is reported as a whole; the
  rename to `append` is a fix only when the value is unused (`is_value_not_usable`: an
  expression statement, or a non-last sequence element), since `append` returns `undefined`.
- **prefer-negative-index**: `slice`/`subarray` (arguments 0, 1), `splice`/`toSpliced`/`at`
  (argument 0), called directly or as `Owner.m.call(x, ..)` / `Owner.m.apply(x, [..])` with
  owner `[]`, `''` (slice only) or `Name.prototype` of a global that has the method. An
  identifier key counts even when computed (`x[slice]`). An argument `x.length - n` (`n` a
  positive safe integer literal; nested subtractions recurse into the left side) on the same
  reference as the receiver (`is_same_reference`: identifiers, `this`, literals by value,
  static keys by value, through TS wrappers) is reported on the call. The fix deletes the
  parenthesized `x.length` plus following whitespace, and the whitespace between `-` and the
  number; no fix with a comment inside the access. Only `length - 1` everywhere is a fix
  (the same for any length); larger offsets are suggestions.
- **prefer-add-event-listener**: any assignment to a non-computed `x.on<event>` (known DOM
  event list) is reported at the property; `null`/`undefined`/`void` (through TS wrappers)
  ask for `removeEventListener`. `beforeunload` without a block-bodied function free of
  value `return`s, `message` and `error` get extra notes and no fix. The fix (plain `=` as
  an expression statement, a value that can be a function, no comment outside receiver and
  handler, and no other assignment in the file to the same reference) writes
  `obj.addEventListener('event', handler)`, parenthesizing a receiver that would start the
  statement with `{`/`class`/`function`/`let [`. A `require`/`import` of an excluded package
  (`koa`, `sax` by default) turns it off for assignments that end after it, so the rule runs
  as a `run_file` scan in source order.
- **prefer-set-size**: `[...x].length` (one spread) and `Array.from(x).length` (one
  argument, nothing optional) with `x` a `Set` per `unicorn_types::is_set`; not when the
  `.length` is written (`is_left_hand_side`: assignment/update targets, destructuring,
  `for` heads, `delete`). The fix renames to `size` and writes `x` as a member object
  (`member_object_text`; extra parentheses inside an unparenthesized `new` callee when the
  chain ends in a call; a `;` from `needs_semicolon`), unless the array has more comments.
- **prefer-array-index-of**: `findIndex`/`findLastIndex` with one callback that is a
  plain (non-async, non-generator) arrow/function of one identifier parameter returning
  `p === v` or `v === p` (concise body or a lone `return`). Skipped for receivers
  `should_skip_known_non_array_receiver` rejects, when the parameter has any other
  reference, or a function expression uses `this`/`arguments` outside nested functions or
  its own name. Fix: rename and replace the callback with `v` (wrapped when `v` was
  parenthesized and the callback wasn't); a suggestion when `v` has side effects; nothing
  with a comment in the callback. `simple_array_search` is shared (prefer-includes's
  `some` form uses the same shape).
- **prefer-array-some** (373/377): `find`/`findLast` (1–2 arguments, nothing optional, no
  type arguments) compared with `undefined`/`null`, used for truthiness
  (`is_boolean_context`: `!`, `Boolean()`, control-flow tests, through `&&`/`||` only, not
  TS wrappers), or stored in a lone unannotated, unexported `const` whose reads are all
  boolean uses (only `Boolean(x)` when the receiver isn't known to be indexed): always a
  suggestion. `findIndex`/`findLastIndex` against `-1`/`0`: fixed by deleting from just
  after the token or comment before the operator and adding `!` for `===`/`==`/`<`
  (parenthesized as a member object, callee, tag, superclass or `**` base; no fix after a
  line comment or with a comment inside); `findLastIndex` only fixes side-effect-free
  inline callbacks. `filter(f).length > 0`/`!== 0` (raw `0`; not `$x`, not a non-function
  argument): fixed for side-effect-free callbacks, else suggested. The 4 failures are
  `x < 0 as any ** 2`: ESLint's TypeScript 5 parser accepts it, tsc 7 (and our parser)
  report TS1005.
- **prefer-array-flat-map**: `x.map(f).flat()` / `.flat(1)` (raw `1`), not on
  `React.Children`/`Children` or known non-arrays, reported from `map` to the call's end;
  fixed (remove `.flat()`, rename) when `f` is an arrow returning an array literal or a
  side-effect-free inline function, suggested otherwise, nothing with comments outside the
  `map` call or type arguments. `x.filter(p => t).flatMap(p => [a, b])` (plain untyped
  expression arrows on the same name, 2+ elements, no optional link in `x`) suggests
  `x.flatMap(p => t ? [a, b] : [])`, wrapping a test that is an arrow, assignment, class,
  `?:`, function, object, sequence, TS wrapper or `yield`.
- **prefer-array-flat**: `a.flatMap(x => x)` (not on a PascalCase identifier unless it is
  a `const` array literal / `new Array`, nor a `const` holding an object, literal, template,
  function, class or other `new`), `a.reduce((x, y) => x.concat(y), [])`, `[].concat(...a)`,
  `[].concat.apply([], a)` / `Array.prototype.concat.call([], ...a)`, and one-argument
  calls of `_.flatten`/`lodash.flatten`/`underscore.flatten` plus the `functions` option
  (trimmed dotted paths). Each matching form reports once. Skips arrays
  `is_known_non_array` accepts and `arguments`; the fix writes `<a>[?].flat()` when the
  call has no comment outside the array.
- **require-array-join-separator**: `x.join()` (receiver not a known non-array) and
  `[].join.call(x)` / `Array.prototype.join.call(x)`, reported from the empty call's `(` (or
  the end of the last argument token) to `)`; the fix inserts `','` before `)`.
- **no-unreadable-array-destructuring**: array patterns (binding `ArrayPattern`s and array
  literals that are assignment targets) with more consecutive holes than
  `maximumIgnoredElements` (default 1); the report covers a type annotation on the
  pattern. A declarator with an initializer and one non-default element (no annotation, an
  indexable initializer, no comment outside the element) is fixed to `x = init[i]` /
  `init.slice(i)`. Members whose nearest pattern (through TS wrappers, defaults, rest; an
  object pattern stops) is an array pattern are reported as property assignments.
- **prefer-dom-node-remove**: `parent.removeChild(child)` (one argument, not an optional
  call, neither side a non-node value) is reported. With a side-effect-free parent and an
  unused value it is fixed: `x.parentNode.removeChild(x)` replaces everything after `x`
  with `[?].remove()` (not when the callee or `parentNode` is parenthesized), other
  non-optional parents become `child.remove()`. Otherwise suggestions: `?.remove()` and
  `.remove()` when the callee/parent member chain has an optional link, else `.remove()`.
  Any comment in the call blocks fixes and suggestions.
- **prefer-modern-dom-apis**: `p.replaceChild(a, b)` / `p.insertBefore(a, b)` on plain
  identifiers (not `undefined`) become `b.replaceWith(a)` (fix) / `b.before(a)`
  (suggestion; `insertBefore(x, null)` appends) when the value is unused; `r.insertAdjacent
  Text|Element(pos, x)` with a known static position and a literal/identifier/static-string
  `x` becomes `before`/`prepend`/`append`/`after` (not when an `insertAdjacentElement`
  result is used); `while (p.firstChild) p.firstChild.remove();` becomes
  `p.replaceChildren();` unless `p` has an optional link anywhere, can't be a node, or the
  syntax checker (a `unicorn_types::Checker` with class syntax, `ParentNode`-like targets,
  `Node`/`Text`/`Window`... non-targets, `window`/`self`/`globalThis` globals) rules it out.
  `expression_start_needs_parens` now treats a statement replacement (the `while`) as a
  statement start.
- **prefer-query-selector**: `getElementById` / `getElementsBy{ClassName,TagName,Name}`
  with one argument on a possible DOM node; `[0]` / `.at(0)` / `.item(0)` after an `All`
  call turns it into `querySelector` and goes. Fixed only when the argument is `null` or a
  non-blank static string whose value the selector reads the same (simple CSS identifiers;
  `ByName` without quotes, backslashes or line breaks); the selector is `#id`, `.a.b`,
  the tag or `[name='v']` (opposite quote), behind `:scope ` off `document`.
- **prefer-dom-node-text-content**: `x.innerText` and a non-computed `innerText` key in a
  binding pattern or an assigned object literal, unless a `unicorn_types::Checker` with
  unicorn's DOM node names says the object (the typed parameter, the declarator's
  annotation or initializer, the assignment's right side) is not a node. The suggestion
  keeps a shorthand binding (`{textContent: innerText}`).
- **prefer-keyboard-event-key**: `e.keyCode` / `charCode` / `which` on a plain parameter
  of an enclosing `addEventListener(type, cb)` callback, JSX `onKey*` handler or
  `KeyboardEvent`-annotated parameter (resolved through the scope's symbols; parameters the
  checker knows are other events skipped), and those keys destructured from it. Only
  `if (e.keyCode === N)` is fixed, from the virtual-key table and the code point; layout
  dependent letters and non-listener contexts get a suggestion. Outside ASCII, "printable"
  (letter, number, punctuation, symbol) is approximated with Rust's alphabetic/numeric
  classes minus whitespace, control, format, private-use and combining-mark ranges.
- **no-array-method-this-argument**: the `thisArg` of `every`...`some` (two arguments; not
  library callees like `_.map` / `React.Children.forEach`, not a non-function callback, not
  a known non-array receiver via `should_skip_known_non_array_receiver`) and of
  `Array.from` / `Array.fromAsync` (three). Removal (from the comma before it) is a fix for
  an arrow callback with an identifier/literal/`this`/static-string `thisArg`, otherwise a
  suggestion; non-arrow callbacks also get `cb.bind(t)`.
- **prefer-string-raw**: one-line strings / untagged templates whose only escapes are `\\`
  (and the string's own quote) become ``String.raw`...` ``, except where a literal is
  required (keys, module sources/specifiers, import attributes, JSX attribute values, enum
  members, module names, literal and import types, abstract/signature keys), Jest inline
  snapshots and `as const`. With the TypeScript grammar or a `.ts`-family name (no type
  information) only `let`/`var` without annotation and `: string` declarators are fixed;
  the rest get a suggestion. ``String.raw`...` `` without a backslash loses its tag (`'...'`
  unless it has expressions or line breaks), with ASI `;`, `return`/`throw`/`yield`
  parentheses when the template starts on a later line, and the tag's parentheses removed.
- **prefer-default-parameters**: `x = x || v` / `??` / `||=` / `??=` statements in a function
  body for its last plain parameter, and (`checkFallbackExpressions`) bindings whose every
  read is `x ?? v` / `x || v` with one operator and one default, through the scope's symbols
  and references. eslint-scope's "same definition node" is the function for parameters
  (our `Decl.node` is the `Param`) and the declarator for variables. Defaults are static
  literals (`-n` included, compared like `Object.is`, bigints by value) or an earlier,
  never-reassigned binding of the same declaration; without type information every default
  counts as compatible. Spans follow typescript-eslint: a parameter binding covers its `?`
  and type annotation.
- **prefer-object-from-entries**: `pairs.reduce(o => ({...o, k}) | Object.assign(o, {k}),
  {})` (the accumulator referenced once, one plain non-`__proto__` property, receiver not a
  known non-array) is fixed to `Object.fromEntries(pairs.map(...))`; `const o = {};` with
  the next statement `for (const [k, v] of pairs) o[k] = v;` becomes
  `Object.fromEntries(pairs)` with the loop removed (`remove_statement`, unicorn's
  `removeStatement`: a `;` when ASI needs one, else the statement's own lines); one-argument
  `_.fromPairs` / `lodash.fromPairs` / configured `functions` calls are renamed.
- `unicorn/dom-node-dataset` (338/338): default mode reports `set/remove/hasAttribute`
  (no `?.` on the member) and `getAttribute` (not awaited) with a static `data-*` name on a
  receiver not known to be a non-DOM value or the global `document`, skipping names that
  map to inherited `Object.prototype` members; the `.dataset` rewrite is a fix, a suggestion
  for `getAttribute`. `preferAttributes` reverses it. A `x?.dataset` that ends the chain is
  an ESTree `ChainExpression`, not a member, so `'k' in el?.dataset`, `Object.hasOwn(el?.dataset, …)`
  and `const … = el?.dataset` are not reported, while `el?.dataset.k` is. A default
  `[el.dataset.k = 1] = a` is a pattern, not a write. Computed keys are evaluated without a
  scope (`dataset[k]` is never known, `dataset['a' + 'b']` is). The `const data = el.dataset`
  suggestion inlines every `data.k` read and needs the element name to resolve to the same
  never-reassigned binding at each use. Added `escape_string` (quote-js-string's escapes),
  `is_identifier_name` and `indent_before` to `unicorn_util.rs` (`linebreak` is `unicorn_ast`'s).
- `has_side_effect` is `unicorn_static::has_side_effect` (eslint-utils does
  not count tagged templates; `has_possible_side_effect` adds tagged templates and
  spreads anywhere, like unicorn).
- Shared parenthesization helpers in `unicorn_text.rs`: `needs_parens_at_expression_start`
  (a `{`/`class`/`function`/`async function`/`let [` start moved to a statement, arrow
  body, `export default` or `for` init), `member_object_needs_parens` /
  `member_object_text` and `needs_semicolon` (`in_unparenthesized_new_callee` and
  `member_on_needs_new_parens` are in `unicorn_util.rs`; `needs_semicolon` works from the token before the insertion point: the statement it ends,
  `]`, `)` except a control statement's head, a function/class expression's `}`, literals,
  object literals, template ends, `!`, identifiers other than `for-of`'s `of` and
  `await`).
Read to understand (not ported), eslint-plugin-unicorn `rules/`:
`require-number-to-fixed-digits-argument.js`, `prefer-math-trunc.js`,
`prefer-code-point.js`, `prefer-reflect-apply.js`, `prefer-prototype-methods.js`,
`prefer-native-coercion-functions.js`, `prefer-dom-node-append.js`, `prefer-negative-index.js`, `prefer-add-event-listener.js`, `prefer-set-size.js`, `prefer-array-index-of.js`, `prefer-array-some.js`, `prefer-array-flat-map.js`, `prefer-array-flat.js`, `require-array-join-separator.js`, `no-unreadable-array-destructuring.js`, `prefer-dom-node-remove.js`, `prefer-modern-dom-apis.js`, `prefer-query-selector.js`, `prefer-dom-node-text-content.js`, `prefer-keyboard-event-key.js`, `no-array-method-this-argument.js`, `fix/remove-argument.js`, `prefer-string-raw.js`, `prefer-default-parameters.js`, `prefer-object-from-entries.js`, `shared/for-of-collection-loop.js`, `fix/remove-statement.js`, `utils/get-sibling-node.js`, `dom-node-dataset.js`, `utils/escape-string.js` (and `quote-js-string`), `utils/get-indent-string.js`, `utils/member-expression.js`, `utils/needs-semicolon.js`, `utils/is-string-literal-required.js`, `shared/is-jest-inline-snapshot.js`, `utils/get-unparenthesized-keyword-argument-parent.js`, `fix/add-parentheses-to-return-or-throw-expression.js`, `shared/event-keys.js`, `utils/is-dom-node.js`, `fix/append-argument.js`, `fix/remove-method-call.js`, `shared/simple-array-search-rule.js`, `shared/dom-events.js`, `shared/negative-index.js`, `utils/{comments,get-expression-text,get-call-argument-text,
get-call-expression-arguments-text,has-optional-chain-element,function-body,is-value-not-usable,is-node-value-not-dom-node,is-same-reference,comparison,is-node-value-not-function,should-add-parentheses-to-member-expression-object,should-add-parentheses-to-expression-statement-expression,needs-semicolon,is-left-hand-side,is-in-unparenthesized-new-expression-callee,should-add-parentheses-to-new-expression-callee,is-new-expression-with-parentheses,is-function-self-used-inside,boolean,is-logical-expression,is-side-effect-free-function,has-possible-side-effect,should-add-parentheses-to-unary-expression-replacement,is-node-matches,comments,get-const-variable-initializer,array-or-object-prototype-property,get-parent-pattern,single-statement,should-report-replace-children-receiver}.js`, `fix/replace-member-expression-property.js`, `ast/{is-method-call,call-or-new-expression,is-member-expression}.js`;
`@eslint-community/eslint-utils` (`hasSideEffect`, `getFunctionNameWithKind`,
`getFunctionHeadLocation`, `ReferenceTracker`).
- **Known parser gap.** `unicorn/prefer-includes` is 243/247: the 4 failing cases are
  `x < 0 as any ** 2` and `x < 0 satisfies boolean as any ** 2`. tsc and typescript-estree
  keep parsing tighter binary operators after `as T` / `satisfies T`, but ts_r_parser stops
  there and reports "';' expected". The fix belongs in the parser, not in the rule.

## eslint-plugin-unicorn rules (round 5, h4)

Current: catch-error-name 120/120, consistent-function-scoping 170/170, no-array-reduce
482/482, no-await-expression-member 71/72, no-empty-file 58/58, no-for-each 431/431,
no-for-loop 276/276, no-negated-condition 134/134, no-object-as-default-parameter 62/62,
no-process-exit 58/58, no-static-only-class 91/91, no-thenable 178/178,
no-useless-fallback-in-spread 61/61, no-useless-length-check 165/165,
no-useless-promise-resolve-reject 121/121, no-useless-spread 283/283,
no-useless-switch-case 34/34, no-useless-undefined 271/271, prefer-export-from 158/158,
prefer-module 152/152, prefer-switch 68/68, prefer-ternary 142/142, prefer-top-level-await
228/228, switch-case-braces 56/56.

Shared helpers of this round live in `unicorn_util.rs` (method calls, identifier
validity, free names, renames, ESTree identifier spans, same-line trailing comments,
indentation, line breaks, `hasSideEffect` with the getter/conversion options (`has_side_effect_with`), switch-case
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
- **no-for-loop.** The plugin singularizes the array name with the `pluralize` package;
  `singular` in `unicorn_util.rs` is our own small set of English rules (irregulars,
  `-ies`, `-xes`/`-ches`/`-shes`/`-sses`, else drop `s`), enough for the oracle names.
  An optional `arr?.[i]` sits in an ESTree chain expression, so `delete (arr?.[i])` is a
  read (reported, unfixed as a reference context). The type-aware `.entries()` check
  (with parser services) is not implemented, only the annotation and syntax checks.
  `unicorn_ast::remove_statements` drops a statement with its own line, as `removeStatement`.
- **prefer-ternary.** The plugin's "minimal ternary" fix (branches that differ in one
  inner value become `foo(test ? a : b)`) is not implemented: no oracle case expects it, so
  only whole `return`/assignment merges are fixed. `let x = a; if (t) x = b;` is a
  suggestion only, and is skipped when `x` is read in `t` or `b`.
- **prefer-top-level-await.** Without type information `isKnownNonPromise` is always
  false, so every unawaited promise-like top-level call is reported.
- **prefer-export-from.** A new `export ... from` goes at the end of the source text (the
  `Program` end, trailing comments and whitespace included). A type-only import or export
  joins a `type` export of the same source if there is one, else a value one with an
  inline `type`.
- **no-thenable.** Under espree a shorthand `export {then}` is one identifier node playing
  both roles, so it is reported twice; typescript-eslint makes two nodes and reports once.

Read to understand (not ported), all from eslint-plugin-unicorn 79.0.0 `rules/`: the rule
files of the rules above, plus `utils/get-available-variable-name.js`,
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
`utils/get-indent-unit.js`, `utils/get-line-indent.js`, `utils/get-sibling-node.js`,
`utils/should-add-parentheses-to-conditional-expression-child.js`,
`utils/is-same-tokens.js`, `shared/minimal-ternary.js`, `utils/singular.js`,
`fix/remove-statement.js`, `utils/get-scopes.js`, `utils/is-reference-context.js`; and from
`@eslint-community/eslint-utils`, `hasSideEffect`, `getFunctionHeadLocation` and
`getFunctionNameWithKind`.

### Batch 3

`no-invalid-well-known-symbol-methods`, `no-incorrect-template-string-interpolation`,
`no-conflicting-constraints`, `prefer-iterator-to-array`, `no-shorthand-property-overrides`,
`prefer-get-or-insert-computed`, `no-accidental-bitwise-operator`, `prefer-math-constants`,
`prefer-simple-sort-comparator`, `no-useless-collection-argument`,
`prefer-dom-node-html-methods`, `prefer-string-match-all`, `no-useless-concat` (each at 100%
of its oracle) and `require-module-specifiers` (67 of 68: `declare export type {} from "m"`
in a `.js` file is a parse error for ts-r, while typescript-eslint recovers). New helpers in
`rules/unicorn_util.rs`: `static_property_name`, `decode_jsx_entities` (shared with the h2
rules; JSX attribute strings reach rules undecoded), `evaluates_node` (a `containsNode`
that skips function bodies and non-static field values but still walks their computed keys;
depth 64), `is_direct_eval_call` and `is_type_slot` (the child sits where a type goes: an
`as` / `satisfies` / assertion type, a type alias body, a type annotation or return type, or
inside a type node; literal types are plain literal nodes, so rules on literals need this).

- **no-conflicting-constraints** and **no-shorthand-property-overrides** have CSS and HTML
  parts upstream (other ESLint languages); only the JSX part is implemented. The shorthand
  rule keeps its own table of CSS shorthands and the longhands each one resets, with the
  extra resets added after nested shorthands are expanded.
- **prefer-math-constants**: a decimal literal is a constant when the constant's shortest
  text starts with it, or, when the literal is shorter, the constant rounded to the
  literal's fraction digits equals it; `PI` needs 2 fraction digits, `E` 3, the rest 4.
  Keys and literal types (`type T = 3.14`, `x as -3.14`) are skipped.
- **prefer-simple-sort-comparator**: the comparator must be a tree of `<`/`>`/`<=`/`>=`
  tests on one mirrored pair of parameter paths returning signed number literals, with both
  signs present, a non-zero first true branch and every branch agreeing with the
  subtraction; no suggestion when it holds a comment or the receiver is a bigint typed array.
- **no-useless-collection-argument**: with `a ?? x` the fix removes `?? x` and the
  parentheses around the `??` (not when its left side is a bare sequence); a comment in the
  argument's removal range makes it a suggestion.
- **prefer-dom-node-html-methods**: reading `.innerHTML` is fixed to `.getHTML()` unless the
  member is itself called, constructed or a tag; writing is only checked with `checkSetHTML`
  and only suggested for a plain `=` whose value is unused.
- **prefer-string-match-all** only fires when the rewrite is provably the same loop: `let m;`
  right before the `while`, `re` a single `let`/`const` in the same statement list built from
  a static regex with `g` that cannot match the empty string (judged on the parsed pattern
  with `crate::regex`: assertions, lookarounds, back references, modifier groups, `\q{}` and
  parse errors all count as "might be empty"), used only in its initializer and the loop
  test; `s` a `const` with a side-effect-free static string; `m` not used outside the loop,
  not written in the body and not captured by a function or class member there.
- **require-module-specifiers**: a bare `export {}` is kept in `.d.ts` / `.d.mts` / `.d.cts`
  files and whenever it is the file's only module marker. typescript-eslint gives exported
  interfaces, type aliases and `declare`d declarations the `type` export kind, so they don't
  count as runtime exports.
- **no-useless-concat** only folds literals on one line, keeps a split `${...}` placeholder
  and `'use ' + 'strict'` in a directive prologue, and has no fix when folding would drop a
  comment or the chain's parentheses, reorder a right template's side effects, or move a
  legacy octal / `\8` / `\9` escape into a template. Its single-quoted output escapes `\`,
  `'`, line terminators and control characters.

Read to understand (not ported): eslint-plugin-unicorn `rules/no-invalid-well-known-symbol-methods.js`,
`rules/no-incorrect-template-string-interpolation.js`, `rules/no-conflicting-constraints.js`,
`rules/prefer-iterator-to-array.js`, `rules/no-shorthand-property-overrides.js`,
`rules/shared/css-shorthand-properties.js`, `rules/prefer-get-or-insert-computed.js`,
`rules/no-accidental-bitwise-operator.js`, `rules/prefer-math-constants.js`,
`rules/prefer-simple-sort-comparator.js`, `rules/no-useless-collection-argument.js`,
`rules/prefer-dom-node-html-methods.js`, `rules/prefer-string-match-all.js`,
`rules/require-module-specifiers.js`, `rules/no-useless-concat.js`, and the utils
`type-arguments.js`, `ast/literal.js`, `ast/is-direct-eval-call.js`, `ast/is-undefined.js`,
`ast/is-in-directive-prologue.js`, `contains-node.js`, `get-concise-arrow-body-text.js`,
`get-expression-text.js`, `get-static-value.js` (getStaticRegExp), `imports.js`,
`needs-semicolon.js`, `escape-string.js`, `escape-template-element-raw.js`,
`has-template-incompatible-escape.js`, `fix/replace-argument.js`, `fix/remove-argument.js`,
`fix/remove-parentheses.js`, `shared/typed-array.js`.
- **Built-in collections** (`no-object-methods-with-collections`, from `unicorn:
  rules/no-object-methods-with-collections.js` and `utils/get-builtin-collection-type.js`).
  `builtin_collection_type` finds `Map`/`Set` from `new Map`/`new Set` (global callee),
  through a single declaration's annotation (`Map`, `ReadonlyMap`, `Set`, `ReadonlySet`
  not shadowed by a user type, `readonly`, unions of one kind) or `const` initializer,
  `as`/`satisfies`/`<T>`/`!`, and both branches of a conditional. `(Object).keys(x)`
  counts: ESTree parentheses are transparent.
- **Split limits** (`prefer-split-limit`, from `unicorn: rules/prefer-split-limit.js`).
  The limit goes before the split call's `)`, after a trailing comma when there is one, so
  `a?.split?.("/")` works too. A destructuring pattern's span takes in its type
  annotation (ESTree puts the annotation on the pattern).
- **Impossible length comparisons** (`no-impossible-length-comparison`, from `unicorn:
  rules/no-impossible-length-comparison.js` and `utils/length-or-size.js`). It shares
  `known_non_collection_length` and `has_shape_property_check` with
  `explicit-length-check`. The compared value goes through `static_value_control_flow`,
  so a `let` reassigned before the comparison is not a constant.
- **`globalThis` members** (`no-unnecessary-global-this`, from `unicorn:
  rules/no-unnecessary-global-this.js`). The name must be an available global
  (`Ctx::global_available`: configured or `/* global */`, not shadowed) and not a reserved
  word. A computed key is evaluated without looking up names (literals, plain templates,
  `+`), as `getPropertyName` does with no scope. Existence checks (logical operands,
  `== null`/`=== undefined`, boolean contexts) are kept: a bare missing global throws.
- **Trim before a prefix test** (`no-unnecessary-string-trim`, from `unicorn:
  rules/no-unnecessary-string-trim.js`). The search string's whitespace test uses
  JavaScript's `\s` set (`unicorn_ast::is_js_space`), not Rust's `char::is_whitespace`.
  zod's `z.string()` is recognised by its import: a runtime `{z}` (or `{"z" as x}`) or
  namespace import from `"zod"` exactly.
- **Splice replacements** (`no-unnecessary-splice` and `no-array-splice`, from `unicorn:
  rules/no-unnecessary-splice.js`, `rules/no-array-splice.js` and
  `shared/splice-replacements.js`). `unicorn_splice` decides which calls are no-ops,
  emptying, `shift`/`unshift`/`pop`/`push`; numbers there are only signed literals
  (`getStaticNumberValue`), not constants. `no-array-splice` skips those calls and wants a
  `let`/`var` local with a fresh-array initializer whose other references are member
  accesses, spreads or `for-of` sources only. Without type information its
  tuple/type-parameter check never fires, so only the annotation test remains.
- **Absolute values** (`prefer-math-abs`, from `unicorn: rules/prefer-math-abs.js`).
  An operand may be a BigInt by a literal, `BigInt(x)`, unary minus of one, an
  `as`/`satisfies`/`<T>` bigint type, `BIGINT_MIXED_UNION` guessing (JSDoc included), or a
  parameter/variable whose annotation, default or initializer is one; any of these on
  either side of a comparison drops the report.
- **Collection bracket access** (`no-collection-bracket-access`, from `unicorn:
  rules/no-collection-bracket-access.js`). A key that names a real member is allowed: the
  collection's own prototype names (Set's ES2025 methods included, as Node 24 has them),
  `Object.prototype` names, and `Symbol.iterator`/`Symbol.toStringTag` (static values give
  `Builtin("Symbol.x")`). Conditional, logical and sequence keys with a call, write or
  member read (eslint-utils' `considerGetters`) skip the rule altogether.
- **Manual padding** (`prefer-string-pad-start-end`, from `unicorn:
  rules/prefer-string-pad-start-end.js`). A name stands for its `const` initializer
  (parentheses dropped, as ESTree does) or its function/class declaration when it has one
  definition and no later write. JSDoc and TS annotations go through
  `is_known_non_string`, booleans through `unicorn_types::is_boolean`.
- **Set methods** (`prefer-set-methods`, from `unicorn: rules/prefer-set-methods.js`).
  "Is a Set" is `builtin_collection_type == Some("Set")`. Operands must be free of side
  effects with getters counted (`unicorn_static::has_side_effect_with_getters`, eslint-utils'
  `considerGetters`, shared with `no-collection-bracket-access`), so `obj.set` is never an
  operand. A `filter` inside `new Set(...)` is reported on the `new`, and the inner call
  then skips itself as a call argument.
- **Booleans and useless coercions** (`no-useless-coercion`, from `unicorn:
  rules/no-useless-coercion.js`, `utils/is-boolean.js` and
  `fix/replace-node-with-expression.js`). `is_boolean` is a `Checker` on the shared engine
  (annotations, JSDoc, declarations, function returns) plus syntax: `!`/`delete`,
  comparisons, logical and conditional expressions whose operands all are, global
  `Boolean`/`isFinite`/`isNaN` and boolean static methods (`Array.isArray`, ...), and
  instance methods picked by the receiver's kind (`includes` on arrays and strings, `has`
  on keyed collections, `test` on regexps; `delete` only on mutable collections). A type
  predicate (`x is T`, not `asserts`) is a boolean annotation. "TypeScript" for the
  `Boolean()` exemption is the file extension or the TS parser.

## eslint-plugin-unicorn rules (round 6, h4)

Rules from eslint-plugin-unicorn 79.0.0's recommended set, each at 100% of its oracle, all
`NO_PRESET`. What we learned, in our words (each file's header has the details):

- **no-new-buffer**: the method comes from the first argument (`is_number` means `alloc`;
  arrays, templates, a static string encoding or a static string/array value mean
  `from`); `alloc` with extra arguments is only a suggestion, and none when a comment or a
  side effect would be dropped.
- **relative-url-style**: JavaScript `new URL(a, b)` / `URL.canParse` / `URL.parse` and JSX
  `href`/`src`/... attributes (CSS, HTML and Markdown need other languages). A `./` is added
  or removed only when the URL resolves the same with and without it against the static
  base or three default bases; `unicorn_url.rs` is the small WHATWG resolver that decides
  (special schemes, dot segments, `?`/`#`/empty inputs, `file` drive letters).
  `unicorn_html.rs` decodes HTML attribute character references (numeric ones as the HTML
  parser does; names only from the JSX table, anything else is "can't say") and splits
  `srcset` candidates.
- **prefer-modern-math-apis**: `Math.log(x) * Math.LOG10E`-style products and quotients and
  `Math.sqrt` of a `+` tree of squares. Fixed only when the result provably can't change
  (the value is unused, or the static operands make both forms exact), else suggested.
- **prefer-classlist-toggle**: an `if`/`else` or `?:` choosing between `classList.add(n)`
  and `classList.remove(n)` on the same reference, and `classList[c ? "add" : "remove"](n)`.
  The condition is negated when the true branch removes and dropped when it is a matching
  `classList.contains(n)`; a fix for statements, a suggestion inside expressions, nothing
  when a comment would be lost.
- **no-blob-to-file**: a single-use `const file = new File([blob], "name")` that only
  reaches `URL.createObjectURL(file)` or a `new FormData()` constant's `append`/`set`. The
  suggestion removes the declaration and passes the blob (plus the name for a two-argument
  `append`); it is withheld when a comment sits in, on, or on the line above the
  declaration.
- **no-unsafe-sqlite-interpolation**: `exec`/`prepare` on a `node:sqlite` `DatabaseSync`
  (`new` of the runtime import, the namespace's or default import's `.DatabaseSync`, or a
  `const` alias of either) with a template substitution or any tagged template as the SQL.
  Names resolve to value bindings, past type aliases of the same name; `require()`,
  `import()` and `let` are not followed.
- **no-late-event-control**: `preventDefault()`/`stopPropagation()`/
  `stopImmediatePropagation()` on an event parameter (named or typed as one) in a nested
  function, a generator handler, or after a suspension point of the handler has finished.
  It shares the event checker and suspension helpers of `no-late-current-target-access`.
- **prefer-abort-signal-timeout**: `const c = new AbortController()` directly followed by
  `setTimeout(() => c.abort(), delay)` and otherwise only read as `c.signal`.
  `unicorn_abort.rs` holds the shared controller shape (declarator, abort callback,
  `.signal` reads that aren't aliased, written or asked for `reason`/`throwIfAborted`) and
  the suggestion (rename to `signal`/`abortSignal` when free, remove the statements).
- **prefer-url-search-parameters**: `query.split("&").map(p => p.split("="))` alone or as
  the argument of `Object.fromEntries`/`new Map`/`new URLSearchParams` (reported with its
  wrapper, unless the wrapper has type arguments). Only suggested; nothing when
  `URLSearchParams` is a local runtime binding other than the `url` module's.
- **prefer-number-is-safe-integer**: `Number.isInteger` (renamed, suggested) and
  `x % 1 === 0`, `Math.floor|trunc(x) === x`, lodash/underscore `isInteger`/`isSafeInteger`
  (suggested as `Number.isSafeInteger(x)` when `Number` isn't local).
- **prefer-url-can-parse**: a `try { new URL(a, b); return/assign bool } catch { opposite }`
  becomes `URL.canParse(a, b)` (fixed). Arguments that could throw while converted
  (side effects with implicit conversions, objects, classes, templates with
  substitutions, tagged templates, the global `Symbol`) are skipped; `declare` and
  type-only bindings of `URL`/`Symbol` count as the global.
- **require-passive-events**: `addEventListener` for touch/wheel events with an inline
  listener that can't call `preventDefault()` (every use of its event parameter is a
  member chain without a `preventDefault` link, a write or a dynamic call; no `arguments`)
  gets `passive: true` (fixed; `undefined`/`null` options only reported).
- **prefer-toggle-attribute**: like prefer-classlist-toggle, for `setAttribute(n, v)` /
  `removeAttribute(n)`; a matching `hasAttribute(n)` test makes a plain
  `toggleAttribute(n)`. Fixed only for an empty value without a `force`; a non-static value
  or a `force` after `?.` is reported without a fix.
- **no-prevent-default-in-passive-listener**: `event.preventDefault()` directly in an inline
  listener registered with `{passive: true}` (last `passive` key; plain properties only),
  before any suspension (shares the suspension helpers above); suggests `passive: false`.
- **prefer-path2d**: one `run_file` walk (explicit stack, no recursion) groups canvas path
  calls per function and innermost loop by receiver (`beginPath()` closes a group); the
  receiver is a context by its local annotation (aliases, interfaces' `extends`, type
  parameter constraints, unions/intersections), a `getContext("2d")` initializer or the
  names `context`/`ctx`. Functions passed to the global `requestAnimationFrame`/
  `setInterval`, called in loops, or called (transitively) from those are repeated; two
  builders and a consumer in a repeated draw are reported. No fix.
- **no-invalid-integrity**: the last `integrity` prop of a native JSX `<script>`/`<link>`
  (no spread after it; string with entities decoded, or a control-flow static string) is
  split on HTML whitespace; recognized entries (`sha256`/`sha384`/`sha512`/`ed25519`) must
  be `alg-base64[=|==][?printable-options]` with 43/64/86/43 base64 characters (either
  alphabet); each bad entry is reported, and metadata without a recognized one once.
- **prefer-scoped-selector**: element `querySelector(All)` with one static selector, not on
  `document`, a non-element root or a non-node value: branches are split CSS-tokenizer
  style (strings, comments, escapes, nested groups), and a branch with a top-level
  combinator but no `:scope` token is reported; one branch suggests a `:scope ` prefix.
- **prefer-url-href**: `<url>.toString()` and global `String(<url>)` for a syntactically
  known native URL (`rules/unicorn_url_type.rs`: global or `url`-module `URL` constructor,
  `const` initializers, annotations through local aliases, return types; annotations of
  parameters and return types don't see types declared in the function body). Fixed to
  `.href` only for a fresh `new URL(...)`, without lost comments.
- **no-url-in-search-params**: `new URLSearchParams(arg)` with a URL (the builtin type
  checker with `URL`, its `url`-module imports, no class heritage), a URL's or `location`'s
  `.href`, or a static `scheme://` string that parses (authority host/port checks only).
  Suggestions: a detached `.search` copy, the live `.searchParams` (parenthesized as a
  `new` callee containing a call), or `new URL(...).searchParams`.
- **no-invalid-url-protocol-comparison**: `<url>.protocol` (shared URL detection) compared
  by equality, `switch` or `[...].includes`/`indexOf`/`lastIndexOf`/`new Set([...]).has`
  with a scheme-like static string not in `lowercase:` form; fixed inside the quotes unless
  another `case` already has that protocol.
- **no-incorrect-query-selector**: `qsa(...)[0]`/`.at(0)`/`.item(0)` (first-match, fixed),
  `querySelectorAll("#id")` (id-selector), a `querySelectorAll()` result or `const` used as a
  control-flow test (length-check, sharing explicit-length-check's `boolean_ancestor` and
  `is_control_flow_test`), and nullish comparisons of both queries.
- **no-unsafe-buffer-conversion**: `new <TypedArray>(v.buffer...)`, `Buffer.from(v.buffer...)`
  (prefer-spread's `is_buffer_reference`) and `v.buffer.slice(...)` without the view's
  `byteOffset`/`byteLength` (or with a known wrong one); no type information, so every
  view counts. The suggestion needs no `?.`, no comment and a side-effect-free view.
- **no-invalid-style-set-property**: `<style>.setProperty(name, value[, priority])` on a style
  declaration (builtin type checker, else `style`/`.style`): a camelCase name whose dashed
  form is a CSS property (`rules/unicorn_css_properties.rs`: the 724 property names mdn-data
  lists, shipped with css-tree 4.1.2, as data; plus the WHATWG `-webkit-` aliases), a value
  with a top-level `!important` (a small css-syntax-3 tokenizer: comments, strings, escapes,
  `url(`, functions and blocks), and a non-empty priority other than `important`.
- **prefer-observer-apis**: a `resize`/`scroll` listener (inline, or a never-reassigned
  function binding) whose body reads layout (element `offset*`/`client*`/`scroll{Width,Height}`
  and `getBoundingClientRect()`/`getClientRects()` on anything that could be an element,
  the global `innerWidth`/`innerHeight`, `visualViewport.width`/`height`, also through object
  patterns); writes and `delete` don't count.

Read to understand (not ported), eslint-plugin-unicorn `rules/`: `no-new-buffer.js`,
`relative-url-style.js`, `shared/get-srcset-candidates.js`, `prefer-modern-math-apis.js`,
`utils/get-const-variable-initializer.js`, `utils/get-primitive-literal-value.js`,
`fix/replace-node-or-token-and-spaces-before.js`, `fix/remove-parentheses.js`,
`prefer-classlist-toggle.js`, `utils/get-negated-expression-text.js`,
`utils/should-add-parentheses-to-unary-expression.js`, `utils/single-statement.js`,
`fix/replace-member-expression-property.js`, `ast/is-expression-statement.js`,
`ast/literal.js`, `no-blob-to-file.js`, `utils/comments.js`, `fix/append-argument.js`,
`fix/remove-statement.js`, `no-unsafe-sqlite-interpolation.js`, `no-late-event-control.js`,
`shared/late-event-handler.js`, `utils/is-event.js`, `utils/contains-suspension-point.js`,
`prefer-abort-signal-timeout.js`, `shared/abort-controller.js`,
`utils/is-global-identifier.js`, `utils/function-body.js`, `prefer-url-search-parameters.js`,
`ast/call-or-new-expression.js`, `prefer-number-is-safe-integer.js`, `prefer-url-can-parse.js`,
`require-passive-events.js`, `prefer-toggle-attribute.js`, `utils/has-optional-chain-element.js`,
`no-prevent-default-in-passive-listener.js`, `prefer-path2d.js`, `no-invalid-integrity.js`, `prefer-scoped-selector.js`,
`prefer-url-href.js`, `utils/is-url.js`, `utils/scope.js`, `utils/imports.js`,
`no-url-in-search-params.js`, `no-invalid-url-protocol-comparison.js`,
`no-incorrect-query-selector.js`, `no-unsafe-buffer-conversion.js`, `shared/buffer-reference.js`,
`no-invalid-style-set-property.js`, `prefer-observer-apis.js`, `utils/get-function-from-identifier.js`.
