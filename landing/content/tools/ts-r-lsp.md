---
title: "`ts-r-lsp`: language server"
description: One shared, memory-budgeted language server per workspace. Runs your project's TypeScript or the ts-r checker, plus ts-r-lint diagnostics.
nav: ts-r-lsp
section: Tools
order: 6
badges: ["warn:Alpha", "ok:On npm: @ts-r.dev/lsp", "next:VS Code extension not packaged"]
---

`ts-r-lsp` gives every editor window on a workspace one shared server instead of a TypeScript
process each. Each window starts a small front process (about 3 MB) that connects to one
daemon per workspace. The daemon runs the type backend, watches memory, and adds lint
diagnostics from [ts-r-lint](ts-r-lint.md).

## Type backends

| Backend | When it is used | What you get |
|---|---|---|
| ts-r | `ts-r.typeBackend` is `ts-r` (the default), the client speaks LSP, and the lib files are found; in VS Code, `ts-r.checker` (see [VS Code](#vs-code)) | The ts-r checker: diagnostics, hover, definition, references, rename, completion, signature help, document symbols, semantic tokens |
| typescript | `ts-r.typeBackend` is `typescript`, or the ts-r backend can't be used | Your project's own TypeScript: tsserver 5.x or 6.x, or TypeScript 7's native `--lsp --stdio` |

The ts-r checker still misses errors tsc reports (see [ts-r-check](ts-r-check.md)). So when a
project opens, the daemon runs your own tsc on it in the background and compares the
results. If they differ, that project's diagnostics come from tsserver instead, while hover,
definition and the other features stay on ts-r. The result is cached until the tsconfig, the
lockfile or the TypeScript version changes. `TS_R_LSP_PARITY=off` turns the check off.

## What works today

- One daemon per workspace, shared by every window, over a Unix socket in the ts-r cache.
- A memory budget: when the backend's memory passes `--memory-budget-mb` and you have been
  quiet for 30 seconds, the backend restarts. Open documents are replayed, so the editor
  doesn't notice.
- Idle shutdown of the backend after `--idle-shutdown-minutes`; the next request starts it
  again. The daemon exits 10 seconds after its last window closes.
- Lint diagnostics and code actions from ts-r-lint's rules and configs: fixes, suggestions,
  "Fix all", "Disable for this line" and "Disable for the entire file".
- A tsserver stand-in (`ts-r-lsp tsserver`) so VS Code's built-in TypeScript support can
  share the daemon.
- No syntax server and no typings installer, which is part of the memory saving.

## Not yet

- In VS Code, the `ts-r.checker` setting (`auto` by default) sends a project to the ts-r
  backend only when it passes the parity check; other projects use TypeScript. Parity is all
  or nothing per project, so one differing diagnostic sends the whole project to TypeScript.
  How many of the public bench projects pass it is not measured yet.
- Over tsserver, the LSP side has no TypeScript code actions, signature help, document
  symbols, formatting, semantic tokens or inlay hints.
- Slower than TypeScript 7's own LSP to first diagnostics, and per keystroke on two of the
  three measured public projects (see [Measured](#measured) and the
  [benchmarks](/benchmarks/)).
- Hover at a call shows the declared signature, not the resolved one. JSX intrinsic
  attributes (`<form onSubmit>`) have no symbol for hover or go to definition.
- Only open files get diagnostics; files that depend on an edited file aren't checked until
  you open them.
- The VS Code extension in `editors/vscode` isn't packaged as a `.vsix` yet. It is tested
  inside VS Code 1.142 (`npm run test:vscode`).
- Windows is untested.

The full list is in [ts_r_lsp's notes](../../../crates/ts_r_lsp/AGENTS.md).

## Command line

```text
ts-r-lsp: one shared, memory-budgeted language server per workspace

USAGE
  ts-r-lsp [--stdio] [OPTIONS]          LSP server on stdio (spawns or attaches to the daemon)
  ts-r-lsp tsserver [TSSERVER ARGS]     tsserver stand-in on stdio (same daemon)
  ts-r-lsp [NODE FLAGS] <path>/tsserver.js [TSSERVER ARGS]
                                        same, when set as VS Code's typescript.tsserver.nodePath
  ts-r-lsp status [--root DIR]          print the workspace daemon's status as JSON
  ts-r-lsp stop [--root DIR]            ask the workspace daemon to exit
  ts-r-lsp ensure-daemon [--root DIR]   start the daemon if needed and print its address
  ts-r-lsp daemon --root DIR            run the daemon in the foreground

OPTIONS
  --root DIR                    workspace root (default: from initialize, else the cwd)
  --memory-budget-mb N          ts-r.memoryBudgetMB (default 1500)
  --idle-shutdown-minutes N     ts-r.idleShutdownMinutes (default 10, 0 = never)
  --type-backend ts-r|typescript  ts-r.typeBackend (ts-r: our checker, LSP clients only)
  --lint auto|on|off            ts-r.lint.enable for this front (default auto, which lints)
  --lint-only                   lint diagnostics and code actions only, no type backend
  --fallback-tsserver PATH      tsserver to use when the project has none
  --node PATH                   node binary for tsserver when node is not on PATH
  --name NAME                   name shown in status and logs
  --version, --help
```

- Editors other than VS Code start `ts-r-lsp --stdio` as an ordinary language server.
- `ts-r-lsp status` prints the daemon's state as JSON: clients, projects, memory and each
  project's parity result.
- `--lint-only` serves lint diagnostics and code actions only, with no type backend.
- `TS_R_LSP_TYPE_BACKEND` overrides `ts-r.typeBackend`.

## VS Code

The extension in `editors/vscode` makes ts-r-lsp VS Code's tsserver, so the built-in
TypeScript features keep working on the shared daemon. Projects that pass the parity check
are answered by the ts-r checker (diagnostics, hover, completions, definition, references,
rename, signature help, semantic highlighting, outline); your TypeScript starts on demand only
for features ts-r doesn't have yet, such as quick fixes and refactorings.

| Setting | Default | Effect |
|---|---|---|
| `ts-r.checker` | `auto` | `auto`: ts-r for projects that pass the parity check, your TypeScript for the rest; `on`: ts-r everywhere; `off`: your TypeScript |
| `ts-r.memoryBudgetMB` | 1500 | Restart the backend above this |
| `ts-r.idleShutdownMinutes` | 10 | Stop an idle backend (0 = never) |
| `ts-r.lint.enable` | `auto` | `auto` turns lint off when the ESLint extension is enabled |
| `ts-r.serverPath` | | Path to `ts-r-lsp` |
| `ts-r.manageTypeScript` | `true` | Point VS Code's TypeScript at the ts-r stand-in |

The extension looks for the binary in `ts-r.serverPath`, then `TS_R_LSP_BINARY`, then
`node_modules/@ts-r.dev/lsp-<platform>/bin/`, then `PATH`, then `~/.cargo/bin`. See
[the extension's notes](../../../editors/vscode/AGENTS.md).

## Measured

On public projects pinned in `bench/repos.toml` (Apple M-series shared laptop, one run per
tool), with the ts-r backend, against each repo's own tsserver and TypeScript 7's native LSP
(7.0.2):

| | ts-r | tsserver | TS 7 LSP |
|---|---|---|---|
| tRPC `router.ts`: first diagnostics | 224 ms | 800 ms | 58 ms |
| tRPC: keystroke to diagnostics, p50 / p95 | 9.4 / 10.5 ms | 24.3 / 29.3 ms | 6.2 / 7.4 ms |
| tRPC: memory, one file / all 154 sources open | 95 / 165 MB | 571 / 757 MB | 477 / 480 MB |
| RxJS `merge-map.ts`: keystroke p50 / p95 | 12.2 / 12.8 ms | 28.6 / 35.1 ms | 13.3 / 17.2 ms |
| RxJS: memory, all 231 sources open | 377 MB | 1,091 MB | 820 MB |
| date-fns: keystroke p50 / p95 | 14.1 / 14.7 ms | timed out | 5.0 / 5.8 ms |
| date-fns: memory, all 1,641 sources open | 232 MB | timed out | 710 MB |

ts-r uses the least memory everywhere, but TypeScript 7's LSP is faster to first diagnostics
on all three and per keystroke on tRPC and date-fns (ts-r re-checks each edit on a fresh
checker). ts-r also reports a few diagnostics tsc doesn't on the opened RxJS and tRPC files.
Large projects haven't been measured yet.

Two editor windows on tRPC (two files open in each) with its TypeScript 6.0.3 as the backend:
698 MB in total, against 838 MB for two plain tsservers and 1,451 MB for VS Code's default
per-window setup.

Lint on keystrokes in tRPC's largest server file (22 KB, all 154 sources open): 15.6 / 15.8 ms
p50 / p95 with the default 10 ms debounce, against 53.3 / 58.6 ms for the ESLint extension's
server. The daemon used 10.6 MB; the ESLint server used 2,296 MB, but it also ran 35 rules
of the config that ts-r doesn't run in the editor yet.

Method and commits: [editor](/benchmarks/#editor),
[editor lint](/benchmarks/#editor-lint) and
[editor memory](/benchmarks/#editor-memory).
