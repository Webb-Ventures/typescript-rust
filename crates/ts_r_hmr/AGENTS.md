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
| `html_proxy.rs` | inline `<script type="module">` in HTML → `/page.html?html-proxy&index=N.js` modules |
| `watch` | re-export of [`ts_r_watch`](../ts_r_watch/AGENTS.md): notify watcher, `Debouncer`, macOS kqueue fast path, dropping FSEvents' replayed startup events (moved out in design record 0012, E4) |
| `timing.rs` | per-step latency records, `/@ts-r/stats` |
| `cache.rs` | `TransformCache`: memory + `ts_r_cache` namespace `dev-transform` |
| `deps.rs` | dependency optimizer: scan from `index.html`, one batch via `bundle_batch`, cache (namespace `dev-deps`), `/@ts-r/deps/`, re-batch on miss |
| `css.rs` | CSS bundling behind a `CssHost` (read, `@import` target, `url()` rewrite); dependency CSS (`bundle_css`, `asset_url`) |
| `css_refs.rs` | finds and rewrites file references in CSS: `url()`, `image-set()` candidates (not in comments, strings or `@import`) |
| `server/app_css.rs` | app CSS host: Vite-style reference resolution, served URLs under the base, data URLs, dependencies for HMR |
| `html_urls.rs` | page URLs: the base on asset attributes, `srcset`, `meta` images, classic-script `import('...')`, `<style>`/`style=""` through the CSS host |
| `plugin.rs` | `PluginHost` seam, `/@id/` plugin-module URLs |
| `http.rs`, `ws.rs` | HTTP/1.1 and RFC 6455 framing (server and client roles) |
| `net.rs` | `Duplex` (owned reader + writer over TCP or TLS), outbound `connect`, `tls` (rustls server/client configs) |
| `env.rs` | `.env` files, `envPrefix`, `import.meta.env` prelude, `define`, HTML `%VAR%` |
| `assets.rs` | asset URLs, `?url`/`?raw`/`?inline`, JSON named exports, public-dir imports |
| `glob.rs` | `import.meta.glob` expansion and the patterns a module watches |
| `wasm.rs` | `.wasm` imports: bounded import/export section reader, `?init` / direct-import / instance modules, the wasm helper |
| `worker.rs` | `?worker` / `?sharedworker` wrapper modules, worker file URLs, the worker env prelude |
| `proxy.rs` | `server.proxy`: rule matching, streamed HTTP forwarding, WebSocket tunnels |
| `cors.rs` | `server.cors`: preflights and response headers; `js_regex` (JS regex source → `regex-lite`) |
| `sourcemap.rs` | VLQ, map composition (plugin chain), column shifting for render-time edits |
| `client/client.js` | browser runtime served at `/@vite/client` (alias `/@ts-r/client`) |
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
  from the module skipping its own acceptance. Each update item names the module whose
  change started the chain (`firstInvalidatedBy`); the client sends it back with
  `vite:invalidate`, and an invalidation whose propagation reaches that module again is a
  full reload, so accept handlers that keep invalidating can't loop.
- **Partial accept (`hot.acceptExports(names)`, Vite's `experimental.hmrPartialAccept`).**
  `acceptExports` does not make a module self-accepting by itself: the scanner records the
  accepted names, merged and sorted over calls, along with the module's export names. A module
  that accepts all of its exports (or has none) is self-accepting. One that accepts only some
  is a boundary that also keeps propagating to its importers. With the flag on, the scanner
  also records which names each import binds (`*` for a namespace or dynamic import, none for
  a side-effect import or a re-export). An importer that uses only accepted names is skipped.
  Any other importer is reached as usual, so having no importers is not a dead end here.
  With the flag off, no bindings are recorded and every importer is reached. Propagation for
  such a change uses the accept info of the code that is running, which is the *old* code,
  captured before the new code is prepared. An update applies through the handlers already
  registered.
  (`vite: packages/vite/src/node/plugins/importAnalysis.ts`, the `acceptExports` and
  imported-bindings collection; `vite: packages/vite/src/node/server/hmr.ts`, `propagateUpdate`.)
- **Graph is built lazily from what the browser requests** (that is the unbundled-ESM graph).
  Nodes are never removed; ids stay valid. Edges are replaced wholesale on each prepare;
  importees that lose their last importer are sent in a `prune` message.
  A pruned module stays `pruned` until something imports or requests it again; changes to
  its file are ignored (a late watcher event from before the prune must not reload the page),
  except that they stamp its `?t=`, so an import restored later fetches the new file.
  Deleting a module pruned in an *earlier* batch, which doesn't accept itself, is a full
  reload (Vite keeps the module with no importers: a dead end;
  `vite: packages/vite/src/node/server/hmr.ts`, `handleHMRUpdate`/`propagateUpdate`); a
  deletion in the batch that pruned it (import removed and file deleted together) only prunes.
- **A change that fails to transform is propagated first.** If nothing accepts the update
  (a dead end), the page reloads and shows the error when it fetches the module again (Vite
  propagates before it transforms, so a broken import added to a module with no boundary
  reloads). Otherwise the error goes to the overlay and the module is marked errored
  (`vite: packages/vite/src/node/server/hmr.ts`, `handleHMRUpdate`).
- **Prune runs the module's dispose callbacks, then its prune callbacks**, after
  `vite:beforePrune`, queued behind any update in flight; then its `hot.data` is dropped, so
  a module imported again starts with fresh data (`vite: packages/vite/src/client/client.ts`
  and `hmr.ts`, `HMRClient.prunePaths`).
  A newly pruned module takes the update's timestamp, so an import restored later re-fetches
  it with `?t=` and does not reuse the disposed instance
  (`vite: packages/vite/src/node/server/hmr.ts`, `handlePrunedModules`).
- **Full-reload paths are percent-decoded** before they are matched against the page
  (`vite:` client `full-reload` handling compares decoded paths).
- **Changes are prepared eagerly** (transform before propagation), so accept info is current and
  the browser's `?t=` fetch is a cache hit. Changes and invalidations are serialized by
  `change_lock`; HTTP serving runs concurrently and only holds the graph lock briefly (never
  across I/O or transforms).
- **Same content = no update.** A change whose BLAKE3 equals the prepared hash (touch, double
  write, late duplicate event) sends nothing, unless the module is in an error state.
  A changed file that reads empty is re-read once its mtime moves (at most 100 ms), so a save
  seen between its truncate and its write doesn't reload the page (Vite's `readModifiedFile`).
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
  A stylesheet imported from JS exports nothing (Vite dropped its default export; the text
  is `?inline`'s default export). An app CSS module (`x.module.css`) is scoped by
  `css_modules` (local classes and keyframes, `:global`/`:local`, `composes` local, global
  and `from` a file; rules in that module's docs, from Vite's `plugins/css.ts` and
  postcss-modules' default naming): it exports the class map as its default and each name
  that is a JS identifier as a named export, and `?inline` gives the scoped text. Scoped
  names hash only the URL and the name (`_<name>_<hash5>`, or `<hash6>_<name>` with
  `css.transformer: 'lightningcss'`, `DevConfig::css_module_naming`), so editing a rule keeps
  the page's class names: the module still self-accepts, where Vite updates its importers
  (a class added by an edit appears after a reload). A dependency's CSS module still
  exports an empty map, so its importer links.
  Client → server: `custom` events (`vite:invalidate`, `ts-r:applied`, user events). A
  client connecting or disconnecting is passed to the plugin host as Vite's
  `vite:client:connect`/`vite:client:disconnect` events (only when a plugin listens).
- **Inline module scripts are modules.** The served HTML (after `%VAR%` and
  `transformIndexHtml`) has each non-empty inline `<script type="module">` replaced by a
  `src` to `{page}?html-proxy&index=N.js`; that URL's graph file is the pseudo path
  `{page file}?html-proxy&index=N.js` (same directory, so relative imports resolve), its
  source the N-th inline script of the page as last served (`Shared::html_pages`, dropped
  when the page file changes), rendered like any module. The hook is not run again for the
  script: a `transformIndexHtml` that is not pure (a new id per call) would give the script
  other text than its page. Each serve stores the page again, so a reload gets the new
  script (`vite: packages/vite/src/node/server/middlewares/indexHtml.ts` caches each inline
  script when it serves the page and invalidates its proxy module).
- **JSON goes through plugin `transform` hooks** (`vite: packages/vite/src/node/plugins/json.ts`):
  a `.json` import (not `?raw`/`?url`/worker) and a plugin module whose id ends in `.json`
  (`\0/data.json`) get the pre hooks on the JSON text, then become a module (named exports),
  then get the post hooks on the JS.
- **Static import attributes are removed** (`vite: packages/vite/src/node/plugins/importAnalysis.ts`):
  `import d from './d.json' with { type: 'json' }` (and `assert { ... }`, and `export ... from`)
  is served without the clause, since the server decides what a module is and serves JSON as
  JS. Left in, the browser rejects the JS MIME type. A dynamic `import(x, { with })` keeps its
  options (`render::import_attributes_span`).
- **`assetsInclude` files are assets.** A file with an unknown extension and no query goes
  to plugin `load`/`transform` as a plugin module, unless `DevConfig::assets_include` (a
  `glob::PathFilter` of root-resolved globs and regexes, filled from Vite's `assetsInclude`)
  matches it: then it is served as an asset URL like a `.png`.
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
  page URLs through `html_urls::rewrite`). Requests have the base stripped (`strip_base`): `/`
  redirects (302) to the base; any other path outside the base that isn't a server path
  (`/@...`) is a 404, an HTML page with a link to the based URL when the request accepts HTML,
  else text (`vite: packages/vite/src/node/server/middlewares/base.ts`).
- **Request order:** WebSocket upgrade (protocol `vite-hmr` → HMR; else a `ws: true` proxy
  rule → tunnel; else HMR), CORS preflight (`OPTIONS` → 204), proxy (matched on the raw URL,
  before the base is stripped), base, routes. Every non-proxied response goes through
  `finish_response`: `server.headers` replace same-named headers, then CORS. Proxied responses
  get CORS headers the upstream didn't send, never `server.headers`.
- **Proxying streams.** The upstream request uses `Connection: close`; the response body is
  copied by its framing (`Content-Length`, chunked, or to EOF, which then closes the client
  connection), never buffered whole. `Expect: 100-continue` is answered locally. An unreachable
  upstream is a 502 with an empty body. `rewrite` is an `Arc<dyn Fn>` (ts-r-dev calls the
  plugin sidecar), and so is `bypass`, run before forwarding: `false` is an empty 404 (no
  Content-Type), a written response is sent as is, a URL is served by the routes as if
  requested, a throw is logged and answered with an empty 500, and a rule with no target that
  isn't bypassed is a 500. WebSocket tunnels are raw byte pipes after the upstream's 101.
- **TLS (`server.https`, feature `tls`, on by default):** rustls with the `ring` provider,
  ALPN `http/1.1` (no HTTP/2). The handshake has a 10 s timeout. Outbound `https://`/`wss://`
  proxy targets verify against `webpki-roots` unless `secure: false`.
- **Env is reloadable state.** `Shared::env` (an `RwLock<Arc<Env>>`) holds the loaded `.env`
  files, `define` and mode; the `import.meta.env` prelude is added at render (not cached with
  the transform). A change to a `.env*` file in the env dir reloads it and sends `full-reload`
  when the result differs. The client runtime and `/@vite/env` carry the `define` globals.
- **`%NAME%` in HTML** takes `import.meta.env.NAME` from the env, or from a `define` of
  `import.meta.env.NAME`: a define that is a JSON string gives its text, any other gives the
  expression as written (`vite: packages/vite/src/node/plugins/html.ts`, `htmlEnvHook`).
- **Glob imports are expanded after the transform cache** (in `prepare_script`), so a new or
  deleted file matching a module's patterns (`Shared::glob_watches`) re-prepares that module
  and propagates like an edit. Before it, a template `import(`./dir/${x}.js`)` in app code
  (not plugin modules or `node_modules`) becomes a glob lookup through the
  `__variableDynamicImportRuntimeHelper` served at `/@id/__x00__vite/dynamic-import-helper.js`
  (`dynamic_import_vars.rs`; `vite: packages/vite/src/node/plugins/dynamicImportVars.ts`,
  with `@rollup/plugin-dynamic-import-vars`'s rules: the static parts become a `*` glob,
  `?query` suffixes are kept, a lone `${x}` or one without a directory is left alone). Then
  the **late stage** (`Stage::Late`: `enforce: 'post'` and `order: 'post'` `transform` hooks)
  runs on the expanded code, so those hooks see globs and template imports already rewritten,
  as Vite's user post plugins run after its glob and dynamic-import-vars plugins. The late
  stage is outside the transform cache, so it runs on every prepare; its watch files and maps
  join the module's like the other stages'. JSON runs it after the post stage; CSS never does. Aliases in patterns go through the plugin `resolveId` (pre).
  The rules taken from Vite (`vite: packages/vite/src/node/plugins/importMetaGlob.ts`):
  keys are relative only when every pattern starts with `.` or `!` and there is no `base`,
  otherwise root-absolute; glob characters in the importer's (or base's) directory names are
  escaped, so `src/[id]/` matches only itself; `caseSensitive: false` matches names ignoring
  ASCII case; a `#x` pattern maps through the nearest `package.json` `imports` (longest key
  wins; `browser`, `import`, `module`, `default` conditions); a match reached through a
  symlinked directory inside the root is imported by its real path. A full reload drops the
  prepared output of modules whose patterns match an added or deleted file, so their next
  request expands again.
- **Globs in pre-bundled dependencies** (Vite's optimizer keeps `import.meta.glob` calls) are
  expanded when the file is served, as if it sat at its URL under the root; such a file is
  sent with `Cache-Control: no-cache`, and a file added or deleted under its patterns
  (`Shared::dep_glob_watches`) reloads the page.
- **The public dir** (`public/` unless `publicDir`) is served at the base, before source files;
  importing `/x.png` from it gives its URL (`?import`, `encodeURI`d), never a module, and
  `?inline` gives its data URL. A public file matches only in the case it is written in
  (`ICON.png` doesn't serve `icon.png` on a case-insensitive disk). A file without an
  extension (public or in the root) is sent with an empty `Content-Type`, as Vite's static
  middleware does.
- **`//x` is not a file path**: it skips the public and root file serving and gets the HTML
  fallback (Vite's static middlewares skip URLs that start with `//`).
- **The server's own modules have two names and any query.** `/@ts-r/client` and
  `/@vite/client`, `/@ts-r/env` and `/@vite/env` (with or without a query) are served URLs:
  an import of them is kept under the base, never resolved as a file.
- **The client is `/@vite/client`**, injected and imported under that name (Vite's playground
  tests look for it); `/@ts-r/client` re-exports it, so both names share one module instance
  (one socket, one `import.meta.hot` registry). The client imports `/@vite/env` for the global
  `define`s, as Vite's does: the same module graph, so a page's modules and the socket's
  `connected` race as they do under Vite (hmr's `acceptExports([])` tests read that order).
- **Stylesheet identity is the absolute file path.** `<style data-vite-dev-id>` and
  `<link data-vite-dev-id>` carry `paths::id(file)`, as Vite's do. The client adopts the
  styles and links the page was served with (by that id) instead of adding a second copy;
  a stylesheet the page links is never also inserted as a `<style>`; `updateStyle` sets the
  text only when it changed and inserts new styles after the last one it inserted (Vite's
  order: `vite: packages/vite/src/client/client.ts`, `updateStyle`).
- **Linked CSS is pruned like a module.** A `<link>`ed stylesheet whose last JS import is
  removed gets a `prune` for its URL, and the client removes its link.
- **A deleted module lets go of its imports** (`ModuleGraph::detach`, Vite's
  `moduleGraph.ts`, `onFileDelete`): its dependencies lose it as an importer, so they can be
  pruned, and restoring the file starts it fresh.
- **The watcher hears about every file the page uses** (`WatchAdder::known`, once per node:
  `ModuleNode::known_to_watcher`): scripts, stylesheets and assets, on their first serve. A
  known file the debouncer sees flagged as created is a modification while it exists and a
  removal once it is gone.
- **A file creation retries modules that failed** (`ModuleNode::retry_on_create`): every
  `Created` change re-prepares them, whether or not the graph knows the new file (a deleted
  file restored), like Vite's `_hasResolveFailedErrorModules` in `handleHMRUpdate`. A retry
  takes a module out of the set, as Vite's `invalidateModule` does; only a later failure the
  page asked for (a request, or an update the page keeps) puts it back. A retry that reloads
  the page leaves it out: the reloaded page asks for it again if it still uses it.
- **Plugin watch files** (`this.addWatchFile` in `load` and `transform`): kept per file in
  `Shared::plugin_watch`, their contents join the module's transform cache key, and editing
  one re-runs the hooks and updates the module. Stylesheets go through the plugins'
  `transform` hooks too (pre on the source, post on the built-in step's CSS; the sidecar runs
  only normal-order hooks there), and their watch files join the stylesheet's dependencies.
  `load` runs for regular files when a `load` hook has no filter (`PluginHooks::load_files`).
- **The transform cache is single-flight**: concurrent misses for one key run the transform
  once and the others wait for its result (`cache.rs`, `Flight`); a result whose watch files
  change its key is stored under the new key. The dependency scan shares `cached_transform`,
  so it never runs a module's plugin transforms a second time. The run records its watch
  files in `Shared::plugin_watch` before it publishes its result: a waiter reads them as
  soon as it wakes, and a module registered without them never updates when they change.
- **The config's `process.env` writes count.** A `vite.config.*` runs in the plugin sidecar,
  so variables it sets on `process.env` are returned with the dev options (`processEnv`) and
  overlaid on the server's environment when `.env` files and prefixes are loaded. The
  sidecar's `define` also merges `environments.client.define`.
- **Never served from the root:** `.env`, `.env.*`, `*.crt`, `*.pem` and anything under
  `.git` (403), like Vite's default `server.fs.deny`.
- **HTML fallback** (`vite: packages/vite/src/node/server/middlewares/htmlFallback.ts`): a
  GET that matched nothing and accepts `text/html` or `*/*` (not `/favicon.ico`) gets the page
  `<path>.html` when it exists, else the root `index.html`. A directory's `index.html` is
  served only for a path ending in `/`: `/dir` tries `dir.html`, then the root page. Pages
  are sent as `text/html` (no charset, as Vite does).
- **App CSS** (`server/app_css.rs`, `vite: packages/vite/src/node/plugins/css.ts`,
  `urlResolver`/`rewriteCssUrls`/`rewriteCssImageSet`): linked and JS-imported stylesheets in
  the root, and a page's `<style>` blocks and `style` attributes, have `@import`s inlined
  (resolved like references, through aliases and packages) and every `url()`/`image-set()`
  reference rewritten. A reference is URL-decoded; a public file becomes `<base><path>`;
  otherwise it is resolved (`/x` from the root, `./x` and `x` from the stylesheet, then the
  plugins' `resolveId` rewrites and the resolver). `?inline`, and an SVG under
  `assetsInlineLimit` with no fragment and no `?no-inline`, become data URLs; anything else is
  its served URL with the base, the query and fragment kept and `t=<last change>` once the
  file has changed. An unresolved reference is kept as written. Quoting follows Vite: an
  unquoted value that needs quotes gets `'...'`, a quoted one keeps its quote.
  Every file read or referenced is a watch dependency of the stylesheet (and of the page,
  for its `<style>`s: changing one reloads all connected pages, since retained page
  dependencies may overlap other pages and linked stylesheets).
- **Page URLs** (`html_urls.rs`, `vite: packages/vite/src/node/server/middlewares/indexHtml.ts`
  `processNodeUrl`, `packages/vite/src/node/assetSource.ts`): the attributes that name files
  (Vite's table: `src`, `srcset`, `href` of `link`/`image`/`use`, `imagesrcset`, `data`,
  `poster`, `content` of image `meta`s) and the string argument of `import('...')` in classic
  inline scripts get the base when root-absolute; relative ones too, for the root `index.html`
  served at another URL. `vite-ignore` elements are left alone (the attribute is removed).
- **Asset import URLs** (`asset_export_url`, `vite: packages/vite/src/node/plugins/asset.ts`
  `fileToDevUrl`): `import x from './a.svg?no-inline&foo=bar'` exports
  `<base>/a.svg?no-inline&foo=bar`: the path `encodeURI`d, the query kept without `import`,
  `url` and `t`. The same rule applies when a plugin module of a known asset falls back to
  the default load. `assetsInlineLimit` (ts-r-dev reads it from the Vite config) sets the SVG
  inline limit; its function form is `DevConfig::assets_inline_fn` (`true`/`false` decide,
  `None` falls back to the limit), used for script imports and CSS `url()`s alike.
- **`html.cspNonce`** (`DevConfig::csp_nonce`, `render::with_nonce`; observed on Vite's csp
  playground): a page gets `<meta property="csp-nonce" nonce="N" />` after the client script,
  and every `<script>`, `<style>` and `<link rel="stylesheet|modulepreload|preload">` without
  a `nonce` gets `nonce="N"`. Script and style contents and comments are skipped.
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
- **A plugin module's `load` result is kept** (`Shared::plugin_loads`, by id) until the
  module is invalidated: its file (or the file behind its id) changed, it is re-prepared
  (`reprepare`), the host calls `ServerHandle::reload_module` (`server.reloadModule`), or
  `ServerHandle::forget_load` (`moduleGraph.invalidateModule` / `invalidateAll`). An
  importer's update fetching it again with `?t=` reuses the kept code, like Vite's soft
  invalidation, so a `load` that counts calls sees one per invalidation
  (`vite: packages/vite/src/node/server/moduleGraph.ts`, `invalidateModule`).
  `reload_module(id)` finds the node by `/@id/` URL or file, re-prepares it and sends the
  update as for a changed file; the handle runs it on its own thread, since the host's reader
  thread must never block (`vite: packages/vite/src/node/server/index.ts`, `reloadModule`).
- **Wasm** (`wasm.rs`, `vite: packages/vite/src/node/plugins/wasm.ts`; the direct-import
  behaviour from the wasm playground's specs): `x.wasm?init` exports a function
  `opts => init(opts, url)` that instantiates the file with the given imports (the helper at
  `/@id/__x00__vite/wasm-helper.js` streams it when the response is `application/wasm`,
  decodes `data:` URLs, and passes the js-string builtins compile options). A plain
  `import { f } from './x.wasm'` (ESM integration) reads the binary's import and export
  sections (bounded LEB reads; `wasm:js-string` builtins skipped), imports each import
  module, instantiates with top-level await and exports the instance's exports; exported
  globals unwrap `.value`, served through a `?vite-wasm-instance` module so a wasm file that
  imports another wasm's globals shares one instance. The glue is a graph module
  (`<file>?wasm-module.js` / `?wasm-instance.js`) so its imports hot-update like a script's.
  `?url` and a plain fetch serve the binary.
- **Workers** (`worker.rs`, `vite: packages/vite/src/node/plugins/worker.ts`): `?worker` /
  `?sharedworker` imports export a function constructing `Worker`/`SharedWorker` on the file's
  URL with `worker_file&type=module` first in the query and the import's other parameters
  after it (`&url` exports the URL; `&inline` means nothing in dev). A `worker_file` request
  serves the script with `/@vite/env` run first (`importScripts` for `type=classic`).
  `new Worker(new URL('./w.js', import.meta.url))` (and `SharedWorker`) is found by the
  scanner (`HotInfo::asset_url_workers`): the URL is never inlined and gets
  `?worker_file&type=classic`, or `type=module` when the options are an object literal with
  `type: 'module'`; `/* @vite-ignore */` leaves it alone. A classic worker gets no hot or env
  import prelude, and when it has non-literal dynamic imports an inline
  `__vite__injectQuery` function is appended (it can't import the client).
- **`optimizeDeps.exclude`** (`DevConfig::optimize_exclude`): a named package (or a subpath of
  it) is never pre-bundled: it resolves to its files and is served module by module.
- **`DevServer::url()`** encodes the base (`encodeURI`), so a base with spaces or non-ASCII
  characters gives a working URL. An asset's `?import` module exports its URL with
  `?t=<last hot update>` after a hot update, as Vite's does.
- **Dynamic imports** (`vite: packages/vite/src/node/plugins/importAnalysis.ts`,
  `dynamicImportVars.ts`): a non-literal `import(expr)` argument, and a template whose text
  names neither a script nor CSS (`` `./${x}.json` ``), is wrapped as
  `__vite__injectQuery(arg, 'import')` (client export) so the request gets `?import` at run
  time. A template's head that is an alias or a `#subpath` import is resolved to its URL
  (`` `@/${x}.js` `` → `` `/src/${x}.js` ``); relative heads are left for the browser.
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
  and Vite config/plugin loading live in `ts_r_dev`. Plugin seam gaps: filtered `load` hooks
  aren't called for plain script, CSS, JSON or asset files; JSON and asset modules' watch
  files aren't tracked; no `emitFile`; plugin source maps
  are composed but asset/JSON modules carry no map; middlewares run only for requests no built-in route serves (and non-GET
  requests), with buffered bodies (no streaming/SSE); the dependency scan resolves through `resolveId` and aliases but doesn't look
  through plugin modules (their bare imports are found on request: re-batch + reload).
- Without a batch-capable `DepBundler`, CJS packages fail with an explanatory error and a
  dependency costs one request per file. Nested `node_modules` (`a/node_modules/b`) are
  re-resolved from the root by `/@modules/b/...` and may pick the wrong copy.
- Dep scan: only `<script type="module" src>` in `index.html` (no inline module scripts, no
  multi-page `build.rollupOptions.input`); imports behind non-literal `import(expr)` are found
  only on request (re-batch + reload).
- Non-literal dynamic `import(expr)` that isn't a template with a relative or aliased head
  only gets `?import` at run time (bare specifiers in it fail); an HTML
  change reloads the page but doesn't invalidate its inline proxy modules by themselves; in app
  (source) CSS, preprocessor `@import`s (`.scss`, ...) aren't handled, and CSS modules ignore
  `css.modules` options (`generateScopedName`, `localsConvention`, ...). Dependency CSS isn't watched (only linked packages).
- `new URL(`./${name}.png`, import.meta.url)` (a template with expressions) isn't turned into
  a glob as Vite does; only string literals are resolved. `import.meta.glob` needs literal
  arguments (as in Vite).
- `base`: dependency batch files and dependency CSS-as-JS still link `/@ts-r/...` without
  the base; they work only because server paths (`/@...`) outside the base are still served.
- Proxy: no `configure`, `bypass`, `agent`, `auth` options or `xfwd`; `cors.origin` as a
  function falls back to the default. No `server.hmr` options (port, path, overlay) yet.
- Output differs cosmetically from Vite's (quote style of rewritten imports, Vite reprints
  plain JS, `'' + import.meta.url` in `new URL`); the tests compare semantics.
- First save of a file on macOS pays FSEvents latency (~11 ms); Linux/Windows have no fast path
  (inotify is already fast; Windows untested).
- Resolution results are cached with the prepared module; a newly created file that would
  change how an existing import resolves (e.g. adding `x.ts` next to `x.js`) is picked up only
  when the importer changes. Errored modules are retried when a file is created.
- `full-reload` latency isn't measured (no acknowledgement after a reload).
- HTTP/1.1 only (also over TLS: no HTTP/2), no compression.
