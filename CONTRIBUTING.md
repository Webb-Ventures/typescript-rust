# Contributing to ts-r

Thanks for helping. Bug reports, mismatches with tsc, ESLint or esbuild, docs fixes and code are
all welcome. This guide covers how to set up, what a change must pass, and how pull requests are
reviewed and merged.

Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md). AI-assisted work follows
the [AI policy](AI_POLICY.md). Security issues go through [SECURITY.md](SECURITY.md), not public
issues.

## How this repo works

The core team develops ts-r in a private repo and publishes it here. Every change merged there
arrives as a pull request from a `sync/tsc-rust-<sha>` branch, labelled `sync`.

Your pull requests are merged here, into `main`.

## Before you start

- **Small fixes** (a wrong diagnostic, a crash, a docs error): open a pull request directly.
- **Anything larger** (a new rule, a new flag, a change to how a tool behaves): open an issue
  first so we can agree on the approach. Design decisions are recorded in
  [docs/design/](docs/design/README.md), and changes to those records are reviewed by people.
- **A mismatch with tsc, ESLint or esbuild**: an issue with the smallest input that shows it, and
  both tools' output, is the most useful report there is.

Read [AGENTS.md](AGENTS.md) before your first change. It applies to people as much as to agents:

- **The from-scratch rule.** No dependency on another compiler, bundler or linter, and no code
  copied or translated from tsc, esbuild, ESLint or any other tool. Their test suites and outputs
  are oracles: run them and compare, but don't port them.
- **Resource limits.** Anything that can run long or grow (fuzzing, corpus runs, benchmarks, real
  projects) runs through `scripts/limit.sh`.
- **Code rules.** No panics on user input, deterministic output, index-based data in hot paths.
- **Each crate's `AGENTS.md`** lists its invariants. Read it before editing the crate, and update
  it in the same change when an invariant changes.

## Setup

You need:

- the Rust toolchain pinned in `rust-toolchain.toml` (rustup installs it on first use);
- Node 22 or newer for the tests that compare against tsc and esbuild.

```sh
git clone https://github.com/<you>/typescript-rust
cd typescript-rust
cargo xtask upstream sync --minimal   # TypeScript's lib files (the checker tests need them)
cargo build
```

## The checks a change must pass

CI runs these on Linux, macOS and Windows. Run them before you push:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo xtask agents-check             # every crate documented, every link resolves
```

A change to the checker, parser, emitter or linter should also say how it moved the relevant
oracle, for example the conformance count from `cargo xtask oracle`. A change that could affect
speed is benchmarked on the pull request: a slowdown beyond the gate in
[bench/scenarios.toml](bench/scenarios.toml) blocks the merge.

Tests live next to the code (unit tests) or under `crates/<crate>/tests/` (fixtures). Snapshot
files are plain text, so diffs stay readable.

## Pull requests

1. Fork the repo, branch from `main`, and open a pull request into `main`. Fill in the template,
   including the AI assistance section.
2. **CI starts once a maintainer approves.** For contributors outside the team, CI's first job
   fails with a note until a maintainer approves your latest commit. The approval re-runs it.
   Each new push needs a new approval, so batch your fixes where you can.
3. **Review.** A maintainer reviews the change. They may ask the review bot for an automated
   review (`@autoreview`) or start a benchmark run (`/bench`). Both run only when a maintainer
   asks. A change to the code or the bench is also benchmarked against `main` on GitHub's Linux
   runners (the Bench (Actions) workflow, once its run is approved): the job summary has the
   tables, and the job fails on a slowdown beyond the gate.
4. **Merge.** A maintainer merges once CI, the review and any benchmark pass.

Commit messages say what changed and what it measured (an oracle count, a timing), in the
imperative: `Checker: report TS2322 for readonly tuple spreads (conformance +4)`. Small,
focused pull requests are reviewed fastest.

## Licensing

ts-r is dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE). Unless you say
otherwise, any contribution you submit is licensed the same way, with no additional terms or
conditions. Only submit work you have the right to license like this.
