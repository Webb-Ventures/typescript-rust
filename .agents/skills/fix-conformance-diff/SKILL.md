---
name: fix-conformance-diff
description: Take one group of diagnostic differences between ts-r and tsc 7 (from cargo xtask tasks), find the cause, fix it from observed behaviour, design records and tsc source read as a reference (never ported), and prove nothing else changed. Use when working an upstream-sync task or any "ts-r reports X but tsc reports Y" issue.
---

# Fix a conformance difference

Our checker must report exactly what tsc 7 reports at the pinned version
([`docs/state/compat.json`](../../../docs/state/compat.json)). Work **one `TSxxxx` group at a
time**. Command reference: [xtask/AGENTS.md](../../../xtask/AGENTS.md).

## 1. Pick the group and capture "before"

```sh
cargo build --release --workspace
cargo xtask oracle <project-dirs...> --out target/conf/tsc.json
cargo xtask oracle <project-dirs...> --cmd "target/release/ts-r-check" --out target/conf/before.json
cargo xtask tasks --ours target/conf/before.json --tsc target/conf/tsc.json --out target/conf/tasks.md
```

Choose one group in `tasks.md`. Note its counts (missing / extra / mismatch).

## 2. Understand the expected behaviour

- Upstream tests and baselines are in `target/upstream/typescript` (run
  `cargo xtask upstream sync` if missing): the test under `tests/cases/`, tsc 7's expected
  errors under `tsc/testdata/baselines/reference/submodule/` (`*.errors.txt`).
- The message template for the code is in the generated diagnostics catalog
  (`cargo xtask codegen diagnostics`).
- Find the TypeScript PR that added the test (GitHub search for the test file name) and read its
  description and discussion. That, the tests and observed tsc behaviour are the spec.
- Read the design record for the area in `docs/design/` and the owning crate's AGENTS.md.

- Read tsc's source to see how the rule works: which error it reports when several apply, in
  what order it checks, how it prints the type. The clone is at `../TypeScript` from the main
  checkout; read it at the pinned tag without checking it out
  (`git -C ../TypeScript show v7.0.2:tsc/internal/checker/relater.go`; the Go checker is
  `tsc/internal/checker/`). Then write our own
  implementation on our data model. **Never copy or translate it line by line, and never open
  ts-rust or any other Rust port of TypeScript.** Record the rule in your own words in the
  crate's AGENTS.md, citing the upstream file. Confirm with probes through `cargo xtask oracle`.

## 3. Reproduce small

Use the `minimize-repro` skill to get a one-file case and add it as a failing test in the owning
crate.

## 4. Fix

Fix the cause in the owning crate, keeping its invariants (index-based data, no panics on user
input, no per-node allocation in hot paths). If the fix needs an invariant to change, stop and
propose a design-record change first.

## 5. Prove nothing else changed

```sh
cargo build --release --workspace
cargo xtask oracle <project-dirs...> --cmd "target/release/ts-r-check" --out target/conf/after.json
cargo xtask tasks --ours target/conf/after.json --tsc target/conf/tsc.json --out target/conf/tasks-after.md
cargo xtask tasks --ours target/conf/after.json --tsc target/conf/before.json   # every change you caused
```

The last report must list only diagnostics from the group you fixed. Anything else is a
regression to fix before review. Then:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo xtask agents-check
cargo xtask bench          # if you touched a measured path; see bench-compare
```

## 6. Write the PR

Group code, before/after counts, the repro test, the upstream PR you used as spec, and the
"nothing else changed" report. Update `docs/state/compat.json` if pass rates moved.
