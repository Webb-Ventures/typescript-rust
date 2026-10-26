---
title: "`ts-r-lint`: linter"
description: A linter with 292 ESLint, typescript-eslint, jsdoc and unicorn rules, 51 of them type-aware, that reads eslint.config, .oxlintrc.json or biome.json.
nav: ts-r-lint
section: Tools
order: 4
badges: ["warn:Alpha", "ok:On npm: @ts-r.dev/lint"]
---

`ts-r-lint` runs ESLint-compatible rules over JavaScript and TypeScript. It reads the config
you already have, reports in ESLint's formats and exits with ESLint's codes, so it can stand in
for `eslint` in scripts and CI when your config only uses the rules it has.

## What works today

- 292 rules: all 64 rules of ESLint 10's `recommended` set and 70 more core rules, the
  typescript-eslint 8.71 `recommended` and `strict` rules and a few more, 40
  eslint-plugin-jsdoc rules and 23 eslint-plugin-unicorn rules. 51 of them are type-aware
  and get their types from ts-r's own checker. `ts-r-lint --list-rules` prints them.
- The published ESLint, typescript-eslint, eslint-plugin-jsdoc and eslint-plugin-unicorn tests for
  those rules: 25,042 of 25,060 cases match (18,140 of 18,140 for rules without types, 6,902 of
  6,920 for type-aware rules).
- Configs: ESLint flat config, `.oxlintrc.json` and `biome.json` (see below).
- `--fix` for rules with fixes.
- `eslint-disable` comments and `/* eslint */` inline config, with reports of unused disable
  comments.
- `.gitignore` is honoured unless you pass `--no-gitignore`.
- Output formats `stylish`, `json`, `sarif` and `github` (GitHub Actions annotations).

## Not yet

- Type-aware rules run only on a tsconfig project where ts-r-check reports the same
  diagnostics as your tsc. Elsewhere they are skipped, and listed on stderr with the reason.
- No ESLint plugins other than typescript-eslint, apart from 40 eslint-plugin-jsdoc and 23
  eslint-plugin-unicorn rules
  (`ts-r-lint --list-rules` names them). Rules from other plugins are skipped and
  listed as "plugin not implemented".

Design record: [0006 lint](../../../docs/design/0006-lint.md).

## Command line

```text
ts-r-lint [options] [paths...]

Lints JavaScript and TypeScript files with ESLint-compatible rules and configs
(eslint.config.*, .oxlintrc.json, biome.json).

Options:
  --fix                         Apply fixes and write files
  -f, --format <name>           stylish (default), json, sarif, github
  -c, --config <path>           Use this config for every file
  --max-warnings <n>            Exit 1 when there are more than n warnings
  --quiet                       Report errors only (warn rules are not run)
  -o, --output-file <path>      Write the report to a file
  --threads <n>                 Worker threads (default: min(4, cores))
  --rule <name>                 Only run this rule (repeatable)
  --no-type-check               Do not run type-aware rules (no tsconfig program)
  --no-gitignore                Do not honour .gitignore files
  --no-warn-ignored             Do not warn about ignored files named explicitly
  --no-inline-config            Ignore eslint-disable and /* eslint */ comments
  --report-unused-disable-directives
                                Report unused eslint-disable comments as errors
  --report-unused-disable-directives-severity <sev>
                                off, warn or error
  --list-rules                  Print the built-in rules and exit
  -h, --help                    Show this help
  -v, --version                 Show the version
```

- `--quiet` doesn't run `warn` rules at all, rather than running them and hiding the result.
- `--rule` narrows a run to one or more rules, which helps when you compare against ESLint.
- Only the report goes to stdout (or to `--output-file`), so `-f json` can be piped. Notes go
  to stderr.
- ESLint doesn't read `.gitignore`. Use `--no-gitignore` when you compare the two.

Exit codes match ESLint's: 0 when there are no errors (and warnings are within
`--max-warnings`), 1 when there are errors or too many warnings, 2 for a config or usage error.

You can also run it through the launcher as `ts-r lint`.

## Config

For each file, ts-r-lint looks at or above the file's directory, in this order:

| Order | Config | How it is read |
|---|---|---|
| 1 | Nearest `eslint.config.{js,mjs,cjs,ts,mts,cts}` | Evaluated by Node (all configs in one process), then cached in the `lint-config` namespace of `~/.cache/ts-r` |
| 2 | Nearest `.oxlintrc.json` | Read directly |
| 3 | Nearest `biome.json` or `biome.jsonc` | Read directly; Biome rules with an ESLint equivalent |
| 4 | None found | ESLint `recommended`, plus typescript-eslint `recommended` for TypeScript files |

Flat configs merge per file with ESLint 10's rules. The config cache is invalidated when the
config, anything it imports locally or the nearest lockfile changes. `--config` uses one
config for every file instead, with patterns relative to the current directory.

Rules a config turns on that ts-r-lint doesn't have are listed once per config on stderr,
with the reason: needs type information, plugin not implemented, not implemented yet, or
unknown.

Environment variables:

| Variable | Effect |
|---|---|
| `TS_R_NODE` | Node binary used to evaluate `eslint.config.*` (default: `node` on `PATH`) |
| `TS_R_LINT_NODE_WRAPPER` | Command to wrap that Node process with |
| `TS_R_LINT_NO_CONFIG_CACHE=1` | Evaluate the config every run (for cold timings) |

## Next to ESLint

ts-r-lint works on its own. A common split is to run ts-r-lint on every save or commit for
the fast rules, and keep ESLint in CI for type-aware rules and plugins ts-r-lint doesn't have.
In the editor, [ts-r-lsp](ts-r-lsp.md) can show the same diagnostics.

## Measured

Like-for-like, on tRPC's server and client sources (147 files): tRPC's own ESLint config with
every rule ts-r-lint doesn't implement removed, the same file for both tools
(`cargo xtask bench --repos trpc lint-trpc-same-rules`, median of 5 runs, Apple M-series shared
laptop, crates as of 2026-10-18):

| Run | ts-r-lint | ESLint 10.10.0 |
|---|---|---|
| Same 48 rules, no type information, config evaluated by Node every run | 733 ms | 1,015 ms |
| Same, with ts-r-lint's config cache (its default) | 15.5 ms | 1,015 ms |

Both report 0 problems from those rules. Most of ts-r-lint's uncached time is Node evaluating
`eslint.config.js`. With the 32 type-aware rules as well the runs aren't comparable yet: on tRPC
the parity gate turns them off, and with the gate off ts-r-lint reports 11 problems ESLint
doesn't. On four dedicated Linux cores (the review box's bench of pull request #42 on
2026-10-24) the same runs took 2,850.7 ms with the config evaluated every run and 30.6 ms with
it cached, against ESLint's 4,005.4 ms.

Apple M-series Mac (a shared laptop), 4 threads, each project's own lint config, on public
projects pinned in `bench/repos.toml` (`cargo xtask bench --repos`, median of 3 to 5 runs):

| Project | ts-r-lint | ts-r-lint, no type check | Project's linter |
|---|---|---|---|
| tRPC server and client sources | 186 ms | 20 ms | ESLint (type-aware): 2,566 ms |
| TypeORM `packages/typeorm/src` | 810 ms | 194 ms | ESLint (type-aware): 6,718 ms |
| Playwright core sources | 725 ms | 59 ms | ESLint: 5,441 ms |
| date-fns `pkgs/core/src` | 76 ms | | oxlint: 348 ms |

These aren't like-for-like yet. ESLint runs plugin rules ts-r-lint doesn't have (on TypeORM,
most of ESLint's problems come from `jsdoc/*` rules), and when ts-r-check's diagnostics differ
from tsc's on a project, ts-r-lint runs its rules without type information (the parity gate),
so the first column measures a type check plus the untyped rules. The type-aware rules
themselves match ESLint's test suites (see Correctness).

Method and commits: [same rules](/benchmarks/#lint-same-rules),
[lint on Linux](/benchmarks/#lint-linux),
[lint](/benchmarks/#lint) and
[correctness](/benchmarks/#correctness).
