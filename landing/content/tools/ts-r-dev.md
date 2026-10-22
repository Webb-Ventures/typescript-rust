---
title: "`ts-r-dev`: dev server with hot reload"
description: A dev server with hot module replacement and Vite's import.meta.hot API. No vite.config support yet.
nav: ts-r-dev
section: Tools
order: 1
badges: ["warn:Alpha", "ok:On npm: @ts-r.dev/dev"]
---

`ts-r-dev` serves your app as unbundled ES modules, transforms only the file you save, and
pushes the update to the browser. It speaks Vite's hot-reload protocol, so code written against
`import.meta.hot` works unchanged. It replaces `vite dev` for the dev loop; it doesn't replace
`vite build` (see [ts-r-build](ts-r-build.md)).

## What works today

- An HTTP and WebSocket server with a module graph built from what the browser requests.
- Hot module replacement with Vite's semantics: self-accepting modules, `hot.accept('./dep')`,
  `hot.decline()`, `hot.invalidate()`, `hot.data`, `hot.dispose` and `hot.on` events. When no
  module accepts an update, the page does a full reload.
- TypeScript, TSX and JSX through ts-r's own transform; JSON, assets, plain CSS and
  CSS imported from JavaScript, with CSS updates applied without a reload.
- React Fast Refresh when the project has a React Refresh runtime installed
  (`react-refresh/runtime`, or Next's copy). Top-level components keep their state across edits.
- Dependencies pre-bundled in one code-split batch with ts-r's bundler at startup, cached per
  lockfile in `~/.cache/ts-r/dev-deps`. `--force` rebuilds the batch.
- An error overlay (`<vite-error-overlay>`) for transform and resolve errors. The server never
  crashes on a bad file; the next good save clears the error.
- pnpm workspace packages linked into the app are treated as source and hot-update like app code.

## Not yet

- No `vite.config` loading and no Vite plugin API (`resolveId`, `load`, `transform`,
  `handleHotUpdate`).
- No `--open`, no HTTPS, no proxy, no CORS headers, no compression. HTTP/1.1 only.
- CSS `@import` and CSS modules (`.module.css`) aren't handled.
- Inline `<script type="module">` in HTML and non-literal `import(expr)` aren't rewritten.
- The dependency scan only follows `<script type="module" src>` in `index.html`.
- Fast Refresh registers only top-level components; an anonymous `export default () => ...`
  isn't registered, so editing it updates its importer instead.
- Windows is untested.

The full list is in the [ts_r_hmr](../../../crates/ts_r_hmr/AGENTS.md) and
[ts_r_dev](../../../crates/ts_r_dev/AGENTS.md) notes.

## Command line

```text
ts-r-dev: dev server with hot module replacement (replaces vite dev)

Usage: ts-r-dev [root] [options]

Options:
  --port <n>         Port to listen on (default 5173; tries the next ones if taken)
  --strict-port      Fail if the port is taken
  --host <addr>      Address to bind (default 127.0.0.1)
  --no-cache         Keep transform results in memory only (no ~/.cache/ts-r)
  --cache-dir <dir>  Use <dir> as the cache root
  --debounce <ms>    Watcher quiet window in milliseconds (default 3)
  --no-watch         Don't watch files
  --force            Pre-bundle dependencies again, ignoring the cached batch
  --no-react-refresh Turn off React Fast Refresh (on when a React Refresh runtime is installed)
  --quiet            Don't log updates
  -h, --help         Show this help
  -V, --version      Show the version
```

- `root` is the directory holding `index.html` (default: the current directory).
- `--port 0` picks any free port. Without `--strict-port` the server tries the next ports.
- `--debounce` is the quiet window after a file event before the update is prepared. Editors
  that save in several steps need it; 3 ms is the default.
- Once listening, the server prints `  Local: http://127.0.0.1:5173/`. Each update prints one
  line with a per-step timing breakdown (watch, debounce, read, transform, graph, push,
  browser, total). `/@ts-r/stats` returns p50 and p95 per step.

Exit codes: 0 for `--help` and `--version`, 2 for a usage error, 1 when the server can't start.

You can also run it through the launcher as `ts-r dev`, which passes the arguments on unchanged.

## Caches

Transform results are cached by content hash in `~/.cache/ts-r/dev-transform` (with
`--cache-dir`, under that directory instead). `--no-cache` keeps them in memory only. Cache keys
never use file times, and cached data holds no absolute paths.

## Next to Vite

`ts-r-dev` works on its own: you can use it for the dev loop and keep Vite (or anything else)
for production builds, tsc for type checking and ESLint for lint. It reads your `index.html`
and `package.json`; it doesn't read `vite.config` yet, so projects that depend on Vite plugins
other than React Fast Refresh won't work.

Because the client runtime is served at both `/@ts-r/client` and `/@vite/client` and the
messages match Vite's, framework code that listens to `vite:` events keeps working.

## Measured

On a generated app, save to browser updated at p50 stays flat as the app grows: 4.9 ms at
1,000 modules, 4.8 to 5.0 ms at 10,000 and 5.4 ms at 50,000 (Apple M-series Mac, release
build, headless Chrome, 47 measured edits per size). The target is under 30 ms.

On a real app, [excalidraw](https://github.com/excalidraw/excalidraw) (about 500 TypeScript
modules served from source, pinned in `bench/repos.toml`), against Vite 8.3.4 with
`@vitejs/plugin-react` 6.1.2 on the same Mac (a shared laptop, 5 cold and 5 warm starts, 50
edits). SCSS is replaced by an empty stylesheet for both servers, because ts-r-dev doesn't
compile it yet:

| | ts-r-dev | Vite 8.3.4 |
|---|---|---|
| Cold start to first render (median) | 939 ms | 1,107 ms |
| Warm start to first render (median) | 538 ms | 1,032 ms |
| Save to DOM updated, p50 / p95 | 8.6 / 9.0 ms | 35.0 / 37.2 ms |
| Server memory after load / after edits | 151 / 153 MB | 371 / 364 MB |

Both kept React state on all 53 edits with no reloads. ts-r-dev's memory includes the Node
process that runs the app's `vite.config` (its aliases). Method and commits:
[hot reload latency](/benchmarks/#hmr) and
[ts-r-dev against Vite](/benchmarks/#dev-vs-vite).
