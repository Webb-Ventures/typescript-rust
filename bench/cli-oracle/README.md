# Launcher smoke matrix for `ts-r`

The `ts-r` launcher contains no compiler code (crates/ts_r_cli/AGENTS.md): `ts-r <tool> args`
execs `ts-r-<tool> args` untouched. So the oracle for the launcher is the standalone binaries.
`run.mjs` builds a small fixture project (a tsconfig, a type error in `bad/`, an
`eslint.config.js`, TypeScript files that print, exit with a code, throw, have a syntax error)
and:

1. **passthrough**: for each tool (`run`, `dev`, `build`, `lint`, `check`, `lsp`) and each case
   (`--help`, `--version`, an unknown flag, and real runs: `ts-r-run` on files that print, take
   arguments, exit 3, throw, fail to parse, don't exist, read the environment; `ts-r-check` on
   a clean and a failing project and a missing one; `ts-r-build` to stdout and to `--outfile`,
   on a syntax error and a missing entry; `ts-r-lint` on clean and failing files and a missing
   path) it runs `ts-r <tool> args` and `ts-r-<tool> args` in the same directory with the same
   environment and compares exit code, stdout and stderr (the build directory, the fixture
   path and timings are normalized);
2. **launcher**: checks the launcher's own commands against its AGENTS.md: `--version`/`-V`,
   `--help` listing every tool, no arguments, unknown command and flag (exit 2), `doctor`
   finding every tool, `ci`'s exit code against check/lint/build run alone, a missing tool
   (exit 127 naming its milestone), `ci` with a missing tool, a `.ts-r-version` pin of another
   version, `TS_R_VERSION` overriding the pin, a malformed pin (exit 2).

dev and lsp only get flag cases: no servers are started. Cases run one at a time with a 60 s
timeout each, with an isolated `HOME` and `TS_R_CACHE_DIR` in a temp directory that is
deleted afterwards (`--keep` keeps it).

```sh
cargo build --release -j 4 -p ts_r_cli -p ts_r_run -p ts_r_dev -p ts_r_build \
  -p ts_r_check -p ts_r_lint_cli -p ts_r_lsp
scripts/limit.sh --mem-mb 2048 --time-s 600 -- node bench/cli-oracle/run.mjs \
  [--bin target/release] [--out target/cli-oracle/results.json]
```

Nothing is skipped. Not covered: Windows (the launcher spawns there instead of exec-ing),
signals and TTY passthrough, `ts-r lsp`/`ts-r dev` as long-running servers.

## Results

8c14f29 (2026-10-24, macOS arm64, Node 24.21.0, release builds): **37 of 37 passthrough cases
and 13 of 13 launcher cases pass.** No launcher fixes were needed.
