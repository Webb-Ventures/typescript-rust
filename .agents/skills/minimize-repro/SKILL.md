---
name: minimize-repro
description: Shrink a failing project or test (a diagnostic that differs from tsc 7, a crash, wrong output) to the smallest input that still fails, then add it as a fixture test in the owning crate. Use before fixing any bug found on a real project, corpus repo or upstream test.
---

# Minimize a reproduction

A fix starts with the smallest input that still shows the bug. Small repros make the cause
obvious, make review easy and become permanent regression tests.

## 1. Pin down the failure

Write down exactly what "still fails" means, as a command you can rerun:

- **Diagnostic difference vs tsc 7:**
  ```sh
  cargo xtask oracle target/repro/case --out target/repro/tsc.json
  cargo xtask oracle target/repro/case --cmd "target/release/ts-r-check" --out target/repro/ours.json
  cargo xtask tasks --ours target/repro/ours.json --tsc target/repro/tsc.json
  ```
  "Still fails" = the same `TSxxxx` group still appears (missing, extra or mismatched).
- **Crash / panic:** the ts-r command exits with a panic message. Never accept a panic on user
  input, even a weird one: that is the bug.
- **Wrong output (build/transform):** running the output differs from running tsc's or esbuild's
  output for the same input.

## 2. Copy the input somewhere disposable

```sh
mkdir -p target/repro/case && cp -R <failing project or test files> target/repro/case/
```

For an upstream conformance test (single file with `// @strict: true` style directives), create
`target/repro/case/tsconfig.json` whose `compilerOptions` carry the same settings, and split
`// @filename:` sections into separate files.

## 3. Shrink, rerunning the check after every step

Go coarse to fine and undo any step that makes the failure disappear:

1. Delete whole files (keep the one the diagnostic is in).
2. Remove `compilerOptions` one at a time; drop `lib` entries; prefer `"noLib": true` with a tiny
   local `lib.d.ts` if the bug doesn't depend on the real libs.
3. Delete declarations, then statements, then members and type arguments.
4. Inline imports; replace library types with local minimal ones.
5. Rename to short, neutral names (`a`, `T`, `f`) once it is small.

Stop when no single deletion keeps it failing. Typical result: one file, under 20 lines.

## 4. Add it as a test

Put the repro next to the code that will be fixed, under `crates/<crate>/tests/` (see that
crate's AGENTS.md for the fixture layout). Store the expected result as plain text
(tsc's normalized diagnostics from `cargo xtask oracle`, or the expected output), and note
the TypeScript version it was taken from (`docs/state/compat.json`).

The test must fail before the fix and pass after it. Then continue with `fix-conformance-diff`
or the crate's normal workflow.
