# ts_r_hmr

Dev server core for `ts-r-dev` (milestone M2): unbundled ESM per module in dev, dependencies
served through a pre-bundling seam (decision D3), a module graph with HMR boundary
propagation, and a client runtime implementing Vite's `import.meta.hot` API so Vite plugins
(React Fast Refresh and others) can work unchanged later. The binary is
[`../ts_r_dev`](../ts_r_dev/AGENTS.md). Root rules: [../../AGENTS.md](../../AGENTS.md).
Design record: [0010](../../docs/design/0010-dev-server.md).

## Seams (later milestones plug in here; the server must not change)

| Trait | Today | Replaced by |
|---|---|---|
| `Transform` (`src/transform.rs`): path + source + `Mode` → JS + source map + import specifiers with spans in the output | `PassthroughJs` (`.js`/`.mjs`, imports from `scan.rs`); `ts-r-dev` adds `ts_r_transform::hmr::TsTransform` (TS/TSX/JSX), wrapped by its React Refresh transform | later plugin layers |
| `Resolve` (`src/resolve.rs`): specifier + importer → `File` / `Dep` / `External` | `SimpleResolver` (relative, root-absolute, `node_modules` with `exports` conditions and `*` patterns, `module`, `main`) | `ts_r_resolver` |
| `DepBundler` (`src/prebundle.rs`): one dependency entry → ESM (`bundle`), or all scanned entries at once → code-split files (`bundle_batch`, optional) | `UnbundledDeps` (serves the package's ESM files one by one; CJS → clear error; no batch) | `ts-r-dev` uses `ts_r_bundler::hmr::DepBundle` (batch with code splitting, CJS interop) |
| `PluginHost` (`src/plugin.rs`): Vite-style hooks at fixed points (`resolveId` before/after the resolver, `load`, `transform` before/after the `Transform`, `transformIndexHtml`, `handleHotUpdate`, middlewares, page custom events) | none (`DevConfig::plugins = None`) | `ts-r-dev`'s Node sidecar running the project's `vite.config` plugins |

Transforms must be deterministic per `(cache_id, root-relative id, content, mode)`: outputs
are cached by content hash. Change `cache_id` when output for the same input can change.

## Module layout

| File | What |
|---|---|
| `server.rs` | `DevServer`/`DevConfig`, accept loop, routing (URL table in the module docs), module prepare/render, dependency serving |
| `hmr.rs` | file change → read → prepare → propagate → push; client messages (`ts-r:applied`, `vite:invalidate`); error state |
| `graph.rs` | `ModuleGraph` (u32 ids, importers/importees, accepted deps, `propagate`) |
| `scan.rs` | JS import / `import.meta.hot` scanner (not a parser; never fails) |
| `render.rs` | import rewriting, HTML client injection, CSS-as-JS, content types, code frames |
| `watch` | re-export of [`ts_r_watch`](../ts_r_watch/AGENTS.md): notify watcher, `Debouncer`, macOS kqueue fast path, dropping FSEvents' replayed startup events (moved out in design record 0012, E4) |
| `timing.rs` | per-step latency records, `/@ts-r/stats` |
| `cache.rs` | `TransformCache`: memory + `ts_r_cache` namespace `dev-transform` |
| `deps.rs` | dependency optimizer: scan from `index.html`, one batch via `bundle_batch`, cache (namespace `dev-deps`), `/@ts-r/deps/`, re-batch on miss |
| `css.rs` | dependency CSS: `@import` inlining, `url()` → served URLs (`asset_url`) |
| `plugin.rs` | `PluginHost` seam, `/@id/` plugin-module URLs |
| `http.rs`, `ws.rs` | HTTP/1.1 and RFC 6455 framing (server and client roles) |
| `net.rs` | `Duplex` (owned reader + writer over TCP or TLS), outbound `connect`, `tls` (rustls server/client configs) |
| `env.rs` | `.env` files, `envPrefix`, `import.meta.env` prelude, `define`, HTML `%VAR%` |
| `assets.rs` | asset URLs, `?url`/`?raw`/`?inline`, JSON named exports, public-dir imports |
| `glob.rs` | `import.meta.glob` expansion and the patterns a module watches |
| `proxy.rs` | `server.proxy`: rule matching, streamed HTTP forwarding, WebSocket tunnels |
| `cors.rs` | `server.cors`: preflights and response headers; `js_regex` (JS regex source → `regex-lite`) |
| `sourcemap.rs` | VLQ, map composition (plugin chain), column shifting for render-time edits |
| `client/client.js` | browser runtime served at `/@ts-r/client` (alias `/@vite/client`) |
| `testutil.rs` | `#[doc(hidden)]` temp dirs, fixture app, HTTP GET, shared with `ts_r_dev` tests |

## Invariants

- **The hot path touches only the changed file.** A save costs: read + BLAKE3 the file, one
  transform (cache miss), resolve its imports, update its edges, walk importers up to the
  nearest boundaries (iterative DFS, each module expanded once), push one JSON message. Nothing
  scans the graph or the project. Keep it that way; the benchmark proves "flat across sizes".
- **Propagation semantics (Vite's):** a self-accepting module is its own boundary; an importer
  that accepts the dep (`hot.accept('./dep')`) is a boundary and the dep is re-imported; reaching
  a module with no importers, or a module that called `hot.decline()`, means full reload. Every
  module on a path from the change to a boundary is *invalidated*: it gets
  `last_hmr_timestamp = T`, and every importer's rewritten import of it carries `?t=T`, so the
  browser fetches fresh instances along the whole chain. Cycles terminate and set
  `isWithinCircularImport` (client reloads if the re-import throws). `invalidate()` re-propagates
  from the module skipping its own acceptance.
- **Graph is built lazily from what the browser requests** (that is the unbundled-ESM graph).
  Nodes are never removed; ids stay valid. Edges are replaced wholesale on each prepare;
  importees that lose their last importer are sent in a `prune` message.
  A pruned module stays `pruned` until something imports or requests it again; changes to
  its file are ignored (a late watcher event from before the prune must not reload the page),
  except that they stamp its `?t=`, so an import restored later fetches the new file.
- **Changes are prepared eagerly** (transform before propagation), so accept info is current and
  the browser's `?t=` fetch is a cache hit. Changes and invalidations are serialized by
  `change_lock`; HTTP serving runs concurrently and only holds the graph lock briefly (never
  across I/O or transforms).
- **Same content = no update.** A change whose BLAKE3 equals the prepared hash (touch, double
  write, late duplicate event) sends nothing, unless the module is in an error state.
- **Files untouched since the watch started are not changes.** FSEvents (macOS) replays writes
  made just before its stream started; they used to reach `handleHotUpdate` and update modules
  the browser hadn't fetched yet (never prepared, so the hash check can't catch them). For
  `watch::STARTUP_WINDOW` (10 s) after the watch starts, a settled change whose file's mtime
  is older than the start is dropped (one `stat` per settled change, only in that window, only
  on macOS; inotify/ReadDirectoryChangesW don't replay). Removals and mtimes with no
  sub-second part (coarse file systems) are always reported. As with chokidar's
  `ignoreInitial`, a file moved in with an old mtime during that window isn't reported.
- **Errors never crash the server.** Transform/resolve/JSON failures become an `error` payload
  (message, `loc`, code frame) broadcast to clients, remembered and replayed to clients that
  connect later, and a 500 for the request. The next successful update clears it. A client
  gets each error once: joining the hub and the replay happen under `last_error`'s lock, which
  `report_error` holds while it broadcasts (and a reload or update clears it before it goes out). Malformed
  HTTP/WS input closes that connection only.
- **Protocol compatibility is a public surface.** Server → client messages are Vite's
  (`connected`, `update` with `js-update`/`css-update` items carrying `path`, `acceptedPath`,
  `timestamp`, `explicitImportRequired`, `isWithinCircularImport`; `full-reload`, `prune`,
  `error`, `custom`). We add only `tsrId` on `update` (clients ignore unknown fields). The
  overlay element is `<vite-error-overlay>`; CSS `<style>` tags carry `data-vite-dev-id`.
  Client → server: `custom` events (`vite:invalidate`, `ts-r:applied`, user events).
- **Non-JS imports carry `?import`** (CSS, JSON, assets) so the server can tell a module
  request from a `<link>`/`<img>` request; `explicitImportRequired` tells the client to keep it
  when re-importing. CSS without `?import` (and not `Sec-Fetch-Dest: script`) is served as CSS
  and updated with `css-update` (link swap).
- **Cache keys are content hashes**, never mtimes: `KeyBuilder("ts-r-dev/transform")` over crate
  version, `Transform::cache_id`, mode, root-relative id, BLAKE3 of the source (plus, when
  `Transform::deps` names files, each one's root-relative path and BLAKE3). Payload format
  is `cache::encode` (bump `cache::FORMAT_VERSION` when it changes). No absolute paths in
  cached data.
- **Files a transform reads are edges too.** `Transform::deps` (default: none) names files
  outside the module graph the output depends on (the declarations `parse<T>` validators are
  generated from: type-only imports never reach the browser). The graph keeps them per module
  (`watch_deps`, reverse index `by_dep`); a change to one re-prepares each module that read it
  and propagates as if that module had changed (skipped when the output is identical). This is
  the only per-change lookup besides `by_file`, and it is a hash-map hit.
- **Dependencies are immutable per lockfile:** `/@modules/<spec>?v=<hash>` where the hash covers
  the lockfile(s), `package.json` and `DepBundler::id`; served with
  `Cache-Control: max-age=31536000, immutable` when `v` matches, and memoized in memory.
  `node_modules` is not watched.
- **Batch mode (when `DepBundler::bundle_batch(&[])` is `Some`)**: at startup `deps.rs` scans
  the source graph reachable from `index.html`'s module scripts (through the transforms, which
  warms the transform cache) for bare imports that resolve into `node_modules`, adds
  `DevConfig::optimize_include`, and bundles them in **one** call, so modules shared between
  entries (React between `react`, `react-dom/client`, `react/jsx-runtime`) exist once. Files are
  served at `/@ts-r/deps/<name>?v=<version>` (immutable when `v` matches; the version hashes
  the cache key, the specifiers and every output file); batch files link to each other by
  versioned URL. The batch is cached in `ts_r_cache` namespace `dev-deps`
  (format `TRDB` v2, no absolute paths), keyed by crate version, bundler id, server binary
  size+mtime, include list, `package.json` and the nearest lockfile (searched upwards, so
  workspace lockfiles count); `--force` (`DevConfig::force_deps`) ignores it. Source modules
  importing deps wait for the batch; a dep not in the batch re-runs it with the entry added,
  stores it, and broadcasts `full-reload` (running modules came from the old batch). JS entries
  only: CSS/JSON imported by app code from packages go through `/@modules/`.
- **Batch source maps** (`DepBatch` file `map`s, from the bundler with `sourcesContent`): each
  batch file ends with `//# sourceMappingURL=<name>.map`, served at `/@ts-r/deps/<name>.map`;
  `sources` are relative to that URL (`../../node_modules/react/index.js`), so DevTools shows
  the original package files and stack traces map back to them. Maps are large and only
  DevTools reads them: they are stored as their own `dev-deps` entries (keyed by version and
  name) and leave memory once stored; a warm start never reads them until requested.
- **CSS imported by dependencies**: the batch's `<stem>.css` (the CSS `<stem>.js` imports,
  concatenated) is served as a style module at `/@ts-r/deps/<stem>.css`, imported by
  `<stem>.js` (appended, so the map's lines stay put).
- **Dependency CSS is self-contained** (`css.rs`): CSS read from a package, by the batch (the
  bundler's file system overlay) or by `/@modules/pkg/x.css` (app imports), has relative
  `@import`s inlined (wrapped in `@layer`/`@supports`/`@media` per the import's conditions;
  cycles dropped, depth ≤ 32) and relative `url()`s rewritten by `css::asset_url`: `/<path>`
  under the root, else `/@ts-r/up/<n>/<path>` (relative to the root's n-th ancestor, so no
  absolute path reaches the cached batch). `/@ts-r/up/` serves only files inside a
  `node_modules` directory. Package assets requested through `/@modules/` are served as files
  (or `export default "<url>"` with `?import`). A linked package's CSS is source: watched and
  hot-updated (`css-update` or `js-update` like app CSS).
- **Linked workspace packages are source**: a bare import whose real path has no
  `node_modules` component (pnpm `workspace:` links) resolves to a file and is transformed and
  hot-updated like app code (served via `/@fs/`), as Vite does. When the graph first reaches
  a file outside the root (an import, or a module's `watch_deps`: a plugin module's backing
  file, a transform input), its package directory (nearest `package.json`) is added to the
  watcher recursively, once (`server::watch_outside`, `watch::WatchAdder`); a directory that
  contains the root is never watched recursively (only the file's own directory). The watch
  thread owns the native watcher, so adding is a channel send, never a lock on the hot path.
- **Extension points for embedders** (`DevConfig`): `html_inject` (HTML inserted after the client
  script, e.g. the React Refresh preamble), `virtual_modules` (URL → JS served as-is, e.g.
  `/@ts-r/react-refresh`).
- **`/@fs/` serves only files already in the graph** (reached through an import), and plain
  paths may not escape the root.
- **Source maps are one chain:** plugin `transform` maps (pre), the built-in transform's map
  and post-plugin maps are composed (`sourcemap::compose`) into one map back to the original
  file (source renamed to its basename, `sourcesContent` filled); a hook that returns code
  without a map is skipped in the chain (treated as not moving text, as Vite does). Render-time edits
  (import URLs, the line-1 hot/env prelude, `process.env.NODE_ENV`, glob expansions) never
  add lines and shift the map's columns (`sourcemap::shift_for_edits`). The result is inlined
  as a data URL.
- **URLs are stored without `base`.** The graph, the HMR payloads (`path`, `acceptedPath`)
  and cached transforms are base-less; the base is added only when text leaves the server
  (`Shared::public_url`: rewritten imports, the client URL, the hot prelude, CSS-as-JS,
  `html_with_base` on `src`/`href` of script/link/img/source/video/audio/track). Requests
  have the base stripped (`strip_base`): `/` redirects (302) to the base; other paths outside
  the base are still served (lenient, so dependency batch URLs keep working).
- **Request order:** WebSocket upgrade (protocol `vite-hmr` → HMR; else a `ws: true` proxy
  rule → tunnel; else HMR), CORS preflight (`OPTIONS` → 204), proxy (matched on the raw URL,
  before the base is stripped), base, routes. Every non-proxied response goes through
  `finish_response`: `server.headers` replace same-named headers, then CORS. Proxied responses
  get CORS headers the upstream didn't send, never `server.headers`.
- **Proxying streams.** The upstream request uses `Connection: close`; the response body is
  copied by its framing (`Content-Length`, chunked, or to EOF, which then closes the client
  connection), never buffered whole. `Expect: 100-continue` is answered locally. An unreachable
  upstream is a 502 with an empty body. `rewrite` is an `Arc<dyn Fn>` (ts-r-dev calls the
  plugin sidecar). WebSocket tunnels are raw byte pipes after the upstream's 101.
- **TLS (`server.https`, feature `tls`, on by default):** rustls with the `ring` provider,
  ALPN `http/1.1` (no HTTP/2). The handshake has a 10 s timeout. Outbound `https://`/`wss://`
  proxy targets verify against `webpki-roots` unless `secure: false`.
- **Env is reloadable state.** `Shared::env` (an `RwLock<Arc<Env>>`) holds the loaded `.env`
  files, `define` and mode; the `import.meta.env` prelude is added at render (not cached with
  the transform). A change to a `.env*` file in the env dir reloads it and sends `full-reload`
  when the result differs. The client runtime and `/@vite/env` carry the `define` globals.
- **Glob imports are expanded after the transform cache** (in `prepare_script`), so a new or
  deleted file matching a module's patterns (`Shared::glob_watches`) re-prepares that module
  and propagates like an edit. Aliases in patterns go through the plugin `resolveId` (pre).
- **The public dir** (`public/` unless `publicDir`) is served at the base, before source files;
  importing `/x.png` from it gives its URL (`?import`), never a module.
- **Plugins are optional and cheap when absent.** Every hook call is skipped unless
  `PluginHost::hooks()` says some plugin has it (`Shared::plugin_hooks`, read once). With
  hooks, a module costs one batched `resolveId` call per stage, one `transform` call per stage
  (inside the transform cache: hits make no calls; the cache id gains the host's `cache_id`), and
  a save one `handleHotUpdate` call.
- **Plugin modules** are graph modules served at `/@id/<id>` (`plugin::plugin_url`, `\0` as
  `__x00__`): ids a `resolveId` returned that aren't plain files (virtual ids, files with a
  query), imports with a query the server doesn't know (`x.svg?react`), and files that are
  neither scripts, CSS, JSON nor known assets (`.md`) while plugins have `load`/`transform`
  hooks. Their source is `load` (or Vite's load fallback: a known asset's URL, else the file's
  text); the file behind an id with a query is a watch dependency, so editing it re-loads and
  hot-updates the module. `/@id/` serves only ids the graph knows. Plugin `transform` maps
  are composed (see source maps above).
- **`/abs/path` specifiers** that name a file system path (inside the root, or whose
  root-relative directory doesn't exist) resolve as `/@fs/abs/path` (`resolve.alias` and plugins
  produce them).

## Latency (the plan's hot-reload budget, target p50 < 30 ms)

Each update logs one line, and `/@ts-r/stats` returns p50/p95 per step:

```
hmr /src/counter.js js-update · watch 0.0 · debounce 3.8 · read 0.0 · transform 0.0 · validators 0.0 · graph 0.0 · push 0.0 · browser 0.8 · total 4.8 ms
```

`watch` = file mtime → native event; `debounce` = first event → batch flushed; `browser` =
measured by the client (message received → modules re-imported and callbacks run), sent back
as `ts-r:applied`; `total` = mtime → acknowledgement received. `validators` = re-preparing
modules whose transform read the changed file (`Transform::deps`; 0 for a plain edit).

Measured (Apple M-series, release, headless Chrome, `ts_r_dev/tests/latency.rs`, edit of a
self-accepting module under a 100-wide import tree, browser-side timing):

| Modules | p50 | p95 | first save of a file (FSEvents path) | initial page load |
|---|---|---|---|---|
| 1,000 | 4.9 ms | 5.9–6.9 ms | 7–17 ms | 0.3 s |
| 10,000 | 4.8–5.0 ms | 5.3–6.4 ms | 7–17 ms | 1.7 s |
| 50,000 | 5.4 ms | 8.3 ms | 16 ms | 13.5 s |

(Ranges are across runs; 47 measured edits per size after 3 warm-up edits.)

Before the kqueue fast path, p50 was 15.9 ms at both sizes, dominated by FSEvents delivery
(≈ 11 ms). Now the 3 ms debounce quiet window is the largest step.

### Real app: excalidraw vs Vite 8.3.4

[`bench/dev/dev.mjs`](../../bench/dev/dev.mjs) `--app excalidraw --server both --mode bench`
(see `ts_r_dev`'s AGENTS.md and [bench/README.md](../../bench/README.md)): excalidraw at the
commit pinned in `bench/repos.toml` (about 500 TS/TSX modules served from source, 47
dependency entries), release build, Apple M-series shared laptop (other agents running),
headless Chrome (a fresh one per load), servers under `scripts/limit.sh` with 4 threads, 5 cold
+ 5 warm runs, 50 measured edits of the welcome-screen heading (after 3 warm-ups) with state
kept by Fast Refresh in both. Start = process spawn → the canvas and heading in the DOM. Cold =
empty ts-r cache / no Vite deps cache. RSS = whole server process group (ts-r-dev includes its
Node sidecar, which runs the config's aliases). SCSS is replaced by an empty stylesheet for
both servers (no preprocessors in ts-r-dev yet). Measured 2026-10-16:

| | ts-r-dev | Vite 8.3.4 + plugin-react 6.1.2 |
|---|---|---|
| Cold start to first render (median) | 939 ms | 1,107 ms |
| Warm start to first render (median) | 538 ms | 1,032 ms |
| Save → DOM updated p50 / p95 | 8.6 / 9.0 ms | 35.0 / 37.2 ms |
| State kept / reloads | 53/53, 0 | 53/53, 0 |
| Server RSS after load / after edits (peak) | 151 / 153 MB (153) | 371 / 364 MB (376) |

Earlier rounds measured a private app's client harness (13 dependency entries): cold 339-354
ms against 442-457 ms, warm 200-238 ms against 418-432 ms, updates 6.7-7.7 ms p50 against
33.6-66.9 ms, RSS 24-28 MB against 180-222 MB. Those numbers are kept for history only.

Reading the plugin config with an empty cache costs one Node run (~50-70 ms: Node itself ~14
ms plus importing the config's modules, plugin-react ~22 ms); warm runs use the cached
manifest. Spawning Node before the Rust setup saves only that setup (~4 ms). Cold includes the
dependency batch (excalidraw: 47 entries → 133 files, ~0.5 s, overlapped with serving source
modules). Server-side per-update total is ~7.5 ms (`debounce` ~4.5, transform + refresh
wrapper ~0.6, browser ~2.4).

## Dependencies

- `ts_r_cache` (path): persistent transform cache and BLAKE3 hashing (`Key::of`).
- `ts_r_watch` (path): the file watcher (`notify`, and `kqueue` on macOS), shared with
  `ts-r-check --watch`.
- `serde_json` (`preserve_order`): HMR protocol messages, `package.json`, CDP in tests (approved list). `preserve_order` is required: `exports` conditions are matched in key order, and without it (sorted keys) `default` beat `import` whenever `ts_r_hmr` was built without another crate turning the feature on.
- `sha1_smol`: SHA-1 for the WebSocket handshake (`Sec-WebSocket-Accept`); tiny, no deps.
- `regex-lite`: `server.proxy` keys starting with `^` and regex `cors.origin` entries are
  JavaScript regexes from the user's config; a small, dependency-free engine (JS source is
  translated by `cors::js_regex`; syntax it lacks, such as lookaround or backreferences, makes
  ts-r-dev skip that rule or origin with a startup warning).
- `rustls` (feature `tls`; `ring`, `std`, `tls12`) and `webpki-roots`: `server.https` and
  `https://`/`wss://` proxy targets. Writing TLS is not something to do from scratch; rustls
  is the memory-safe standard and needs no system OpenSSL. `webpki-roots` gives Mozilla's root
  store for verifying proxy targets. Build without default features to drop both.
- **Not** hyper/tokio/tungstenite: the server talks to one browser on localhost (≤ 6 HTTP/1.1
  connections + 1 WebSocket); thread-per-connection with keep-alive and blocking writes keeps
  the hot path to a few syscalls, starts in ~2 ms, and keeps the binary and build small. HTTP
  and WebSocket framing here are ~700 lines with unit tests. Revisit if we add HTTP/2 (which
  browsers only speak over TLS).

## Testing

- `cargo test -p ts_r_hmr`: unit tests per module (scanner, graph propagation incl. diamonds,
  cycles, decline, invalidate, 200k-deep chain; rewriting; debouncer editor patterns; files
  written just before the watch starts aren't reported; HTTP/WS framing; cache round trip and
  restart; resolver); `tests/server.rs` runs the real server and
  watcher against the fixture app with a WebSocket client standing in for the page (atomic
  saves, dep accept, CSS both ways, error + recovery + replay, prune, full reload,
  invalidate, HTML change, keep-alive, restart with a warm cache, a linked package outside
  the root watched with its JS and CSS hot-updated).
  `tests/plugins.rs` drives every `PluginHost` hook with an in-process host. Watcher-driven
  steps wait (bounded) for their own effect (the expected `handleHotUpdate` call, the served
  body, a message carrying the written text), never for "the next message", so a stray
  native event can't stand in for the write under test.
  `tests/config.rs` checks the Vite-config surface against outputs observed from Vite 8.3.4
  on the same fixtures: env files/precedence/prefix, `define`, `base`, HTML `%VAR%`, assets
  (`?url`/`?raw`/`?inline`, SVG inlining, `new URL`), JSON named exports, `public/`, globs
  (eager/lazy/`import`/`query`, a new file re-expands), proxy (prefix, regex, `rewrite`,
  target path, chunked and POST bodies, 502, WebSocket), CORS and `server.headers`, and HTTPS
  serving + proxying to a TLS upstream. `tests/fixtures/tls/` is a **test-only** self-signed
  certificate for `localhost`/`127.0.0.1` (never use it outside tests).
- Browser end-to-end and the latency benchmark live in `ts_r_dev` (they need the binary).
- The unit-test binary calls `ts_r_testing::cap_memory!()` (dev-dependency): the graph tests
  build a 200k-module chain.

## Gaps

- No `parse<T>` validator expansion here (`ts_r_dev` wraps the transform). React Fast Refresh
  and Vite config/plugin loading live in `ts_r_dev`. Plugin seam gaps: `load` isn't called for
  plain script, CSS, JSON or asset files; no `this.addWatchFile`/`emitFile`; plugin source maps
  are composed but asset/JSON modules carry no map; middlewares run only for requests no built-in route serves (and non-GET
  requests), with buffered bodies (no streaming/SSE); the dependency scan resolves through `resolveId` and aliases but doesn't look
  through plugin modules (their bare imports are found on request: re-batch + reload).
- Without a batch-capable `DepBundler`, CJS packages fail with an explanatory error and a
  dependency costs one request per file. Nested `node_modules` (`a/node_modules/b`) are
  re-resolved from the root by `/@modules/b/...` and may pick the wrong copy.
- Dep scan: only `<script type="module" src>` in `index.html` (no inline module scripts, no
  multi-page `build.rollupOptions.input`); imports behind non-literal `import(expr)` are found
  only on request (re-batch + reload).
- Non-literal dynamic `import(expr)` isn't rewritten (bare specifiers in it fail); inline
  `<script type="module">` in HTML isn't rewritten; in app (source) CSS
  `@import` and CSS modules (`.module.css`) aren't handled and `url()` is made absolute only
  (dependency CSS is bundled, see above). Dependency CSS isn't watched (only linked packages).
- `new URL(`./${name}.png`, import.meta.url)` (a template with expressions) isn't turned into
  a glob as Vite does; only string literals are resolved. `import.meta.glob` needs literal
  arguments (as in Vite).
- `base`: dependency batch files and dependency CSS-as-JS still link `/@ts-r/...` without
  the base; they work only because paths outside the base are served.
- Proxy: no `configure`, `bypass`, `agent`, `auth` options or `xfwd`; `cors.origin` as a
  function falls back to the default. No `server.hmr` options (port, path, overlay) yet.
- Output differs cosmetically from Vite's (quote style of rewritten imports, Vite reprints
  plain JS, `'' + import.meta.url` in `new URL`); the tests compare semantics.
- First save of a file on macOS pays FSEvents latency (~11 ms); Linux/Windows have no fast path
  (inotify is already fast; Windows untested).
- Resolution results are cached with the prepared module; a newly created file that would
  change how an existing import resolves (e.g. adding `x.ts` next to `x.js`) is picked up only
  when the importer changes. Errored modules are retried when any file is created.
- `full-reload` latency isn't measured (no acknowledgement after a reload).
- HTTP/1.1 only (also over TLS: no HTTP/2), no compression.
