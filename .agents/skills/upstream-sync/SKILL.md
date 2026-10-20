---
name: upstream-sync
description: Review or prepare a TypeScript bump PR - re-pin TypeScript, sync upstream tests/libs/diagnostics, read the upstream diff, regenerate code, run the tsc oracle, file the differences as tasks and check the gates. Use when a bump PR from the nightly upstream job appears or when asked to move ts-r to a new TypeScript version.
---

# Upstream sync (TypeScript bump)

ts-r supports the latest TypeScript 7 stable within 7 days of release. A bump PR changes
[`upstream/UPSTREAM.json`](../../../upstream/UPSTREAM.json) and nothing is merged until every
gate is back where it was. Upstream tests, baselines, `lib.*.d.ts` files and diagnostic messages
are inputs; the merged TypeScript PRs' descriptions and tests are the spec. **Never copy or
translate compiler source.**

Command reference: [xtask/AGENTS.md](../../../xtask/AGENTS.md).

## 1. Orient

```sh
cat docs/state/compat.json            # current pin, last sync, pass rates, open task groups
cargo xtask upstream check            # what is newer upstream (exit 1 = stable bump available)
```

If you are preparing the bump yourself (no bot PR yet), work in a worktree on a branch named
`upstream/typescript-<version>`:

```sh
cargo xtask upstream bump --to <version>      # e.g. 7.0.3; omit --to for the latest stable
git diff upstream/UPSTREAM.json docs/state/compat.json
```

## 2. Sync the data files

```sh
cargo xtask upstream sync             # target/upstream/typescript at the new pin (gitignored)
```

Only tests, baselines, `src/lib`, `tests/lib`, the diagnostic catalogs and license files are
checked out. If the command refuses a path, do not widen the allow-list; ask a person.

## 3. Read what changed

```sh
OLD=<previous version or sha>; NEW=<new version>
cargo xtask upstream diff "$OLD" "$NEW" --out target/upstream/diff.md
```

Read `target/upstream/diff.md` top to bottom. For each new or changed test, open the compare link
at the top and find the merged PR that added it; its description explains the intended behaviour.
Paste the summary table into the PR description.

## 4. Regenerate

```sh
cargo xtask codegen diagnostics --out <path the diagnostics crate includes>
cargo xtask codegen diagnostics --out <same path> --check
```

The diagnostics crate's AGENTS.md says where the generated file lives. Never hand-edit it.
New diagnostic codes from step 3 usually mean new checker work: note them for step 6.

## 5. Run the oracle

For each corpus project or materialized conformance test directory:

```sh
cargo xtask oracle <project-dir>... --out target/oracle/tsc.json
cargo xtask oracle <project-dir>... --cmd "target/release/ts-r-check" --out target/oracle/ours.json
cargo xtask tasks --ours target/oracle/ours.json --tsc target/oracle/tsc.json --out target/oracle/tasks.md
```

(Until `ts-r-check` exists, only the tsc side can run; record its output so the next run has a
baseline.)

## 6. File the tasks

Each `## TSxxxx` group in `tasks.md` becomes one issue labelled `upstream-sync`, with the
group's checklist and a minimal reproduction (use the `minimize-repro` skill). Order them: syntax
(1xxx) before checker (2xxx), largest groups first. Update `openTaskGroups` in
`docs/state/compat.json` with the issue numbers.

## 7. Check the gates

All must hold before merge; never lower a gate to make the PR pass:

- conformance pass rate at the new pin back to where it was (`passRates` in compat.json),
- corpus diagnostics identical to tsc at the new pin (`tasks.md` says "No differences"),
- generated files up to date (`cargo xtask codegen diagnostics --check`),
- no benchmark regression above 5% (`bench-compare` skill),
- `cargo xtask agents-check`, `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`.

A person reviews and merges the bump PR.
