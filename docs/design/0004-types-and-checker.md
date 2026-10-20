# 0004: Type representation and checker architecture

Status: Accepted 2026-10-18 by Riley Webb
Owners: `ts_r_types` (type store, relation caches), `ts_r_checker` (relations, printing,
checking, inference, narrowing), `ts_r_check` (the `ts-r-check` CLI);
consumers: `ts_r_lint` (type-aware rules), `ts_r_lsp`, `ts_r_sound`, history ([0005](0005-history.md))

## Context

The checker must give the same diagnostics as tsc 7.0.2 (the M5 gate: conformance tests and 60
real repos), run a cold check faster than ts-rust's `--fast` mode, stay at or under tsc 7's peak
memory, and serve check, lint and the editor from **one type graph** per program (plan:
Architecture, "one type graph shared by check, lint and lsp"). It builds on the binder
([0003](0003-binder.md)): per-file `BoundFile`s plus a program link with merged globals and
module exports.

Observed tsc 7 behaviour that constrains the design:

- Type order in printed unions is content-based, not creation-order: probe
  `let v: "b" | "a" | 3 | 1 | boolean = {} as object` prints `'"a" | "b" | 1 | 3 | boolean'`, and
  `Z[] | Y[]` prints `Y[] | Z[]`. So we may intern types in any order, in parallel, as long as
  nothing user-visible depends on `TypeId` numbers.
- tsc 7 checks files in parallel with several checkers, and its output is still deterministic.

## Decision

### Crates

- `ts_r_types`: `TypeId`, `TypeData`, the interned `TypeStore`, type lists, signatures, mappers,
  relation cache storage. No AST walking, no symbol knowledge.
- `ts_r_checker`: program loading (`load_program`: libs, roots, imports), the shared `Checker`
  (link, symbol table, `TypeStore`, per-symbol links), per-thread `Ck` workers, declared and
  inferred types, **relations and error elaboration, the type printer** (both need member
  resolution and symbol names, so they live here, not in `ts_r_types`), inference, narrowing,
  the per-file walk and diagnostics.
- `ts_r_check`: the `ts-r-check` binary (`-p`, `--noEmit`, `--pretty`), tsc's output format and
  exit codes.

### Type representation

```rust
pub struct TypeId(u32);                       // index into the store; never reused
pub struct TypeData { pub flags: TypeFlags, pub kind: TypeKind } // ≤ 24 bytes
pub struct TypeListId(u32);                   // interned, immutable list of TypeIds
```

- **Fixed ids for intrinsics**: `any`, `unknown`, `never`, `void`, `undefined`, `null`,
  `string`, `number`, `bigint`, `boolean` (= `true | false`), `symbol`, `object`, the error type,
  `true`, `false`, the empty object type, and the "silent never"/"wildcard" helpers are
  allocated first in a fixed order, so they are compile-time constants.
- **Structural interning** (hash-consing) for every type whose identity is its content:
  literal types (string literal atoms are interned in the program-wide string interner),
  unions and intersections (keyed by the sorted, de-duplicated member `TypeListId`),
  type references / instantiations (`target`, `TypeListId` of arguments), tuples (element
  types + flags), indexed access, conditional, mapped and template literal instantiations
  (keyed by declaration + mapper), `keyof`, unique symbols (keyed by symbol).
- **Identity by declaration** for types created from declarations: classes, interfaces,
  enums, anonymous object types from type literals and object literals, function types, are keyed
  by `(SymbolRef or (file, NodeId), mapper id)`. Two equal type literals in different places are
  different types, as in TypeScript.
- **Flags** (`u32`): `ANY UNKNOWN STRING NUMBER BIGINT BOOLEAN ENUM STRING_LITERAL ...
  OBJECT UNION INTERSECTION INDEX INDEXED_ACCESS CONDITIONAL SUBSTITUTION TYPE_PARAMETER
  TEMPLATE_LITERAL STRING_MAPPING NON_PRIMITIVE` plus derived summary bits computed once at
  intern time: `CONTAINS_TYPE_PARAMETER`, `CONTAINS_WILDCARD`, `CONTAINS_ANY`,
  `CONTAINS_OBJECT_LITERAL`, `IS_GENERIC_OBJECT`, `IS_GENERIC_INDEX`. Hot predicates are one
  bit test.
- **Union order**: members are stored in a canonical *stable order* (kind rank, then literal
  value / symbol name / declaration position, as observed from tsc 7), computed from content,
  so printing needs no re-sort and results do not depend on which worker interned a type first.
- **Members and signatures** of object types are resolved lazily into side tables
  (`ResolvedMembers { properties: sorted (Name, PropId), call/construct sigs, index infos }`)
  keyed by `TypeId`, written once (`OnceLock`-style slots).
- **Symbols** are the binder's `SymbolRef`s mapped to a program-wide `MergedSymbolId` (u32).
  Per-symbol checker state (`declared_type`, `type_of`, `resolved_alias`, `base_types`,
  `enum_values`) lives in parallel `Vec<OnceLock<..>>` arrays indexed by `MergedSymbolId`.
  Synthetic symbols (properties of mapped/instantiated types) get ids above the binder's range.

### Store and parallelism

- The `TypeStore` is append-only and shared by all workers: data lives in fixed-size chunks
  (`2^12` entries) that never move, indexed by `TypeId`; the intern maps are sharded
  (64 shards, `RwLock<FxHashMap<Key, TypeId>>`, shard chosen by the key's hash). Reads of an
  existing `TypeId` take no lock.
- **Per-file workers.** Program build = parse + bind every file in parallel (rayon), then link
  (sequential, cheap). Check = files partitioned into work items (one file each, ordered by
  size, work-stealing); each worker checks a whole file and appends its diagnostics into a
  per-file slot. Lazily computed symbol types are shared: whoever computes first publishes into
  the `OnceLock`; computations are pure, so a race only wastes work.
- Circularity detection (`push_type_resolution`) is per worker (thread-local stack) and keyed by
  (symbol, property kind); a cycle found by one worker is resolved the same way by any other
  because resolution is deterministic per symbol.
- **Determinism rule**: diagnostics are sorted by (file order, start, code, message) before
  output; no output may depend on `TypeId`/`MergedSymbolId` numeric order or on which worker
  computed something first. A CI test checks with 1 and N threads and diffs.

### Relations

- Relations: identity, subtype, strict subtype, assignable, comparable. A shared cache per
  relation maps `(source: TypeId, target: TypeId)` packed in a `u64` to `Related | Unrelated`
  (sharded, insert-only). `Maybe` results during recursive structural comparison stay in a
  worker-local stack/cache and are only published when the outermost comparison finishes.
- Fast paths before the cache: identical ids, `any`/`unknown`/`never` rules, primitive flag
  masks, literal-to-base, union membership by binary search over the canonical member list.
- Error elaboration (the chain of "Types of property 'x' are incompatible") re-runs the failed
  comparison with reporting on; results are never cached with elaboration state.

### Layering plan (each layer lands with its conformance tests)

1. Declared types of symbols, intrinsic/literal/object types, signatures, structural
   assignability, property access, calls without generics, the diagnostics for those.
2. Narrowing: interprets the binder's flow graph per reference (`typeof`, equality, truthiness,
   `instanceof`, `in`, discriminants, assertion calls, assignments, `never`-returning calls),
   with a per-worker flow-type cache keyed by `(FlowId, reference key)`.
3. Generics: type parameters, instantiation through mappers (a mapper is an interned
   `(sources TypeListId, targets TypeListId)` pair), inference (candidate collection by
   priority, contextual typing, return-type inference, `const` type parameters).
4. `keyof`, indexed access, conditional types (deferred when the check type is generic,
   distributive over unions, `infer` via the binder's `TypeParams` scopes), mapped types
   (homomorphic modifiers, `as` clauses).
5. Template literal types and intrinsic string mappings.
6. JSX, decorators, `using`, JS/JSDoc checking, declaration emit support for `ts_r_decls`.

### Memory target

At or under tsc 7's peak RSS on the same project (measured by `cargo xtask bench`). Budget
guides: `TypeData` ≤ 24 bytes; type lists in one shared `Vec<TypeId>`; no per-type `Vec`
allocation; per-node checker caches (resolved signature, node type) are sparse maps keyed by
`(file, NodeId)`, only for nodes that need them; ASTs of `.d.ts` dependencies may be dropped
after binding and declared types are computed from them, re-parsed on demand (LSP budget mode).

### One graph for check, lint and editor

`Program` exposes a read-only query API used by every tool:

```rust
program.type_of_node(file, node) -> TypeId
program.symbol_at(file, node) -> Option<MergedSymbolId>
program.resolved_signature(file, call) -> Option<SignatureId>
program.is_assignable(a, b) -> bool
program.properties_of(ty) -> &[PropId]; program.apparent_type(ty) -> TypeId
program.type_to_string(ty, enclosing: Option<(FileId, NodeId)>) -> String
program.diagnostics(file) -> &[Diagnostic]
```

- `ts-r-check` builds a `Program`, checks, writes history.
- `ts-r-lint` type-aware rules call the same API on the same `Program` (in `ts-r-lsp`, the very
  same instance; in the CLI, a `Program` built by the same library with checking done lazily per
  query, so no second program).
- `ts-r-lsp` keeps one long-lived `Program` per workspace. An edit replaces the file's AST and
  `BoundFile`, re-links, and bumps a generation counter; per-symbol caches for symbols declared
  in files whose export signatures changed are invalidated (same rules as history), the rest
  stay. The type store is append-only within a generation; when garbage exceeds the memory
  budget, the server rebuilds the store from live files in the background (a fresh `Program`
  sharing the unchanged ASTs/`BoundFile`s by `Arc`).

## Invariants agents must keep

1. Types are `u32` ids into one append-only store; no `Rc`/`Box` type graphs; `TypeData` stays
   ≤ 24 bytes (unit-tested).
2. Structural types are hash-consed: equal content ⇒ equal `TypeId`.
3. Nothing observable depends on `TypeId`/`MergedSymbolId` numeric order or thread scheduling;
   1-thread and N-thread runs produce identical output (CI test).
4. Relation caches store only final results; `Maybe` never escapes a worker.
5. Lint and LSP use the query API; they never build their own type information.
6. Every checker access to another file's declaration goes through the dependency recorder
   required by [0005](0005-history.md).

## Alternatives

- **Separate checkers per thread with private type tables (tsc 7's model).** Simpler, but
  duplicates lib/dependency types per checker (memory ×N) and makes a shared graph for lint and
  the editor impossible. Rejected for the shared store; we keep per-worker scratch state only.
- **Arena of `Rc<Type>`**: pointer chasing, not shareable across threads, no archiving. Rejected.
- **Salsa-style query database for everything.** Considered for the LSP; too much overhead per
  type for a checker that creates millions of types. We use salsa-like invalidation at the
  file/symbol level (0005) and plain caches below it.

## Implementation notes (M5 checker core, 2026-10-15)

Decisions taken while building the first checker; they refine the plan above.

- **Threads.** `check_program` runs at most 4 `std::thread` workers (256 MB stacks), each
  owning one `Ck` that it reuses across files; files are handed out by an atomic counter.
  Per-`Ck` caches (node types, flow types, instantiations, declaration signatures) are
  private; per-symbol results (`SymLinks`) are shared write-once cells.
- **Speculation.** Overload candidates and inference check arguments with
  `speculative > 0`: nothing is cached per node, function expressions get fresh signatures,
  and symbol types that may depend on speculative parameter types (unannotated declarations
  inside function expressions) are not published to `SymLinks`.
- **Reporting.** A node checked quietly (while computing a declaration's type) is checked
  again with reporting when the walk reaches it (`checked` set). Function and class
  expression bodies are deferred and checked after the statement walk.
- **Narrowing** walks the binder's flow graph backwards per reference key (symbol + property
  path); label results are cached per `(file, flow node, reference, declared, initial)`;
  loop labels iterate to a fixed point (at most 6 rounds) with node-type caching disabled
  meanwhile; assertion and `never`-returning calls are recognised only when a callee
  signature has an `asserts` predicate or a declared `never` return.
- **Output** is sorted by (file path, start, length, code, message) and deduplicated; with
  syntax errors only syntax errors are reported (as tsc does).
- **Silent fallback.** Unimplemented constructs produce `TypeId::UNRESOLVED`, an any-like
  type that never produces diagnostics, so gaps cost missing errors, not false ones.

## Implementation notes (M5 declaration checks, 2026-10-15)

- **Declaration checks run once per file after the statement walk** (`decls.rs`): unused
  declarations, class and accessor rules, heritage compatibility. They are syntactic walks
  over the AST and the binder's resolved references plus queries to the type layer; they
  never change how types are computed.
- **Relation budget.** Heritage checks relate whole class and interface types, which on
  deeply recursive generic hierarchies can explode. These checks set a per-worker budget
  on structural comparisons and structural inference steps; a spent budget counts as
  "related", so it can only lose errors, never invent them.
- **Inheritance from intersection bases.** Interface and class members include the
  properties, signatures and index signatures of intersection-typed bases.
- **Printing relative to the error location** (`print_scope`) is used by declaration checks
  only; other diagnostics still print fully qualified names.
- **Redeclarations compare each declaration's own type** (annotation, else widened
  initializer, else `any`) with the first declaration's by identity. Initializer types that
  this layer may approximate (literals narrowed by flow, `any`, type parameters,
  self-references) are not compared.
- **Decorator placement** is checked syntactically. The parser drops decorators written on
  declarations that cannot have them, so such a declaration is recognised by its span
  starting at `@`.
- **Overload sets are found by adjacency** in each statement list and class body. Then the
  implementation is related to each overload: parameters overload-to-implementation
  (bivariant for methods and constructors, and without strictFunctionTypes), and returns
  in either direction. Only the first incompatible overload of a set is reported.
- **Abstract and `override` checks read the resolved base types.** An inherited abstract
  member counts as implemented when any non-abstract member of that name is in the class
  or in another base (a merged interface). `override` and `noImplicitOverride` look names up
  in the base instance type, or the base constructor type for `static` members. Ambient
  classes and `declare` fields are exempt, and so is implementing an abstract member.
- **Initialization order is syntactic.** TS2729 walks from a property access up to the
  property initializer or static block evaluating it, and stops at functions and at method
  keys, which run later. It then compares member positions in the class body, rather than
  running flow analysis over `this` properties. A field without an initializer counts as
  initialized once any earlier static block could have assigned it.
- **Property initialization (TS2564) checks identifier and `#private` names only.** Fields
  whose annotation resolves to an error type (unknown name, wrong type-argument count)
  are skipped, as in tsc.
- **Merged namespaces share exports across files** for unqualified lookups: a name the
  binder could not resolve inside `namespace A` is looked up in the exports of `A`'s other
  declarations before globals.
## Implementation notes (M5 type operators, 2026-10-15)

Layers 4 and 5: mapped, conditional and template literal types (`crates/ts_r_checker/src/`
`mapped.rs`, `conditional.rs`, `template.rs`).

- **Representation.** A mapped type is an object type `ObjectKind::Mapped { decl, mapper }`
  interned by (declaration node, canonical mapper of its outer type parameters, alias); its
  parts (constraint, `as` clause, template, modifiers) are read from the node and
  instantiated on demand. A conditional type has one `ConditionalRoot` per node (check,
  extends, `infer` parameters, outer parameters, distributive flag) and is interned as
  (root, mapper, alias) when deferred. A template literal type is (text literals, span
  types), normalised on creation (literal spans folded, unions distributed).
- **Special mappers.** `MapperId::PERMISSIVE` maps every type parameter to the wildcard and
  `MapperId::RESTRICTIVE` to an unconstrained copy (`TypeStore::restrictive_type_param`). A
  conditional resolves to its false branch when the permissive check type is not assignable
  to the permissive extends type, to its true branch when the restrictive check type is
  assignable to the restrictive extends type, and is deferred otherwise. A string literal that
  does not match a template extends type takes the false branch. Tail-recursive branches loop
  (at most 1000 steps) instead of recursing.
- **Distribution.** Homomorphic mapped types (`{ [K in keyof T]: ... }` with `T` a type
  parameter) and distributive conditionals distribute over a union argument; arrays and
  tuples map element-wise. A distributed conditional's union carries no alias (tsc prints
  `"b" | "c"` for `Exclude<"a" | "b" | "c", "a">`); a single constituent keeps the alias.
- **Members** of a resolved mapped type are synthetic symbols (`SynthSym` with a `mapped`
  origin) whose types are computed lazily. A generic mapped type is opaque, except that a
  homomorphic one reads through the constraint of its type variable
  (`Readonly<X & P>` has `X`'s properties) and `{ [K in C]: X }[C]` relates through `X`.
- **Inference.** Candidates through homomorphic mapped types (`Partial<T>`) and to both
  branches of a conditional target (`Awaited<T>`) are weak: used only when there is no other
  candidate. Keys inferred to a mapped constraint `[K in P]` combine into a union. Reverse
  mapping through other mapped forms is not modelled and infers a silent `UNRESOLVED`.
  Template targets infer span by span (`${infer N extends number}` converts). Once these
  types resolve instead of staying `UNRESOLVED`, real code needs the call's contextual type:
  inferences from it to the return type are the lowest priority (`Output<T>` returns,
  `Promise.resolve` in a typed position), they contextually type arguments whose parameter
  type is a top-level type parameter, and object literal candidates join into one union.
  A union target `U | readonly U[]` sends to the naked `U` only the source members that
  inferred nothing to the other members (`flatMap`).
- **Relations.** A deferred conditional target is related branch by branch (only the
  branches that can be taken; `T extends never ? never : T` is `T`); a source equal to the
  check type, or a conditional with `infer`, stays permissive. Template literal and string
  mapping targets match by pattern; non-generic template sources relate as `string`.
- **Template expressions** get a template literal type in a const context or when the
  contextual type has a string literal, template literal or string-constrained generic
  member; otherwise `string`.
- **Limits.** Instantiation depth and count overflow report TS2589 at the type reference or
  expression being checked (an overflow while resolving an alias body is reported at the
  body); tuples over 10 000 elements report TS2799 (type) or TS2800 (expression); template
  cross products of 100 000 or more members report TS2590 at the template node and become the
  error type. A mapped type over an unresolved operand stays `UNRESOLVED`.

## Implementation notes (M8 check performance, 2026-10-20)

- A `Maybe` result is published before the outermost comparison finishes once it is final:
  a success that assumed only pairs at or above its own frame, or one whose assumed frame
  has since succeeded (it waits on a worker-local pending list until then and is dropped if
  that frame fails). Failures are final at once. Results that met the depth limit, a spent
  budget or a variance measurement in progress are never published; results that met the
  deeply nested instantiation limit are published only by the outermost comparison.
  Invariant 4 holds: what reaches the shared cache is a final answer, the same whichever
  file or thread asked first.
- Successes found while relating to an intersection target's members (excess and weak type
  checks deferred) are cached per worker, apart from the shared cache.
- Unions built without subtype reduction are cached per worker by their written members.
- Module resolution runs on the parsing threads; the serial program walk reads the
  resolver's caches. Only top-level module requests use the result cache: recursive
  `imports` targets have a smaller depth budget, so their results cannot be shared with
  top-level requests or chains at other depths.
- Files much bigger than average are handed to the workers first (largest first), the rest
  in program order; invariant 3 is what makes the order free to choose.

## Status

Extended by [0012](0012-tsc-emit.md) (2026-10-19): the query API gains `type_to_type_node` and an `EmitResolver` for declaration and JS emit.

Proposed 2026-10-14 by the M5 binder agent, as the plan the checker agents build from.
Checker core (layers 1-3 partially, plus the CLI) implemented 2026-10-15; see the
implementation notes and `crates/ts_r_checker/AGENTS.md` for what is covered. Measured
then: 50.5% of 13398 conformance variants match tsc exactly; no false positives on the
private-app apps. Layers 4 and 5 (mapped, conditional and template literal types)
landed the same day: 51.3% exact, 1004 variants with a false positive (from 1062); across
all 25 private-app projects 59 diagnostics are ours only (from 112), none of them new.
Declaration checks (unused, heritage, index signatures, redeclarations, decorator placement,
overload sets, abstract members, `override`, accessor/property overrides, initialization
order, modifier order) merged with them: 58.0% exact, 937 variants with a false positive,
52 private-app diagnostics ours only, none from declaration checks.

Accepted 2026-10-18 by Riley Webb.
