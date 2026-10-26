---
title: Compatibility and status
description: What ts-r matches today against tsc 7, esbuild and ESLint, and how the top 1,000 npm packages fare. Generated from ecosystem/STATUS.md and the crates' notes.
nav: Compatibility and status
section: Reference
order: 2
generate: status
badges: ["warn:Alpha"]
---

ts-r measures correctness against the tools it replaces: tsc 7.0.2 for parsing, resolution,
emit and type errors, esbuild 0.28.2 for bundling, and ESLint with typescript-eslint for lint.
This page is generated when the site is built. The ecosystem section comes from
[ecosystem/STATUS.md](../../ecosystem/STATUS.md), and the conformance table from numbers
recorded in the crates' `AGENTS.md` files and design records (the build fails if a quoted
number disappears from its source, so the table can't silently go stale).

## Summary

| Tool | Works today | Main gaps |
| --- | --- | --- |
| [`ts-r-run`](tools/ts-r-run.md) | Runs TypeScript on Node 22.15+; passes 100 of 100 compared tests of tsx's own suite and 36 of 36 of Node's type-stripping tests | Only the entry's tsconfig is used; Windows untested |
| [`ts-r-dev`](tools/ts-r-dev.md) | Hot reload, Vite's `import.meta.hot` API, React Fast Refresh, dependency pre-bundling | No CSS preprocessors; some `vite.config` options and plugin APIs; HTTP/1.1 only |
| [`ts-r-build`](tools/ts-r-build.md) | esbuild's CLI flags, ESM/CJS/IIFE, splitting, minify, source maps, `--watch` | No plugins, no CSS bundling, lowering down to ES2015 (not ES5) |
| [`ts-r-check`](tools/ts-r-check.md) | tsc's codes and messages; 97.5% of conformance tests exact; 3 of 6 public bench projects match exactly | No "Did you mean" suggestions on regular expression errors (TS1369); some inference and narrowing; emits like tsc unless `--noEmit` |
| [`ts-r-lint`](tools/ts-r-lint.md) | 680 rules (51 type-aware) matching 75,282 of 75,305 of ESLint's own tests; ESLint flat config, oxlint and Biome configs | Type-aware rules only where ts-r-check matches your tsc; plugins other than typescript-eslint unsupported (48 eslint-plugin-jsdoc, 352 eslint-plugin-unicorn and 21 eslint-plugin-import rules aside) |
| [`ts-r-lsp`](tools/ts-r-lsp.md) | Shared server per workspace; lint; our checker or your TypeScript as the type backend | Slower than TypeScript 7's LSP to first diagnostics; most projects fall back to your TypeScript for diagnostics |

## Conformance

<!-- generated:conformance -->

## Ecosystem: the top 1,000 npm packages

`cargo xtask ecosystem` downloads the most-downloaded npm packages and runs each stage that
applies: parse every file, transform TypeScript sources, bundle the package and run it, check
its `.d.ts` files against tsc 7, and run its own test suite with tsc, tsx, ts-node and esbuild
swapped for ts-r. See [ecosystem/README.md](../../ecosystem/README.md) for how each stage
compares.

<!-- generated:ecosystem -->

## What "works today" means

A feature is listed as working when it runs from source on a branch that passed its gates
(`cargo test`, `cargo clippy -D warnings`, `cargo xtask agents-check`) and its numbers are
recorded in the repo. Alpha releases are on npm under the `next` dist-tag; see
[Getting started](getting-started.md).
