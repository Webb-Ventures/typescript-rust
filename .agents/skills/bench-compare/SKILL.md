---
name: bench-compare
description: Run the ts-r benchmark harness against main and the pinned reference tools (tsc 7, esbuild, oxc-parser, tsx), check the 5% regression gate, and write the comparison table into the PR. Use for any PR that touches a crate on a measured path, or when asked how fast something is.
---

# Bench compare

Benchmarks gate merges: one of our tools more than 5% slower than `main` blocks the PR.
Scenarios live in [`bench/scenarios.toml`](../../../bench/scenarios.toml); the reference tools
are pinned in [`bench/package.json`](../../../bench/package.json). Command reference:
[xtask/AGENTS.md](../../../xtask/AGENTS.md).

## 1. Prerequisites

```sh
hyperfine --version          # brew install hyperfine | apt install hyperfine | cargo install --locked hyperfine
node --version               # Node 22.6+ (reference tools)
cargo xtask upstream sync --minimal   # parse inputs (lib.dom.d.ts etc.) in target/upstream
cargo xtask bench --list     # scenarios, which tools are active / pending, exact commands
```

`cargo xtask bench` installs the reference tools into `bench/node_modules` (`npm ci`) on first
use; `--setup` forces a reinstall.

## 2. Build both sides

Close other heavy programs; timings on a busy laptop are noise.

```sh
git worktree add ../ts-r-main origin/main
(cd ../ts-r-main && cargo build --release --workspace)
cargo build --release --workspace
```

## 3. Run

```sh
cargo xtask bench --bin-dir ../ts-r-main/target/release --save main
cargo xtask bench --save pr --baseline bench/results/main.json --markdown bench/results/comment.md
```

Add scenario-id filters (`cargo xtask bench parse`) to iterate on one scenario, but run the full
set before asking for review. Exit 1 means one of our tools regressed past `max_regression`
and past `noise_sigmas` standard deviations of the two runs combined; the regressed scenario ids
are in `bench/results/<save>-regressions.txt`. CI then re-runs only those scenarios, PR first and
base second, and fails only if `cargo xtask bench --compare` confirms the regression.

## 4. Read the result

- `vs main` is the median change for each tool; only **bold** (ours) rows are gated.
- `vs fastest` compares against the best reference tool: that is the target in the plan
  (for example parser >= Oxc, start-up < 5 ms).
- A large `±` (spread) means the run is noisy: rerun before drawing conclusions.
- If a regression is real, profile it (`samply record target/release/<tool> ...`), fix it, rerun.

## 5. Report

Paste `bench/results/comment.md` into the PR description (`bench-actions.yml` also benches PRs
touching `crates/` on GitHub's runners and posts every runner's tables in the PR's bench comment). Say which scenarios moved and why.

Never edit `max_regression`, scenario commands or reference versions to make a PR pass; changes
to `bench/` are reviewed by a person like any other gate. Flip a tool from `pending` to active
in the same PR that ships the binary it runs.
