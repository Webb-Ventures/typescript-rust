# editors/vscode

Minimal VS Code / Cursor extension for ts-r (L1 type backend, L2 lint, L3 the ts-r checker in
VS Code). Plain JavaScript with `// @ts-check`, no bundler, no runtime dependencies (the lint
connection and the bridge use a hand-written LSP client, `standin/lspclient.js`, not
`vscode-languageclient`). Root rules: [../../AGENTS.md](../../AGENTS.md). Server side:
[../../crates/ts_r_lsp/AGENTS.md](../../crates/ts_r_lsp/AGENTS.md).
Design record: [0011](../../docs/design/0011-editor-daemon.md).

## How it plugs in: a tsserver stand-in, not a language client

VS Code's built-in TypeScript extension keeps all the UI (hover, completions, refactors,
quick fixes, ...). We only replace the tsserver process it starts:

1. `extension.js` writes `<globalStorage>/tsdk/`:
   - `lib/tsserver.js`: a copy of `standin/tsserver.js`;
   - `package.json`: `{"version": <VS Code's bundled TypeScript version>}` (VS Code reads the
     version from `<tsdk>/../package.json` to gate protocol features);
   - `ts-r.json`: `{bin, fallback, settings}`. `bin` is the `ts-r-lsp` path, `fallback` is VS
     Code's bundled `typescript/lib`, `settings` are the `ts-r.*` values.
2. It sets **user** settings: `js/ts.tsdk.path` (or `typescript.tsdk` on builds without the
   unified keys, e.g. Cursor) = that lib dir, `useSyntaxServer` = `never`, and automatic type
   acquisition off. The user's old values go to `globalState` and come back with
   `ts-r.manageTypeScript: false` or the "Restore VS Code's Own TypeScript Server" command.
   User-level tsdk is used without the workspace-trust prompt that a workspace tsdk needs.
3. It sets `process.env.TS_R_WORKSPACE_ROOT` in the extension host. VS Code forks tsserver with
   a copy of that env, so the stand-in knows the workspace before the first request.
4. VS Code forks `lib/tsserver.js` (`--useNodeIpc`, or stdio when
   `typescript.tsserver.nodePath` is set). The stand-in runs `ts-r-lsp ensure-daemon`, then
   spawns `ts-r-lsp tsserver <VS Code's args>` and bridges IPC messages to JSON lines and
   `Content-Length` frames back to IPC messages. Cancellation files
   (`--cancellationPipeName`) are watched by the Rust front.
5. When it cannot work (no binary, daemon will not start, `--serverMode` other than semantic,
   `TS_R_STANDIN_DISABLE` set), the stand-in `require`s the bundled `tsserver.js` in-process with
   the same argv, so TypeScript keeps working.

Alternative (no shim process): set `typescript.tsserver.nodePath` to the `ts-r-lsp` binary;
it accepts `<node flags> <tsserver.js> <args>`. Not automated by the extension.

## Lint (L2): a lint-only LSP connection

Lint does not go through the tsserver stand-in (VS Code's TypeScript extension would show it
as TypeScript errors). `lint/client.js` starts `ts-r-lsp --lint-only --root <folder>`, an LSP
front attached to the same workspace daemon, with no type backend:

- sends `didOpen` / `didChange` (incremental) / `didSave` / `didClose` for `file:` documents in
  TS/TSX/JS/JSX, and `workspace/didChangeWatchedFiles` for `eslint.config.*`, `.oxlintrc.json`,
  `biome.json(c)` (the daemon re-reads configs and re-lints open files);
- shows `publishDiagnostics` in a `ts-r-lint` diagnostic collection (source `ts-r-lint`, the
  rule id linking to its docs);
- registers a code action provider (`quickfix`, `source.fixAll.ts-r`) that asks the daemon:
  the rule's fix and suggestions, "Fix all <rule> problems", "Disable <rule> for this line"
  (`// eslint-disable-next-line`), "Disable <rule> for the entire file" (`/* eslint-disable */`).
  `"editor.codeActionsOnSave": {"source.fixAll.ts-r": "explicit"}` fixes on save;
- shows the daemon's one info message about configured rules ts-r-lint does not run
  (type-aware rules, plugins) and logs the full list in the ts-r output channel;
- reconnects if the front exits (at most 3 times a minute; then "ts-r: Restart Lint").

### Coexisting with the ESLint extension

`ts-r.lint.enable`: `auto` (default) | `on` | `off`. `lint/mode.js` decides:

| Setting | ESLint extension (`dbaeumer.vscode-eslint`) | ts-r-lint |
| --- | --- | --- |
| `auto` | enabled in this workspace and `eslint.enable` not `false` | **off**: no duplicate problems |
| `auto` | not installed, disabled, or `eslint.enable: false` | on |
| `on` | anything | on (turn ESLint off for the workspace yourself) |
| `off` | anything | off |

VS Code only lists enabled extensions, so a workspace-disabled ESLint extension counts as not
enabled. The decision is re-made when `ts-r.lint.enable`, `eslint.enable` or the installed
extensions change; the reason is logged in the ts-r output channel. Only files governed by an
ESLint, oxlint or Biome config are linted.

## The ts-r checker in VS Code (L3): the stand-in routes

`ts-r.checker`: `auto` (default) | `on` | `off`.

| Setting | Projects that pass the parity check | Projects that fail it (or are unchecked) |
| --- | --- | --- |
| `auto` | ts-r answers; no tsserver unless a feature needs it | the shared tsserver, as `off` |
| `on` | ts-r | ts-r for everything; diagnostics from ts-r-lsp's internal fallback TypeScript |
| `off` | the shared tsserver (L1) | the shared tsserver (L1) |

### Mechanisms considered

- **`typescript.validate.enable: false` plus our own providers (a language client).** VS Code
  would show both engines' hover, completions, definitions, references and code actions
  (the TS extension still registers its providers; only validation has a switch), so every
  feature needs a per-feature setting or duplicates. There is no setting for hover,
  completions or definitions, and the settings are per workspace, not per project.
- **`typescript.tsserver.*` settings / workspace-scoped disabling of the built-in TS
  extension.** Disabling `vscode.typescript-language-features` loses everything ts-r does not
  have (quick fixes, refactors, organize imports, inlay hints, JS/TS settings UI), and it is
  per workspace, so a project that fails parity would lose diagnostics too. Rejected.
- **A TS server plugin.** It runs inside a real tsserver, so tsserver (and its memory) is always
  there. Rejected: the point is that tsserver does not start.
- **The tsserver stand-in as a router (chosen).** We already own the process VS Code starts as
  tsserver. `standin/router.js` speaks tsserver to VS Code and decides per file which engine
  answers. VS Code's TS extension stays the only UI, so there are no duplicates and nothing is
  lost: what ts-r cannot answer goes to a real tsserver started on demand. Cost: we
  translate LSP answers to tsserver bodies (`standin/convert.js`).

### How the router works

`standin/tsserver.js` reads `checker` from `ts-r.json` (env `TS_R_CHECKER` overrides). With
`auto`/`on` and a workspace root, it runs the router instead of passing through. Engines:

- **R**: one LSP connection to the daemon, `ts-r-lsp --type-backend ts-r --lint off`
  (`standin/lspclient.js`). It is started at once. If it fails, or the ts-r backend is not
  available (no lib files), the router passes everything to T.
- **T**: the daemon's shared tsserver (`ts-r-lsp tsserver`, the L1 path), started **lazily**:
  for files of projects on TypeScript, and on demand for commands ts-r does not have. It is
  stopped after `TS_R_BRIDGE_IDLE_MS` (180 s) with no T-only files and no requests.
- **S**: VS Code's bundled tsserver in `--serverMode syntactic` (about 130 MB), lazily, for
  syntax-only commands on ts-r files: format, on-type format, selection range, linked editing,
  JSX closing tag, doc comment template, brace matching. Same idle stop.

Per file (`decide`): the router asks R for `ts-r/projectInfo` (small additive request in
`crates/ts_r_lsp/src/tsr/bridge.rs`: project, config, state, parity summary). `auto` routes
`matched` projects to R and everything else (`fallback`, `unknown`: the check could not run)
to T. While a project is `checking`, the router waits up to 1.5 s for a cached result
(the daemon sends cached results at once); after that the file goes to T and moves to R when
`ts-r/status` says `matched` (re-routing re-opens the file in the new engine, closes it in the
old one and clears the old diagnostics). `on` routes every file to R.

For R files:

- quickinfo, completions (+ entry details with auto-import edits), definition (+ bound span),
  type definition, references, document highlights, rename, signature help, semantic
  classifications, navtree and projectInfo are translated to LSP requests;
- `geterr` is answered from R's pushed diagnostics (`source: "ts-r"` so the Problems panel
  shows which engine reported them; syntax / semantic / suggestion split by `data.kind`).
  In `on` mode R forwards a mismatched project's fallback TypeScript diagnostics with
  `source: "ts"`; `splitDiagnostics` keeps those too (with that source), or the file would be
  cleared;
- `getOutliningSpans` gets an empty answer: VS Code then folds by indentation and
  `// #region` markers;
- code actions come from R's `textDocument/codeAction` (L4, `viaTsrActions`, converted by
  `convert.js` `codeFixesBody` / `refactorsBody` / `fileCodeEdits`): `getCodeFixes` (the
  error codes become context diagnostics; R fills in their messages), `getCombinedCodeFix`
  (the fix-all edits R listed with the last `getCodeFixes` of that file version),
  `organizeImports` (modes as source action kinds), `getApplicableRefactors` (implicit on
  every cursor move, and the refactor menu, `triggerReason: "invoked"`, with
  `includeInteractiveActions`) and the `getEditsForRefactor` that follows it (the engine that
  listed the refactors, `refactorEngine`, answers it). R gets `tsr: {interactive, format}`
  (the `configure` format options of that file) and, for Move to file, `tsr.targetFile`;
  `getMoveToRefactoringFileSuggestions` goes to R's `ts-r/moveToFileSuggestions`. ts-r has
  every refactor tsserver lists, so the refactor menu of TypeScript files is R's; in
  JavaScript files (`isJsFile`) the menu stays T's (JSDoc-based refactors such as Extract to
  typedef are T-only), and a menu request R fails on goes to T.
  `getEditsForFileRename` goes to R's `workspace/willRenameFiles` (VS Code asks after the
  move, so edits of the moved file are renamed to its new path) unless the file is open on T.
  What R has no answer for (no fix for those codes, a fix-all it did not list, an error) goes
  to T;
- every other command (inlay hints, file references, call hierarchy, ...) is sent to T after
  opening the file there; `preparePasteEdits` returns `false` unless T already runs;
- the project's tsconfig problems (`extends` not found, unknown options, no inputs) are sent
  as a `configFileDiag` event when a file of the project opens, from `ts-r/projectInfo`'s
  `configDiagnostics`, as tsserver does when it loads a project;
- `getSupportedCodeFixes` is answered from a cached list (`tsdk/code-fixes-<version>.json`,
  computed once from the bundled `typescript.js`), so VS Code knows which errors have fixes
  without T.

Unsaved text across engines. Each engine reads files that are not open in it from disk, so:

- **Same project** (a ts-r file that T answers a request for): `openInT` opens the project's
  other open ts-r files in T with it, and later ones of that project join while T has it.
- **Across projects (`auto`), only where the other engine's projects use the file.** A ts-r
  file is opened in T too when the file list of the T project of a file T serves includes it
  (`projectInfo` with `needFileNameList`, asked by the bridge for each file T serves, again
  after an edit on an `import`/`export`/`require` line of that file or a
  `projectsUpdatedInBackground` / `projectLoadingFinish`; the answers never reach VS Code). A
  T file is opened in R when `ts-r/usedBy` says a project R serves for this window reads it
  (asked when files open, close or move engines, and on `ts-r/inputsChanged`). In R it is a
  *mirror* (`languageId: "ts-r-mirror"`): read by R's projects, owned by none, so no
  ts-r diagnostics and no project or fallback TypeScript starts for it. The checks run
  outside the request queue (`refreshMirrors`), then open the files from inside it
  (`applyMirrors`). After that, its edits and its close go to both engines through the
  existing `inT` / `inR` paths; T's diagnostics for a ts-r file and R's for a mirror are
  dropped. Nothing is mirrored that no project of the other engine uses; a mirror stays until
  the file closes, moves engines or T stops.

Files on disk: R needs nothing from the bridge. The daemon watches the inputs of its ts-r
projects natively (`crates/ts_r_lsp/src/tsr/watch.rs`), so an unopened file that changes on
disk reaches ts-r's hover and diagnostics without VS Code. `watchChange` answers T's own
`createFileWatcher` events and goes to T only. `reloadProjects` ("TypeScript: Reload
Project") reaches both: R gets `ts-r/reloadProjects` (configs and closed inputs read again),
and T gets the command when it runs.

`configure`, `compilerOptionsForInferredProjects` and `configurePlugin` are stored and
replayed to T and S when they start. The router also answers two private commands (VS Code's
`typescript.tsserverRequest` forwards `_`-prefixed commands unchanged): `_ts-r.engines`
(which engine serves each project and file, T/S pids, which command last started T, the
requests answered without T, the last `ts-r/status`) and
`_ts-r.configure {checker}` (switch `auto` <-> `on` live).

### Memory in VS Code (measured on a private monorepo; historical)

`bench/editor-memory.js` (see Checks) in VS Code 1.142 (macOS arm64, release `ts-r-lsp` with M5
history). It ran on a private monorepo with TypeScript 6.0.3, read-only; for the pinned public
repos, `cargo xtask bench --editor --repos <names> --run` prints and runs it (bench/README.md). Editor-side memory is the
RSS of the extension host's process tree plus the ts-r daemon's tree (every language server).
VS Code's own main, renderer and GPU processes are 1.25-1.46 GB in every run and are not
included. Stock means VS Code's defaults: semantic and syntax tsservers, plus type acquisition.
Repeated runs vary by up to about 60 MB (a second cold matched-only run: 414 MB).

| Files open | Stock VS Code | ts-r, `auto`, first open (no cached parity) | ts-r, `auto`, warm |
| --- | --- | --- | --- |
| 13 files in 11 projects (3 match parity, 8 fall back) | 822 MB (peak 886) | 748 MB (peak 1063) | **724 MB** (peak 784) |
| 12 files in the 3 projects that match | 730 MB (peak 786) | 352 MB (peak 671) | **345 MB** (peak 404) |

- Where it goes (warm, mixed set): the extension host 166 MB and the JSON server 75 MB are the
  same in both setups. Stock adds the semantic tsserver (262), the syntax tsserver (227) and
  typingsInstaller (90). ts-r adds the stand-in (73), three `ts-r-lsp` fronts (13), the daemon
  (49, the ts-r checker for 3 projects) and the shared tsserver for the 8 fallback projects
  (348).
- Matched-only set: no tsserver process at all. TypeScript-side memory goes from 495 MB to
  106 MB (stand-in 69, fronts 7, daemon 30). 34 implicit refactor requests were answered
  without starting TypeScript. Diagnostics: the same 21 file diagnostics (TS2307 ×15,
  TS7006 ×6) and the 3 tsconfig `extends` errors.
- The first open's peak includes the parity check's tsc runs, which happen once per tsconfig
  and TypeScript version. In that cold run, the first hover in a fallback project took 11 s:
  TypeScript loaded 8 projects while tsc ran. Warm, hovers are 9-54 ms (stock 11-347 ms).
- On that monorepo most projects fell back (see `crates/ts_r_lsp/AGENTS.md`, Parity),
  so the mixed set saves 12%. The more projects pass parity, the closer a window gets to the
  matched-only numbers. The daemon's ts-r side alone, with all 649 files open through an LSP
  client, is 386 MB (M5 history; `bench/editor-latency.js`).

### Extension side

- `ts-r.checker` change: `auto` <-> `on` is live through `_ts-r.configure`, no reload. To or
  from `off` rewrites `ts-r.json` and restarts the TS server (VS Code's own command), no window
  reload and no prompt.
- Status bar (`checker.js` `describeEngines`, polled every 3 s while focused and on editor
  change): the active file's engine (`ts-r`, `ts-r · N on TypeScript`, `TypeScript` with a
  warning background), a tooltip line per project with the reason (parity passed, the
  difference, not checked). Clicking it runs "ts-r: Show Which Engine Serves Each Project"
  (`ts-r.showEngines`), a quick pick with the projects, the three modes and the report command.
- "ts-r: Report Parity Mismatch" (`ts-r.reportParity`, `checker.js` `parityIssue`) opens
  `github.com/Webb-Ventures/typescript-rust/issues/new?labels=parity` prefilled with the editor,
  platform, ts-r and TypeScript versions and, per project ("Project 1", ...), the result and
  the differing diagnostic codes with counts. Never file names, positions, messages or source.
  Without a bridge it reads `ts-r/status` from the lint connection or `ts-r-lsp status`.

## Problem matcher (`$ts-r`)

`contributes.problemMatchers` declares `ts-r` (tasks use `"problemMatcher": "$ts-r"`): `$tsc`'s
pattern with `TSR?\d+` codes, so sound-mode errors (`error TSR0201: ...`, record 0007,
decision 2) are picked up from `ts-r check` output in both plain and pretty form. The same
pattern ships for GitHub Actions in `npm/ts-r/problem-matcher.json`;
`test/problem-matcher.test.js` keeps the two equal.

## Settings

`ts-r.checker` (`auto` | `on` | `off`, see above), `ts-r.memoryBudgetMB` (1500), `ts-r.idleShutdownMinutes` (10, 0 = never),
`ts-r.lint.enable` (`auto` | `on` | `off`, see above), `ts-r.serverPath`,
`ts-r.manageTypeScript`. A change to another type setting rewrites `ts-r.json` and restarts the TS
server (the daemon applies the new values at the next hello); a change to `ts-r.lint.*` only
starts or stops the lint connection. L1's `ts-r.lint` (`off` | `syntax` | `full`) is gone; the
daemon still reads it (`syntax`/`full` mean on). L1's `ts-r.typeBackend` is replaced by
`ts-r.checker` (LSP clients other than VS Code still choose with `--type-backend`).

Binary lookup: `ts-r.serverPath`, `TS_R_LSP_BINARY`, `<folder>/node_modules/@ts-r.dev/lsp-<platform>/bin/`,
`PATH`, `~/.cargo/bin`.

## Checks

```sh
npm install            # devDependencies only: typescript, @types/vscode, @types/node
npm run check          # tsc --noEmit over extension.js, standin/, lint/, test/
npm test               # drives the stand-in like VS Code (IPC and stdio), the router
                       # (test/bridge.test.js) and the lint connection against
                       # target/*/ts-r-lsp
```

`npm test` needs a built `ts-r-lsp` (`cargo build -p ts_r_lsp`, or `TS_R_LSP_BIN`) and the lib
files (`TS_R_LIB_DIR` or `target/upstream`); it uses the fake tsserver backend and a fake tsc
for parity, so no TypeScript install. `test/bridge.test.js` covers routing (`auto`: matched
project to ts-r, mismatched to TypeScript), diagnostics, hover, completions, definition,
syntax commands on the syntax server, folding, on-demand TypeScript for code fixes and its idle
stop, switching to `on`, and a cached parity result never starting TypeScript. With stubbed
engines it covers unsaved text reaching TypeScript for a ts-r project's other files, both
cross-engine directions (only files the other engine's projects use) and Reload Project
reaching ts-r and TypeScript.

`npm run test:vscode` (`test/vscode/run.js` + `suite.js`) runs the extension in a real VS Code
(stable, downloaded by `@vscode/test-electron` into `target/vscode-test/`) with a fresh user
data dir and a temp workspace: `pass/` (the parity check passes with the real tsc) and `fail/`
(its tsc output gets an injected `TS9999`). It checks, inside the extension host: ts-r
diagnostics, hover, completions and definition in `pass/`; no tsserver process in the extension
host's or the daemon's process tree after cursor moves; the refactor menu answered by ts-r
(convert export, Move to a new file) with TypeScript still not started; TypeScript on demand
for call hierarchy; `fail/` diagnostics from TypeScript with no duplicates; the status bar text; the issue
URL has codes but no file names; `auto` -> `on` -> `off` -> `auto` switching. Needs
`cargo build --release -p ts_r_lsp`, `npm install`, and about 1 GB; run it as
`scripts/limit.sh --mem-mb 6144 -- node editors/vscode/test/vscode/run.js` (about 10 s after
the first download). VS Code 1.142's macOS binary is `Code`, not `Electron`; run.js handles it.

`bench/editor-memory.js` + `bench/editor-memory-probe.js` (not packaged) measure
editor-side memory in a real VS Code on a real project. The runs are stock, ts-r with no
cached parity, and ts-r warm (`--cache`). Each run opens a fixed file set without editing it,
asks for a hover in each file, waits until diagnostics and parity settle, then sums RSS per
process kind. Run each mode on its own under `scripts/limit.sh --mem-mb 6144`. The cache and
the daemon socket go to `/tmp/tsrm-*`, so it never shares a daemon with another ts-r on the
same project. Results are in the L3 section above.

`bench/lint-keystrokes.js` (not packaged) measures lint latency per keystroke and server
memory through LSP, for `ts-r-lsp --lint-only` or the ESLint extension's own server
(`~/.vscode/extensions/dbaeumer.vscode-eslint-*/server/out/eslintServer.js`) run headlessly.
Run it under `scripts/limit.sh` and stop the daemon afterwards; usage is in its header,
results in `crates/ts_r_lsp/AGENTS.md`.

`bench/editor-latency.js` (not packaged) measures a type backend the same way: first
diagnostics, hover / definition / completion latency, keystroke-to-diagnostics on one file
and memory with one or all files open, for `ts-r-lsp` (ts-r backend), a tsserver.js or TS 7's
native LSP. Same rules (limit.sh, stop the daemon); results in `crates/ts_r_lsp/AGENTS.md`.

## Gaps

- Bridge: ts-r files fold by indentation (no outlining spans from ts-r). Features without a
  ts-r answer start the TypeScript server on demand (memory comes back after the idle stop).
  Hover/completion gaps of the ts-r checker (see `crates/ts_r_lsp/AGENTS.md`, Gaps L3) show
  as-is in matched projects. A file that moves engines (parity result arrives) is re-opened in
  the new one; its diagnostics refresh at the next `geterr`.
- Bridge, cross-engine mirrors: a ts-r file opened in T makes T load that file's own project
  too (tsserver gives every open file its default project), so a TypeScript project importing
  an open ts-r file costs T that project's memory while the file is open. Mirroring is checked
  after opens, closes, engine moves and import-line edits, not per keystroke: an import typed
  into a T file reaches a mirror about a second later. A mirror no longer needed stays until
  the file closes.
- Bridge in `on` mode: mismatched projects get diagnostics from ts-r-lsp's internal fallback
  TypeScript (a second process of the user's TypeScript), everything else from ts-r.
- TS 7 projects: tsserver clients cannot use the native server, so VS Code windows get the
  bundled tsserver (shared). LSP clients get TS 7.
- Changes user-level settings (restorable); a multi-root window uses its first folder as root.
- Not packaged as a `.vsix` yet. Tested inside VS Code 1.142 (`npm run test:vscode`); Cursor is
  not tested.
- Lint: the VS Code side (diagnostic collection, code action provider, ESLint detection) is
  type-checked but not run inside VS Code; `npm test` covers the protocol and the auto/on/off
  decision. Open files reach the daemon twice (stand-in and lint connection); the daemon keeps
  one copy per front.
