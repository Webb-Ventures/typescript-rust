# ts_r_lint_cli

The `ts-r-lint` binary: argument parsing, output and exit codes. All behaviour lives in
[../ts_r_lint](../ts_r_lint/AGENTS.md). Root rules: [../../AGENTS.md](../../AGENTS.md).

## Usage

```
ts-r-lint [paths] [--fix] [-f stylish|json|sarif|github] [-c config] [--max-warnings N]
          [--quiet] [--threads N] [--rule NAME] [--no-gitignore] [--no-warn-ignored]
          [--no-type-check]
```

## Invariants

- **Exit codes follow ESLint:** 0 = no errors and warnings within `--max-warnings`; 1 = lint
  errors or too many warnings; 2 = bad arguments or a fatal config problem.
- **stdout carries only the report** (so `-f json` can be piped). The unsupported-rules
  summary and notes go to stderr.
- **Threads default to `min(4, cores)`.** Keep benchmark comparisons at `--threads 4`.
- **Config evaluation is cached.** Benchmarks must report both the cached time and a cold
  time with `TS_R_LINT_NO_CONFIG_CACHE=1` (Node evaluates the configs, ~0.3 s), and use
  `--no-gitignore` when comparing with ESLint, which does not read `.gitignore`.
- **Type-aware rules are on by default** (`types_available = true`): files whose config
  enables them are linted per tsconfig project on our checker, guarded by the parity check
  against the project's own tsc (cached under `~/.cache/ts-r/lint/parity`).
  `--no-type-check` turns them off. Benchmarks report cold (parity not cached: tsc runs) and
  warm times separately; `TS_R_LINT_TIMING=1` prints per-project times on stderr.
- Keep this crate thin: new behaviour goes in `ts_r_lint` where it can be tested.
