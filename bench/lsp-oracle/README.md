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

8c14f29 (2026-10-24, macOS arm64 (Apple M4 Max), tsc 7.0.2, `--jobs 4`, 27 s): **685 of
2550 compared tests pass (26.9%)**; 3694 of 9216 queries agree. 3769 of the 6319 test files
are skipped (3698 verify no compared feature).

| Feature | Pass | Fail | Skip | Queries agree |
| --- | ---: | ---: | ---: | ---: |
| hover | 192 | 326 | 20 | 1420 / 2513 |
| completion | 239 | 820 | 74 | 789 / 3037 |
| references | 72 | 273 | 12 | 628 / 1443 |
| rename | 85 | 91 | 13 | 267 / 549 |
| definition | 42 | 125 | 25 | 288 / 528 |
| signatureHelp | 42 | 110 | 24 | 234 / 658 |
| highlights | 13 | 104 | 20 | 58 / 431 |
| typeDefinition | 0 | 16 | 3 | 10 / 57 |

## Run history

| Commit | Date | Pass | Compared | Rate | Change |
| --- | --- | ---: | ---: | ---: | --- |
| 8c14f29 | 2026-10-24 | 685 | 2550 | 26.9% | First run |
