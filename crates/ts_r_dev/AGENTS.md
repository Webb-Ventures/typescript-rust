# ts_r_dev

The `ts-r-dev` binary (`[[bin]] name = "ts-r-dev"`): the HMR dev server, replacing `vite dev`.
`ts-r dev` (the [`ts_r_cli`](../ts_r_cli/AGENTS.md) launcher) execs it with arguments
untouched. All server logic lives in [`ts_r_hmr`](../ts_r_hmr/AGENTS.md); this crate is
argument parsing, startup output, wiring (TS transform from `ts_r_transform`, dependency
batches from `ts_r_bundler`, React Fast Refresh in `src/react_refresh.rs`) and the
end-to-end tests. Root rules:
[../../AGENTS.md](../../AGENTS.md).
Design record: [0010](../../docs/design/0010-dev-server.md).

## CLI

`ts-r-dev [root] [--port <n>] [--strict-port] [--host [addr]] [--mode|-m <mode>] [--base <path>]
[--open [path]] [--no-cache] [--cache-dir <dir>] [--debounce <ms>] [--no-watch] [--force]
[--no-react-refresh] [--no-plugins] [--quiet]` (`--opt=value` works too). Defaults: root `.`,
`127.0.0.1:5173` (next free port unless `--strict-port`; `--port 0` = any; `--host` alone =
`0.0.0.0`), mode `development`, transform cache in `~/.cache/ts-r/dev-transform`,
dependency batch in `~/.cache/ts-r/dev-deps` (`--force` rebuilds it and re-reads the plugin
manifest). Flags override `vite.config` (`server.host`/`port`/`strictPort`/`open`, `base`,
`mode`). `--open` launches `$BROWSER` or the system opener (`open`/`xdg-open`/`start`; `BROWSER=none`
skips it).
Prints `Local: http://…/` once listening; update lines (with the per-step latency breakdown)
go to stdout. Exit codes: 0 help/version, 2 usage error, 1 startup failure.

## React Fast Refresh (`src/react_refresh.rs`)

On when the project has a React Refresh runtime (`react-refresh/runtime`, or Next's
`next/dist/compiled/react-refresh/runtime`) and `--no-react-refresh` isn't given. The runtime
is React's, pre-bundled with the other deps (`optimize_include`); our parts are a
`Transform` wrapper around `TsTransform` (re-parses the JS output with `ts_r_parser`), the
`/@ts-r/react-refresh` glue module and an HTML preamble importing it before the app.

- Registers top-level components (capitalised functions/arrows, `export default function
  Named`) as `"<id> <name>"`, and HOC calls: a capitalised binding or `export default` whose
  initializer is a call taking a function, a capitalised identifier or another such call
  (`memo`, `forwardRef`, `connect(..)(X)`, `withBox(Inner)`, nested up to 16 deep). Each call
  result is registered (`%default%` for default exports) and an inline function argument too,
  as `<outer>$<callee>` (`%default%$memo$forwardRef`), with its own hook signature. Anonymous
  default functions/arrows and classes stay unregistered, so editing them remounts via the
  importer, as React documents; hook signatures (hook callees, bindings, `useState`/
  `useReducer` args, custom hooks) so changed hooks remount; `// @refresh reset`, and a
  custom hook that isn't a top-level binding, force a remount on every edit.
- A module registering components self-accepts; the update refreshes in place when all its
  exports are components with unchanged names, else `invalidate()` (Vite plugin semantics).
- Insertions never add lines: prelude on line 1, `__tsr_sN()` calls mid-line, footer after
  the code. The source map is fixed by shifting columns (VLQ re-encode). `node_modules` is
  never wrapped. Bump the wrapper's `cache_id` suffix (`+react-refresh/3`) when output changes.
- Generated names share a `__tsr_` prefix, or `__tsrN_` when the module already contains
  `__tsr_` (no collisions); a module mentioning `$RefreshReg$`/`$RefreshSig$` is left alone.

## Vite plugins (`src/vite_plugins.rs`, `src/vite_sidecar.mjs`)

With a `vite.config.{js,mjs,ts,mts,cjs,cts}` in the root (and no `--no-plugins`), a Node
sidecar (`node`, or `TS_R_NODE`) imports it, flattens and orders `plugins` (`apply`,
`enforce`, hook `order`, hook `filter`s), runs `config` → `configResolved` →
`configureServer` → `options` → `buildStart`, and implements `ts_r_hmr::PluginHost` over JSON lines
(stdin/stdout; plugin stdout is moved to stderr). Stdin is read while the startup hooks run,
so a `this.resolve` there gets its answer (null before the dev server attaches); other
requests wait for startup. Hooks: `resolveId` (with `this.resolve`
falling back to our resolver, and `resolve.alias`), `load`, `transform`,
`transformIndexHtml` (strings, tags, `{ html, tags }`), `handleHotUpdate`/`hotUpdate`,
connect-style middlewares, `server.ws.send`/`on`. `this.meta` carries the installed Vite's
and Rolldown's versions (plugins pick APIs by it). The sidecar script is written by us
from the documented plugin API (no Vite code).

- **Hook filters are read on every call**, so a filter an `options` hook (or any earlier
  hook) assigns is the one used (`vite: packages/vite/src/node/server/pluginContainer.ts`
  runs `options` once before the build starts).
- **Module types** (`vite: packages/vite/src/node/server/transformRequest.ts`,
  `getModuleTypeFromId`): a pre-stage `transform` gets the type guessed from the id (the
  first `.ext` up to the end that is known: `js`, `jsx`, `ts`, `tsx`, `json`, `text`,
  `css`; else `js`), a post-stage one `js` (our built-in step made JS, as Vite's JSON and
  TS plugins return), and a hook returning `moduleType` changes it for the hooks after it.
  A `filter.moduleType` list (or `{ include }`) skips a hook for other types. A stylesheet's
  post stage is the exception: it runs on CSS (type `css`) and only the normal-order hooks,
  the ones Vite runs between `vite:css` and `vite:css-post`
  (`vite: packages/vite/src/node/plugins/index.ts`, `resolvePlugins`); `enforce: 'post'` and
  `order: 'post'` hooks, which see the JS wrapper there, are skipped.
- **`this.addWatchFile` / `this.getWatchFiles`** in `load` and `transform`: the context
  collects the files per call, and `load`/`transform` answers carry them (`watchFiles`; a
  transform that only watched files answers with `code: null`). The server keys and watches
  them (`ts_r_hmr`, "Plugin watch files"). `hooks.load_files` says a `load` hook has no
  filter, so regular files go through it too.
- **An ESM config in a CommonJS package** (`vite.config.js` with `import` in a
  `"type": "commonjs"` package): when importing it throws a `SyntaxError`, the sidecar copies
  it next to itself as `<config>.timestamp-<pid>-ts-r.mjs` (`.mts` for TS), imports that and
  removes it in a `finally` (`vite: packages/vite/src/node/config.ts`,
  `loadConfigFromBundledFile` writes a temp `.mjs` the same way).
- **`/plugin-react` is replaced, not run:** plugins named `vite:react*`
  / `@vitejs/plugin-react*` only run their `config` hooks; JSX options from the resolved config
  (`oxc.jsx.importSource`/`runtime`, `esbuild.jsx*`) configure `TsTransform`, and Fast Refresh
  is ours.
- **Manifest cache** (`ts_r_cache` namespace `dev-plugins`, key: config and the local files
  it imports (relative specifiers, transitively) + `package.json` + lockfile contents +
  sidecar script; plugin transforms are cached under the same key): plugin names, hooks
  present, JSX options. No hooks left
  (plugin-react only) → Node never starts once cached. Hooks cached → Node starts in the
  background and the server is ready at once (early hook calls wait in its stdin); a manifest
  that changed is re-cached with a "restart" note. `--force` re-reads it.
- **Limits:** `--max-old-space-size` (`TS_R_PLUGIN_MEM_MB`, default 2048, max 8192), per-call
  timeout (`TS_R_PLUGIN_TIMEOUT_S`, default 30 s) covering the request write and the reply (a
  timed-out call kills the sidecar), killed on drop, exits when its stdin
  closes (ts-r-dev killed). Startup logs `plugins: <names> from <config> (Node sidecar pid N, T
  ms); built in: <replaced>` plus warnings for config that isn't applied (a `cors.origin`
  function, regexes `regex-lite` can't compile).
- **Dev options from the config** (manifest field `dev`, computed by the sidecar after
  `configResolved`): `mode`, `base`, `define` (expression text; non-string values JSON-encoded), `envPrefix`, `envDir`,
  `publicDir`, `server.{host,port,strictPort,open,headers,cors,proxy,https}`,
  `experimental.hmrPartialAccept` (as `hmrPartialAccept`), `css.transformer` (as
  `cssTransformer`, the shape of CSS modules' class names), applied by
  `apply_dev_options`. `envDir`/`publicDir` are kept relative to the root when given relative
  or inside it (checkouts with the same config share the cached manifest) and resolved
  against ts-r-dev's root. A proxy `rewrite` function stays in Node: the rule records its index and
  each proxied request calls the sidecar's `proxyRewrite` op (so a config with a `rewrite`
  always starts the sidecar). A `bypass` function works the same way (`proxyBypass`): it gets
  a request (headers, no body) and a fake `res`; what it wrote, or what it returned (`false`,
  a URL, a throw), goes back as a `proxy::Bypass` (`vite: packages/vite/src/node/server/middlewares/proxy.ts`).
  A rule with a `bypass` and no `target` is kept. `server.https` `cert`/`key` are cached as PEM text in the
  manifest (that is how plugin-generated certificates, e.g. `@vitejs/plugin-basic-ssl`, survive
  a warm start without Node); the cache lives in the user's `~/.cache/ts-r`.
  `TS_R_MODE` passes the mode to the sidecar; the mode is part of the manifest key.
- **Plugin source maps:** `transform` returns each hook's map (`{code, maps}`), and the server
  composes them with ours (see `ts_r_hmr`).
- **Cold start overlaps Node with Rust setup:** `VitePlugins::begin` spawns the sidecar first,
  `main` builds the TS transform, guard options and refresh runtime meanwhile, then
  `Starting::finish` waits for the manifest. The sidecar enables Node's module compile cache
  (`module.enableCompileCache`, Node ≥ 22.1), so the second cold run of a config that imports
  `vite` skips most of its parse/compile.

## `parse<T>` (`src/guard.rs`)

`GuardTransform` wraps the TS transform: files importing `@ts-r.dev/guard` are expanded by
`ts_r_guard::Expander` first, validators inlined per file exactly as in `ts-r-run` (record
0007); guard errors (TSR1501, ...) go to the error overlay with their location. Cache id
suffix `+ts-r-guard/1/<explain>/<max-depth>/<max-validator-kb>` plus `/<Type>=<module>#<export>` per refinement. `Transform::deps` reports the program's other sources (the files
the validators were generated from), so editing only a type re-emits and hot-updates every
module that parses it. The expansion `deps` computes is memoized per path and source hash and
handed to the `transform` that follows (one expansion per change); when the source is
unchanged, `deps` only re-reads the dep files and re-expands if their contents changed (a type
file that starts importing another changes the list). Wrappers (React Refresh) forward `deps`.

## Invariants

- No server logic here: anything a test or another embedder needs belongs in `ts_r_hmr`.
- Startup prints the URL line in exactly the form `  Local: <url>` (the tests parse it).
- Hand-rolled argument parsing (no clap), like the launcher.

## Dependencies

- `ts_r_hmr`, `ts_r_transform` (feature `hmr`), `ts_r_bundler` (feature `hmr`), and
  `ts_r_parser`/`ts_r_ast`/`ts_r_codegen` for the refresh transform (path).
- `ts_r_guard`, `ts_r_config`: `parse<T>` call sites; `ts_r_sound` (only
  `config::guard_options`, `[sound.guard]` read at startup and part of the cache id).
- `ts_r_cache`: the plugin manifest cache.
- `serde_json`: source maps in the refresh transform, CDP messages in tests (approved list). Also the
  sidecar protocol.
- `ts_r_testing` (dev): memory cap for the plugin tests.

## Testing

- `cargo test -p ts_r_dev`: CLI parsing unit test; `tests/e2e.rs` starts the binary on the
  fixture app (`ts_r_hmr::testutil::write_fixture_app`), opens it in headless Chrome over the
  DevTools protocol (our own WebSocket client, `tests/support/mod.rs`), and checks: initial
  load (deps, JSON, assets, both CSS kinds), self-accepting update via atomic save with
  `hot.data` and `dispose`, `hot.on` events firing once (stale listeners removed), dep accept,
  CSS updates, error overlay and recovery, all without a reload (a per-page marker survives),
  then a full reload when no boundary exists, and the logged latency lines. A second test throws
  inside a pre-bundled CommonJS dependency and maps Chrome's stack frame back to the line in
  the original `node_modules` file through the batch's source map (cold, then warm from disk).
  A third loads CSS from packages (a dependency's own `import './x.css'` with an `@import`
  and a `url()`, a stylesheet imported from a package hoisted above the root, and a linked
  package's CSS) and hot-updates the linked one without a reload.
  `tests/refresh.rs` (needs `TS_R_REACT_ROOT`, a directory whose
  `node_modules` has react, react-dom and react-refresh) edits one component per way of
  defining it and checks React's keep-state/remount rules in Chrome: named default export,
  `memo`, `forwardRef`, nested `memo(forwardRef(function ..))` and HOC exports keep state;
  anonymous default exports, classes and `// @refresh reset` remount; a hook change remounts.
  `tests/plugins.rs` (needs `node`): a config with virtual modules, pre/post transforms, hook
  filters, `this.resolve`, aliases, a `.md` transform, HTML tags, GET/POST middlewares,
  `handleHotUpdate` and `ws.on`/`send`, over HTTP and in Chrome (hot updates of plugin
  modules), and checks the sidecar dies with the server; a plugin-react-only config never
  starts Node once cached; with `TS_R_VITE_TOOLS` + `TS_R_REACT_ROOT`, vite-plugin-svgr
  (`?react`) and `react({ jsxImportSource })` render in Chrome, an SVG edit hot-updates with
  state kept, and a warm restart starts the sidecar in the background.
  The Chrome tests skip with a
  message when Chrome is absent (`TS_R_CHROME` overrides the path).
- Benchmark: `scripts/limit.sh -- cargo test --release -p ts_r_dev --test latency -- --ignored
  --nocapture` (`TS_R_LATENCY_SIZES=1000,10000,50000`, `TS_R_LATENCY_EDITS=50`). `limit.sh`
  bounds the server and Chrome too; the test binary itself calls `ts_r_testing::cap_memory!()`. Generates apps of N modules,
  edits a self-accepting module deep in the tree, and reports browser-measured save→update
  p50/p95 plus the server's per-step breakdown. Results are in `ts_r_hmr`'s AGENTS.md.

- Real app: [`bench/dev/dev.mjs`](../../bench/dev/dev.mjs) runs excalidraw (the public Vite +
  React app pinned in `bench/repos.toml`; `cargo xtask bench --fetch-repos --repos excalidraw`
  copies `bench/dev/excalidraw/` into the checkout as `ts-r-bench-dev/`: an `index.html`, a
  `main.tsx` rendering `excalidraw-app/App`, and a `vite.config.mjs` with excalidraw's source
  aliases) under ts-r-dev or Vite 8.3.4 in headless Chrome. `--mode check`: renders with no
  console errors, edits the welcome-screen heading and checks the update lands without a
  reload with the DOM node kept. `--mode bench`: cold/warm start to first render, save→update
  p50/p95, server RSS. Everything runs under `scripts/limit.sh`; see
  [bench/README.md](../../bench/README.md).
- The React Refresh runtime is looked up from the canonical root, so `ts-r-dev .` in an app
  folder whose `node_modules` is in a parent directory (excalidraw's harness, monorepo apps)
  still gets Fast Refresh.

## Gaps

- Vite plugins: the seam gaps in [`ts_r_hmr`](../ts_r_hmr/AGENTS.md#gaps) apply; `this.parse`,
  `this.load`, `ssrLoadModule` throw; `server.moduleGraph` only knows modules a plugin's `load` produced (`getModuleById`, a minimal module record); `invalidateModule`/`invalidateAll` drop the server's kept `load` result and `server.reloadModule` (also on `environments.client`) re-loads and hot-updates the module (sidecar messages `forget`, `forget_all`, `reload`); `server.watcher` only
  sees changes that reach `handleHotUpdate`. The sidecar costs ~25 ms to start with inline plugins and
  ~80–440 ms when the config imports `vite` (cold disk cache); with a cached manifest that happens in the background after "ready".
- From `vite.config`: no `css.*` (preprocessor options, CSS modules), `server.hmr`,
  `server.fs`, `server.watch`, `optimizeDeps.exclude`/`esbuildOptions`, proxy
  `configure`, `cors.origin` functions. A changed certificate file is picked up only
  when the manifest key changes (config, `package.json`, lockfile) or with `--force`. No Ctrl-C cleanup beyond the
  OS reclaiming resources (the transform cache flushes on drop only when the server stops
  normally; unflushed entries are just misses next time).
- The latency benchmark uses generated JS; the real-app benchmark (results in `ts_r_hmr`'s
  AGENTS.md) edits one file. Vite's own playground specs run against both servers in
  [`bench/dev-oracle`](../../bench/dev-oracle/README.md); its README lists the failing tests
  by cause (today one: glob and dynamic-import-vars output isn't visible to post plugins).
- No CSS preprocessors: the excalidraw harness maps `.scss` to an empty stylesheet for both
  servers, so the page renders unstyled.
- Fast Refresh: components created inside other expressions (object literals, conditionals,
  `const A = cond ? X : Y`) and lowercase-bound HOC results aren't registered, so editing
  them invalidates to the importer; no Babel-plugin output parity tests.
