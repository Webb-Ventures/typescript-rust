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
N differences, `--keep` leaves the temporary projects, `--tsr PATH` runs another ts-r-lsp
binary, and `--dump FILE` appends every compared query as a JSON line
(`{test, feature, marker, file, at, tsc, tsr}`; `at` is the marker's line with `¦` at the marker) for grouping mismatches by cause. Tests are skipped when they verify no
compared feature (code fixes, formatting, navigation bars, ...), when both servers answer
nothing at every marker, or when they need a setup the harness can't give both servers the
same way: non-TS extensions, `outFile`, symlinks, files outside the project, non-source files,
two files with one name, `noImplicitReferences`, `resolveReference`. `results.json` lists
every skip reason.

## Results

682d76b (2026-10-27, macOS arm64 (Apple M4 Max), tsc 7.0.2, `--jobs 4`):
**2085 of 2516 compared tests pass (82.9%)**; 8023 of 8967 queries agree.
3803 of the 6319 test files are skipped (3698 verify no compared feature, 156 get no answer
from either server). Up from 1967 of 2516 at 5628954: 118 tests fixed (JSDoc completions;
completions in template holes, generic-call object literals, awaited members, import
statement keywords and import types; references, rename, definition and hover on names in
`{@link}`, `@see` and `@throws` paths; references and rename of string literal types; JSX
intrinsic tags and attributes), none lost. The compared count is unchanged: two rename tests
where tsc answers nothing (`renameImport`, `jsObjectDefinePropertyRenameLocations`) now get
no answer from ts-r either and are skipped, and tsc's timeouts move a few others in and out
run to run (rerun with `--filter` and counted).

| Feature | Pass | Fail | Skip | Queries agree |
| --- | ---: | ---: | ---: | ---: |
| hover | 419 | 101 | 18 | 2338 / 2515 |
| completion | 875 | 151 | 107 | 2553 / 2792 |
| references | 254 | 91 | 12 | 1160 / 1443 |
| rename | 144 | 32 | 13 | 414 / 549 |
| definition | 139 | 27 | 26 | 482 / 526 |
| signatureHelp | 136 | 14 | 26 | 637 / 656 |
| highlights | 105 | 12 | 20 | 387 / 431 |
| typeDefinition | 13 | 3 | 3 | 52 / 55 |

Remaining failures by cause (example tests in parentheses):

- **Type display (ts_r_checker)**, most hover and many signature-help failures: conditional
  type aliases resolved where tsc defers them (`quickInfoFunctionCheckType`), no `... N
  more ...` truncation (`quickInfoCanBeTruncated`), mapped and intersection types, `typeof import("...")` with an
  absolute path, `| undefined` added to optional parameters (`signatureHelp_unionType`),
  module-qualified names (`qualifyModuleTypeNames`: `m.c`) and unqualified names inside
  signatures (`quickInfoGenericTypeArgumentInference1`: `Underscore.Iterator`), types not
  named through their import alias (`quickInfoExportAssignmentOfGenericInterface`), type
  predicates printed as `boolean` (`signatureHelpOnTypePredicates`), contextual parameters
  of generic signatures (`contextualTypingGenericFunction1`).
- **Quick info (ts_r_lsp)**: `@property` names inside JSDoc (`quickInfoPropertyTag`),
  hyphenated property names (`jsDocTagsWithHyphen`), `{@link https://...}` names in `@property`
  tags (`quickInfoForJSDocWithHttpLinks`), type-recovery corners of `@param {..}` (`quickInfoCommentsCommentParsing`), keywords and
  meta properties (`arguments`, `import.meta`, `new`, `super`, `as const`), enum members
  with odd names (`(typeof Foo)["\t"]`), discriminated contextual unions
  (`quickInfoUnion_discriminated`), `(alias)` on `export as namespace` and JavaScript
  `require` (`quickInfoForUMDModuleAlias`, `jsRequireQuickInfo`).
- **Highlights (ts_r_lsp)**: members inherited across classes
  (`documentHighlightAtInheritedProperties5`), `export { x as y }` names
  (`documentHighlightInExport1`), JSDoc `@typedef`
  (`jsdocTypedefTagServices`), a few keyword sets (`getOccurrencesConst04`,
  `getOccurrencesTryCatchFinallyBroken`), JSX tag names.
- **References and rename (ts_r_lsp)**: keywords (`referencesForStatementKeywords`,
  `referencesForExpressionKeywords`), JSDoc `@param`, `@template` and `@import`
  (`findAllRefsJsDocImportTag*`), import types (`findAllRefs_importType_named`), module
  specifiers, CommonJS `require` and `module.exports`, JSX runtime imports
  (`jsxFindAllReferencesOnRuntimeImportWithPaths1`), quoted export names
  (`arbitraryModuleNamespaceIdentifiers_*`), going back up from a
  re-export of a renamed default (`import boom from "./d"`), constructors reached through
  export specifiers (`findAllRefsOfConstructor_multipleFiles`).
- **Definition (ts_r_lsp)**: decorator and JSX overloads (the checker records no resolved
  signature for them), JavaScript expandos and `require` destructuring
  (`goToDefinitionExpandoClass*`, `goToDefinitionDestructuredRequire`), `instanceof`
  (`Symbol.hasInstance`), meta properties (`goToDefinitionMetaProperty`), a `yield` and a
  `case` edge (`goToDefinitionYield3`, `goToDefinitionSwitchCase6`), import types.
- **Completion (ts_r_lsp)**: JSX in tags the parser closes early
  (`jsxTagNameCompletionUnderElementUnclosed`), `this` in property initializers
  (`completionsThisPropertiesInPropertyDeclaration2`) and contextual `this`
  (`memberListOnContextualThis`), import statement snippets (`importStatementCompletions4`),
  object literals in overloaded calls and partial generic types
  (`completionListInObjectLiteral7`, `completionsWithOptionalPropertiesGenericPartial2`),
  protected and private members after `.`, `import("./x").` inside JSDoc braces
  (`javascriptModulesTypeImport`), exhaustive `case` snippets, shorthand properties in call arguments
  (`completionPropertyShorthandForObjectLiteral*`), type parameters in their own defaults
  and constraints, keyword sets by position, auto-import details.
- **Signature help (ts_r_lsp)**: JSX (`tsxSignatureHelp1`, `signatureHelpJSX`), `... N
  more ...` truncation (`quickInfoCanBeTruncated`), JSDoc `@overload`
  (`quickInfoJsDocTags13`), JavaScript fallbacks (`signatureHelpJSMissingPropertyAccess`),
  contextual generic callbacks (`callback(y3: T)`), the overload order of string literal
  overloads (`overloadOnConst`), argument names in calls of unions of dissimilar signatures
  (`calledUnionsOfDissimilarTyeshaveGoodDisplay`), the file order of a multi-file test.
- **Type definition (ts_r_lsp)**: `import.meta` (`goToTypeDefinitionImportMeta`) and JSDoc
  `@typedef` (`goToTypeDefinition_typedef`, `jsDocServices`).

## Run history

| Commit | Date | Pass | Compared | Rate | Change |
| --- | --- | ---: | ---: | ---: | --- |
| 8c14f29 | 2026-10-24 | 685 | 2550 | 26.9% | First run |
| 4eda074 | 2026-10-24 | 795 | 2549 | 31.2% | Tagged templates, instantiated and `new`-less signature help; `Function` members and `arguments` completions; tsc 7 alias hover; highlight kinds; overload definitions |
| f591d51 | 2026-10-26 | 1048 | 2543 | 41.2% | Completions in string literals and module specifiers (package `exports`/`imports`/`typesVersions`, `paths`), object literal and binding pattern properties, `name?` labels for optional members, own-initializer and CommonJS scope rules |
| 88dfde6 | 2026-10-26 | 1308 | 2541 | 51.5% | tsc 7 quick info in hover (a line per meaning and overload, resolved signatures at calls, owners with type arguments, `(alias)` lines, multi-line object types); completions in class, interface and type literal bodies, `this` members, JSX attributes, closing tags and intrinsic tags |
| 7068c74 | 2026-10-26 | 1562 | 2540 | 61.5% | tsc 7 quick-info printing mode in hover; references, rename and highlights on declaration keywords and modifiers, `get`/`set`, through destructuring targets, `for`-`of` bindings, contextually typed object literals and string or numeric literal keys; keyword highlights (`switch`/`case`/`break`, `if`/`else`, `try`, `async`/`await`, `return`, modifiers); definition at `override` and keywords; tsc's type definition rule (declaration names, modules, enum members) |
| 1960c24 | 2026-10-26 | 1706 | 2517 | 67.8% | Completions from expected types (type literals in type arguments offer the constraint's members, string, number and bigint literal entries), no list where a name is being declared, the scope of unclosed bodies, members of possibly undefined values and non-identifier members; signature help at type argument lists, `super(`, contextually typed parameter lists and tagged templates, the resolved or best candidate instantiated, rest tuples expanded; references on constructors |
| aed0aea | 2026-10-27 | 1864 | 2517 | 74.1% | References, rename and highlights through inherited and overridden members, search meanings (value, type, namespace), renaming import and export aliases, default keywords, private names and computed keys; hover JSDoc parsed and rendered as tsc 7 does (tags, links, examples) from the declaration tsc documents, contextual object literal properties, module names, anonymous classes and functions, `prototype` owners |
| 5628954 | 2026-10-27 | 1967 | 2516 | 78.2% | Go to definition at calls (the resolved signature), unnamed functions and classes by their assigned name, literal and binding keys through contextual and destructured types, ambient modules and import specifiers; completions read possible type arguments as types, offer namespaces in type positions only with a type below them, and list a module's exports in import and export clauses; type alias hovers print the aliased type (checker's InTypeAlias mode), JSDoc `@typedef`/`@callback` hovers and docs, setter write types, instantiated candidates at unresolved generic calls; names inside JSDoc tag types found for every feature; daemon test flakes fixed |
| 682d76b | 2026-10-27 | 2085 | 2516 | 82.9% | JSDoc completions (tag names, `@param` names, types in tag braces, `@import` specifiers and attributes); completions in template holes, object literals of generic calls (the declared parameter type with undecided type parameters at their constraints), awaited promise members, `import` statement keywords and `import("x").` members; references, rename, definition and hover on names in `{@link}`, `@see` and `@throws` paths; references, rename and highlights of string literal types; JSX intrinsic tags and attributes in references, rename, definition and hover |
