---
title: "`ts-r-run`: run TypeScript with Node"
description: Runs a TypeScript file on the Node you already have, like tsx or ts-node. Needs Node 22.15 or later. Compiles the module graph once and caches it.
nav: ts-r-run
section: Tools
order: 5
badges: ["warn:Alpha", "ok:On npm: @ts-r.dev/run"]
---

`ts-r-run` runs a TypeScript file with Node. It compiles the files the entry imports, caches
them, and starts Node with a small preload that serves the compiled code. It replaces `tsx`
and `ts-node` for scripts, servers and tests. Like them, it doesn't check types; use
[ts-r-check](ts-r-check.md) or tsc for that.

## What works today

- `.ts`, `.tsx`, `.mts` and `.cts` entries, with ES modules and CommonJS in the same graph.
- Enums, namespaces, parameter properties and TC39 decorators.
- tsconfig `paths`, and the JSX settings from `tsconfig.json`.
- Stack traces mapped back to the TypeScript lines.
- `--watch`, which restarts Node when a file of the module graph changes, recovers after a
  syntax error and leaves no processes behind.
- A warm start that only checks file sizes and times before handing over to Node.
- Emit that matches tsc 7: all 339 files of the private-app apps and packages compile to
  the same JavaScript.

## How a run works

The default `prebuild` strategy:

1. If every input of the last run for this entry and these flags still has the same size and
   modification time, start Node straight away with the recorded result.
2. Otherwise load the nearest `tsconfig.json`, walk the import graph from the entry and
   compile each level in parallel. Every file goes through the cache. Type-only imports are
   never followed.
3. Write a manifest of the compiled graph, then start
   `node --import <preload> <entry> args`.

The module format follows Node's rules: `.mts` and `.cts` first, then the `type` field of
`package.json`, then the file's syntax. A file that mixes ES syntax with CommonJS globals runs
as CommonJS, as it does under tsx.

ts-r-run resolves only relative specifiers and tsconfig `paths`. Package imports stay with
Node, so `exports` and runtime conditions behave exactly as they do without ts-r.

## Not yet

- The cache check doesn't notice a new file that would change a resolution, such as adding
  `a.ts` next to an `a/index.ts` that was used before. Edit any input or pass `--no-cache`.
- `--strategy hook` doesn't see `require()` calls inside CommonJS modules. Use the default
  strategy.
- Only the entry's `tsconfig.json` is used for every file, as with tsx. Per-package tsconfigs
  in a monorepo are ignored.
- Windows is untested.

## Command line

```text
ts-r-run: run a TypeScript file with Node

Usage: ts-r-run [options] [node options] <file> [args...]
       ts-r-run [options] --test [node options]   (Node's test runner, default files)

Options:
  --watch               Restart when a file of the module graph changes
  --tsconfig <file>     Use this tsconfig (default: nearest tsconfig.json above <file>)
  --no-source-maps      Don't map stack traces back to the TypeScript source
  --no-cache            Don't read or write the transpile cache (~/.cache/ts-r)
  --strategy <s>        prebuild (default): compile the graph, then start Node
                        hook: compile on demand from Node's module.register hooks
  --node <path>         Node binary (default: node on PATH)
  -h, --help            Show this help
  -V, --version         Show the version

Other options before <file> go to Node (use --flag=value for flags with values,
except -r, --require, --import, --env-file, -C and --conditions).

Environment: TS_R_RUN_THREADS (compile threads), TS_R_CACHE_DIR (cache root).
Needs Node 22.15+ (module.registerHooks).
```

- Arguments after `<file>` go to your script. Unknown options before it go to Node, for
  example `ts-r-run --inspect --env-file .env server.ts`.
- `--no-source-maps` makes large graphs start faster, at the cost of stack traces that point
  at the compiled code.
- `--strategy hook` compiles each module when Node asks for it, through a compiler process
  that runs next to Node. It is slower on large graphs (see below); the default is the better
  choice for most projects.

| Variable | Effect |
|---|---|
| `TS_R_RUN_THREADS` | Threads used to compile the graph |
| `TS_R_CACHE_DIR` | Cache root (default `~/.cache/ts-r`) |

Exit codes: Node's own exit code (on Unix ts-r-run replaces itself with Node), 1 when a file
doesn't compile (errors print in tsc's format), 2 for a usage error.

You can also run it through the launcher as `ts-r run`.

## Next to tsx and Node

ts-r-run needs nothing installed in the project. It runs on the Node you already have, so
`ts-r-run script.ts` works where you would write `tsx script.ts` or
`node --experimental-strip-types script.ts`. Unlike Node's type stripping, it handles enums,
namespaces, parameter properties, decorators and tsconfig `paths`.

## Measured

Node 24.21.0, Apple M-series Mac, 4 threads, median of 15 runs:

| | hello.ts | 1,000 modules |
|---|---|---|
| `node -e 0` (floor) | 23 ms | 24 ms |
| `node --experimental-strip-types` | 50 ms | 215 ms |
| tsx 4.23 (binary; `npx tsx` adds about 400 ms) | 79 ms | 270 ms |
| ts-r-run, warm | 29 ms | 101 ms |
| ts-r-run, entry touched (cache warm) | 30 ms | 116 ms |
| ts-r-run, empty cache | 32 ms | 137 ms |
| ts-r-run `--no-source-maps` | 29 ms | 81 ms |
| ts-r-run `--strategy hook` | 48 ms | 390 ms |

Method and commits: [run](/benchmarks/#run) and
[correctness](/benchmarks/#correctness).
