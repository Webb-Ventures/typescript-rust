# ts_r_transform

TypeScript/TSX → JavaScript transforms matching tsc 7.0.2's emit: type erasure, classes,
enums, namespaces, decorators, JSX, import elision and module formats. Used by ts-r-run, the
dev server (feature `hmr`) and later the bundler. Root rules: [../../AGENTS.md](../../AGENTS.md).
Design: [../../docs/design/0002-transform-and-codegen.md](../../docs/design/0002-transform-and-codegen.md);
AST: [../../docs/design/0001-ast.md](../../docs/design/0001-ast.md); printer:
[../ts_r_codegen/AGENTS.md](../ts_r_codegen/AGENTS.md).

## API in one breath

```rust
let opts = TransformOptions::from_compiler_options(&parsed.options, path, is_cjs_file);
let out = ts_r_transform::transpile(path, src, &opts, &CodegenOptions::default());
out.code; out.map; out.diagnostics; out.specifiers;
// or: let r = transform(&mut ast, src, &opts); then ts_r_codegen::print_with(.., &PrintHints {
//     erased: &r.erased, prelude: &r.prelude, single_line: &r.single_line, multi_line: &r.multi_line,
//     map_ranges: &r.map_ranges, moved_comments: &r.moved_comments, .. })
// program emit: transform_with(&mut ast, src, &opts, &resolver, file) with an EmitResolver
```

`EmitResolver` is the cross-file seam (record 0012): `is_value_alias` answers whether an
import alias names a value (`Yes`/`No`/`Unknown`). `SyntacticResolver` (per-file transpile)
answers from the file alone, as isolatedModules; `ts_r_emit::CheckerResolver` asks the
checker. `const_enum_value` gives the value of another file's const enum member for an
access whose root is an import, or a local const enum member the file-only evaluator could
not resolve (`enums.rs` inlines it as `0 /* E.A */`). `enum_member_value` folds an enum
member whose initializer reads another file (`A = Other.B`); it is asked only for original
members the file-local evaluator could not fold (default `None`; `CheckerResolver` does not
answer it yet). `jsx_runtime_found` says the
automatic JSX runtime module (`react/jsx-runtime`) resolves in the program: JSX then no
longer keeps the classic factory's root import (`React`) alive, as in tsc; the per-file
default is `false` (keep it).
`export default X` where `X` is an import of a type only (`type_alias_imports`) is erased
with the import, as tsc does. Constructs answered `Unknown` are listed in `TransformResult::unknown`; a program
emitter must not write that file (TSR2002).

`build_errors(&parsed)` picks the parse errors a build tool fails on, as esbuild does
(`build_errors.rs`): syntax errors and the grammar errors that are ECMAScript early errors
or syntax errors in JavaScript (`a && b ?? c`, TS5076), not tsc-only grammar rules
(`declare function f() {}`, `if (x) ;`, `a?.#x`).
`TranspileOutput::build_errors` holds those plus the transform's errors; the dev server
fails on them, while `diagnostics` keeps everything tsc reports. `sloppy_only_syntax` lists
`with` statements and `delete name` with whether the file itself makes them strict code;
`has_module_syntax` says whether `import`/`export` does (the bundler decides the error).

`TransformOptions::from_json(r#"{"module":"commonjs"}"#, path)` builds options from a
`compilerOptions` object (tests, tools). `ModuleFormat::{Esm { node_require }, CommonJs,
Preserve}` is per file. `hmr::TsTransform` (feature `hmr`) implements `ts_r_hmr::Transform`.

`TsTransform::for_root(root)` resolves options per file the way Vite does (tsconfck 3.1.6,
`src/find.js`, `src/util.js`): the nearest `tsconfig.json` up from the file's directory, with
no `include` check; only when that config has `references` and doesn't include the file, the
first referenced config that does (`files`, else `include` (default `**/*`), minus `exclude`).
Browser adjustments (ESM, at least ES2022, automatic JSX for `preserve`) are applied after.
`map_options` changes every file's options and keeps the lookup; `TsTransform::new` uses
one fixed set. `deps()` lists the config chain, so editing a tsconfig invalidates the cache.
Parsed configs are cached and re-read when the config or one it extends changes.

## Module layout

| File | What |
|---|---|
| `transform.rs` | `Tx` (the pass state) and the main walk: statements, expressions, patterns, functions, imports/exports, elision, temporaries |
| `scope.rs` | per-file scope analysis run first: bindings, resolved references, type names, namespace/enum scopes |
| `builder.rs` | node constructors (`Build` trait), `replace`, `clone_node`, `deep_clone` (through the generated `Ast::map_children`) |
| `class.rs` | parameter properties, fields under both `useDefineForClassFields`, `accessor` lowering, constructor synthesis |
| `decorators.rs` | legacy (`experimentalDecorators`) decorators and `emitDecoratorMetadata` |
| `es_decorators.rs` | TC39 decorators lowered to `__esDecorate`/`__runInitializers` IIFEs (native only for ESNext with `useDefineForClassFields`) |
| `enums.rs` | enums, constant folding, const enum inlining |
| `namespace.rs` | namespaces/modules as IIFEs |
| `cjs.rs` | CommonJS output: requires, interop helpers, exports, live bindings |
| `jsx.rs`, `entities.rs` | JSX classic/automatic/dev, pragmas, text folding, HTML entities |
| `helpers.rs` | helper texts (captured from tsc output) and their dependencies |
| `options.rs` | `TransformOptions` from tsconfig options |
| `build_errors.rs` | which parse errors fail a build (esbuild's rule, observed) |
| `downlevel/` | lowering for targets below ES2022 (record 0012, E5): `lower.rs` is the one walk (function contexts, hoisted temporaries, unique names), `operators.rs` (ES2021 logical assignment and separators, ES2019 catch, ES2016 `**`), `es2020.rs` (`?.`, `??`), `es2018.rs` (async generators, `for await`, object spread), `rest.rs` (object rest), `template.rs` (tagged templates with invalid escapes: `__makeTemplateObject`), `es2017.rs` (async functions, `_super`, `arguments`), `es2022.rs` (class statics after the class, class aliases, `Reflect.get` / `Reflect.set` for static `super`, `useDefineForClassFields` defines, private member storage), `private.rs` (private name
uses: `__classPrivateFieldGet`/`Set`/`In`, receivers captured for compound and update
operators, `.call`/`.bind` for calls and tags), `using.rs` (`using` / `await using` below ESNext; at ESNext tsc 7 keeps every `using`, a module's top-level one followed by exports too: `env`/`try`, hoisted declarations, regrouped exports, CommonJS `void 0` list order); `mod.rs` `unlowered` lists output syntax newer than the target (TSR2003) |
| `hmr.rs` | dev-server adapter (feature `hmr`) |

## Invariants

- **tsc 7 emit is the spec**, observed through the oracles below; tsc's source may be read
  to understand a rule, never translated (root AGENTS.md). A behaviour change needs a fixture (or a baseline) proving tsc does it.
  Exception: optional and parenthesized static `super` calls in class-decorated classes
  preserve the receiver and short-circuiting, even where tsc 7.0.2 loses them (record 0002).
- **In place, children first**: visit children, then `replace(ast, slot, new)`; never hold a
  `&Node` across a mutation (nodes are `Copy`; read, then write).
- **Synthesized nodes have empty spans**; a synthesized node's start is its source-map hint.
- **Source-map ranges** (`Layout::Tsc`): a synthesized node tsc maps gets `Tx::map_range(id,
  span)` (reaches codegen as `TransformResult::map_ranges`, sorted, last one wins). Observed
  ranges: namespace `var N;` and IIFE statement → the declaration (a nested segment's from
  its name), names / `P.N` / `exports.N`'s `N` → the segment's name, the IIFE's `}` → the
  body's; enums the same with the declaration *list* (through the declarator) and each
  member's statement and outer assignment → the member; `N.x`/`exports.x` targets and
  references rewritten to them → the name they replace; `N.f = f;` → the declaration;
  CommonJS `require` declarations and `module.exports =` → the import / `export =`; a moved
  field's `this.x = v;` → the field and `this.x` → its name (`x` unmapped); a synthesized
  constructor and its `}` → the class; parameter properties: the value and the field key →
  the name, and with moved initializers also `this.p` → the name, the statement → the
  parameter from its name. CommonJS: `exports.x = v` of an exported variable → the
  declarator (each destructured leaf's `exports.a` → its element), references rewritten to
  `exports.x` → the reference, `__exportStar(...)` → the `export *`, `exports.ns = ns` of a
  re-exported namespace import → the specifier; exported function and class names in
  `exports.f = f` are unmapped. Namespace destructuring targets map to their pattern (a
  shorthand leaf to its key). `(x as T)` stripped of its parentheses → `(` to `)`;
  `(0, mod.f)` has no range, its `mod.f` maps to `f`. E6 (real repositories): CommonJS
  preamble statements map nothing (`NO_MAP` both sides); `export { x } from` maps the
  `require` to the declaration and each getter and its value's property to the specifier; a
  re-exported import's getter value maps the import specifier; `export * as ns` maps the
  statement to the declaration and `exports.ns` to the name; an exported arrow or function
  expression's stand-in declarator sits at the statement start; a static field's
  `static { this.x = v; }` block maps nothing, its statement ends at the field's end (the
  assignment at the key's end only in a class that also moves instance initializers into
  the constructor);
  `this["k"]` maps the key and the cloned key maps nothing.
- **Moved comments**: a statement moved out of its place (an instance field initializer
  into the constructor) is recorded in `Tx::moved_comments` with the original span
  (`TransformResult::moved_comments`, sorted), so codegen prints the field's comments with
  it instead of at the field.
- **Scope results describe the original tree**: `Scopes::binding_of_ref` only knows parsed
  identifiers. Look a name up before replacing its node.
- **`require` specifiers written for imports are synthesized double-quoted strings**
  (`cjs::double_quoted`): tsc 7 prints `require("y")` for `import x from 'y'` and
  `import x = require('y')`, but keeps `import('y')`'s quotes in `require('y')`.
- **Erased ranges go into `Tx::erased`** so codegen drops their comments (also a
  namespace body's `{`, so the comments on its line go: tsc drops them).
- **Layout hints**: a synthesized block or embedded statement tsc prints on one line is
  marked with `Tx::mark_single_line` / `mark_single_line_body` (accessor get/set pairs,
  CJS export getters, `static { _classThis = this; }`, `static { __setFunctionName(...); }`,
  computed-key static blocks, private-member forwarders and descriptor functions, the
  access object's `set` arrow, `if (_metadata) ...`), an array tsc prints one element per
  line with `mark_multi_line` (`__decorate([` lists). They reach codegen as
  `TransformResult::single_line` / `multi_line`; every other synthesized block breaks.
- **Lowering runs after the TypeScript transform**, on its output tree, only below ES2022
  (`downlevel::lower::run`); whatever it leaves newer than the target is reported by
  `downlevel::unlowered` and the program emitter refuses that file (TSR2003). Each year's
  `var` of temporaries is its own statement after the prologue, ordered ES2016, ES2017,
  ES2018, ES2020 chains, ES2020 `??`, ES2021, ES2022 classes (tsc's transformer order).
  Temporaries and unique names (`arguments_1`, `e_1`, `it_1`) are placeholders named in print
  order once the tree is final (`Lower::name_temps`); never pick a final name while lowering.
  The TypeScript transform's temporaries (`Tx::temp_atoms`) are renamed in the same pass;
  class temporaries (`Lower::reserved`) keep their names out of nested functions.
  Unique names are numbered scope by scope: a function's own names in order, then its nested
  functions' (`Lower::name_uniques`).
- **`using` lowers before the year lowering** (`downlevel::using::run`, after the TypeScript
  transform, so it sees CommonJS output: `exports.x = ...` statements, the `void 0` list it
  reorders through `cjs::State::void0_stmt`). Its `env_N`/`e_N`/`result_N` are fresh names in
  creation order (a scope's statements, then its functions); below ES2022 `lower.rs` adopts
  them as unique names (`Tx::using_names`) so a `for await`'s `e_1` interleaves as tsc's does.
  `using::remaining` walks the final tree; a `using` directly in a `case` clause stays (tsc
  leaves it).
- **Private names below ES2022** (`es2022.rs` declares, `private.rs` rewrites uses): each
  lowered class pushes a `PrivEnv` on `Lower::privates` while its body is visited; a use
  resolves the innermost class declaring the name. Storage is hoisted into the ES2022 `var`:
  `_C_instances` first, the alias when static code or a static private needs it, then the
  storage in member order with an `accessor`'s storage (`*_accessor_storage`, unless the TC39
  decorator transform made it: `Tx::decorator_storages`) last. A private class evaluated in
  a loop gets an arrow IIFE with its own storage and alias temporaries; a declaration becomes
  a `let` initialized by that IIFE. Outer `await` / `yield` in its evaluation and `super` in
  extracted private methods stay native and are refused with TSR2003. The `WeakMap`s and `WeakSet`
  are created in the first computed method key if there is one, else first in the class's
  sequence; method functions and key captures follow in member order, other static
  expressions after them. Generated storage names for computed `accessor` keys (`_a`, `_b`)
  are counted across the file apart from temporaries (`Tx::storage_count`). A private
  destructuring target becomes `({ set value(_a) { __classPrivateFieldSet(...); } }).value`,
  its receiver captured first (`(_b = this, [...] = v)`, `Lower::priv_caps`); the setter's
  parameter is one name per function, named after its hoisted temporaries
  (`Lower::setter_param`, the setter listed in `Lower::transparent`). Private names that
  no class declares, or in object literal keys, print as nothing (`this.`; errors, as tsc).
  A logical assignment to a private name (`this.#x ??= v`) reads once and sets only when the
  operator says so (`get(...) ?? set(..., v)`); below ES2020 the `??` is lowered too, and an
  `**=` below ES2016 goes through `Math.pow` (`private.rs`).
- **`super` in an async arrow of a plain method** (below ES2017): the arrow becomes an
  `__awaiter` generator, which can't see `super`, so the enclosing method gets the same
  `_super` object of `super` property getters an async method gets (`super_in_async_arrow`,
  `super_names`, `es2017.rs`); only code inside lowered async functions reads through it.
  Those walks also enter a nested class's heritage, computed keys and key captures
  (`class_outer_code`), which run in the method. Below ES2020 an optional `super` call
  (`super.m?.()`) goes through the optional chain lowering with `_super.m` as its base and
  `this` as the receiver (`(_a = _super.m) === null || _a === void 0 ? void 0 :
  _a.call(this)`, `lower_chain`).
- **Static `super` in nested classes** (`es2022.rs` `usage` / `subst`): a key capture is
  evaluated outside its class, so its `this` and `super` are the outer ones; the walks enter
  nested classes with the outer flags but only a synthesized key-capture block of theirs
  (`Tx::inline_static`), never their methods or field initializers. A class expression's key
  captures come before its `__setFunctionName` call (observed).
- **Static `super` in class-decorated classes** (`es_decorators.rs` `StaticSuper`): reads go
  through `Reflect.get(_classSuper, k, _classThis)`, writes through `Reflect.set` (the value
  `(Reflect.set(.., _a = v, ..), _a)` unless discarded: an expression statement, a comma
  operand, a `for` initializer or update). Compound and logical assignments both store
  `Reflect.get(..) op v` (tsc 7 does not short-circuit `??=` there); a `for...of` target
  `super.x` is read, as tsc does. Temporaries belong to their static block (a `var` first),
  arrow (body made a block) or field initializer (`(() => { var _a; return v; })()`), each
  named from `_a` again; a destructuring setter's parameter too (observed in
  `esDecorators-class{Declaration,Expression}-classSuper.*`).
- **Misplaced `accessor`** (an error) on a method, accessor, constructor or class stays when
  `accessor` fields are not lowered (ESNext), and an `accessor enum` gives `accessor var E;`
  at every target (autoAccessorDisallowedModifiers).
- **Static privates of class-decorated classes** (`static_privates.rs`, ES2022 <= target <
  ESNext): as tsc does, every static private member becomes a variable declared first in
  the decorator IIFE (a class decorator may replace the class, so `#x` brand checks go
  through `_classThis`). A field becomes `static { _C_f = { value: v }; }`
  in place; methods and accessors become one static block of `_C_m = function _C_m() {}`
  assignments, after the `_classThis` and `__setFunctionName` blocks. An undecorated
  `accessor #x`'s storage variable comes after its `_get`/`_set` ones. Uses become
  `__classPrivateFieldGet/Set/In` with `_classThis` as the brand, visited in source order
  (that order requests the helpers). A nested class that declares the same private name
  shadows it. Observed in tsc's `esDecorators-classDeclaration-classThisReference`,
  `esDecorators-classDeclaration-{fields,methods,accessors}-staticPrivate*`
  baselines (no tsc source read).
- **Named evaluation** (`transform.rs` `key_class_hint` / `expr_named`): an anonymous
  decorated class gets `__setFunctionName(_classThis, "n")` from where it is assigned:
  `x = class {}`, `let x = class {}`, an object literal key (identifier, string, number or
  a computed literal; never a plain `__proto__`), shorthand and binding defaults
  (`{ e = class {} }`, `{ f: g = class {} }` takes `g`), a class field's key (also through
  a decorated field's `__runInitializers(this, _x_initializers, v)`), and
  `export default (class {})` (`"default"`). A computed key that is not a literal is
  captured: `[_a = __propKey(f())]` and `__setFunctionName(_classThis, _a)`
  (`Tx::key_named`). Observed in tsc's
  `esDecorators-classExpression-namedEvaluation.*` baselines (no tsc source read).
- **Legacy decorator class alias** (`C_1`, for self-references): from ES2022 a class with a
  static field or block assigns it in a first `static { C_1 = this; }`; otherwise
  `let C = C_1 = class C {}` (tsc: tsc/internal/transformers/tstransforms/legacydecorators.go).
- **`accessor` storage names**: `#x_accessor_storage`, made unique against the private
  names in the source (`#x_1_accessor_storage`); other keys `#_a_accessor_storage`,
  `#_b_...`, numbered per class from ES2022 (across the file below it), apart from
  temporaries.
- **Computed keys using `this`/`super`** (ES2022+, captured into a static block): the
  capture runs in an arrow made before the class, `(_c = () => { _a = this.k; }, class {
  static { _c(); } })`, or a `_c = () => { ... };` statement before a declaration
  (`Tx::class_key_capture`).
- **Bundler-only options**: `TransformOptions::default_class_name` names an anonymous
  `export default class` that ES2022 lowering must name (the bundler passes `<stem>_default`,
  esbuild's name); tsc never sets it (`None`).
- **Static blocks the TypeScript transform synthesizes** for a static initializer or a key
  capture are listed in `Tx::inline_static`: below ES2022 they become a statement after the
  class (`C.x = 1;`, `_a = k`), source static blocks an IIFE. Push every such block there.
- **Decorator metadata** (`decorators.rs`, `scope.rs`): a type reference asks
  `EmitResolver::metadata_type` (the program emitter answers from the checker, see
  `../ts_r_emit/AGENTS.md`; per-file transpile answers `Syntactic`: what the file shows).
  Unions, intersections and a conditional type's two branches serialize to their members'
  common value or `Object`; `never` is dropped (an intersection with it is `void 0`),
  `null`/`undefined` only without `strictNullChecks`. Each guarded member builds its guard
  before the comparison, so its temp is declared even when the union ends as `Object`. An
  accessor's type is its setter's parameter type when there is a setter, else the getter's
  return type. `BigInt` below ES2020 is `typeof BigInt === "function" ? BigInt : Object`.
  Which imports stay alive is a separate rule (tsc: tsc/internal/checker/checker.go,
  marking decorator metadata references): a decorated class keeps its constructor's
  parameter types, a method its parameter and return types, a parameter decorator its
  whole signature's, a field its type, an accessor its own annotation or else its pair's;
  a rest parameter keeps its element type (`...a: T[]`, `...a: X<T>`); a union keeps an
  entity only when every member (skipping `never`, and `null`/`undefined` without
  `strictNullChecks`) is a reference to the same plain name; a plain name reached only
  through `import type` keeps nothing. Decorated `declare` fields are decorated.
  (tsc: tsc/internal/transformers/tstransforms/typeserializer.go for the serialization.)
- **Helpers**: `Helper::ALL` order, then `Helper::REQUEST_ORDER` ones (`__await`,
  `__asyncGenerator`, `__asyncValues`, `__asyncDelegator`, `__classPrivateField*`) in the
  order first requested. A class with an `accessor` storage asks for `__classPrivateFieldGet`
  and `Set` before its members are visited (tsc makes the accessor's getter and setter first).
  CommonJS helper imports are inserted after all lowering passes have requested helpers.
- Deterministic output; `TransformOptions::cache_key` (the `Debug` form) covers every field,
  so add new behaviour switches as fields.
- No panics on any parsed input (error-recovered trees included); deep trees run on a big
  stack (`lib.rs`).

## Oracles and tests

- `cargo test -p ts_r_transform` (and `--features hmr`): unit tests, plus
  `tests/fixtures.rs`, which transpiles every fixture under `tests/fixtures/<feature>/` and
  compares it with tsc's stored output (`<name>.tsc.js`) by AST shape. Fixture options are
  `// @option: value` header lines. `KNOWN_GAPS` lists fixtures tsc emits differently; it may
  only shrink.
- Re-bless after adding a fixture (needs tsc 7):
  `scripts/limit.sh -- cargo run --release -p ts_r_transform --example oracle -- --bless crates/ts_r_transform/tests/fixtures`
  (`--stored` compares against the stored files without tsc).
- tsc's emit baselines (upstream checkout in `target/upstream/typescript`):
  `scripts/limit.sh -- cargo run --release -p ts_r_transform --example baseline_oracle -- [--filter S] [--show N] [--list FILE] [--dump DIR]`
  (`--dump` writes each mismatch's normalized outputs as `<test>__<file>.ours.js` / `.tsc.js`). A test's own `tsconfig.json`
  supplies options under its `// @` lines; outputs pair with inputs of the same stem that match first.
  Last run (2026-10-27, v1/emit5): 1,723 match, 4 mismatch (tests below ES2022, `outFile`, AMD/System/UMD and
  syntax errors are skipped); `--filter esDecorators`: 140 match, 0 mismatch. The 4 left need
  another file's declarations, which a per-file transpile cannot see (an imported enum
  value, an imported const enum, an alias of an imported interface); the program emitter
  (`ts_r_emit`) matches them.
- Ecosystem: `cargo xtask ecosystem --top 1000 --stages transform` (see
  [../../ecosystem/README.md](../../ecosystem/README.md)) compares shipped TS sources with
  tsc's emit. Last run: 48/48 packages pass.
- Real projects: `scripts/limit.sh -- cargo run --release -p ts_r_transform --example project_oracle -- <tsconfig.json>...`.
  private-app `apps/*` and `packages/*` (15 tsconfigs, 339 files): 339/339 match, 0
  unparsable, 18.7 ms to transpile 1.1 MB.
- `tests/tsc_layout.rs`: each `tests/tsc_layout/<name>.ts` transpiles under
  `Layout::Tsc` byte-for-byte to the `.js` next to it (tsc 7's output, regenerated with tsc
  under `scripts/limit.sh`, never by hand): comments around erased and replaced statements.
  Whole-program fixtures (layout hints, decorated CommonJS exports, stand-in comments) are
  in `ts_r_emit/tests/tsc_layout/`.
- `cargo run -p ts_r_transform --example transpile -- <file> ['{"module":"commonjs"}'] [--map] [--tsc]`
  (`--tsc`: `Layout::Tsc`).
- `tests/runtime.rs`: each `tests/runtime/<name>.ts` is transpiled for ESNext and for ES2015
  to ES2021 (all lowered, no TSR2003), Node runs every output through `scripts/limit.sh`
  (512 MB, 60 s) and all must print the same. Skipped without `node`.
- Lowering against the baselines: the program emitter oracle
  (`ts_r_check/examples/emit_oracle.rs`) prints an "E5 gates" section.

## Observed CommonJS and module rules (tsc 7.0.2)

- JavaScript files with CommonJS syntax (a `require(...)` call, or an assignment to
  `exports.x`, `exports[k]` or `module.exports`) and no ESM syntax get no `__esModule`
  marker; `import.meta` makes a file a module. `moduleDetection: auto` forces `.mts`,
  `.cts`, `.mjs`, `.cjs` and, under node16+, `type: module` files to be modules.
- JavaScript files keep imports as written (unused ones, `import {}`); `require()` calls in
  them are rewritten under `rewriteRelativeImportExtensions`.
- `import { default as X }` alone uses `__importDefault`; `export import x = require()`
  gets no `void 0` line. `importHelpers` with CommonJS: `const tslib_1 = require("tslib");`
  after the `void 0` lines and hoisted function exports, calls become `tslib_1.__x`.
- A decorated class exported from a CommonJS module (`export class` or `export { C }`):
  `exports.C = C;` right after `let C = class C {}`, and `exports.C = C = __decorate([...],
  C);` (also through the `C_1` alias). `export default` decorated classes assign
  `exports.default = C;` after the decoration.
- Helper order is request order: `__runInitializers` comes before `__esDecorate` when
  instance methods are decorated, or the class is decorated and so are instance fields or
  accessors; otherwise `__esDecorate` first.
- JSX: a spread of an object literal is flattened into the props object unless the literal
  sets `__proto__` (`{ __proto__: x }` or `{ "__proto__": x }`, not computed/shorthand).
  A pragma (`@jsx h`) counts only first on its comment line; `&#X41;` (upper-case `X`) is
  not an entity.
- CommonJS `export = x` becomes `module.exports = x;` at the end of the file, and the
  comments on the `export =` line go. After a top-level `using` it is `_default = x;` in the
  `try` and `module.exports = _default;` after it.
- A module-scope `var` exported by an `export { }` list gets `exports.y = y;` after every
  declaration of it, nested ones included (`{ }`, `if`, `try`, labels: a single statement
  becomes a block); `for (var h = x;;)` moves the declaration and its export before the
  loop, and `for (var n in o)` starts its body with `exports.n = n;`
  (`cjs::nested_var_exports`; `topLevelVarHoistingCommonJS`).
- `verbatimModuleSyntax` keeps `export default I` of a type (an error).
- `importHelpers` in a `.cts`/`.cjs` file under `module: preserve` uses
  `const tslib_1 = require("tslib")` (`TransformOptions::require_helpers`).
- `module: es2015` has no `export * as ns from "m"`: it becomes `import * as ns_1 from "m";
  export { ns_1 as ns };` (a string name gets a temporary, `default` an `export default`).
- A namespace import re-exported from CommonJS (`import * as ns ...; export { ns }`) is
  `exports.ns = ns;`, not a getter.
- `TransformOptions::strip_numeric_separators` is set with no `target` or one below ES2021
  (codegen prints `1_000` as `1000`); the separators are not otherwise lowered.
- `(x as T)` loses its parentheses when the comments in them are inline (`(/* c */ x as T)`
  → `/* c */ x`); a line comment after the `(` keeps them.
- A namespace or enum exported by an `export { }` list assigns every alias in its IIFE
  argument, on each of its declarations and whoever declared the name first:
  `(N || (exports.B = exports.N = N = {}))`, not `exports.N = N;` after it
  (`cjs::list_aliases`; const enums keep the statement after).
- A namespace's IIFE parameter is `N_1` (the next unused `N_k`) when the body declares a
  value named `N` in any nested scope: a `const`, `var`, function, class, parameter, catch
  variable, inner namespace or enum, an `import N =` alias or a `declare const`; types,
  enum members and names merged from another declaration don't count
  (`namespace::param_name`). Otherwise `(function (N) { const N = 0; })` is a SyntaxError.
- An `import` or `export` in a single-statement position (`if (x) export type {};`, a syntax
  error) does not make the file a module: only top-level statements count (`scope.rs`
  `sub_stmt`).
- TC39 decorators on a class that `extends null`: the synthesized constructor has no
  `super(...arguments)`.
- An enum member initializer that names a same-file `const x = <constant>` (no type
  annotation, declared earlier) folds to the value (`A = base * 2` → `8`); a `const`
  declared later stays a reference (`enums::const_value`, `Scopes::const_inits`,
  `isolatedDeclarationErrorsEnums`).
- A namespace is instantiated by an exported `declare function` or a `declare namespace`
  holding a value (`export declare namespace I { const x: number; }`): the empty IIFE is
  kept. Types and type-only nested namespaces still make it a type (`uniqueSymbolsPropertyNames`).
- CommonJS leaves `import.defer(...)` and `import.source(...)` as written; only plain
  `import()` becomes `Promise.resolve().then(() => require())` (`importDeferCallCommonJS`).
- A const enum read through an `import X = M.E` alias asks the program resolver, like an
  imported root.
- CommonJS reads an exported `declare var/let/const` and an exported `import x = ...`
  (either form) through `exports.x` everywhere, assignments and calls included
  (`(0, exports.r)()`); exported `declare` functions, classes, enums and namespaces stay as
  written.
- An exported `import X = N` whose target is only a type (an uninstantiated namespace, an
  interface, a type alias, found through the enclosing namespace bodies) is dropped, with
  no `void 0` entry (`namespace::alias_target_is_value`; `acceptableAlias1`).
- A namespace or enum writes no `var` when an earlier class, function, `var`, `let` or
  `const` of that name (not a `declare var`) or an earlier namespace or enum declared it:
  `class C {} enum C { A }` is `class C {} (function (C) {...})(C || (C = {}));`
  (`namespace::first_declaration`).
- An exported destructuring declaration in a namespace is flattened, one assignment per
  name: `N.a = o.a, _a = o.b, N.b = _a === void 0 ? 1 : _a, _b = k, N.c = o[_b]`. A value
  read more than once is captured in a temporary unless it is an identifier; a non-literal
  computed key is always captured; a defaulted value feeding a nested pattern gets a second
  temporary; array rests are `v.slice(i)`, object rests `__rest(v, ["a", "0"])`
  (`namespace::flatten`; tsc: tsc/internal/transformers/destructuring.go). Computed keys
  before an object rest and empty object patterns keep the destructuring assignment.
- Temporaries skip `_i` and `_n` (`Tx::temp_name`), and each namespace IIFE names its own
  from `_a` again: a namespace frame's temporaries are released when it closes.

## Dependencies

`ts_r_span`, `ts_r_ast`, `ts_r_parser`, `ts_r_diagnostics`, `ts_r_codegen`, `ts_r_config`,
`rustc-hash`; `ts_r_hmr` with feature `hmr`. Dev: `ts_r_testing`.

## Gaps (largest baseline mismatch groups first)

- `using`: the helper order when `__setFunctionName` is requested between the two `using`
  helpers.
- Below ES2022: a private member as a `for...of` / `for...in` destructuring target and
  `#x in o` outside any class are refused (TSR2003); nor is `super` in
  the static code of a class without `extends` lowered (tsc moves it out unchanged, a
  SyntaxError; we keep the class so it runs). A class expression with computed static keys
  names its alias after the key temporaries (`_b = class {}, _a = k()`); tsc names the alias
  first (`_a = class {}, _b = k()`). Helper order
  follows our visit order (the constructor before methods), not always tsc's. `super` inside
  an async arrow outside an async method and computed keys inside a pattern nested under an
  object rest (tsc flattens them too) are not handled. Private methods using `super`, and
  private classes evaluated in a loop with an outer `await` or `yield`, are refused (TSR2003).
  `o?.#x` (an error, TS18030) is refused too; tsc 7.0.2 drops the `?.` there, and panics on
  `o?.#m()` below ES2020. `p?.a.#x` gives `__classPrivateFieldGet(p?.a, ...)` (below ES2020,
  the lowered chain), the same as tsc, which also loses the short circuit past `.a`.
- TC39 decorators: static privates of class-decorated classes
  (`static_privates.rs`) leave `++`/`--`, logical assignments, destructuring targets and
  compound assignments on a non-`this` receiver as written (tsc caches the receiver in a
  function-scoped temporary).
- `module: preserve` adds an `export {}` marker where tsc 7 adds none (tsc never adds it
  under `preserve`; the bundler relies on the marker, so only `.cts`/`.cjs` files and files
  with `export =` or `import x = require()` drop it). Program emit sets
  `TransformOptions::preserve_module_marker = false` and matches tsc (record 0012, E6).
- Cross-file const enums: the bundler does not pass imported enum values in yet.
- An object rest in an exported namespace destructuring at ES2018 and later: tsc 7.0.2
  calls `__rest` without defining it (a ReferenceError); we define the helper.
- Const enums of another file are inlined only through a program resolver
  (`EmitResolver::const_enum_value`, answered by the checker in `ts_r_emit`). Two gaps
  remain: with `preserveConstEnums` an import used only by inlined accesses is kept, and a
  namespace import used only as `ns.E.A` is kept (tsc elides both; the output still runs).
- Synthesized comments other than inlined const enum values (`0 /* E.A */`, via
  `Ast::add_synthetic_comment`) are not produced.
