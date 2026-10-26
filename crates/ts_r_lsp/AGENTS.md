# ts_r_lsp

`ts-r-lsp`: one shared, memory-budgeted language server per workspace (milestone L1). Editors
talk LSP or the tsserver protocol to a thin stdio front; the front attaches to (or spawns) a
per-workspace daemon; the daemon runs the type backend. In L1 the only backend is
`typescript`: the project's own TypeScript (tsserver for 5.x/6.x, the native `--lsp --stdio`
server for 7.x). Since L2 the daemon also lints open files with `ts_r_lint` (see "Lint"
below). Since L3 the default backend for LSP clients is `ts-r`, our own checker running in the
daemon process behind the same `Backend` trait (see "ts-r backend" below). Root rules:
[../../AGENTS.md](../../AGENTS.md). Editor side: [../../editors/vscode/AGENTS.md](../../editors/vscode/AGENTS.md).
Design record: [0011](../../docs/design/0011-editor-daemon.md).

## Processes

```
editor window ──stdio──> ts-r-lsp (front, ~3 MB) ──unix socket / named pipe──┐
editor window ──stdio──> ts-r-lsp tsserver (front) ──────────────────────────┤
                                                                             v
                          ts-r-lsp daemon --root R  (one per root+version, detached)
                                   │ one slot per backend key (shared mode)
                                   v
                          node tsserver.js ...   |   tsc --lsp --stdio (TS 7)
```

- Endpoint: `blake3(root, version, PROTOCOL_VERSION)[..16]` → `<cache>/lsp/<id>.sock`
  (`/tmp/ts-r-lsp-<user>/` when the path would exceed the 104-byte sun_path limit),
  `\\.\pipe\ts-r-lsp-<id>` on Windows. Beside it: `<id>.lock` (spawn lock), `<id>.pid` (daemon
  flock: exactly one daemon per endpoint), `<id>.log`, `<id>.cancel/` (tsserver cancellation).
- Wire: Content-Length frames both ways. First frame is a `Hello` (`wire.rs`); the daemon
  answers `{"tsr":"welcome"}` or `{"tsr":"error"}` (final: fronts do not retry it). Later
  control frames: `{"tsr":"cancel","seq"}`, `status`, `stop`.
- `ts-r-lsp` with no command is the LSP front; `tsserver` the tsserver front; also `status`,
  `stop`, `ensure-daemon`, `daemon`. `<node flags> <x>.js args` (VS Code's
  `typescript.tsserver.nodePath` form) is the tsserver front with `x.js` as fallback.

## Modules

| Module | What |
|---|---|
| `jsonrpc` | Hand-written framing (`read_frame`, `write_message`, tsserver's line-or-frame input), message classification, error codes |
| `text`, `uri` | `TextDoc` (UTF-16 positions at the edge), LSP/tsserver position conversion, file URI <-> path (VS Code's encoding) |
| `settings` | `ts-r.*` settings (`typeBackend`, `memoryBudgetMB`, `idleShutdownMinutes`, `lint.debounceMs`; per client `lint.enable`, `lintOnly`) + test-only timing overrides |
| `lint` | Lint worker thread (config lookup and cache shared with `ts-r-lint`), LSP diagnostics, code actions, disable comments |
| `ipc`, `wire` | Endpoint, socket/pipe, spawn lock, daemon lock; `Hello` and tsserver argv parsing |
| `backend/` | `Backend` + `BackendFactory` traits, `ProcessBackend`, TypeScript discovery (`find_typescript`, TS 7 platform package) and launch specs |
| `docs` | Per-client open documents, `Mirror` (what the backend has), divergence detection (K-12) |
| `translate` | LSP <-> tsserver for diagnostics, hover, definition/typeDefinition/implementation, references, rename, completion (+resolve) |
| `daemon` | Single-threaded event loop: clients, slots, request routing, diagnostics fan-out, memory/idle policy, status |
| `front` | LSP front (reconnect + replay) and tsserver front (cancellation-file watcher, local configure answers) |
| `process` | RSS sampling of process trees (`ps`; `tasklist` on Windows) |
| `tsr/` | The ts-r backend: coordinator (`mod.rs`), per-project checker threads (`project.rs`), sound mode (`project/sound.rs`), releasing closed files (`project/release.rs`), type-aware lint (`project/lint.rs`), diagnostics conversion (`convert.rs`), language features (`features/`, code actions in `features/actions/`), parity check (`parity.rs`), tsserver fallback (`fallback.rs`), watching project inputs (`watch.rs`) |
| `client`, `fake` | `ScriptedClient` for tests/benchmarks; fake tsserver/LSP backends (`__fake-backend`) |

## Invariants

- **The daemon loop is the only owner of state.** Reader threads (clients, backends, sampler)
  only post `Event`s; writer threads only drain channels. No locks around daemon state.
- **Mirror invariant:** for every slot, the backend's copy of each document equals the text of
  that document's owner client (the most recent editor). Every sync is an explicit-range edit
  list or open/close (`SyncOp`); tsserver gets edits reversed (it applies last-to-first).
- **K-12:** if two clients hold *different unsaved* text for the same file, the daemon splits
  into per-client mode (one backend per client) and merges back when ≤ 1 client remains. A
  clean window viewing a file another window has dirty sees the dirty text (by design).
- **Ordering:** a client's messages reach its backend in order; responses go only to the
  requester (ids remapped per slot); diagnostics only to clients whose text matches the backend.
  A window that opens a file the backend already has (no sync op is sent) gets the backend's
  last diagnostics for it: tsserver's cached `geterr` results, or an LSP backend's last push
  (`Slot::pushed`, kept with the mirror version it arrived at and dropped on close).
- **Backends restart transparently** (budget, crash, idle stop): documents are replayed,
  in-flight requests fail with `ContentModified`/`RequestFailed`, never hang. Crash restarts
  are limited to 3 per 60 s.
- **Memory:** sampled RSS of the backend tree vs `memoryBudgetMB` (the in-process ts-r backend:
  the daemon's RSS plus its fallback TypeScript's tree); restart only when the user
  has been quiet for `restart_idle` (30 s) with no pending requests, at most once per activity
  period. `idleShutdownMinutes` (0 = never) stops the backend; the next request restarts it.
  The daemon exits `linger` (10 s) after its last client.
- **No syntax server, no typings installer:** tsserver gets
  `--disableAutomaticTypingAcquisition`, and the stand-in refuses `--serverMode` other than
  semantic.
- Behaviour comes from TypeScript's CLI help, package layout, protocol and tests. Its source may
  be read to understand a feature, never translated (root AGENTS.md).

## Lint (L2)

- **One lint worker thread** per daemon (started on the first lint request, 256 MB stack like
  `ts-r-lint`'s workers) owns `ConfigLookup`, loaded configs and rule instances. The daemon
  loop sends it `(path, text)` jobs and gets `Event::Lint` back; it never lints on the loop
  thread. Queued jobs for one file collapse to the newest text.
- **Configs** come from `ts_r_lint::project::{ConfigLookup, load_configs}`: the same nearest
  ESLint / oxlint / Biome lookup and the same Node evaluation + `lint-config` cache as
  `ts-r-lint`. Files no config governs are not linted (no built-in default in the editor).
  A saved or watched (`workspace/didChangeWatchedFiles`) config file reloads configs and
  re-lints every open file. Files an ESLint config imports are checked (mtime, size) before
  the loaded config is used again; a change loads it again and re-lints every open file. No rule reads other files yet, so an edit re-lints only that file.
- **When:** open and save lint at once; edits after `ts-r.lint.debounceMs` (10) of quiet.
  Results are published only to clients whose text equals the linted text, and kept per
  client: windows with different unsaved text of a file each get their own text linted. A text that does
  not parse keeps the previous results (no flicker; the type backend shows the error).
- **Who:** per client `ts-r.lint.enable` (`auto` | `on` | `off`; the server treats `auto` as on,
  editor extensions resolve it). LSP clients with a type backend get type and lint
  diagnostics merged into one `publishDiagnostics` per file (`Client::type_diags`). A
  *lint-only* client (`lintOnly: true` in `initializationOptions`, or `--lint-only`) has no
  slot and no type backend: the VS Code extension's lint connection. tsserver clients never
  get lint.
- **Unsupported rules** of a config: one `window/logMessage` with the list per client and
  config, and one `window/showMessage` (info) per client. Never diagnostics.
- **Type-aware rules** run on the ts-r backend's checker, not here (see "Type-aware lint").
- **Code actions** (from the last results, only if the client's text is the linted text):
  the rule's fix, its suggestions, "Fix all <rule> problems", "Disable <rule> for this line",
  "Disable <rule> for the entire file", and `source.fixAll.ts-r` (one non-overlapping pass, as
  one `--fix` pass). On a line that starts in JSX children the line comment is
  `{/* eslint-disable-next-line rule */}` (a `//` comment there would be rendered text); the
  JSX text is found by parsing with the options `ts_r_lint::parse_for_lint` used
  (`lint::parse_options`: module goal, operators after `as T`), so both see one tree. LSP backends' own code actions are kept and lint's appended.

## ts-r backend (L3)

- **Chosen** when `ts-r.typeBackend` is `ts-r` (the default; env `TS_R_LSP_TYPE_BACKEND`
  overrides), the client speaks LSP and the lib files are found (`LibStore::locate`, cached in
  `tsr::libs()`); otherwise the project's TypeScript as in L1. tsserver fronts keep TypeScript.
- **Threads:** `TsrBackend::send` posts to one coordinator thread (`tsr`) that owns the open
  documents (`TextDoc`, UTF-16 only here), maps each file to its project (nearest
  `tsconfig.json`/`jsconfig.json` whose file list has it, references included; else one
  inferred project) and forwards. Each project is one thread (256 MB stack) that owns its
  `Arc<ProgramFiles>`; a check session borrows it (`Checker::layered(&program, base)`), so no
  `unsafe` and no self-referential structs. Everything the backend emits goes through the daemon's sink
  as LSP messages, so the L1 routing, mirror and lint merging apply unchanged.
- **Per edit** (design record 0005): the session ends, edits queued behind it are coalesced,
  the edited files are re-parsed (`SourceFile::new` over the overlay), and the program is
  reloaded only if a file's imports or triple-slash references changed. The edited file's
  syntax diagnostics are published at once with its previous semantic diagnostics remapped
  through the edits (syntax first, no flicker); then a new top-level `Checker` re-checks, one
  file per step and checking the inbox between steps: the edited files, then, when an edited
  file's `ts_r_decls` API hash changed, the open files with a use whose deep hash changed
  (`ts_r_history::shape::ProgramShapes`, built at the first API change and kept current;
  after a reload old and new shapes are compared, or every importer when there were none).
  A newer edit cancels the rest (`cancelled_passes`). `$/cancelRequest` answers
  `RequestCancelled`.
- **Levels (M5 history, `ts_r_history`):** before each session `Project::arrange` orders the
  program lib, dependencies, stable project files, hot files (the 3 most recently edited
  files and everything that needs them) and finds the lower levels in the process-wide
  registry or builds them; the session's checker is layered on them. Lib and dependency
  levels are shared by every project with the same files and checking options (paths and
  emit settings do not count; their `SourceFile`s too); the stable level survives keystrokes in the same files, so a keystroke
  re-checks only hot work. Reordering changes `FileId`s: `index` and the published
  diagnostics (and their related spans) are remapped. Opening a document is not an edit (it
  does not change the hot set); `relayout` skips arranging when nothing changed. The project
  keeps its `base` between sessions so the registry (weak references) finds it.
- **Diagnostics** are LSP `publishDiagnostics` with `source: "ts-r"`, `data.kind`
  (`syntax`/`semantic`), `Unnecessary`/`Deprecated` tags; options as `ts-r-check` (skipLibCheck,
  checkJs). Only open files get diagnostics; closing a file clears them.
- **Language features** (`tsr/features/`) run on the project thread inside the check session,
  on the worker that checked the file (so node types are cached): `locate` (offset to node,
  name to symbols: bindings, references, members via the object's type, qualified names,
  import/export specifiers, shorthands, destructuring keys, component JSX attributes),
  `hover` (tsc 7's quick info, built in `quick_info.rs`: one line per meaning or overload,
  the resolved signature at a call, owners with type parameters or arguments, object types
  one member per line (`multiline.rs`), narrowed type at expressions, JSDoc as markdown;
  an import alias shows its target as tsc 7 does: `(alias) function name(...)`, no
  `import` line; see "Quick info" below),
  `complete` (members after `.` from the object's apparent type, or a namespace's exports;
  scope locals innermost first, globals, keywords; with a typed prefix, auto-imports of the
  project's own modules' exports with the import edit in the file's style: quotes,
  semicolons, `.js` extensions (always in an ES module under `node16`/`nodenext`), joining
  an existing import; `completionItem/resolve` adds
  the hover line and docs, routed by `data.uri`), `signature` (the enclosing call, `new` or
  tagged template, active parameter from top-level commas or the `${}` holding the cursor,
  labels with UTF-16 parameter offsets, no `new` prefix, the resolved instantiation of a
  generic signature, JSDoc),
  `nav` (definition through aliases and module specifiers, to the overload a call resolves
  to, type definition, references, highlights (write kind only where a value is stored:
  assignments, initialised declarations, `this.x += 1`), prepareRename/rename with shorthand expansion; references scan non-lib,
  non-`node_modules` files whose text contains the name, including string-literal element
  accesses `o["x"]`), `outline` (document symbols,
  semantic tokens with TypeScript's legend). A checker panic is caught per request and per
  check (`guarded`): the session's workers are dropped, the request fails, the thread lives.
  A panic while arranging levels or building the checker (`arrange`, `build_checker`) falls
  back to one level, then to no checker (`without_checker`): every owned file gets one
  "ts-r internal error" diagnostic, requests fail with `InternalError`, and the next change
  rebuilds. The project thread never dies silently.
- **Parity check (K-17, `parity.rs`):** when a project opens (state `checking`), a background
  job (one at a time, `nice`d) runs the user's own tsc (`node tsc.js` next to the found
  tsserver, or the TS 7 binary; `-p <config> --noEmit --pretty false`, capped at 4096 MB and
  600 s like `scripts/limit.sh`) and our checker on the project from disk, and compares the
  multisets of `file(line,col): TScode` keys. The result is cached in
  `<cache>/lsp/parity/` under a key of the config's inputs (the tsconfig and everything it
  extends), the nearest lockfile and the TypeScript version, so it reruns only when those
  change. `TS_R_LSP_PARITY=off` disables it; `TS_R_LSP_PARITY_TSC` (JSON argv) replaces tsc in
  tests. A run that cannot happen (no TypeScript, tsc failed) leaves the project `unknown`, on
  our checker.
- **Fallback (`fallback.rs`):** on a mismatch the project's state becomes `fallback`: its
  project thread stops publishing semantic diagnostics (syntax ones too) and the coordinator
  opens the project's documents in one shared process of the user's TypeScript (tsserver
  `updateOpen` + `geterr`, or the TS 7 language server with pull `textDocument/diagnostic`
  after each open and change, and pushed diagnostics), forwarding full texts on change and
  publishing its diagnostics only when they belong to the current text. Every other feature
  stays on our checker. The process restarts when it exits, at most 3 times; then the
  projects go back to our checker (`unknown`). The mismatch writes a plain-text report (both
  outputs, the differing keys) whose path is in the status.
- **Status:** the backend sends `ts-r/status` (`{backend, version, summary,
  fallbackTypeScript, fallbackPid, projects: [{project, config, state: checking | matched |
  fallback | unknown, openFiles, parity: {result, detail, typescript, ours, theirs, onlyOurs,
  onlyTheirs, report, cached, oursMs, theirsMs}}]}`) on every state change; the daemon keeps
  the latest (also in `ts-r-lsp status` as `slots[].tsr`), broadcasts it to every LSP client
  including lint-only ones, sends it to late joiners and answers a `ts-r/status` request.
- **`ts-r/projectInfo` (`tsr/bridge.rs`)** `{textDocument: {uri}}` -> `{project, config (null
  for the inferred project), state, parity, configDiagnostics}` for one file, opening its project
  if needed. `configDiagnostics` are the tsconfig's parse and discovery diagnostics
  (`ConfigInfo::diagnostics`, LSP ranges), which the bridge shows on the tsconfig. The
  VS Code bridge (`editors/vscode/standin/router.js`) uses it to route each file to ts-r or to
  TypeScript; with a cached parity result the answer is final at once.
- **Cross-engine mirroring (`tsr/bridge.rs`).** A bridge that serves some projects with
  TypeScript mirrors an open TypeScript file into ts-r only when a ts-r project uses it, so
  ts-r reads its unsaved text instead of the saved one. `ts-r/usedBy` `{uris}` ->
  `{<uri>: [project]}` lists, per file, the loaded projects whose program contains it (the
  file set each project reports after a load, `ProjectHandle::inputs`); a load that changes a
  project's set sends `ts-r/inputsChanged` `{project}` (broadcast to the slot's LSP clients) so
  the bridge asks again. A `didOpen` with `languageId: "ts-r-mirror"`
  (`bridge::MIRROR_LANGUAGE_ID`) is a mirror (`OpenDoc::mirror`): every project gets its text
  as for any open document, but no project owns it: its project is not started for it, it
  gets no diagnostics, it is not counted in `openFiles`, it is never re-homed and the
  fallback TypeScript never opens it. The daemon's document mirror keeps the language id, so
  a backend restart replays it as a mirror.
- **Lazy fallback:** the fallback TypeScript starts with the first open document of a
  `fallback` project, not when the parity result arrives, so a client that only asks
  `ts-r/projectInfo` (the bridge, for files it then sends to its own tsserver) never starts it.
- **Shared slots stop when empty:** when a client leaves and its shared slot has no members
  left while other clients stay attached, the slot's process stops (the bridge's on-demand
  tsserver gives its memory back; previously it lived until the daemon's idle shutdown).
- **The daemon watches its projects' inputs (`tsr/watch.rs`), whatever the editor does.** VS
  Code's bridge registers no file watchers and other clients may not either, so the backend
  does not depend on `workspace/didChangeWatchedFiles`. Watched, each non-recursively (one
  watch per directory, `node_modules` never walked): the directories of every started
  project's tsconfig chain (`ConfigInfo::inputs`; the config's own directory also sees new
  configs and sources) and of every file of its loaded program, inside the root or not (each
  load sends `CMsg::Inputs`; lib files and paths through `node_modules`, `.git`, `.hg`,
  `.svn` or `target` are left out). At most `MAX_DIRS` (4096) directories; added only, for
  the backend's life. Events are debounced on the `tsr-watch` thread as in `ts_r_hmr` (quiet
  30 ms, at most 300 ms; a temp file created and gone in one burst is dropped; a missing file
  gets 50 ms to come back; on macOS a change whose mtime predates its directory's watch is an
  FSEvents replay and is dropped during the first 10 s) and *sent* to the coordinator as
  `CMsg::Watched`; only the coordinator's loop touches state. A settled change goes through
  the same `on_changes` as `workspace/didChangeWatchedFiles`: closed files are re-read by the
  projects that use them, `.json` / `ts-r.toml` / created / deleted paths reload configs.
  Only TS/JS sources, `.json(c)`, `ts-r.toml` and created or removed directories are
  reported. A directory created in a watched one is watched with its subdirectories (depth 8,
  256 at most) and then reported as created once more, so files made in it before its watch
  began are found by the config reload that follows. A client that also sends `didChangeWatchedFiles` costs a second, idempotent
  re-read. `TS_R_LSP_WATCH=off` turns the watcher off.
- **`ts-r/reloadProjects`** (notification; the bridge sends it for VS Code's "Reload
  Project"): every config is read again and every reported input that is not open is re-read
  from disk.

## String completions (`tsr/features/strings.rs`)

Rules learned from `tsc: src/services/stringCompletions.ts` (v7.0.2), written in our own
terms. The fourslash tests are the oracle.

- A position is inside a string when it is past the opening quote of a string literal, a
  no-substitution template or a template part (`${` parts belong to their template), and
  before the closing quote; an unterminated literal runs to its end. A `"use strict"`
  directive is a string too. Comments win over strings.
- What is listed depends on the literal's parent: module specifiers (`import`/`export ...
  from`, `import x = require()`, `import()`, `require()`, `import("x")` types) list files and
  packages; `o["|"]` and `"|" in o` list the object type's property names (no `__@` symbols,
  no `#private`); the other side of `===`/`!==`/`==`/`!=` lists its string literals; an object
  literal key lists the contextual type's properties not written yet; a `case` lists the
  discriminant's literals not used by another case; a call or `new` argument lists the
  literals of that parameter in every candidate signature (a rest parameter's element type
  too); type positions list the literal members of an indexed access's key, the other union
  members, or the constraint of the type parameter a type argument fills. Anywhere else, the
  contextual type's string literals. Nothing found answers `null`, as tsc does.
- Labels are escaped as they would be written in the literal: `\\`, `\n`, `\r`, `\t`, and `"`
  (even in single quotes); in a template, `` ` `` instead of `"`.
- Module specifiers: a relative fragment (or a rooted one when there are no `paths`) lists the
  directory part of the fragment under the importing file's directory (the importing file and
  `@types` left out; folders as folders). Names follow the file's import style: no extension
  (bundler/classic), or `.js`/`.mjs`/`.cjs`/`.jsx` (ES modules under `node16`/`nodenext`, or
  a file that already writes extensions); `.js` files only with `allowJs`, `.json` only with
  `resolveJsonModule`, other extensions only when an ambient `declare module "*.ext"`
  exists. A bare fragment lists `paths` keys without `*`, ambient module names, type roots'
  packages (`@types/a__b` as `@a/b`, filtered by `types`), then the dependencies of every
  package.json above the file, or, if none, what the ancestor `node_modules` hold.
- Package entry points (`tsc: tsc/internal/ls/string_completions.go`): a package with
  `exports` lists only what its keys allow (conditions read in the order the import kind
  resolves with: `types`, `import` or `require`, `node`, `default`; the first matching
  condition wins); with no `exports`, `typesVersions` paths, then the package's files. The
  project's own `imports` (`#x`) work the same way from the nearest package.json, mapping
  output paths back to sources through `outDir`/`rootDir`. A pattern key (`./sub/*`,
  `./bar/_*/suffix`) lists the files below its target directory that match the target's
  suffix, named relative to the directory already typed (`pkg/sub/b/` lists `c`, not
  `sub/b/c`), with the key's suffix added to files but not folders; when the typed text
  stops inside the key's prefix, only the rest of the prefix is offered. A target whose
  suffix already gives the extension (`*.js`) is not given it twice; a declaration file
  stands for its `.js`. `paths` keys follow the same pattern rule.
- `allowImportingTsExtensions`: an ES module under `node16`/`nodenext` names `.ts`/`.tsx`/
  `.mts`/`.cts` files with their extension; elsewhere they keep it only behind a wildcard
  target (`#*` -> `./src/*`) and only when the file does not already write `.js` endings.
- `/// <reference path="|` lists files with their names as on disk; `types="|` lists type
  roots' packages. Directory listings stop at 2000 entries and 64 ancestors.

## Completion contexts (`tsr/features/complete.rs`)

Rules from `tsc: tsc/internal/ls/completions.go` and `tsc: tsc/internal/binder/binder.go`
(v7.0.2), in our words; the fourslash tests are the oracle.

- Object literal: when the name being typed follows the literal's `{`, a `,`, or the end of
  an earlier member with no comma yet (a shorthand member only from an earlier line), the
  list is the contextual type's properties (a parenthesized literal takes its parens'
  context; for a union, every property of its object members, leaving out primitives,
  arrays and callable members) minus the names other members write (spreads do not count;
  the member under the cursor does not either), minus members that only this literal
  declares (an inferred type parameter). Non-identifier names are quoted. With no
  contextual type, or one without properties, the names in scope are offered instead.
- Object binding pattern (`const { | } = v`, `({ | }: T) =>`, `for (const { | } of xs)`,
  nested `{ a: { | } }`): the properties of the destructured type (declared type, then
  initializer, then the iterated type; a parameter's declared or contextual type) minus
  the names already bound; `private`/`protected` members only inside their class. When that
  type cannot be known the list is empty, never the scope.
- Optional properties (`?`, a mapped type's or a union's optional result) are labelled
  `name?` with `insertText`/`filterText` `name`, in member, object, pattern and string
  (`"|" in o`, `o["|"]`) lists; object and pattern lists sort them after required ones.
- Class, interface and type literal bodies (`class_items.rs`): at a member-name slot
  (after the body's `{`, after a member's `;`/`}`/`,` or on a later line, or after member
  modifiers) the list is the member keywords (`public`, `static`, `get`, `readonly`, ...;
  interfaces and type literals only `readonly`) plus, in a class, the base class's and
  implemented interfaces' members not yet written (not `private`/`#private`; `static`
  lists the base constructor's statics; `override` only the base class's; after `private`
  no list). Inside methods, accessors, constructors, property initializers and static
  blocks (arrows see through, other functions end the search), the class's `this` members
  are offered among the names in scope as `this.name` (statics in static code); a
  function's declared `this` parameter type gives its members the same way.
- JSX (`jsx_items.rs`): after a tag name (`<Foo |`, `<Foo a="" |`, an opening tag still
  without its `>`) the list is the props type's members minus attributes already written
  (members a spread provides are still offered); a tag the checker could not resolve reads
  its own signatures (first call parameter, or the `props` of what a construct signature
  returns, through constraints). After `</` the list is the open element's name; at `<|`
  the names in scope plus `JSX.IntrinsicElements`'s tags. Text between tags offers nothing.
- Positions (`position.rs`): the token before an offset and the node covering it, and
  whether an offset sits in a type (annotation, type arguments, `extends`/`implements`,
  `as`/`satisfies`), which switches the scope list to types and namespaces. A namespace
  is offered there (and among `a.b.|` members in a type, not after `typeof` or in
  `import x = a.b`) only when a type is somewhere below it, through aliases and nested
  namespaces (`tsc: tsc/internal/ls/completions.go`, the type-location symbol filter).
- Import and export clauses (`import { | } from "m"`, `export { | } from "m"`, the token
  before the name being the clause's `{`, a `,` or `type`): the module's exports minus
  `default` and the names other specifiers already take (the one being edited does not
  count), then `type` unless just written; nothing when all are taken or the module does
  not resolve. A quoted export inserts `"a b" as a_b` in an import without `as`; a
  re-exported name has kind variable. `ns.|` of a module in an expression lists its values
  and namespaces only.
- Possible type arguments (`tsc: tsc/internal/ls/utilities.go`, the backward scan for a
  `<` that might open type arguments): reading tokens back from the offset over balanced
  `<>`, `()`, `[]`, `{}` and the tokens a type may hold, to an identifier right before an
  unmatched `<`, counts the arguments already written. The list is types when the callee
  has a signature with at least that many type parameters (construct signatures after
  `new`), when it is a type reference already, or after `new X<` whatever `X` is; not when
  `typeof` precedes, and not when the identifier is being declared.
- Scope: a variable is not offered inside its own initializer (`const x = |`), except when
  the cursor is past the initializer's end with nothing typed, or inside a JSX tag; blocks,
  arrows and patterns end the search. A CommonJS JavaScript file (one that refers to
  `require`, `module` or `exports` freely) has `module` and `exports` as locals.
- Expected types (`type_args.rs`, `expected.rs`): a type literal in a type argument
  (directly, as a property type below one, or in a union or intersection there) offers the
  members of the type parameter's constraint not yet declared (for calls, the union of the
  constraints across the callee's signatures, uninstantiated). Literal entries (`"a"`, `2`,
  `3n`, never enum members) come from the expected type: a property's or tuple element's
  expected type in such a literal; in an expression, the contextual type (every signature's
  parameter at a call argument, the declared type after `=`, the other side of `===`, the
  switched value after `case`). Strings take the quote of the file's first import.
- Definition locations (`definitions.rs`): where a new name is being written (`var |`,
  `class |`, `function f(|`, a declaration's `<|`, `catch (|`, an enum body after `{`/`,`,
  `[|` of an array pattern, `import |`, `{ a as | }`) nothing is offered; a constructor's
  parameter list offers only the parameter-property modifiers. At the start of an existing
  name the token before it decides, and a JSDoc comment in between lifts the block.
- Scope anchor (`complete.rs`): an empty word completes from the node the token before it
  ends (`(p) => |` at the end of the file is inside the arrow), except after `}`/`)`/`;`/`]`
  where it is the innermost node holding the position strictly (`f() {}|` is outside `f`);
  a body the parser closed at the end of the file holds the position after it.
- Parameters: inside a parameter's type or default the parameter itself and the later ones
  are not offered (`f(a, b = |, c)` offers `a`).
- Members: `o.|` and `o?.|` on a possibly undefined value list the defined value's members;
  a member whose name is not an identifier is offered as `["a b"]`, its edit replacing the
  dot; after `?.` private names are not offered.

## Quick info (`tsr/features/quick_info.rs`)

Rules from `tsc: tsc/internal/ls/hover.go` and `tsc: tsc/internal/checker/checker.go`
(`instantiateSymbol`), v7.0.2, in our words; the fourslash tests are the oracle.

- The location's meaning (value, type or namespace position; a declaration name is all
  three) picks which of a merged symbol's meanings get a line; when none match, all are
  shown. Each meaning is its own line: `(local) var`, `(property)`, `(accessor)` (also
  `accessor` fields), `(method)`, `function`, `constructor`, `class`/`(local class)`/
  `abstract class`, `interface`, `enum`/`const enum`, `(enum member) E.m = 0`,
  `namespace`/`module`, `(type parameter) T in C<T>`/`in f<T>(x: T): void`/
  `in type A<T>`, `type A<T> = <body>`.
- A type alias's body is its declared type printed with `PrintMode::quick_info_alias`
  (tsc's `InTypeAlias`: the outermost type without its own alias), so `type B = Exclude<A,
  "a">` shows the union it makes and a JSDoc `@typedef`/`@callback` its object or function
  type. A conditional type shows its text as written (tsc keeps one with a generic check
  type deferred; our checker does not yet). A JSDoc alias's documentation is the comment it
  sits in, without the `@property`/`@param`/`@returns` tags that build it.
- An accessor at its setter's name or written through (`o.v = 1`) shows the setter's
  accepted type (`Ck::write_type_of_symbol`); elsewhere the getter's.
- Names inside JSDoc tag types (`@type {Bar}`, `@param {Bar} x`, `@extends {a.B}`) are
  found by `locate::node_at`: a tag's type lies in a comment, outside its host's span, so
  an offset inside one descends from that type (hover, definition, references, rename).
- Functions show every overload (`function f(x: number): void` per line; from five on, the
  first three then `// +N more overloads`); on a declaration's own name, that
  declaration's signature; at a call of an overloaded or generic function, the signature
  the call resolved to (`f<string>(x: string)`); one that does not resolve shows the
  candidate instantiated as signature help does (written type arguments, else inferred,
  `any` in JavaScript: `constructor B<unknown>(): B<unknown>`). A callee variable shows the
  resolved signature as its type. `new C(..)`/`C(..)` of a class with one construct
  signature shows `constructor C<args>(..): C<args>`.
- Member names are `Owner<params or args>.name`, `Owner["lit"]` or `Owner[1]`. A member
  read through an instantiated type shows the owner's type arguments when its symbol was
  instantiated: always for methods (their function type may mention the owner's type
  parameters), for other members only when instantiation changed the printed type
  (`Array<T>.length`); otherwise the declared parameters with constraints and defaults.
- Names gain their enclosing namespaces unless the location is a declaration or inside
  that namespace's body. Through an import alias each line is prefixed `(alias) `, the
  target's declared type is shown (no narrowing), and a target that cannot be named at
  the location (declared in another module or an ambient `declare module "m"`) is shown
  under the alias's name; a module target is `(alias) module m` (`import = require`) or
  `(alias) namespace m`.
- Types and signatures print through the checker's quick-info mode
  (`PrintMode::quick_info(at)`: names scoped to the location, aliases followed, optional
  parameters without the `| undefined` optionality adds); no text post-processing. An
  optional property read keeps `| undefined`; an optional method's signatures come from its
  type without that `undefined` (`remove_optionality`); a property being assigned
  (`o.p = 1`, `o["p"] += 1`, `o.p++`) shows its type without `missing` (the write type under
  `exactOptionalPropertyTypes`); an optional chain's link (`a?.b`) shows the property's own
  type.
- Hovering the `class`, `function`, `interface`, `enum` or `default` keyword of a
  declaration describes the declaration; a string or number literal that names a member
  (`o["a"]`, `{ 1: x }`) describes that member.

## Documentation (`tsr/features/jsdoc.rs`, `doc_source.rs`)

Rules from `tsc: tsc/internal/ls/hover.go` (`getDocumentationFromDeclaration`,
`getJSDocOrTag`, `writeNameLink`) and `tsc: tsc/internal/parser/jsdoc.go` (where a tag
starts), v7.0.2, in our words.

- `jsdoc.rs` parses `/** */` text itself (the parser's JSDoc pass only lowers types): a
  tag starts at any `@` followed by an identifier start, except in a fenced block, an
  inline `` `code` `` span (to the end of its line), `{@link ...}` and `@author`'s
  `<email>`. Markdown: the comment, then per tag except `@type`/`@typedef`/`@callback`
  `\n\n*@tag*`, its name in backticks (`@param`/`@property`, a missing name as an empty
  one; `@template` names comma-joined; `@augments` class) and ` — comment` (just
  ` comment` when it starts with `-`). Types in braces are not shown except `@throws`'s;
  an unclosed `{ {...}` type ends after its inner braces; `@param o.a` after `o` is
  dropped. `@example` is a `tsx` fence (`<caption>` after ` — `). `@see name` and
  `{@link name text}` link to the name's declaration as `file#line,col-line,col` (1-based,
  the name's span, through aliases), a URL as `[text](url)`, or print `name text` when the
  name does not resolve; names resolve in the documented function's body first (its
  parameters).
- `doc_source.rs` picks the one declaration documented (see its module docs): the
  overload or constructor hovered, the single signature a call resolved to, an
  interface's call or construct signature at a call, else the value declaration (or the
  first interface, enum, namespace, type parameter, type alias); a union property declared
  in several places has none. Its own nearest `/**` comment (other comments may sit
  between) wins, else a parameter's `@param` (by name, a binding pattern by position), a
  type parameter's `@template`, the statement of a first declarator, the variable or
  property a function or class expression initializes, a destructured property's
  declaration, the first overload, then the same-named member of the base types.
  `namespace a.b` documents `a` only. A property read through an index signature shows
  that signature's comment (the first applicable non-`string` index, else the `string`
  one). A comment with `@typedef`/`@callback` documents that type, not what follows it.

## Keywords and navigation (`tsr/features/keywords.rs`, `nav.rs`)

Syntax-only rules for the keywords tsc 7 answers without a symbol. Learned from the fourslash
tests and from reading `src/services/documentHighlights.ts`, `src/services/findAllReferences.ts`
and `tsc/internal/ls/definition.go` (tsc 7.0.2); written here in our own terms on our AST.

- **Highlights on a keyword** (`keyword_highlights`, tried before symbol highlights): an
  `if`/`else` chain (an `else if` on one line is one span), a function's `return`s and the
  `throw`s it owns (a `throw` in a `try` with a `catch` belongs to the `try`), `try`/`finally`
  (not `catch`), a `switch` with its `case`/`default` and its own `break`s, a loop with its own
  `break`/`continue` (labels followed), `constructor`, a `get`/`set` pair, `async`/`await`,
  `yield`, and every occurrence of a modifier among the declarations around (none for
  `default`, `in`, `out`). Loops use an explicit stack; walks stop at `MAX_DEPTH`.
- **References without a symbol** (`special_references`): `this` (every `this` of the same
  container; in a static member, every static member's `this` of that class), `super` (same
  class, same static-ness), labels (the label is the declaration), primitive type keywords
  (`string` across the project's own files). References of a class name also list the `this`
  of its static members; rename skips those unless the cursor is on `this`.
- **Go to definition on a keyword** (`keyword_definition`): `return`/`await`/`yield` go to the
  innermost function or method (its name, a method's key, the variable an anonymous function
  initializes, else the whole function); `case`/`default` to their `switch` keyword;
  `export default <expr>`'s `default` to the whole statement; `break l`/`continue l` to the
  label (nothing on the label itself); `this` to the function's `this` parameter, else the
  class name in a class member, else nothing; `super` to the base class, plus for
  `super(...)` the whole constructor it calls; `override` to the base member (instance: base
  types; static: the base constructor type), else its own name; any other modifier and the
  `function`/`class`/`interface`/`enum`/`type`/`namespace`/`const`/`let`/`var` keyword to the
  definition of the declared name. A variable statement's modifier goes nowhere.
  Find-all-references on such a keyword (`keyword_name`) lists the declared name's
  references; document highlights on it do not move to the name (`const` alone has none).
- **Property names in destructuring and literals** (`locate.rs`): a key of an object literal
  used as an assignment target (`({ a: x } = o)`, `for ({ a } of xs)`, `[{ a }] = xs`,
  nested) means the property of the type it takes apart, as does a binding key in
  `for (const { a } of xs)` (the iterated type). A member of a contextually typed object
  literal also means the contextual type's property (`Meaning::contextual`): references and
  rename treat the two as one; an untyped literal stands alone.
- **Literal keys** (`literal_key`): a string or numeric literal that names a member
  (`"a": 1`, `get "42"()`, `{ "a": T }`) or indexes one (`o["a"]`, `o[0]`) is a reference
  like a name, spelled by its cooked text (numbers in JS spelling). The reference covers
  the text between the quotes (only when written without escapes) or the whole number.
  Renaming a numeric index quotes the new name (`o["z"]`); a numeric key stays bare.
  References on `get`/`set` are those of the accessor's key.
- **Type definition** follows tsc's symbol-of-type rule: the declarations of each union
  member's symbol (not the alias: `Pick<T, K>` goes to its mapped type), the first type
  argument of a well-known global generic first (`Array`, `Promise`, `Map`, `Pick`, ... the
  list in `known_type_argument`), a function's own inferred type (or the function that
  initializes the variable) replaced by its single signature's return type, and a type-only
  symbol whose type has no declaration going to itself. Keywords before a name stand for
  the name.
- **Go to definition on names** (`nav.rs`, from observed tsc 7 and `tsc:
  tsc/internal/ls/definition.go`):
  - *Calls.* When the name is the callee of a call, `new` or tagged template (directly or
    as `o.name`) and the resolved signature has a declaration that is function-like (a
    function, method, arrow, call/construct signature or constructor type; not a function
    type), that declaration is added. If it is the cursor symbol's own (one of its
    declarations, the constructor of its class, or the function its variable, property or
    assignment holds), a function shows only that overload and a constructor shows the
    class declarations (not merged namespaces) plus that constructor; otherwise the
    symbol's declarations plus the signature (`new I()` through a construct signature).
    Through an import the symbol is the alias, so every overload stays.
  - *Spans.* A constructor has no name: the whole declaration is shown. An unnamed
    function, arrow or class expression is shown by the name it is assigned to (`const f =
    () => 0`, `{ m: function () {} }`, `x.y = class {}`).
  - *Object literals and patterns.* A key of an object binding pattern goes to the
    property of the destructured type (each union member's; nothing for an untyped
    pattern). `{ x }` goes to the variable and the contextual property; another member
    name of a contextually typed literal goes only to the contextual property, of the
    union members its unit-valued discriminants are assignable to (all when none is), and,
    when the contextual type was inferred from this literal, of the declared parameter
    type with type parameters replaced by their constraints.
  - *Modules.* A specifier, or the name of `declare module "x"`, that names an ambient
    module goes to its declarations; an import from a shorthand ambient module goes to the
    module. A string name in an import or export specifier goes where the alias resolves.
  - *Fallbacks.* `o.k` with no property goes to the index signatures of `o`'s type that
    apply to `k` (every union member). `typeof this` and a `this` parameter behave like
    the `this` keyword.
- `ident_at` takes the name that ends exactly at the cursor when the cursor is not on a name
  (`a|()`), as tsserver's touching-name lookup does.
- **Inherited members** (`inherit.rs`, from `tsc: tsc/internal/ls/findallreferences.go`,
  `forEachRelatedSymbol`): a non-static class or interface member is one with the
  same-named members its container inherits (base class, `implements`, interface
  `extends`, any depth). Two members are related when the declaration sets reached upward
  from each share one (an override, its base, a sibling override, an implementation). A
  class in its own base chain has no base; interfaces keep theirs.
- **Search meaning** (`ref_meaning.rs`, from `tsc: tsc/internal/ls/utilities.go`): a
  search covers the meanings of the location it starts from (value, type, namespace),
  widened by every declaration sharing one; each candidate must be used in one of them, so
  an interface merged with a namespace and a function is searched as a type only from a
  type position. Rename searches every meaning.
- **Aliases** (`aliases.rs`): a search starts at the cursor's symbol followed through
  transparent aliases (`import { a }`, `export { a }`, a default import except in a
  rename) and stops at the first alias that renames (`{ a as b }`, `import * as ns`,
  `import x = require(..)`, `export default expr`) or at the declared symbol. A name
  matches when the search symbol is on its alias chain (`Checker::link.alias_chain`), and
  the names aliases give the symbol are scanned too (not for a rename, nor through entity
  `import x = N.y`). The imported side of a renaming specifier and the entity of `export
  default a` / `export = a` stand for the specifier's immediate target. From the `default`
  keyword: the keyword, the export's imports and their uses; an anonymous default export's
  keyword is its declaration.
- **Shorthands and private names**: references (not rename) from `{ x }` or `const { x } =
  o` cover the variable and the property; `#x` resolves lexically to the innermost class
  declaring it; computed literal keys (`["a"]`, `` [`a`] ``) and string enum member names
  are literal keys.
- **Constructor references** (`constructor_refs.rs`): find-all-references on a
  `constructor` keyword (or a modifier before it) lists the class's constructor keywords as
  declarations, then the callee name of each `new` of a class the constructor builds (its
  own class and, followed through any depth, derived classes that declare no constructor;
  through imports and qualified names, not through a variable holding the class), `this`
  of `new this()` in those classes' static members, and `super` of `super(...)` calls made
  by constructors (arrows in between allowed, not other functions or methods) of classes
  extending one of them. `implements` does not count.

## Signature help (`tsr/features/signature_site.rs`, `signature.rs`)

Rules from tsc 7's fourslash answers and `tsc: tsc/internal/ls/signaturehelp.go`, the
module docs give them in full:

- Sites: arguments or type arguments of a call, `new`, tagged template (the template is
  argument 0, each substitution the next) or `super(...)`; a `<` list that did not parse as
  type arguments (`f<|`, `let x: C<|`, read backwards over balanced brackets, the `<` after a
  name that is not being declared); a function's or parenthesized expression's parameter
  list whose contextual type has a call signature. The innermost opener wins; an unclosed
  list retries from the node before the cursor.
- Index: top-level commas before the cursor (a spread tuple counts its fixed elements);
  count: commas plus one, zero for an empty list.
- Candidates: the callee's signatures; the resolved one (only when it really applies:
  arity, assignable arguments, type argument count) replaces its declaration and is
  active. Otherwise the first candidate with a rest parameter or enough parameters, else the
  longest, instantiated from written type arguments (default, constraint, `unknown`; `any`
  in JS) or from inference over the written arguments.
- Rest tuples expand into parameters (a union of tuples into one signature each); unnamed
  elements take `name_i`, duplicates `_1`, `_2`. Labels print in quick-info mode, string
  literal annotations as written, `?` parameters without the added `undefined`.
- A private or protected constructor used outside its class gets no help.

## Sound mode (M6, `tsr/project/sound.rs`)

Design record [0007](../../docs/design/0007-sound-mode.md), "Reporting". All of it lives in
`project/sound.rs` (a child module of `project.rs`, so it reads the project's state); the
hooks elsewhere are one line each: `Project::sound` (state), `sound_check` in
`check_and_publish`, `sound_decorate` in `publish`, `sound_request` at the top of `answer`,
`sound.invalidate()` on `PMsg::Config`, and `ts-r.toml` (and every `.json`, so `tsrconfig.json`) counting as a config change in
`Server::on_changes`.

- **Which files.** A file's level is its `// @ts-r <level>` pragma, else the project's
  `[sound]` configuration (`ts_r_sound::config::discover` on the project's tsconfig: its
  `"ts-r"` key, else the nearest `tsrconfig.json` or `ts-r.toml`, more than one being TSR0020; the inferred project looks from the workspace root),
  else off. Read once per project and again after a config change (`.json` or `ts-r.toml`
  in `workspace/didChangeWatchedFiles`). Configuration errors go to the daemon log.
- **When.** Right after a file's regular check, before publishing: an `off` file costs one
  pragma read. Otherwise `ts_r_sound::check_sound_files` runs the pass for that file only,
  with the file's semantic diagnostics as the base, and its `TSR` diagnostics join the
  semantic ones in the same `publishDiagnostics` (so they are remapped through edits and
  replaced with the next check like the rest).
- **Forced checkers on levels.** Each forced program (same files, forced options) is passed to
  `ts_r_history::arrange` with the project's hot files before its checker is built, so its lib,
  dependency and stable levels are built once per option set and then found in the registry
  (the levels are kept in `SoundState` between passes). If a forced program would be ordered
  differently (never seen), the files are put back and the pass runs without levels (logged).
- **Wire form.** `source: "ts-r sound"`, `code: "TSR0201"` (numeric `100201` under
  `code-prefix = "TS"`), `codeDescription.href` = `DOCS_BASE` + `rules/S-02/`
  (`https://webb-ventures.github.io/tsc-rust/sound/rules/S-02/`; general `TSR00xx` codes link
  to `.../sound/`). The pages are the site's job (record: a page per rule).
- **Quick fixes** (`textDocument/codeAction`, computed from the request's
  `context.diagnostics` with our source): every rule: "Add '@ts-r-unsafe S-xx' with a reason"
  (inserts `// @ts-r-unsafe S-xx: ` above the line with its indentation; the empty reason is
  TSR0001 until written, so a hatch can't be added without one); TSR0002: remove the unused
  directive; TSR0201 / TSR1401: `x as T`, `<T>x` and `const v: T = source` become
  `parse<T>(...)`, offered only when `ts_r_guard` can validate `T`, adding or extending the
  `@ts-r.dev/guard` import (before the first statement, so a pragma stays first); TSR0301: `x!`
  becomes `expect(x, "x is set")`; TSR1001: `await` (in async functions) or
  `void x.catch(console.error)`; TSR1002: `.catch(console.error)` under the `void`; TSR1302:
  the `@throws {T}` tags the message lists, added to the declaration's JSDoc or a new one.
  The ts-r backend used to fail `codeAction` (which also dropped lint's actions); it now
  answers with these, and lint's are appended by the daemon as for other backends.
- **`ts-r/soundLevel`** `{textDocument}` -> `{level, config, passes, lastMs}`: the file's
  level for the status bar.
- Projects on the fallback TypeScript (parity mismatch) publish no ts-r diagnostics, sound
  ones included.

Measured with `editors/vscode/bench/editor-latency.js --sound sound` (the pragma is prepended
to the opened text in memory; a private monorepo, read-only (historical; public-repo runs are in bench/README.md), parity off, macOS arm64, release,
100 keystrokes 150 ms apart, `scripts/limit.sh`):

| File | sound | keystroke p50 / p95 | first diagnostics | RSS | sound diagnostics |
|---|---|---|---|---|---|
| `packages/providers/src/payments/stripe.ts` (17.6 KB) | off | 5.3 / 6.5 ms | 22 ms | 32 MB | 0 |
| | sound | 15.8 / 19.8 ms | 44 ms | 47 MB | 24 |
| `apps/web/.../billing/card-form.tsx` (4.3 KB) | off | 3.1 / 4.5 ms | 52 ms | 48 MB | 0 |
| | sound | 7.3 / 18.1 ms | 150 ms | 89 MB | 1 |

The pass checks the file again with the forced checker and, while the checker workaround in
`ts_r_sound` is in place (`exactOptionalPropertyTypes` narrowing), once more without that
flag; the extra RSS is the forced option sets' lib levels.

Not done: the VS Code bridge (`editors/vscode/standin/convert.js`, `splitDiagnostics`) skips
diagnostics whose source is not `ts-r`, so VS Code windows on the bridge don't show sound
diagnostics yet (plain LSP clients do); no status-bar item reads `ts-r/soundLevel` yet; the
"sound mode is unavailable on the fallback backend" message; quick fixes for S-04, S-05,
S-07, S-08 and S-09 (record "Migration"); the hatch fix inserts plain text (no snippet
placeholder for the reason); `ts-r/fileDiagnostics` (pull) has no sound diagnostics.

## Type-aware lint (M4, `tsr/project/lint.rs`)

- With the ts-r backend, every project thread runs the **type-aware half** of an owned
  file's lint rules (`FileConfig::typed_only`: rules with `requires_types`, no unused-directive
  reports) right after checking it, on the same checker worker
  (`ts_r_lint::typed::lint_program_file`), so the rules reuse the check's types. The
  results (`source: "ts-r-lint"`, rule as `code`) join the semantic diagnostics in the
  project's `publishDiagnostics` and are remapped through edits like them. The daemon's
  lint worker runs the other half without types; the daemon merges both as before.
- Configs: `lint::TypedConfigs` (an `Engine` with `types_available`, per-path cache), reset
  when a lint config file changes on disk or the project's tsconfig changes, and every open
  file is re-linted. The first file of a config publishes its type diagnostics before
  loading the config (an ESLint config is evaluated by Node), then again with lint results.
- Projects on the fallback TypeScript (parity mismatch) publish nothing of ours, so no
  type-aware lint either (as `ts-r-lint` skips type-aware rules for such projects).
- The lint switch: with `ts-r.lint.enable` off the daemon drops `source: "ts-r-lint"`
  diagnostics and code actions from the backend. The "rules not run" note leaves out the
  type-aware rules when the ts-r backend is the type backend.
- Code actions: the backend answers `textDocument/codeAction` with the type-aware rules'
  fixes, suggestions and disable comments, only once the file is checked after its last
  edit (an edit moves the published spans, not the fixes' edits); `source.fixAll.ts-r` stays
  with the lint worker (it does not include type-aware fixes yet).
- `TS_R_LSP_TYPED_LINT=0` turns it off. The first run per file is logged with its rule
  count, problems and time.
- Measured (`editors/vscode/bench/editor-latency.js`, a private monorepo's copy (historical; public-repo runs are in bench/README.md), project ESLint
  configs with 20 type-aware rules, parity off, release, macOS arm64; typed lint off / on):
  `packages/providers/src/payments/stripe.ts` keystroke p50 / p95 6.05 / 7.35 -> 6.78 /
  8.09 ms, one lint 2.4-3.0 ms (13 problems), RSS 31.7 -> 34.4 MB; `card-form.tsx` 2.79 /
  3.72 -> 3.31 / 4.10 ms, 2.4 ms per lint, RSS 47.6 -> 50.6 MB. Gap: loading the config
  (~0.3-0.5 s of Node on a cold `lint-config` cache) runs on the project thread, so a
  request arriving then waits (hover p95 in those runs 325-525 ms).

## Releasing closed files (M5 history, `tsr/project/release.rs`)

- A session's checker keeps the results of every file it checked (types and per-symbol state,
  function bodies included) until the session ends, and closing files does not end it. So a
  close starts a quiet timer (`TS_R_LSP_RELEASE_MS`, default 2000 ms, restarted by every close);
  when it runs out with nothing queued, the project ends its session. With open files left,
  the next session starts on a fresh checker and a rebuilt stable level (the shared lib and
  dependency levels are kept alive across the rebuild). With none left, the program, levels,
  shapes and indexes are dropped and the thread waits, tracking document texts, closes,
  options and fallback, until an open or a request reloads it. Published diagnostics of open
  files stay; requests check their file again on demand (`ensure_ck`).
- Freed memory sits between live allocations of the same size, so the RSS would not move:
  500 ms after the last release of any project, one helper thread asks the allocator to
  return free pages (`malloc_zone_pressure_relief` on macOS, `malloc_trim` on glibc). These
  two calls are the crate's only `unsafe`, allowed locally with `// SAFETY:` comments.
- Hooks in `project.rs` are one line each (`note_close` in `on_own`, the idle session's wait
  and its empty-queue branch, `release` at the top of the thread loop). The sound pass's kept
  levels are not released yet.
- Measured (a private monorepo (historical; public-repo runs are in bench/README.md), release, macOS arm64; a scripted LSP client like
  `editors/vscode/bench/editor-latency.js --open-all` that then closes all but 10 sources
  spread over the list and reads `vmmap -summary`; parity off): all 649 sources open 405-420 MB (RSS and physical footprint alike); then closing
  all but 10, physical footprint 342-353 MB before, 211-222 MB after (`ps` RSS 400 -> 375 MB:
  macOS keeps returned pages in the RSS until it needs them); a fresh daemon with the same 10
  open: 130-140 MB. Reopening a file of a released project: kernel 2 ms, infra-project (1559 files)
  173 ms to its hover.

## Editor actions (L4, `tsr/features/actions/`)

What tsserver does in the editor beyond diagnostics and navigation, on our checker. Behaviour
comes from running tsserver 6.0.3 on fixtures (`tests/fixtures/actions/*.txt`, recorded by
`tests/oracle/actions.js` into `<name>.tsserver`), never from its source. Every rule observed
is written at the top of the module that implements it.

- **Plumbing.** `textDocument/codeAction` on the ts-r backend: `features::handle` →
  `actions::code_actions` (byte offsets throughout; `Action::to_lsp` converts edits to UTF-16
  ranges at the edge). The answer is our actions, then sound mode's, then lint's (appended by
  `project.rs::answer` and the daemon). Kinds advertised: `actions::KINDS`. Context
  diagnostics with `source` `ts-r`, or `ts` (fallback projects: tsserver's diagnostics, our
  edits), select quick fixes; fix-alls run over the worker's diagnostics of the file. Source
  actions come only when `context.only` asks (as tsserver's clients do).
- **Keystrokes stay free.** Nothing runs before a request. A request that only asks for
  organize kinds (what editors send on save) skips the file check (`actions::organize_only`).
- **Organize imports** (`organize.rs`, `source.organizeImports`, `source.removeUnusedImports`,
  `source.sortImports`): groups, merging, sorting, unused removal, comments, JSX factories,
  detection of the file's case and `type` order from its import lists (fewest neighbours out
  of order); export lists always sort ordinally with `type` specifiers last, and a reprinted
  multi-line export list breaks lines only between specifiers that were neighbours on
  separate lines (tsserver's packed layout). Semicolons as the file uses them
  (`style::uses_semicolons`: the first five statements or interface/type-literal/class members
  at any depth, so `{ a: T; b: U }` early on means semicolons), trailing commas kept in imports
  that were only reordered.
- **Quick fixes** (`fixes.rs`; table of codes, titles and fix ids at its top): add or update an
  import (`imports.rs`: bare specifiers from the program's resolutions, `paths` aliases,
  relative paths, sorted insertion, quotes/semicolons/`.js` of the file, header comments),
  spelling, missing `await` (and on a `const`'s initializer), `async` on the containing
  function (with `Promise<T>`), implement interface and add missing properties
  (`members.rs`), unused declarations (`unused.rs`: delete, remove import, prefix with `_`),
  a missing function declaration for an undeclared callee (parameters named and typed after
  the arguments, return type from the call's context, appended at the end of the file).
  A diagnostic sent with an empty message (the VS Code stand-in sends only codes) takes the
  message of the file's diagnostic with that code at that span. Fix-alls where tsserver has them (two or more diagnostics with the fix); source actions
  `source.addMissingImports.ts` and `source.removeUnused.ts`.
- **Refactors** (`refactor.rs`; table at its top), offered with `data.refactor` =
  tsserver's `<refactor>/<action>` names, and listed in tsserver's menu order
  (`refactor::menu_rank`: convert export, convert import, extract type, inline variable, move
  to file, move to a new file, braces, function conversions, destructured parameters,
  template string, optional chain, accessors, overloads, extract symbol, infer return type).
  Every refactor tsserver 6.0.3 lists on the fixtures is there: infer function return type;
  convert named imports to a namespace import and back, and to a default import (modules
  written with `export =`); extract to constant (every function scope, expression-bodied
  arrows included, the module scope, and class members as `readonly` fields; scope numbers
  as tsserver numbers them) and extract to function (module scope, every enclosing function
  as an inner function, and enclosing classes as `private` methods; parameters for the
  enclosing function's locals in order of first use, typed as used, type parameters carried
  with the constraints they need; variables used after the range returned, destructured when
  several; `async`/`await` when the range awaits); move to a new file and move to file
  (`Action::creates`: imports copied or added on both sides, `export` added where needed,
  importers in other files retargeted, relative or through the shorter `paths` alias).
  A refactor is not offered when its result could be wrong: selections outside expression
  positions (types, patterns, specifiers, property names, a tagged template's quasi),
  `this`/`return`/`yield`/jumps leaving the range, writes to outer locals, captured
  narrowings the extracted code would lose (`narrowed_capture`, `rewrite::narrowed_access`,
  `rewrite::extract_hazard`), `this` in nested functions of a class scope, overload groups
  and merged declarations split by the range, namespace importers or `export *` of a moved
  file, importers through a package specifier. JavaScript files get no type annotations, no
  infer return type and no class-member extraction.
- **Move to file.** VS Code's flow: the menu (`includeInteractiveActions`, our
  `tsr.interactive`) shows an interactive "Move to file" without edits; the client asks
  `ts-r/moveToFileSuggestions` (`actions::move_suggestions`: `newFileName` as Move to a new
  file would name it, `files` = the program's non-lib, non-declaration, non-`node_modules`
  files with a compatible extension, a directory's files before its subdirectories), then
  resolves the action with `tsr.targetFile`. Into an existing file the moved statements are
  appended and its imports merged.
- **Function refactors** (`functions.rs`; rules at its top): add/remove braces in an arrow
  function, convert to anonymous/named/arrow function, convert parameters to destructured
  object, convert overload list to single signature. Destructuring rewrites every call in
  the program and is not offered when a use is not a plain call (`f.call`, `typeof f`, JSX, a
  value use, a spread, extra arguments) or when a method could be overridden. Convert to
  named function is not offered when a `let`/`var` is assigned again. Moved code keeps its
  source text, except one-line type literals, reprinted as tsserver prints them (`emit`,
  `print_type`). Detection is syntactic; the checker runs only when the action is built.
- **Expression refactors** (`rewrite.rs`; rules at its top): convert to template string,
  convert to optional chain expression, inline variable, generate 'get' and 'set' accessors.
  Syntax only, except one checker query for operand types when inlining to the right of
  `+`. Not offered when the result could change meaning: shadowed names, `this`/`await`
  moving into another function, side effects duplicated or reordered, a line that would join
  the one before it, contextually typed literals. The module also has `extract_hazard`
  (short-circuit and conditional branches and loops for constants; narrowing conditions) and
  `narrowed_access` (property accesses narrowed in place), used by extraction to decline.
- **Convert export / extract type** (`exports.rs`, `extract_type.rs`; rules at their tops).
  Positions use `exports::token_start` / `prev_token_end` (a cursor in a token's leading
  trivia belongs to the token); on a cursor move these need a selection (convert import: the
  whole statement). Convert export rewrites every importer it can see and declines rather
  than leave one unconverted. Extract type reprints the type with its own printer
  (`extract_type::Printer`): when the AST gains a node kind usable as a type, add it to
  `is_type_kind` and `Printer::ty`, or the refactor silently isn't offered for it. It is
  inserted before the statement (before an overload group's first declaration).
- Negative fixtures compare as "// not offered" (the list of other refactors offered there
  is dropped on both sides), so adding a refactor does not break them.
- **File renames** (`rename.rs`): `workspace/willRenameFiles` (capability
  `workspace.fileOperations.willRename`, files and folders). The coordinator asks every
  loaded project, and the projects owning the renamed files, for its edits; it adds the edits
  for those projects' tsconfigs and answers with their merge (`rename::merge`: an edit
  already taken, or one overlapping it, is dropped, so a file in two projects is edited
  once). tsserver also answers only for loaded projects. Relative specifiers that resolve to
  a moved file, and those inside moved files, are recomputed keeping their style (extension,
  `.js` for TypeScript files, explicit or implicit `/index`). `paths`-alias and package
  specifiers to a moved file are recomputed: first matching alias in `paths` order (against
  `baseUrl` when set, as tsserver 6 does), else the same package (`exports` keys when it has
  them), else relative. tsconfig `files`/`include`/`exclude`, `paths` values and path-typed
  options that name a moved path follow it. The answer is the same whether the program still
  has the files at their old paths or already sees them at the new ones; after the move,
  importers in projects that never had the moved files find them on disk (`FileSystem`).
- Edits at one offset: `lsp_edits` joins an insertion with the replacement starting there, so
  clients can't reorder them (tsserver's `export import { x }` when both land at offset 0).
- Needed from the front end: TS1308 (`await` in a function that is not async) — the parser
  now parses such an `await` as tsc does and the checker reports it (see their AGENTS.md).

Measured (private-app, release, macOS arm64, parity off):
- Fixtures (L4 round 2): 418 action fixtures (organize, quick fixes, refactors, move
  suggestions, file renames), all recorded with one oracle setup (tsserver 6.0.3, titles,
  VS Code's `allowTextChangesInNewFiles`); 389 equal to tsserver's output, 29 marked
  `@differs` with our output in `.expected`: tsserver results that do not parse or change
  meaning (we decline or fix them; listed in each module's header), tsserver's reformatting
  of moved code, `fix-implement-all` (empty-class layout), `refactor-move-new-file-local`
  (`export import { scale }`). Round 1 had 78 fixtures, 76 equal. Rename fixtures: 21 of 21.
- Sweeps: `tests/actions.rs` runs refactors at every offset, node and line of every fixture
  and the quick fixes of every diagnostic (79.5k actions): no panics, no overlapping edits,
  no new syntax errors; with `TS_R_ACTIONS_DEEP=1` the 8,395 distinct results type-check
  with no new errors. `examples/actions_sweep` on private-app (25 projects, up to 80
  files each, 611 files, 632k requests, 892k actions): 0 panics, 0 results with new syntax
  errors; per request p50 0.007-0.9 ms, p99 under 1.5 ms, max 41 ms.
- Organize imports on every source file but `infra/` (`tests/oracle/organize-corpus.js`, 532
  files, 75 changed by tsserver): 532 equal to tsserver (round 1: 520; the 12 were export
  lists' ordinal sort and packed layout, and tsserver's semicolon vote counting type
  members). Per request p50 / p95: ours 1.0 / 28 ms, tsserver 0.6 / 1.8 ms (ours includes
  queueing behind the just-opened file's check).
- `editors/vscode/bench/editor-latency.js` before / after L4: keystroke to diagnostics p50
  card-form.tsx 2.78 / 2.75 ms, stripe.ts 5.89 / 5.87-6.05 ms (noise); code actions at 40
  identifiers p50 0.07 / 0.08 ms and 0.09 / 0.08-0.09 ms, p95 0.15 / 0.32 ms and 0.13 / 0.27
  ms (0 / 7 and 0 / 4 positions now get actions: refactors); organize imports p50 0.06 / 0.09
  ms and 0.10 / 0.12-0.14 ms; quick fixes on a just-typed TS2304 0.32 / 0.49 ms and 0.36 /
  0.45-0.56 ms.
- Same bench, round 2 (base = round 1's binary / now, 60 keystrokes, 40 positions):
  keystroke to diagnostics p50 / p95 card-form.tsx 3.20-3.27 / 3.91-5.14 ms vs 3.19-3.32 /
  3.91-3.93 ms, stripe.ts 2.63 / 3.34 vs 2.61 / 3.25 ms; code actions p50 0.08 / 0.07-0.09
  ms and 0.07 / 0.06 ms, p95 0.26 / 0.58-0.77 ms and 0.17 / 0.18 ms (positions with actions 7
  / 12 and 8 / 11: the new refactors); organize imports p50 0.08 / 0.09 and 0.09 / 0.06 ms.
- Gaps (round 2):
  - Not offered where tsserver offers them: Extract to typedef (JavaScript; the AST has no
    JSDoc nodes, so VS Code's JS refactor menu stays on TypeScript); Extract Symbol at a bare
    cursor when invoked (ours needs a selection); destructured parameters for methods with a
    base class or subclasses, object-literal methods, `let`/annotated-`const` arrows and
    non-TS files; overloads with type parameters or differing return types; accessors for
    string-named or decorated fields, fields sharing a line, static fields of anonymous
    classes, contextually typed object literals; template string with a comment after the
    chain; inline variable for function/class expression initializers and annotated
    declarations with object/array/function literals (a side-effecting initializer inlines
    only into the next statement); extract type with comments inside; extraction whose
    types print truncated (`...`) or with `infer` (the checker's printer writes `infer U[]`
    without parentheses).
  - Formatting: tsserver reformats code it moves (operator and comma spacing, `function(`,
    multi-line type literals, call arguments); we keep the source text.
  - Move to a new file is offered at some cursor positions where tsserver is not.
  - Convert export does not rewrite importers through re-exports (tsserver neither).
  - Renames: importers in projects that are not loaded are not edited (as tsserver); after
    the move a package specifier is found again only through `node_modules` above the
    importer; `references` and package.json `exports` are never rewritten; our resolver
    ignores `baseUrl` for `paths` (the rename code applies it itself).
  - Checker bugs the fixtures work around: TS2339 on `C.v` in a static block of
    `export default class C`; `this`-typed members of an extracted class method flagged.

## Dependencies

`serde_json` (message bodies; the transport itself is hand-written), `blake3` (endpoint ids, as
in `ts_r_cache`), `ts_r_cache` (cache root), `ts_r_lint` (lint engine, public API only; it turns
on serde_json's `preserve_order` for the whole binary), `ts_r_parser` + `ts_r_ast` (where a
disable comment may go in JSX), `interprocess` (Windows named pipes only).
L3: `ts_r_ast`, `ts_r_binder`, `ts_r_checker`, `ts_r_types`, `ts_r_decls`, `ts_r_config`,
`ts_r_resolver`, `ts_r_diagnostics`, `ts_r_span` (public APIs only). M6: `ts_r_sound` (the
sound pass and configuration), `ts_r_guard` (whether a `parse<T>` quick fix can validate `T`).
`notify` (native file watching of the ts-r backend's project inputs: FSEvents / inotify /
ReadDirectoryChangesW is real platform work; the approved crate, same version and features as
`ts_r_hmr`, `ts_r_build`, `ts_r_run`; `tsr/watch.rs` follows `ts_r_hmr`'s debounce patterns
without depending on the dev server crate).
Dev: `ts_r_testing` (memory cap for tests/examples on real projects), `ts_r_parser` (the
action sweeps re-parse results).

## Testing

```sh
cargo test -p ts_r_lsp                      # unit + tests/daemon.rs, tests/lint.rs (fake backends, oxlint configs), tests/tsr.rs (ts-r backend), tests/release.rs (closing files), tests/actions.rs (L4 fixtures)
TS_R_LSP_TEST_TS6=<node_modules with typescript 5/6> \
TS_R_LSP_TEST_TS7=<node_modules with typescript 7>   \
  scripts/limit.sh -- cargo test -p ts_r_lsp --test real_typescript
scripts/limit.sh -- cargo run --release -p ts_r_lsp --example two_windows -- <repo>
# parity with tsc 7 --lsp on TypeScript's fourslash tests (bench/lsp-oracle/README.md)
scripts/limit.sh --time-s 1500 -- node bench/lsp-oracle/run.mjs --jobs 4
# L4: record tsserver's answers for the action fixtures (TypeScript 6 from private-app)
scripts/limit.sh -- node crates/ts_r_lsp/tests/oracle/actions.js <typescript/lib dir> [names]
# organize imports on a whole project, tsserver vs ts-r
TS_R_LSP_PARITY=off scripts/limit.sh -- node crates/ts_r_lsp/tests/oracle/organize-corpus.js \
  target/release/ts-r-lsp <typescript/lib dir> <root> [--from N] [--max N] [--show N]
```

`tests/daemon.rs` waits on events (a message, a status predicate) with a 30 s deadline, never
on a fixed sleep before an assertion: messages from two windows reach the daemon in no
defined order, and a loaded CI runner can take seconds to start a daemon. The front retries
connecting for 20 s (an old daemon may hold its lock that long while it exits).

Action fixtures (`tests/fixtures/actions/<name>.txt`, verbs and directives documented at the
top of `tests/actions.rs`): verbs `refactor`, `fix`, `fixAll`, `organizeImports`, `rename`,
`moveSuggestions`; directives include `@format`, `@preferences` (tsserver user
preferences), `@target` (Move to file), `@open: <files>` (only these are open, so only their
projects answer a rename), `@symlinks: <link> -> <target>, ...`; a fixture with its own
`*/tsconfig.json` files is one project per tsconfig.

`tests/actions.rs` env: `TS_R_ACTIONS_ONLY=<prefix>` (run some fixtures),
`TS_R_ACTIONS_PRINT=1` (print every result), `TS_R_UPDATE_SNAPSHOTS=1` (write `.expected` of
`@differs` fixtures), `TS_R_ACTIONS_DEEP=1` (type-check every distinct sweep result; use
`--release`). Real projects:
`scripts/limit.sh -- cargo run --release -p ts_r_lsp --example actions_sweep -- [--max-files N] <tsconfig.json>...`
(`TS_R_SWEEP_SHOW=1` prints the first syntax error of a broken result).

Test-only env: `TS_R_LSP_TSSERVER_CMD` / `TS_R_LSP_NATIVE_CMD` (JSON argv replacing the
backend program), `TS_R_LSP_NODE`, `TS_R_LSP_SAMPLE_MS`, `TS_R_LSP_RESTART_IDLE_MS`,
`TS_R_LSP_LINGER_MS`, `TS_R_LSP_IDLE_SHUTDOWN_MS`, `TS_R_LSP_RELEASE_MS`, `TS_R_LSP_LINT_DEBOUNCE_MS`, `TS_R_LSP_WATCH` (`off`: no input watcher), `TS_R_FAKE_ALLOC_MB`,
`TS_R_CACHE_DIR`; lint also honours `TS_R_NODE`, `TS_R_LINT_NODE_WRAPPER`, `TS_R_LINT_NO_CONFIG_CACHE`.
Each daemon test uses its own cache dir, so its own daemon; `Drop` stops it.

Measured (a private monorepo (historical; public-repo runs are in bench/README.md), TS 6.0.3, two windows, 2 files each, macOS arm64, release):
ts-r-lsp 318 MB total (2 fronts 3 MB each + daemon and tsserver 313 MB) vs two plain tsservers
561 MB vs VS Code's default per-window setup (semantic + ATA + syntax server) 1180 MB. First
diagnostics: cold window ~420 ms (plain tsserver ~370 ms), second window ~140 ms (plain ~330 ms).

Lint, measured with `editors/vscode/bench/lint-keystrokes.js` (scripted LSP client, 300
keystrokes 100 ms apart into a comment of the largest linted file of a private monorepo (historical; public-repo runs are in bench/README.md), 20 KB; project configs; macOS arm64,
release): didChange to publishDiagnostics p50 15.7 ms / p95 16.0 ms with the default 10 ms
debounce (0.8 / 1.7 ms with debounce 0; one lint 0.6 ms). Daemon RSS lint off / on: 3.0 /
6.5 MB with that file open, 6.9 / 19.1 MB with all 557 sources open. The ESLint extension's
server (3.0.34, headless, pull diagnostics, same configs including 41 type-aware rules ts-r
skips): p50 47.1 / p95 58.2 ms, 874 MB (one file) and 1405 MB (557 files).

ts-r backend (L3), measured with `editors/vscode/bench/editor-latency.js` (scripted LSP client;
a private monorepo (historical; public-repo runs are in bench/README.md), macOS arm64, release; parity off unless noted) against tsserver 6.0.3 (the
project's TypeScript) and TS 7's native LSP (`tsc --lsp`, 7.0.2):

| | ts-r | tsserver 6.0.3 | TS 7 LSP |
|---|---|---|---|
| Largest file `packages/api-client/src/schema.gen.ts` (168 KB): first diagnostics | 17 ms | 218 ms | 34 ms |
| keystroke to diagnostics p50 / p95 (100 keys, 150 ms apart) | 4.7 / 5.4 ms | 10.5 / 16.1 ms | 4.4 / 6.9 ms |
| hover p50 / p95 | 0.21 / 0.45 ms | 0.42 / 3.49 ms | 0.14 / 0.70 ms |
| definition p50 / p95 | 0.27 / 0.52 ms | 0.22 / 0.65 ms | 0.10 / 0.20 ms |
| RSS, that file open | 43 MB | 442 MB | 108 MB |
| RSS, all 649 sources open (24 projects, 15 s settle) | 386 MB | 1132 MB | 578 MB |
| open all 649 to diagnostics on each | 0.33 s | 4.4 s | 1.3 s |
| `apps/web/.../card-form.tsx` (TSX, React): first diagnostics | 47 ms | 270 ms | 27 ms |
| keystroke p50 / p95 | 2.8 / 3.5 ms | 9.9 / 12.9 ms | 4.3 / 6.5 ms |
| hover / definition / completion p50 | 0.10 / 0.09 / 0.07 ms | 0.33 / 0.16 / 0.28 ms | 0.13 / 0.09 / 0.18 ms |
| completion p95 | 1.19 ms | 7.35 ms | 2.69 ms |

Before M5 history (levels), every edit re-checked on a fresh `Checker`: card-form keystrokes
were 25.7 / 27.8 ms (24 ms of instantiation and union work for React's types, redone per
edit), schema.gen.ts 6.6 / 16.7 ms, and all-open RSS 580-587 MB. With shared lib and stable
levels the card-form check per keystroke is 0.4 ms (`ts_r_history`'s `probe` example: parse
0.06, arrange 0.6, link 0.6, check 0.4 ms). Every feature on all 650 files (`examples/tsr_probe` style probe): 0
errors, 0 panics; means hover 0.11 ms, references 0.44 ms, completion 2.6 ms, signature help
0.13 ms.

Parity (K-17) on private-app against tsc 6.0.3: 2 of 24 projects match
(`packages/contracts`, `packages/testing`); 22 fall back to tsserver for diagnostics. Of tsc's
3844 diagnostics, 3530 have the same code and position in ts-r (3641 total); 314 are only
tsc's (TS7031 implicit-any binding elements 136, TS7006 47, TS2339 43, TS2741 35, ...), 111
only ours (TS2345 23, TS2339 21, TS7006 14, TS2591 12, ...). The 24 checks took 14 s in the
background (tsc 12.8 s, ours 0.5 s) and come from the cache afterwards. In fallback,
card-form shows tsserver's 25 diagnostics, keystrokes 11.8 / 16.7 ms (tsserver's speed), and
features stay on ts-r (hover p50 0.12 ms).

## Gaps (L1)

- LSP over tsserver: no TypeScript code actions (lint's are served), signature help, document symbols, formatting, semantic
  tokens, inlay hints, pull diagnostics (TS 7 native has what it has; it is passed through).
- tsserver fronts cannot use a TS 7 backend: they get the fallback tsserver (VS Code's bundled).
- Per-client settings: plugins, locale and TS 7 client capabilities come from the first client.
## Gaps (L3)

- Each edit still links the whole program (0.6 ms on apps/web) and arranges levels (0.6 ms).
  The first edit in a new file rebuilds the stable level (its types are recomputed lazily,
  like a cold check). An import change reloads the whole program (worst project,
  infra-project with 1559 files: 158 ms) and rebuilds the project's levels; lib and
  dependency levels are found again by content.
- Features (the fourslash oracle, 1967 of 2516 tests at 5628954, lists the rest by cause:
  bench/lsp-oracle/README.md): hover has no `@property`-name hovers in JSDoc and no keyword
  or meta-property hovers (`arguments`, `import.meta`, `new`); it
  prints names in signatures unqualified (`Iterator<T>` where tsc writes
  `Underscore.Iterator<T>`) and does not name a type through the alias it was imported by
  (`a<string>` for `import a = require(..)`); JSX intrinsic attributes (`<form onSubmit>`)
  have no symbol (the checker does not type JSX yet); references do not go back up from a
  re-export to the declaration the way tsc does from some imports (`import boom from
  "./d"` where `d` re-exports a renamed default), nor through JSDoc, `require` or `import()`.
  Completions: no accessibility filtering after `.` (private members are listed),
  auto-imports only from the project's own (non-`node_modules`) modules and never
  `default` exports. Signature help: none in JSX tags, no `... N more ...` truncation, no
  JSDoc `@overload`.
- Input watching is per directory and add-only (directories stay watched after their files
  leave the program); a project inside a directory named `target` is not watched;
  `node_modules` changes (installs) need "Reload Project" or a restart.
- Unopened dependents are not checked (only open files get diagnostics). Projects do not share
  `SourceFile`s, so a file in two projects is parsed twice.
- Parity: 22 of 24 private-app projects fall back today (checker gaps, mostly TS7031 /
  TS7006 implicit any in callbacks and JSX attribute checks). Parity is all-or-nothing per
  project; there is no per-file or per-code fallback. The fallback TypeScript sees only the
  open documents' texts (other files from disk), like any editor.
- VS Code windows reach the ts-r backend through the extension's tsserver bridge (an LSP
  client of this daemon inside the stand-in), not through the tsserver front: the tsserver
  front itself still only serves TypeScript.

- Windows: named pipes via `interprocess` and `tasklist` sampling are written but never compiled or run (no Windows target here).
