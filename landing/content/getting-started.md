---
title: Getting started
description: Install the ts-r tools from npm, or build them from source, and try each one next to tsc, ESLint and Vite.
nav: Getting started
section: Start
order: 1
badges: ["warn:Alpha"]
---

## Install from npm

ts-r is in alpha. Releases are published on the npm dist-tag `next`, with native binaries for
macOS (arm64, x64), Linux (arm64, x64; glibc and musl) and Windows (arm64, x64). Install the
`ts-r` launcher (`@ts-r.dev/cli`) with every tool, or one tool on its own:

```sh
npm install --save-dev @ts-r.dev/cli@next      # every tool and the ts-r launcher
npm install --save-dev @ts-r.dev/check@next    # just ts-r-check (or run, dev, build, lint, lsp)
```

npm installs only the binary for your platform. `@ts-r.dev/check` and `@ts-r.dev/lsp` carry
TypeScript's lib files, so `ts-r-check` works as installed. Then check what the launcher sees:

```sh
npx ts-r doctor
```

It prints where each tool was found, the cache directory and its size, and the nearest
`tsconfig.json` and `package.json`.

Each release also has archives of every binary, with `SHA256SUMS`, on the
[GitHub releases page](https://github.com/Webb-Ventures/typescript-rust/releases).

## Build from source

Building from source takes one `cargo build`.

### Prerequisites

- **Rust.** The repo pins its toolchain in `rust-toolchain.toml` (Rust 1.93.1). With
  [rustup](https://rustup.rs) installed, `cargo` picks it up automatically.
- **Node.js 22.15 or later**, for `ts-r-run` (it runs your code on the Node you have),
  for `ts-r-lint` when your ESLint config is a JavaScript file (Node evaluates it once and
  the result is cached), and for `ts-r-lsp` when it runs your project's own TypeScript.
- **git**.

### Build

```sh
git clone https://github.com/Webb-Ventures/typescript-rust
cd typescript-rust
cargo build --release
```

On a machine that's busy with other work, `cargo build --release -j 4` keeps the build to four
jobs.

The binaries land in `target/release/`:

| Binary | What it is |
| --- | --- |
| `ts-r` | The launcher: `ts-r dev` runs `ts-r-dev`, and so on |
| `ts-r-dev` | Dev server with hot reload |
| `ts-r-build` | Bundler with esbuild's command line |
| `ts-r-check` | Type checker |
| `ts-r-lint` | Linter |
| `ts-r-run` | Runs a TypeScript file on Node |
| `ts-r-lsp` | Language server for editors |

Put them on your `PATH`:

```sh
export PATH="$PWD/target/release:$PATH"
```

Then check what the launcher sees with `ts-r doctor`.

### Lib files for ts-r-check

`ts-r-check` needs TypeScript's lib files (`lib.es5.d.ts` and friends). They aren't in the
repo. Fetch the pinned version once:

```sh
cargo xtask upstream sync --minimal
```

Or point `TS_R_LIB_DIR` at a directory that has them. `ts-r-check` looks in `TS_R_LIB_DIR`,
then next to its own binary, then in the synced copy under `target/upstream/`. Details are
in [the ts-r-check notes](../../crates/ts_r_check/AGENTS.md).

## A first command for each tool

Each tool reads the config you already have. These use only flags the tools accept today
(with a tool's own package installed, run it through `npx`, for example `npx ts-r-run src/main.ts`):

```sh
ts-r-run src/main.ts                     # instead of tsx src/main.ts
ts-r-run --watch src/server.ts           # restart when a file of the module graph changes
ts-r-dev .                               # dev server on http://127.0.0.1:5173/
ts-r-build src/index.ts --bundle --outfile=dist/index.js --sourcemap
ts-r-check -p tsconfig.json              # type check, tsc's output format
ts-r-lint src                            # lint with your eslint.config.*, .oxlintrc.json or biome.json
ts-r-lsp status                          # the editor daemon for this workspace, as JSON
```

Through the launcher (the `@ts-r.dev/cli` package) the same commands are `ts-r run`, `ts-r dev`,
`ts-r build`, `ts-r check`, `ts-r lint` and `ts-r lsp`; it passes the arguments on unchanged. `ts-r ci` runs check, lint and
build in parallel.

Each tool has its own page with what works today, its flags, and how it fits next to the tool it
replaces:

- [ts-r-dev](tools/ts-r-dev.md): dev server, instead of `vite dev`
- [ts-r-build](tools/ts-r-build.md): bundler, instead of esbuild or `vite build`
- [ts-r-check](tools/ts-r-check.md): type checker, instead of `tsc --noEmit`
- [ts-r-lint](tools/ts-r-lint.md): linter, instead of ESLint or oxlint
- [ts-r-run](tools/ts-r-run.md): runner, instead of tsx or ts-node
- [ts-r-lsp](tools/ts-r-lsp.md): language server, instead of tsserver and the ESLint server

## The editor extension

The VS Code and Cursor extension lives in `editors/vscode/`. It isn't packaged as a `.vsix`
yet and hasn't been tested inside a running VS Code, so there is no one-click install. What it
does and how it is tested are in [its notes](../../editors/vscode/AGENTS.md). Editors that
speak LSP can run `ts-r-lsp --stdio` directly.

## Use one tool at a time

No ts-r tool needs another ts-r tool. Pick the one that hurts most today and run it next to
the rest of your setup:

- Keep `tsc` in CI and try `ts-r-run` for scripts.
- Keep ESLint in the editor and try `ts-r-lint` in a pre-commit hook.
- Keep `vite build` and try `ts-r-dev` for local development.

Going back means deleting one line. Before you rely on a tool, check
[what works today](status.md) and the [measured numbers](/benchmarks/).
