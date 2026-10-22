# 0011: Editor daemon (`ts-r-lsp`) and the VS Code bridge

Status: Accepted 2026-10-18 by Riley Webb
Owners: `ts_r_lsp` (fronts, the per-workspace daemon, backends, lint in the editor, the ts-r
backend), `editors/vscode` (the extension, the tsserver stand-in and its router); consumers:
`ts_r_lint`, `ts_r_checker`, `ts_r_history` ([0005](0005-history.md)), `ts_r_sound`
([0007](0007-sound-mode.md)), `ts_r_cache` (cache root, [0009](0009-cache-and-launcher.md))

This record was written after the code, from the code and the `AGENTS.md` files of
`crates/ts_r_lsp` and `editors/vscode`. It describes what exists. The editor features of the
ts-r backend (hover, completion, code actions, refactors) and their measurements are left to
`ts_r_lsp`'s `AGENTS.md`. This record fixes the process model, the wire formats, settings,
document ownership, engine routing and the threading model.

## Context

- Every editor window normally starts its own TypeScript servers. VS Code's default setup
  starts a semantic tsserver, a syntax tsserver and the typings installer per window.
  Measured on a monorepo with two windows: two plain tsservers took 561 MB, VS Code's
  default setup 1180 MB, and one shared daemon with its tsserver 318 MB. Milestone L1: one
  shared, memory-budgeted language server per workspace.
- L2 adds lint diagnostics and fixes from `ts_r_lint` in the same daemon. L3 adds our own
  checker as the default type backend for LSP clients. A per-project parity check against the
  user's own tsc (K-17) decides whether its diagnostics can be trusted.
- VS Code's built-in TypeScript extension owns the whole TS/JS UI (hover, completions,
  refactors, quick fixes, settings). It must stay the only UI, with no duplicate providers, and
  whatever ts-r can't answer must still work.
- Two windows can hold different unsaved text for the same file (K-12).
- TypeScript's behaviour is taken from its CLI help, package layout and protocol, never from
  its source.

## Decision

### Processes

```
editor window ──stdio──> ts-r-lsp            (LSP front)      ──socket / pipe──┐
VS Code window ──IPC──> stand-in tsserver.js ──> ts-r-lsp tsserver (front) ────┤
                                         └──> ts-r-lsp --type-backend ts-r ────┤
                                                                               v
                         ts-r-lsp daemon --root R   (one per root + version, detached)
                              │ one slot per backend key
                              v
             in-process ts-r checker | node tsserver.js | tsc --lsp --stdio (TS 7)
```

`ts-r-lsp` commands:

- no command (or `--stdio`): the LSP front;
- `tsserver [args]`: the tsserver front;
- `<node flags> <x>.js [args]`: the tsserver front with `x.js` as the fallback tsserver (the
  form used by VS Code's `typescript.tsserver.nodePath`);
- `status`, `stop`, `ensure-daemon`, `daemon --root DIR`.

Options: `--root`, `--memory-budget-mb`, `--idle-shutdown-minutes`, `--type-backend
ts-r|typescript`, `--lint auto|on|off`, `--lint-only`, `--fallback-tsserver`, `--node`,
`--name`. Option values travel to the daemon in the hello's `settings`. A usage error exits 2.

### Endpoint and daemon lifetime

- The id is `blake3(canonical root, "\0", crate version, PROTOCOL_VERSION)`, as 16 hex
  characters. The socket is `<cache>/lsp/<id>.sock`, or `/tmp/ts-r-lsp-<user>/<id>.sock` when
  the path would not fit in `sockaddr_un` (104 bytes on macOS). On Windows the endpoint is the
  named pipe `\\.\pipe\ts-r-lsp-<id>`. Beside the socket: `<id>.lock` (spawn lock),
  `<id>.pid` (held under an exclusive lock by the running daemon, so there is exactly one
  daemon per endpoint), `<id>.log` (the daemon's stderr) and `<id>.cancel/` (tsserver
  cancellation files).
- `ensure_daemon` first tries to connect. If that fails, it takes the spawn lock, checks
  again, spawns `ts-r-lsp daemon --root <root>` detached, and waits until the endpoint
  accepts. A front's `connect` retries for up to 5 s (a daemon that is shutting down may refuse
  it).
- The daemon exits `linger` (10 s) after its last client leaves, or on a `stop` control
  message.

### Wire format and `Hello`

Every message in both directions is one `Content-Length` frame holding a JSON object. The
first frame from a front is the hello:

```json
{"tsr":"hello","protocol":1,"kind":"lsp|tsserver|status","root":"/repo","name":"window 1",
 "pid":42,"settings":{...},"tsserver":{"cancellationPipeName":..,"globalPlugins":..,
 "pluginProbeLocations":..,"locale":..,"serverMode":..},"fallbackTsserver":"<lib dir>",
 "nodePath":"/usr/bin/node"}
```

- The daemon refuses a hello whose `protocol` isn't its `PROTOCOL_VERSION` (1; bumped whenever
  this format changes), an unknown `kind`, a missing `root`, or a canonical root other than its
  own. A refusal is `{"tsr":"error","message"}`, and it is final: fronts do not retry it.
- Otherwise the daemon answers `{"tsr":"welcome","daemonPid","backend","slot","notes"}`. A
  lint-only client gets `backend: "ts-r-lint"` and `slot: null`. A `status` front gets
  `{"tsr":"status","status"}` and then disconnects.
- After the hello, frames carry plain LSP messages (LSP fronts) or tsserver messages (tsserver
  fronts). A frame with a `"tsr"` key is a control message: `{"tsr":"cancel","seq"}` (a tsserver
  front relaying the editor's cancellation file), `{"tsr":"status"}`, `{"tsr":"stop"}`.
- The LSP front takes its root from `--root`, then from `initialize` (`rootUri`,
  `workspaceFolders`, `rootPath`), then the cwd. The tsserver front takes it from `--root`,
  then `TS_R_WORKSPACE_ROOT`, then the first `projectRootPath` the editor sends (within 3 s),
  then the cwd. While it waits, it answers `configure` and `compilerOptionsForInferredProjects`
  itself.
- If the daemon connection drops, the LSP front respawns the daemon and replays `initialize`,
  `initialized` and its open documents; requests in flight fail with `RequestFailed`. The
  tsserver front exits instead, and the editor restarts tsserver.

### Settings (`ts-r.*`)

Settings arrive in LSP `initializationOptions`, in `workspace/didChangeConfiguration`, and in
the hello's `settings` (which also carries the front's command-line flags). Keys may be flat
(`memoryBudgetMB`), prefixed (`ts-r.memoryBudgetMB`) or nested (`{"ts-r": {...}}`). Unknown keys
are ignored. Bad values (wrong types, `memoryBudgetMB` under 64, `lint.debounceMs` outside
0..5000, negative minutes) are ignored with a line in the daemon log, never an error.

| Setting | Default | Scope |
|---|---|---|
| `typeBackend` (`ts-r` \| `typescript`; env `TS_R_LSP_TYPE_BACKEND` overrides) | `ts-r` | daemon |
| `memoryBudgetMB` | 1500 | daemon |
| `idleShutdownMinutes` (0 = never) | 10 | daemon |
| `lint.debounceMs` | 10 | daemon |
| `lint.enable` (`auto` \| `on` \| `off`, booleans; the L1 `lint: "syntax"\|"full"` means on) | `auto` (treated as on) | per client |
| `lintOnly` (`--lint-only`) | false | per client |

Daemon-wide settings apply to the whole daemon from whichever hello or change sends them.
`ts-r.checker`, `ts-r.serverPath` and `ts-r.manageTypeScript` are VS Code settings, read by
the extension only. Timing knobs for tests come from the environment (`TS_R_LSP_SAMPLE_MS`,
`TS_R_LSP_RESTART_IDLE_MS`, `TS_R_LSP_LINGER_MS`, `TS_R_LSP_IDLE_SHUTDOWN_MS`,
`TS_R_LSP_LINT_DEBOUNCE_MS`, `TS_R_LSP_RELEASE_MS`).

### Clients, slots and document ownership

- A **client** is one connected front. It keeps its own copy of every document it has open
  (`ClientDoc`: text with UTF-16 positions at the edge, language id, its URI, and tsserver's
  `scriptKindName`/`projectRootPath`).
- A **slot** runs one backend. Its key is `ts-r`, `tsserver:<tsserver.js path>` or
  `lsp:<native exe path>`. In shared mode, clients that need the same key share one slot.
- Each slot has a `Mirror`: for every document the backend has open, the text it was given and
  its **owner**, the client whose text it is. Invariant after every event: the backend's text
  equals the owner's text. Each sync is an open, a close, or an explicit-range edit list
  (`SyncOp`). tsserver gets the edits in reverse order, because it applies them last to first.
- When the owner edits a document, its edits are forwarded as they are. If they don't
  reproduce the client's text, a full replace is sent instead. Any other event runs
  `Mirror::decide`:
  - if all the members' texts are equal, the current owner stays;
  - otherwise each text is compared with the file on disk. If exactly one distinct dirty
    (unsaved) text exists, the backend gets it: a clean window viewing a file that another
    window has dirty sees the dirty text, by design. Ties go to the current owner, then the
    client whose event triggered the decision;
  - if there are two or more different dirty texts, the result is `Diverged` (K-12). The daemon
    switches to **per-client mode** (one slot per client) and merges back when at most one
    client remains.
- Responses go only to the requester; request ids are remapped per slot. Diagnostics go only
  to clients whose text equals the text they were computed for. A client's messages reach its
  backend in order.

### Engine routing in the daemon

`choose_backend(hello)`:

1. An LSP front, with `typeBackend` `ts-r` and our lib files found (`tsr::libs()`;
   `TS_R_LIB_DIR`), gets the in-process **ts-r** backend.
2. Otherwise the project's TypeScript is used: `node_modules/typescript` in the root or an
   ancestor. TS 7 runs its native server (`tsc --lsp --stdio`, from the platform package);
   TS 5/6 runs `node lib/tsserver.js` with `--disableAutomaticTypingAcquisition`,
   `--noGetErrOnBackgroundUpdate` and a per-slot `--cancellationPipeName`, and no
   `--serverMode` (no syntax server).
3. A tsserver front can only use a tsserver. If the project has none (no TypeScript, or only
   TS 7), it gets the hello's `fallbackTsserver` (VS Code's bundled one). If that is missing
   too, the hello is refused with the reason.
4. Lint-only clients get no slot. The daemon's lint worker serves them.

An LSP client on a tsserver backend has its requests translated by `translate.rs` (diagnostics,
hover, definition/type definition/implementation, references, rename, completion and resolve).
The daemon requests diagnostics itself, 120 ms after an edit. The ts-r backend speaks LSP
behind the same `Backend` trait (`send`, `pid`, `in_process`, `helper_pids`, `shutdown`;
output through a sink of events), so routing, the mirror and lint merging don't depend on
which backend runs.

On the ts-r backend, each project (nearest `tsconfig.json`/`jsconfig.json` whose file list has
the file, else one inferred project) runs the parity check when it opens. The check runs the
user's tsc and our checker on the project from disk and compares the multisets of
`file(line,col): TScode`. The result is cached in `<cache>/lsp/parity/`, keyed by the config's
inputs, the nearest lockfile and the TypeScript version. A project's state is `checking`,
`matched`, `fallback` or `unknown`. A `fallback` project takes its diagnostics from one shared
process of the user's TypeScript, started lazily with the first open document of such a
project, while every other feature stays on our checker. The backend broadcasts its state as
`ts-r/status`. `ts-r/projectInfo {textDocument}` returns `{project, config, state, parity,
configDiagnostics}` for one file.

### VS Code: the tsserver stand-in and its router

- The extension writes `<globalStorage>/tsdk/`. This holds `lib/tsserver.js` (a copy of
  `standin/tsserver.js`), a `package.json` with VS Code's bundled TypeScript version, and
  `ts-r.json` (`{bin, fallback, checker, settings}`). It points the **user** settings
  `js/ts.tsdk.path` (or `typescript.tsdk`) at that directory, sets `useSyntaxServer: never` and
  turns type acquisition off. The user's previous values are saved and restored by
  `ts-r.manageTypeScript: false` or the restore command. It also sets `TS_R_WORKSPACE_ROOT` in
  the extension host's environment.
- When VS Code forks the stand-in, it runs `ts-r-lsp ensure-daemon`. The stand-in falls back
  to running the bundled `tsserver.js` in-process with the same argv when that fails, when no
  binary is found, when `--serverMode` isn't semantic, or when `TS_R_STANDIN_DISABLE` is set.
- With `ts-r.checker` `off`, the stand-in spawns `ts-r-lsp tsserver` and passes IPC messages
  through as JSON lines and `Content-Length` frames. With `auto` or `on` (env `TS_R_CHECKER`
  overrides), it runs `standin/router.js`, which speaks tsserver to VS Code and picks an engine
  per file:
  - **R**: an LSP connection, `ts-r-lsp --type-backend ts-r --lint off`, started at once. If it
    fails, or the ts-r backend is unavailable, everything goes to T.
  - **T**: the daemon's shared tsserver (`ts-r-lsp tsserver`), started lazily.
  - **S**: VS Code's bundled tsserver in `--serverMode syntactic`, started lazily for
    syntax-only commands (format, selection range, linked editing, JSX closing tag, doc comment
    template, brace matching).
- T and S stop after `TS_R_BRIDGE_IDLE_MS` (180 s) with no files on them and no requests.
- Per file, the router asks R for `ts-r/projectInfo`. In `auto` mode, `matched` projects go to
  R, and `fallback` and `unknown` projects go to T. A `checking` project waits up to 1.5 s for
  a cached result, then goes to T and moves to R when `ts-r/status` says `matched`. Moving a
  file reopens it in the new engine, closes it in the old one and clears the old diagnostics.
  In `on` mode every file goes to R.
- For R files, the router translates the requests ts-r can answer (`standin/convert.js`):
  quickinfo, completions, navigation, rename, signature help, semantic classifications,
  navtree, `geterr` from R's pushed diagnostics (sources `ts-r` and `ts`), code fixes,
  refactors, organize imports and file renames. Commands with no ts-r answer go to T, opening
  the file there first. `_ts-r.engines` and `_ts-r.configure {checker}` are private commands.
  `auto` ↔ `on` switches live; switching to or from `off` rewrites `ts-r.json` and restarts the
  TS server.
- Lint uses its own connection (`lint/client.js`: `ts-r-lsp --lint-only --root <folder>`), so
  its problems are not shown as TypeScript errors. `ts-r.lint.enable: auto` resolves to off
  while the ESLint extension is enabled for the workspace, and the extension sends the
  daemon `on` or `off`.
- Binary lookup: `ts-r.serverPath`, `TS_R_LSP_BINARY`,
  `<folder>/node_modules/@ts-r.dev/lsp-<platform>/bin/`, `PATH`, `~/.cargo/bin`.

### Threading model

- **The daemon's event loop is the only owner of daemon state.** One thread runs
  `Daemon::run`. It receives `Event`s (`Accepted`, `ClientMsg`, `ClientGone`, `Backend{slot,
  generation, ev}`, `Rss`, `Lint`) from one channel and wakes at least every 250 ms for timers.
  Other threads only post events or drain channels: the accept thread, a reader and a writer
  thread per client, reader threads per backend process (events from a stale generation are
  ignored), and the `rss-sampler` (every 10 s, `ps`, or `tasklist` on Windows). There are no
  locks around daemon state.
- **Lint**: one `lint` worker thread per daemon (256 MB stack), started on the first lint
  request. It owns the config lookup, loaded configs and rule instances. Jobs are `(path,
  text)`, queued jobs for one file collapse to the newest text, and nothing is linted on the
  loop thread.
- **ts-r backend**: one coordinator thread (`tsr`) owns its open documents and maps files to
  projects. Each project is one thread (256 MB stack) that owns its program and runs check
  sessions on `ts_r_checker` workers, checking its inbox between files so that a newer edit
  cancels the rest. Also: one parity thread at a time (`nice`d, tsc capped at 4096 MB and
  600 s), short-lived rename and stop threads, and one thread that returns freed pages to the
  allocator after closed files are released.
- Fronts use a few threads to pump stdio and the socket. The tsserver front also watches
  cancellation files.

### Memory, restarts and idle

The backend tree's RSS is sampled every 10 s. For the ts-r backend this is the daemon's own
RSS plus its fallback TypeScript's tree. Above `memoryBudgetMB`, the backend is restarted only
after clients have been quiet for 30 s (`restart_idle`) with no requests pending, at most once
per activity period. Open documents are then replayed, and in-flight requests fail with
`ContentModified`/`RequestFailed`; they never hang. A crashed backend is restarted at most 3
times in 60 s; after that the clients are told it will retry on the next edit.
`idleShutdownMinutes` with no activity stops every backend, and the next request restarts it.
When the last member of a shared slot leaves while other clients stay, the slot's process
stops.

## Invariants agents must keep

- The daemon loop is the only owner of daemon state: reader threads only post `Event`s, and
  writer threads only drain channels.
- Mirror invariant: for every slot, the backend's copy of each document equals its owner's
  text, and every sync is an explicit open, close or edit list.
- K-12: two different dirty texts for one file split the daemon into per-client slots, and it
  merges back when at most one client remains.
- A client's messages reach its backend in order. Responses go only to the requester.
  Diagnostics go only to clients whose text matches.
- Backend restarts are transparent: documents are replayed and in-flight requests fail, never
  hang. Crash restarts are limited to 3 per 60 s.
- No syntax server and no typings installer in the daemon. The stand-in refuses `--serverMode`
  other than semantic (it falls back to the bundled tsserver).
- Bump `PROTOCOL_VERSION` whenever the front ↔ daemon format changes. The endpoint id includes
  it and the crate version, so mismatched fronts and daemons never meet.
- Settings never fail: unknown keys are ignored, and bad values are ignored and logged.
- Lint never runs on the loop thread, and lint results are published only for the exact text
  that was linted.
- VS Code's TypeScript extension stays the only TS/JS UI. ts-r reaches it only by standing in
  for tsserver, and whatever ts-r can't answer goes to a real tsserver.
- TypeScript's source may be read to understand a feature, never translated.

## Known gaps

- LSP clients on a tsserver backend get no TypeScript code actions (lint's are served), no
  signature help, document symbols, formatting, semantic tokens, inlay hints or pull
  diagnostics.
- tsserver fronts can't use a TS 7 backend; they get the fallback tsserver. The Rust tsserver
  front still serves only TypeScript: VS Code reaches the ts-r backend only through the
  router's LSP connection.
- Plugins, locale and TS 7 client capabilities come from the first client of a slot.
  Daemon-wide settings come from whichever client sent them last.
- A multi-root VS Code window uses its first folder as the root. Open files reach the daemon
  twice in VS Code (stand-in and lint connection), and the daemon keeps one copy per front.
- Bridge: ts-r files fold by indentation (no outlining spans). In VS Code, sound-mode
  diagnostics (source `ts-r sound`) are dropped by `splitDiagnostics`. A file that moves
  engines refreshes its diagnostics at the next `geterr`.
- Parity is all or nothing per project, and the fallback TypeScript sees only the open
  documents' texts.
- Windows: named pipes (`interprocess`) and `tasklist` sampling exist but have not been
  exercised by an editor on Windows. The extension is not packaged as a `.vsix`, and Cursor is
  untested.

## Alternatives

- **One server per window** (the editors' default): measured above (561 MB for two plain
  tsservers and 1180 MB for VS Code's default against 318 MB shared). Rejected for the shared
  daemon.
- **A VS Code language client with `typescript.validate.enable: false`**: VS Code would show
  both engines' hover, completions, definitions, references and code actions, because only
  validation can be switched off, and settings are per workspace, not per project. Rejected.
- **Disabling the built-in TypeScript extension**: loses everything ts-r doesn't have (quick
  fixes, refactors, organize imports, inlay hints, the settings UI), and works per workspace,
  so a project that fails parity would lose diagnostics too. Rejected.
- **A TypeScript server plugin**: it runs inside a real tsserver, so tsserver and its memory are
  always there. Rejected.
- **The tsserver stand-in as a router** (chosen): we already own the process VS Code starts as
  tsserver. The cost is translating LSP answers into tsserver bodies (`convert.js`).
- **`typescript.tsserver.nodePath` pointing at `ts-r-lsp`** (no shim process): supported (the
  `<node flags> <x>.js` form) but not automated by the extension.
- **`vscode-languageclient` and a framework transport**: the extension has no runtime
  dependencies (a hand-written LSP client, `standin/lspclient.js`), and the daemon's framing is
  hand-written. `interprocess` is used only for Windows named pipes.

## Status

Written 2026-10-18 from the code at the top of the stack (`m7/bench-box`), after L1-L4 had
shipped. Accepted 2026-10-18 by Riley Webb. Measurements (memory per setup, keystroke latency,
parity counts) are kept in `crates/ts_r_lsp/AGENTS.md` and `editors/vscode/AGENTS.md`.
