# 0002: Transform, code generation and ts-r-run

Status: Proposed (drafted for M1; awaiting review)
Owners: `ts_r_transform`, `ts_r_codegen`, `ts_r_run`; consumers: `ts_r_hmr` / `ts-r-dev`
(through `ts_r_transform::hmr`), later `ts-r-build`

## Context

M1 ships `ts-r-run`, which replaces tsx and ts-node: it must run any TypeScript a project
contains, so "erase the types" is not enough. Enums, namespaces, parameter properties,
`useDefineForClassFields`, legacy decorators with metadata, `import x = require()`, JSX and
CommonJS output all change runtime code. The same transform feeds the dev server (M2) and the
bundler (M3).

The specification is **tsc 7.0.2's emit** (`npx -y -p typescript@7.0.2 tsc`), observed, never
read: outputs are compared by AST shape after parsing both as JavaScript (formatting and
comments don't count). Three oracles:

- per-feature fixtures (`crates/ts_r_transform/tests/fixtures/**`, with tsc's output stored
  next to each as `<name>.tsc.js`),
- tsc 7's own emit baselines (`tsc/testdata/baselines/reference/**.js` in the upstream
  checkout; every test whose options we support),
- every `.ts`/`.tsx` file of a real monorepo (private-app `apps/` and `packages/`, compiled
  with `tsc -p` per tsconfig).

Exception: static `super` calls in class-decorated classes preserve JavaScript receivers
and optional short-circuiting, including parenthesized callees. Observed tsc 7.0.2 output
drops the optional check on `super.m?.()` and the receiver on `(super.m)()`; these forms
instead use `Reflect.get(..)?.call(_classThis)` and `Reflect.get(..).call(_classThis)`.
`tests/runtime/es_decorated_super_calls.ts` in `ts_r_transform` checks runtime semantics.

Everything sits on the index-based AST of [0001-ast.md](0001-ast.md). Output targets ES2022 and
later only.

## Decision

### Pipeline

`parse → transform (in place) → print`. `ts_r_transform::transpile(path, src, &opts,
&codegen)` is the one-call API; `transform(&mut Ast, src, &opts) -> TransformResult` exposes
the middle step (`erased` spans, the helper `prelude` text, diagnostics).

### Transform (`ts_r_transform`)

- **In place.** Nodes are rewritten in the parsed `Ast`. New nodes are appended; a replaced
  node's slot receives a copy of the new node (`builder::replace`), so parents never need
  updating. Children are visited before their parent is replaced.
- **Synthesized nodes have empty spans** (`start == end`). The start is a source-map hint;
  `0..0` means unmapped. Codegen prints their literals from cooked values.
- **One scope pass first** (`scope.rs`): bindings, reference → binding resolution for the
  original nodes, type-only names, namespace and enum member scopes. Import elision, enum
  member references, namespace export rewriting and CommonJS reference rewriting read it.
- **Erased code** (types, `declare`, overloads, type-only imports) is recorded as spans in
  `TransformResult::erased`; codegen drops comments inside them.
- **Helpers** (`__decorate`, `__importStar`, ...) are tsc's exact helper texts, printed after
  the directive prologue; `importHelpers` (ESM) imports them from `tslib`; `noEmitHelpers`
  omits them.
- **Hoisted temporaries** (`_a`, `_b`) are declared once per function body as `var _a, _b;`
  after the directives; namespace bodies hoist their own.
- **Per-file module format** (`ModuleFormat::{Esm, CommonJs, Preserve}`): node16/nodenext pick
  it from the extension and the nearest `package.json` `type`, like tsc.
- **Targets below ES2022 are not lowered.** `downlevel.rs` is the slot for that pass; ES2022
  is `downlevel::MIN_TARGET`.

Covered: type erasure, parameter properties, class fields under both
`useDefineForClassFields` settings (including tsc's "move every instance initializer once one
moves" rule and accessor lowering), `accessor` fields, enums (constant folding, reverse
mappings, string members, merging) and const enums (inlined; `preserveConstEnums`),
namespaces (IIFEs, qualified names, merging, exported variable rewriting), `import x =
require()` / `export =` / `export import`, legacy decorators with `emitDecoratorMetadata`,
TC39 decorators (lowered to tsc's `__esDecorate`/`__runInitializers` IIFE form, with
`Symbol.metadata`, `accessor` fields, private members and computed keys; native only for
ESNext with `useDefineForClassFields`),
JSX (classic, automatic, automatic-dev, pragmas, entity decoding, text folding), import
elision with `verbatimModuleSyntax`, `rewriteRelativeImportExtensions`, CommonJS output
(interop helpers, live bindings, `(0, f)()` calls, dynamic `import()`), `createRequire` for
`import =` in Node ES modules.

### Code generation (`ts_r_codegen`)

- Prints any tree, TypeScript included. It never edits source text; parsed literals print
  their raw source form so they round-trip byte for byte.
- **Parentheses come from precedence and position**, never from the source: binary/unary
  precedence and associativity, `??` mixed with `||`/`&&`, `**` with a unary left operand,
  `new` callees, optional chains, and statement-start ambiguities (`{`, `function`, `class`,
  `let [`, `async function`). Arrow functions with one type parameter print `<T,>` in TSX.
- **ASI-safe**: every statement ends in `;` or `}`; `return`/`throw`/postfix operators never
  break the line before their operand.
- **Comments** live in the AST's side table. Readable layout keeps statement-level comments;
  compact keeps legal (`/*!`, `@license`, `@preserve`) and annotation (`#__PURE__`,
  `#__NO_SIDE_EFFECTS__`) comments. `comments: false` keeps legal comments only.
- **Layouts**: `Readable` (4-space indent, tsc-like) and `Compact` (no optional whitespace).
- **Source maps V3**, one segment per token start, one source; `to_json`, `to_data_url`,
  `decode`. `collect_specifiers` reports the output span of every module specifier (used by
  the dev server to rewrite imports).

### ts-r-run (`ts_r_run`)

Two strategies were built and measured (Node 24.21.0, Apple M-series, 4 compile threads,
median of 15 runs; `cargo run --release -p ts_r_run --example bench`):

| | hello.ts | 1,000 modules |
|---|---|---|
| `node -e 0` (floor) | 23 ms | 24 ms |
| `node --experimental-strip-types` | 50 ms | 215 ms |
| tsx 4.23 (direct binary; `npx tsx` adds ~400 ms) | 79 ms | 270 ms |
| **ts-r-run, prebuild, warm (default)** | **29 ms** | **101 ms** |
| ts-r-run, prebuild, cache warm but entry touched | 30 ms | 116 ms |
| ts-r-run, prebuild, empty cache | 32 ms | 137 ms |
| ts-r-run, hook strategy (cache warm) | 48 ms | 390 ms |

**Decision: "prebuild" is the default.**

1. Resolve the project: the nearest `tsconfig.json` above the entry (or `--tsconfig`), loaded
   without globbing `include`.
2. **Fast path**: a stamp file keyed by (tool build, entry, tsconfig flag, source-map flag)
   lists every input (sources, tsconfigs, the `package.json`s that decided module formats)
   with size and mtime. If all match, start Node immediately with the recorded manifest.
3. Otherwise walk the module graph breadth-first from the entry, compiling each level in
   parallel (`TS_R_RUN_THREADS`, default min(cores, 8)). Each file goes through `ts_r_cache`
   (namespace `run`, key = tool build + options + file name + package type + source-map flag +
   source bytes). Specifiers come from the *transformed* tree, so elided type-only imports are
   never followed.
4. Write one manifest file (`<cache root>/run-graphs/<blake3>.graph`): a JSON index line
   `{"f":{path:[format,start,end]},"r":{parent:{specifier:target}}}` and then all code.
5. Start `node [node flags] --import <hooks.mjs> <entry> [args]` (exec on Unix). The preload
   uses synchronous `module.registerHooks` (covers both `import` and `require`): `resolve`
   answers from `r`, then Node's resolver, then a `.js → .ts` / extensionless fallback; `load`
   slices the manifest. Files the walk missed (computed specifiers) are compiled on demand by
   `ts-r-run --compile`.

The hook strategy (`--strategy hook`) starts Node at once with off-thread `module.register`
hooks that ask a long-lived `ts-r-run --serve` child for every resolve and load. It loses
because each module costs a cross-thread hop plus an IPC round trip, and `require()` inside
CommonJS bypasses async hooks.

Runtime rules:

- **Module format** follows Node: `.mts`/`.cts`, then `package.json` `type`, then syntax
  detection for typeless packages (value `import`/`export`, `import.meta` or top-level
  `await` → ESM). Like tsx, a file with ES syntax that also uses `require`/`module`/
  `exports`/`__dirname`/`__filename` and nothing ESM-only runs as CommonJS.
- **Resolution** uses `ts_r_resolver` in `bundler` mode for relative specifiers and tsconfig
  `paths` only; bare package specifiers are left to Node (runtime conditions). Declaration
  files are never targets.
- **Options** come from the tsconfig, with target raised to ES2022, JSX forced to a runtime
  (`react-jsx` unless `jsx: react`), `importHelpers` and `rewriteRelativeImportExtensions` off.
- **Source maps** are inline data URLs whose `sources` is the file name only (resolved
  against the module URL), enabled with `module.setSourceMapsSupport(true)`. They cost about
  20 ms per 1,000 modules (81 ms with `--no-source-maps`).
- **`--watch`** watches the directory of every input (notify), restarts on a change to an
  input (30 ms settle, SIGTERM then SIGKILL after 3 s), keeps waiting after syntax errors or
  exits, and the child exits by itself if ts-r-run disappears.

### Dev server adapter

`ts_r_transform::hmr::TsTransform` (feature `hmr`) implements `ts_r_hmr::Transform` for
`.ts`/`.tsx`/`.mts`/`.cts`/`.jsx`: always ESM, source map with `sourcesContent` in dev,
imports from codegen's specifier spans, the first syntax error as a `TransformError` with a
1-based position. `ts-r-dev` registers it in front of the JS passthrough.

## Invariants agents must keep

- Synthesized nodes have empty spans; codegen never reads source text for them.
- A transformed tree printed by codegen reparses to the same tree (round-trip tests).
- `transform` is deterministic for (source, options); `TransformOptions::cache_key` covers
  every option that changes output.
- ts-r-run cache payloads contain no absolute paths (source map `sources` is the base name;
  `react-jsxdev` is downgraded to `react-jsx`).
- Every cache key includes the tool build fingerprint (executable size + mtime) and options.
- Output for targets below ES2022 is not lowered (`downlevel::is_supported`).
- Oracle results do not regress: the fixture test's `KNOWN_GAPS` list only shrinks, and the
  private-app project oracle stays at 0 mismatches.

## Alternatives

- **Blank-space type stripping** (Node's built-in approach): keeps positions and needs no
  source map, but cannot handle enums, namespaces, parameter properties, decorators or
  CommonJS output, so it can't replace tsx. Rejected as the only path; it could become a fast
  path for files that use erasable syntax only.
- **Use `ts_r_binder` for scopes** ([0003-binder.md](0003-binder.md)): it arrived after the
  transform. The transform's own `scope.rs` is per-file and lighter; switching is open.
- **ts-r-run hook strategy**: measured above, 3.9x slower on 1,000 modules.
- **Coarser source maps** (one segment per line): saved 6 of the 19 ms; not worth the less
  precise stack traces.
- **Rewriting specifiers in the output instead of a resolve hook**: needs `require()` spans
  (codegen only reports `import`/`export`/`import()`), and a hook lookup is cheap.

## Status

Superseded in part by [0012](0012-tsc-emit.md) (2026-10-19): `ts-r-check` emit lowers down to ES2015 (phase E5) and is gated on byte equality with tsc, not shape alone.

Proposed 2026-10-15 with the M1 transform track (branch `m1/transform-run`).
