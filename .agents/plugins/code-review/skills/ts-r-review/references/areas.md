# What matters, area by area

Tracks land as branches named `<milestone>/<track>` and merge into `main` through PRs. Use the
paths a PR touches to pick the sections below. Each crate's `AGENTS.md` is the authority;
this file says where reviewers usually find real bugs.

## Front end: span, lexer, parser, AST (M1)

`crates/ts_r_span`, `ts_r_lexer`, `ts_r_parser`, `ts_r_ast` · design `0001-ast.md`

- Every loop consumes a token or bails; error recovery cannot spin on the same token.
- Recursion depth limit with a diagnostic (deeply nested parens, JSX, types, templates).
- Re-scan points (`/` vs regex, `>` vs `>>`, template continuations, JSX text) restore lexer
  state exactly.
- AST: nodes are `u32` indices into one arena per file; new node kinds follow 0001 and are
  handled by every exhaustive consumer (transform, codegen, binder, checker, lint visitors).
- Syntax diagnostics use tsc's codes and message text (from `ts_r_diagnostics`), at tsc's span.

## Transform, codegen, run (M1)

`ts_r_transform`, `ts_r_codegen`, `ts_r_run` · design `0002-transform-and-codegen.md`

- Emit matches tsc 7.0.2: type erasure leaves no stray tokens (ASI hazards after erased
  annotations, `satisfies`, `as`, non-null `!`), enum and namespace IIFEs, decorators, import
  elision of type-only imports, module formats.
- Source maps: V3, mappings stay monotonic; byte offsets converted to UTF-16 columns only at
  the map edge.
- Printer precedence and parenthesisation (arrow bodies returning object literals, `new`
  with call expressions, `in` inside `for` initialisers, `?.` chains).
- `ts-r-run`: runs on the user's own Node; flags and exit codes passed through.

## Dev server (M2)

`ts_r_hmr`, `ts_r_dev` · Vite-compatible `import.meta.hot`

- HMR boundary propagation through the module graph; invalidation on delete/rename; cycles.
- HTTP/WebSocket handling never panics on malformed requests; path traversal outside the
  project root is impossible.
- Watcher (`notify`) events debounced and deduplicated; no unbounded queues.

## Bundler and build (M3) · minifier (M7)

`ts_r_bundler`, `ts_r_build`, `ts_r_minify` · design `0008-bundler.md`

- Parity oracle is esbuild 0.28.2: ESM/CJS interop (`__esModule`, default import of CJS),
  tree shaking keeps side effects (`sideEffects`, getters, top-level calls), scope hoisting
  renames without capturing, code splitting chunk order deterministic, metafile stable.
- Minifier: constant folding and dead-branch removal must keep semantics (`typeof` of
  undeclared names, `NaN`/`-0`, getters, `arguments`, `eval`/`with` scopes disable renaming,
  TDZ for `let`/`const`, `Function.prototype.length`, property name mangling only where
  provably private).
- Incremental rebuilds invalidate on every input that affected output (cache key coverage).

## Linter (M4)

`ts_r_lint`, `ts_r_lint_cli` · design `0006-lint.md` · skill `add-lint-rule`

- Rules match ESLint / typescript-eslint as observed: options and defaults, message text,
  report location, fixes and suggestions (fixes never overlap, never change semantics, and
  re-lint clean). The rule oracle numbers in commit messages must not go down.
- Type-aware rules read types only through the checker's public API.
- Config loading: flat config, oxlint, Biome; `eslint-disable` directive semantics
  (unused-directive reporting, rule lists, line vs next-line).

## Binder, types, checker, history (M5)

`ts_r_binder`, `ts_r_types`, `ts_r_checker`, `ts_r_decls`, `ts_r_history`, `ts_r_check`
· designs `0003-binder.md`, `0004-types-and-checker.md`, `0005-history.md`
· skills `fix-conformance-diff`, `minimize-repro`

- Parity target is tsc 7.0.2: same diagnostic **code**, message text, span and related
  information. A fix for one conformance group must not change others; the PR should say what
  the oracle numbers did.
- Fixes come from observed behaviour and design records, never tsc source (see the
  from-scratch rule). Upstream identifiers in comments are a smell.
- Shared `Checker` state is used from many `Ck` workers: caches keyed by everything they
  depend on, no worker-local state leaking into shared caches, results sorted and deduplicated
  before output.
- Type store: interned, `TypeId` identity semantics preserved; relation caches cannot cache a
  result that depended on a still-resolving type (circularity).
- Recursion in instantiation/relations reports TS2589 / depth diagnostics instead of
  overflowing.
- History/incremental: anything that can change a file's diagnostics is part of its hash
  (signature hashes in `ts_r_decls`, lib and config changes); a stale result is a correctness
  bug.

## Sound mode and guards (M6)

`ts_r_sound`, `ts_r_guard` · design `0007-sound-mode.md` (status may be Proposed)

- TSR rules, `@ts-r-unsafe` escapes, `@ts-r` level pragmas, `[sound]` config; read types only
  through `ts_r_checker`'s public query API.
- Code that depends on a design record still marked Proposed must not merge before it is
  Accepted.
- `parse<T>` validators: the compiled predicate and the error descriptor agree for every type
  form they claim to support.

## Editor: LSP and VS Code (L1-L4)

`ts_r_lsp`, `editors/vscode`

- LSP positions are UTF-16 (or negotiated encoding) at the protocol edge only; internal data
  stays UTF-8 byte offsets.
- Daemon is memory-budgeted and shared per workspace; requests cancelled on supersede; no
  unbounded document or diagnostic caches.
- Quick fixes and organise imports produce edits that apply cleanly (non-overlapping, against
  the right document version).
- VS Code extension is plain JS with `// @ts-check`, no bundler and no runtime dependencies.

## Ecosystem, release, upstream (M7, M0)

`ecosystem/`, `xtask`, `upstream/UPSTREAM.json`, `npm/`, `bench/` · skills `upstream-sync`,
`bench-compare`

- Harness runs real packages: wrapped in `scripts/limit.sh`, at most 4 threads, temp output
  cleaned up.
- Pins in `upstream/UPSTREAM.json` and `bench/package.json` move only in bump PRs that also
  sync tests and regenerate code.
- Release: signed binaries, npm publish with provenance; no secrets in logs.

## Site and landing

`landing/` (Next.js App Router, `output: 'export'`): the whole site, with the docs sources in
`landing/content/`

- Static export only: no server-only Next.js features (route handlers with dynamic behaviour,
  `headers()`, ISR, middleware); `basePath`/trailing slashes match GitHub Pages.
- Coming-soon vs launched mode driven by `landing/data/site.json` and `SITE_MODE`; nothing
  launched-only leaks into coming-soon builds.
- Launched-only routes are `page.launched.tsx` / `route.launched.ts` (`pageExtensions` by mode).
- `npm run build` in `landing/` runs `scripts/check.mjs` (content, quoted numbers, links, anchors).

## CI (.github/workflows)

- Runners are GitHub-hosted (`ubuntu-24.04`, `windows-2025`, `macos-*`; 2 vCPU where offered); permissions minimal per job; secrets never echoed;
  `pull_request_target` / `issue_comment` jobs never run PR code with secrets.
- Third-party actions pinned to a major version or SHA; deploy only from `main`.
