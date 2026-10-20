# 0003: Binder: scopes, symbols, merging and the control-flow graph

Status: Proposed
Owners: `ts_r_binder` (per-file binding and program linking); consumers: `ts_r_decls`,
`ts_r_checker`, `ts_r_lint`, `ts_r_lsp`, `ts_r_transform` (scope analysis only)

## Context

M5 needs a binder that every whole-program tool shares: the checker (name resolution, merging,
narrowing), the linter (scope rules such as no-shadow, no-unused-vars), the LSP (go to
definition, rename, references) and the history store (which symbols each file uses, see
[0005](0005-history.md)). It runs on the index-based AST of [0001-ast.md](0001-ast.md).

Constraints:

- **Per-file and pure.** Binding a file must depend only on its AST and a few options, so it can
  run in parallel and be cached and invalidated per file. Everything cross-file (global merging,
  `declare global`, module augmentation, import/export linking) is a separate, cheap program-level
  pass.
- **tsc 7 parity for bind-time diagnostics.** Codes, positions and the choice of message were
  established by probes run through tsc 7.0.2 (`cargo xtask oracle`), not from tsc source. The
  observations that shaped the rules below are recorded inline.
- **Plain data.** Same rules as the AST: `u32` ids into flat arrays, no pointers, no `HashMap`
  in the finished result (so a `BoundFile` can be archived later), deterministic order.

## Decision

### Inputs and outputs

```rust
pub fn bind(ast: &Ast, src: &str, opts: &BindOptions) -> BoundFile;
pub struct BindOptions {
    pub force_module: bool,                     // moduleDetection=force, .mts/.cts, type:module
    pub always_strict: Option<bool>,            // None = TS 7 default (true)
    pub allow_unreachable_code: Option<bool>,   // None = tsc default (no CLI diagnostic)
    pub allow_unused_labels: Option<bool>,
    pub preserve_const_enums: bool,             // or isolatedModules: const enums are code
}
```

`src` is only used for diagnostic arguments that quote source text (computed keys such as
`["prototype"]`).

`BoundFile` owns: `symbols`, `decl_nodes`, `tables`/`table_entries`, `scopes`, `flow`
(nodes + antecedent lists + node→flow map), `refs` (resolved and unresolved identifier
references), `imports`/`exports` summaries, `diagnostics`, and `extra_names` (strings the
binder had to synthesize). It never mutates the AST.

### Ids and names

| Id | Repr | Meaning |
|---|---|---|
| `SymbolId` | `u32` | Index into `BoundFile::symbols`; allocated in binding order |
| `TableId` | `u32` | A symbol table (locals, exports or members); `TableId::NONE` = 0 |
| `ScopeId` | `u32` | Index into `scopes`; 0 is the file scope |
| `FlowId` | `u32` | Index into `flow.nodes`; 0 is `Unreachable`, 1 the file's `Start` |
| `Name` | `u32` | Below `0x8000_0000`: an AST `Atom` of this file. Above: a reserved internal name (`__call`, `__new`, `__index`, `__constructor`, `export=`, `__export` (export *), `__computed`, `(Missing)` (a name left missing by parser recovery, declared like any other), `__global`, anonymous `__class`/`__function`/`__object`/`__type`) or an entry of `extra_names` |
| `SymbolRef` | `{ file: FileId, symbol: SymbolId }` | Cross-file symbol identity (program level) |

A string is turned into a `Name` by `ast.atoms.find(s)` first, then `extra_names`, so equal
strings always give equal names within a file. Numeric property names are canonicalised to
JavaScript `ToString` form (`0x10` → `"16"`, `1.0` → `"1"`), string-literal property names use
their cooked value, and `"default"` is the ordinary string (so `export default` and
`export { x as default }` meet in one table entry).

### Symbols

```rust
pub struct Symbol {
    pub name: Name,
    pub flags: SymbolFlags,      // u32 bitset, below
    pub decls: (u32, u32),       // range in decl_nodes, in binding order
    pub value_decl: Option<NodeId>,
    pub parent: Option<SymbolId>,// container symbol for members/exports
    pub members: TableId,        // instance members (class, interface, type/object literal)
    pub exports: TableId,        // namespace/enum exports, class statics, module exports
}
```

Flags: `FUNCTION_SCOPED_VARIABLE`, `BLOCK_SCOPED_VARIABLE`, `PROPERTY`, `ENUM_MEMBER`,
`FUNCTION`, `CLASS`, `INTERFACE`, `CONST_ENUM`, `REGULAR_ENUM`, `VALUE_MODULE`,
`NAMESPACE_MODULE`, `TYPE_LITERAL`, `OBJECT_LITERAL`, `METHOD`, `CONSTRUCTOR`, `GET_ACCESSOR`,
`SET_ACCESSOR`, `SIGNATURE`, `TYPE_PARAMETER`, `TYPE_ALIAS`, `ALIAS`, `EXPORT_STAR`,
`OPTIONAL`, `PARAMETER`, `ABSTRACT`, `AMBIENT`, `EXPORTED`, `TYPE_ONLY`, `DEFAULT_EXPORT`,
`ASSIGNED` (written after its declaration; drives closure narrowing),
`CONST_ENUM_ONLY_MODULE`. Meaning groups `VALUE`, `TYPE`, `NAMESPACE` are unions of these.

A namespace is *instantiated* (`VALUE_MODULE`) if it contains any value declaration, an
instantiated nested namespace, a `const enum` (probe: `namespace X { export const enum E {} }`
conflicts with `let X`) or an `import =` alias; otherwise only `NAMESPACE_MODULE`. A local
`export { a as b }` (no `from`) also instantiates it when some specifier's local name is a value
declared in the block or at the file's top level, even through `export type { a }` (probe:
`declare const a: number; declare namespace N { export type { a } }` makes `typeof N` legal,
while a namespace that only re-exports a type alias, with or without `type`, gives TS2708).

### Merging and conflicts

Each declaration kind has *excludes*; a new declaration conflicts when the existing symbol's
flags intersect its excludes. The table was measured with a 16×16 probe (every pair, both
orders, module scope):

| Declaration | Conflicts with |
|---|---|
| `var` | `let`/`const`, function, class, enum, const enum, instantiated namespace |
| `let`/`const` | var, let/const, function, class, enum, const enum, instantiated namespace |
| function (any) | var, let/const, enum, const enum |
| class (any) | var, let/const, class, type alias, enum, const enum |
| interface | type alias, enum, const enum |
| type alias | class, interface, type alias, enum, const enum |
| enum | everything except enum and namespaces |
| const enum | everything except const enum and non-instantiated namespaces |
| instantiated namespace | var, let/const, const enum |
| non-instantiated namespace | nothing |
| alias (`import`, `export {}` specifier) | alias |
| `export default <expr>` / `export = <expr>` | everything bound *before* it (one-way: a later class or interface default decides by its own excludes, so `export default foo; export default class C {}` is the checker's TS2323) |
| parameter, type parameter | same name in the same list |
| property / method / accessor | property ↔ method ↔ accessor, except one get + one set; method ↔ method overloads merge |
| interface property signature | repeated within one interface body or type literal only; the same property in another declaration of a merged interface merges (the checker compares types) |

Rules, all observed:

1. **Function declarations bind first.** In each statement list, `FunctionDecl`s are declared
   before the other statements (probe: `let y; function y(){}` reports TS2300, not TS2451).
2. **A conflicting declaration is detached.** It gets a fresh symbol that is not entered in the
   table, so later declarations are compared with the original symbol only (probe:
   `let y; class y {}; function y(){}` reports TS2300 on `let` and `function`, nothing from the
   binder on `class`).
3. **Message choice:** TS2567 if either side is an enum; else TS2528 if the name is `default`
   in a module's exports; else TS2451 if the existing symbol is block-scoped; else TS2300.
   The error is reported on every declaration of the symbol and on the new one (deduplicated by
   span and code), at the declaration's name; anonymous `export default function/class` at the
   declaration; `export default <expr>`/`export = <expr>` at the expression when it is an
   identifier, otherwise at the statement.
4. **Exported vs local declarations of one body are one-way.** Exports go in the container's
   `exports`, the rest in the body's locals. An exported declaration conflicts with a local of
   the *same body* bound before it (by the export's excludes); a local bound after an export
   never conflicts (the checker reports TS2395). Probes: `let x; export var x` → TS2451 on
   both; `export var x; let x` → nothing from the binder.
5. **Member tables.** Each class *declaration* gets its own member table (two `class C {}` in
   different merges never share members); the declarations of one interface symbol share one
   table. A static method named `prototype` (also `"prototype"`, `["prototype"]`) is TS2300
   at the key with the key's source text; static properties and accessors are not.
6. **Type parameters** of a function live in the function's scope together with its top-level
   locals (excludes: `TYPE` minus type parameters), so `function f<T>() { type T = 1 }` is
   TS2300 on both and a repeated type parameter is TS2300 on the repeat only.
7. **`default`:** a conflict involving a declaration with the `default` modifier is only
   reported in a module's exports table (in a namespace it is the checker's TS2652).
   `export default` with a missing expression reports TS2528 at the end of `default`.
8. Checker-only conflicts are *not* binder errors: duplicate implementations (TS2393), class and
   function (TS2813/2814), import/local conflicts (TS2440), exported-variable redeclaration
   (TS2323), namespace placement (TS2433/2434), export/local mismatch (TS2395).

Exported declarations in a module or namespace go in the container's `exports` table; their
non-exported counterparts in `locals`. Ambient bodies (a declaration-file module, an ambient
module, a `declare namespace`) export every declaration without `export` too, unless they hold
an export clause (`export {}`, `export … from`, `export *`) or an export assignment
(`export =`, `export default <expr>`); `export default class`/`function` and `export as
namespace` keep the implicit exports, and `import x = …` is never implicitly exported
(observed: `type A = 1; export class B {}` in `a.d.ts` lets `import { A } from "./a"` resolve;
add `export {}` and it is TS2459). A file is an external module when it has a top-level
`import`/`export` (including `export {}` and `import x = require()`), or `force_module`.
`declare global { }` binds into the file's `global_augmentations` table; `declare module "x"`
in a script is an ambient module (`ambient_modules`); in a module it is an augmentation
(`module_augmentations`, resolved at program level). An augmentation of a name the target
re-exports (`export { Foo } from "./b"`, `export *`) merges with the re-exported declaration,
not the alias (observed: members from both are visible, no TS2300); a conflict found that way
is reported at the augmentation and at the named re-export (or at the declaration for
`export *`).

### Scopes

`Scope { kind, node, parent, locals: TableId, container_symbol }` with kinds `File`,
`Module` (namespace body), `Function` (also arrows, methods, accessors, constructors, signatures
and function types for their parameters), `Class` (class name in class expressions),
`Block`, `Catch`, `For` (loop head `let`/`const`), `Switch` (the case block),
`TypeParams` (type parameter lists, `infer` in conditional types, mapped type keys) and
`StaticBlock`. Catch clause bindings are block-scoped variables in the `Catch` scope (so
`catch (e) { let e }` conflicts). `var` and function declarations hoist to the nearest function, module or file
scope (functions in blocks stay block-scoped). `let`/`const`/`class`/`enum`/`using` bind in
the innermost block scope; each such symbol records the span start of its declaration so the
checker can report TDZ uses (TS2448/2449/2450) without re-walking. `scope_of_node` is a sorted
`(NodeId, ScopeId)` array for scope-creating nodes; arbitrary nodes find their scope through
their nearest scope-creating ancestor.

### References

During the walk the binder records every identifier reference (expression identifiers,
shorthand properties, type references' leftmost identifier, `typeof` queries, heritage
expressions, export specifiers without `from`) with its scope and meaning (value, type,
namespace). After binding, a local pass resolves each reference through the scope chain
(a `Module` scope also searches its merged namespace symbol's exports; class members are never
in lexical scope). Inside `declare global { }`, a name the block declares with any meaning stops
the walk and leaves the reference free, so it resolves to the merged global (observed with tsc
7: a `var Foo` in the block makes `Foo` in a type position the global interface, not the
module's local `Foo`). A reference in a parameter list, or in a function nested in one, does
not see the function's body locals except hoisted functions and namespaces (tsc 7.0.2:
`function f(a = x) { var x }` reads the outer `x`; a body function is found and the checker
reports TS2373). Results: `refs.resolved` (sorted `(NodeId, SymbolId)`) and `refs.free`
(names not declared in the file: globals, or errors for the checker). The free list and the
import aliases that resolved references hit are the file's *used-symbol* seeds for history.
Assignments to resolved symbols set `ASSIGNED`.

### Control-flow graph

```rust
pub struct FlowNode { pub kind: FlowKind, pub flags: u8, pub node: Option<NodeId>, pub a: u32, pub b: u32 }
```

16 bytes. Kinds: `Unreachable`, `Start` (`node` = function or none; `a` = flow at the
function expression's creation, for closures), `Assignment` (`node` = target or declarator,
`a` = antecedent), `TrueCondition`/`FalseCondition` (`node` = condition expression),
`BranchLabel`/`LoopLabel` (`a..b` = range in `flow.lists`, the antecedents), `SwitchClause`
(`node` = switch, `b` = clause index range packed as two `u16`), `Call` (call statements that
may be assertion or `never`-returning calls), `ArrayMutation` (`x.push(...)`, `x[i] = v`),
`ReduceLabel` (finally: `a` = target label, `b` = antecedent list), `Narrowed`-free: the
checker interprets conditions; the binder only records shape.

Built like tsc's model as observed in narrowing behaviour: conditions on `&&`, `||`, `??`,
`!`, `?:`, optional chains and logical assignments split into true/false branches; loops get
`LoopLabel`s with back edges; inside an optional chain (`a?.b.c(x)`) each `?.` continues
under a nullish-flagged `TrueCondition` on its object (so `x` sees `a` non-nullish) and the
chain's end joins the matching `FalseCondition`s; `break`/`continue`/labels target the right label; `return`/`throw`
make the rest unreachable; `try` routes every point of the `try` block into `catch` and
`finally`, and `finally` continues through a `ReduceLabel` to each pending jump target.
`flow.node_flow` (sorted `(NodeId, FlowId)`) records the flow at: identifiers, `this`, `super`,
property/element accesses in expression position, `return`/`throw`/call statements, and each
function's end (`FunctionFlow { start, end, return_label }`). Statements found unreachable are
collected (`unreachable_ranges`) and reported as TS7027 only when `allowUnreachableCode` is
`false`; unused labels likewise give TS7028 only for `allowUnusedLabels: false`.

Observed details that matter for TS7027:

- Immediately invoked function and arrow expressions (not async, not generators) are bound
  inline: their body continues the caller's flow, so `(() => { return })(); x` is reachable.
- Default values (parameters, binding elements, destructuring defaults) are conditional: the
  flow after them joins the flow before.
- Namespace bodies do not change the enclosing flow (a `return`-less namespace never makes
  following code unreachable).
- Only the literals `true`/`false` fold as conditions (`while (true)`, `if (false)`); nothing
  folds under `!` (`while (!false)` is not an infinite loop for reachability).
- Unreachable `var` declarations without initializers, function declarations, interfaces,
  type aliases, non-instantiated namespaces and (unless `preserveConstEnums`/`isolatedModules`)
  const enums are not reported.
- Never-returning calls, assertion functions and exhaustive `switch`es need types: the
  checker reports those, not the binder.

### Bind-time diagnostics

| Code | When |
|---|---|
| 2300 / 2451 / 2567 / 2528 | Conflicts, see Merging |
| 1100 / 1210 / 1215 | `eval`/`arguments` declared or assigned in strict code (module / class / other) |
| 1212 / 1213 / 1214 | Strict-mode reserved word (`implements interface let package private protected public static yield`) used as an identifier (not property names) |
| 1101 | `with` statement (all code is strict in TS 7) |
| 1102 | `delete` of an identifier |
| 1344 | Label on any declaration statement (`var`, `let`, `class`, `interface`, `type`, `enum`, `namespace`, function, `declare ...`); in a label chain only the innermost |
| 1114 | Duplicate label in a label chain |
| 1117 / 1119 | Duplicate object literal property / property and accessor |
| 2397 | Script-level declaration named `globalThis` (not inside `declare global`), or a script-level variable, function or namespace named `undefined` (program level) |
| 2435 | `declare module "x"` directly in a namespace body, or nested in a module augmentation; nested in a script's ambient module it is accepted; in a block it is the checker's TS1234 |
| 2669 | `declare global` anywhere but the top level of a module or directly in an ambient module |
| 2664 | Top-level module augmentation whose module cannot be found, in a non-declaration file (program level). Nested augmentations are not resolved |
| 7027 / 7028 | Unreachable code / unused label, only when the option is `false` |

Code is strict in modules, under `alwaysStrict` (default on in TS 7; probe: script files get
TS1100/TS1212), under a `"use strict"` prologue and in class bodies, names and `extends`
clauses (those use the class messages 1210/1213). Reserved-word and `eval`/`arguments` checks
are skipped in ambient contexts (`declare`, declaration files, ambient namespaces). JS files
also count CommonJS indicators (`require("x")` at top level, `exports.x =`, `module.exports`)
and any file using `import.meta` is a module. Driver rule, not binder: in JS files
`checkJs: false` drops binder diagnostics and plain JS (no `checkJs`) drops TS2300. Diagnostics use tsc's message
text from the synced catalog; tests compare against `tsc/testdata/baselines` (see the crate's
AGENTS.md for the harness and current match counts).

### Program-level linking

`Program::link(files: Vec<BoundFile>, resolver)` produces:

- `globals`: a merged global table `Name string → MergedSymbol { flags, decls: Vec<SymbolRef> }`
  built from every script file's top-level declarations and every `declare global` block, in
  file order (lib files first). Cross-file conflicts follow the same excludes/messages and
  are reported on every declaration (with `file` set).
- `module_links`: per file, per import/export specifier, the resolved `FileId` (through
  `ts_r_resolver`), or an ambient module (`declare module "x"`, also `"*.css"` patterns), or
  unresolved. Ambient module and augmentation declarations of the same name merge. A relative
  specifier inside an ambient module declaration stays unresolved (observed: TS2439, and
  `export * from "./a"` there re-exports nothing). In a module augmentation (`declare module
  "x"` or `declare global` in a module), a relative specifier of an import or export statement
  resolves only when the file also imports the same specifier outside augmentations
  (observed: TS2307 otherwise; `import("./a")` types there resolve as usual).
- `resolve_export(file, name) -> Option<SymbolRef>` following aliases, `export *` (with cycle
  detection and the rule that local exports shadow star exports) and `export =`.
- `export as namespace X` adds a UMD global that behaves like a value+namespace module whose
  exports are the file's exports; it merges with and conflicts like a namespace. It is
  dropped silently when a script file earlier in program order (lib files first) already
  declares a global `X` (observed: `export as namespace Promise` is no error), while
  `declare global` blocks merge after every UMD name and still conflict with it.

Cross-file rules (observed): the message is TS2567 if an enum is involved, else TS2451 if
*either* side is block-scoped, else TS2300 (within a file only the existing side decides).
Merging recurses into namespace/class/enum exports and interface members (depth-limited).
In module augmentations an alias conflicts with any declaration, and augmentations reach
declarations re-exported through `export *`. Declaration files tolerate unresolved
augmentations (no TS2664).

### Caching and invalidation

`BoundFile` is a pure function of (AST, `BindOptions`, binder version). Its cache key is
`(BINDER_VERSION, options bits, file content hash)`; it is kept in memory next to the AST and
may later be archived with it through `ts_r_cache`. When a file changes it is re-parsed and
re-bound alone. The program link is recomputed incrementally: a changed *module* only re-links
its own specifiers and the files whose links pointed at it; a changed *script* or a file whose
`declare global`/ambient-module summary hash changed re-merges the globals (a sorted name list,
cheap). The summary hash covers the names and flags of global declarations, so editing a body in
a script does not re-merge.

## Invariants agents must keep

1. Binding never panics and never mutates the AST; on deep input it stops descending (stack
   budget) instead of overflowing.
2. Every id stored in a `BoundFile` indexes that file's arrays; no pointers, no `HashMap` in the
   finished result; all lists are in binding order or sorted by id.
3. `bind(ast, opts)` is deterministic: identical inputs give byte-identical dumps.
4. The excludes table and message rules above are the only source of bind-time conflicts;
   changing them needs an oracle probe and an update to this record.
5. Binder diagnostics are a subset of tsc 7's for the same input (measured by the conformance
   harness; extra diagnostics are bugs).
6. Cross-file information never enters `BoundFile`; it lives in the program link.

## Alternatives

- **Symbols as `Rc<RefCell<Symbol>>` with tables per node (tsc's shape).** Rejected: pointer
  graphs cannot be archived or shared between processes, and cost more memory.
- **One global symbol arena for the whole program.** Rejected: it serialises binding and
  makes per-file invalidation expensive; `SymbolRef` + a merged-symbol map keeps files
  independent.
- **Per-node scope array (`Vec<ScopeId>` parallel to nodes).** Rejected for now: 4 bytes per node
  for a lookup the checker needs rarely; references carry their scope instead.
- **Resolving names lazily in the checker only.** Rejected: bind-time local resolution is cheap,
  gives the linter and LSP scope answers without a checker, and yields the used-symbol sets
  that history needs.

## Status

Accepted 2026-10-14 by the M5 binder agent. Conflict rules measured on tsc 7.0.2; the
conformance harness and its current counts are in `crates/ts_r_binder/AGENTS.md`.
