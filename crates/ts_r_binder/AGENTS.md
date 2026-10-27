# ts_r_binder

Per-file binding (scopes, symbols, declaration merging, import/export bindings, local name
resolution, the control-flow graph and bind-time diagnostics) and the program-level link that
merges globals, ambient modules and augmentations across files and resolves module
specifiers through `ts_r_resolver`. Root rules: [../../AGENTS.md](../../AGENTS.md). Design
record: [../../docs/design/0003-binder.md](../../docs/design/0003-binder.md) (read its
"Merging and conflicts" rules before touching `declare`).

## API in one breath

```rust
let bound = ts_r_binder::bind(&ast, src, &BindOptions::default()); // BoundFile, plain data
bound.exports(); bound.script_globals(); bound.lookup(table, name); bound.resolved(ident);
bound.symbol_of_decl(node); bound.member_tables(sym); bound.flow; bound.diagnostics;
bound.dump(&ast, src); bound.dump_flow(&ast, src);              // deterministic text dumps
let p = Program::link_with(files, &resolver);                     // or Program::link(files, |spec, from, req| ..)
p.global("Promise"); p.module_target(file, request); p.resolve_export(target, "x");
p.resolve_alias(sym); p.export_names(target); p.diagnostics;
```

Files: `binder.rs` (the walk, merging, flow, diagnostics), `symbols.rs` (flags, facts and
the excludes table), `names.rs` (`Name` interning), `resolve.rs` (post-walk reference
resolution), `flow.rs` (graph types), `program.rs` (linking), `messages.rs` (catalog
texts), `dump.rs`.

## Invariants

- **tsc 7.0.2 is the spec.** tsc's source may be read to understand a rule (never translated,
  and never a Rust port of TypeScript; root AGENTS.md). Every
  conflict/message rule came from a probe (`npx -y -p typescript@7.0.2 tsc` on a tiny file;
  the observed facts are listed in the design record). Changing a rule needs a probe and an
  update to 0003.
- **Never panics, always terminates.** Recursion goes through the depth guard (`MAX_DEPTH`
  3000 plus a native stack budget); past it the binder reports TS99001 and stops descending
  (`tests/limits.rs` builds a 200k-deep AST). Alias / `export *` chains are bounded
  (`MAX_CHAIN` deep, `MAX_STAR_MODULES` modules per `export *` search; the all-paths
  `star_export_is_type_only` walk spends a `MAX_STAR_MODULES` call budget) and cycle-checked; program merging recursion is depth-limited (32). Every
  loop walks a finite list or moves strictly up a scope/parent chain.
- **Plain data out.** `BoundFile` holds ids into its own arrays (no pointers, no hash maps);
  identical input gives a byte-identical dump. Cross-file facts live only in `Program`.
- **Augmentations of re-exported names** (`declare module "./a" { interface Foo {} }` where
  `a` has `export { Foo } from "./b"` or `export *`) merge with the re-exported declaration,
  never with the re-export alias: `Program::augmentation_redirects` lists the pairs for the
  checker's symbol table, and conflicts through a named re-export are reported at the
  re-export (tsc 7: `mergeSymbolRexportFunction`). When the module's `export =` is a
  namespace, its exports count as the module's (fastify's `export type { FastifyInstance }`
  inside `namespace fastify`).
- **Augmenting an `export =` entity without namespace meaning** (a variable or class, or an
  expression that is no entity name: an object literal, a literal, a function or class
  expression, JavaScript's `module.exports = {...}`; not an enum, nor anything merged with a
  namespace) reports TS2671 at the augmentation's name
  and drops it (its target becomes `Unresolved`). A lone function is not reported yet (tsc 7's
  CLI does, its conformance baseline `augmentExportEquals2` does not). A variable merged
  with a type-only namespace does have namespace meaning: an augmentation of it that declares
  values is TS2649 at the augmentation's name instead, and is dropped too (`augmentExportEquals7`).
- **A resolver path outside the program** (`Program::link` returning a path that is none of
  its files, an untyped JavaScript package) leaves the request unresolved, but an
  augmentation of it is not TS2664: the checker reports TS2665 there.
- **A global conflicting with a type-only namespace** (`namespace A { interface I {} }` then
  `declare var A` in a later file) is TS2649 at the later declaration, not TS2300, and the
  later symbol is not merged (tsc 7: `mergeSymbol` in checker.go). ts-r marks an instantiated
  namespace `VALUE_MODULE | NAMESPACE_MODULE`, so "type-only" is `NAMESPACE_MODULE` without
  `VALUE_MODULE`.
- **`export =` lookups by text work**: `Names::find("export=")` is the reserved
  `Name::EXPORT_EQUALS` even when the file has a string atom `"export="` (typescript.d.ts
  has `ExportEquals = "export="`; checked before atoms), so `resolve_export` reaches an `export =`
  entity's exports for named imports. `default` is never taken from those exports: a default
  import of an `export =` module is the entity itself.
- **Implicit exports**: declaration-file modules, ambient modules and `declare namespace`
  bodies export everything (except `import x =`) unless they have an export clause or export
  assignment (`has_explicit_exports`); requests made inside an ambient module declaration
  carry `in_ambient_module` and relative ones are never resolved. Import/export statements
  in a module's augmentations (`declare module "x"`, `declare global`) carry
  `in_augmentation`; relative ones resolve only when the file imports the same specifier
  elsewhere (0003).
- **`resolution-mode`.** An `import type`, `export type ... from` or `import()` type whose only
  attribute is `resolution-mode: "import" | "require"` records it in
  `ModuleRequest::resolution_mode`; `link_with` resolves the request in that mode instead of
  the file's (custom `link` callbacks get the whole request). Other imports ignore it.
- **Module detection** (`file_is_module`): a JSDoc `@import` (`Ast::is_jsdoc`) does not
  make a file a module; its names are global in a script, as in tsc 7.
- **Whole-module aliases of scripts** (`Program::is_script_target`): `import * as ns` and
  `import x = require(...)` of a program file that is not a module (not `is_module`, not
  `commonjs`, not `.json`) resolve to nothing, so the alias is unknown (the checker reports
  TS2306).
- **JSDoc type aliases** (`@typedef`/`@callback`, `Ast::is_jsdoc`): at the top level of a
  JavaScript ES module they are exported; in a CommonJS module or a script they stay local.
  A JSDoc `@typedef default` that clashes with a default export is TS2300 under each
  declaration's own name, not TS2528.
- **CommonJS exports** (`bind_commonjs_exports`). A JavaScript module without ECMAScript
  module syntax (`BoundFile::commonjs`) declares, where `exports`/`module` is not a local
  name: `exports.x = v`, `module.exports.x = v` (and `["x"]`) a `PROPERTY` export `x` with
  `EXPANDO | EXPORTED | EXPORT_SPECIFIER`, declared by each assignment target; and
  `module.exports = v` the `export=` symbol, an alias when `v` is an entity name, else
  `EXPORT_EXPRESSION` declared by the assignment (more assignments add declarations;
  `module.exports = exports` is skipped). `Object.defineProperty(exports, "x", d)` (or
  `module.exports`; also a CommonJS indicator) declares export `x` by the call, `READONLY`
  unless `d` has a `set` member or `writable: true`. A JavaScript `require("lit")` call
  adds an `ImportEquals` module request.
- **Expandos.** `f.x = v`, `` f[`x`] = v `` and `f["x"] = v` declare expando members of `f`
  by their static text, and in JavaScript so does `Object.defineProperty(f, "x", d)`
  (declared by the call, `READONLY` like the export form; `F.prototype = v` is an ordinary
  expando too); `f[sym] = v` / `f[K.a] = v` (an entity-name key) are recorded in
  `BoundFile::late_expandos` for the checker to name by the key's type, and do not
  disqualify `f` from having expandos. A `const` (any variable in JavaScript) with a
  function initializer gets expandos with or without a type annotation. So does a JavaScript
  variable initialized to `{}` (a container: `var o = {}; o.x = 1`), from assignments in
  the same function. A JavaScript class declaration, and a JavaScript variable initialized
  with a class expression, get expando statics (`C.x = v` declares `x` in the class
  symbol's exports, `expando_classes`). `export default function f() {}` has a local and
  an export symbol: both exports tables hold its expandos (one property symbol). In a JavaScript class, `this.x = v` in a constructor, method, accessor
  or static block (arrows included, nested functions and classes not) declares the member
  `x` (static in static methods and blocks) unless the class declares it
  (`declare_this_properties`): a `PROPERTY` with `EXPANDO`, declared by the assignment
  targets (fixture `js_containers.js`).
- **JSDoc side types.** A JSDoc `@type` of a function, an assignment target, an object
  literal property or an `export default`, and each `@overload` signature, is no field of the tree (`Ast::jsdoc_tags`); it is bound where its host is (`bind_jsdoc_side_types`), so
  its names and `import("m")` types are resolved like an annotation's (member assignment
  targets included: `bind_member_assignment`).
- **Static blocks run inline.** A class static block gets its own `FunctionFlow` entry (an
  unlinked start node) but its body continues the surrounding flow, and the flow after the
  class continues from the end of the block. A class's static blocks are bound first (in
  order); the other members follow from the last block's end, and the flow after the
  class drops their effects (assignments in computed names do not reach it), as tsc 7
  behaves.
- **Which closures continue the outer flow.** The start node of a function expression,
  arrow, object literal method or accessor, or a method or accessor of a class
  *expression* links to the flow where it is created (the checker narrows captured
  variables through it). Constructors and members of class declarations start unlinked,
  as tsc 7 narrows (fixture `closure_narrowing_containers.ts` in the checker).
- Parameter properties (`constructor(readonly x: T)`) are instance members with
  `SymbolFacts::READONLY` when `readonly`, like property declarations.
- **Optional chains branch.** In `a?.b.c(x)` the rest after each `?.` (keys, arguments,
  closures created there) runs under a `TrueCondition` on the object flagged
  `flow_flags::NULLISH` (not nullish); the outermost chain node joins those flows with the
  matching `FalseCondition`s. Parentheses end a chain.
- **Assignment order.** A member or element target (`(x = "", o).p = v`, `o[k] += v`) binds
  its object and key before the value, the assignment after it; identifier and pattern
  targets bind the value first. A node whose flow is recorded twice (the target of
  `x ||= y` is read, then assigned) keeps the first record: `node_flow` is sorted stably.
- **Type queries read the flow.** The identifier of `typeof x` in a type gets the flow
  where the type is bound, so the checker narrows it there (`(b as typeof a).x` after
  `a.kind === "a"`, observed tsc 7). A qualified `typeof a.b` records the flow on its
  `QualifiedName` node (`typeof x.p` after `x.p === "A"` is `"A"`) and on its leftmost
  identifier, which the checker uses for the whole name when the node has none
  (`typeof this.a.b` inside `if (this.a)`).
- **JSX tags read the flow.** A value tag (`<C />`, not an intrinsic lowercase name)
  records the flow at its identifier, and a `<o.n.C />` tag at each `JsxMemberExpr` and at
  its root identifier, so `if (C) <C />` narrows the tag (tsc binds a tag name like any
  other reference).
- **Array mutations.** Every `xs.push(..)` / `xs.unshift(..)` call (not only a statement)
  gets an `ArrayMutation` node after its arguments, with an exception edge like a call;
  its object is a reference through parentheses, the last expression of a comma list and
  an assignment's target (`(xs = [], xs).push(1)`; tsc: binder/binder.go,
  bindCallExpressionFlow). Statement calls that are not mutations get a `Call` node.
- **Switch fallthrough.** `FlowGraph::fallthroughs` lists each non-empty clause but a
  switch's last whose end is reachable in the binder's graph, with the flow at its end
  (empty clauses group with the next). The checker reports TS7029 for those still
  reachable after call narrowing (`noFallthroughCasesInSwitch`).
- **Checker-level unreachable code.** Under `allowUnreachableCode: false` only,
  `FlowGraph::stmt_flow` records the flow before each statement the binder finds reachable
  but would report as unreachable (`is_reportable`); the checker reports TS7027 for those
  its call narrowing makes unreachable. Nothing is recorded otherwise.
- **Binder diagnostics ⊆ tsc's.** An extra diagnostic is a bug; a missing one may be the
  checker's job (see the gaps below). TS2669 (misplaced `declare global`) is only reported
  for a declaration directly in a file or module body; in a block or function body the
  checker reports TS1234 instead. `export` on a quoted module or `declare global` is TS2668
  at the first modifier (tsc: binder/binder.go, bindModuleDeclaration). A quoted module at
  a script's top level whose name is relative (`.`, `..`, `./`, `../`, backslash forms) or
  rooted (`/`, `\`, `c:`, `c:/`, `^/`, a URL with a path) is TS2436 at the name; tsc
  reports it from checkModuleDeclaration (tsc: checker/checker.go, tspath/path.go
  IsExternalModuleNameRelative), but it needs nothing beyond the binder's context.
  TS5061: an ambient module that is not an augmentation
  (a script's, or one not at a module file's top level) whose name has more than one `*`,
  at the name (`tsc: tsc/internal/binder/binder.go`, module declarations).
- **Type parameters of a function** (`PendingRef::in_type_params`) do not see its parameters
  or its body's locals (`<T extends typeof a>(a: T)` leaves `a` free). A symbol that is
  also a type parameter keeps that meaning (tsc: checker.go resolveNameHelper, the
  function-like locals rule).
- **Entity names through merged globals.** `Program::resolve_entity` looks a member of a
  global namespace up in every file's declaration of it (`A.B` from inside `namespace A.C`
  of one file finds `B` from another file's `namespace A`).
- **Assignment-pattern defaults** (`[x = d] = v`, `{ x = d } = v`) are bound in order with
  no branch, unlike a binding pattern's defaults (`bind_assign_target`; tsc: binder.go,
  destructuring assignment flow; parity fixture `thenables_tuples_and_pattern_defaults`).
- Front-end crates are read-only for this crate; parser problems go to
  [../../docs/design/ast-requests.md](../../docs/design/ast-requests.md).

## Dependencies

`ts_r_ast`/`ts_r_span`/`ts_r_diagnostics` (input and output types), `ts_r_resolver` +
`ts_r_config` (module resolution and its options for `Program::link_with`), `rustc-hash`
(hash maps during binding only). Dev: `ts_r_parser`, `ts_r_testing` (memory cap).

## Tests and harnesses

- `cargo test -p ts_r_binder`: `tests/fixtures.rs` (binder dumps of `tests/fixtures/*`,
  snapshots in `tests/snapshots/`, diagnostics checked against tsc; accept new output with
  `UPDATE_SNAPSHOTS=1`), `tests/program.rs` (cross-file merging, augmentations, export
  resolution; expectations from tsc), `tests/limits.rs` (pathological inputs).
- Conformance (needs `cargo xtask upstream sync`):
  `cargo build --release -j 4 -p ts_r_binder --example conformance` then
  `scripts/limit.sh --mem-mb 4096 --time-s 600 -- target/release/examples/conformance [filter]`.
  Compares the binder's codes (2300, 2451, 2567, 2528, 2397, 2435, 2436, 2649, 2664, 2668, 2669, 2671, 5061, 1100-1215 strict
  mode, 1101, 1102, 1114, 1117, 1344, 7027, 7028) per test variant with the baselines and
  writes mismatches to `target/binder-conformance.txt`. Like the checker's driver, it drops
  TS1xxx codes outside the binder's own (`BINDER_CODES`) in a file whose parse has errors
  other than checker grammar codes (`CHECKER_GRAMMAR_CODES`); keep both lists in step with
  `crates/ts_r_checker/src/driver.rs`.
  Current (2026-10-24, parity/binder-emit): 12783 variants; 464 expect binder diagnostics, 418 match exactly; 1720 of 1839
  expected diagnostics matched (119 missing, 19 extra).
- Corpus: `cargo build --release -j 4 -p ts_r_binder --example corpus` then
  `scripts/limit.sh --mem-mb 4096 --time-s 600 -- /usr/bin/time -l target/release/examples/corpus ../private-app`.
  private-app (691 project files + 5219 `node_modules` declaration files, 48 MB):
  parse+bind ≈ 40k files/s on 4 threads, bind alone ≈ 70k files/s/thread, link ≈ 0.1 s,
  peak RSS 367 MB (all ASTs kept), 0 panics, 0 binder diagnostics in project files.
- Debugging: `cargo run -p ts_r_binder --example binddump -- [--flow] [--unreachable] f.ts`,
  `--example astdump`.

- `delete` of a missing operand (`delete ;`) is TS1102 in strict code: a missing
  expression is an identifier to tsc.
- `export as namespace N` (tsc: binder.go, the namespace-export binding): modifiers on it
  are TS1184; then it is TS1316 below the top level (inside a namespace or an ambient
  module), TS1314 in a script, TS1315 in an implementation file, and only otherwise does
  it declare the UMD global (`umd_name`; `umd_export_placement`).

## Known gaps (where the remaining mismatches are)

- TS1212/1213/1214 in files with syntax errors: tsc's binder reports strict-mode reserved
  words only in a file without parse errors; the checker's driver drops them. Its other
  TS1xxx errors (TS1100, TS1101, TS1102, TS1184, TS1210, TS1215, TS1314-TS1316, TS1344)
  stay whatever the parser found (tsc: tsc/internal/binder/binder.go, the contextual
  identifier check is the one guarded by the file's diagnostics).
- Late-bound names (`[sym]`, `[E.member]`, unique symbols: TS1117/TS2300) need types.
- TS7027 after never-returning calls, assertion functions and exhaustive switches needs
  types (checker).
- Conflicts with lib declarations (`Symbol`, `Promise`) need lib files in the program.
- Harness-only: `@ts-ignore`/`@ts-expect-error` suppression, automatic `@types` inclusion and
  package-id deduplication of identical `node_modules` copies are driver concerns.
