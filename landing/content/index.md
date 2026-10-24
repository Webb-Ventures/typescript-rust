---
title: ts-r docs
description: Six separate Rust tools for TypeScript, each usable on its own next to tsc, ESLint and Vite. What works today, how to install it, and every error code.
nav: Overview
section: Start
order: 0
badges: ["warn:Alpha: npm install @ts-r.dev/cli@next"]
---

ts-r is a family of command-line tools for TypeScript, written from scratch in Rust. Each tool is
a separate binary that replaces one tool you use today and reads the config you already have.
You can adopt one without the others: none of them needs another ts-r tool or a background
service.

ts-r is in alpha, on the npm dist-tag `next`. To try a tool, [install it from npm](getting-started.md).

## The tools

| Tool | Replaces | Status today |
| --- | --- | --- |
| [`ts-r-dev`](tools/ts-r-dev.md) | `vite dev` | Dev server with hot reload and React Fast Refresh; loads `vite.config` and runs Vite plugins |
| [`ts-r-build`](tools/ts-r-build.md) | esbuild, Vite build | esbuild-compatible bundler CLI; no plugins or CSS bundling yet |
| [`ts-r-check`](tools/ts-r-check.md) | `tsc --noEmit` | Type checker in progress: 96.6% of tsc 7's conformance tests match exactly |
| [`ts-r-lint`](tools/ts-r-lint.md) | ESLint + typescript-eslint, oxlint | 154 rules, 51 type-aware, matching 15,495 of 15,517 of ESLint's tests |
| [`ts-r-run`](tools/ts-r-run.md) | tsx, ts-node | Runs a TypeScript file on your Node, with `--watch` and source maps |
| [`ts-r-lsp`](tools/ts-r-lsp.md) | tsserver, the ESLint server | One shared language server per workspace, with a VS Code extension |

The `ts-r` launcher runs them as `ts-r dev`, `ts-r build` and so on, and `ts-r ci` runs check,
lint and build in parallel. It contains no compiler code
([crates/ts_r_cli](../../crates/ts_r_cli/AGENTS.md)).

## Where to go next

- [Getting started](getting-started.md): build the tools and run each one.
- [Compatibility and status](status.md): what passes against tsc, esbuild and ESLint today,
  and how the top 1,000 npm packages fare.
- [Error codes](/errors/): one page per diagnostic, starting with the most
  common ones, each with an example and a fix.
- [Sound mode](sound-mode.md): the opt-in mode that makes types hold at runtime (pre-alpha).
- [Benchmarks](/benchmarks/): every measured number, with how it was
  measured, including the ones that miss their targets.

## How these docs are made

The site is a statically exported Next.js app that renders Markdown from
[landing/content](.). Facts come from the crates' `AGENTS.md` files, the design
records and measured runs; error-code examples are run through `ts-r-check` and shown only when
it reports the code. Each page has a Markdown version at the same address with `index.md`
appended, and [llms.txt](/llms.txt) lists them all.
