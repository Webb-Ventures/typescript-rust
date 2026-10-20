# ts_r_cli

The `ts-r` launcher binary (`[[bin]] name = "ts-r"`). It finds and runs the `ts-r-<tool>`
binaries and contains **no compiler code**. Root rules: [../../AGENTS.md](../../AGENTS.md).
Design record: [0009](../../docs/design/0009-cache-and-launcher.md).

## Commands

| Command | Does |
|---|---|
| `ts-r run\|dev\|build\|lint\|check\|lsp [args]` | exec `ts-r-<tool>` with args untouched |
| `ts-r ci [--skip-missing]` | check, lint, build in parallel; prefixed lines; timing summary |
| `ts-r doctor` | where each tool is found, cache dir/size/namespaces, nearest tsconfig/package.json |
| `ts-r --version` / `--help` | launcher version / usage |

Exit codes: tool's own (exec), 127 tool not installed, 126 exec failed, 2 usage or bad pin,
`ci`: 0 if every tool passed, else 1.

## Invariants

- **Startup budget < 5 ms.** Hand-rolled arg parsing (no clap), no filesystem access before the
  command needs it (`--version`/`--help` touch nothing). Measured on an M-series Mac (release,
  500 runs, via Python `subprocess` whose own spawn of `/usr/bin/true` is 1.6 ms median):
  `ts-r --version` 2.1 ms, `ts-r run` exec-ing a sibling `true` 3.2 ms → launcher overhead
  ≈ 0.5 ms. Binary 0.7 MB. (hyperfine was not installed.)
- **Unix uses `exec`**, so signals, stdio, TTY and exit code belong to the tool. Elsewhere it
  spawns, waits and forwards the exit code.
- **Lookup order** (own version): launcher's directory (and its symlink-resolved directory),
  `<cache>/bin/<version>/`, `PATH`. A **pinned other version** only looks in
  `<cache>/bin/<pinned>/`, because binaries elsewhere have an unknown version.
- **Version selection**: `TS_R_VERSION` > nearest `.ts-r-version` walking up from cwd
  (first non-comment line, optional leading `v`, `[0-9][A-Za-z0-9.+_-]*`) > launcher version.
- **Missing tools say which milestone ships them** (table in `src/tools.rs`; update it when a
  tool ships or a milestone moves).
- `ci` never interleaves partial lines (whole lines under the stdout/stderr lock), keeps
  stderr on stderr, passes bytes through unchanged, sets `TS_R_THREADS = cores / tools` for
  each tool, and forces colour (`FORCE_COLOR`, `CLICOLOR_FORCE`) only when it is itself on a
  TTY and `NO_COLOR` is unset. If a tool is missing it runs nothing unless `--skip-missing`.
- Cache location logic is **not duplicated**: it comes from `ts_r_cache::cache_root()`.
  (`npm/ts-r/bin/ts-r.js` mirrors it in JS for its fallback; keep them in sync.)

## Dependencies

- `ts_r_cache` (path): cache root, `bin_dir`, read-only `inspect` for doctor. No external crates.

## Testing

- `cargo test -p ts_r_cli`: unit tests (version parsing/selection, lookup order, messages);
  `tests/cli.rs` (Unix) copies the built binary into a sandbox with fake shell-script tools and
  an isolated `TS_R_CACHE_DIR`/`PATH`, covering arg/exit-code passthrough, search order,
  pinning, `ci` output and failure modes, doctor, exec failure.
- Manual: `cargo run -p ts_r_cli -- doctor`.

## Gaps

- No download/install of tool binaries into `<cache>/bin/<version>/` yet (the npm shim gets the
  launcher; tools must be placed there or shipped next to it). The plan's "fetches only the
  tool you call" is not implemented.
- `ts-r ci` doesn't yet run tsc 7 alongside `check` for the parity report the plan describes.
- No `ts-r cache gc|clean` subcommand; doctor only reports.
- Windows paths are compiled but untested (no exec; `.exe` suffix handling only).
