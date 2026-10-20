# ts-r

Fast TypeScript tooling in Rust: `ts-r-dev`, `ts-r-build`, `ts-r-check`, `ts-r-lint`,
`ts-r-run` and `ts-r-lsp`, plus a tiny `ts-r` launcher. Each tool is a separate binary that
works on its own next to tsc, ESLint and Vite, and gives its full speed and memory win alone.
The plan lives in `.lavish/toolchain-plan.html`; design records live in `docs/design/`.

## The from-scratch rule

Every layer is written here: lexer, parser, AST, binder, checker, transformer, codegen,
minifier, resolver, bundler, linter, dev server, language server.

- Do not depend on another compiler, bundler or linter (no swc, oxc, biome, tree-sitter,
  rolldown, esbuild, typescript, ts-rust/tsc-rs crates or WASM builds).
- Do not copy or translate source code from tsc, ts-rust, esbuild or any other tool. Work from
  our design records and from observed behaviour. Their *test suites* and *outputs* are oracles:
  run them, compare against them, but don't port their implementation.
- Small general-purpose crates are fine when they save real work and are well maintained
  (for example `rustc-hash`, `memchr`, `smallvec`, `bumpalo`, `rkyv`, `memmap2`, `blake3`,
  `serde`/`serde_json`, `notify`, `rayon`, `crossbeam`). Say why in the crate's AGENTS.md.

## Layout

| Path | What it is |
|---|---|
| `crates/ts_r_span` | Source text, byte spans, line/column mapping |
| `crates/ts_r_diagnostics` | Diagnostic codes, messages, tsc-style rendering |
| `crates/ts_r_ast` | Compact index-based AST shared by every tool |
| `crates/ts_r_lexer` | Lexer for TS, TSX and JS |
| `crates/ts_r_parser` | Error-recovering parser |
| `crates/ts_r_config` | tsconfig.json: JSONC, `extends`, compiler options |
| `crates/ts_r_resolver` | Node and TS module resolution |
| `crates/ts_r_cache` | Content-addressed on-disk cache in `~/.cache/ts-r` |
| `crates/ts_r_emit` | Program emit for ts-r-check: output paths, source maps, safe writes, checker-backed EmitResolver |
| `crates/ts_r_cli` | The `ts-r` launcher (no compiler code) |
| `crates/ts_r_hmr` | Dev server core: HTTP/WebSocket, module graph, HMR, Vite-compatible `import.meta.hot` |
| `crates/ts_r_dev` | The `ts-r-dev` binary |
| `crates/ts_r_lsp` | Language server and the shared editor daemon (`ts-r-lsp`) |
| `editors/vscode` | VS Code extension |
| `xtask` | Repo tooling: `cargo xtask <cmd>` (upstream sync, agents-check, bench, codegen, ecosystem) |
| `ecosystem/` | Top-npm-packages harness (M7): pinned list, STATUS.md, repros ([ecosystem/README.md](ecosystem/README.md)) |
| `upstream/UPSTREAM.json` | Pinned TypeScript / esbuild / Vite / typescript-eslint / test262 versions |
| `docs/design/` | Design records. Agents must follow them; people review changes to them |
| `landing/` | The website, ts-r.dev: one Next.js static export with the home, tool and sponsor pages and, once launched, the docs (sources in `landing/content/`), error-code pages and benchmarks (`landing/AGENTS.md`). Only in the development repo, not the public one |
| `.public/` | Files only the public repo has (its release, sync-back and AI-policy workflows), laid over its root by the sync (`.github/public-sync/README.md`, development repo only) |
| `.agents/skills/` | Task playbooks (SKILL.md) for recurring agent work (`.claude/skills` links here); `.agents/plugins/` holds the code-review plugin |

More crates arrive per the roadmap (binder, checker, transform, codegen, bundler, lint).

## Working in this repo

- Build and test: `cargo test --workspace`. Lint: `cargo clippy --workspace --all-targets -- -D warnings`.
  Format: `cargo fmt --all`.
- Before finishing: `cargo xtask agents-check` (every crate has an AGENTS.md, links resolve).
- When you create a crate, write its `Cargo.toml` and its `src/lib.rs` or `src/main.rs` in one step.
  The workspace uses `crates/*`, so a manifest without a target breaks `cargo` for every other agent.
- Stay inside the crates you own for a task. Add dependencies in your crate's own
  `Cargo.toml`, not the root one.
- Every crate has an `AGENTS.md` with its invariants. Read it before editing the crate, and
  update it in the same change when an invariant changes.

## Branches and commits

ts-r is developed in a private repo (Webb-Ventures/tsc-rust) and published to the public one
(github.com/Webb-Ventures/typescript-rust): every push to the private main opens a sync PR there,
and a PR merged in the public repo is carried back as a PR in the private one. Outside
contributors work in the public repo: see CONTRIBUTING.md and AI_POLICY.md. The rest of this
section is how the core team works in the private repo.

Work lands as a stack of branches on `origin`, one per track, in dependency order. `main` only
moves when a person merges a reviewed branch.

- Branch names: `<milestone>/<track>`, for example `m1/front-end`, `m5/binder`, `l1/editor`.
  Each branch builds on the branch it depends on. The PRs form a GitHub stack into `main`, each
  based on the one below it. Stacks only grow at the top, so the top branch of the stack is the
  integration branch: a finished track is merged (`--no-ff`) into it, pushed as the track's branch
  (a fast-forward), and its PR is appended to the stack (`gh stack link <stack> <branch>`). Start
  new work from the top branch (today `main`: the first M8 stack is merged). CI (ci.yml,
  bench.yml, pages.yml) runs only on the top PR, and skips jobs whose code already passed.
- Commit at every milestone: when a coherent piece works and `cargo test`, `cargo clippy -D warnings`
  and `cargo xtask agents-check` pass for your crates. Small, reviewable commits beat one huge one.
  Commit messages say what works and the measured results (speed, oracle match counts).
- Commit only the paths you own. Never commit `target/`, benchmark results, caches or binaries.
  `node_modules` inside `crates/*/tests/fixtures/` is committed on purpose.
- If you were started in your own git worktree, commit and push your branch there
  (`git push -u origin <branch>`). If you share the main working tree with other agents, don't run
  git commands that change HEAD, the index or files (`checkout`, `stash`, `reset`, `add`, `commit`):
  the coordinator commits your work onto its branch.
- Never push to `main`, never force-push a branch you didn't create, never rewrite pushed history.

## Resource limits (this machine is shared: 48 GB RAM, 16 cores, several agents at once)

A runaway fuzz test once used 180 GB of memory and nearly crashed the machine. These rules are not optional.

- Run anything that can loop or grow without bound (fuzzing, mutation tests, corpus runs, benchmarks,
  oracle runs, `npx`/`node`/`tsc` on real projects, long-running servers) through
  `scripts/limit.sh [--mem-mb N] [--time-s N] -- <cmd>`. Defaults: 4096 MB for the whole process tree,
  600 s; what the command leaves running in its process group is killed when it exits. Never raise the cap above 8192 MB, and never above 4096 MB for more than one command at a time.
- The one exception: above 8192 MB only through `cargo xtask bench --mem-mb N --dedicated-machine`,
  and only on a machine nobody else is using (bench/README.md, "Large repos: a dedicated machine").
- Rust test, example and harness binaries that run generated or real-world input must call
  `ts_r_testing::cap_memory!()` (aborts at 4096 MB, or `TS_R_MEM_CAP_MB`). Add `ts_r_testing` as a
  dev-dependency.
- Fuzzing: bounded iterations (a fixed count or seconds, never `loop {}` without a limit), bounded input
  size, and a per-input time limit. Never leave a fuzzer running in the background.
- Every loop in the lexer, parser, binder and checker must consume input or bail out. Recursion has a
  depth limit that produces a diagnostic, not a stack overflow. Pathological inputs (deep nesting, huge
  files, lone `#`, unterminated constructs) get regression tests.
- Use at most 4 threads for corpus and benchmark runs (`RAYON_NUM_THREADS=4`, `--test-threads=4`) and
  `cargo build -j 4` when other agents are building.
- Delete large temporary outputs when finished.

## Code rules

- Edition 2024. `unsafe` is denied workspace-wide. A crate that needs it (SIMD, mmap) allows it
  locally with a `// SAFETY:` comment on each block and a note in its AGENTS.md.
- Never panic on user input. Malformed source, config or JSON produces diagnostics.
- Output is deterministic: no `HashMap` iteration order in output, no timestamps or absolute
  paths in cached data.
- Performance is a feature: index-based data (`u32` ids into arrays) over `Rc`/`Box` graphs,
  interned strings, no per-node heap allocation in hot paths, no `clone` of large structures.
  Benchmarks gate merges.
- Platform-specific code goes in a module named for its platform: `windows.rs`, `unix.rs`,
  `macos.rs` or `linux.rs` (or a folder of that name), declared under the matching `#[cfg(..)]`.
  CI tests a change to such a file only on that platform, and `agents-check` fails if the module
  isn't gated. A `#[cfg(windows)]` block inside a shared file still runs CI on every platform,
  and so must anything that reaches into a platform folder with `#[path]`.
- Positions are UTF-8 byte offsets (`u32`). Convert to UTF-16 or line/column only at the edges
  (LSP, diagnostics output).
- Tests: unit tests next to code, fixture tests under `crates/<crate>/tests/`. Snapshot files are
  plain text so diffs are readable.
