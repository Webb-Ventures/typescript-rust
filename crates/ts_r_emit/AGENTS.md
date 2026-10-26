# ts_r_emit

Program emit for `ts-r-check` (design record 0012, phase E1): where outputs go, what they
contain beyond the per-file transform, source maps, and writing them safely. Root rules:
[../../AGENTS.md](../../AGENTS.md). Spec:
[../../docs/design/0012-tsc-emit.md](../../docs/design/0012-tsc-emit.md) (tsc 7.0.2's observed
behaviour; tsc's emitter may be read to understand a rule, never translated).

## API in one breath

```rust
let checker = Checker::new(&program);
let resolver = CheckerResolver::new(&checker, &options);
let host = ts_r_checker::typenode::specifier_host(&fs, &options);
let cfg = EmitConfig { options: &options, config: Some(&parsed), threads: 4, case_sensitive, specifiers: Some(&host) };
let (located, global) = ts_r_emit::root_dir_diagnostics(&program, &cfg); // TS6059, TS5011, TS5009
let clashes = ts_r_emit::output_path_conflicts(&program, &cfg); // (5055 | 5056, output path)
let result = ts_r_emit::emit(&program, &resolver, &cfg);  // everything in memory
// all diagnostics known (noEmitOnError) -> then:
let (written, ts5033) = ts_r_emit::write_outputs(&result, &mut FsWriter::protecting(inputs));
print!("{}", ts_r_emit::listing(&written)); // --listEmittedFiles
```

Files: `lib.rs` (config, `emit`, the resolver, rootDir diagnostics, JSON printing),
`paths.rs` (output paths), `maps.rs` (map paths and JSON), `writer.rs` (`Writer`,
`FsWriter`, `MemoryWriter`), `dts/` (declaration emit, E2: `mod.rs` statements, classes and selection of what is
printed; `types.rs` type nodes, parameters and members; `reuse.rs` types written from
syntax; `isolated.rs` the `isolatedDeclarations` TS9xxx rules; `expando.rs` the namespace
written for a function's expando properties, `f.x = 1`). Declaration emit works on a clone
of the file's AST (inferred types are added to the clone, then rolled back between passes)
and prints it with its own printer: tsc's declaration layout (type literals always
multi-line, `/** */` comments only) differs from JS emit enough that sharing codegen would
cost both. It never mutates the checker's AST.

## Invariants

- **Never silently wrong** (record 0012, invariant 2). A file is skipped with an error rather
  than written wrong: syntax newer than the target that the transform could not lower
  (TSR2003 per file, JavaScript only, from `TransformResult::unlowered`, record 0012 E5), the
  resolver answering `Unknown` (TSR2002 per file),
  a declaration that would print an unresolved type (TSR2001, that file's `.d.ts` only),
  a type that is only reachable through a non-portable `node_modules` path (TS2883), an
  exported anonymous class type with private or protected members (TS4094), a type that
  does not follow from syntax under `isolatedDeclarations` (TS9xxx; tsc writes no `.d.ts`
  for such a file either; TS9023 once per expando property at its first assignment, where
  an element access key must be a literal or an entity name to declare one; a method
  with a non-literal computed name is TS9038 and still TS9008 without a return type; a
  literal name is a string, number, boolean, substitution-free template or signed number
  (`[-1]`); an object literal's TS9038 name that is an entity name is printed as written,
  so the local it names is printed too and may be TS9010 itself
  (tsc: tsc/internal/pseudochecker/lookup.go, tsc/internal/ast/utilities.go)),
  declarations of JavaScript inputs (TSR2006, JSDoc declaration emit is not implemented).
  `EmitResult::skipped` then makes the exit code 1. Declarations do not depend on the target:
  a file refused with TSR2003 still gets its `.d.ts`.
- **Nothing is written before every diagnostic is known**: `emit` only produces
  `OutputFile`s; the caller decides (`noEmitOnError`) and then calls `write_outputs`. The
  one exception is the caller's: ts-r-check writes next to the check when no diagnostic can
  stop the writes (no `noEmitOnError`, not incremental).
- **Writes are irreversible**: an output equal to an input path is dropped with TS5055 (and
  the file's map with it); duplicate output paths are dropped with TS5056, including their
  source files' maps. `FsWriter` additionally refuses any path that is, after following links, a
  program input (an `outDir` symlink back into the sources), reported as TS5033. tsc 7.0.2
  overwrites the input in that case; we deliberately do not. Files are compared by identity
  (device and inode on Unix, canonical path elsewhere), and the inputs' identities are only
  read once an output already exists. Writes are direct (no temp-and-rename) and happen
  even when bytes are unchanged, as tsc's; the directory is created only when the write
  fails with NotFound. `Writer::write_all` writes a batch; `FsWriter` spreads it over up to
  `with_threads` threads (default 4; writing is system calls), results in output order.
- **Deterministic**: files are handed to at most 4 worker threads (64 MB stacks) by an atomic
  counter, results kept in per-file slots and concatenated in program order; 1 and 4 threads
  give identical outputs (`tests/emit.rs`).
- Output order (tsc's `--listEmittedFiles`): program order (dependencies first, a JSON
  module before its importer), per file `.js.map` before `.js`.
- Paths are absolute, `/`-separated, normalized (`ts_r_config::path`).
- **JavaScript is printed as written**: the checker's tree of a JavaScript file carries the
  types its JSDoc desugared into; the emitter's clone drops them first (`Ast::strip_jsdoc`).
- **Layout**: JS is printed with `LAYOUT` = `Layout::Tsc` and the transform's layout hints
  (`PrintHints::single_line` / `multi_line`), so outputs are byte-comparable with tsc.

## Observed rules (tsc 7.0.2 probes; tests encode each)

- `rootDir`: explicit, else the tsconfig's directory, else (no tsconfig: the emit oracle) the
  common source directory. A source outside `rootDir` is emitted next to itself.
- No common source directory when two emitted files have different roots (drives,
  compared with the file system's case sensitivity): TS5009 (global, or at the `outDir`
  key) when `outDir` is set without `rootDir` or a tsconfig and some program file has a
  drive or URL root (`tsc: tsc/internal/compiler/program.go`,
  `tsc/internal/outputpaths/commonsourcedirectory.go`); TS5011 is then not reported.
- TS5011: `outDir` or `declarationDir` set, no `rootDir`, not `composite`, a tsconfig, and
  the common source directory differs from the config directory; anchored at the `outDir`
  key; not under `noEmit`. TS6059: explicit `rootDir` (or `composite` with a tsconfig, whose
  `rootDir` is the config directory) — every emitted file outside it (roots: a global error
  with the "The file is in the program because:" chain; imported files: at the first import
  specifier in program order, also when `include` matches them too, and then with the chain
  listing every reason: each import, then "Matched by include pattern 'p' in '<abs
  tsconfig>'"; checked against tsc 7.0.2 output); default `rootDir` with `outDir`/`declarationDir` — roots only. Both are option
  errors: no syntactic or semantic diagnostics are reported, emit still happens.
- TS5055 / TS5056 (`output_path_conflicts`; `tsc/internal/compiler/program.go`, the output
  path check): not under `noEmit`; for each emitted source in program order, its outputs
  (`.js`, its map, `.d.ts`, its map; JSON: the copy) are TS5055 when the path is a non-lib
  program file and TS5056 when an earlier output had it. Both can fire for one path, and
  repeats are kept (diagnostic sorting deduplicates). JavaScript counts only with `allowJs`;
  a non-root file under `node_modules/` (even reached by a relative import) never counts.
- JSON modules are written only into `outDir`, reprinted (`{ "x": 1 }`). One that is only
  imported (not a root) and lies outside `rootDir` (explicit, else the config directory) is
  not written and is no source of the layout: no TS6059, not in the common source directory
  behind TS5011, no TS6307 in `ts_r_projects` (`imported_json_is_emitted`; observed:
  `tsbuild/resolveJsonModule/include-only*`).
- Maps: see `maps.rs`'s module docs (location, `sources`, `sourceRoot`, `mapRoot`, URL
  forms, key order, inline data URLs).
- `.d.ts`: `stripInternal` drops a declaration when any of its leading comments (block or
  line) contains `@internal`; a comment on the line the previous token ends on is that
  token's trailing comment and does not count. An import whose bindings are all elided
  stays as `import "m";` only when `m` augments the importing module
  (`declare module "./this"`); `declare global` does not keep it. Under
  isolatedDeclarations such a kept import is TS9026 at the import, since keeping it needs
  the other file (tsc: tsc/internal/transformers/declarations/transform.go). Kept imports and
  parameter properties carry their own JSDoc. A hashbang line is kept at the top.
  `export default`/`export =` of a function or arrow expression is written as the export
  first, then `declare function <name or _default>(…)` with the statement's comments; only
  its parameters map (`Emitter::export_function`). `/** */` comments after the last
  statement are kept, followed by a space when no line break follows them in the source.
- `.d.ts.map` (E6, `dts/mod.rs` "declaration map"): every node kept from the source maps
  its start (first token) and its end; tokens such as `?`, modifiers and mapped-type
  `readonly`/`+`/`-` map their start and end; comments map start and end. Nodes the emitter
  builds map nothing: inferred types, a `declare` modifier list unless it is the source's
  exact list, parameter-property properties, `export default`/`export =` of an identifier,
  the parameters of a function type rebuilt from a function expression (`Emitter::quiet`,
  `rebuilt_params`). Segments merge with `SourceMapBuilder::add_tsc`, as JS maps do.
  Exceptions, observed with tsc 7.0.2 and kept by `TypeNodeCx::origins`/`Emitter::origins`:
  - parts of an inferred type tsc writes from the source's syntax are the source's nodes
    and map like them (annotations of signatures and properties, `as T`, literals of a
    `const` context, keys of an object literal written from its syntax; the rules are in
    `ts_r_checker/src/typenode/reuse.rs`). `inferred_type_from` passes the initializer or
    the only `return` expression for this;
  - a private method is written as a new property: no end mark, and a start mark only when
    the source's modifiers are all kept (`private async m()` has none).
- Output order per file: `.js.map`, `.js`, `.d.ts.map`, `.d.ts` (`--listEmittedFiles`).
- Refusals: TS4094 once per private or protected member of an anonymous class type (its
  static side's too, `#names` included), with a
  related TS9027 "Add a type annotation to the variable x." on a variable; TS4023 on a
  variable whose type recurs through another module's unexported alias
  (`NotNameable::CannotBeNamed`; other declaration kinds still print `/*elided*/ any`);
  TS7056 at the declaration's name when the type written from its structure passes tsc's
  serialization length (`NotNameable::TooLong`, `typenode::MAX_SERIALIZED_LENGTH`).
  A name written from the source that resolves to a declaration whose statement is not
  directly in a file or namespace body (`{ var a; }`, `if (x) { export var b; }`, a loop
  head) is a private name: such a statement is never printed (tsc's emit resolver never
  treats its container as visible, `tsc: tsc/internal/checker/emitresolver.go`). The error
  is at the name, worded by the declaration it is written in: variable TS4025, type alias
  TS4081, function parameter TS4078 / return type TS4060, class property TS4031 (static
  TS4028), interface property TS4033 (`Emitter::private_name`; other kinds print as before).
- An initialized `const` or `readonly` property whose annotated type is still a fresh
  literal (an alias of `typeof import("./m").default` for `export default 1`) is written
  `= 1` instead of its annotation (`Emitter::fresh_literal_of`). Only a `typeof import()`
  annotation, directly or through aliases, counts: tsc's `typeof k` is the regular literal,
  so `const e: typeof k = 1` keeps its annotation (the checker keeps `k`'s fresh literal
  there, which its `switch` narrowing relies on).
- A `[Symbol.x]` key is late-bindable even when `x` is typed `symbol`, unless
  `SymbolConstructor` has no `x` (TS2339): then the member is dropped like other
  unbindable keys (`Emitter::unbindable_key`).
- Malformed `import()` type attributes are written as parsed: the parser recovers them as
  empty `{ with: {} }` attributes. An `as` type holding an `assert` import type is not
  reused; the type is written from the checker (`typenode::has_assert_import_type`).
- ts-r-check gives the specifier host the program's project references
  (`typenode::specifier_host_with_references`), so a package specifier resolves back to a
  referenced project's output declaration.

## The resolver seam

`CheckerResolver` implements `ts_r_transform::EmitResolver::is_value_alias` with
`Checker::alias_is_value` (`ts_r_checker/src/emit.rs`): an alias whose target has a value
meaning stays; a const enum target without `preserveConstEnums` (or `isolatedModules`) is
not a value: its accesses are inlined. An enum with unevaluable values answers `Unknown`,
so the file is refused with TSR2002. `const_enum_value` answers an access of another
file's const enum member (`E.A`, `E["A"]`, `ns.E.A` through an import), or a local enum
whose initializer depends on an import, with
`Checker::const_enum_access_value`; under `isolatedModules` it answers nothing, so tsc's
property access stays. `enum_member_value` answers an enum member whose initializer reads
another file (``A = `${BAR}` ``, `G = 2 + BAR` with `BAR` imported) with
`Checker::enum_member_value`, under `isolatedModules` too: tsc folds those either way
(computedEnumMemberSyntacticallyString2). `jsx_runtime_found` is precomputed per file
(`jsx_runtimes_found`): the file's runtime specifier
(`ts_r_checker::jsx_runtime::runtime_specifier`) names an ambient module or resolves to a
program file. Without the program (per-file
transpile) `SyntacticResolver` keeps today's isolatedModules behaviour. ts-r-check builds a
second `Checker` for emit (the check's checker is internal to `check_program`); alias
resolution is lazy, so this costs the link step only.

`metadata_type` answers decorator metadata for a type reference in a decorated signature
(`emitDecoratorMetadata`), in our words after reading tsc's rule
(tsc: tsc/internal/checker/emitresolver.go, the type reference serialization kind):
the name is resolved as a value and as a type. A value that is the same symbol as the type
(a class, or a constructor-typed value such as global `Promise`) is `Value`, printed as an
entity (`a_1.Foo` under CommonJS); reached only through `import type`/`export type` it is
`TypeOnly("Function")` and keeps no import. A name that resolves to nothing is `Unknown`
(guarded: `typeof (_a = typeof X !== "undefined" && X) === "function" ? _a : Object`). Any
other type maps by its resolved type: `any`/`unknown` and object types `Object`, `void`,
`undefined`, `null` and `never` `void 0`, then `Boolean`, `Number`, `BigInt`, `String`,
tuples and arrays `Array`, `Symbol`, callables `Function`. Answers reached through a
type-only alias are `TypeOnly(..)`, which the transform's scope pass uses to drop the
import reference.

## Measurements

Latest (2026-10-27, v1/emit5, `cargo run --release -p ts_r_check --example emit_oracle --
--emitter program`): ES2022+ JS outputs 2,159: bytes 2,088 (96.7%), shape 2,137 (99.0%; the
22 others are in `emit_known_gaps.txt`), map segments equal on 17 of 18 bytes-exact outputs
with a map baseline; all 17,352 outputs: 15,888 bytes-exact (91.6%), JS 13,725 / 14,476
(94.8%), `.d.ts` 1,961 / 2,640. Declarations (E2): 1,191 of 1,315 baselines without
JavaScript inputs have every `.d.ts` bytes-exact (90.6%); 1,191 / 1,634 over all baselines.
The paragraphs below are the milestone measurements.

Emit oracle, `--emitter program` (2026-10-19, ES2022+ JS outputs, 2,159): bytes 1,946
(90.1%, gate >= 90% met), shape 2,012 (93.2%; the 147 others are listed in
`../ts_r_check/examples/emit_known_gaps.txt` with their cause), map segments equal on 0 of 17
byte-exact outputs with a map baseline (gate unmet: tsc maps node ends and more tokens than
our printer, see the codegen AGENTS.md). The record's E1 implementation notes have the
per-bucket history.

Declarations (E2, 2026-10-19): 1,127 of 1,320 baselines without JavaScript inputs have every
`.d.ts` bytes-exact (85.4%, gate 85%); 1,127 / 1,639 over all baselines. Consumer test
(brace-expansion, lru-cache, minimatch, rxjs): 261 / 261 `.d.ts` bytes-exact, 4 / 4
consumers identical. See the record's E2 implementation notes.

Real repositories (E6, 2026-10-19, `cargo xtask emit-repos`, 13 gated cases): 7,047 / 7,279
output files byte-identical with tsc 7 (96.8%, gate 95%; was 68.4%), peak RSS at or under
tsc 7's on 13 / 13 cases. See the record's E6 implementation notes.

Tests: `tests/emit.rs` (refusals, `using`, TS5055 overwrite protection, JSON copies, writer
order, 1-vs-4-thread determinism) and `tsc_layout_projects`: each `tests/tsc_layout/<case>/` (a tsconfig and sources, `expected/`
holding tsc 7.0.2's outputs, regenerated with tsc under `scripts/limit.sh`, never by hand)
is emitted through `MemoryWriter` and compared byte for byte. Real writes (a symlinked
`outDir` into the sources, `rootDir` outside the project, outputs over inputs,
`noEmitOnError`, `--listEmittedFiles`) are fixture-tested in `ts_r_check/tests/cli.rs`.

## Dependencies

`ts_r_span`, `ts_r_ast`, `ts_r_diagnostics`, `ts_r_config` (options, paths, globs),
`ts_r_checker` (program, checker), `ts_r_transform`, `ts_r_codegen`. Dev: `ts_r_testing`.
No external crates.
