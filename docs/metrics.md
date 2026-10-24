# ts-r metrics

Status as of 16 Oct 2026; the correctness rows for the checker, lint and the bundler were
measured again on 24 Oct 2026 at `087afc9`. Alpha: releases are on npm under the `next` dist-tag.

Speed and memory are measured on pinned public repos ([bench/repos.toml](../bench/repos.toml)):
**excalidraw** (a Vite + React app, about 500 modules), **tRPC**, **date-fns**, **RxJS**,
**TypeORM**, **Playwright** and **three.js x10**, on a shared macOS arm64 laptop (16 cores,
48 GB) with 4 threads while other jobs ran, so treat timings as approximate. Each number comes
from the files listed under [Sources](#sources); `cargo xtask bench --repos` reproduces them.

## Speed and memory

| Tool | Replaces | Measure | ts-r | Baseline | Speed | Memory |
|---|---|---|---|---|---|---|
| `ts-r-dev` | Vite 8.3.4 | excalidraw: save to update (p50, 50 edits) | 8.6 ms | 35.0 ms | **4.1×** | |
| | | excalidraw: cold start to first render | 939 ms | 1,107 ms | 1.2× | |
| | | excalidraw: warm start to first render | 538 ms | 1,032 ms | 1.9× | |
| | | excalidraw: dev server memory after edits | 153 MB | 364 MB | | **2.4× less** |
| `ts-r-lint` | ESLint 10.10.0 | tRPC, same 48 rules, no types, config evaluated each run⁴ | 733 ms | 1,015 ms | **1.4×** | 1.5× less |
| | | Same, config cached (ts-r-lint's default)⁴ | 15.5 ms | 1,015 ms | 65× | 57× less |
| | ESLint | tRPC, type-aware config¹ | 0.19 s | 2.57 s | 14× | 6.7× less |
| | | TypeORM, type-aware config¹ | 0.81 s | 6.72 s | 8.3× | 4.6× less |
| | | Playwright | 0.73 s | 5.44 s | 7.5× | 5.0× less |
| | oxlint | date-fns | 0.076 s | 0.348 s | 4.6× | 21× less |
| | ESLint server | Editor, per keystroke (p50, tRPC) | 15.6 ms | 53.3 ms | 3.4× | |
| | | Editor memory, 154 files open | 10.6 MB | 2,296 MB | | **217× less** |
| `ts-r-build` | esbuild 0.28.2 | excalidraw: first build + 20 rebuilds | 1.36 s | 3.46 s | **2.5×** | |
| | | excalidraw, minified, cached / no cache² | 132 / 255 ms | 201 ms | 1.5× / 0.8× | 2.0× less (no cache) |
| | | three.js x10, cached / no cache⁵ | 132.2 / 255.0 ms | 246.2 ms | 1.9× / 0.97× | 1.6× less (no cache) |
| `ts-r-lsp` | tsserver | tRPC: first errors on opening a file | 224 ms | 800 ms | 3.6× | |
| | | tRPC: keystroke to errors (p50) | 9.4 ms | 24.3 ms | 2.6× | |
| | | RxJS: memory, all 231 files open | 377 MB | 1,091 MB | | 2.9× less |
| `ts-r-check` | tsc 7.0.2 | tRPC `packages/server`³ | 108 ms | 189 ms | 1.7× | 2.5× less |
| | | RxJS `packages/rxjs`³ | 462 ms | 1,280 ms | 2.8× | 2.6× less |
| | | TypeORM³ | 548 ms | 567 ms | 1.04× | 2.2× less |
| | | Playwright³ | 592 ms | 685 ms | 1.2× | 2.4× less |
| `parse<T>` | typia 9.7.2 | Validation speed | | | 0.85–1.07× | |
| | | Bundle size, 10 API types | 5.6 KB | 25.5 KB | | 4.5× smaller |

A speed below 1× means ts-r is slower.

¹ Not like-for-like: ESLint runs plugin rules ts-r-lint lacks, and where ts-r-check's
diagnostics differ from tsc's, ts-r-lint runs without type information (its parity gate).
² ts-r-build leaves out excalidraw's 61 locale chunks (template-literal `import()` isn't
bundled yet), which flatters its times.
³ On 24 Oct 2026 (`087afc9`) ts-r-check matched tsc exactly on tRPC and TypeORM; it reported
2 extra diagnostics on Playwright and 6 extra and 5 missing (of about 21,900) on RxJS. Times
will move as it grows.
⁴ Like-for-like: tRPC's ESLint config limited to the rules ts-r-lint implements, the same file
for both tools, `lint-trpc-same-rules` in `bench/scenarios.toml` (2026-10-18). Both report 0
problems from the rules. ESLint evaluates its config on every run; the cached row skips that.
⁵ Measured again on 2026-10-21 at commit 2fb2555 (load average 11-14): the last of three
invocations of 10 runs each. The other two gave 231.8 and 233.9 ms without the cache against
esbuild's 262.7 and 238.5 ms; peak RSS 420-422 MB (cached 243-246 MB) against 695-696 MB.

Read with these caveats:

- `ts-r-lsp` vs TypeScript 7.0.2's native server: behind on speed, ahead on memory. On tRPC TS 7
  takes 58 ms to first errors and 6.2 ms per keystroke (ts-r 224 ms and 9.4 ms) and uses 480 MB
  with every source open (ts-r 165 MB). On date-fns tsserver didn't publish diagnostics within the
  harness's 120 s.
- `ts-r-lint` in the editor skips 35 of tRPC's configured rules (17 not implemented, 17 plugin
  rules, 1 type-aware); the ESLint server ran them.
- `ts-r-run` vs tsx 4.23: 29 vs 79 ms for hello.ts, 101 vs 270 ms for 1,000 modules (Node 24.21.0, median of 15 runs; commit e92e486).

### Incremental checking (history)

`ts-r-check` with its saved history and nothing changed: tRPC 23 ms, date-fns 78 ms, RxJS 57 ms,
TypeORM 126 ms, Playwright 40 ms (cold: 108-592 ms). The one-file-edit runs were measured on a
private app before the switch to public repos and are not re-measured yet.

### Builds and packages

| Measure | Value |
|---|---|
| Release profile (fat LTO) vs thin LTO | 7% smaller, 2–4% faster |
| PGO on top | 3–5% faster, 26% larger (opt-in; measured before training moved to the public repos) |
| npm package, packed | `ts-r-lint` 1.2 MB · `ts-r-check` 2.3 MB · `ts-r-lsp` 3.7 MB · all tools 11.7 MB |
| Minified output vs esbuild, top 1,000 packages | 3.1% larger in total over 909 packages; median ratio 1.048 (ecosystem/STATUS.md) |
| `parse<T>` runtime | 510 B gzipped (budget 0.5 KB) |

## Correctness

| Area | Result |
|---|---|
| Type checker vs tsc's conformance suite (exact diagnostics) | 97.0% of 12,750 compared test variants |
| private-app projects with exactly tsc's diagnostics | 25 of 25 |
| private-app diagnostics: extra / missing vs tsc | 0 / 0 |
| Lint rules | 154 (ESLint recommended; typescript-eslint recommended, strict and 51 type-aware rules) |
| Lint rule oracle (ESLint + typescript-eslint test cases) | 15,495 / 15,517 (non-type-aware 8,597 / 8,597; type-aware 6,898 / 6,920) |
| Lint on private-app, every configured rule (type-aware included) vs ESLint | 26,578 match, 0 missed, 0 extra; 23 of 23 projects pass the parity gate |
| Parser + printer round trip over tsc's test files | 18,260 files, 0 mismatches |
| Parse corpus (tsc's tests + private-app) | 0 errors where tsc accepts the file |
| esbuild's bundler test cases | 666 / 746 (324 skipped) |
| tsconfig error projects matching tsc | 114 / 118 |
| Sound-mode spec cases | 221 / 221 |
| Incremental vs full check | identical in all 25 projects; 18,000 random edits (seeds 1–60 × 300) |

### Top 1,000 npm packages (`cargo xtask ecosystem`)

| Stage | Pass |
|---|---|
| Parse | 99.9% (988 / 989) |
| TS transform | 100% (48 / 48) |
| Bundle and run, vs esbuild | 100% (915 / 915) |
| Type check, vs tsc 7 | 86.6% (614 / 709) |
| Package's own test suite through ts-r | 0 / 4 (all four fail only on ts-r-check extra errors; all pass with those ignored) |

## Sources

Each number is recorded with its method in the crate or file that owns it:

- Public-repo runs (check, lint, build, editor, 2026-10-16; three.js x10 again 2026-10-21):
  [bench/README.md](../bench/README.md) and `landing/data/bench.json`.
- Dev server: `crates/ts_r_hmr/AGENTS.md` (excalidraw against Vite, `bench/dev/dev.mjs`).
- Lint correctness: `docs/design/0006-lint.md`, `crates/ts_r_lsp/AGENTS.md`.
- Build: `docs/design/0008-bundler.md`, `bench/esbuild-oracle/README.md`.
- Checker: `crates/ts_r_checker/AGENTS.md`, `docs/design/0004-types-and-checker.md` (`m5/checker-*`).
- History: `crates/ts_r_history/AGENTS.md`, `docs/design/0005-history.md`.
- `parse<T>`: `crates/ts_r_guard/AGENTS.md` (`m6/guard-gaps`).
- Release builds: `xtask/AGENTS.md` (`m7/release`).
- Ecosystem: `ecosystem/STATUS.md` (`m7/ecosystem`, later fix branches).
