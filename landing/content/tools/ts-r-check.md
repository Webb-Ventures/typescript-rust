---
title: "`ts-r-check`: type checker"
description: A type checker with tsc's command line and output format. It reports only what it is sure of; many checks are still missing.
nav: ts-r-check
section: Tools
order: 3
badges: ["warn:Alpha", "ok:On npm: @ts-r.dev/check"]
---

`ts-r-check` type-checks a TypeScript project and prints diagnostics in tsc's format, with
tsc's error codes. Like tsc, it also writes the project's JavaScript, source maps and
declaration files unless `noEmit` is set; pass `--noEmit` to only check, as with
`tsc --noEmit`.

The checker is the least finished part of ts-r. Read [Status](../status.md) before you rely
on it.

## How to read its results

The checker follows one rule: no false positives first. When it meets something it doesn't
implement yet, it gives that expression a silent type that is never reported. So:

- An error from ts-r-check is very likely an error tsc reports too.
- A clean run from ts-r-check does not mean tsc is clean. It can miss errors.

Keep tsc in CI until ts-r-check covers the features your project uses.

## What works today

- Loading the project from `tsconfig.json`, including `extends`, with tsc's config
  diagnostics such as [TS6053](../errors/TS6053.md).
- Module resolution that matches tsc's: 63,958 resolutions across 24 private-app
  configs, 0 mismatches.
- Assignability, calls and overloads, generics and inference, unions and narrowing, type
  operators (`keyof`, mapped, conditional and literal types), classes and declarations,
  and unused-code checks. Errors such as [TS2322](../errors/TS2322.md),
  [TS2304](../errors/TS2304.md), [TS2307](../errors/TS2307.md) and
  [TS7006](../errors/TS7006.md) print with tsc's wording.
- tsc's output in both plain and pretty form.

## Not yet

- JSX element and attribute types.
- Evolving arrays (`let xs = []`).
- Implicit `any` in binding patterns (TS7031) and `this` parameter checks.
- Reverse mapped types: inferring `T` from `{ [K in keyof T]: Box<T[K]> }`.
- `keyof X` and indexed-access aliases print expanded (`"a" | "b"` where tsc prints
  `keyof Foo`).
- Circular base classes (TS2310) and some heritage checks.
- TypeScript syntax in `.js` files: the TS8xxx errors such as [TS8002](../errors/TS8002.md)
  aren't reported. ts-r-check parses `.js` files with the plain JavaScript grammar and reports
  a syntax error at the same place instead.
- Program-level compiler-option checks such as [TS5052](../errors/TS5052.md) aren't printed
  yet.

The full list is in [ts_r_checker's notes](../../../crates/ts_r_checker/AGENTS.md).

## Command line

```text
usage: ts-r-check [-p tsconfig.json] [options]
```

`--help` lists every option. It accepts tsc 7's command line; the flags below are the common
ones.

| Flag | What it does |
|---|---|
| `-p <path>` | The project's `tsconfig.json`. Without it, the nearest `tsconfig.json` above the current directory (TS5058 or TS5057 when there is none). |
| `--noEmit` | Only check: write no JavaScript, source maps or declarations. Without it (and without `noEmit` in the tsconfig), ts-r-check writes them as tsc does; syntax it cannot lower for the target yet is reported as TSR2003. |
| `--pretty false` | Plain output: `path(line,col): error TSxxxx: message`, chain lines indented two spaces per level, no summary. |
| `--threads N` | Checker threads. Default 4, clamped to 1 to 4. |

Pretty output is the default on a terminal. It adds code frames and a
`Found N errors in M files.` line. Paths are relative to the current directory, and output is
sorted by path, then position. Config diagnostics print first.

Exit codes follow tsc:

| Code | When |
|---|---|
| 0 | No errors |
| 1 | Errors, and `--noEmit`, `noEmit` or `noEmitOnError` is in effect |
| 2 | Errors otherwise (tsc's "errors, outputs generated") |

You can also run it through the launcher as `ts-r check`.

## Lib files

ts-r-check needs TypeScript's `lib.*.d.ts` files. It looks in this order:

1. The directory in `TS_R_LIB_DIR`.
2. A `lib/` directory next to the executable (or `../lib`).
3. `target/upstream/typescript/tsc/internal/bundled/libs` in a source checkout, after
   `cargo xtask upstream sync --minimal`.

See [Getting started](../getting-started.md) for the source setup.

## Next to tsc

ts-r-check reads the same `tsconfig.json` and prints the same format, so editor problem
matchers and CI annotations that parse tsc's output work unchanged. It also takes tsc's
`--build` and `--watch`, and writes the same output files. Running both and comparing their
output is a good way to try it; [ts-r-build](ts-r-build.md) makes bundles.

For stricter checks than tsc's, see [Sound mode](../sound-mode.md).

## Measured

Against tsc 7.0.2:

| Test | Result |
|---|---|
| tsc's conformance baselines, 12,750 compared test variants | 96.7% exact (all diagnostics and chains); 216 variants with a diagnostic tsc does not report, 22 of them in tests where tsc reports nothing |
| All 25 private-app tsconfig projects | All 25 match tsc exactly (0 missing, 0 extra) |
| Ecosystem check stage | 550 packages pass, 159 fail (77.6%) |

Most extra diagnostics in the ecosystem run are TS2344, TS2305, TS2694 and TS2304. Some of
the 937 conformance extras are runner artefacts, such as tests for module kinds tsc 7 removed,
which have no baseline.

Speed and memory on public projects (`cargo xtask bench --repos`, cold check, no history;
Apple M-series shared laptop, median of 5, peak memory of one run):

| Project | ts-r-check | tsc 7.0.2 |
|---|---|---|
| tRPC `packages/server` | 108 ms, 117 MB | 189 ms, 294 MB |
| date-fns `pkgs/core` | 226 ms, 216 MB | 323 ms, 558 MB |
| RxJS `packages/rxjs` | 462 ms, 458 MB | 1,280 ms, 1,168 MB |
| TypeORM `packages/typeorm` | 548 ms, 377 MB | 567 ms, 826 MB |
| Playwright (root project) | 592 ms, 463 MB | 685 ms, 1,094 MB |

With its saved history and nothing changed, ts-r-check takes 23 to 126 ms on the same
projects. These repos don't type-check cleanly under TypeScript 7, and ts-r-check doesn't
report the same diagnostics as tsc on them yet: it reports extra ones on all five (from a few
dozen on date-fns to over a thousand on RxJS) and misses some of tsc's. ts-r also skips work it
doesn't implement yet, so these numbers will move as the checker grows.

Method and commits: [check](/benchmarks/#check),
[ecosystem](/benchmarks/#ecosystem) and
[correctness](/benchmarks/#correctness).
