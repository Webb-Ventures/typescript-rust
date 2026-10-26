# 0010: Dev server, HMR and `ts-r-dev`

Status: Accepted 2026-10-18 by Riley Webb
Owners: `ts_r_hmr` (server core: HTTP/WebSocket, module graph, HMR, client runtime, watcher,
caches, seams), `ts_r_dev` (the `ts-r-dev` binary: CLI, wiring, React Fast Refresh, Vite plugin
sidecar, `parse<T>` expansion); consumers: `ts_r_transform` (`hmr::TsTransform`),
`ts_r_bundler` (`hmr::DepBundle`, record [0008](0008-bundler.md)), `ts_r_cli` (`ts-r dev`)

This record was written after the code, from the code and the two crates' `AGENTS.md`. It
describes what exists. Behaviour that is still missing is listed under "Known gaps".

## Context

- Milestone M2 replaces `vite dev`. The plan's decision D3: source modules are served as
  unbundled ESM, one module per request, and `node_modules` is pre-bundled. The plan sets a
  hot-reload budget of p50 under 30 ms from save to the browser having the update applied.
- Existing apps and their Vite plugins must keep working. The protocol between the server and
  the browser, and the `import.meta.hot` API, are Vite's (observed from Vite 8.3.4 on the
  same fixtures). Frameworks (React Fast Refresh) and plugins reach the server only through
  that API and through Vite's plugin hooks.
- Later milestones fill in the transform (TS/TSX/JSX), the resolver and dependency
  pre-bundling (M3 bundler). The server must not change when they arrive, so it talks to
  them through traits.
- The server talks to one browser on localhost: a handful of HTTP/1.1 connections and one
  WebSocket.

## Decision

### Crates

`ts_r_hmr` holds every piece of server logic that a test or another embedder could need.
`ts_r_dev` parses arguments, prints startup output, and wires the parts together: the TS
transform from `ts_r_transform`, dependency batches from `ts_r_bundler`, React Fast Refresh,
`parse<T>` expansion (`ts_r_guard`) and the Vite plugin sidecar. `ts-r dev` (record
[0009](0009-cache-and-launcher.md)) execs `ts-r-dev` with its arguments untouched.

### CLI (`ts-r-dev`)

`ts-r-dev [root] [--port <n>] [--strict-port] [--host [addr]] [--mode|-m <mode>] [--base <path>]
[--open [path]] [--no-cache] [--cache-dir <dir>] [--debounce <ms>] [--no-watch] [--force]
[--no-react-refresh] [--no-plugins] [--quiet] [-h|--help] [-V|--version]`, with `--opt=value`
accepted too. Arguments are parsed by hand.

- Defaults: root `.`, `127.0.0.1:5173`. If the port is taken, the next free one is used
  unless `--strict-port` is given. `--port 0` means any port; `--host` with no address means
  `0.0.0.0`. Mode `development`, base `/`, debounce quiet window 3 ms.
- Command-line flags override `vite.config` (`server.host`/`port`/`strictPort`/`open`, `base`,
  `mode`).
- `--no-cache` keeps transforms in memory only. `--cache-dir` sets the cache root. `--force`
  rebuilds the dependency batch and re-reads the plugin manifest.
- `--open` runs `$BROWSER` (`none` skips it), else `open`, `xdg-open` or `start`.
- Once listening it prints `  Local: <url>` in exactly that form (tests parse it). Update lines
  go to stdout. Exit codes: 0 for help and version, 2 for a usage error, 1 for a startup
  failure.

### Seams

| Trait | Contract | Implementations |
|---|---|---|
| `Transform` (`transform.rs`) | `cache_id`, `handles(path)`, `transform(path, root-relative id, source, Mode) -> {code, map, imports with spans in code}`, optional `deps(input) -> Vec<PathBuf>` | `PassthroughJs` (`.js`/`.mjs`); `ts-r-dev`: `TsTransform` wrapped by `GuardTransform`, wrapped by `ReactRefresh` |
| `Resolve` (`resolve.rs`) | `resolve(specifier, importer) -> File \| Dep{package, specifier, file} \| External` | `SimpleResolver` (relative, root-absolute, `node_modules` with `exports` conditions `browser, import, module, development, default` and `*` patterns, `module`, `main`) |
| `DepBundler` (`prebundle.rs`) | `id`, `bundle(one dep) -> ESM`, optional `bundle_batch(all deps) -> DepBatch` | `UnbundledDeps` (a package's ESM files as they are; CommonJS → an explanatory error); `ts-r-dev`: `ts_r_bundler::hmr::DepBundle` (batches with code splitting) |
| `PluginHost` (`plugin.rs`) | `cache_id`, `hooks()`, `attach`, `resolve_ids`, `load`, `transform(stage)`, `transform_index_html`, `handle_hot_update`, `middleware`, `client_event` | none by default; `ts-r-dev`: the Node sidecar |

`DevConfig` also takes `html_inject` (HTML inserted right after the client script) and
`virtual_modules` (URL → JS served from memory). Transforms are tried in order, and the first
whose `handles` matches serves the file.

### URL space and request order

`/` and HTML navigations (client injected); source files (transformed ESM, imports
rewritten); `/x.css` (CSS for `<link>`; with `?import`, a self-accepting JS module); `?import`,
`?raw`, `?url`, `?inline` (JS with a default export); `/@modules/<spec>?v=<hash>` (a
dependency through `bundle`); `/@ts-r/deps/<name>[.map]` (batch files); `/@fs/<abs>` (only
files the graph reached); `/@id/<id>` (plugin modules the graph knows, `\0` written
`__x00__`); `/@ts-r/up/<n>/<path>` (only files inside a `node_modules` directory);
`/@vite/client` (the client pages get; alias `/@ts-r/client`, which re-exports it);
`/@ts-r/env` (alias `/@vite/env`); `/@ts-r/ws`;
`/@ts-r/stats`; `/@ts-r/ping`; then the public dir, the root, plugin middlewares and the SPA
fallback.

Requests are handled in this order:

1. A WebSocket upgrade: protocol `vite-hmr` → HMR; else a `ws: true` proxy rule → a tunnel;
   else HMR.
2. A CORS preflight (`OPTIONS` → 204).
3. `server.proxy`, matched on the raw URL. It streams by the response's framing and never
   buffers whole bodies; an unreachable upstream is a 502.
4. The base is stripped (`/` redirects to the base; paths outside the base are still served).
5. The routes above.

Responses that are not proxied get `server.headers`, then CORS headers. URLs inside the graph,
in HMR payloads and in cached transforms are stored without the base. The base is added only
when text leaves the server.

### Module graph and propagation

- `ModuleGraph` nodes are `u32` ids into one `Vec`. A node is created when the browser
  requests a module, so the graph is exactly what the page loaded, and nodes are never
  removed. Each prepare replaces the module's edges wholesale. Importees that lose their last
  importer are sent in a `prune` message and stay pruned until something imports or requests
  them again.
- Accept information is static. `scan.rs` (a scanner, not a parser, which never fails) reads
  `import.meta.hot.accept()`/`accept(cb)`/`acceptExports(..)` (self-accepting),
  `accept("dep")`/`accept([..])` (accepted deps), and `decline()` from the transformed code.
- Propagation follows Vite's rules. A self-accepting module is its own boundary. An importer
  that accepts the changed dep is a boundary, and the dep is re-imported. Reaching a module
  with no importers, or one that called `decline()`, means a full reload. The walk is an
  iterative DFS that expands each module once. Every module on a path from the change to a
  boundary gets `last_hmr_timestamp = T`, and every importer's rewritten import of it carries
  `?t=T`. Cycles end the walk and set `isWithinCircularImport`. `vite:invalidate` propagates
  again from the module, skipping its own acceptance.
- `Transform::deps` names files outside the graph that a module's output depends on (the
  declarations that `parse<T>` validators are generated from). The graph keeps them per module
  (`watch_deps`, reverse index `by_dep`). A change to one re-prepares each module that read it,
  as if that module had changed.

### Hot path

One save costs: read the file and hash it with BLAKE3, run one transform (a cache miss),
resolve the file's imports, update its edges, walk importers up to the nearest boundaries, and
push one JSON message. Changes are prepared eagerly (transform before propagation), so
accept information is current and the browser's `?t=` fetch is a cache hit. A change whose
hash equals the prepared hash sends nothing, unless the module is in an error state.
Transform, resolve and JSON errors become an `error` payload (message, `loc`, code frame).
It is broadcast, remembered and replayed to clients that connect later, and the request gets
a 500. The next successful update clears it.

### HMR protocol

Server → client messages are Vite's: `connected`; `update` with `js-update`/`css-update` items
(`path`, `acceptedPath`, `timestamp`, `explicitImportRequired`, `isWithinCircularImport`);
`full-reload` (optional `path`); `prune` (`paths`); `error`; `custom`. The only addition is
`tsrId` on `update`, which clients ignore. Client → server messages are `custom` events:
`vite:invalidate`, `ts-r:applied` (the client's own timing, used for the latency
breakdown), and user events from `import.meta.hot.send`. The overlay element is
`<vite-error-overlay>`, and CSS `<style>` tags carry `data-vite-dev-id`. Non-JS imports carry
`?import`, so the server can tell a module request from a `<link>`/`<img>` request.

### `import.meta.hot` (client runtime `client/client.js`)

Each module that mentions `import.meta.hot` gets a prelude on line 1 at render time. The
prelude imports `createHotContext` from the client and sets `import.meta.hot` to the module's
context (`render::hot_prelude`).
The context provides `data`, `accept()`, `accept(cb)`, `accept(dep, cb)`, `accept(deps, cb)`,
`acceptExports` (treated as self-accept), `dispose`, `prune`, `decline` (a no-op at runtime;
the server reads it statically), `invalidate(message)`, `on`, `off` and `send`. A re-executed
module drops the previous instance's accept callbacks and listeners. Render-time edits (import
URLs, the prelude, `process.env.NODE_ENV`, glob expansions) never add lines; they shift
source-map columns instead.

### Vite plugins (`ts-r-dev`)

When the root has a `vite.config.{js,mjs,ts,mts,cjs,cts}` (and `--no-plugins` isn't given), a
Node sidecar (`node`, or `TS_R_NODE`) loads it. The sidecar is written by us from the
documented plugin API. It flattens and orders the plugins (`apply`, `enforce`, hook `order`,
hook `filter`), runs `config` → `configResolved` → `configureServer` → `buildStart`, and
implements `PluginHost` over JSON lines on stdin/stdout (ops `resolve`, `load`, `transform`,
`html`, `hot`, `middleware`, `proxyRewrite`, and `event` notifications; plugin stdout is
moved to stderr).

- Hook calls are skipped unless `PluginHost::hooks()` reports a plugin that has the hook.
  `transform` calls run inside the transform cache, so hits make no calls.
- Plugins named `vite:react*` and `@vitejs/plugin-react*` run only their `config` hooks. Their
  JSX options configure `TsTransform`, and Fast Refresh is ours.
- The manifest (plugin names, hooks present, JSX options, dev options) is cached in namespace
  `dev-plugins`. Its key covers the config, the local files it imports, `package.json`, the
  lockfile, the sidecar script and the mode. When no hooks remain, Node never starts once the
  manifest is cached. When hooks are cached, Node starts in the background and early calls
  wait for it.
- Limits: `--max-old-space-size` from `TS_R_PLUGIN_MEM_MB` (default 2048, max 8192) and a
  per-call timeout `TS_R_PLUGIN_TIMEOUT_S` (default 30 s; a call that times out kills the
  sidecar). The sidecar is killed on drop and exits when its stdin closes.

### React Fast Refresh and `parse<T>` (`ts-r-dev`)

React Fast Refresh is on when `react-refresh/runtime` (or Next's compiled copy) resolves from
the canonical root and `--no-react-refresh` isn't given. React's runtime is pre-bundled with
the deps. Our parts are the `ReactRefresh` transform wrapper (component and HOC registration,
hook signatures, self-accept, or `invalidate()` when the exports change), the
`/@ts-r/react-refresh` glue module and an HTML preamble. `GuardTransform` expands
`parse<T>` call sites as `ts-r-run` does (record [0007](0007-sound-mode.md)) and reports the
program's other sources as `deps`. Each wrapper puts a version in its `cache_id` suffix
(`+react-refresh/N`, `+ts-r-guard/1/...`), bumped when its output changes.

### Caches

- **Transforms** (`cache.rs`): memory first (cleared above 256 MB), then `ts_r_cache`
  namespace `dev-transform`. The key is `KeyBuilder("ts-r-dev/transform")` over the crate
  version, `Transform::cache_id` (plus the plugin host's `cache_id`), the mode, the
  root-relative id and the BLAKE3 of the source. When `deps` names files, each one's
  root-relative path and BLAKE3 are added. The key never uses mtimes, and no payload holds an
  absolute path.
- **Dependencies**: `/@modules/<spec>?v=<hash>` and `/@ts-r/deps/<name>?v=<version>` are
  immutable per lockfile (`Cache-Control: max-age=31536000, immutable` when `v` matches) and
  memoized in memory. `node_modules` is not watched.

### Dependency pre-bundling (batch mode)

When `DepBundler::bundle_batch(&[])` is `Some`, `deps.rs` starts a background thread at
startup. The thread scans the source graph reachable from `index.html`'s
`<script type="module" src>` (through the transforms, which warms the transform cache) for
bare imports that resolve into `node_modules`. It adds `DevConfig::optimize_include` and
bundles everything in **one** call, so modules shared between entries exist once. Batch files
link to each other by versioned URL.

The batch is cached in namespace `dev-deps` (format `TRDB`, no absolute paths in the payload).
Its key covers the crate version, the bundler id, the server binary's size and mtime (standing
in for the bundler's revision), the include list, `package.json`, the nearest lockfile
(searched upwards) and the root. `--force` ignores the cache. Source modules that import deps
wait for the batch. A dep missing from the batch re-runs the batch with the entry added and
broadcasts `full-reload`. Each batch file ends with `//# sourceMappingURL=<name>.map`. The maps
are stored as their own entries and read only when requested. A batch's `<stem>.css` is
served as a style module that `<stem>.js` imports. Dependency CSS has relative `@import`s
inlined (depth ≤ 32, cycles dropped) and `url()`s rewritten to served URLs.

### File watching

- `watch.rs` uses `notify` (FSEvents with zero latency and `NoDefer`, inotify,
  ReadDirectoryChangesW) on the root, recursively. Paths with a `node_modules`, `.git`, `.hg`,
  `.svn` or `.DS_Store` component are ignored. A dedicated watch thread owns the native watcher
  and the `Debouncer`. Defaults: quiet window 3 ms, maximum wait 30 ms, and a 40 ms grace for
  files that are empty (truncate then write) or missing (delete then recreate). A file found
  missing that is back after the grace is reported once, as created (chokidar reports a
  removal and an addition; the server acts on the addition). Atomic saves
  (temp file + rename) come out as one modification of the target.
- macOS fast path: the 64 most recently changed files (`FAST_PATH_FILES`) are also watched
  with kqueue on their own thread, re-armed after every change. FSEvents delivers in about
  11 ms; kqueue delivers in about 0.1 ms. The late FSEvents duplicate is a second burst with an
  unchanged hash.
- **FSEvents startup replay rule.** FSEvents also reports writes made just before its stream
  started. For `watch::STARTUP_WINDOW` (10 s) after the watch starts, on macOS only, a
  settled change whose file's mtime is older than the moment the watch started is dropped:
  one `stat` per settled change, only in that window. Removals are always reported, and so is
  an mtime with no sub-second part (coarse file systems). As with chokidar's `ignoreInitial`,
  a file moved in with an old mtime during the window isn't reported.
- When the graph first reaches a file outside the root (a linked workspace package, a
  `watch_deps` file), its package directory (nearest `package.json`) is added to the watcher
  recursively, once. A directory that contains the root is never added recursively. Adding is a
  channel send to the watch thread, never a lock on the hot path.
- A change to a `.env*` file in the env directory reloads the env and sends `full-reload` when
  the result differs. A new or deleted file that matches a module's `import.meta.glob` patterns
  re-prepares that module.

### Threading model

Plain std threads; no async runtime.

- One accept thread (`ts-r-dev-accept`), and one thread per connection (`ts-r-dev-conn`:
  keep-alive, blocking writes, 120 s read timeout, optional TLS handshake with a 10 s timeout).
  A WebSocket connection keeps its thread.
- One watch thread (`ts-r-dev-watch`) plus, on macOS, one kqueue thread. Each settled batch is
  handled on the watch thread (`hmr::on_batch`). Client messages (`vite:invalidate`,
  `ts-r:applied`) are handled on their WebSocket connection's thread.
- One dependency-optimizer thread at startup.
- Changes and invalidations are serialized by `Shared::change_lock`. HTTP serving runs at the
  same time and holds the graph lock only briefly, never across I/O or transforms.
- The Vite sidecar is a child process. `ts-r-dev` spawns it before the Rust-side setup, so the
  two overlap.

## Invariants agents must keep

- The hot path touches only the changed file: no graph or project scans per save. The only
  per-change lookups are `by_file` and `by_dep`, each a hash-map hit.
- Propagation semantics, server → client messages, and the `import.meta.hot` surface are Vite's.
  Additions are fields that clients ignore.
- Transforms are deterministic per `(cache_id, root-relative id, content, mode)`. Change
  `cache_id` (or a wrapper's suffix) whenever the output for the same input can change. Bump
  `cache::FORMAT_VERSION` or the `dev-deps` format when a payload layout changes.
- The graph is built from browser requests, and nodes are never removed.
- Same content means no update. Files untouched since the watch started are not changes.
- Errors never crash the server. Malformed HTTP or WebSocket input closes only that connection.
- `/@fs/` and `/@id/` serve only what the graph knows, and plain paths may not escape the root.
- Render-time edits never add lines. Every source map is composed into one chain back to the
  original file.
- URLs are stored without `base`.
- Plugins cost nothing when absent. No server logic goes in `ts_r_dev`.

## Known gaps

- `Resolve` still uses `SimpleResolver` in `ts-r-dev`; `ts_r_resolver` isn't wired in. A newly
  created file that would change how an existing import resolves is picked up only when the
  importer changes.
- Plugins: `load` isn't called for plain script, CSS, JSON or asset files. There is no
  `this.addWatchFile`/`emitFile`; `this.parse`, `this.load` and `ssrLoadModule` throw;
  `server.moduleGraph` is a stub. Middlewares run only for requests no built-in route serves,
  with buffered bodies. Not read from the config: `css.*`, `server.hmr`, `server.fs`,
  `server.watch`, `optimizeDeps.exclude`/`esbuildOptions`, proxy `configure`/`bypass`,
  `cors.origin` functions.
- Dependency scan: only `<script type="module" src>` in `index.html`. Non-literal `import(expr)`
  is found only on request (re-batch and reload) and isn't rewritten. Without a batch-capable
  bundler, CommonJS packages fail.
- App CSS: no `@import` handling, no CSS modules, no preprocessors. Dependency CSS isn't watched.
- `base`: dependency batch files and dependency CSS-as-JS still link `/@ts-r/...` without the
  base.
- HTTP/1.1 only (no HTTP/2, no compression). Windows has no watcher fast path and is untested.
  `full-reload` latency isn't measured.
- Fast Refresh doesn't register components created inside other expressions, or HOC results
  bound to lowercase names.

## Alternatives

- **hyper/tokio/tungstenite**: rejected. The server serves one browser on localhost (at most
  6 HTTP/1.1 connections and 1 WebSocket). A thread per connection with blocking writes keeps
  the hot path to a few syscalls, starts in about 2 ms, and keeps the binary and build small.
  HTTP and WebSocket framing are about 700 lines with unit tests. Revisit if HTTP/2 is added.
- **FSEvents alone on macOS**: p50 was 15.9 ms, dominated by FSEvents delivery (about 11 ms).
  The kqueue fast path brought p50 to about 5 ms (1k-50k modules). `notify` can't run its
  FSEvents and kqueue backends side by side, so the `kqueue` crate is used directly, bounded
  to 64 file descriptors.
- **mtime-based transform cache keys**: rejected for content hashes, so touches, duplicate
  events and restarts reuse work. The dependency batch key does include the server binary's
  size and mtime, because the bundler's version is not bumped per change.
- **One dependency per request** (`/@modules/` only): files shared between entries are
  duplicated, and CommonJS can't be served. Batch mode with code splitting is the default
  whenever the bundler supports it.
- **Running `@vitejs/plugin-react` in Node**: replaced by our transform and our Fast Refresh,
  so its per-module transform calls never cross to Node. Once the manifest is cached, Node
  never starts for a config whose only plugin is plugin-react.

## Status

Written 2026-10-18 from the code at the top of the stack (`m7/bench-box`), after M2 had shipped
and M3's dependency batches had been wired in. Accepted 2026-10-18 by Riley Webb. Measured
results (latency tables, a real app against Vite 8.3.4) are kept in `ts_r_hmr`'s `AGENTS.md`.
