---
title: "`ts-r-build`: bundler and minifier"
description: An esbuild-compatible bundler and minifier. Same flags as esbuild 0.28.2, ES2022 and later targets, no plugins or CSS bundling yet.
nav: ts-r-build
section: Tools
order: 2
badges: ["warn:Alpha", "ok:On npm: @ts-r.dev/build"]
---

`ts-r-build` bundles and minifies TypeScript and JavaScript for production. Its command line
follows esbuild 0.28.2, so most esbuild commands work after swapping the binary name. Like
esbuild, it strips types without checking them. Run [ts-r-check](ts-r-check.md) or tsc for that.

## What works today

- Bundling ES modules and CommonJS, with tree shaking that honours `sideEffects` and
  `/*#__PURE__*/` annotations.
- Output formats `esm`, `cjs` and `iife`, and code splitting with `--format=esm --splitting`.
- Platforms `browser`, `node` and `neutral`.
- tsconfig `paths` and `baseUrl`, from the nearest `tsconfig.json` or from `--tsconfig`.
- Minification of whitespace, identifiers and syntax, separately or all at once with `--minify`.
- Source maps: linked, external, inline or both.
- JSX with the classic or the automatic runtime.
- `--watch`, which rebuilds when an input changes.
- A persistent build cache in `~/.cache/ts-r`.

## Not yet

- No plugins and no JavaScript config file. Everything is set on the command line.
- CSS is bundled (`@import`, `url()` assets, CSS modules), but esbuild's CSS syntax lowering
  is limited to nesting.
- No serve mode with `--watch`, and no legal-comment extraction.
- No `--drop` or `--pure`.
- Syntax lowering goes through tsc 7's down-level passes, so ES2015 is the lowest output.
  With `--target=es5` (or `--supported:<feature>=false` for an ES2015 feature) `const`,
  classes, destructuring and similar syntax are errors, as in esbuild; arrows and template
  literals are kept as written.
- IIFE output isn't re-indented, and runtime helpers are repeated per chunk.
- Not implemented: `import defer` and `import source`, glob `require` and `import()` of
  template strings, Yarn PnP module resolution (a tsconfig `extends` through PnP works).

The full list is in the design record, [0008 bundler](../../../docs/design/0008-bundler.md).

## Command line

```text
Usage: ts-r-build [options] [entry points]

Simple options:
  --bundle                  Bundle all dependencies into the output files
  --define:K=V              Substitute K with V while parsing
  --external:M              Exclude module M from the bundle (can use * wildcards)
  --inject:F                Import the file F into all input files
  --format=...              Output format (iife | cjs | esm)
  --loader:X=L              Use loader L for .X files (js, jsx, ts, tsx, json, text,
                            file, css, empty, base64, dataurl, binary)
  --minify                  Minify the output (sets all --minify-* flags)
  --outdir=...              The output directory (for multiple entry points)
  --outfile=...             The output file (for one entry point)
  --packages=external       Exclude every package import from the bundle
  --platform=...            Platform target (browser | node | neutral)
  --sourcemap               Emit a source map
  --splitting               Enable code splitting (requires --format=esm)
  --target=...              Environment target (es5..es2025, esnext, node18, chrome110,
                            ...); newer syntax is lowered
  --watch                   Rebuild when a source file changes

Advanced options:
  --banner:js=...           Text at the start of every JS output
  --chunk-names=...         Path template for shared chunks (default "[name]-[hash]")
  --conditions=...          Custom package.json exports conditions
  --entry-names=...         Path template for entry points (default "[dir]/[name]")
  --footer:js=...           Text at the end of every JS output
  --global-name=...         Name of the global for --format=iife exports
  --ignore-annotations      Ignore /*#__PURE__*/ and sideEffects
  --jsx=...                 transform | automatic | preserve
  --jsx-dev                 Use React's automatic runtime in development mode
  --jsx-factory=...         What to use for JSX instead of React.createElement
  --jsx-fragment=...        What to use for JSX instead of React.Fragment
  --jsx-import-source=...   Override the package name for the automatic runtime
  --log-level=...           verbose | debug | info | warning | error | silent
  --main-fields=...         Override the main file order in package.json
  --metafile=...            Write metadata about the build to a JSON file
  --minify-whitespace       Remove whitespace
  --minify-identifiers      Shorten identifiers
  --keep-names              Keep function and class names when identifiers are renamed
  --mangle-props=...        Rename all properties matching a regular expression
  --mangle-quoted=...       Mangle quoted properties too (true | false)
  --reserve-props=...       Do not mangle these properties
  --minify-syntax           Use equivalent but shorter syntax
  --no-cache                Do not use the persistent build cache
  --out-extension:.js=.mjs  Use a custom output extension
  --public-path=...         Prefix for file-loader URLs and chunk imports
  --resolve-extensions=...  Comma-separated list of implicit extensions
  --sourcemap=...           linked | external | inline | both
  --sources-content=false   Omit sourcesContent in source maps
  --supported:F=false       Treat syntax feature F (esbuild's names) as unsupported
  --tree-shaking=...        Force tree shaking on or off (true | false)
  --tsconfig=...            Use this tsconfig.json instead of the nearest one
  --threads=N               Worker threads (default: TS_R_THREADS or all cores, max 8)
  --version                 Print the version
```

The flags mean what they mean in esbuild. Where ts-r differs:

- Unknown flags are errors. A few esbuild flags that only change cosmetics, such as
  `--charset`, are accepted and ignored.
- Without `--outfile` or `--outdir`, the bundle goes to stdout.
- `--target` and `--supported` lower through tsc 7's down-level passes, one ECMAScript year
  at a time: an unsupported feature lowers everything newer than the year before it arrived,
  so the output may lower more than esbuild's.
- `--threads` defaults to the `TS_R_THREADS` environment variable, else the number of cores,
  capped at 8.
- `--no-cache` skips the persistent cache (the `build` namespace in `~/.cache/ts-r`).

Exit codes: 1 on any error, 0 otherwise. You can also run it through the launcher as
`ts-r build`.

## Next to esbuild and Vite

`ts-r-build` works on its own. Use it where you call the esbuild command line today, or to
build a library while the app keeps Vite. It has no JavaScript API, so scripts that call
`esbuild.build({...})` from Node need to switch to the command line first. It doesn't read
`vite.config`.

[ts-r-dev](ts-r-dev.md) uses the same bundler to pre-bundle an app's dependencies.

## Measured

On four dedicated Linux cores (the review box's bench of pull request #42 on 2026-10-24,
`cargo xtask bench --repos three-x10`, median of 10, peak memory not measured), three.js x10
minified with source maps took 962.9 ms without the cache and 484.6 ms with it, against
esbuild 0.28.2's 1,409.8 ms.

Older runs on an Apple M-series Mac (a shared laptop), 4 threads, against esbuild 0.28.2, on
public projects pinned in `bench/repos.toml` (`cargo xtask bench --repos excalidraw,three-x10`):

| Input | ts-r-build | esbuild |
|---|---|---|
| excalidraw app, minified ESM, no cache (median of 10) | 255 ms | 201 ms |
| Same, with the persistent cache | 132 ms | 201 ms |
| excalidraw: first build plus 20 edit-and-rebuild cycles, one process | 1,358 ms | 3,457 ms |
| three.js x10, minified IIFE with source maps, no cache (median of 10) | 255.0 ms | 246.2 ms |
| Same, with the persistent cache | 132.2 ms | 246.2 ms |

On that laptop, without its cache, ts-r-build was slower than esbuild on excalidraw and about
as fast on three.js x10: the rows above are the last of three invocations at commit 2fb2555; the
other two gave 231.8 and 233.9 ms against esbuild's 262.7 and 238.5 ms. It also doesn't bundle
template-literal `import()`s yet, so on excalidraw it leaves out the 61 locale chunks esbuild
emits (7.2 MB of output against 9.3 MB), which flatters its times. The three.js x10 bundles
agree: both load in Node with the same exports; ts-r's is 2.2% larger.

Correctness:

- esbuild's end-to-end bundler tests, run against ts-r-build: 1,046 of 1,066 compared cases
  pass (98.1%); 14 of the 20 failures are intended differences where ts-r-build follows
  tsc 7.
- Ecosystem bundle stage: all 915 compared packages pass (100%). Minified output is 2.9%
  smaller than esbuild's in total over 914 packages, with a median per-package ratio of 0.993.

Method and commits: [build on Linux](/benchmarks/#build-linux),
[build on the laptop](/benchmarks/#build),
[ecosystem](/benchmarks/#ecosystem) and
[correctness](/benchmarks/#correctness).
