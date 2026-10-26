# ts-r

[![CI](https://github.com/Webb-Ventures/typescript-rust/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Webb-Ventures/typescript-rust/actions/workflows/ci.yml)
[![npm](https://img.shields.io/npm/v/@ts-r.dev/cli/next?label=npm&color=cb3837)](https://www.npmjs.com/package/@ts-r.dev/cli)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](#license)
[![Sponsor](https://img.shields.io/badge/sponsor-ts--r-f0814a?logo=githubsponsors&logoColor=white)](https://ts-r.dev/donate/)

**Fast TypeScript tooling, written from scratch in Rust.** ts-r type-checks, lints, bundles,
runs and serves TypeScript, aiming for the same results as the tools you use today, and faster:
ts-r-check type-checks tRPC, date-fns and RxJS 2.0–5.4× faster than tsc 7 ([benchmarks](#benchmarks)).
Each tool is its own small binary and works on its own next to tsc, ESLint and Vite, so
you can adopt one without the rest.

```sh
npm install --save-dev @ts-r.dev/cli@next   # the ts-r launcher and every tool
npx ts-r check           # type check, with tsc 7's diagnostics
npx ts-r lint            # lint, with ESLint and typescript-eslint rules
npx ts-r build           # bundle, with esbuild's CLI
npx ts-r run server.ts   # run TypeScript (replaces tsx / ts-node)
npx ts-r dev             # dev server with HMR and Vite's import.meta.hot
```

> **Status: preview.** The type checker is the most mature tool. It gives tsc 7's exact output on
> 97.0% of TypeScript's own conformance tests. It also matches tsc 7 exactly on tRPC, date-fns,
> TypeORM and Playwright, and is within a few diagnostics on RxJS (none extra, 5 missing, of
> about 21,900). Every other tool works on real projects
> but has known gaps (below). Version numbers stay below 1.0 until that changes.

## Contents

- [The tools](#the-tools)
- [Install](#install)
- [Benchmarks](#benchmarks)
- [How it works](#how-it-works)
- [Build from source](#build-from-source)
- [Contributing](#contributing)
- [Sponsors](#sponsors)
- [License](#license)

## The tools

| Tool | Replaces | Works today | Known gaps |
|---|---|---|---|
| `ts-r-check` | `tsc` (type checking, emit, `--build`, `--watch`) | tsc 7's codes, messages and chains; JavaScript, declaration and source-map emit; project references | Some inference and narrowing; 3.0% of conformance tests still differ |
| `ts-r-lint` | ESLint, oxlint | 158 rules, 51 of them type-aware on ts-r's own checker; 15,746 of 15,766 cases of ESLint's, typescript-eslint's and eslint-plugin-jsdoc's own tests pass; flat config, oxlint and Biome configs | Type-aware rules only where ts-r-check matches your tsc; plugins other than typescript-eslint (and three eslint-plugin-jsdoc rules) |
| `ts-r-build` | esbuild, `vite build` | esbuild's CLI flags; ESM, CJS and IIFE output; code splitting; minification; source maps; `--watch` | No plugins, no CSS bundling, lowering down to ES2015 (not ES5) |
| `ts-r-run` | tsx, ts-node | Runs TypeScript on Node 22.15+ with tsc 7's JavaScript output | Only the entry's tsconfig is used |
| `ts-r-dev` | `vite dev` | Hot reload, Vite's `import.meta.hot` API, React Fast Refresh, dependency pre-bundling, `vite.config` and Vite plugins | No CSS preprocessors; some `vite.config` options and plugin APIs; HTTP/1.1 only |
| `ts-r-lsp` | tsserver, the ESLint server | One server per workspace for lint and types; our checker or your own TypeScript as the type backend | Most projects still fall back to tsserver for diagnostics |
| `ts-r` | | Launcher for all of the above, plus `ts-r ci` (check, lint and build in parallel) and `ts-r doctor` | |

What each tool matches, and how it is measured: each crate's `AGENTS.md` and the design records
in [docs/design/](docs/design/README.md).

## Install

Every tool is an npm package with a native binary for macOS (arm64, x64), Linux (arm64, x64;
glibc and musl) and Windows (arm64, x64). Install one, or the `ts-r` launcher (`@ts-r.dev/cli`) with all of them:

```sh
npm install --save-dev @ts-r.dev/cli@next      # every tool and the ts-r launcher
npm install --save-dev @ts-r.dev/check@next    # just ts-r-check (or run, dev, build, lint, lsp)
```

To type-check with ts-r wherever your scripts and tools already run `tsc`, use the drop-in
`typescript` package. It runs ts-r-check where ts-r-check gives the same result as your own
TypeScript on that project, and hands everything else to your TypeScript:

```jsonc
// package.json
"devDependencies": {
  "typescript": "npm:@ts-r.dev/typescript@next",
  "typescript-real": "npm:typescript@^7.0.0"
}
```

During the alpha, use `@next` or an exact version (`@7.0.1000-alpha.3`): a range like `^7`
doesn't match prerelease versions.

The details are in [npm/typescript/README.md](npm/typescript/README.md). Packages and platforms
are in [npm/ts-r/README.md](npm/ts-r/README.md).

## Benchmarks

Each table shows ts-r's median time per scenario next to the fastest other tool on the same
workload. Workloads include type-checking pinned open-source repos (tRPC, date-fns, rxjs) and
bundling three.js ten times over, among others. Pull requests that change the crates or the
benchmarks are benchmarked against their base: maintainers' PRs on four dedicated cores of a
bench machine, fork PRs on GitHub's shared Linux runners. A slowdown beyond the gate fails the
bench check, which reviewers read before merging. Method and scenarios:
[bench/README.md](bench/README.md).

### Unreleased (main)

<!-- bench:main:start -->
| Benchmark | ts-r | Fastest other tool | ts-r is |
|---|---:|---:|---:|
| `version-startup` | `ts-r` 2.03 ms | `esbuild` 3.93 ms | **1.9× faster** |
| `check-trpc` | `ts-r-check` 474 ms | `tsc-7` 979 ms | **2.1× faster** |
| `check-date-fns` | `ts-r-check` 998 ms | `tsc-repo` 1.97 s | **2.0× faster** |
| `check-rxjs` | `ts-r-check` 1.20 s | `tsc-7` 6.51 s | **5.4× faster** |
| `build-three-x10` | `ts-r-build` 963 ms | `esbuild` 1.41 s | **1.5× faster** |

Medians on linux-x86_64 (four dedicated cores), measured 2026-10-24. Scenarios: [bench/scenarios.toml](bench/scenarios.toml) and [bench/repos.toml](bench/repos.toml).
<!-- bench:main:end -->

The table above is written from one review-box bench of a pull request
(`node scripts/readme-bench.mjs main`): this one is #42's, at `d8a6a97` (merged as `8b6c630`),
whose code differs from main's only in `ts_r_run`, which none of these scenarios runs. The one
below is the same table at the time of the latest release, and is updated by each release pull
request.

### Latest release

<!-- bench:release:start -->
Release **v0.1.0-alpha.4**:

| Benchmark | ts-r | Fastest other tool | ts-r is |
|---|---:|---:|---:|
| `version-startup` | `ts-r` 2.03 ms | `esbuild` 3.93 ms | **1.9× faster** |
| `check-trpc` | `ts-r-check` 474 ms | `tsc-7` 979 ms | **2.1× faster** |
| `check-date-fns` | `ts-r-check` 998 ms | `tsc-repo` 1.97 s | **2.0× faster** |
| `check-rxjs` | `ts-r-check` 1.20 s | `tsc-7` 6.51 s | **5.4× faster** |
| `build-three-x10` | `ts-r-build` 963 ms | `esbuild` 1.41 s | **1.5× faster** |

Medians on linux-x86_64 (four dedicated cores), measured 2026-10-24. Scenarios: [bench/scenarios.toml](bench/scenarios.toml) and [bench/repos.toml](bench/repos.toml).
<!-- bench:release:end -->

## How it works

Every layer is written in this repo, with no other compiler, bundler or linter underneath:
lexer, parser, binder, type checker, transformer, code generator, minifier, module resolver,
bundler, linter, dev server and language server. TypeScript's, esbuild's and ESLint's test suites
and outputs are the oracles: ts-r runs them and compares, and does not copy their code.

| Path | What it is |
|---|---|
| `crates/ts_r_lexer`, `ts_r_parser`, `ts_r_ast` | Error-recovering parser for TS, TSX and JS into a compact, index-based AST |
| `crates/ts_r_binder`, `ts_r_checker` | Scopes and symbols; the type checker |
| `crates/ts_r_transform`, `ts_r_codegen`, `ts_r_emit` | TypeScript to JavaScript, printing, source maps and declaration files |
| `crates/ts_r_resolver`, `ts_r_config` | Node and TypeScript module resolution; tsconfig.json |
| `crates/ts_r_bundler`, `ts_r_minify` | The bundler and minifier behind ts-r-build and ts-r-dev |
| `crates/ts_r_lint` | Lint rules and config loading |
| `crates/ts_r_lsp`, `editors/vscode` | Language server and the VS Code extension |
| `docs/design/` | Design records: why each piece works the way it does |

[AGENTS.md](AGENTS.md) has the full crate map and the rules every change follows.

## Build from source

You need the Rust toolchain pinned in `rust-toolchain.toml` (rustup installs it) and Node 22 or
newer for the tests that compare against tsc and esbuild.

```sh
git clone https://github.com/Webb-Ventures/typescript-rust
cd typescript-rust
cargo xtask upstream sync --minimal   # TypeScript's lib files, which the checker tests need
cargo build --release                 # binaries in target/release
cargo test --workspace
```

`cargo xtask` also runs the oracles (`cargo xtask oracle`), the benchmarks (`cargo xtask bench`)
and the top-npm-packages harness (`cargo xtask ecosystem`). See [xtask/AGENTS.md](xtask/AGENTS.md).

## Contributing

Contributions are welcome: bug reports, mismatches with tsc or ESLint, and pull requests.

- [CONTRIBUTING.md](CONTRIBUTING.md): how to set up, the checks a change must pass, and how
  pull requests are reviewed and merged.
- [AI_POLICY.md](AI_POLICY.md): AI-assisted work is welcome from Anthropic's, OpenAI's and Meta's
  Muse models only. Pull requests that credit any other model are closed automatically.
- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) and [SECURITY.md](SECURITY.md) (report vulnerabilities
  privately).
- [CHANGELOG.md](CHANGELOG.md): what changed in each release.

## Sponsors

ts-r is free and open source, and its development is funded by sponsors.

<table>
  <tr>
    <td align="center" width="280">
      <a href="https://webbventuresgroup.com"><strong>Webb Ventures</strong></a><br>
      <sub>Founding sponsor</sub>
    </td>
  </tr>
</table>

**Sponsor ts-r:** if ts-r saves your team time, [support its development](https://ts-r.dev/donate/).
Sponsors are listed here.

## License

ts-r is MIT licensed ([LICENSE](LICENSE)). Unless you say otherwise, any contribution you submit
for inclusion in ts-r is licensed under MIT, with no additional terms or conditions.

TypeScript's lib files shipped with `ts-r-check` and `ts-r-lsp` are under TypeScript's own
Apache-2.0 license, included next to them. Upstream test data fetched by
`cargo xtask upstream sync` stays in the gitignored `target/` directory under its own license.
