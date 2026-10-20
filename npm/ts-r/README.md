# ts-r

Fast TypeScript tooling written in Rust, with nothing to install:

```sh
npx ts-r run server.ts   # run TypeScript (replaces tsx / ts-node)
npx ts-r dev             # dev server with HMR
npx ts-r build           # production bundle
npx ts-r check           # type check
npx ts-r lint            # lint
npx ts-r ci              # check + lint + build in parallel
npx ts-r doctor          # which tools are installed, cache location, detected tsconfig
```

Each command is a separate binary (`ts-r-run`, `ts-r-dev`, ...) that works on its own next to
tsc, ESLint and Vite. Pin a version per repo with a `.ts-r-version` file.

## One tool, or all of them

| Package | Binary | Replaces |
|---|---|---|
| `ts-r` | `ts-r` launcher, plus every tool below | |
| `@ts-r.dev/run` | `ts-r-run` | tsx, ts-node |
| `@ts-r.dev/dev` | `ts-r-dev` | vite dev |
| `@ts-r.dev/build` | `ts-r-build` | esbuild, vite build |
| `@ts-r.dev/check` | `ts-r-check` | tsc (type checking) |
| `@ts-r.dev/lint` | `ts-r-lint` | ESLint, oxlint |
| `@ts-r.dev/lsp` | `ts-r-lsp` | tsserver, the ESLint server |
| `@ts-r.dev/typescript` | `tsc` | the `typescript` package (see its README) |

Installing `@ts-r.dev/lint` gets you `ts-r-lint` and nothing else: no bundler, no checker.

## How the packages work

Every package's `bin` is the same tiny Node shim (no dependencies, no install scripts). The native
binaries ship in per-platform packages listed as `optionalDependencies`
(`@ts-r.dev/lint-darwin-arm64`, `@ts-r.dev/lint-linux-x64-gnu`, `@ts-r.dev/lint-win32-x64-msvc`, ...); npm
installs only the one matching your OS, CPU and libc. The shim finds it and runs it with
inherited stdio, forwarding the exit code. In the `ts-r` package the shim also puts the installed
tools' binaries first on `PATH`, so `ts-r lint` runs `ts-r-lint` directly.

Supported: macOS (arm64, x64), Linux (arm64, x64; glibc and musl), Windows (arm64, x64).

Overrides: `TS_R_BIN_DIR=<dir>` runs the binaries in `<dir>` (for example a local
`target/release`); `TS_R_BINARY=<path>` replaces just the launcher.

## Errors in CI and editors

`ts-r check` prints tsc's format, plus sound-mode errors with `TSR` codes
(`src/a.ts(12,7): error TSR0201: ...`). Problem matchers that only know `error TS\d+` miss
those, so ts-r ships its own:

- GitHub Actions: `echo "::add-matcher::node_modules/ts-r/problem-matcher.json"` before the
  check (`node_modules/@ts-r.dev/check/problem-matcher.json` with just that package) turns
  `TS` and `TSR` errors into annotations.
- VS Code tasks: the ts-r extension contributes the `$ts-r` matcher
  (`"problemMatcher": "$ts-r"`), the same pattern as `$tsc` with `TSR` codes added.
