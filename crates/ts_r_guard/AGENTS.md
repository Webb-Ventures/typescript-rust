# ts_r_guard

`parse<T>` runtime validators (milestone M6): evaluates a type argument on declarations and
emits a compiled predicate plus an error descriptor per type, and the shared runtime
(`ParseError`, `$parse`, the descriptor walker). Root rules: [../../AGENTS.md](../../AGENTS.md).
Design: [../../docs/design/0007-sound-mode.md](../../docs/design/0007-sound-mode.md), section
"`parse<T>` runtime validators".

## API in one breath

```rust
let mut g = Generator::new(&program, GuardOptions { max_depth: 512, explain: Explain::Full });
let e = g.add(file_index, type_arg_node)?;   // Err(Unsupported { ty, reason }) -> TSR1501
g.code();                  // const $is_User = (v, d) => ..; const $d_User = () => [..]; $is_e0 / $d_e0 ...
g.parse_call(&e, "data");  // $parse(data, $is_e0, $d_e0, "User")
g.module();                // runtime parts + code + `export const e0 = { is, parse, safeParse, assert }`
```

`ts_r_sound` (S-14, TSR1501) calls `Generator::add` too, so the editor and a build can't
disagree about what is supported.

Call sites (`expand.rs`) and bundles (`vfs.rs`):

```rust
let ex = Expander::new(fs, ExpandOptions { guard, layout: Layout::Inline });   // or with_options(.., tsconfig options)
let x = ex.expand(path, src)?;   // None: no `@ts-r.dev/guard` value import. x.code, x.deps, x.errors (TSR1501/1402/1503/1504)
let gfs = GuardFs::new(real_fs, working_dir, guard, /* shared (bundle) */ true); // a FileSystem for the bundler
let gfs = gfs.with_tsconfig(path);   // optional: one tsconfig for every file (`--tsconfig`)
gfs.errors_in(&build.inputs); gfs.touched();   // errors of each file's last read, kept across rebuilds
gfs.forget_configs();   // a tsconfig changed (watch): options are reloaded, every file re-parsed
gfs.forget_files(&changed);   // watch: drop the parsed copies of changed/deleted files
ex.parsed_files(); gfs.parsed_files();   // files parsed so far (tests, reports)
```

- **Source to source, before the TypeScript transform**, so `ts-r-run` (`Engine::compile_file`),
  `ts-r-build` (`GuardFs`) and `ts-r-dev` (`ts_r_dev::guard::GuardTransform`) print the same
  validators. Value imports from `@ts-r.dev/guard` are removed (newlines kept); a typed call or
  instantiation expression (`parse<T>`, `safeParse`, `is`, `assert`, `parseJson`,
  `fetchJson`, `guard`, `env`, `body`, `query`, `args`, `stored`, `readJson`) has its callee and type arguments
  replaced by `$f$<form>$<entry>`, a top-level arrow over the type's validator (every form
  but `is` goes through the entry's `$f$parse$<entry>`); `expect`, `panic`, `ParseError`,
  `ExpectError`, `PanicError` and namespace imports become constants bound to the runtime's
  (the error classes are getters: `const ParseError = $ParseError();`). The generated code goes on **one line** after the shebang and the
  directive prologue, so line numbers (and source maps) of user code don't move.
- `Layout::Inline` (run, dev, unbundled `ts-r-build`): the runtime items the generated code
  reaches (`rt.rs` splits `runtime.js` into top-level items and follows `$name` references,
  the same tree shaking a bundler does), declaration functions and entries in the file. The
  error classes live in the global symbol registry (`Symbol.for("ts-r.ParseError")`), so
  inlined copies agree on `instanceof`.
- `Layout::Shared` (bundles): imports from virtual modules under `<project root>/.ts-r-guard/`:
  `rt.js` (the whole runtime, exporting every name; importers import only what they use) and `<decl file>/<Name>.js` per
  declaration (`<decl file>/~mod~<module>/<Name>.js` inside `declare module`, `~abs/...`
  outside the root), exporting `$is`/`$d` and importing what it references. A module's text
  depends only on the declarations (`Generator::decl_module`), it is regenerated on every
  read (the bundler's content-hashed caches see edits), and directory listings are complete
  without knowing who asks (a declaration file lists all its type aliases and interfaces),
  so the bundler's listing cache can't miss one. Tree shaking drops unused validators and
  runtime parts (`/* @__PURE__ */` error classes).
- Names: declarations `$is_<Name>`/`$d_<Name>`, entries that are not a plain reference
  `$is$e0`/`$d$e0` (an entry equal to an earlier one shares its functions), forms
  `$f$parse$e0`, template patterns `$re1`. Inline output aliases a declaration whose body
  equals an earlier one's (`const $is_B = $is_A, $d_B = $d_A;`).
- **Forms the record lists without semantics** (decided here; the record's table only gives
  `T` and `ParseError`):
  - `body<T>(req)` validates `req.body`, the parsed body Express, Fastify and Koa
    (`ctx.request`) set. No reading or JSON parsing: it returns `T`, not a promise, so a fetch
    `Request` needs `parse<T>(await req.json())`.
  - `query<T>(req)` reads `req.query` when it is an object (Express, Fastify, Koa), else the
    search params of `req.url` (fetch `Request`, `node:http`, Hono's `c.req`); a URL, URL
    string or `URLSearchParams` also works. A repeated key gives an array.
  - `args<T>(argv?)` reads `process.argv.slice(2)` (or `argv`): `--name value`,
    `--name=value`, `-n value`, `--flag` (true when the property is `boolean` or no value
    follows), `--no-flag` (false). Names match a property as written or in camelCase. Other
    words, and every word after `--`, go to `_`. Repeated names collect into arrays.
  - **Coercion policy:** only the string sources (`env`, `query`, `args`) convert, and only
    as the declared property type says (`$conv`): decimal numbers, `true/false/1/0` booleans,
    literal members, and array-typed keys collect their values. Undeclared keys are dropped
    (unless `T` has an index signature). `parse`, `body` and every other form never convert;
    there is no option to change this, because a validator that rewrites data is no longer
    a check of it.
- Callee recognition is syntactic (the file's own `@ts-r.dev/guard` import, named or namespace);
  a local binding with the same name (parameter, variable, function, class, destructuring)
  suppresses the TSR1402/TSR1503 misuse errors for that name, but a shadowed name *called with
  a type argument* is still expanded.
- `deps` are every other non-lib file of the program the file was evaluated in (its import
  closure): `ts-r-run` adds them to the run's inputs (stamp) and the cache key is the expanded
  text, so editing a type re-generates its validators.
- **Parsed-file cache.** Each `Expander` (one per process in every tool: `ts_r_run::Engine`,
  `ts_r_dev::guard::GuardTransform`, the build's `GuardFs`, kept across watch rebuilds) loads
  programs through one `ts_r_checker::SourceCache`, so a file of overlapping import closures
  is parsed and bound once per contents, not once per expansion (a chain of n modules: n
  parses, was n(n+1)/2). Invariants:
  - an entry is reused only for the same path, bind options and `is_external`, and only when
    the text read now is byte-equal to the cached one, or the file system's stamp (length,
    mtime; `RealFs` only) equals the one seen at the last read and the mtime is more than
    2 s older than that read (coarse clocks: recently written files are always read and
    compared). The overlaid file (the text being expanded) is never stamped, only compared;
  - reading a changed text evicts every other version of that path (one contents version
    per path); past 65,536 paths the cache starts over; `forget_configs` clears it (bind
    options come from the tsconfig) and `forget_files` drops changed or deleted paths
    (`ts-r-build --watch` calls it with each change set);
  - cached files are immutable `Arc<SourceFile>`s shared by programs (as lib files are);
    output does not depend on whether a file came from the cache.
  The closure is still walked (resolved, stamped) once per expansion: on real files a chain
  of 1,000 modules takes 2.7 s (was 9.9 s); see the commit for the measurements.

## Invariants

- Types come from declarations only: type aliases, interfaces (merged, `extends`), enums with
  constant members, imports, `export ... from`, `export *`, namespace imports and ambient
  `declare module "x"` blocks. Never the checker's types (record P4). Names not declared in the
  program's sources are lib globals by name (`Date`, `Map`, `Record`, `Partial`, ...).
- Every unsupported type is an `Err` with a reason; a failed `add` leaves nothing behind.
- Bounded: at most 200 nested types and 200 000 evaluation steps per `add`, 32 import hops.
  Generated predicates stop at `max-depth` named-type levels (cyclic values are rejected).
- Output is deterministic: declaration functions are named `$is_<Name>` (`_2`, `_3` on clashes,
  also with an entry's `e0`, `e1`, ...) in first-use order; no hashes of paths.
- `parse` returns the value it was given (no copy). Extra properties are kept (open question 11);
  optional properties are exact (present `undefined` only when the type allows it). Messages
  never include values, only their kinds.
- **User classes** (open question 13, decided 2026-10-15) are checked with
  `v instanceof <class>`. The class must have a value the validator can import: abstract,
  generic, reached through `import type`/`export type`, `declare class` outside a `.d.ts`,
  global (script file) or unexported classes are `Err` (TSR1501, each with its own reason). The
  generated code imports it (`import { <export> as $c_<Name> } from "<spec>"`): a class in
  `declare module "x"` from `x`; else a specifier the program already uses for the class's file
  (a package name as is; the user's relative specifier re-based onto the output file for inline
  output, so the extension convention stays the user's; the file's absolute path for shared
  modules, the specifier's for a `.d.ts`). In the file that declares the class, inline output
  uses the class's own name. A class must be exported even when used only in its own file, so
  the editor and every layout agree.
- **Refinements** (open question 14): a type alias or interface whose name is in
  `GuardOptions::refinements` validates as `G::Refine(base, i)`: the base type, then the
  registered `(v: unknown) => v is T` predicate (`import { <export> as $r_<Type> }`; a path is
  relative to the output file inline, absolute in shared modules). The base of an alias of an
  intersection is its non-object members (`string` for `string & { __brand }`), the brand
  members dropped even when they can't be evaluated; an intersection of object types keeps
  them all. Without a refinement, a primitive intersected with object types is TSR1501 naming
  the alias and the config key (or, written in place, asking for an alias). The name matches
  any declaration with that name in the program.

## Generated code and runtime (`runtime.js`, `emit.rs`)

- Predicates are arrows (`const $is_User = (v, d) => d < 512 && $obj(v) && typeof v.id ===
  "number" && ..`): `&&` chains without parentheses, `||` chains parenthesised. Loops over
  elements take shared checks by name: `$isArr(v.tags, $str, d)`, and a declaration's
  predicate with the depth plus one (`$isArr(v.items, $is_Item, d + 1)`); anything else is
  an arrow. Discriminated unions are a conditional chain on the tag
  (`$obj(v) && (v.type === "a" ? .. : v.type === "b" && ..)`), each branch without the tag.
  A missing optional key is tested with `in` (`v.k === undefined && !("k" in v) || ..`), not
  `$has`: V8 inlines `in` (twice as fast on a missing key); they differ only for inherited
  keys. `safeParse` runs `is` first, and lowers `Error.stackTraceLimit` to 0 while it builds
  the error (a value, never thrown to the caller): capturing a stack is 95% of an invalid
  input's cost, so this made the all-issues path 7x faster.
- Descriptors (only built after a predicate fails): a primitive is its name (`"string"`,
  `"null"`, checked against `$kind`); anything else is `[kind, ...data]` where the kind is a
  runtime function that walks (`kind(d, v, depth)`) and names the type (`kind(d)`): `$O`
  (flat `key, desc` pairs, `0` before an optional key; index descriptor third), `$A`, `$T`,
  `$L` (literals), `$U` (union without object members), `$D` (discriminated:
  `key, tags, member, ..`), `$V` (other unions with object members, best-match reporting),
  `$R` (reference: shown name, descriptor function, arguments), `$C` (class: name, plus the
  imported class for a user class; a global by name otherwise), `$F` (refined: base
  descriptor, predicate, name; `expected Email` when the predicate fails), `$Map`, `$Set`, `$X`
  (template: the shared `$reN` and its shown form), `$Any`. A kind nobody uses is never
  shipped. The walk state (`$P` path, `$I` issues, `$K` limit) is module-level; `$probe`
  saves it around trial walks.
- **`[sound.guard]`** (`GuardOptions`): the tools read it once at startup through
  `ts_r_sound::config::guard_options` (`ts-r.toml`, `tsrconfig.json` or the tsconfig's
  `"ts-r"` key; more than one is TSR0020 and the tsconfig wins, then `tsrconfig.json`). This
  crate can't read it itself (`ts_r_sound` depends on it). `refinements` (above) are part of
  the options (`Clone`, not `Copy`). `explain = "none"` uses `$parse0` and emits no descriptors inline (bundles tree-shake
  them), unless `env`/`query`/`args` need one to convert; `"first"` passes `1` as the issue
  limit (default 20). `max-depth` is the predicates' `d < N` and the walker's `$MAX_DEPTH`.
  `max-validator-kb` (default 8, 0 = off): an entry whose generated predicate and
  descriptor, plus those of every declaration it reaches, exceed it is TSR1504 at its first
  type argument (measured on generated text, before minification). The options are part
  of every cache key that covers expanded code (ts-r-run salt, ts-r-dev cache id).
- `runtime.js` rule: every top-level item is one `const`/`let`/`function` starting at column
  0, comments before an item belong to it, and nothing at module level has side effects (so
  `rt.rs` and bundlers can drop any item). Error classes are getters (`$ParseError()`), not
  `/* @__PURE__ */` calls. Needs ES2022 (`Object.hasOwn`, `??=`).

## Size (fixture `tests/fixtures/app`, `ts-r-build --bundle --minify`, gzip -9)

| | before (M6) | minified | gzip |
|---|---|---|---|
| runtime alone (`size-runtime.ts`, one `parse<string>`; the runtime is line 1) | 4,177 B / 1,751 B | **725 B** (830 B whole bundle) | **510 B** (556 B whole bundle) |
| runtime with the walker parts the ten types use (`$O`, `$A`, `$L`, `$R`, `$U`, `$D`, `$X`, loops) | | 2,432 B | 1,264 B |
| runtime + ten typical API types (`size-all.ts`, `types.ts`) | 8,220 B / 2,954 B | 5,557 B | 2,346 B |
| per type (validators only, average of the ten) | 404 B / 120 B | 312 B | 108 B (in context) |

The record's runtime budget (1.0 KB minified / 0.5 KB gzip) is met for the core (`$parse`,
`ParseError`, message, primitive walk). Each descriptor kind a program uses adds 50 to 300 B
minified on top (the union kinds most). Per type the record's guess (twice the declaration,
400 B for a 10-property interface) is not met: a validator is about 3x its declaration
(User: 113 B declared, about 330 B generated), because the descriptor repeats every key and
type for the error path. The floor of this design is roughly predicate (about as long as the
declaration in `typeof` form) plus descriptor (about the declaration again) plus an entry
arrow; going lower means interpreting descriptors on the fast path (rejected by the record:
typia speed) or compiling an issue-collecting predicate (bigger per type).
`ts_r_build/tests/guard.rs` pins both minified sizes (5% gate). With `explain = "none"` the
ten-type bundle is 2,720 B / 1,216 B (no descriptors, no walker; errors read `Invalid User`).

## Speed against typia (`bench/typia/run.sh`)

typia 9.7.2 (TypeScript 5.8.3), installed into a temp dir; its validators come from its own
CLI (`typia generate`). Both sides are bundled by `ts-r-build`; Node 24, Apple M-series, a
shared machine (runs vary by about 10%). Throughput per type and operation over the ten API
types (`bench/typia/data.ts`: one valid and one invalid value each, 2-3 wrong fields),
median of 3 x 150 ms after warm-up, geometric mean, ts-r / typia:

| operation (ts-r vs typia) | minified | unminified |
|---|---|---|
| `is` valid (`is<T>` vs `createIs`) | 53 M/s vs 58 M/s, 0.91x | 67 vs 65, 1.03x |
| `is` invalid | 59 vs 59, 1.00x | 71 vs 68, 1.05x |
| `parse` valid (`parse<T>` vs `createAssert`) | 39 vs 46, 0.85x | 51 vs 48, 1.07x |
| `parse` invalid, thrown (all issues vs typia's first) | 180 k vs 187 k, 0.97x | 186 k vs 192 k, 0.97x |
| `safeParse` vs `createValidate`, valid | 37 vs 48, 0.78x | 48 vs 48, 1.02x |
| `safeParse` vs `createValidate`, invalid (all issues) | 0.9 M vs 8.7 M, 0.10x | 0.9 M vs 8.6 M, 0.11x |

- The fast path matches typia unminified. The minified column above predates a minifier fix:
  `ts_r_minify` used to rewrite module-level `const` to `let`, which V8 does not
  constant-fold (Order `is` valid at 0.65x of our unminified bundle). It now keeps top-level
  `const`; minified runs at the unminified speed (geomean 0.99-1.00x of unminified on every
  operation, Order 0.98x) for 36 B more on the ten-type bundle.
- The all-issues error path is 10x slower: `ParseError` is an `Error` (message, class) and
  the walk interprets descriptors, where typia's `validate` returns plain objects from
  compiled code. The record trades error-path speed for size here.

Bundle size, `ts-r-build --bundle --minify`, bytes minified / gzip:

| ten types | ts-r | typia |
|---|---|---|
| `parse<T>` vs `createAssert<T>()` | 5,519 / 2,342 | 25,456 / 3,302 |
| `is<T>` vs `createIs<T>()` | 2,146 / 886 | 4,566 / 875 |
| `is` + `parse` + `safeParse` vs `is` + `assert` + `validate` | 6,346 / 2,668 | 51,709 / 5,490 |

## Not done yet

- `env<E>()` reads `globalThis.process?.env` (no `import.meta.env` in browsers yet).
- Options are read at startup: `ts-r-dev` and `ts-r-build --watch` need a restart after
  editing them; a warm `ts-r-run` sees an edit (the config file is a stamp input) but not a
  newly created `ts-r.toml` or `tsrconfig.json` (as for a new tsconfig).
- Refinement predicates are not checked to exist or to have a `v is T` signature (a missing
  export fails when the generated import runs). Classes are matched by `instanceof` only:
  structurally equal objects from another realm fail.
- `ts-r-dev` inlines validators per file (not the record's per-declaration virtual modules);
  a type edit re-emits each dependent module through `Transform::deps` (`Expanded::deps`).
- Bundler plugins (Vite, Rollup, esbuild). The minified fast path is 0.85-0.91x typia
  (see "Speed against typia": the minifier's `const` to `let`).
- `object`, number index signatures, mapped types beyond the utility types, `import("x")` types,
  qualified names other than `ns.T` and `Enum.Member`.
- `${number}` holes use a regular expression (decimal, exponent, hex, octal, binary) that is
  close to but not exactly tsc's numeric-string rule.

## Dependencies

`ts_r_ast`, `ts_r_binder` (number formatting), `ts_r_checker` (only `ProgramFiles`,
`load_program` and `nodes::key_text`, no types), `ts_r_config` (file systems, tsconfig
options), `ts_r_parser` (listing a declaration file's types), `rustc-hash`. Dev:
`ts_r_testing`.

## Tests

- `tests/node.rs`: validators for every property of `Cases` in `tests/fixtures/types.ts` run in
  node (through `scripts/limit.sh`, 1 GB, 60 s) against `tests/fixtures/vectors.mjs`: accepted
  values, rejected values and exact `ParseError` messages (104 vectors). Skipped without node.
  Also checks every `Unsupported` property gives its reason.
- `tests/limits.rs`: deep nesting, self-reference and a 2000-member union.
- `tests/expand.rs`: call-site expansion on an in-memory project (lines kept, forms, errors
  with positions, shadowing, determinism) and `GuardFs`'s virtual tree.
- `tests/cache.rs`: the parsed-file cache (a 200-module chain is parsed 200 times, not
  20,100; a changed file is re-parsed by contents and by stamp, unchanged stamped files are
  not read; a same-length rewrite within the racy window is seen; `forget_files`/
  `forget_configs`).
- `tests/classes.rs`: user classes and refinements, inline and in shared modules (imports,
  bindings, every TSR1501 reason).
- End to end on `tests/fixtures/app` (45 valid and invalid inputs over every form, a class
  and refinements registered in its `tsrconfig.json`: a branded string, a branded number
  read by `query`, a plain alias; printed with the `ParseError` shape;
  `expected.txt`): `ts_r_run/tests/guard.rs` (also: errors, and
  a type edit reaching a cached run) and `ts_r_build/tests/guard.rs` (the bundle prints the
  same; shared modules, tree shaking, unbundled output, errors, size gate);
  `ts_r_dev`'s `guard` unit test (the dev transform).
