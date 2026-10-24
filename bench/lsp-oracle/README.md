# LSP oracle for ts-r-lsp

`run.mjs` treats TypeScript's fourslash tests as data: it reads the test files in
`target/upstream/typescript/tests/cases/fourslash/` (TypeScript 7.0.2, commit
`1e4744d68260a7cb91b62b12edc3f6a2187faaf1`; fetched by `cargo xtask upstream sync`), and for
every test that verifies an editor feature it:

1. writes the test's files (with their `// @` compiler directives as a `tsconfig.json`) to a
   temporary project, with the `/*marker*/` and `[|range|]` annotations removed,
2. opens them in tsc 7.0.2's language server (`tsc --lsp --stdio`, the `typescript` package
   pinned in `bench/package.json`) and in `ts-r-lsp` (the ts-r backend),
3. sends both servers the same request at every marker: hover, completion, definition, type
   definition, references, rename, signature help or document highlights (whichever the test
   verifies), and compares the normalized answers.

The fourslash file's own expectations are not used: tsc 7's live answer is the oracle, so a
test where tsc 7 itself disagrees with the expectation is compared against what tsc 7 says.
A test passes when every query agrees. Normalization: locations as `file:range` sets (order
does not matter), hover as its code block plus docs with whitespace collapsed, signature help
as the labels, active signature and active parameter, highlights as range plus kind. A
completion query agrees when ts-r's list contains every entry tsc ranks above globals and
keywords (locals, members: `sortText` below 15) and tsc's list contains every such entry of
ours; globals and keywords are not compared. Only the tests' files and markers are used;
nothing from TypeScript's implementation runs in ts-r-lsp.

```sh
cargo build --release -p ts_r_lsp -j 4
(cd bench && ../scripts/limit.sh -- npm ci)
scripts/limit.sh --time-s 1500 -- node bench/lsp-oracle/run.mjs --jobs 4
node bench/lsp-oracle/fails.mjs [REGEX]   # list failures from target/lsp-oracle/results.json
```

`--filter REGEX` and `--feature hover,completion` narrow the run, `--show N` prints the first
N differences, `--keep` leaves the temporary projects. Tests are skipped when they verify no
compared feature (code fixes, formatting, navigation bars, ...), when both servers answer
nothing at every marker, or when they need a setup the harness can't give both servers the
same way: non-TS extensions, `outFile`, symlinks, files outside the project, non-source files,
two files with one name, `noImplicitReferences`, `resolveReference`. `results.json` lists
every skip reason.

## Results

4eda074 (2026-10-24, macOS arm64 (Apple M4 Max), tsc 7.0.2, `--jobs 4`, 27 s):
**795 of 2549 compared tests pass (31.2%)**; 4050 of 9094 queries agree.
3770 of the 6319 test files are skipped (3698 verify no compared feature, 123 get no answer
from either server). Up from 685 of 2550 at 8c14f29: 110 tests fixed, none regressed.

| Feature | Pass | Fail | Skip | Queries agree |
| --- | ---: | ---: | ---: | ---: |
| hover | 198 | 320 | 20 | 1446 / 2513 |
| completion | 293 | 765 | 75 | 905 / 2915 |
| references | 72 | 273 | 12 | 628 / 1443 |
| rename | 85 | 91 | 13 | 267 / 549 |
| definition | 45 | 122 | 25 | 297 / 528 |
| signatureHelp | 78 | 74 | 24 | 406 / 658 |
| highlights | 24 | 93 | 20 | 91 / 431 |
| typeDefinition | 0 | 16 | 3 | 10 / 57 |

Remaining failures by cause (example tests in parentheses):

- **Type display (ts_r_checker)**, most hover and many signature-help failures: type aliases
  printed expanded or not where tsc differs, no `... N more ...` truncation
  (`quickInfoCanBeTruncated`), mapped and intersection types, `typeof import("...")` with an
  absolute path, `| undefined` added to optional parameters (`signatureHelp_unionType`),
  module-qualified names (`qualifyModuleTypeNames`: `m.c`), type predicates printed as
  `boolean` (`signatureHelpOnTypePredicates`), merged class and namespace aliases.
- **Highlights (ts_r_lsp)**: keyword occurrences (`switch`/`case`/`break`, `if`/`else`,
  `async`/`await`, `return`, modifiers, `this`: the `getOccurrences*` tests), members
  inherited across classes, `export { x as y }` names, JSX tag names.
- **References and rename (ts_r_lsp)**: default exports, arbitrary module namespace names,
  constructors (`findAllRefsOfConstructor2`), import types (`findAllRefs_importType_named`),
  JSDoc `@typedef`, string literal types (`renameStringLiteralTypes2`), the extra edits tsc
  makes when renaming a default import.
- **Definition (ts_r_lsp)**: `override` to the base member (`goToDefinitionOverriddenMember*`),
  keywords (`return`, `await`, `yield`, `case`), ambient and shorthand module names,
  contextually typed object literal properties, labels, meta properties, JSX.
- **Completion (ts_r_lsp)**: no module-specifier or path completions (locals are listed inside
  import strings: `completionForStringLiteral*Import*`), no string-literal-type completions,
  private `#` names, JSX attributes.
- **Signature help (ts_r_lsp)**: type-argument help (`f<|`, `signatureHelpTypeArguments`),
  `super(` calls (`signatureHelpForSuperCalls1`), JSX (`tsxSignatureHelp1`), rest tuples
  expanded into parameters (`signatureHelpExpandedRestTuples`), contextual signatures for a
  callback argument (`signatureHelp_contextual`), the active overload after an argument that
  rules one out (`signatureHelpOnOverloadsDifferentArity`), generic overloads other than the
  resolved one shown uninstantiated.
- **Type definition (ts_r_lsp)**: every test fails on at least one marker; ts-r answers nothing
  on a declaration's own name (`goToTypeDefinition3`, `goToTypeDefinitionModifiers`) and on
  module names (`goToTypeDefinitionModule`), and goes to the enum rather than the enum member
  (`goToTypeDefinitionEnumMembers`).

## Run history

| Commit | Date | Pass | Compared | Rate | Change |
| --- | --- | ---: | ---: | ---: | --- |
| 8c14f29 | 2026-10-24 | 685 | 2550 | 26.9% | First run |
| 4eda074 | 2026-10-24 | 795 | 2549 | 31.2% | Tagged templates, instantiated and `new`-less signature help; `Function` members and `arguments` completions; tsc 7 alias hover; highlight kinds; overload definitions |
