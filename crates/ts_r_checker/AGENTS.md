# ts_r_checker

The type checker: program loading (lib files, roots, imports), the shared `Checker`, per-thread
`Ck` workers, declared and inferred types, relations with error elaboration, the type printer,
calls/overloads/inference, control-flow narrowing, the per-file walk, comment directives and
the parallel driver. Root rules: [../../AGENTS.md](../../AGENTS.md). Design record:
[../../docs/design/0004-types-and-checker.md](../../docs/design/0004-types-and-checker.md)
(read its "Implementation notes" before changing caching or threading).

## API in one breath

```rust
let libs = LibStore::locate()?;                         // $TS_R_LIB_DIR, exe dir, target/upstream
let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), threads);
// project references (E3): imports of a referenced project's sources load its outputs;
// Resolution::reference_source names the source, TS6305 when that output is not built
let program = load_program_with_references(&fs, &roots, &options, Some(&libs), threads, refs);
// ... and every package.json resolution looked for (the buildinfo records them)
let (program, package_jsons) = program::load_program_tracking_package_jsons(/* same */);
// many overlapping programs in one process (ts_r_guard): parse each file once per contents
let cache = SourceCache::new();
let program = load_program_cached(&fs, &roots, &options, None, 1, &cache); // cache.parses()
let result = check_program(&program, CheckRun { threads: 4 }); // sorted, deduplicated
result.diagnostics; result.error_count();
// lower level: let checker = Checker::new(&program); let mut ck = Ck::new(&checker, file);
// ck.check_source_file(file); ck.check_expr(loc); ck.is_assignable(s, t); ck.type_to_string(t)
// editor queries (ts_r_lsp): ck.type_to_string_at(t, scope) (`m1.m2.c`, qualified from
// scope), ck.jsx_props_type(opening), ck.pattern_parent_type(pattern)
```

Files, by layer:
- Setup: `program.rs` (files, libs, `load_program`), `checker.rs` (`Checker` shared state,
  `Ck` worker state and caches), `symbols.rs` (symbol table over the binder's link, merged
  symbols, `SymLinks`; augmentations of a module whose `export =` is a namespace, class,
  function or enum merge with that entity, not the module), `nodes.rs` (AST helpers), `diag.rs` (diagnostics and message chains),
  `diagnostics_generated.rs` (message catalog, generated; do not edit), `emit.rs`
  (`Checker::alias_is_value`, the answer behind `ts_r_emit::CheckerResolver`: does an import
  alias name a value; `Some(false)` for an evaluable const enum target without
  `preserveConstEnums`, `None` when its values cannot be evaluated).
- Types of declarations: `declared.rs` (classes, interfaces, aliases, enums, type nodes),
  `symtype.rs` (type of a symbol), `members.rs` (member resolution, signatures, bases),
  `instantiate.rs` (mappers, the TS2589/TS2799/TS2800 overflow reporting), `unions.rs`
  (union/intersection construction, widening).
- Type operators: `mapped.rs` (mapped types: instantiation, homomorphic distribution,
  members, `keyof`, inference to mapped types), `conditional.rs` (conditional types:
  resolution, distribution, `infer`, tail recursion, relation to conditional targets),
  `template.rs` (template literal types, intrinsic string mappings, pattern matching and
  inference). Design: the "M5 type operators" notes in design record 0004.
- Relations: `relate.rs` (identity/subtype/assignable/comparable, caches, excess properties,
  elaboration chains), `print.rs` (type printer), `typenode.rs` (`type_to_type_node`: a type
  as type-node AST for declaration emit, record 0012; it mirrors `print.rs`'s order and
  forms, never truncates, and returns `NotNameable::Unresolved` for `UNRESOLVED` rather than
  printing `any`; cycles print `any` as tsc does, depth beyond 64 is `TooDeep`).
  `typenode/specifier.rs` writes `import("...")` specifiers: package names are only
  candidates until the `ModuleSpecifierHost` (the program's resolver, passed in by the
  emitter) resolves them back to the same file, so `exports`, `types` and symlinks are
  honoured without a second implementation of them. With a host, a module only reachable
  through a `node_modules` path that does not resolve back (an `exports` map blocks it, a
  nested `node_modules`) is `NotNameable::NotPortable` (TS2883), never a deep import; an
  anonymous class type with private or protected members is `PrivateMember` (TS4094, every
  member), and with `TypeNodeCx::report_unnameable` a recursive type whose only name is
  another module's unexported alias is `CannotBeNamed` (TS4023), as is a unique-symbol
  property key another module declares that the file cannot name (named by the first name
  of its chain: `Foo` for `Foo.sym`); a mapped type's property
  keyed by a unique symbol is `NonSerializableProperty` (TS4118).
  `typenode/reuse.rs` reuses nodes of the enclosing file where tsc writes the source's
  syntax (`expr_type_to_type_node` for an initializer or returned expression; annotations of
  parameters, returns, properties and accessors when they write the same `TypeId` and
  their names resolve the same at the enclosing declaration); object literal keys written
  from the literal's syntax are recorded in `TypeNodeCx::origins` for the declaration map.
  Another file's annotation (and its literal property values) is copied into the output
  tree (`copy_foreign`): literals keep their text, names are rewritten through `entity_of`
  (an `import("m").X` too), nothing is mapped; anything it cannot copy (`this`, template
  literal types, computed keys) is written from the type. A reused node that declares a type
  parameter the written type renames (`infer T` written `T_1`) is not reused.
  Naming rules observed in tsc 7 (record 0012, E6 notes): a symbol reachable through a
  top-level namespace import of the file prints `ns.Name`; a local declaration another
  module exports by an export declaration is named by that export (`import("./c").default`,
  `import("./foo")` for `export =`, `y.Member` through a namespace import of the module);
  `import("…").Name` goes through a module the file imports that exports the symbol
  (explicitly or by `export *`) when that specifier ranks before the declaring module's
  (package before relative, then fewer path segments), else through a program module that
  re-exports the symbol when its relative specifier has fewer path segments
  (`CachedTypes::reexporters`, built once); a relative specifier keeps the `.js` or `.ts`
  ending (`Ending::Ts`) the file's own relative imports use (a `.ts` specifier of a `.d.ts`
  file does not count). A value property of a variable's type literal is named through the
  variable (`typeof Op.or`).
  E2 leftovers (`m8/dts-leftovers`, observed in tsc 7.0.2): a member of a module's
  `export =` namespace is `import("m").B`, also in the module's own file; a package's
  `exports` condition branch (`default` or an active condition) names its file; a chunk
  re-exported by a same-package entry is named through it (`shorter_reexport`: package
  before relative, then fewer segments; any re-exporter when the declaring module has no
  specifier); an instantiation of an anonymous type nested in 11 instantiations of the
  same declaration is `/*elided*/ any` (`MAX_INSTANTIATION_NESTING`; the declared type
  does not count; instantiated object literals carry `ObjectFlags::INSTANTIATED` for
  this); a signature written from a type keeps type parameter defaults, and a `?`
  parameter written from its type says `| undefined`; a type written past
  `MAX_SERIALIZED_LENGTH` (approximate, see its doc) is `NotNameable::TooLong` (TS7056).
  Known difference: `typeof k` of `const k = 1` keeps the fresh literal here (tsc's is
  regular); narrowing by `case k:` against such discriminants compares `TypeId`s, so
  regularizing it needs the narrowing to compare regular literals first.
- Program loading (`program.rs`): a module found through `node_modules` is `external`
  (a library: not emitted, no rootDir rule), also for a linked workspace package whose real
  path is outside `node_modules`; that package's JavaScript is still loaded under `allowJs`.
  JavaScript whose resolved path is under `node_modules` is loaded only within
  `maxNodeModuleJsDepth` (default 0) imports of other files, also through a relative
  import (`../../node_modules/eslint/lib/linter`); past it the import is untyped (TS7016).
- Expressions: `expr.rs`, `func.rs` (function expressions, contextual typing), `call.rs`
  (calls, `new`, overload resolution, arity and TS2769 reporting), `infer.rs`, JSX:
  `jsx_check.rs` (the `JSX` namespace of an element: global, factory-scoped or from the
  `jsxImportSource` runtime; intrinsic and value tags, function and class components,
  `ElementAttributesProperty`/`ElementChildrenAttribute`/`LibraryManagedAttributes`/
  `IntrinsicAttributes`/`IntrinsicClassAttributes`, overloads with TS2769, generic
  components, factory diagnostics TS2874/TS2875/TS2879/TS17016/TS17017 (an `@jsx` pragma
  with fragments needs `@jsxFrag`, whose root must be in scope), TS2604/TS2607/TS2786
  (intrinsic tags against `JSX.ElementType`; overloads by their intersected returns; a tag
  resolves against construct signatures before call signatures, and a union tag without
  either combines each member's like a union's call signatures: props are the first
  parameter, the return is checked against `Element | null | ElementClass`, and a member
  without any is TS2604; tsc: `getUninstantiatedJsxSignaturesOfType` in `jsx.go`; the
  tag is a narrowable reference, `<o.C />` included, so `if (C) <C />` reads `C` narrowed; a
  fragment under `jsx: react` with default factories checks `{ children }` against
  `React.Fragment`'s props at `<>`, tsc: `getJSXFragmentType`),
  TS2608 (a props or children name container with several properties, once at its first
  declaration; it then counts as missing; tsc: `getNameFromJsxElementAttributesContainer`
  in `jsx.go`), TS2609 (a spread child that is not an array; only `any` is exempt), TS2639 (a grammar
  error: not in files with syntax errors), TS6229 (a function component's least minimum
  argument count over its call signatures exceeds the largest parameter count of a
  function-component first parameter of the classic factory's overloads; a rest
  parameter or no such overload means no check, nor does the automatic runtime; the
  attributes are then not checked; tsc: `checkApplicableSignatureForJsxOpeningLikeElement`
  in `jsx.go`), TS7026; the
  observed rules are in its module doc), `jsx.rs` (the attributes type,
  attribute and child contextual types with discriminated union props, spreads (TS2698,
  TS2783; a lone spread of a type parameter is the attributes type; with a spread plus a
  written attribute or child the result is fresh and only the written ones are checked for
  excess, tsc: `createJsxAttributesTypeFromAttributesProperty` in `jsx.go`), elaboration of
  attributes and children TS2745/TS2746/TS2747, several children checked against the
  element types of the children type's array members and the iterated types of its other
  iterable members (tsc: `elaborateJsxComponents` in `jsx.go`); hyphenated attributes the target lacks,
  written as attributes or as keys of an object literal spread straight into them, are
  ignored by excess, weak-type and index-signature checks in `relate.rs`; with both
  `IntrinsicAttributes` and `IntrinsicClassAttributes` declared, a non-excess failure
  against the props intersection is reported against the first member it fails, with no
  head and no excess check (tsc: `reportErrorResults` in `relater.go`)), `jsx_runtime.rs` (which files import a
  JSX runtime: `react-jsx`/`react-jsxdev`, `@jsxRuntime automatic`, or any `@jsxImportSource`
  pragma, even under `preserve`; `@jsxRuntime classic` turns it off; pragmas match without
  case; `program.rs` loads it like an import). `unused.rs` counts no JSX factory as used
  under the automatic runtime, an `@jsx` pragma's root in place of the options', and an
  `@jsxFrag` root only in files with fragments.
- Narrowing: `flow.rs` (backward flow walk, typeof/truthiness/equality/instanceof/`in`/
  discriminant/type-guard/asserts narrowing, `switch (true)`, definite assignment).
  `typeof x === "string"` turns an object member the primitive (or `Function`) is
  assignable to into it (`{ toString(): string }` is `string`); intersections with a
  primitive are that primitive (`string & { __brand }`). `x === v` with `x` `unknown` or
  holding `{}` (`!!u`) is `v`'s type for a primitive or `object` `v`, `object` for another
  object type. `x == v` also keeps the `string`/`number`/boolean-literal members of `x` that
  coerce to a `string`, `number` or `boolean` `v` (tsc: `isCoercibleUnderDoubleEquals` in
  `tsc/internal/checker/flow.go`). A reference read in unreachable code (after a `never`-returning call, also
  from a closure created there) has its declared type, not `never`. `instanceof` keeps members that
  derive from the instance type (classes nominally, other objects by assignability, arrays
  and tuples for `Array`), else uses instance types that are subtypes of members, else the
  instance type or an intersection; `prototype` (not `any`) wins over construct
  signatures, and a union of constructors does not narrow the false branch.
  `x.constructor === C` (true branch only) narrows only when `C` has call or construct
  signatures and a `prototype` whose type is not `any`, `Object` or `Function`; it reads
  the candidate from `prototype` (an `any` reference becomes the candidate), keeps class
  instances of the candidate's own class (not bases, not same-shaped classes) when either
  side is a class, and otherwise members (primitives through their apparent type) that are
  subtypes of the candidate. A comparison with `Object`, with an `any`-typed `prototype`
  (`y.constructor`, typed `Function`) or with a bare construct-signature type leaves `x` as
  it is (tsc: flow.go, narrowTypeByConstructor).
  `typeof x` in a type is narrowed where it stands (the binder records the flow at the
  identifier). A loose comparison (`u == 42`, `u == E.A`) never narrows `unknown`, only
  `== null`/`== undefined` do; `u?.p` and `u!` read `unknown` as `{}`.
  `p?.isBlock()` truthy makes `p` non-nullish and still applies `isBlock`'s `this is T`
  predicate. A condition on `(x = f()).p` or `(x = f())?.p` tests `x.p` (discriminants
  narrow through the assignment); truthiness of `(x = f())?.p` rules out a nullish `x`,
  equality and `typeof` tests of it do not (observed).
  A call ends the flow (or asserts) by its single non-generic signature without resolving
  the call: resolving checks the arguments, whose flow types may be mid-walk then (an
  exhaustive `default: assertNever(x)` in a loop read `x` as `any`).
  The implicit `default` of a switch without one is walked first; whether the switch is
  exhaustive is asked only when that path adds a new non-`never` type, and a nested query
  while it is being computed settles it as not exhaustive (the outer result is dropped), so
  a loop's partial type does not make it exhaustive for good (`switch_exhaustive_loop_bypass`;
  tsc: checker/flow.go getTypeAtFlowBranchLabel, isExhaustiveSwitchStatement).
  An `asserts c` call (no type) whose argument is a constant false condition (`false`,
  `(false)`, `false && x`) ends the flow like a `never` call; `!true`, `0`, `null`, a
  `const` holding `false` and `asserts c is T` do not (tsc 7, observed).
  `(f(), x).p` is the reference `x.p`; `this` declared by a `this` parameter narrows.
  `if (x = y)` narrows `y` as the condition `y`, and `if (x &&= y)` when true;
  `isString(x?.y)` (a predicate ruling out `undefined` in the branch) makes `x` non-nullish;
  so do the clauses of `switch (x?.y)` / `switch (typeof x?.y)` that are all `case`s whose
  value can't be `undefined` (`"undefined"`), and the body of `for (k in x?.y)`
  (`switch_optional_chain_nonnull`; tsc: checker/flow.go, switch clause narrowing);
  `any` stays `any` under a predicate or assertion whose type is exactly the global
  `Object` or `Function`, as with `instanceof` (`any_predicate_object_function`);
  `"p" in x` leaves `any` alone and narrows `unknown` to `Record<"p", unknown>` when true.
  An intersection property is optional only when every *member* constituent (property,
  method, accessor) is: a global `var` of `typeof globalThis` does not vote, and as a target
  it does not reject an optional source (TS2327 needs a member target). The right operand
  of `in` may be a primitive (TS2638) when it is `{}` or an intersection whose base
  constraint is `{}` (`T & {}`, not `T & {}` with `T extends object`). For `instanceof`, an
  intersection with a generic member derives from the instance type only when one member
  does (a generic one through its constraint; `{}` never derives from `Object`), so
  `T & {}` by `Object` intersects to `T & Object` (`in_keyword_intersection_optional`;
  tsc: checker.go createUnionOrIntersectionProperty, hasEmptyObjectIntersection;
  relater.go propertyRelatedTo, isTypeDerivedFrom);
  a function value with no construct signatures (`Function`, a plain or JS function)
  has instance type `{}` for `instanceof`: the true branch keeps the object members
  (`any` becomes `{}`), the false branch is not narrowed (`instanceof_function_value`;
  tsc: flow.go narrowTypeByInstanceof, getInstanceType);
  a predicate or assertion on a discriminant (`assertEqual(x.kind, "a")`, `` x[`kind`] ``)
  narrows `x` by it. `typeof x === "Object"` (not a `typeof` result) keeps the object
  members when true (`unknown` gives `object`) and the others when false.
  Definite assignment (TS2454) follows the flow graph for every `var`/`let`/`const`
  declarator, initialized or destructured too (`v; let v = 0;`, a read after a `try`
  that assigns), except `x!: T`, reads in the variable's own unannotated initializer,
  reads in parameter initializers and names reached through an import. `++`/`--` and
  arithmetic compound assignments are transparent to flow (the reference keeps its
  narrowed type; an unassigned variable stays unassigned) and their result must fit the
  left side's narrowed read type with literals widened (a setter's own type for
  accessors). The left side of `&&=`/`||=`/`??=` is read without TS2454. The `Start` of
  an immediately invoked function continues into the flow before the call, and is no flow
  container of its own (a read in it is not an outer read). Decorators of members and
  parameters have no flow position (no TS2454). A closure reading an outer local `let`
  without initializer reports TS2454 unless some assignment to it anywhere is definite
  (`=`, `&&=`/`||=`/`??=`, destructuring); `x++` and `x |= 1` alone don't count
  (`closure_compound_assignment_uninitialized`; tsc: checker/flow.go, the assignment
  marking walk).
  `++`/`--` and compound assignment: an unassignable target (`++E`, `f += 1`) reports only
  TS2628-TS2632/TS2588; a non-number operand reports TS2356 and not TS2357. `x!` is the
  target `x` (constants included); `(x as T)`, `(x satisfies T)` and `<T>x` are valid
  targets without the constant check.
  While a loop's type is being computed, a condition narrowing it to `never` gives
  `TypeId::SILENT_NEVER`: property accesses and calls on it are silent and give it back, an
  assignment of it adds nothing to the loop (`x = x.slice()` in the branch not reached yet).
  Tuples (`members.rs`): the array members see the tuple as `this` (`sort(): this`). A
  union's combined call signature intersects a rest parameter's type at each argument
  position (tuple elements by position, array element types otherwise) with the other
  members' parameters, and keeps a type every member agrees on.
  Inference (`call.rs`): a context-sensitive function argument fixes only the type
  parameters its unannotated parameters' contextual types mention. An arrow whose
  expression body is a context-sensitive function is context sensitive itself, and fixes
  what the returned function's contextual parameters mention (`() => a => a.foo` against
  `() => (a: T) => void` fixes `T`, widened: tsc `hasContextSensitiveReturnExpression`).
  A computed entity-name key in a signature's binding pattern prints as written
  (`({ [foo.bar]: c }: {}) => any`). A type parameter source
  (a class's `this`) infers to an object target through its constraint (`infer.rs`).
  A conditional argument infers from its context-free arms in the first pass, and arms
  that are calls see the earlier arguments' inferences. A context-sensitive object
  literal infers in the first pass from its context-free properties (nested literals
  recurse) to the parameter's object members' property types, never to a naked type
  parameter, so `{ initialValues: 12, nextValues: v => v }` gives `V = number` before `v`
  is typed rather than `V`'s default (tsc checks such an argument with its sensitive
  functions skipped, and the result is non-inferrable;
  `context_free_object_properties_infer`). Before a context-sensitive function argument
  is typed, its annotated non-rest parameters (`?` adds `undefined` under
  strictNullChecks) and its annotated return type (not a predicate) infer to the
  contextual signature's types at the same positions, so `(t1: D, t2) => ..` against
  `(t: T, t1: T) => void` gives `t2: D` (tsc: `inferFromAnnotatedParametersAndReturn`
  in checker.go; `annotated_params_infer_first`). With a generic return context, a
  context-sensitive argument fixes to the context's inferences that don't mention the
  outer call's open type parameters. A conditional type's `infer` takes an empty array
  literal's element type (the implicit `never`); a call's type parameters take it as a
  plain `never` candidate (`from([])` is `Box<never>`). A literal candidate is widened
  only when no candidate came from inside another type (type argument, property, index
  signature, signature): `both<T>(a: T[], b: T)` with `([], 1)` infers `1`. In an expression,
  a generic tuple read by a numeric literal past its required prefix, at a single
  variadic `...T`, is `T[number]` (`expr.rs`). A type predicate keeps a member only
  assignable to its type when neither is a subtype of the other (`flow.rs`). A generic
  callee's inferred return type is resolved for return-context inference when it is not
  being resolved already (`inference_context_for`). A call met again while it is still
  being resolved, through a variable's type or a function's return type (a callback
  argument calls the variable the call initializes: `const [s, set] = useState(() =>
  ({ f: a => set(a) }))`), has an error result there with no cycle recorded, so the
  callbacks keep their return types and the outer argument is still checked; met again
  only through loop flow it takes its first signature (`resolve_call`; tsc:
  `checker.go` resolving signature).
  Names (`symbols.rs`): inside `declare global`, a name the block declares is the merged
  global only when that has the meaning asked for (a global `var x` does not hide the
  file's `import * as x` from `x.T`); a namespace lookup the binder resolved to a value
  looks further out.
  Mixins (`members.rs`): a class whose base constructor is a type parameter (or an
  intersection holding one) has the value type `typeof C & T`. Construct signatures of an
  intersection: a mixin member (one `new (...args: any[]) => X`, no type parameters) gives
  no signature of its own; its instance type intersects, in member order, into the return
  of every other member's signatures (`typeof A & Mixin` news `A & X`). Base statics and
  class derivation (protected access) walk every class member of an intersection base.
  `flow/evolving.rs`: evolving arrays (`let xs = []` under noImplicitAny in TypeScript files:
  each read is the array of what `push`/`unshift`/`xs[n] = v` added before it; nothing
  pushed is `any[]` with TS7005/TS7034; reads from nested functions are `any[]` too).
  `let x = null` / `= undefined` is auto-typed like `let x;`, and a closure reading an
  auto variable assigned after it reports TS7005/TS7034 (`evolving_arrays`). An auto
  variable read before any assignment reaches it is `undefined` (not `any`), and an
  assignment inside a nested function makes closure reads TS7005 with TS7034 at the
  declaration (`auto_variable_undefined`, `auto_variable_closures`). An auto variable with
  an `x = []` assignment reads through the same walk (start and declaration: `undefined`
  under strictNullChecks) when some path reaching the read evolves; an operation target
  (`x.push`) is silent only when every path evolves, else it is the joined type
  (`number[] | boolean[]` pushes `never`). The array of `push`/`unshift` and of operation
  targets is the reference its operand stands for: `(x = [], x).push(1)`
  (`evolving_auto_variable`; tsc: checker/flow.go, getReferenceRoot,
  isEvolvingArrayOperationTarget). A loop label being iterated is shared through
  `Ck::ev_pending`: a read of the same variable re-entered while checking a value assigned
  in the loop (`o = yield* o`) sees the antecedents joined so far, and when that is still an
  implicit `any[]` the read reports TS7005/TS7034 once the loop is done (the nested check is
  quiet). A value assigned to an evolving array (declared `any[]`) that is not assignable to
  `any[]` reads as `any[]` (`evolving_array_loop_reentry`; tsc: checker/flow.go,
  getTypeAtFlowLoopLabel, getTypeAtFlowAssignment).
  Destructuring: a const binding (or an unassigned binding of a parameter pattern) is a
  `BindingAlias` for `ref.prop` (`const { kind } = v`, `const k = v.kind`); a condition on it
  narrows `ref` when `prop` is a discriminant, and a sibling binding reads its property of the
  narrowed parent (a pattern without a reference is keyed by its first binding's symbol).
  A parent narrowed to `never` (the default clause after every `kind` case) makes the
  sibling `never` (`destructured_sibling_never`; tsc: checker.go getNarrowedTypeOfSymbol).
  An aliased condition (`const ok = isSuccess && x; if (ok)`) narrows a constant reference;
  an unassigned binding of a parameter or catch variable pattern is one, like the
  parameter itself (`aliased_condition_param_pattern`; tsc: checker/flow.go,
  isConstantReference).
  Type arguments get the same treatment for constraint checks (TS2344): inside
  `T extends X ? <here> : ...` the argument is instantiated with `T := X & T` first
  (`narrowed_in_true_branch` in `check.rs`). A generic check type that is not a type
  parameter (`T[K] extends X ? F<T[K]>`) makes the argument `X & T[K]`, and so does an
  intersection argument with it as a member (`F<T[K] & {}>`, `F<{} & T[K]>`); the walk then
  goes on outwards, so an outer `K extends keyof T ?` still maps `K`
  (`conditional_narrowed_intersection_argument`; seen on @babel/traverse; tsc:
  checker.go getConditionalFlowTypeOfType).
- Syntax-only expression checks: `shapes.rs` (TS2695 comma left sides without side effects,
  TS2872/TS2873 conditions always truthy/falsy, TS2869/TS2871 `??` left sides never/always
  nullish; observed rules in the module doc).
- Iterables: two checks, each with one set of callers. `iterable.rs` `check_iterable_value`
  covers array binding patterns (nested ones too) and spreads (`spread_element_type`; a
  call's spread of `never` is reported, an array literal's is not). `check.rs`
  `check_iterable` covers for-of operands, `yield*` operands (TS2504 when async) and
  destructuring assignment patterns, the latter through `check_destructuring_assignment_from`.
  Don't call both for one value. A generic source is judged by its constraint without
  `null`/`undefined`, since references narrowed by `typeof` keep their generic type here.
- Initializer reads: `param_refs.rs` (called for every resolved identifier read: TS2372/
  TS2373 for parameter defaults reading themselves, later parameters, or body locals when
  the parameter list is lowered; TS2301 for instance property initializers reading
  constructor locals, also in place of TS2304; TS2304 for a parameter-list read that only
  resolves to a body local, `check_body_local_from_parameter`, also for `typeof x` in
  parameter annotations). Under strictNullChecks a TS file's parameter that reads itself
  in its own default, and whose annotation (or `?`) admits `undefined`, is also TS2502 at
  its name: removing the optionality from its declared type asks whether the initializer
  can be `undefined`, which needs the parameter's type again
  (`self_referencing_default_annotation`, `param_default_self_reference_annotation`; tsc:
  checker.go removeOptionalityFromDeclaredType). A type name in a function's signature that resolves to a type
  declared in its body is TS2304 (`type_from_signature_to_body`), and so is an `infer`
  type parameter named inside its own `extends` clause (`infer_in_own_extends` in
  `static_refs.rs`); both are checked in `resolve_type_name`. In `symbols.rs`
  `resolve_ident`, exports of a namespace merged with an enum are out of scope inside the
  enum's body (`namespace_export_in_enum`). TS2371 (initializers in signatures without
  a body, binding-element defaults too) is in the modifier walk in `check.rs`.
- Qualified type names (`resolve_type_name`): an export found on the left namespace that
  has no type meaning gives TS2749 (`'A.B' ... Did you mean 'typeof A.B'?`) when it is a
  value and TS2694 on the right name otherwise (an uninstantiated namespace used as a
  type), and resolves to nothing.
- Assignment target shapes (`assign_targets.rs`): TS2364/TS2779 for `=` and compound
  assignments, TS2701/TS2778 for object rest targets, TS2406/2487/2780/2781 for
  `for...in`/`for...of` heads, TS2777 for `++`/`--` (in `check_reference_target`). An
  arithmetic compound assignment checks its operand types first and reports the shape only
  when they pass (TS2362/2363/2365 suppress it, TS18050 does not); a `for...in` head only
  when TS2405 did not fire.
- Index signature key types that are not keywords, template literals or `unique symbol`
  (those are the parser's) are checked in `index_keys.rs`: TS1337 for a generic key or one
  with a string/number/enum literal member, TS1268 for anything else that is not
  string/number/symbol/template/string-mapping (or an intersection with one).
- TS2480 (`let_names.rs`): a binding named `let` anywhere in a `let`/`const`/`using`
  declaration's names. It is a TS2xxx grammar error, so the driver's TS1xxx filter for
  files with syntax errors misses it; the check asks `has_syntax_errors` itself. Use that
  helper for any other TS2xxx grammar error tsc drops in broken files.
- `await` and `yield` inside a parameter of their nearest function are TS2524/TS2523
  (`in_parameter_initializer_of_container`); nested functions and classes start a new
  scope, and parameter decorators are not part of the parameter.
- Object literal duplicates the binder cannot see (computed keys named by their literal or
  unique symbol type, names repeated across a spread) are TS1117/1118/1119 from
  `object_dups.rs`, which skips the pairs the binder already reported.
- Read-only writes (TS2540, `is_readonly_write`): read-only symbols (readonly properties
  and parameter properties, getters without setters, enum members, namespace `const`s) and
  every member of a namespace import (`import * as ns`; not `import ns = require()` or a
  copy). Writes are `=`/compound targets, `for...in`/`for...of` heads and `++`/`--`
  operands (`check_readonly_update_target`). A constructor may write its class's own
  read-only properties through `this`, also inside an immediately invoked arrow, not in
  other arrows or functions. A class's synthesized `prototype` is writable (tsc 7). A
  late-bound getter (`get [k]()`) with a sibling `set [k]` is writable, with the setter's
  parameter type (`late_bound_setter`; the members table keeps only the getter).
- TS2507 (`check_base_constructor_type`): the first `extends` expression of a class must
  be `any`, `null`, a type with construct signatures, a type parameter constrained to one,
  or a union of those. Other instantiable types are accepted. JavaScript classes are
  checked too, and a JavaScript function (`@constructor` or not) is not a constructor in
  tsc 7. A parameter's JSDoc `{T=}` prints as `T | undefined` (tsc:
  tsc/internal/checker/nodecopy.go; `is_jsdoc_optional_annotation`).
- TS2774 (`uncalled.rs`): identifiers and property chains in a tested position (`if` or
  `?:` test, left of `&&`, through `&&`/`||`/`??`) whose type is a callable, never-nullish
  function, unless the same reference appears in its region (rules at the top of the
  file). Only under `strictNullChecks`; JavaScript files are left alone. In the same
  tested positions, `E.A` on an expression whose symbol is an enum (an `import X = E`
  alias does not count) with an enum-literal type reports TS2845 "always 'true'/'false'"
  by the member's value instead (`enum_member_condition`; tsc: checker/checker.go,
  checkTestingKnownTruthyType).
- TS2395 (`export_merge.rs`): the binder keeps exported and local declarations of one
  name in different tables, so the checker compares them per container by meaning
  (rules at the top of the file). TS2652 lives there too: a named `export default`
  declaration and the declarations of its name in the same body that share a meaning with
  it (a namespace means a value only when its own body is instantiated); those skip TS2395.
  Both run in JavaScript files as well. An import's meaning follows its target through a
  module's `export =` (a named import the `export =` value's type answers with a property
  takes that property's meanings), and a target declared only by property or method
  signatures is a type, not a value (tsc: checker/checker.go, getDeclarationSpaces).
- A value read of a top-level, non-exported import that shares its name with an exported
  variable of the same file resolves to that variable (`import_merged_export_value` in
  `symbols.rs`; tsc: getExportSymbolOfValueSymbolIfExported), so `export const Obj = Obj`
  next to `import Obj` is TS2448, not a read of the import. Only variables: functions keep
  the import. The other way round, an alias read as a value whose target is a local type
  with an exported value of its name in the same block (`interface F {}; export const F
  = ...; export default F`) is a silent error type (`local_merged_export_value`): tsc's
  merged local symbol has no value type, so neither TS2693 nor later errors.
- `void e` checks its operand later, with the deferred nodes (tsc: checkVoidExpression ->
  checkNodeDeferred), so `export const x = void x` is TS2448 without a circular TS7022.
- TS2323 (`export_redeclare.rs`): a module file's export symbol that merged two or more
  counted declarations (not interfaces, not overload signatures; never namespaces or
  enums) is reported at each declaration. Only the file's own export table is checked,
  not `declare module "x"` bodies. Two default-exported function implementations are also
  TS2393 there, unless their local names clash (then `overloads.rs` reports them).
- TS2374 (`check_duplicate_index_signatures` in `index_keys.rs`): instance index
  signatures are gathered from every member table of the owner (a class and a merged
  interface have separate tables), and each key type member declared twice is reported on
  each signature that declares it.
- TS2481 (`check_var_shadowing_block_scoped` in `let_names.rs`): a `var` name found first
  as a `let`/`const` in a block, `for` or `switch` scope between the declaration and the
  scope it hoists to. It walks the binder's scopes directly.
- TS2502 (`circular_annotations.rs`): `push_resolution` records, per symbol, the first
  resolution cycle through its `Resolving::Type` entry (`type_cycles`). An annotated
  declaration whose symbol's type was circular is reported unless that cycle went through an
  inferred return type or a type alias whose body has an array, tuple or reference with
  type arguments (tsc resolves those lazily; we resolve them eagerly). Only the value
  declaration (or the type-determining accessor) is reported. TS7022 (unannotated
  variables, `noImplicitAny`) uses the same records but only for cycles made of
  `Resolving::Type` steps alone, outside loops, with no function in the initializer: our
  eager function and loop-flow checking makes cycles tsc does not have. The one exception
  is a cycle of variable types and plain class methods' inferred return types (`next() {
  return v; }` with `for (var v of new It())`): tsc finds it too, and reports the variable
  (TS7022) and each method at its name (TS7023, `report_circular_return` reading the
  method symbol's own signature; `is_class_method_sig`). A cycle through a method with no
  variable step is still not reported.
- TS7039 (`noImplicitAny`): a mapped type without a template (`{ [P in "bar"] }`), at the
  mapped type (tsc: checker.go, checkMappedType, reportImplicitAny).
- TS7023 for getters (`report_circular_getter` in `check.rs`, `noImplicitAny`): a getter
  without a return type annotation whose property type (inferred from its body, no setter
  annotation) is circular is reported at its name, once per getter. Only cycles made of
  return types and accessor or class field types count: one through a variable, parameter,
  JavaScript `this.x =` property or node (an object literal's own type, built eagerly
  here) is one tsc doesn't have. Any recorded cycle that contains the getter's symbol or
  signature may qualify (`getter_circular_return`; tsc: checker.go getTypeOfAccessors).
- TS2729 (`init_order.rs`, rules at the top of the file) also covers static
  initializers reading later declarations of other kinds (enum members, object literal
  properties, namespace exports, other classes' static fields) and field decorators; the
  JSX closing tag replay drops it. TS2715 (`abstract_access.rs`): `this.p` and
  destructurings of `this` reaching an abstract property during construction. TS2612
  (`check_base_property_overwrite`): an uninitialized field redeclaring a base field under
  `useDefineForClassFields`, unless the constructor assigns it. TS2816 (`check_this`):
  `this` in a static field value (arrows included) of a class with legacy
  (`experimentalDecorators`) class decorators.
- **Widening implicit-any errors** (`widening_errors.rs`; only under noImplicitAny without
  strictNullChecks, TypeScript files). A declaration whose unannotated initializer type still
  holds a widening `null`/`undefined` reports once it widens to `any`: TS7005 for variables,
  TS7031 for binding elements (walked over the unwidened initializer, element by element),
  TS7006 for parameters with an initializer, TS7008 for class properties. Before that, the
  type is searched: a union with an empty-object member stays silent, arrays and tuples look
  at their elements, and an object literal's own `null`/`undefined` property reports TS7018
  at that property instead (`{ x: null }` is TS7018 on `x`, not TS7005). Functions with no
  return annotation check their returns (and yields) the same way: TS7010/TS7055 at the name
  (a function's, or a method's/getter's key), TS7011/TS7025 at a nameless function, skipped
  when a contextual signature gives a non-generic return or yield type
  (`widening_implicit_any`; tsc: checker.go, reportErrorsFromWidening and
  reportWideningErrorsInType). A spread result belongs to the object literal that made it
  (`spread_owner`, what tsc gives it as its symbol), carries the widening trait of its
  property types, and an optional right property merged into a widening left one keeps the
  left's declaration, so `{ p: null, ...f() }` reports TS7018 at `p`. Spreading an optional
  property drops its `undefined` without strict null checks too (tsc: checker.go,
  getSpreadType, removeMissingOrUndefinedType).
- Implicitly `any` interface and type literal members (`implicit_members.rs`): TS7008 for
  property signatures, TS7032/TS7033 for accessor signatures with no annotation in the
  pair; a getter types its setter's parameter (no TS7006). Files with syntax errors are
  skipped for TS7008 (our parser recovers members tsc does not). In classes
  (`check_setters_without_getters`), a set accessor whose computed name is no literal or
  unique symbol is an accessor of its own: TS7032 names it by the key's source text
  (`[k]`) and no getter types it (tsc: tsc/internal/checker/checker.go, getTypeOfAccessors).
- TS2310 (`check_circular_interface_bases`): every interface declaration with an
  `extends` clause whose symbol's bases lead back to it (`has_circular_bases`). Classes
  get TS2506 instead. A class `extends` naming an interface with no value
  (`extends_interface_only`) is TS2689 and its expression is not checked. Only the first
  type of the first `extends` clause is checked; the rest of `extends A, B` and a second
  `extends` clause are grammar errors only (tsc: checker.go, checkClassLikeDeclaration
  through GetExtendsHeritageClauseElement). An element whose expression the parser
  recovered as missing is not counted: `class C extends await<string>` in a module checks
  `string` (TS2863), as tsc's list holds no such element. An `implements`
  or interface `extends` element that is not an identifier or a dotted chain of them (a
  call, an optional chain `a?.B`) is TS2500 / TS2499 at the expression, besides the
  reference's own errors (`check_heritage_entity_name`; tsc: checker/checker.go).
- Namespace parts of entity names (`resolve_namespace_name`, helpers in
  `namespace_names.rs`) are looked up with namespace meaning only. A type, class or value
  there is TS2713/TS2702/TS2503/TS2749, decided by the rules at the top of that file. A
  missing member is TS2694, or TS2724 with a suggestion among exports of the needed meaning;
  a missing name is TS2503, or TS2833 with a namespace suggestion. TS2694 names an aliased
  namespace by its symbol; a namespace exported from a module file is led by the quoted file
  path, as tsc qualifies it up to the file (tsc: `checker.go`, getFullyQualifiedName;
  `namespace_in_module_named_by_file`). `import x = A.B` entities are checked the same way
  (`check_import_equals_entity`). JavaScript files and symbols are left alone.
- Use before declaration (`check_use_before_declaration` in `expr.rs`, TS2448/2449/2450):
  a block-scoped variable read in its own initializer is reported unless deferred; IIFEs,
  computed names of methods and instance fields, and member decorators are not deferring;
  a static block or static field initializer may read its own class; an enum reading
  itself and ambient classes are never reported. Deferred bodies are drained again after
  the declaration checks (decorator expressions queue some).
- Computed member names: key types are checked for class members, interface and type
  literal members as for object literals (TS2464, `check_computed_key_type`; `[P in K]`
  property keys are left to TS7061); `this`/`super` in computed names (TS2465/2466,
  `in_computed_name` in `expr.rs`, observed rules in its doc).
- Predefined type names: `decl_names.rs` (classes, interfaces, type aliases, enums and
  type parameters named `any`, `string`, ...: TS2414/2427/2457/2431/2368).
- Merged type parameters: `type_param_merge.rs` (class and interface declarations of one
  symbol: TS2428; observed rules in the module doc). The same file checks every type
  parameter list's defaults, JSDoc `@template [T=X]` included: a required parameter after
  an optional one is TS2706 at the parameter; a default whose type reference resolves to
  its own parameter or a later one of the list is TS2744 at the reference (resolved, so a
  shadowing parameter of a nested signature is fine; tsc: tsc/internal/checker/checker.go,
  checkTypeParameters). When missing type arguments are filled with defaults
  (`fill_type_args`), the parameters not filled yet map to the error type, so such a
  forward reference is a silent error type in the filled list (`f14<A>(a, b)` against
  `<T, U = V, V = C>` takes any `b`; tsc: fillMissingTypeArguments;
  `forward_default_error_type`). A default must be assignable to its parameter's
  constraint instantiated with the parameter as the default (TS2344 at the default:
  `<T, U extends T = number>`). A default whose resolution needs itself (`interface
  I<T = I> {}`: the reference fills `T` from its own default) is circular: it reads as no
  default and is TS2716 at the default. A default naming a non-generic type alias that is
  being resolved, directly or as an array element (`T = ParsedNode` in `interface Seq<T =
  ParsedNode>` with `type ParsedNode = Scalar | Seq`), is no cycle: the alias reads as a
  forward reference there, because tsc defers a reference inside an alias that needs its
  defaults (tsc: isDeferredTypeReferenceNode; `default_names_resolving_alias`; yaml,
  typebox). Type argument counts ask whether a default is
  declared (`has_type_param_default`) without resolving it, so they never start that
  cycle (tsc: checker.go, checkTypeParameter, getResolvedTypeParameterDefault;
  `type_param_default_checks`).
- Merge placement: `merge_order.rs` (namespaces before or in another file than the class
  or function they merge with: TS2434/2433, checked for each part of a dotted
  `namespace A.B.C` at that part's name, and a local namespace against an exported class
  or function of its name in the same block; enum declarations starting without an
  initializer: TS2432).
- Import conflicts: `import_conflicts.rs` (an import whose target shares a meaning with a
  local declaration of its name in the same block: TS2440; an `export { x }` specifier
  merged with an exported declaration sharing a meaning with its target: TS2484 at the
  specifier; observed rules in the module doc).
  An alias merged with a local value (variable, function, class, enum or instantiated
  namespace) is typed as that local value, whether or not the alias target has a value
  meaning too (tsc: `getTypeOfSymbol` checks value flags before the alias flag).
- For-in heads: `check_for_in_head` in `check.rs` (a single declarator: pattern TS2491,
  type annotation TS2404; an expression head: TS2491 for literal patterns, TS2405 when
  the string keys of the object are not assignable to it, skipped when writing the head
  already reported, e.g. TS2540 for read-only members).
- `yield`: TS1163 in `in_generator` (`expr.rs`; decorators, `extends` expressions and
  class member computed names see the generator around the class; an enum member
  initializer is no generator body, `yield_in_enum_initializer`); TS2523 and TS7057 in
  `yield_any.rs` (its own container walk: classes are transparent, a method's computed
  name belongs to the method; observed rules in the module doc).
- `super`: `super_calls.rs` (placement TS2335/2336/2337/2660 from `check_super`; derived
  constructors without `super()` TS2377; `this`/`super.x` before `super()` TS17009/17011,
  a backwards walk over the flow graph that stops at `super(...)` call statements;
  observed rules in the module doc).
- Static members: `static_refs.rs` (class type parameters referenced from static members:
  TS2302, from `check_type_reference`).
- Local exports and augmentations: `export_locals.rs` (`export { x }` of a non-local or
  unknown name: TS2661/TS2304, also in a script's `declare global`, where only the block's
  own declarations are local. A name whose first declaration is not a top-level statement
  of a script or lib file is local too, so `declare module "m" { export { Buffer } }` may
  export a global that a `global { }` block inside another `declare module "m"` declares
  (@types/node; tsc: `checkExportSpecifier` in tsc/internal/checker/checker.go only
  rejects `undefined`, `globalThis` and names whose declaration container is a global
  source file); imports and exports in `declare global` or a resolved module
  augmentation: TS2667/TS2666; `global { }` without `declare`: TS2670). Unlinked relative
  specifiers in augmentations are TS2307 in `check_module_requests`.
- Reference directives: `references.rs` (`/// <reference path>` naming no program file:
  TS6053/6054/6231/6504, or the file itself: TS1006). Program diagnostics: the driver
  adds them with the syntax diagnostics, so they survive parse errors. "Not found" means
  no candidate path is a program file (the loader loads every reference it finds).
  The shared `reference_diagnostics` takes one source file and the full program's paths,
  so history can report and refresh them without parsing every file.
  `/// <reference lib="x" />` naming no lib is TS2727 with the closest lib name
  (`relate::spelling_suggestion` over `ts_r_config::enums::LIBS`), else TS2726.
  A leading `/// <reference ... />` is read as tsc's parser reads it: after `///`, blanks
  and the tag `<reference` (letters and `-` only, any case), a run of `name="value"`
  arguments that stops at `/>` or at anything else. The last argument of a name wins, and
  the directive is the first of `no-default-lib="true"`, `types`, `lib` and `path` it has;
  with none of them it is TS1084 at the whole comment (`invalid_reference_diagnostics`).
  TS1084 is a parse error in tsc, so the driver counts the file as having syntax errors
  (tsc: `extractPragmas` and `processPragmasIntoFields` in tsc/internal/parser/parser.go).
- Missing global types: `driver::missing_global_type_diagnostics` (TS2318 for each of the
  ten core global types no file declares, observed with `noLib`; `CallableFunction` and
  `NewableFunction` only under `strictBindCallApply`). Global diagnostics; the
  driver adds them, and ts-r-check also reports them next to option errors.
  Global types tsc looks up only when a construct needs them report TS2318 then, once per
  name (`Ck::global_type_reported` / `report_missing_global_type`, global diagnostics):
  `Disposable` / `AsyncDisposable` for `using` / `await using`, `TypedPropertyDescriptor`
  for a method or accessor decorator call, `IterableIterator` / `AsyncIterableIterator`
  for an unannotated generator when `Generator` / `AsyncGenerator` is missing, and
  `Iterable` (ES2015+) for an empty or rest-only array binding pattern whose initializer
  is an array literal (only that asks for the pattern's contextual type).
  (`tsc: tsc/internal/checker/checker.go`, `getTypeFromArrayBindingPattern`,
  `createGeneratorType`, `getGlobalTypeChecked` callers.)
- Walk and driver: `check.rs` (statements, classes, type references, modifiers, imports,
  `@ts-ignore`/`@ts-expect-error`, matched as prefixes: `// @ts-ignore-line` is one; they
  cover a checked JavaScript file's JSDoc syntax errors too, which tsc 7 reports with the
  semantic errors: `tsc: tsc/internal/compiler/program.go`; they also hide the program's
  link (merge) errors on their line, such as a duplicate `declare global` member, and such a
  hit counts as the directive's use, so no TS2578: `driver::link_diagnostic_reported`,
  `check::suppressed_by_directive`; a lib file's half of a clash with a project
  declaration, TS2300 `Duplicate identifier 'bold'` in `lib.es2015.core.d.ts`, is reported
  as tsc 7's CLI does unless `skipLibCheck` or `skipDefaultLibCheck`:
  `driver::lib_link_diagnostics_reported`; the conformance example does not compare them,
  since the baselines keep only some, as `lib.es5.d.ts(--,--)`),
  `driver.rs` (threads, ordering).
- A property missing on a global lib type that a later lib adds is TS2550 naming that lib
  (`lib_hint.rs`: `[].at` without es2022). The table holds the container/member pairs
  observed hinted by probing every member the bundled lib files add after es5; tuples,
  unions, intersections, subclasses, `ReadonlyArray` and element accesses get TS2339.
- A property missing on a class instance (or `this`) is TS2576 only when the class's
  constructor type has it and its declaration carries `static`: a merged namespace's export
  and the synthetic `prototype` stay TS2339. The suggestion is the printed receiver type and
  the name (`'C<number, string>.foo'`, `'List[k]'` for element access)
  (`Ck::class_has_static_member`; tsc: `typeHasStaticProperty` in
  tsc/internal/checker/checker.go; `calls_members.rs::static_member_through_an_instance`).
- Explicit type arguments are counted before value arguments. A candidate must take both
  counts. When none does: if no signature takes that many type arguments, the error is about
  them (one signature: TS2558 with its `min-max` range; overloads: the largest count below
  and the smallest above, TS2743 when both exist, else TS2558 with the one that does);
  otherwise the argument-count error (TS2554/2575) is computed over only the signatures
  that take the type arguments. An empty `<>` counts as none (TS1099 alone). `new f()` of a
  value with call signatures only is resolved as a call of them (argument and type-argument
  errors as usual) and is `any`; without noImplicitAny a declared signature that does not
  return `void` is TS2350, a `this: void` one TS2679 (`resolve_call_inner`,
  `report_type_arg_arity_error`; tsc: `reportCallResolutionErrors`,
  `getTypeArgumentArityError` and `resolveNewExpression` in tsc/internal/checker/checker.go;
  `calls_members.rs::type_argument_count_is_checked_before_argument_count`).
- Argument-count errors over overloads: TS2575 when the count lies strictly between the
  fewest required and the most accepted arguments, naming the largest required count below
  it and the smallest accepted count above it (0 is a real count: `f()` next to `f(a, b)`);
  otherwise TS2554 (TS2555 when any signature has a rest parameter), with the missing
  parameter as related information only when there are too few. An array literal spread
  straight into call or `new` arguments (parentheses allowed) is a tuple, so its elements
  are counted as arguments (`is_spread_into_call`; tsc: `getArgumentArityError`,
  `isSpreadIntoCallOrNew` in tsc/internal/checker/checker.go;
  `calls_members.rs::argument_count_errors_over_overloads`).
- A missing key on a fresh object literal reads as `undefined` with TS2339 only for a plain
  read; a written access (`=`, compound or logical assignment, `++`/`--`) or a callee indexes
  the widened literal and takes the implicit-any path. On that path, after the static (TS2576),
  number-index (TS7015) and spelling (TS2551) cases, TS7052 suggests calling `get` (or `set`
  for any write) when the object has that member as a plain one-signature function taking at
  least one argument whose first parameter accepts the key; the hint is prefixed by the
  receiver when it is a name or property path (`m.prop.get`). A union key is reported through
  its first member that names no property (the head still prints the whole key); an enum
  member key is shown as `[E.a]` (`index_method_suggestion`, `access_path_text`; tsc:
  `getPropertyTypeForIndexType`, `getSuggestionForNonexistentIndexSignature` in
  tsc/internal/checker/checker.go, `tryGetPropertyAccessOrIdentifierToString` in
  tsc/internal/checker/utilities.go; `calls_members.rs::index_signature_method_hint`).
- A catch variable is `unknown` or `any` (`useUnknownInCatchVariables`), or its annotation
  when that is `any`/`unknown`; any other annotation is TS1196 and the variable is an error
  type. A destructured catch variable is checked like a variable's pattern (TS2339 per
  missing key, `{}` for `unknown` without strict null checks; TS2488 for array patterns).
  A `let`/`const`/`using` directly in the catch block named like the catch variable is
  TS2492 at its first such binding, for every name of a destructured catch variable and
  of a destructured declaration too (`catch ([a, b]) { const [c, b] = ... }`;
  `catch_pattern_redeclaration`). Calling an `unknown` value is TS18046 (`catch_variable_type`,
  `check_catch_redeclaration`; tsc: `getTypeForVariableLikeDeclaration`, `checkCatchClause`
  in tsc/internal/checker/checker.go; `calls_members.rs::destructured_catch_variables`).
- A type parameter constrained by `any` (`T extends any`, `T extends Tree<any>` that
  reduces to `any`) is constrained by `unknown`, a mapped type's key by `string | number |
  symbol`; an unresolved constraint stays an error type. A declared `unknown` constraint is
  named in TS5075 ("assignable to the constraint ... 'unknown'"); no constraint gives TS5082
  (`type_param_constraint`; tsc: `getConstraintFromTypeParameter` and
  `reportRelationError` in tsc/internal/checker/checker.go;
  `calls_members.rs::extends_any_constrains_like_unknown`).
- A sync iterator whose `next()` has call signatures but whose result has no `value`
  still iterates, as `any`, and is TS2490 at the iterated expression instead of TS2488. A
  `return` or `throw` member that is present but not callable (after dropping
  `null`/`undefined`) is TS2767; iteration still succeeds (`IterNotes`, filled by
  `iterated_type_noting`; tsc: `getIterationTypesOfMethod` in
  tsc/internal/checker/checker.go; `calls_members.rs::iterator_result_without_value_and_non_method_return`).
- A destructured parameter whose contextual type is `unknown`, whether declared
  (`Promise<unknown>.then`) or inferred without candidates (`trans<T>(f: (x: T) => string)`
  called with `({ a }) => a`), takes the binding pattern's own type, so the argument check
  then fails with TS2345 (`param_type_from_contextual`; tsc: `assignParameterType` in
  tsc/internal/checker/checker.go; parity `unknown_binding_pattern_fallback`).
- An unannotated rest parameter with a binding pattern takes the pattern's implied type,
  not `any[]`: `...[a, b]` is `[any, any]` and needs two arguments, `...[a, b = 1]` one or
  two, `...[a, ...b]` at least one (`param_type`; tsc: `getTypeForVariableLikeDeclaration`
  in tsc/internal/checker/checker.go; `calls_members.rs::rest_parameter_pattern_counts_its_elements`).
- In a class whose `extends` expression is the `null` literal, `super` (static or instance,
  not a `super()` callee) is `null`, so `super.x` is TS2531; the first `super(...)` call in
  its constructor is TS17005 (`check_super`, `check_derived_constructor_super`; tsc:
  `checkSuperExpression`, `checkConstructorDeclaration` in tsc/internal/checker/checker.go;
  `calls_members.rs::super_in_a_class_extending_null`).
- A rest parameter whose type is not an array or tuple (`never`, a union of tuples) takes
  the arguments at and after it as one tuple: each is checked against the contextual
  type of that element in each member (an `unknown` member does not absorb a generic one,
  so `keyof T` is not read through its constraint), literals stay literal where the
  context is primitive, and the tuple is related to the rest type, TS2345 at the first
  rest argument or at the call when there is none (`non_array_rest_type`,
  `check_candidate_args_in`; tsc: `getSignatureApplicabilityError`, `getSpreadArgumentType`
  in tsc/internal/checker/checker.go; `calls_members.rs::non_array_rest_parameter_takes_a_tuple`).
- Declaration checks (run once per file after the walk, entry point `decls.rs`):
  `unused.rs` (`noUnusedLocals`/`noUnusedParameters`: TS6133/6138/6192/6196/6198/6199/6205,
  a syntactic pass over the binder's resolved references; its observed rules are listed in
  the module doc; JavaScript files too: a JSDoc `@import` is a value import reported from
  its `@`, top-level `@typedef`/`@callback`s are never reported, and TS6205 for `@template`
  lists starts just before the first tag's `@`), `classes.rs` (accessors and class members), `heritage.rs` (class
  `extends`/`implements` and interface `extends` compatibility, `extends` cycles TS2506: TS2415/2416/2417/2420/2422/
  2430/2720/2320, ambient `declare class`es included; observed rules in the module doc), `indexes.rs` (members against index
  signatures: TS2411/2413), `redeclare.rs` (`var` and merged property redeclarations
  must have identical types: TS2403/2717; JavaScript `var`s too, a JSDoc `@type` being
  their annotation; a redeclared `var`'s initializer is checked against its own
  annotation), `decorators.rs` (decorator placement:
  TS1206/1249/1433; the expressions of valid decorators are checked here, function
  decorators with an `any` context, and a class decorator in a generator may `yield`),
  `decorator_calls.rs` (each valid decorator resolved as a call with the arguments the
  runtime passes, through `resolve_typed_call` in `call.rs`: TS1238-1241, TS1329, and the
  legacy return types TS1270/1271; standard decorators are checked by argument count
  only), `overloads.rs` (overload sets: TS2383-2394, TS2512; implicit-any
  return types of body-less declarations: TS7010/7013/7020; an unannotated body-less
  declaration returns `any`, and so does one whose body the parser made up after a
  missing `{`, a zero-width block; for TS2394 two callback
  parameters compare as signatures, overload callback to implementation callback with
  parameters only contravariant, also without strictFunctionTypes, so
  `cb: (x: 'hi') => number` against `cb: (x: string) => number` is incompatible:
  `overload_callback_params`; tsc: checker/relater.go, compareSignaturesRelated),
  `abstracts.rs` (abstract
  method bodies and unimplemented inherited abstract members: TS1245, TS2515/2650/
  2653-2656; TS2513 for `super.m` lives in `expr.rs`), `overrides.rs` (`override` and
  `noImplicitOverride`: TS4112-4117, TS4127; accessors overriding properties and back:
  TS2610/2611), `init_order.rs` (class fields read before their initialization: TS2729),
  `modifiers.rs` (the modifier grammar of every declaration from the written modifier
  tokens: one error per declaration, at the first keyword that breaks a rule, TS1024-1046,
  TS1070/1071/1089/1090/1184, TS1242-1253, TS1275/1276, TS18010/18019, TS1079/1191 on
  import declarations; the rules are in
  its module doc).

## Invariants

- **tsc 7.0.2 is the spec.** Expected output comes from running tsc
  (`npx -y -p typescript@7.0.2 tsc --pretty false ...`). tsc's source may be read to understand
  how a rule works (which error it picks, check order, how it prints types), but the code here
  is our own: never translated, never shaped like tsc's, and never from a Rust port of
  TypeScript (root AGENTS.md).
- **No false positives first.** Anything not implemented yields `TypeId::UNRESOLVED` (any-like,
  never reported). An extra diagnostic is worse than a missing one. `TypeId::ERROR` is
  different: it is tsc's error type (a reported unresolved name or import) and behaves as
  `any`, so implicit-any checks still run through it (property and element access, a
  conditional's check type, an `any` tag). A type reference with the wrong number of type
  arguments (TS2314/TS2707) is `ERROR` too in TypeScript files. A class, interface or alias
  merged with a generic function takes its type parameters from the type declarations only.
- **Messages match tsc's text exactly**, including chain lines; take texts from
  `diagnostics_generated.rs`, never retype them.
- **Related information** (rendered by the pretty output, ignored by the conformance oracle)
  is attached to the diagnostic just reported with `relate_last` (`related_span_of`:
  signature members are spanned whole) after recording `self.diags.len()` as the mark; it
  is dropped when nothing was reported. So far (observed): TS2728 on spelling suggestions,
  TS6500/TS6501 on elaborated property errors, TS6212/TS6213 "did you mean to call",
  TS2771 on TS2769, TS6210/TS6211/TS6236 on too few arguments (the first overload with the
  smallest minimum above the count; the arity error is spanned at the callee's name).
- **Never panics, always terminates.** Type and expression recursion is depth-limited
  (instantiation, relation and base-type depth limits report or fall back to `UNRESOLVED`);
  flow loops iterate at most 6 rounds; alias and base chains and base constraints
  (`T extends \`x${T}\``) are cycle-checked. Enum evaluation is cycle-checked and bounded
  across dependencies, including during unchecked emit.
  Instantiation overflow sets `inst_overflow` (TS2589) and an over-long tuple
  `tuple_overflow` (TS2799/TS2800); whoever resolves the type reports and clears them
  (`report_instantiation_overflow`), so a flag never leaks to an unrelated node. Probes such
  as `type_mentions` save and restore both flags and the count. Conditional tail recursion
  stops at 1000 steps, tuples at 10 000 elements, template cross products at 100 000.
- **The `this` argument** (`check_this_arg`, observed): a candidate with a declared `this`
  parameter checks the call's receiver (through parentheses, `!` and `?.`), or `void`
  without one, before its arguments; a failure is TS2684 at the receiver (or the whole
  call), a missing property replacing that head like TS2345's, and under TS2769 for
  overloads. `super.m()` is not checked. A union's combined signature intersects the
  members' `this` types. In signature relations a strict function type's `this` is
  contravariant (methods either way), and a source `this: void` asks nothing.
- **Union call signatures** (`union_signatures`; tsc: `getUnionSignatures` in
  `tsc/internal/checker/checker.go`, `findMatchingSignatures` and `isMatchingSignature` in
  `relater.go`). Every member signature is tried first, single-signature members included:
  it stands for the union when each other member has an identical signature, else one that
  needs no more arguments and whose parameters it is a subtype of at each of its positions
  (counts spread a tuple rest out: `(...a: [])` takes none). Several such signatures can
  come out (`F1 | F2` with `(a, b?)` and `(a, b?, c?)` gives both). A non-generic one that
  matched other signatures intersects their declared `this` types and combines their type
  predicates; its return is the subtype-reduced union. Only when nothing matches and at most
  one member has overloads are the parameters intersected position by position.
  When some members have rest parameters (not all at one position), the members fold pair by
  pair (tsc: `combineUnionOrIntersectionParameters`): positions up to the longer side's count
  intersect both types, the longer side's rest stays a rest, a rest only on the shorter side
  becomes an extra `...args` after the longer side's positions, and differing names become
  `arg<i>` (`((a, ...o: Y[]) => R) | ((...o: X[]) => R)` takes `(a, ...o: (Y & X)[])`).
- **Abstract construct signatures** (tsc: `resolveNewExpression` in `checker.go`). `new` of
  a type with any abstract construct signature is TS2511 and an error call: arguments are
  checked on their own (no TS2554/TS2345) and the result is the error type.
- **Members of a generic mapped type over a non-`keyof` key type** (`mapped_lower_bound_keys`,
  `lower_bound_of_key_type`; tsc: `getLowerBoundOfKeyType` in `checker.go`). The key type
  is read at its lower bound: `keyof T` as the keys of `T`'s apparent type, a distributive
  conditional applied to the bound of its check type, unions and intersections member by
  member. When that bound is concrete the mapped type is its own apparent type with those
  members (`Pick<T, Exclude<keyof T, "a">>` with `T extends { a; b }` has `b`, and `.a` is
  TS2339); otherwise it stays opaque (`UNRESOLVED`). `keyof` of a class instance leaves
  out private and protected members (so `Omit<this, K>` drops them). A generic mapped type
  is never the empty object type (`{ ...rest, b }` keeps its `Omit`), and `typeof x ===
  "function"` does not narrow it to `Function` (a generic mapped target relates only to a
  generic mapped source; tsc: `relater.go`).
  A homomorphic mapped type (`keyof X`, no `as` clause) whose `X` is an intersection with
  a generic member (`O & Omit<D, keyof O>`) is also its own apparent type, keyed by the
  properties of `X`'s members (`mapped_generic_intersection_modifiers`; tsc:
  `resolveMappedTypeMembers` in `checker.go` iterates the modifiers type's properties
  when the constraint is `keyof` of it). So `Required<O & Omit<D, keyof O>>` has the
  members `D` gives, and assigning a literal with them is no TS2739 (@eslint/core).
- **Intersections emptied by a discriminant** (`reduced_type`; tsc:
  `isDiscriminantWithNeverType` in `checker.go`). A property two object members declare,
  literal in at least one of them and `never` in none, whose types intersect to `never`
  makes the intersection `never` (`{ a: "" } & { a: number }`; `string & number` does not).
  A call of such an intersection is TS2349 on `never`.
- **Strict subtype arity** (`signature_related_mode`; tsc: `compareSignaturesRelated` with
  `StrictArity` in `relater.go`). Under the strict subtype relation (union reduction) a
  source signature with more parameters or a rest the target lacks is not a subtype, nor
  is one whose optional parameter stands where the target's is required, so
  `(() => void) | ((x?: string) => void)` keeps both and calls accept `f("hello")`.
- **Intersections reduced by a constraint** (`reduce_by_constraint` in `unions.rs`; tsc:
  `getIntersectionTypeEx` in `checker.go`). An intersection of exactly two types, a type
  parameter or indexed access `T` and a non-union primitive, `object` or `{}` `P` (not a
  generic template literal or string mapping), whose base constraint is made only of
  primitives, `object` and `{}`: is `T` when the constraint is a strict subtype of `P`
  (`T extends "A" | "B"`: `T & string`, `T & {}`), and `never` when no member of a union
  constraint is a strict subtype of `P` and `P` is not one of the constraint (`T &
  undefined`, `U & boolean` with `U extends string | number`). Unconstrained `T` stays.
- **Unknown-like union targets** (`simple_related`; tsc: `isSimpleTypeRelatedTo` in
  `relater.go`). Under `strictNullChecks`, for assignability and comparability, a union
  holding `undefined`, `null` and an empty anonymous object type (`{}`, or an alias of it)
  accepts any source, `unknown` included; `{} | null` and `{ a: string } | null |
  undefined` do not.
- **Constructor returns** (`check_constructor_return`; tsc: `checkReturnStatement` in
  `tsc/internal/checker/checker.go`). `return e` in a constructor relates `e` to the class's
  instance type (its own type parameters as arguments: `B<T>`), elaborated into `e` like any
  assignment, else on the statement; a failure adds TS2409 on the statement. A bare
  `return;` asks nothing.
- **Repeated class fields** (`class_member_merged_into`; tsc: `declareSymbol` in
  `tsc/internal/binder/binder.go`, `checkVariableLikeDeclaration` in `checker.go`). In tsc a
  class field merges into the earlier same-named member of the same staticness when that
  symbol holds only fields and accessors (a method only merges with methods, a getter only
  with a setter, an `accessor` field with nothing); our binder reports the repeat (TS2300)
  and keeps it in its own symbol, so the checker replays that merge order and compares the
  field's type with the first member's (getter type for accessors): TS2717 at the key.
- **Accessor pair modifiers** (`check_accessor_pair_modifiers`; tsc: checkAccessorDeclaration
  in `checker.go`). The first getter and first setter of a statically named class member
  (same staticness) are compared once: only one `abstract` is TS2676, a `protected` getter
  with a public setter or a `private` getter with a non-private setter TS2808, each at both
  names.
- **Array literal elements in elaborations** (`elaborate_array`; tsc: `elaborateArrayLiteral`
  in `tsc/internal/checker/relater.go`, `getWidenedLiteralLikeTypeForContextualType` in
  `checker.go`). The failing element's type is the element checked in the context of its
  target: a fresh literal is widened (`[1]` against `null[]` reports `number`) unless the
  target holds literals of its kind or the literal sits in a const context; a literal that
  is still a literal of its widened type (`true` of `boolean`) is kept.
- **Literals kept by context are regular** (`widen_for_mutable_location`; tsc:
  `getWidenedLiteralLikeTypeForContextualType` in `tsc/internal/checker/checker.go`). In a
  mutable location a literal either widens or, when its contextual type takes literals, stays
  a literal without freshness, so `let p = Object.freeze({ c: "1" }).c` is `"1"`.
- **Literals typed by a pattern** (`pattern_literal_excess`,
  `check_assignment_pattern_excess`; tsc: `checkObjectLiteral` and
  `getContextualTypeForBindingElement` in `tsc/internal/checker/checker.go`). An object
  literal whose contextual type is a pattern's implied type (an unannotated declaration's
  initializer, a destructuring assignment's right side, the default of a nested pattern whose
  parent type has no such property) reports TS2353 for every property the pattern doesn't
  name, against the pattern's type. Spreads are skipped; a computed name of a non-literal
  type prints as `[expr]`. Assignment patterns with a spread or computed name accept
  anything; an empty assignment pattern `{}` rejects every property (an empty binding
  pattern accepts all).
- **Patterns in bodiless signatures** (`report_signature_binding_elements`). A binding
  pattern parameter of a method signature, call/construct signature or function type
  declares no symbols, so under `noImplicitAny` every binding without a default is TS7031
  `any` at its name (tsc 7 reports them the same way; observed in
  destructuringParameterDeclaration1ES5).
- **Missing and renamed binding names** (`report_implicit_binding_elements`,
  `in_unresolved_bodyless_signature`; tsc: `checkVariableLikeDeclaration`,
  `getTypeFromBindingElement` in `checker.go`). A keyword in an object pattern can't be
  shorthand (`{ while }`): the parser leaves the name missing, and an untyped parameter
  reports it as TS7031 `'(Missing)'` `any`. tsc reports a binding element's implicit any
  when the parameter's type is first computed, and skips the type for a renamed element
  (`{ a: b }`, missing names included) of a function without a body; so in a function
  declaration none of whose declarations has a body (TS2391), a renamed element gets
  TS2842 only. An overload with an implementation is checked against it, which computes
  its parameter types: TS7031 there too (destructuringParameterDeclaration6,
  parametersSyntaxErrorNoCrash1/2/3).
- **Where `satisfies` errors go** (`report_satisfies`, `nodes::effective_check_node`; tsc:
  `checkSatisfiesExpression` in `checker.go`, `elaborateError` in `relater.go`). A failed
  `e satisfies T` is first elaborated into `e` (property and element errors keep their own
  TS2322/TS2741/TS2353 messages); otherwise it is TS1360 at the `satisfies` keyword (the tag
  name for JSDoc `@satisfies`). An argument, a returned expression and an arrow's expression
  body are reported at and elaborated into the expression inside parentheses and
  `satisfies` (`fn1({ a: 1 } satisfies unknown)` reports at `a`); a `return` statement
  that is not elaborated keeps the statement's position.
- **Overloads fix function arguments' contexts** (`fixed_arg_contexts`, observed). With
  several candidates, a candidate first checks the arguments that are not context-sensitive
  functions (a function only needs a parameter type it could fit: `function_may_fit`); if
  they fit, it checks every argument in order, and the first candidate to reach a function
  fixes its contextual type for every later candidate and for the final check (a generic
  candidate fixes them all, as its inference checks every argument). When every candidate
  fails, the last one's error comes from the same in-order check. When some candidate is
  generic, the failed call stands for the candidate with the most parameters (the first of
  those), its type arguments inferred without context-sensitive arguments
  (`skip_sensitive_inference`): its return type is the call's, and functions still unfixed
  take its context; with no generic candidate they take the parameter type all candidates
  agree on, else none. `contextual_type` and `speculative_arg_type` read the fixed context
  before `context_overrides`; each resolution of a call clears its entries first, so
  speculative and final resolutions agree. Context-sensitive functions nested in an object
  or array literal argument are fixed the same way: the first pass checks the literal with
  each of them read as `any` (`placeholder_fns`), and the first candidate whose in-order
  check reaches the literal fixes their contexts (`fix_nested_contexts`). A single candidate never fixes; a single
  generic candidate whose other arguments fail is instantiated without its
  context-sensitive arguments too.
- **Inference between type operators.** `S[K]` to `T[L]` infers `S` to `T` and `K` to `L`;
  `keyof S` to `keyof T` infers `S` to `T` contravariantly (tsc: inference.go,
  inferFromTypes), so `find<T, K extends keyof T>(o: T[K]): [T, K]` called with an `A[B]`
  gives `[A, B]`.
- **Inference priorities.** Candidates through homomorphic mapped types and to the branches
  of a conditional target are weak (used only without other candidates); inferences from
  the call's contextual type to the return type come last (only for parameters the
  arguments infer nothing for) and contextually type arguments only where the parameter
  type is a type parameter at the top level (`Promise.resolve({ m: 'PUT' })`). Object literal
  candidates combine into one union candidate after the others. Instantiations of one alias
  infer from alias arguments (`Output<R>` to `Output<T>`; object and union aliases by
  measured variance). An `any` argument does not infer `any` into nested positions
  (tsc infers `unknown` for `attempt<T>(p: Promise<T>)` given `any`).
- **Deterministic and thread-safe.** Shared results are write-once and must not depend on the
  thread or on speculation: no published node caching while `speculative > 0` or inside a
  pending flow loop (speculative results live in `spec_node_types` / `flow_temp`, tagged
  with the assumptions they hold under), and symbol types depending on speculative parameter
  types are not published. The same holds for contextual parameter types, function
  expression types and their signatures taken inside a pending flow loop (an aliased
  condition `if (ok)` re-checks `Array.from(g).some(x => ...)` while `g`'s loop type is
  open): a function expression is checked there as if speculatively.
- **Program loading** (`load`): files are parsed in breadth-first waves (`parse_wave`, up to
  `threads` threads); each parsing thread also resolves its file's module requests into the
  shared resolver's caches (`warm_resolutions`), and the first wave parses the default libs on
  a thread of its own. The serial walk that follows only reads those caches, so the file list
  and its order do not depend on which thread got there first.
- **Check order** (`check_files`): with more than one thread, files with more than
  1/(`BIG_FILE_SHARE` x threads) of the program's text are checked first, biggest first, so
  no thread starts a long file when the others are done; the rest follow in program order.
  Output must not depend on that order (results are kept by file and sorted). After
  checking, a big program's type store and levels are freed on a thread of their own
  (`release_in_background`); the checker is not used afterwards.
- `.json` program files (`resolveJsonModule`: an explicit `.json` include or an import) are
  parsed with `ts_r_parser::parse_json` and never type-checked (`should_check`), as in tsc;
  only their syntax errors are reported.
- Syntax errors suppress semantic diagnostics (tsc behaviour), and the program diagnostics too
  (TS6053 and TS2688 for missing `/// <reference path|types>` targets, in any file: tsc 7.0.2
  reports only the syntax errors, TS1084 included). tsc's test harness reports both,
  and so does `check_program_through_syntax_errors` (used by the conformance example): every
  file is checked, but a file with syntax errors gets no checker grammar errors (TS1xxx; tsc's
  checker reports those only in clean files; TS1294 for `erasableSyntaxOnly` and TS1141
  for a non-literal `import(x)` type are semantic and stay, as do the import checks TS1192,
  TS1205, TS1361, TS1362, TS1484 and TS1485, which tsc's checker reports with `error`;
  so do the binder's own TS1xxx errors listed in `BINDER_CODES`, see the binder's AGENTS.md). Recovered trees must not produce semantic
  errors tsc does not: a `return` outside a function does not check its expression, an empty
  `for (var in X)` / `for (var of X)` head is not checked, a missing operand is not TS2357,
  and in JavaScript `require("m")` (one string argument) is not a use of a `require` value.
- The parser's checker-grammar errors (`SourceFile::grammar`, also in `SourceFile::syntax`:
  `class C<> {}`, `set x() {}`, TS1093 on a constructor annotation) are checker diagnostics:
  the driver reports them only for checked files (`should_check`) and they do not stop
  semantic checking. A constructor's or setter's annotation is not checked as a return type
  (no TS2355).
  Regex errors that depend on the target (TS1501 flag, TS1503 named groups) are grammar-only
  in the parser; `check.rs` keeps them only when the file's target is below the one named in
  the message (`regex_needs_target`). Octal and `\8`/`\9` escapes in strings (TS1487/TS1488)
  are in `SourceFile::lenient`: they are tsc syntax errors (the driver reports them with the
  syntax errors and marks the file broken), so the grammar merge skips them.
  Grammar errors the parser does
  not report (TS1042/1044/1070/1090/1184/2369 modifiers, TS1163, TS1308 for `await` in a
  function that is not async, at the keyword) are reported here; TS5076,
  TS1106, TS1497 and TS1164 come from the parser, so never emit them here too. Those and the
  TS8xxx errors for TypeScript syntax in JavaScript (except TS8006) do not suppress
  semantic diagnostics (`driver::CHECKER_GRAMMAR_CODES`).
- Declaration checks that relate whole class/interface types run under a relation budget
  (`RelState::budget`, counted in `recursive_related` and structural inference). A spent
  budget means "related" and never produces an error; pathological recursive generics
  (`complexRecursiveCollections`) otherwise exhaust memory. Every other outermost relation
  (`type_related_entry`, `relation_error_chain`) gets `MAX_RELATION_WORK` (100 000)
  comparisons the same way: relating instantiations of big generic interfaces structurally
  (rxjs's `Observable`, whose measured variance is unreliable) expands exponentially. Results
  computed after the budget ran out are not cached.
- **Measured variance** (`variance.rs`). Two instantiations of one generic class, interface
  or alias relate by their type arguments in the direction each parameter's measured
  variance allows (independent, covariant, contravariant, bivariant, invariant). A
  measurement relates instantiations with marker type parameters (sub, super, unrelated);
  marker instantiations compare structurally and other instantiations of a type being
  measured are "maybe related". It is a pure function of the declaration: nested
  measurements and relation results stay in the measurement's `Probe` (never the shared
  cache) and only outermost results are published, so it doesn't depend on the order files
  or threads ask. A measurement that meets a conditional, indexed-access, `keyof`,
  substitution, template literal, string-mapping or generic mapped type mentioning a marker,
  or a signature whose rest parameter is a union of tuples mentioning one
  (`next(...[value]: [] | [N])`), or runs out of its budget, is unreliable: then (and for a
  covariant `void` target argument) arguments the variances reject fall back to structure.
  A failure of an unreliable measurement or with an invariant parameter is explained
  structurally, others (a covariant `void` one included) by the arguments.
- **Signature lists.** Two instantiations of one type literal (or one generic type) relate
  their signatures pairwise with type parameters erased; other lists need each target
  signature matched by some source signature (erased), and a target no source matches is
  explained by comparing it with the first source signature.
- **Speculation is bounded and does not redo work.** Speculative checks (`enter_speculation`
  / `leave_speculation`: candidate inference, contextually sensitive arguments, inferred
  type predicates) publish nothing, so anything they re-check multiplies:
  - every change of speculative assumptions (entering or leaving speculation, a contextual
    type override, speculative parameter types) goes through `push_assumptions` /
    `pop_assumptions`; expression types and flow results computed while speculating are
    reused under the same assumptions id (`spec_epoch`), never published;
  - an unannotated local inside a speculatively checked function expression (or context
    sensitive arrow) is typed once per check of that function (`spec_frames`, keyed by
    `speculation_owner`), not once per reference; so is a binding of an unannotated
    destructuring pattern (`const [value] = args`), whose type follows the pass's contextual
    rest parameter (`is_declared_without_annotation`);
  - a function expression argument checked speculatively against the same contextual type,
    with the same parameter types around it, reuses its type (`spec_fn_cache`,
    `speculative_fn_key`);
  - a call checked while speculating does not re-check its arguments for errors;
  - an assignment of a non-union declared type, and the declaration of an unannotated
    `const`, narrow to the declared type without checking the assigned value
    (`assignment_effect`, `assigned_type`).
  As a safety net each outermost speculative check may check `MAX_SPECULATIVE_WORK`
  (200 000) expressions and a file `MAX_FILE_SPECULATIVE_WORK` (500 000; the bench repos
  need at most about 21 000); past that, expressions are `UNRESOLVED` and the file gets one
  TS2589 at the innermost reporting expression around the overflow.
- Printed type names carry no namespace qualifiers (tsc 7) unless `Ck::print_qualified` is
  set: relation messages whose source and target would print alike, union members or tuple
  elements that would print alike, and an enum type widened from one of its members.
  Under `print_qualified`, `Ck::print_scope` (set only by declaration checks) still drops
  the namespaces that enclose the error location. `Ck::type_to_string_at` sets both for
  editor hovers, where tsc qualifies names from the hovered location (`m1.m2.c`).
- **Names in messages** (tsc: `checker.go`, read as a reference). TS2341/TS2445 name the
  declaring class by its declared type, type parameters included (`D<T>`); TS18013 names
  the class symbol (a class expression after its variable, `C`) (`access_error_class_names`).
  Destructuring reads are accessibility-checked too: an object binding element (at the
  element) or an object assignment pattern's property (at the property) whose literal name,
  computed keys of a literal type included, is a private or protected property of the
  parent's type gets TS2341/TS2445 as a property access would (tsc: `checker.go`,
  checkVariableLikeDeclaration and checkObjectLiteralDestructuringPropertyAssignment;
  `Ck::check_destructured_member_access`, parity `destructured_member_access`).
  A property symbol in a message is its first declaration's name as written
  (`Ck::written_name`: `'2.0'`, `"e"`, `1.0`, `[c0]`; tsc: `nodebuilderimpl.go`,
  getNameOfSymbolAsWritten) in TS2416, TS2325/TS2442/TS2443/TS2444, TS2327 and TS2326;
  TS2717 names the redeclaration as it writes the key (`literal_property_names_in_messages`).
- **Operator and operand positions** (tsc: `checker.go`, checkDeleteExpression and the
  arithmetic operator checks, read as a reference). TS2447 sits on the operator token and
  names it as written (`^=` for a compound assignment, `Ck::operator_token_span`); TS2703
  sits on the operand with its parentheses skipped; a bad parameter default is reported at
  the parameter, so a parameter property's span starts at its modifier. Too many arguments
  start at the first excess one; an empty (missing) argument starts at the next token
  (`operator_and_operand_positions`).
- **Member order of normalized literals** (tsc: `checker.go`, getWidenedTypeOfObjectLiteral
  and getNamedMembers; `utilities.go`, compareSymbolsWorker, read as a reference). An
  object literal widened next to its union siblings lists its members, the `undefined`
  stand-ins for missing ones included, by where they were declared (file order, then
  position). tsc makes one stand-in per property name and reuses it, so a name is placed by
  the property its first stand-in came from (`Ck::undefined_prop_origin`;
  `normalized_literal_member_order`).
- **Object types in union order; spread member order** (tsc: `utilities.go`, CompareTypes,
  compareTypeNames and compareSymbolsWorker; `checker.go`, getNamedMembers and
  getSpreadType, read as a reference). Among object members of a union, named types
  (declared, referenced, aliased) come first by name; unnamed ones (type literals, object
  literals, `typeof` types) follow by declaration position, interleaved; a written `{}` is a
  single shared type with no declaration, so it sorts after them (with mapped types); tuples
  last (`Checker::object_sort_key`). A spread's result lists its properties by where each
  was declared, ones with no declaration last by name, not left-then-right
  (`Ck::sort_props_by_declaration`; `spread_member_and_empty_literal_order`).
- **`extends any` reads as `unknown`** (tsc: `checker.go`, getConstraintFromTypeParameter,
  read as a reference). The "could be instantiated with a different subtype of constraint"
  hint names a declared `any` constraint `unknown` (`constraint_extends_any_named_unknown`).
- **An intersection spread over a union prints as written** (tsc: `checker.go`,
  getIntersectionTypeEx and getConstituentCount, read as a reference). `T & (0 | 1 | 2)`
  becomes the union `(T & 0) | (T & 1) | (T & 2)`; when that union is a single union
  distributed, has no alias, holds an intersection and spells more pieces than the written
  intersection, its origin is that one intersection, so it prints `T & (0 | 1 | 2)`
  without parentheses (`Ck::constituent_count_of`; `intersection_over_union_as_written`).
- Identifiers the binder could not resolve fall back to the exports of other declarations
  of each enclosing namespace, innermost first, before globals; `namespace A.B { }`
  encloses B, then A (`dotted_namespace_outer_exports`). Class statics and enum members
  merged into a namespace are not in scope in its body, even when the binder resolved the
  name through the shared exports table (`member_out_of_scope`;
  `merged_members_not_in_scope`): the lookup then goes on outwards, so
  `typeof Decimal` in `namespace Decimal` beside `class Decimal { static Decimal }` is
  the class (decimal.js, bignumber.js).
- **CommonJS names in JavaScript.** A JavaScript file with no ECMAScript module syntax and a
  CommonJS indicator anywhere (`SourceFile::is_commonjs_module`: a one-argument
  `require(x)`, an assignment to `module.exports[.x]` or `exports.x`/`exports["x"]`,
  `Object.defineProperty(exports, ..)`) defines `module` and `exports`; a one-argument
  `require(x)` call in any JavaScript file (with `require` undeclared) is not reported.
  Elsewhere they are TS2591/TS2304, as is a bare `require` (`commonjs_names_in_js`).
  Their types (observed tsc 7; `require`, `module` and `exports` count when undeclared or
  ambient, `is_commonjs_global`):
  - `require("m")` in JavaScript is the module's type: its `export=` value or its exports
    object (`commonjs_require_type`), `any` when unresolved or not a module; TS2307 and
    TS2591 apply to its specifier, TS7016 does not. `var m = require("m")` is not widened.
  - The binder declares `exports.x = v`/`module.exports.x = v` as exports (expando-typed:
    the union of the widened assigned values, `undefined` ones dropped, `any` alone;
    `Object.defineProperty(exports, "x", d)` is `d`'s widened `value`, its `get` method's
    return type or its `set` method's parameter type, `define_property_type`) and
    `module.exports = v` as `export=` (the value's type, unwidened; the first assignment's
    when there are several).
  - With an `export=`, `exports` and `module.exports` are its value
    (`commonjs_export_equals_type`), so `exports.x = ...` is TS2339 when it lacks `x`; `any`
    inside the value itself and when there are several assignments. TS2309 is reported at
    the first `module.exports = v` when other CommonJS exports exist
    (`commonjs_export_assignment_conflicts`); `module.exports = exports` changes nothing.
  - A default import of a CommonJS JavaScript module is the whole module, even when it
    assigns `exports.default`, unless it exports `__esModule` (`commonjs_default_module`).
- **No expando namespaces.** In a JavaScript type name `Outer.Inner`, a variable or function
  `Outer` (an expando container) has no namespace meaning: TS2503 at `Outer` (tsc 7), unless
  it is a `require` binding or the root of a dotted `@typedef` name in a file declaring it.
- **`// @ts-check`** (`SourceFile::has_ts_check`, before the first token, `@ts-nocheck`
  wins; the pragma name ends at whitespace or `:`, so `// @ts-nocheck: why` counts) checks a JavaScript file without `checkJs` (`should_check`). ts_r_history's lazy
  `checks` and ts_r_lsp's `semantic` still look at `checkJs` only.
- **Plain JavaScript** (`driver.rs` `is_plain_js`): a JavaScript file with `checkJs` unset
  (not `false`), no `// @ts-check`/`// @ts-nocheck` and outside `node_modules` is checked
  too, but only the codes in `PLAIN_JS_CODES` are kept (grammar, redeclaration, strict-mode
  and private-name errors, TS2528, TS2839; observed in tsc 7). An unknown `#name` property
  (TS2339) is TS1111 there. Only `check_program*` does this: ts_r_history and ts_r_lsp go
  through `should_check` and do not report plain-JavaScript errors yet.
- **Private names on `any`** (`property_access`): `x.#a` where `x` is `any` or an error
  type and no class encloses the access is TS18016 at the name (tsc 7); inside a class it
  is silently `any`. Other object types get TS2339 (`private_name_js_errors`).
- **Unreachable code after calls** (`check_unreachable_stmts`, `allowUnreachableCode:
  false` only): runs of statements in the binder's `stmt_flow` whose flow is not reachable
  (a call returning `never` or an assertion, an exhaustive switch) are one TS7027 over the
  run; runs inside a reported run are not reported again. A call ends the flow only through
  a dotted name (`f()`, `a.b.f()`, not `f[0]()`) whose variable or property symbol has an
  explicit type (`const f = (): never =>` does not; `compute_call_kind`). A mapped type's
  property (`Readonly<S>["panic"]`) counts when the property it maps has one
  (`never_call_through_mapped_property`; tsc: checker/flow.go, getExplicitTypeOfSymbol). After
  `try { fail() } finally {}` the rest is unreachable too: the walk through the binder's
  `ReduceLabel` at a finally block's end sees only the finally entry's normal antecedents
  (the ends of the try and catch blocks), not its exception edges (`unreachable_after_finally`;
  tsc: checker/flow.go, isReachableFlowNode).
- **Type-only names in JS imports and exports** (`check_js_type_only_aliases`, checked JS,
  top-level statements): an import specifier or default import whose alias target has no
  value meaning is TS18042 at the imported name (`import("m").name`, or `import("m")` for a
  default import); a non-type-only export specifier naming a type is TS18043 at its local
  name. A top-level JSDoc `@typedef` of an ES module lives only in the module's exports, so
  an unresolved export name is looked up there (`js_type_only_imports_exports`). `import
  type` in JS is TS8006 and suppresses the semantic pass (tsc 7).
- **Switch fallthrough** (`check_case_fallthrough`): under `noFallthroughCasesInSwitch`, a
  clause in the binder's `fallthroughs` whose end flow is reachable (`is_flow_reachable`:
  calls returning `never` end it) is TS7029 at the clause (`switch_fallthrough`).
- **Assignment-declared properties read too early** (`used_before_assignment_declaration`):
  under strictNullChecks, reading `X.p` where `p`'s value declaration is `X.p = v` (not
  `this.p`) in the same flow container, and flow analysis of that reference (initial type
  `T | undefined`) still includes `undefined`, is TS2565 (`super.p`, or `f.p` before
  `f.p = v`; tsc 7). CommonJS `module.exports.p` is not covered (`module` is `any`), nor a
  `const` arrow's expando in TypeScript (`expando_used_before_assigned`).
- **Arrow `this` parameters.** An arrow function whose first parameter is `this` is TS2730
  at the parameter (`check_function_like`; JSDoc `@this` on an arrow is the parser's,
  `arrow_this_param`). Elsewhere (`check_this_param_placement`, from the type-annotation
  walk; tsc: checker/checker.go, checkParameter) a `this` parameter after the first is
  TS2680, one of a constructor, construct signature or constructor type TS2681, and one of
  a get/set accessor (class, object literal or signature) TS2784, all at the parameter.
- **JavaScript `this.x = v` properties.** `/** @private */` or `@protected` on the
  assignment statement makes the property private or protected (`JsDocTagKind::Private`/
  `Protected`, read by `jsdoc_assignment_access`). A property assigned only in a class's
  methods or accessors, never its constructor, is `T | undefined` under strictNullChecks
  (not in a derived class, where tsc uses an inherited property's type; not modelled).
  `this.x` is no entity name for TS18047-TS18049: possibly-nullish `this.x` is
  TS2531-TS2533 (`js_this_assignment_access`).
- **JavaScript base-class type arguments** (`check_js_class_extends_type_args`): a generic
  base class whose `@extends`/`@augments` type arguments are missing or too many is TS8026
  (`Expected A<T> type arguments ...`), or TS8027 when some parameters have defaults (tsc 7
  prints `D<T, U>-<min>`), at the `extends` expression, under `noImplicitAny` only. Without
  it nothing is reported and the missing arguments are `any` (`js_extends_type_args`).
  Without type arguments, a parameter's default of `unknown` or `{}` fills in as `any` too
  (`fill_type_args_in`; `class C extends Foo` over `Foo<T = {}>` gives `props: any`; tsc:
  checker.go, fillMissingTypeArguments with isJavaScriptImplicitAny).
- **JSDoc on object literal members.** `/** @type {T} */ a: v` (or a shorthand `a`): the
  property has type `T`, `v` is contextually typed by `T` and checked against it at the key
  (`jsdoc_typed_property`). A method's `@type` without call signature is TS8030 (as for a
  function declaration or a class method, not a getter; not for function expressions or
  arrows; `jsdoc_illegal_tags`), and an untyped parameter under a `@type` without call
  signature is TS7006. A failed JSDoc cast is reported at its
  type (TS2352), a JSDoc type predicate outside a return type is TS1228.
- **JSDoc type syntax in TypeScript** (`check_jsdoc_syntax`, from `Ast::jsdoc_syntax`):
  TS17019 (postfix `?`/`!`), TS17020 (prefix) and TS8020 (`*`), suggesting the inner type
  (`!`), `T | undefined` (postfix `?`) or `T | null | undefined` (prefix `?`), with
  `strictNullChecks`; `any`, `unknown`, `never`, `void` and `undefined` as they are. Like
  grammar errors they are dropped in files with syntax errors (driver). A primitive type
  name used as a value (`f(number)`) is TS2693, not TS2304/TS2552. A missing `await`
  whose parent is a call (`await(p)` outside an async function) is TS2311. A missing named
  import written as a string literal shows with its quotes in TS2305/TS2724
  (`'"missing"'`; `name_messages_misc`). A type-only `Promise`,
  `Symbol`, `Map`, `WeakMap`, `Set` or `WeakSet` read as a value is TS2585 (suggesting
  `lib` es2015) instead of TS2693 (`type_as_value_info`; tsc: checker/checker.go,
  checkAndReportErrorForUsingTypeAsValue).
- **JSDoc in JavaScript** (`jsdoc.rs`). `SourceFile::new` runs `ts_r_parser::attach_jsdoc` on
  every JavaScript file before binding, so JSDoc types are ordinary annotations, type
  aliases and imports to the binder and checker; its JSDoc syntax errors
  (`SourceFile::jsdoc`) are reported for checked files. What differs for JSDoc nodes
  (`Ast::is_jsdoc`), as tsc 7 does:
  - `String`/`Number`/`Boolean`/`BigInt`/`Void`/`Undefined`/`Null` (the global or no
    declaration, no type arguments) are the primitives (with type arguments, `Void`,
    `Undefined` and `Null` are TS2315); `function` is `Function`; `Object<K, V>` is
    `Record<K, V>` (no TS2315); `Object` is `any`, `array` is `any[]` and `promise` is
    `Promise<any>` without `noImplicitAny` (lookup failures with it). `exports` and
    `module` in a CommonJS module are values (TS2749).
  - A generic class or interface with missing type arguments takes `any` for them, and
    extra ones are ignored: TS2314 only under `noImplicitAny` (a type alias always). TS2314
    names an alias without its type parameters (`'Pair'`; TypeScript too).
  - A failed `@satisfies` is reported at the tag name and elaborated like an assignment
    (TS2741, TS2353, TS2322 in the literal), TS1360 replacing a plain TS2322 head
    (`report_jsdoc_satisfies`). (tsc 7 does the same for TypeScript `satisfies`, at the
    keyword; not done here.)
  - A whole module as a type (`module_as_type`): `import("m")` without a qualifier, and
    `M` for `const M = require("m")`, is the class or other type of the module's
    `export =` (in JavaScript, `module.exports =` a class declaration or expression);
    any other module (an ECMAScript module, a namespace or value, a CommonJS module
    exporting a function, object or variable) is TS1340 (TS2749 for the `require` name).
  - `import("m").A.B` as a type (TypeScript and JSDoc; `check_import_type_qualifier`):
    TS2694 `Namespace '"<path>".A' has no exported member 'B'` at the first name a source
    module (or namespace in it) does not export, or at a last name that is only a value.
    Not for `declare module`, `export =` or CommonJS JavaScript modules, nor the root of a
    dotted `@typedef` name. An import resolved to a `.jsx` file without `jsx` set is
    TS6142, as for `.tsx`, whether or not the file is in the program.
  - Another name through a `require` binding (`is_require_binding`), `o.Name` on a plain
    variable (`@typedef {T} o.Name` is not modelled), `typeof module.exports.x`, and a
    qualified import type of a CommonJS JavaScript module are left unresolved and unreported. Except, as tsc 7
    reports TS2749 for them, a plain name bound to a value of a module
    (`require_destructured_value`): `const C = require("m").C`, and `const {C} =
    require("m")` of a `module.exports = {C}` property, of `exports.C = v` with `v` not a
    class expression or identifier, or of an ECMAScript export without type meaning.
  - `/** @type {T} */ x.y = v`: `v` is contextually typed by `T` whatever `x.y` is; an
    expando property has type `T` (`jsdoc_expando_type`), and a later `this.y = w` in a
    method is contextually typed by it too. For `module.exports = v`, `exports.x = v` (no
    `module.exports =`) and `F.prototype = v`, `v` is checked against `T` at the target,
    and `T` is the `module.exports` type.
  - `/** @type {T} */ export default v`: `v` is contextually typed by `T` and checked
    against it (`jsdoc_export_default_type`).
  - `/** @type {F} */ function f() {}`: `F`'s signature is the function's contextual
    signature (parameter, `this` and return types); its type node is checked like an
    annotation (`detached_jsdoc_types`, which also has the `@type` of assignment targets),
    and TS8030 when `F` has no call signature (`any` and the error type included), several,
    or one with fewer parameters than the function. A function declaration's single non-generic `F` signature
    (`jsdoc_declared_signature`) types its parameters and is its declared return type
    (`return` statements are checked against it). A function (declaration, expression or
    arrow) whose `@type` has a single call signature gets the missing-return checks
    (TS2355/2366/2534/7030) against that signature's return type, reported at the tag's
    type (`check_jsdoc_typed_function_returns`). Likewise a getter without annotation gets
    them against its setter's annotated parameter type, reported at the getter's name
    (`getter_setter_typed_missing_return`; tsc: checker.go getReturnTypeFromAnnotation).
  - `C.prototype.m ??= function () {}` (also `||=`, `&&=`) types `this` like `=`.
  - `o.f = function () {}`: `this` is `o`'s type (silent `any` when it isn't a known object
    type); `exports.f = function` and `module.exports.f = function` leave `this` untyped
    (TS2683), as tsc 7 does (`assigned_function_this`).
  - Names through the root of a dotted `@typedef`/`@callback` (`Ast::jsdoc_namespaces`)
    are not modelled and stay silent.
  - JSDoc syntax errors stay when the file has parse errors (driver: `SourceFile::jsdoc`
    is exempt from the "no TS1xxx in a broken file" rule), and the JSDoc errors tsc 7
    reports as parse errors (a nameless `@typedef {T}`) are in `SourceFile::syntax`.
  - Importing names from a JavaScript script (no ECMAScript or CommonJS module syntax;
    `@import` included) is TS2306, as for a declaration-file script; `.jsx` and `.tsx`
    targets without `jsx` are not reported. A type-only import (`@import`, `import type`)
    with an empty `with { }` is TS1464. JavaScript ES modules get the TypeScript import
    checks (TS1192, TS2305, TS2306); CommonJS modules and JSON targets do not.
  - A JSDoc type naming an import whose target is only a value is TS2749, unless the import
    or its immediate export also has a type or namespace meaning (`import_export_has_type`:
    a default export merged with an interface) or the target is `export =`.
  - `override` checks run in JavaScript with the JSDoc `@override` messages
    (`override_message`: TS4119-TS4123, TS4128).
  - TS7053 on an element access of `this` prints the class (the `this` type's constraint).
- **Implicit any in JavaScript**: TS7006/TS7019 as in TypeScript; a parameter is exempt only
  when its function has a JSDoc `@type` or follows `@overload` comments (`has_jsdoc_type`).
  An `@overload` comment without `@returns`/`@return` is TS7012 at the tag name
  (`check_jsdoc_overload_returns`; overload signatures themselves are not modelled). A
  binding-pattern parameter made optional (`{ a }?`, or JSDoc `@param [o]` / `{T=}`) with
  no default in a function with a body is TS2463 at the parameter (`check_param`; tsc:
  tsc/internal/checker/checker.go, checkParameter). JSDoc syntax errors (`SourceFile::jsdoc`) are part of each checked file's `check_files`
  result, so the history paths report them too.
- **Untyped JavaScript signatures** (`is_untyped_js_signature`): a JavaScript function,
  method or constructor with no parameter type (no typed `@param`, no `@type` on it, no
  annotation) has `min_args` 0: every parameter is optional.
- **JavaScript object literals** (`is_js_literal_type`: an object literal type whose
  declaration is an `ObjectLit` in a JavaScript file) are open without `noImplicitAny`: a
  missing property is `any`, not TS2339. A `{}`-initialized JavaScript variable has its
  expando properties (binder) and prints as an object literal (`{ k: number; }`).
- `yield` outside a generator reports TS1163 only; its operand is not checked.
- An unannotated generator without a contextual signature infers
  `Generator<Y, R, unknown>` / `AsyncGenerator<..>` (`infer_generator_type`): `Y` the
  union of yielded types, `R` the returned ones (`void` without any, `undefined` added
  when the end is reachable); a single unit type widens, a union keeps its literals. A
  contextually typed generator stays `UNRESOLVED`. In such a generator a `yield`
  expression's value is `any`.
- A `with` body is not checked (tsc: its names are `any`): TS2410 at the statement, TS1300
  directly in an async function (`with_block_names`).
- Unresolved names in a class member that name a class member get tsc's variants:
  TS2663 `this.x` for an instance member declared in a class (not in static members),
  else TS2662 `C.x` for any property of the static side (`prototype`, `Function` and
  `Object` members included); arrows are transparent, other functions end the search; an
  anonymous class expression takes its variable's name (`class_member_name_suggestions`).
- A type name that only resolves to a namespace (instantiated or not) is TS2709, before
  the TS2749/TS2304 fallbacks; aliases and module files (UMD globals) are excluded
  (`namespace_as_type`). The TS2749 fallback sees a module file's or namespace's exported
  declarations (not export specifiers or `export * as ns`) as in scope
  (`lookup_in_scope_exports`): `export const X = 1; let y: X` is TS2749. An alias with no
  type meaning of its own is TS2709 when it imports a whole module (`import * as M`,
  `import M = require`, without `export =`) or resolves to a namespace declaration
  (`import n = N`, `import { N }`).
- A type reference whose name resolves to an import with no type or namespace meaning,
  and whose target is a plain value (not an alias, not `export =`), keeps looking in the
  globals: `import { Blob } from "./x"` of a `const Blob` still lets `let b: Blob` name the
  global `Blob` type (`global_type_past_value_alias`; tsc: `getSymbol` in `checker.go`
  checks a found alias's target flags against the meaning and skips it on a miss). With
  no global of that name it is still TS2749 (node-fetch, fetch-blob).
- **Type libraries.** `types: ["*"]` is every non-`.` package directory of the type roots
  (one without declarations is skipped, where tsc reports TS2688 "implicit type library");
  a missing explicit entry is a global TS2688 with tsc's "The file is in the program
  because" chain. `/// <reference types>` resolves in its file's mode under the Node module
  kinds (a CommonJS file takes the `require` condition) unless `resolution-mode` is given
  (`SourceFile::references` kinds `types-import`/`types-require`;
  `type_reference_resolution_mode`, `types_wildcard`, `types_entry_missing`).
  A `/// <reference types="x" />` that does not resolve (the loader records it in
  `ProgramFiles::missing_type_references`, not under `noResolve`) is TS2688 at the name as
  written, a program diagnostic beside the path-reference ones
  (`references::type_reference_diagnostics`). A package whose `exports` gives `.` no
  types is not found through its `types` field. History's lazy loader does not record
  these yet. The conformance harness turns `// @symlink: a,b` under `@filename: f` into
  symlinks `a` and `b` to `f`.
- Call, construct and method signatures merged from several interface declarations list
  the later declaration's signatures first (outside lib files).
- The printer shows the `undefined` an optional property adds (`a?: string | undefined`)
  except under `exactOptionalPropertyTypes`.
- Under `exactOptionalPropertyTypes` an optional property's `missing` type is dropped for
  writes (`o.p = v`) and when relating property types (optionality is checked on its own),
  so an explicit `undefined` only meets an optional property that declares it.
- Contextual types of optional members (tsc: `getTypeOfConcretePropertyOfContextualType`,
  `getContextualTypeForElementExpression`): an optional tuple element's context includes
  `undefined` like an optional property's (`[number?]` gives `number | undefined`); under
  `exactOptionalPropertyTypes` neither has `missing` (`T` inferred from `y?: number` is
  `number`).
- A property an object literal gains from a sibling in a union (`{ a } | { b }` normalized
  to `b?: undefined`) has the type `missing` under `exactOptionalPropertyTypes` (tsc:
  `getUndefinedProperty`), so `"a" in z` makes `z.a` present.
- An optional tuple element adds `undefined` to the tuple's element type and to its array
  base (tsc: `getTupleBaseType`, where the element type arguments already carry the
  optionality): `for (e of [number, string?])` and `.map`/`.filter` see
  `string | number | undefined`. An optional chain operand is an entity name for TS18048
  (`item?.id` is named `'item.id'`).
- **Computed member names.** `const A = Symbol()` / `Symbol.for(..)` declares a unique
  symbol; symbol-keyed names are `__@name@symid` and print as `[A]`, `[C.key]` or
  `[Symbol.iterator]` (`Checker::symbol_key_display`). Only entity names (`k`, `a.b.c`)
  late-bind a member of a class, interface or type literal (`collect_computed_members`);
  `[f()]` and `[(k)]` do not, and are reported (TS1165/1166/1168/1169/1170). Members whose
  key type is not a literal or unique symbol imply index signatures (`computed_index_infos`)
  whose `decl` is the member, not an `IndexSignature`. Only entity-name keys imply one (a
  `number` key implies only a `number` index), and an implied index also covers the named
  members it applies to (on a generic class's instance side or a generic interface the
  implied index is `any`), so it only constrains members with non-entity dynamic keys
  (`[+s]`, `[s + s]`); named members meet written indexes only (`written_indexes` in
  `indexes.rs`). `static [k: K]: V` signatures are on the static side (`typeof C`) and are
  not compared by TS2417.
- **Static side of a derived class.** Its own members are the class's exports (a merged
  namespace's too); the base constructor's properties are added for names it lacks, and also
  replace an own member that is an alias (`export { S as default }` in the namespace has no
  value meaning of its own), so `R.default` is the base's and TS2417 compares nothing there
  (`class_static_members` in `members.rs`; `static_alias_member_inherited`, seen on postcss;
  tsc: checker.go addInheritedMembers).
- **Flow containers.** Functions, class property initializers and method bodies (and the
  computed key of a method or property) and namespace bodies are flow containers (outer variables are assumed assigned inside); a static block is not (its
  body runs inline in the surrounding flow, binder too). An outer local `let` that is never
  assigned and has no initializer is still reported when read in a closure.
- **Comparability.** An unconstrained type parameter is comparable with any non-generic type
  and `{}` with primitives; two unrelated type parameters are not. Relational operators
  compare enum members as their primitive (`base_for_comparison`). Signatures compare with
  both sides' type parameters erased to `any`.
- **`void` in unions.** `void | undefined` keeps both (only subtype reduction drops the
  `undefined`); a union with `void` is not a literal type, so error messages don't widen it.
  A call on `Function | (() => T)` checks the arguments against the callable members and
  is `any`.
- **Recursive aliases.** In a deferred position (an array or tuple element, a type argument
  of a generic interface: `Array<J>`, `Iterable<J>`, `Box<J>`) a plain reference to a
  non-generic type alias that is being resolved, or not yet resolved while another alias
  is, is a store forward reference (`deferred_alias_ref`, `pending_forwards` in
  `declared.rs`); the alias's resolution links it (without this, react18's `ReactNode =
  ... | Iterable<ReactNode>` read as `Iterable<any>` and accepted anything). Generic
  aliases and class type arguments are not deferred. Relations unforward both sides
  first (`[Traversable]` with `type Traversable = object`; `generic_constraint_args`).
  A union of an aliased union and members it already contains is that aliased union
  (`C | undefined` with `type C = undefined | ...` prints `C`; `union_origin_items`).
- **Conditional and indexed sources.** A deferred conditional source relates to a target
  without type parameters through its constraint, then its branches' union
  (`conditional_source_related`; a constraint that is still generic stays permissive); an
  intersection with a conditional member also tries its base constraint. A source
  `{ [K in C]: X }[C]` relates as `X[K := C]` (and any `{ [K in C]: X }[I]` has the base
  constraint `X[K := I]`; one that leads back to itself has none); `(A & B)[K]`'s base
  constraint is the intersection of `A[K]`'s and `B[K]`'s, without a union part that contains
  another part. A conditional without `infer` relates to a generic `keyof X` through its
  branches' union. Type predicates narrow a conditional member with no concrete base
  constraint by intersection (`DeepPartial<T> & any[]`). A concrete source never relates to
  `(M & O)[K]` with a generic key when the intersection holds a generic mapped type: tsc
  has no constraint to relate it through, so `obj[k] = undefined` on
  `({ [P in keyof T]: X } & O)[keyof T]` is TS2322
  (`spread_not_contextual_mapped_intersection_index`; tsc: `structuredTypeRelatedTo` in
  `tsc/internal/checker/relater.go`).
  A generic homomorphic mapped source without a name clause, whose type variable is
  constrained to arrays and tuples only, relates to a target without type parameters as the
  mapped type applied to that constraint (its apparent type), so `Hom<T>` with `T extends
  [number] | readonly [string]` is not an `any[]`; other generic mapped sources stay opaque
  (`mapped_over_tuple_constraint_relation`; tsc: `getResolvedApparentTypeOfMappedType` in
  `tsc/internal/checker/checker.go`, used through `getApparentType` in `relater.go`).
- **Element access in expressions.** `obj[key]` defers to `Obj[Key]` only for a generic key
  or a variadic tuple; a generic object with a concrete key reads its apparent properties
  (each member of a literal union key). A conditional key not assignable to `keyof Obj` is
  TS2536 (only conditional keys for now: `keyof` relations for other generic keys are not
  complete enough). The object of `x[k]` with a generic `k` is not a constraint position
  (no union-constraint substitution). A `never` key reads `never`. Intersections of
  primitive unions intersect as sets before distributing (`keyof` a union of tuples).
  A tuple indexed past its fixed elements at the type level is its rest element type, or
  `undefined` without one (`Parameters<F>[3]`); a type node doing that is TS2493.
- **Generic indexed access type nodes (TS2536).** A type node `O[K]` whose type stays a
  deferred indexed access is checked once (`check_generic_indexed_access_type`): each
  member of `K` must be assignable to `keyof O`, or be numeric-like when `O` has a number
  index signature, or name a property of `O`'s apparent type; else TS2536 printing the
  written `O` and `K` (tsc: checker.go, checkIndexedAccessIndexType; the private/protected
  TS4105 is not reported). Both sides are first read with the constraints their position
  implies (tsc: getConditionalFlowTypeOfType): in the true branch of `C extends E ? X : Y`
  a `C` is `E & C` (constraint first, as tsc prints its substitution types; unary tuples
  `[C] extends [E]` too; each parameter position on the way up flips variance, and a
  contravariant position only takes it for a type variable), `C["a"]` there is
  `C["a"] & E["a"]`. Parameter types of signatures written as types take it too, so in
  `number extends T ? (cb: (n: number) => void) => void : never` the `n` is `T & number`
  (two parameter positions: covariant). Such an `E & C` remembers `C` and `E` (a
  symbol-less alias on the intersection, `implied_intersection`): instantiated, it is `C`
  alone when `C` is no longer generic and satisfies `E`, as the conditional would pick its
  true branch then, so `(...args: T[K]) => void` with `T[K] = [a: A, b?: B]` is
  `(a: A, b?: B) => void`, not `(...args: unknown[] & [a: A, b?: B]) => void` (tsc:
  checker.go, instantiateTypeWorker, substitution types). The key `K` of a
  homomorphic `{ [K in keyof T]: X }` whose `T` is constrained to arrays or tuples is
  `K & (number | `${number}`)` inside `X`. An unconstrained `...infer R` in a rest position
  is indexed as `R & unknown[]`, and a conditional object as its constraint with such an
  `infer` read as `unknown[]`; that `unknown[]` is not made the declared constraint (it
  changes conditional inference: readonly tuples sliced into a rest no longer fit).
  Nodes inside the type arguments of `import("m").X<...>` are not checked: tsc's
  checkImportType resolves them but never walks them (tsc: checker.go, checkImportType).
  `S` relates to `keyof T` through `keyof C` for `T`'s constraint `C`, where a string index
  signature gives `string | number` (so `number` indexes a `T extends Record<string, X>`).
  `keyof (A & B)` with a generic member is `keyof A | keyof B` unless a member is `{}`
  (tsc: getIndexTypeEx/shouldDeferIndexType); `keyof S` relates to `keyof T` when `T`
  relates to `S`; a type parameter whose constraint is a union or intersection mentioning
  type parameters relates to `keyof X` through it; `O[K]` relates to `keyof X` through
  `O`'s constraint (an object `O`, or recursively an indexed access `O`), a generic key into
  a string index signature reading the signature's type (tsc: relater.go,
  structuredTypeRelatedToWorker; getConstraintFromIndexedAccess).
  When `O` itself is a mapped or conditional type (not a bare type variable) written in
  the true branch of conditionals over type parameters, each such check type `C` is
  replaced by its `E & C` inside `O` first (`mapped_under_implied_constraints`): under
  `T extends any[] ?`, `{ [K in keyof T]: K }[number]` and `V<T>[number]` for a
  distributive `V<X> = X extends unknown ? ... : never` are valid. A conditional `O` is
  keyed by its constraint, and a nested conditional in a branch is read through its own
  constraint (a few levels) when that ends at a rest-position `infer` array
  (type-fest's `VariablePartOfArray<T>[number]`, expect-type, array-flatten).
- **`arguments` in a function.** Inside a function other than an arrow, `arguments` that
  resolves to a declaration outside the innermost such function is that function's
  arguments object (`IArguments`), reads and assignment targets both (tsc:
  binder/nameresolver.go, function scopes answer `arguments` after their locals).
- **Discriminated unions.** An object source that relates to no member of a union target
  is tried per combination of its discriminant values (at most 25): every value must select
  some members by their discriminants, and the source must relate to each selected member
  on its other properties (`RelState::excluded_props` skips the discriminants). Errors
  elaborate against the members some source value selects (a union when several).
  `matching_discriminant_type` keeps members that lack the discriminant property.
- **Excess properties against unions.** A fresh object literal is checked for excess
  properties once, against the union (its non-generic members, discriminated); the members
  are then related without the check (`RelState::skip_excess_props`). With only object
  members, each property must also be assignable to the union of its types in the selected
  members (index signature types, `undefined` for a member without it), reported at the
  property (`{ foo: "x" }` against `StringKeys | NumberKeys`).
- **Variadic splits.** A target `[...A, ...B]` infers only once `A` has a fixed-length tuple
  candidate: `A` takes that many source elements and `B` the rest. Calls infer from a `this`
  receiver (through its base signature) before the rest arguments and again after them, so
  `f.bind(null, x)` is `(...rest) => R` with `f`'s remaining parameters. A contextual rest
  tuple is labelled with the contextual parameters' names (`[value: T, index: number]`).
  A rest parameter at or past the contextual rest position takes the rest type sliced at
  its position (`rest_type_at`): the tuple from the matching element on (`(a, ...x)`
  against `(...args: [A, B, C])` gives `x: [B, C]`), and past an array or generic rest
  an array of its element (`T[number][]`; tsc: relater.go, getRestTypeAtPosition;
  `contextual_rest_slice`). A rest element `...U[]` of a target tuple infers once, from the literal-reduced union of
  the source elements it covers (`[1, "a", true]` to `[T, ...U[]]` gives `U` the single
  candidate `string | boolean`; tsc: `getElementTypeOfSliceOfTupleType` in
  `inferFromObjectTypes`, `checker.ts`).
- **Generic signatures against generic signatures.** The source's type arguments are
  inferred from the target's parameter types, then (for those still open) from the return
  types, where a union inference that breaks the constraint keeps the members that meet it
  (`T | Promise<T>` for `<S extends string>` gives `T`); nothing inferred is `unknown`
  (`<S>() => S[]` against `<T>(x: T) => T` compares `unknown[]`). Identity alone pairs the
  type parameters by position (`generic_signature_inference`).
- **Conditional and intersection inference.** Conditional to conditional infers check,
  extends and branch types pairwise. Between intersections, shared members are dropped
  first; a union source infers to an intersection member by member unless the
  intersection has a naked type parameter, which takes the whole union.
- **Signature slots.** Inference pairs source and target signatures from the end; target
  signatures beyond the source's count take its first (a one-signature function fills every
  slot of expect-type's ten-overload matcher).
- **Missing keys.** Instantiating `T['k']` with an object type that lacks `k` is `unknown`
  (`undefined extends TOptions['transformer']` with `TOptions = {}`). A non-generic `O[K]`
  type node reports TS2339 (literal key), TS2537 (`string`/`number` without an index
  signature) or TS2538 (`any` without a string index, `boolean`, objects), per failing union
  member, except in the true branch of a conditional that checks `O`. A numeric string key
  reads a number index signature. Element access keys report TS2538 per failing union
  member too (a written bigint literal as `bigint`). A computed key without a literal name
  in an object binding pattern (`check_destructured_computed_key`) needs an index signature
  of the parent: TS2538 for `any` keys (and keys that failed to resolve) when it has none,
  TS2537 for `string`/`number` keys; untyped parameters of declarations, methods and
  functions called in place are skipped. Object assignment patterns (`({ [k]: x } = o)`)
  run the same check against the assigned value (`check_computed_key_read`); a primitive
  parent is named by its wrapper (`Type 'Number' has no matching index signature`).
- **Nullish.** `unknown` without `null`/`undefined` is `{}` (`u!`, `u ?? x`); a generic left
  side of `??` that may be nullish contributes `NonNullable<T>`. `Symbol.iterator in x`
  keeps only members with the property or a symbol index signature.
- **Array-or-nullish unions.** An object literal against `X[] | X | undefined` (or `null`)
  is not elaborated into its properties (tsc matches it with the first non-array member,
  and `null`/`undefined` sort first); the relation chain matches it with `X` and, for a
  fresh literal, drops the `Type '{...}' is not assignable to type 'X'` line. Object
  sources against unions of at most three members pick the best match without the nullish
  members.
- **Naked type parameters in unions.** A source member that infers only weakly into the
  other members of a union target (a conditional's branches, a homomorphic mapped type)
  still infers to the naked type parameter (`mergeDeep(1, 2)` with `T | DeepPartial<T>`
  gives `number`). A primitive infers nothing through a homomorphic mapped type.
- **Object literal supertypes.** Under the subtype relations a type with properties an
  object literal type lacks (other than `undefined` ones) is not its subtype, so the
  common supertype of `User` and `{ name: string }` is `User`.
- **Common supertype of candidates.** Under strictNullChecks the candidates are taken
  without their `null`/`undefined` members (added back to the result afterwards); literals
  of one base type form a union, else the leftmost candidate no later one strictly
  supersedes wins if all others are its strict subtypes, else the leftmost no later one
  supersedes (`B` and `D | undefined` give `B | undefined`; tsc: `getCommonSupertype` in
  `tsc/internal/checker/inference.go`; `common_supertype_nullable`).
- **Contextual properties of intersections.** A property's contextual type in an
  intersection of object types is the intersection of each constituent's: a generic mapped
  constituent's template for the key, else the property's type, else (only while no
  constituent has the property) its index signatures; an `any` contributes `unknown`, so
  `{ ml: any } & { ml: "edge" }` keeps `"edge"` literal. Tuple elements of an intersection
  are looked up the same way by index (tsc: `getTypeOfPropertyOfContextualType` in
  `tsc/internal/checker/checker.go`; `contextual_intersection_any_property`).
- **Spreads of unions.** `{ ...(X | {}) }` and `{ ...(X | undefined) }` spread `X`'s
  properties as optional (optional on both sides stays optional); other unions of object
  types distribute, and the parts are not normalized as object literals. Union identity
  is member-wise both ways, not by count. This holds for any union with exactly one member
  that is not nullish or an empty object, not only two-member ones.
- **Circular type aliases** (TS2456; tsc: `checker.go`, isDeferredTypeReferenceNode,
  isResolvedByTypeAlias and mayResolveTypeAlias, read as a reference). An alias is
  circular when building its body needs its own type at once. Built at once: alias
  references (and their type arguments), `typeof v` of an annotated variable (then its
  annotation, with nothing lazy but object literals, functions and mapped types), and
  the parts of unions, intersections, parentheses, type operators, indexed accesses,
  template literals, rest/optional elements, and a conditional's check and extends types.
  Built lazily: object literal, function and mapped types; an array, a tuple without a
  variadic `...T`, or a generic class/interface reference that is the alias's whole body
  (through parentheses and `readonly`), or that sits only inside the constructs above
  and has an element/type argument that may name an alias (or omits a defaulted type
  argument). So `type A = A[]`, `type B = [number, B]`, `type C = Array<C>` are fine and
  `type D = Box<D>` (an alias), `type E = [...E]`, `type F = typeof v; var v: F[]` are
  not. `circular_annotations.rs` walks the syntax for this; not yet followed: a
  conditional's branches when its check type is not generic, mapped type constraints,
  unannotated variables, functions' return types and interface members.
- **Rest parameter and rest element types** (tsc: `checker.go`, checkParameter and
  checkTupleType, read as a reference). A rest parameter's type must be assignable to
  `readonly any[]` (TS2370), also in function types, call/construct signatures and method
  signatures without a body. In a tuple type, `...X[]` (through parentheses or a lone
  `[...X[]]`) is a rest element by its syntax; any other `...T` is checked by its type: an
  array or a tuple with a rest is a rest, anything else that is nullable or not assignable
  to `readonly any[]` is TS2574, and the rest are neither. In a conditional's true branch
  the type is narrowed by the check first (`M extends readonly X[] ? [...M] : ..`, as for
  TS2344; @reduxjs/toolkit's `ExtractDispatchExtensions`). A second rest is TS1265, an
  optional element after a rest TS1266 (both grammar errors, skipped in files with syntax
  errors); the first error stops the walk. `infer X` in a rest position, and references to
  such an `X`, are implicitly array-constrained, so they are never checked.
  When relating `T` to `readonly any[]` runs out of instantiation depth (a recursive
  alias spread in its own branch, `[A, ...M<Tail<T>, A>]`), the element counts as an
  array and no TS2589 is reported: tsc's relation stops at deeply nested types with a
  maybe (tsc: `isDeeplyNestedType` in `relater.go`; type-fest's `MergeDeep`).
- **Object rest in an assignment pattern.** `({ a, ...r } = o)` assigns `o` without the
  named properties (`object_rest_type`) to a plain target `r`, so an incompatible target
  is TS2322 at `r`. Only a last rest with statically known names is typed.
- **Which spreads are valid** (tsc: `checker.go`, isValidSpreadType, read as a reference).
  Take each union member's base constraint (an unconstrained type parameter stays itself;
  an unresolved type is accepted), drop the members that are always falsy, then the spread
  is valid when what is left is `any`, `object`, an object type or a non-primitive
  instantiable type, or a union or intersection whose members all are. Otherwise TS2698 at
  the spread and the literal's type is the error type (`can_spread`). So `{ ...12 }`,
  `{ ...nullOrUndefined }` and `{ ...t }` with `T extends number` fail; `{ ...t }` with
  an unconstrained `T` is fine.
- **Overwritten properties** (tsc: `checker.go`, checkSpreadPropOverrides). Under
  `strictNullChecks`, a property written before a spread that the spread always sets is
  TS2783 at the written property: the name must be a required property of every union
  member of the spread, and no member may be nullish, primitive-like, `object`, an index
  type or an empty object (`report_spread_overwrites`).
- **What a spread copies** (tsc: `checker.go`, getSpreadType and isSpreadableProperty).
  Private and protected properties are not copied and remove the same name from the left
  side; `#names` are not copied; methods and accessors are skipped when the spread is a
  class instance or the member is declared in a class; a set-only accessor of an object
  literal copies as `undefined` (`spread_types`, `optional_spread_of`).
- **Constant element keys.** `o[key]` is a narrowable reference when `key` is a `const`,
  a parameter, or a `let`/`var` that is never assigned after its declaration; its flow
  key is the literal property name for a unit-typed key, otherwise the key's symbol.
- **Conditional sources.** A deferred conditional source relates through its constraint,
  then through its branches; generic branches relate through their base constraint
  (`Awaited<E>` with `E extends { a: 1 }` is not assignable to `never`).
- **Callback return contexts.** While a call infers, a defaulted type parameter that only
  a callback's return type mentions stays open in that callback's contextual type
  (`then(v => Promise.reject())` gives `Promise<never>`, not `Promise<T>`); optional
  callbacks (`f?: ((v) => R) | null`) count as callbacks.
- **Return literals.** Only a single unit return type widens (`return 1` gives `number`);
  a union of returns keeps its literals (`1 | "a"`, `1 | Promise<string>`). It is kept
  when the contextual return type takes it, a type parameter of a call being inferred
  read through what the call's contextual type implies (`const x: 0 | 1 = invoke(() =>
  1)` infers `T = 1`; tsc: `instantiateContextualType` with the return mapper;
  `return_context_literal_callback`).
- **Generic call arguments resolve once.** A generic call argument without
  context-sensitive functions inside keeps the contextual type it had while the outer call
  inferred (when that mentions the outer type parameters) for the final argument check, as
  tsc caches the first resolved signature of a call: `fn1(mk(Enum.A))` for `fn1<T>(a:
  Box<T>)` gives `mk` the context `Box<T>`, so `Enum.A` widens and `T = Enum` (tsc:
  `getResolvedSignature` in tsc/internal/checker/checker.go; parity
  `generic_call_arg_first_context`). Not modelled: tsc's outer return mapper is built once
  per outer call, at its first nested generic call, so a later argument's return context
  does not see earlier arguments' inferences (`baz(makeFoo(Enum.A), makeFoo(Enum.A))`).
- **Inherited constructors.** A class without a constructor takes its base's construct
  signatures instantiated with the `extends` clause's type arguments (`extends Repo<E>`),
  `any` only without them.
- **`keyof (X & T)` mapped types.** Inferring to `Readonly<X & T>` reverse-maps the source
  to `T` (weakly); other inferred parameters inside `X` get a silent `any`.
- **Non-null of nullish narrowing.** `x!` where `x` narrowed to only `null`/`undefined`
  (`x = undefined; x!.y`) reads the declared type (not through parentheses).
- **Array return contexts.** Inferring from a call's contextual type through a non-simple
  homomorphic mapped type (`Promise.all`'s `Awaited<T[P]>`), an array source reverse-maps
  to an array for `T` (its element inferred through the template, `any` when that fails),
  so an array literal argument whose parameter is a tuple-like constrained `T` is no tuple
  there (`(await Promise.all([a, b])) as Post[]`) and its elements see the element type
  (`const p: Promise<Player[]> = Promise.all([{ position: "GOALKEEPER" }])` keeps the
  literal). A type parameter the context implies is a tuple makes the array literal a
  tuple (`const r: OK<[string, number]> = ok(["a", 1])`; tsc: `instantiateContextualType`
  in tsc/internal/checker/checker.go, `inferReverseMappedType` in inference.go;
  parity `return_context_array_literal_args`).
- **Comparable unions.** A union source not comparable to a target elaborates against its
  last member.
- **Classes extending `any`.** A class whose `extends` expression is `any` inherits
  `[x: string]: any` (`C.prototype[k] = f`, `c.anything`).
- **Constant mapped templates.** Inferring to `{ [P in keyof T]?: X }` where `X` does not
  read `T` reverse-maps the source's keys to `unknown` (`{ k1: unknown; v: unknown }`,
  `[unknown]`); array literals in a generic homomorphic mapped context are tuples.
- **Definitely unrelated types.** Structural inference skips a source and target that each
  have a required property the other lacks (`Uuid` against `FindOperator<T>`), so the source
  still goes to a naked `T` in `T | FindOperator<T>`.
- **Widened arrays of object literals.** Widening an array whose element is an object
  literal makes the element regular, so an inferred `T` from `[{ assertId: 1 }]` is not
  checked for excess properties against `T`'s constraint.
- **Annotated expandos.** `f.x = v` declares an expando on an annotated `const f: Fn =
  () => ...` too: the variable keeps its annotation, the function initializer gets the
  properties (checked against `Fn`), and a property `Fn` declares is contextually typed by
  `Fn`'s member and has the type of its values widened as in a mutable location
  (`annotated_expando_values`): `f.done = true` under `done: true` stays `true`, and
  `f.done = false` is `false`, failing both the assignment and the annotation. Late-bound
  expandos (`f[sym] = v`, `f[k] = v` with `k` a literal) follow the same rule when `Fn`
  declares the key (tsc: checker.go, getAssignmentDeclarationInitializerType).
- **Generic signatures in context.** Two instantiations of one declared generic signature
  (a method read through `ZodNumber` and through `ZodType<unknown>`) first relate with their
  type parameters identified; failing that, and for any other generic source, type
  arguments are inferred from the target's parameters, then taken by position (when every
  target type parameter meets the source constraint), then inferred from the return types.
  A directly inferred target type parameter is checked against the source constraint
  instantiated with every inference (`<T extends I2<T>>`). Identity keeps positions.
- **Fully matched union sources.** Inferring `string` to `K | string | symbol` matches the
  `string` member and still gives `K` the source as a low-priority candidate (`K = string`).
- **Redeclared generic references.** `var y: IPromise2<..>; var y: Promise2<..>` differ when
  the identity check says so, even if they are mutually assignable.
- **Contextual `this` in object-literal methods.** A `this` parameter on the contextual
  signature (`{ [k: string]: { (this: C, ...): void } }`) types `this` before the literal.
- **Exhaustive switches in flow.** At a label joining other paths, the implicit `default` of a
  switch covering its discriminant adds nothing (definite assignment after it); as the only
  path it passes types through. Every case must be a unit type (`case a === b:` is not).
- **Synthetic default imports in type names.** `import net from 'net'` without a default
  export names the module in qualified type names (`net.Socket`).
- **Referenced outer type parameters.** A type literal or function type outside any block
  is instantiated only by the outer type parameters it names (`referenced_outer_type_params`);
  `typeof` queries, blocks, `this`, mapped and `infer` parameters keep all of them, and a
  parameter named in the extends type of an enclosing conditional type counts as named (its
  implied constraint can appear inside; tsc: isTypeParameterPossiblyReferenced). So
  `(data: { elements: El[] }) => R` doesn't mention `R` in its parameter.
- **`const` type parameters.** A literal whose contextual type (at its level or any
  enclosing array, spread or property level) is a `const` type parameter, a union or
  intersection with one, or an indexed access of one is in a const context
  (`const_type_param_context`): readonly tuples (mutable when the parameter's constraint is
  a mutable array), readonly properties, literal types. Parameter types mentioning one skip
  the return-context instantiation; a `const` rest parameter infers a readonly tuple. Tuples
  of const array literals combine as candidates like object literals
  (`const_array_literals`). The scan only runs when some file declares one.
- **Type-parameter modifiers** (`check_type_param_modifiers`, read from the text before the
  name, JSDoc `@template` included): the first offending keyword is reported: `const` on an
  interface's or type alias's parameter TS1277, `in`/`out` other than on a class, interface
  or type alias TS1274, any other modifier TS1273 (`type_param_modifiers`).
- **`Symbol()` initializers.** A global `Symbol()`/`Symbol.for()` call initializing a
  `const` or a `static readonly` property has that declaration's `unique symbol` type, so an
  annotation `unique symbol` accepts it. Any other unannotated variable whose initializer's
  type is a unique symbol is a `symbol` (`const B = A`), but only at the top level: a union
  such as `Awaited<T> | typeof A` keeps it (`unique_symbol_in_union_initializer`).
- **Intersection `this`.** The apparent type of an intersection with a class or interface
  member instantiates `this` as the whole intersection (`intersection_this`).
- **Merged overload order.** Same-named methods merged across interface declarations list
  later declarations' overloads first, each declaration's own overloads in source order
  (`merged_overload_order`).
- **Binding defaults.** Under an annotated declaration a default only removes `undefined`
  from the element type and must be assignable to it (TS2322 at the name, object literals
  elaborate); otherwise the element type is the declared type and the default, subtype
  reduced, literals kept in `const` (`binding_default_types`). An array pattern types array
  literals reached through conditionals and `||`/`??`/`&&` as tuples
  (`array_pattern_conditional_tuple`). An unannotated `var { e = 1 } = {}` reads `e` as
  missing from the pattern-typed literal, so its type is the default's (`number`), not an
  error (`pattern_default_empty_object_literal`).
- **Spread index signatures.** `{ ...h }` keeps `h`'s index signatures; members before or
  after the spread keep only those both sides have (`spread_index_signatures`).
- **Tuple rest sources.** Inferring `(x: X, ...args: A)` from `(...args: [X, Y])` (a bound
  method) slices the tuple: `A = [Y]` (`tuple_rest_inference_offset`).
- **Object literal indexing.** An object literal expression (or its `as const`) indexed by
  `string` or `number` is the union of its property types and `undefined`, no TS7053; a
  missing literal key is TS2339 (`object_literal_string_index`).
- **Inference variance.** Inference into a method signature's parameters records
  contravariant candidates as ordinary ones (methods are bivariant), and without
  strictFunctionTypes parameters infer covariantly. Instantiations of the same generic alias
  infer argument to argument (contravariant parameters flipped by measured variance) and
  match each other as union members (`method_parameter_bivariant_inference`).
- **Co- or contravariant inference.** With both kinds of candidates the covariant inference
  wins only when it is not `never` or `any`, is assignable to some contravariant candidate,
  and covers every covariant candidate of its own and of each type parameter whose
  constraint is exactly this one (`every<T, U extends T>` with `T`'s `Decorator | Modifier`
  array or `U`'s `Modifier` infers `T` from the callback's parameter). When the chosen
  inference breaks the constraint the other one is tried before the constraint itself
  (tsc: `getInferredType` in `tsc/internal/checker/inference.go`;
  `co_contra_inference_conflict`).
- **Higher-order function inference.** For a call whose declared return type has one
  non-generic call or construct signature, an argument with one generic signature whose
  contextual type has one non-generic signature of the same kind waits until the other
  context-free arguments are inferred, then (in argument order with the context-sensitive
  ones) infers into a fresh context: its parameters contravariantly to the contextual
  parameters, then its return type, with its own type parameters left free (renamed `T1`,
  `T2`, .. when a name is taken by one adopted earlier). When that context has inferences
  and no type parameter has inferences in both, they are adopted and the argument's type
  parameters become the type parameters of the returned signature (`pipe(list, box)` is
  `<T>(a: T) => { value: T[] }`); inferences from the call's contextual return type count
  as inferences here. Otherwise, when the parameter's contextual parameter types mention
  only type parameters with inferences, the argument is instantiated in that context (the
  inferences so far, then the return context's) and infers from the instance
  (`instantiateSignatureInContextOf`); else it infers normally. A contextual
  return type with one generic signature infers its own type parameters as plain types
  (tsc: `instantiateTypeWithSingleGenericCallSignature`, `getSignatureInstantiation` and
  `inferTypeArguments` in `tsc/internal/checker/checker.go`, `hasOverlappingInferences` in
  `inference.go`; `higher_order_generic_inference`).
- **Generic source signatures in context.** Inferring from a generic source signature to a
  non-generic target instantiates the source by inferring its type parameters from the
  target's parameter types; one that no source parameter (or `this`) mentions is its
  default, else its constraint, else `unknown` (tsc: `instantiateSignatureInContextOf`,
  `getInferredType`; `generic_arg_unmentioned_type_param`). Against a generic target the
  target's type parameters are erased to `any` and the source's (and the target's, in the
  source) are their base constraints: each constraint (or
  `unknown`) resolved through the others' once per further type parameter, a remaining
  (circular) mention as `any`; this covers a function expression that took its parameter
  types from the generic target (`f1(x => x)` with `cb: <S>(x: S) => T` infers `T =
  unknown`; tsc: `getBaseSignature`, `inferFromSignatures`;
  `generic_callback_base_constraints`).
- **Closures after the last assignment.** A function expression or arrow created after
  every assignment to a mutable variable (none in nested functions) keeps the narrowing at
  its creation (`closure_after_last_assignment`).
- **Destructuring assignments.** `[a, b] = v`, `({ k: a, b = d } = v)` and
  `for ([k, v] of xs)` narrow each target to its part of the value (defaults remove
  `undefined` and add the default; rest elements are not modelled); an array literal value
  of an array pattern is a tuple (`destructuring_assignment_narrowing`).
- **Discriminated intersections.** A source with union discriminants relates to a union of
  intersections of object types by combination, like a union of objects
  (`discriminated_intersection_members`).
- **Destructured references.** An object binding of an unannotated variable initialized
  by a reference (`const { a } = o`) reads `o.a` narrowed where it is declared
  (`destructured_reference_narrowing`). `o[k]` with a constant key of a literal union type
  narrows like a property (`union_key_element_narrowing`). `ref?.p === v` narrows `ref`
  unless `v` may be `undefined` (type parameters do not count;
  `optional_chain_generic_equality`). Optional chain arguments see the `?.` objects
  non-nullish (binder flow; `optional_chain_arguments`).
- **Untyped imports and type-only names.** A side-effect import (`import "pkg/mode"`) of
  untyped JavaScript is never TS7016 (`side_effect_import_untyped`). An unresolved one is
  TS2882; with `noUncheckedSideEffectImports: false` (default on in TS 7) side-effect imports
  are not checked at all (tsc: tsc/internal/checker/checker.go checkImportDeclaration). A relative import of a
  JavaScript file outside the program is TS7016 like a package's; the hint under it needs
  a package id (`Resolution::package`: a `package.json` with `name` and `version`;
  observed) and a bare specifier. It is, in order: TS6278 naming the declarations the
  resolver found ignoring `exports` (`Resolution::alternate`; the library is `@types/x`
  when they are there); TS7040 when some module resolution of the program entered the
  `@types` package; TS7058 (`declare module`) when one found a `.d.ts` in the package
  itself; else TS7035 (`untyped_package_hint`; tsc: tsc/internal/checker/utilities.go
  CreateModuleNotFoundChain, tsc/internal/compiler/program.go GetPackagesMap). An automatic JSX runtime (`react/jsx-runtime`) resolving to
  untyped JavaScript is TS7016 at the first JSX element (whole element), whatever
  `noImplicitAny` says (observed; `jsx_check.rs`). An imported type alias
  or interface used as a namespace (`CM.Editor`) falls back to a global namespace of that
  name (a UMD `export as namespace`), else TS2702 (`type_import_as_namespace`).
- **`require` of ES modules (node16/node18 only).** A module resolving to an ESM-format
  program file (not JSON) is TS1479 from a static import or re-export of a CommonJS-format
  file, TS1541/TS1542 from its type-only imports and `import()` types without a
  `resolution-mode` attribute, and TS1471 from `import x = require()` in any file. In a
  `.ts`/`.tsx`/`.js`/`.jsx` non-declaration file the chain adds the 1480-1483 hint, which reads
  the deciding `package.json` from `SourceFile::package_scope` (`require_of_esm_node16`).
- **`/// <reference types>` modes.** A types reference resolves in its file's mode (node16+:
  `require` conditions in a CommonJS-format file) unless it has a `resolution-mode`
  attribute (`SourceFile::references` kinds `types-import`/`types-require`;
  `types_reference_mode`).
- **Type-only aliases used as values.** A value identifier whose alias chain
  (`Program::alias_chain`) has a type-only link is TS1361 (nearest link an import) or TS1362
  (an export); the value type is still used. Not reported in declaration files, ambient
  declarations, type positions, `export =`/`export default` of the name, computed names of
  abstract members, when a link merges with a local value or the target merges across files
  (`type_only_value_use`). A named import or re-export of a name the module provides only
  through `export type *` is TS1362 too (`Program::star_export_is_type_only`: one plain
  `export *` of the name makes it a value). `import * as ns` of a module whose `export =`
  names a type-only alias follows that alias's chain too. A module namespace object's value
  properties (`value_exports`) leave out members whose alias chain has a type-only link and
  names the module gets only through `export type *`, unless the member also merges with a
  local value (`const A` next to `import type { A }`, even when the import is unresolved):
  `ns.A` is then TS2339 (tsc: checker.go getPropertyOfTypeEx skips non-value members when
  type-only meanings are excluded). Qualified type names (`ns.A` as a type) still use the
  module's exports.
- **`await` at the top level of a module** is reserved: declarations named `await` outside
  functions, namespaces and ambient declarations are TS1262, reported with the semantic
  diagnostics (tsc does not treat it as a syntax error; `top_level_await_names`).
- **Import attributes and `resolution-mode`** (`check_import_attributes`;
  `resolution_mode_attributes`, `resolution_mode_reference`): TS2823 when `module` has no
  attributes; under `node18`..`nodenext` in a CommonJS-output file TS2856; elsewhere TS2857
  on type-only forms and TS1454 for `resolution-mode` on other imports. A type-only form
  whose only attribute is a valid `resolution-mode` is exempt. TS1464 for `resolution-mode`
  with other keys, TS1453 for a bad value (type-only forms, `import()` types,
  `/// <reference types>`). All of these are grammar errors: none in a file with syntax
  errors (tsc: checker.go checkImportAttributes). Requests with a valid mode that differs from the file's resolve
  apart (`ProgramFiles::mode_resolutions`, looked up with `Checker::resolution`). JSDoc
  `@import` attributes are not modelled.
- **`import()` type attributes** (`check_import_type_mode`; tsc: checker.go
  getResolutionModeOverride): anything but exactly one attribute is TS1464 at the inner
  `{ ... }`; a single string-literal key other than `resolution-mode` is TS1463 at the key;
  a string value other than `import`/`require` TS1453. These are grammar errors: none in a
  file whose parser (in tsc) reported legacy `assert` attributes (`has_assert_attributes`).
  TS1340 from an `import()` type naming a non-module is kept in files with syntax errors.
- **Import attribute values and types** (`check_import_attributes`,
  `check_import_attributes_type`; tsc: checker.go checkImportAttributes,
  getTypeFromImportAttributes): every value that is not a string literal is TS2858 at the
  value, and then the declaration's import is not checked further. The attributes form an
  object literal of their regular literal types, which must be assignable (with
  elaboration) to the global `ImportAttributes | undefined`; declaration files stop there.
  An `import()` call's options are checked against `ImportCallOptions | undefined` (also
  their contextual type; checkImportCallExpression), spread options excepted. TS2353 quotes
  a property name that is not an identifier (`'"extra-key"'`).
- **`import()` of a CommonJS module under `node16`..`nodenext`** (`dynamic_import_type`;
  tsc: checker.go getTypeWithSyntheticDefaultImportType, canHaveSyntheticDefault): when
  the target may have a synthetic default (it is not ES-format; for other module kinds, a
  declaration file of a non-ESM package with no `default` or `__esModule` export) the
  promised type is the namespace spread into a fresh object followed by `default:` the
  namespace (`readonly` dropped, like tsc's spread).
- **Imports and exports directly in a namespace** (`check_namespace_module_statement`;
  `declare module "x"` and `declare global` are not namespaces): an import naming a module is
  TS1147 at its specifier, a re-export TS1194 at its specifier, and `export { a }` TS1194 in a
  non-ambient namespace only. Their requests are never resolved, so no TS2307
  (`request_in_namespace`). Not modelled: tsc's TS2307 when such an import alias is used
  (`namespace_module_statements`).
- **Module elements outside the top level** of a file or module block (in a block, function
  body, labeled or `if` statement; `check_misplaced_module_element`, at the statement):
  TS1235 namespace, TS1234 `declare module "x"`/`declare global` (the binder then skips
  TS2669), TS1231 `export =`, TS1258 `export default expr`, TS1233 export declaration (TS1474
  in JavaScript), TS1232 import (TS1473 in JavaScript). Modifiers on other declarations there are
  TS1184 (`modifiers.rs`). Their specifiers are not
  resolved or checked (`misplaced_module_elements`). A misplaced `export =`/`export default
  expr`'s expression is not checked either, nor one directly in a namespace body
  (`declare` or not; not `declare module "x"`), which is TS1063 / TS1319 at the statement
  (tsc: checker.go checkExportAssignment).
- **CommonJS names in JavaScript** (`Ck::unresolved_identifier`): an unresolved `require`
  that is the callee of `require("x")` is `any`, and so are `module`/`exports` in a CommonJS
  JavaScript file (`Ck::is_commonjs_js_file`: no top-level ES `import`/`export`, and
  somewhere `module.exports`, `exports.x`/`module.exports.x` assignments or a `require("x")`
  call; `exports.a.b = 0` does not count). Elsewhere they stay TS2591/TS2304. `exports` and
  `module.exports` are the `export=` value when there is one, and `require("m")` the
  module's type (see "CommonJS names in JavaScript" above; `commonjs_js_names`).
- **Legacy import assertions** are TS2880 at `assert` (`check_import_assertions`): on
  import/export declarations and `import()` types in every file (a syntax error in tsc, so
  the tsc CLI then hides semantic errors), and at an identifier `assert` key of an `import()`
  call's options object (`import_assertions_deprecated`, `import_assertions_dynamic`).
- **Re-exported `default`** (`export { default } from "./b"`) of a module without one is
  TS2305 unless a default can be synthesized, as for a default import (TS1192): synthetic
  defaults allowed and the module has `export =` or may have a synthetic default
  (`reexport_missing_default`). `import { default as d }` is a default import too
  (`synthetic_default_module` accepts the named `default`).
- **Namespace imports of callable `export =`** (`namespace_import_module_type`; tsc:
  checker.go resolveESModuleSymbol, getTypeWithSyntheticDefaultImportType,
  cloneTypeAsModuleType): `import * as ns` (not `export * as ns`) of a module whose
  `export =` value has call or construct signatures or a `default` property, or of a
  CommonJS file from an ES module under the Node kinds, is typed as the value's
  properties and index signatures without its signatures (so `ns()` is TS2349), plus
  `default: <value>` when the module can have a synthetic default
  (`namespace_import_synthetic_default`, no `allowSyntheticDefaultImports` gate: Node
  ESM-to-CJS always, ESM-to-ESM never, TS sources only with `export =`, declaration files
  unless the `default` (a property of the `export =` value's type) is declared as a default
  export, or `__esModule` is exported). The view prints `typeof X` for a class, enum or
  namespace entity (`ObjectFlags::MODULE` on the literal), its structure for a function.
- **A synthetic default overrides a real one** (`node_commonjs_default_module`, resolved in
  `resolve_alias`; tsc: checker.go getTargetOfModuleDefault, canHaveSyntheticDefault):
  under `node16`..`nodenext` a default import, `{ default }` import or `default` re-export
  in an ES module of a CommonJS-format file without `export =` is the whole module, even
  when the file has `export default`. A `{ default }` import or re-export of a TypeScript
  source with `export =` is the `export =` entity, as a default import is (not the entity's
  `default` member).
- **`"module.exports"` exports** (`module_exports_export`, resolved in `resolve_alias`;
  tsc: checker.go getTargetOfImportEqualsDeclaration, getTargetOfModuleDefault,
  resolveESModuleSymbol): under `node20` and `nodenext`, `import x = require("m")` of a
  module with `export { y as "module.exports" }` is that export (any formats), and so is a
  default, `{ default }` or namespace import in a CommonJS-format file of an ES-format
  module. A type-only export there makes value uses TS1362 (the export joins the alias
  chain), also through JavaScript's `const x = require("m")`
  (`check_type_only_require_use`).
- **`isolatedModules` / `verbatimModuleSyntax` aliases** (`isolated_modules.rs`; tsc:
  checker.go checkAliasSymbol, checkConstEnumAccess, getSymbol): `verbatimModuleSyntax`
  turns `isolatedModules` on (`CheckOptions::isolated_modules`), but "explicit
  isolatedModules" checks (TS2865 import/local value conflicts in `import_conflicts.rs`,
  TS2748 on uses through an alias) read `isolated_modules_explicit`. The flag name in
  messages is `verbatimModuleSyntax` when that is on. Rules for non-type-only,
  non-ambient aliases of TypeScript files are listed in the module doc (TS1484, TS1485,
  TS1288, TS1269, TS1205, TS1448, TS1295/TS1286, TS2748). An ambient const enum used as a
  value is TS2748 under explicit `isolatedModules`, or under `verbatimModuleSyntax` when the
  use is not through an import (the import itself already reported it). A value lookup
  skips an alias whose target has only type or namespace meanings (`alias_lacks_meaning`),
  so `import { Date } from "./types"` where `Date` is an interface still lets `new Date()`
  reach the global value; under explicit `isolatedModules` that use is TS2866 at the import
  (`check_global_shadowed_by_type_import`; tsc: checker.go resolveNameHelper).
  `export default X` / `export = X` (`check_isolated_export_assignment`; tsc: checker.go
  checkExportAssignment): `export default` in a CommonJS-emitted file under
  `verbatimModuleSyntax` is TS1295/TS1286 and nothing more; else `X` must be a value
  (TS1282/TS1284) not reached through a type-only declaration unless merged with a local
  value (TS1283/TS1285), and under `isolatedModules` an imported type or a type-only
  declaration of another file must be marked type-only first (TS1291/TS1292,
  TS1289/TS1290). Also under `verbatimModuleSyntax` in CommonJS-emitted files: `export` on a
  top-level non-ambient declaration other than a type alias, interface or namespace is
  TS1287 at `export` (a modifier grammar error, `modifiers.rs`; tsc: grammarchecks.go
  checkGrammarModifiers), on a namespace with values too
  (`check_verbatim_commonjs_namespaces`), and with `module: commonjs` every `import()` is
  TS1295/TS1286 (grammar; tsc: grammarchecks.go checkGrammarImportCallExpression).
  Global scripts merge across files, which a transpiler cannot see: under `isolatedModules`
  an instantiated non-ambient namespace of a script (const enums count, they are preserved)
  is TS1280 at each part of its name (tsc: checker.go checkModuleDeclaration), and a bare
  reference in a non-ambient enum to a member declared by another file's declaration of
  the same enum is TS1281 (tsc: binder/nameresolver.go). Such references resolve through
  the merged enum in every mode (`lookup_in_merged_namespaces`). `isolatedModules` also
  makes a const enum read before its declaration TS2450, like a regular enum (tsc:
  checker.go checkResolvedBlockScopedVariable). With `emitDecoratorMetadata` and ES module
  emit (not under `verbatimModuleSyntax`), the type names decorator metadata serializes for
  a decorated node (constructor parameters of a class; parameters and return type of a
  method; an accessor's annotation or its pair's; a property's type; a decorated
  parameter's whole signature; one name shared by a union, skipping `never`) must not
  resolve to a non-type-only import that has no value: TS1272 at the type name (tsc:
  checker.go markDecoratorAliasReferenced; `check_decorator_metadata_imports`).
- **`type` specifiers in a type-only list** (`import type { type a }`, `export type { type a
  }`) are TS2206/TS2207 at the first such specifier's `type` keyword, a grammar error (clean
  files only; `check_type_modifier_in_type_only_list`; tsc: grammarchecks.go
  checkGrammarTypeOnlyNamedImportsOrExports). `import type A, { B }` (or `, * as ns`) is
  TS1363 at `type` instead (grammarchecks.go checkGrammarImportClause).
- **Type-only `import x = ...`.** `import type X = N.Y` whose entity resolves is TS1392 at
  the statement (grammar, clean files). For any `import x = a.b.c`, the whole name and then
  each left part is looked up without resolving its last alias; the first one whose alias
  chain has a type-only link is TS1380 (an import) / TS1379 (an `export type` or `export
  type *`) at the whole entity. An `import A = a.A` re-exported plainly does not carry its
  entity's type-only-ness on (tsc: checker.go checkImportEqualsDeclaration,
  checkAndReportErrorForResolvingImportAliasToTypeOnlySymbol;
  `check_import_equals_type_only_ref`).
- **`export *` collisions** are TS2308 at each later `export *` of a module (or namespace)
  that brings a name an earlier one brought for a different entity (after resolving aliases),
  naming the first one's specifier as written; not when the module exports the name itself,
  nor for `default` (`check_export_star_collisions`; tsc: checker.go
  getExportsOfModuleWorker).
- **Default imports without a default** are TS1192, or TS2613 ("did you mean `import { x }`")
  when the module's own export table (not `export *`) holds the local's name (`has_own_export`;
  tsc: tsc/internal/checker/checker.go reportNonDefaultExport). Under `node16`..`nodenext` an
  ES module importing an ES module (`.d.mts`, `"type": "module"`) gets no synthetic default,
  not even from an `export =` (`esm_imports_esm`; canHaveSyntheticDefault).
- **Named imports of an `export =` module** (`import { a } from "m"`, `export { a } from
  "m"`) find the entity's exports, or, for a value entity, its value's own properties:
  `Object`'s and `Function`'s members do not count, a union needs the property in every
  member (`has_own_property`; tsc: tsc/internal/checker/checker.go getExternalModuleMember).
  An interface, function or class without such a member is TS2305. Importing the entity by
  its own local name is TS2595 (ES module output), TS2597 (JavaScript) or TS2616
  (`import x = require`; reportNonExportedMember). `export * from "m"` of such a module is
  TS2498 at the specifier (`check_export_star_of_export_equals`).
- **`export =` with other exports** is TS2309 at each `export =` of a module file or
  `declare module "x"` body whose own export table (not `export *`) holds a value: a value
  symbol, or an alias resolving to a value or to nothing. Types, interfaces, uninstantiated
  namespaces and `export {}` do not count. JavaScript `module.exports` is not modelled
  (`export_assignment_conflicts`).
- **`export =` in ES module output** is TS1203 (`check_esm_export_assignment`): under
  `es2015`..`esnext` in any file but `.cts`/`.cjs` (of declaration files only `.d.mts`), under
  `node16`..`nodenext` in ESM-format files including declaration files; never under
  `commonjs`/`preserve` (`esm_export_assignment`, `esm_export_assignment_node`).
  A top-level `import x = require()` (not type-only) is TS1202 under `es2015`..`esnext` in
  every non-declaration file, `.cts` included; `node16`..`nodenext` allow it
  (`esm_import_assignment`).
- **Names reserved by CommonJS emit** (`emits_commonjs`: `commonjs` but `.mts`/`.mjs`, or a
  CommonJS-format file under `node16`..`nodenext`; `commonjs_generated_names`): a class
  named `Object` (declaration or expression, any depth, script files too, not ambient) is
  TS2725 with the `module` name. Only when emitting (not `noEmit`), in module files:
  top-level non-ambient functions, classes, variables and imports named `require`/`exports`
  are TS2441, and an exported variable `__esModule` is TS1216.
- **`<T>() => ...` in `.mts`/`.cts`**: an arrow whose only type parameter has no constraint
  and no trailing comma is TS7060 at the parameter (`reserved_arrow_type_params`).
- **Explicit extensions** (`needs_explicit_extension`, `extension_suggestion`;
  `explicit_extensions`): under `moduleResolution` `node16`/`nodenext`, an unresolved
  relative specifier with no `.` in its last name, resolved in ESM mode (static imports of
  ESM files, `import()` anywhere), is TS2835 with a suggestion when a non-declaration program
  file matches with an extension added (`.mts`/`.mjs` first, then `.ts`/`.tsx` (`.jsx` under
  `jsx: preserve`)/`.js`/`.jsx`, then `.cts`/`.cjs`), else TS2834; side-effect imports too.
  `./` and `../` stay TS2307. Requests resolve in their own mode: one in another mode than
  its specifier's first request in the file (`import x = require("./")` after
  `import "./"` in an ES module) is kept in `ProgramFiles::mode_resolutions`
  (`program::request_mode`).
- **TypeScript extensions in specifiers** (`check_ts_extension_import`;
  `Resolution::ts_extension` is the resolver's `resolved_using_ts_extension`). When the
  extension written in the specifier is what found a program file (`./a.ts` → `a.ts`), an
  emitted import (declaration with a value clause, value `export ... from`, `import x =
  require()`, `import()`; not side-effect imports, import types or JS `require`) needs
  `allowImportingTsExtensions` or `rewriteRelativeImportExtensions` (either allows it), or a
  declaration-file importer: else TS5097 naming the extension. A `.d.ts` specifier is TS2846
  instead. Under `rewriteRelativeImportExtensions` (outside ambient code, type-only imports,
  import types and `.d.ts` specifiers): a relative TS-extension specifier the extension did
  not find (`./foo.ts` → `./foo.ts/index.ts`) is TS2876 with the importer-relative real path;
  a non-relative one it did find (`#internal/foo.ts`), to an emitted file, is TS2877
  (tsc: tsc/internal/checker/checker.go resolveExternalModule, tsc/internal/ast/utilities.go
  IsEmittableImport, tsc/internal/core/core.go ShouldRewriteModuleSpecifier).
- **Arbitrary-extension declarations** (`Resolution::arbitrary_extension`,
  `program::needs_arbitrary_extensions`): `./x.html` resolving to `x.d.html.ts` from a
  non-declaration file without `allowArbitraryExtensions` is TS6263 and the module is not
  linked (the import is untyped; the file is not loaded through it) (tsc:
  tsc/internal/module/util.go GetResolutionDiagnostic).
- **Imports of scripts** (`check_script_import`): `import x = require("./s")` and
  `export * from "./s"` of a program file that is not a module (no `import`/`export`, not a
  JavaScript CommonJS module, not JSON) are TS2306 at the specifier, and the alias resolves to
  nothing (`Program::is_script_target` in the binder), so uses of `x` are not checked further
  (tsc: tsc/internal/checker/checker.go resolveExternalModule).
- **CommonJS output under `node16`..`nodenext`** (`builds_into_node_commonjs`: a
  non-declaration module whose format is not ESM): a top-level `await` is TS1309 at the
  keyword, but only when its operand starts with a word or string literal (tsc 7 skips
  `await (x)`, `await [x]` and other punctuation), and a top-level `for await` always is;
  every `import.meta` is TS1470 (`commonjs_top_level_await`).
- **Emit helpers (`importHelpers`, `emit_helpers.rs`).** Every non-declaration module file
  imports `tslib` implicitly: `load_program` resolves it before the file's own requests
  (`helpers_specifier`, mirrored by `ts_r_history::module_requests*`), so a found `tslib`
  joins the program. Syntax the emit lowers with a helper (async functions and generators,
  `for await`, object rest, private names below ES2022, decorators, `using`, CommonJS
  import/export interop) requests it, in source order. No `tslib` (ambient or resolved) is
  TS2354 once per file at the first request; a missing export is TS2343 once per helper per
  file; old `__classPrivateFieldGet`/`Set` with too few parameters are TS2807. The module doc
  lists the requests and their error positions (`emit_helpers_no_tslib`,
  `emit_helpers_old_tslib`, `import_helpers_missing`).
- **Unmodelled JSX contexts.** Implicit-any parameters are not reported where a JSX
  attribute's props are not modelled (once the props are resolved and are not `any` or
  generic, an attribute they lack has no contextual type and TS7006 is reported;
  `jsx_context_modelled`), also through conditionals, `||`/`??`, the right of
  `&&`, array literals and context-less object literals, nor in calls on a parameter that
  is itself in such a context (`item.tags.map(tag => ..)`; `jsx_unmodelled_context_params`).
- **Intersections of arrays** iterate and destructure to the intersection of their element
  types (`array_intersection_elements`).
- **Discriminants must be discriminants.** Truthiness, equality and `typeof` of `ref.p`
  narrow `ref` only when `p` is a discriminant of a union in the type at that point
  (`{ timer: undefined }` is left alone): the members that have `p` give it differing
  types, one of them a literal type (a unit type or a union of units only), so an optional
  method present in one member (`() => void | undefined`) is no discriminant
  (`is_discriminant_property`, `discriminant_needs_literal_non_uniform`; tsc: relater.go
  isDiscriminantProperty, checker.go createUnionOrIntersectionProperty). The false branch of `instanceof C` keeps object
  types that are not class instances (an `Error` interface structurally equal to `C`;
  `non_union_property_truthiness`).
- **IIFE spread arguments.** A spread tuple fills one parameter per element, an array or
  `any` spread one parameter of its element type, a missing one is `undefined`; a rest
  parameter takes the rest as a tuple (`iife_spread_arguments`).
- **Signature grammar.** `readonly T` needs an array or tuple type there (TS1354); a
  parameter property cannot be a binding pattern (TS1187, at the parameter); parameters of
  signatures in types (function and constructor types, interface and type-literal
  members) without a type are TS7006/TS7019 under `noImplicitAny`, `this` included
  (`signature_param_grammar`). An untyped binding-pattern parameter there reports TS7031
  on each name without a default instead (`m([, a], {p, q: {r}})`: a, p, r; tsc:
  checker.go, implicit-any reporting for binding elements).
- **JSON modules** (`resolveJsonModule`): `.json` resolutions are part of the program,
  bound as modules, never checked; the module's type is its value widened like a `let`
  initializer, and a default import is that value (`resolve_json_module_types`). Under the
  Node module kinds the module is `{ default: value }` (an ES module's namespace import),
  a default import is the value, and a CommonJS file's namespace import is the value's
  properties plus `default: value` (`json_import_type`). JSON syntax errors come from the
  parser (ast-requests R9).
- **Generic constraint arguments.** A conditional's constraint (and its branches against
  `keyof X`) reads a true branch that is the check type as `check & extends`
  (`Extract<K, keyof O>`); a type parameter constrained to an indexed access relates to
  `keyof X` through it; a generic intersection relates to a union through its constraint
  (`O & N`); ambient `#private` members are untyped by design (no TS7008)
  (`generic_constraint_args`, `ambient_private_name_members`).
- **Template `infer` placeholders.** With nothing inferred, an `infer X` placeholder is
  `string`; a source that cannot match a pattern made of placeholders only (`string`
  against `${infer F}${infer R}`) infers `never` for each, so recursion over `string`
  stops (`template_infer_placeholders`). An unconstrained placeholder's constraint is
  `string` (`implied_infer_constraint`), so `{ [K in X | keyof O]: .. }` takes it as a key
  and `Box<X>` in the true branch is checked against `Box`'s constraint (TS2344); other
  `infer` parameters are still not checked as type arguments
  (tsc: `getInferredTypeParameterConstraint` in `tsc/internal/checker/checker.go`;
  `template_infer_string_constraint`, hono's `ExtractParams`).
- **Chain order.** Pending property levels (and the head they deferred) are flushed before
  "Types of parameters 'a' and 'b' are incompatible" is added, so they nest below it.
- **Elaboration paths.** Pending levels flush outermost first. A property level starts a path
  that takes in the following properties and, only right after a property, return levels; the
  heads between them are dropped. One property prints "Types of property 'x'", a longer path
  "The types of a.b" (or "The types returned by a().b" when the second step is a return). A
  return level outside a path prints nothing; a suppressed head that owned a level prints
  just above it. A class's `prototype` is never a path step. A strict function type's
  callback parameter is explained by its signatures without a head line (not inside callback
  mode, and not recursively) (`relation_elaboration_paths`).
- **Identity.** Every `any` is identical to every other `any`. Signatures are identical only
  with as many type parameters (identical constraints), parameters and required arguments.
  TS2403 skips types printing `any` unless both declarations are annotated or the function
  shapes differ, since inference can give `any` where tsc does not (`redeclaration_identity_order`).
- **Pairs in progress.** A pair the current top-level query is already comparing counts as
  related before any cached result (the reporting pass re-runs a pair whose failure is
  cached; its members must still see it as in progress). Pairs from an enclosing query are
  consulted only after the cache (`recursive_relation_reporting`).
- **Relation results that assumed a pair in progress** (`recursive_related`). A success
  that met a pair on the stack below its own frame ("maybe related") is not cached at once:
  it waits on `RelState::pending` (looked up by `pending_set`) and is cached when the frame
  it assumed settles as a success, or dropped when that frame fails. Hitting the deeply
  nested instantiation limit assumes the root pair and taints the result (`Assumed::nested`):
  only the outermost result is cached. Hitting the depth limit, a spent budget or a variance
  measurement in progress makes the result unknown: never cached. A failure never depends on
  an assumption and is always cached. Variance measurements keep their own pending list
  (saved and restored around the measurement) and cache into their `Probe`. Successes
  computed while relating to an intersection target's members (`in_target_member`, which
  defers excess and weak type checks) go to `permissive_cache` with the `deferred_skipped`
  they set, never to the shared cache; pending entries are keyed by that state too.
- **Union cache** (`unions::UnionCache`, `Ck::union_cache`, per worker, kept across
  files). A literal or plain union of at least 4 written members maps the member list (with
  the reduction and alias) to its result, so re-unioning the same members skips the sort and
  dedupe. Subtype reduction and lists with a forward-declared alias member are not cached.
  The result is a pure function of the key, so the cache does not change output.
- **Discriminating caches** (`Ck::discriminants`, per worker, kept across files): for a
  union contextual type, the sorted optional property names of its members and, per
  property name, each member's property type (or that the name cannot discriminate).
  `discriminate_union_by` reads them for every object literal property it types.
- **Error anchors.** A source with call (else construct) signatures, one of whose
  return types (not `any` or `never`) fits the target, is reported at the expression
  (tsc's "Did you mean to call this expression?"), not at the declaration name; object
  literal methods stay at their name; arguments use `elaborate_literal_error` (no hint). TS2558 spans the
  type arguments, not the brackets. A type parameter source reports its constraints one at
  a time (`error_anchors`).
- **`globalThis`.** A script's top-level `this` is `typeof globalThis`, in JavaScript too (a CommonJS
  module is a script for this: tsc 7 gives it no external-module indicator). A missing name read
  or written through it is `any` (TS7017 under noImplicitAny), except a global `let`/`const`,
  which is a missing property (TS2339) (`global_this_access`).
- **Predicates, `#private` and `?` parameters in messages.** A type predicate mismatch
  heads its chain with TS1226 over TS2518 (`this` against a parameter), TS1227 (another
  parameter position) or the predicate types' elaboration. Two classes' `#x` members give
  TS18015 naming the declaring classes, unless one class derives from the other: through
  every base (`class_derives_from`), so a class extending a global interface's constructor
  derives from each class that interface extends, one per merged declaration. The base
  matches by declaration as well as by symbol: an imported `export default class B` is typed
  through its export symbol (`default`), its declaration binds a separate local `B`, and the
  export symbol prints as the declaration names it except after a module path
  (`import("./b").default`; `default_export_class_base`, @nodelib/fs.walk). A function or arrow expression's `?` parameter
  prints as annotated (`x?: string`; declarations and methods add `| undefined`)
  (`predicates_private_names_optional_params`). Its parameters in general print their
  annotation as written (`x: 'bye'`, `Array<T>`, aliases) while the signature is not
  instantiated, unless the annotation names an enclosing declaration's type parameter
  (`written_expression_params`).
- **Written constraints.** A declared type parameter's constraint prints from its node
  (`T extends Array<Base>`, aliases, union members in source order) when the node is made
  of references, keywords, arrays, unions, intersections and parentheses; an instantiated
  copy written `Array<U>` prints `Array<X>`. Union (strict subtype) reduction does not take
  a primitive as a subtype of `Object`; the subtype relation of inference does
  (`written_constraints_object_subtype`).
- **Pattern initializers.** An object literal (parenthesized or `as const` too)
  initializing an unannotated object binding pattern, a variable's or a parameter
  default, gets only the excess property check against the pattern's type (TS2353 for each
  excess property, through nested object and array patterns; none with a rest element; see
  "Literals typed by a pattern"). Computed pattern keys of literal type name their property
  (`pattern_literal_excess`). An annotated empty pattern (`{}`, `[]` or holes only) under
  strictNullChecks doesn't relate its initializer to the annotation: `const {}: undefined = 1`
  is only TS2532 for the nullish annotation (tsc: `checkVariableLikeDeclaration` in
  `tsc/internal/checker/checker.go`).
- **Object literal `this`.** Without a contextual type (or `ThisType` marker), `this` in an
  object literal's method or function-valued property is the literal's own widened type,
  under noImplicitThis in TS files (a cycle through the literal gives the silent
  `UNRESOLVED`; tsc's TS7023 there is not reported) (`object_literal_this`).
- **Unconstrained type parameters.** Under strictNullChecks the apparent type of a type
  parameter without a constraint is `unknown`, so it has no members (`t.toString()` is
  TS2339 on `T`; `t["k"]` is TS7053 against `unknown`). Past an optional chain link it reads
  as `NonNullable<T>` (`T & {}`), which has `Object`'s members. Without strictNullChecks it
  is still `{}`. A call with the wrong number of explicit type arguments (TS2558) checks its
  arguments against the first signature instantiated with the written type arguments
  (`unconstrained_type_param_members`).
- **Write types of intersections and union keys.** A property of an intersection with a
  setter in any member is written as the intersection of the members' write types (setter
  parameter, else property type). `o[k] = v` with a union of literal keys on a non-generic
  object checks `v` against the intersection of each property's write type
  (`intersection_write_types`).
- **Intersection targets defer nested checks.** While a source is related to each member
  of an intersection target (`RelState::in_target_member`, kept through property
  relations, cleared for the members of a source intersection and by every top-level
  `is_type_related`), excess property and weak type checks don't fail; they set
  `deferred_skipped`. When all members passed and a check was deferred, the source's
  properties are related to the intersection's combined properties with the checks in
  force, so the failure reads `Types of property 'x' are incompatible` directly under the
  head (no member line), and `{ x: { b } }` is assignable to `{ x: { a? } } & { x: { b? } }`.
  In that state only cached successes are read and successes are not cached
  (`intersection_target_deferred_checks`).
- **Weak type message.** A weak type failure is TS2560 ("Did you mean to call it?") when
  the return type of the source's first call signature, or else of its first construct
  signature, is related to the target; TS2559 otherwise (tsc: `structuredTypeRelatedTo` in
  `tsc/internal/checker/relater.go`).
- **`keyof` origins.** `keyof A` of a class, interface, reference or aliased type is the
  union of its keys with the index type `keyof A` as its origin, so it prints as `keyof A`
  (and `keyof A | undefined` in a union). An origin item that is not a member (this one) is
  never mapped or filtered: `map_type` and `filter_type` drop such origins (`keyof_origin`).
- **Literal keys to `keyof T`.** A string literal source (or a union of them) inferred to
  `keyof T` records `{ k1: any; k2: any }` in `InferenceContext::literal_keyof`; `T` takes
  the intersection of those only when it has no other candidate. The receiver of `o?.m()`
  infers a `this` parameter without `null` and `undefined` (`literal_keyof_inference`).
- **Tuple rest element reads.** A literal index at or past a tuple's rest element (no
  variadic elements) reads the union of the rest element type and the elements after it,
  plus `undefined` under noUncheckedIndexedAccess when the index reaches the number of
  fixed elements (`tuple_rest_element_access`). Writes keep the index signature.
- **Destructuring assignments.** `[a, b] = v`, `({ a, b: c } = v)` and `for (target of xs)`
  relate each target to the part of the source it receives (`check_destructuring_assignment`:
  `array_element_at`, `destructured_property_type`; a default removes `undefined` under
  strictNullChecks), TS2322 at the target. When the source is a reference each part is
  narrowed as `v[0]` / `v.a` at that point (`narrowed_path_type`). Rest elements, missing
  properties (tsc's TS2339 there is not reported) and out-of-range tuple elements are only
  checked as targets. An assignment target that is not an identifier or a property access
  (through parentheses and assertions) is TS2364, after the operand checks of a compound
  arithmetic assignment; not for `super` or in files with syntax errors
  (`destructuring_assignment_checks`).
- **Generator yield contexts.** In an annotated generator a bare `yield` relates
  `undefined` to the yield type (TS2322 at the `yield`). The operand of `yield* x` is
  contextually typed as `Generator<Y, R, N>` (also `AsyncGenerator<...>` in an async
  generator) with the generator's yield and next types and `R` the context of the `yield*`
  itself (silent `never` without one), so `const n: number = yield* g()` infers `g`'s
  return type. A `yield*` evaluates to the `value` of its iterator's `done: true` results
  (`undefined` for arrays and strings under strictBuiltinIteratorReturn; `any` when no
  result says `done: true`; `yield_star_result_type`; tsc: checker.go,
  getContextualTypeForYieldOperand, checkYieldExpression). An unannotated immediately invoked generator takes its yield and return
  types from its call's context (`generator_context`). `in_generator` looks through a class's
  heritage clauses and computed member names, not its member bodies
  (`generator_yield_context`).
- **Generator inference.** An unannotated generator's return type is `Generator<Y, R, N>`
  (`AsyncGenerator` when async): `Y` is the union of its yields (`undefined` for a bare
  `yield`, a `yield*` operand's iterated type; `never` without yields), `R` comes from its
  returns as for other functions (`void` without any), both widened like returns. `N` is
  the intersection of what resumes it: each `yield*` operand's next type (a known iterator's
  third argument, else its `next` parameter, else `unknown`) and each contextually typed
  `yield` result; without those, the contextual iterator's next type, else `unknown`
  (`infer_generator_type`). A used `yield` result with no contextual type in a generator
  with neither annotation nor contextual iterator type is TS7057 under noImplicitAny (not for
  `yield*`, discarded results, binding-pattern initializers or `import()` specifiers)
  (`generator_inference`).
- **Generator annotations.** A generator's annotation must accept `Generator<Y, R, N>` (or
  `AsyncGenerator`) built from the annotation's own iteration types: those of its iterator
  members (`generator_context_full`), else `Y` from iterating it with `any`/`any`, else
  `any`/`any`/`unknown`; an async generator gets none from a sync `Iterable`. TS2322/TS2741
  at the annotation; `void` is TS2505; skipped in files with syntax errors
  (`check_generator_annotation`). In annotated generators, `yield` operands (awaited when
  async), bare `yield`s (`undefined`) and what `yield*` operands iterate relate to the yield
  type (`generator_annotation_checks`).
- **Iterability.** for-of operands (after the TS18047/TS18046 nullish check), array and
  argument spreads, `yield*` operands (TS2504 in async generators) and values destructured by
  array patterns (declarations, parameters, assignments, for-of targets; at the pattern) must
  iterate, else TS2488 (`check_iterable`, or `check_iterable_value` for spreads and binding
  patterns; see Iterables above). `any` and error types pass; `unknown` doesn't;
  spreading `never` into an array literal is allowed. A generic member iterates by its
  constraint without nullish members (unconstrained can't). Intersections whose required
  unit-typed properties conflict count as `never` (`reduce_conflicting_intersections`). The
  `[Symbol.iterator]` method must take no arguments. A parameter pattern destructures its
  default when unannotated, else its type without the `undefined` optionality or a default
  adds (`iterable_checks`).
- **Iterable printing.** `Iterable`, `AsyncIterable`, `IterableIterator` and
  `AsyncIterableIterator` print without trailing `any` arguments (`Iterable<number>`);
  `Iterator`, `Generator` and other types with defaults print all of theirs.
- **Failed overloaded calls.** When no signature of an overloaded, non-generic set fits, the
  call's type is the intersection of their return types (`never` for `number & string`); a
  single signature's failure keeps its return type. A union property with a setter in any
  member is written as the union of each member's write type (`overload_failure_results`).
- **Message type names.** TS2367 shows literal operands widened when their base types are
  unrelated (`number` and `string`, but `"a"` and `"b"`) (`equality_operand_widening`). A
  private member's type is the declaring class, or the relation's interface type when that
  interface has members of its own (`private_member_type_names`).
- **Alias-named references.** A generic class or interface reference, array or tuple written
  as a type alias's body is a separate store type carrying the alias
  (`TypeStore::reference_aliased`), printed by the alias (`type NB = Box<number>` prints `NB`);
  a tuple that spreads a type not written as an array does not take it. Relations compare the
  plain reference (`without_reference_alias`): a missing property below names the plain
  reference and leaves the head; a type parameter's constraint, base types and `this`
  instantiations drop the alias too (`alias_reference_names`, `alias_reference_relations`).
- **Readonly array heads.** TS4104 replaces the head of the relation it explains (also an
  argument's TS2345), naming an alias source by its alias (`readonly_array_head`). `delete`
  through a readonly index signature is TS2542 (`delete_readonly_index`).
- **Logical operators.** `t || u` with a generic `t` that may be nullish is
  `U | NonNullable<T>`; `&&`'s left operand has no contextual type
  (`logical_operator_generics`). A tuple's minimum length counts its required elements, and
  an optional source element for a required target one is TS2623 (`tuple_arity_messages`).
- **Tagged templates.** Resolved like calls (`call::Args::Tagged`): the effective arguments are
  a `TemplateStringsArray` (`Arg::Elem` at the template) and each substitution, contextually
  typed as argument `1 + i`; an unterminated template accepts too few arguments. A
  constructable tag is TS2349 (no TS2348, no untyped `Function` call); two templates in an
  array literal are TS2796; a `super` tag is not resolved (`tagged_template_calls`).
- **`new` of a callable.** A callee with call signatures but no construct signatures is
  resolved against its call signatures, so `new f()` gets the same arity and argument
  errors as `f()` (JS functions, `@constructor` ones too, are never constructable in tsc 7).
  Without `noImplicitAny` a declared signature that doesn't return `void` is TS2350 and a
  `this: void` one TS2679; with it the call is TS7009. The result is `any`, or the return
  type of a signature without a declaration (`call::new_of_callable`; tsc:
  tsc/internal/checker/checker.go, resolveNewExpression and checkCallExpression).
- **Identity.** Identical object types have as many properties (`identity_property_count`).
- **Spread arguments.** A spread of unknown length fits a signature when it starts at or past
  the required parameters and before the last parameter or into a rest one (else TS2556);
  each spread is then checked by position, against an optional parameter's type with
  `undefined`. An IIFE's parameters past its written arguments (a spread counts as one) are
  optional; a spread tuple with a trailing rest element types its parameters
  (`spread_argument_arity`). Against `X | undefined`, a single source (or `boolean`) is
  reported against `X`, a union source keeps the whole target (`nullable_target_heads`).
- **Compound-like widening.** A mutable variable whose literal initializer type survives
  `widen_literal` (a non-fresh literal) gets the literal's base type when the file assigns it
  compound-like: `x op= y`, or `x = a op b` with an arithmetic, shift or bitwise operator
  (`compound_assigned_vars`, cached per file; `compound_like_widening`).
- **Spread and static field contexts.** An object spread's contextual type is its object
  literal's; an array spread has none (tsc's `getContextualType` has no spread-element
  case), so `[...['y']]` against `XY[]` spreads a `string[]`; a static field of a class expression without an annotation takes the property
  of the class's contextual type (`spread_and_static_field_context`).
- **Reverse mapped inference.** `T` behind a homomorphic `{ [K in keyof T]: X }` whose
  template reads `T[K]` is inferred (weakly) as the source's keys, each typed by what its
  value infers for `T[K]` against `X` (union of candidates, else the intersection of
  contravariant ones, else `unknown`); arrays and tuples map element-wise. Optionality is
  kept unless the mapped type adds `?`, readonly unless it adds `readonly`. The template's
  other type parameters then infer from the source against the mapped type with `T`
  reverse-mapped (`any` sources excluded). A recursive mapped type meeting the same source
  again (`reverse_mapping` stack), and a primitive inside a reverse mapping, give a silent
  `any`. A generic mapped type over a `const` type parameter is a const context, and a
  literal returned into a `const` type parameter's indexed access stays literal
  (`reverse_mapped_inference`).
- **Enum relation by members** (`enum_types_related`, cached per enum pair in
  `Ck::enum_relation`; tsc: `isEnumTypeRelatedTo` and `computeEnumMemberValue` in
  `tsc/internal/checker/relater.go` and `checker.go`). Two different enums are related only
  when both are regular (non-`const`) enums of the same name and every source member exists
  in the target with the same value. A value nobody knows (a computed member, or a member
  without initializer in an ambient non-`const` enum) counts as numeric: it only clashes
  with a string. Enum types, two enums' literal unions, and two enum literals of equal
  value all go through this rule; a failure reports the first reason under the head
  (`Property 'd' is missing in type 'First.E'`, TS4125, TS4126) and the union is not
  elaborated member by member (`enum_relation_by_members`).
- **Enum literal narrowing.** `x !== "A"` removes the enum member whose value is `"A"`
  (`enum_literal_narrowing_this_marker`). It also removes an intersection holding that
  unit as a member (a branded `"right" & { right: "right" }`, `T & null`; tsc:
  `narrowTypeByEquality` with `isUnitLikeType` in `flow.go`;
  `narrow_not_equal_branded_unit`).
- **ThisType in unions.** A `ThisType<X>` marker in a union contextual type (an optional
  parameter's `Opts | undefined`) types `this`.
- **Mixin constructors.** A construct signature with one rest parameter of type `any[]` or
  `any` is a mixin constructor.
- **Spread freshness.** An object literal with spreads is fresh (`fresh_spread_literal`,
  `spread_literals`), but only the properties written in it are excess, not spread ones.
- **Array literal elaboration.** Elements are typed in the target element's context and
  stay fresh (their excess properties are errors); a union with one array-like member (or
  number index signature) elaborates against it. A spread whose argument is itself an
  array literal is tupleized too, so its elements are elaborated one by one; a literal
  that tupleizes to a plain array (a lone rest such as `[...xs]`) is not elaborated
  (`spread_not_contextual_mapped_intersection_index`; tsc: `elaborateArrayLiteral` in
  `tsc/internal/checker/relater.go`).
- **Did you mean (TS2820).** A string literal against a union with string literal members
  (default head only) suggests the closest by spelling (`string_literal_suggestion`;
  `spread_freshness_and_literal_suggestions`).
- **Property elaboration.** When a property of a non-fresh object is elaborated, a literal
  value is reported widened unless the target property's type has literals of its kind, it is
  in a const context, or it is a boolean literal (`elaborate_object`;
  `property_elaboration_widening`).
- **Argument elaboration context.** A failed argument is elaborated with the parameter pushed
  as its context override (`call.rs` `elaborate_argument`): the call is still resolving, so
  re-checked nested values would otherwise lose their contextual type
  (`argument_elaboration_context`).
- **Pattern source elaboration.** A failed non-generic string mapping source elaborates
  through `string` (against a mapping of its own kind through the mapped types, against
  another kind not at all); a template literal source through itself with its mapping holes
  widened to `string`, unless the target is a pattern too (`report_pattern_source_constraint`;
  `pattern_source_elaboration`).
- **Computed names.** A late-bound property (`[c0]`, an entity name key) is named as declared
  in relation messages (`written_name`); a computed key of a string literal type prints quoted
  even when numeric (`{ "1": number; }`, `declared_quoted_name`). A template expression in an
  element access index or a computed property name keeps its literal / template type
  (`is_template_literal_context`), and a pattern key reads the index signatures it fits,
  intersected (`pattern_key_index_info`) (`late_bound_property_names`,
  `computed_key_literals`, `pattern_key_index_access`).
- **Lone rest tuples.** `create_tuple` returns the array for a tuple of one rest element
  (`[...number[]]` is `number[]`); an intersection with an array member and a numeric
  literal `length` is a tuple context (`number[] & { length: 2 }`) (`lone_rest_tuples`).
- **Plain interface extensions.** A non-generic interface without members of its own and with
  one base relates as that base against another instantiation of the base's generic type, so
  the type arguments are elaborated (`plain_extension_base`;
  `plain_interface_extension_relation`).
- **Spread elements.** An array literal with spreads elaborates when its own tuple type
  (`tupleized_array_literal`) is a tuple: each position compares that tuple's type there
  (`tuple_position_type`), then reports the element's own type (a spread's iterated type) when
  that fails too, else the position type. Holes stop it. Tuple targets elaborate only their
  leading fixed elements (`spread_element_elaboration`).
- **Tuple contexts from the end.** An array literal element after every spread (or in a
  literal against a tuple with a rest) takes the tuple's trailing fixed elements counting from
  the end (`contextual_element_type`; `contextual_tuple_elements_from_end`).
- **Open tuple suffixes.** An open-ended source tuple relates its suffix to the target's when
  its suffix is at least as long and its rest starts no earlier; other shapes stay permissive.
  Several source positions landing in a target rest are reported as a range (TS2627)
  (`open_tuple_suffix_relation`).
- **Type parameter receivers.** The apparent type of a type parameter constrained to a class
  or interface (or an intersection with one) binds `this` to the type parameter
  (`x.self()` is `T`) (`type_parameter_this_receiver`).
- **Contextual unions with any.** Per-member contextual element and property types drop an
  `any` member when others exist, so literals stay literal (`contextual_union`;
  `contextual_union_with_any`).
- **Enum unions and top-level returns.** `base_of_literal` of a union of one enum's members is
  that enum (so `E.A | E.B` and `E.C` candidates combine); intersection members of a return
  type are at its top level (`NonNullable<T>` keeps `T`'s literal) (`enum_union_inference_top_level`).
- **Type-argument candidates first.** Candidates inferred through the type arguments of two
  instantiations of one generic (same reference, same alias, array element, tuple to tuple) are
  `InferenceContext::preferred` and lead the candidate list, so the common supertype falls back to
  them: `f<T>(a: T, b: Box<T>)` with `(string, Box<number>)` infers `number`; a supertype candidate
  still wins, and `{ v: T }` or a base-interface match is not preferred (`type_argument_inference_priority`).
- **Excess properties under comparability.** The comparable relation checks fresh object
  literals' excess properties too (`c === { id, name }` has no overlap with `C`; a fresh case
  clause reports `'name' does not exist in type 'C'` through `check_type_related`). A deferred
  intersection-member check also relates the intersection's combined index signatures
  (`{ [x: string]: A } & { [x: string]: B }` to `A & B`). `deeply_nested` matches a type with
  no recursion identity only with itself, so a union target is not counted as every other
  unidentified target. A reference whose type arguments contain an object literal (the
  type of an array literal of object literals) has no shared identity either, so nested
  array literals are never cut off as deeply nested and their inner TS2741 still reports
  (`array_literal_nesting_not_deep`; tsc: `getRecursionIdentity` in
  `tsc/internal/checker/relater.go`). `===`/`!==` with an unparenthesized object, array, function, class or
  regex literal operand reports TS2839 (`excess_property_comparable_nested`,
  `equality_with_new_object`); so do `==`/`!=`, except in JavaScript. An equality with the
  global `NaN` (parentheses skipped, either side; a local `NaN` does not count) reports TS2845
  at the binary expression, 'false' for `==`/`===` and 'true' for `!=`/`!==`, with a related
  TS1369 "Did you mean '[!]Number.isNaN(x)'?" at the other operand (`...` when it is not an
  entity name), dropped when both sides are `NaN` (`nan_equality`; tsc:
  checker/checker.go, checkNaNEquality).
- **Limited reverse mapping.** `{ [K in keyof T & L]: T[K] }` (also `keyof T & ('a' | 'b')`)
  infers `T` from the source's properties whose keys are in `L` (`keyof_param_limit`,
  `constrained_reverse_source`); an object literal's literals stay at the top where `T`'s
  constraint has that primitive and deeper where the constraint's property type takes literals
  (`keep_constrained_literals`). A generic mapped type's contextual property is its template
  (`T["x"]`), and a union base constraint contributes by member. In the inference mapper a
  parameter with nothing inferred takes its default over the fixed ones, and a parameter's own
  inference is fixed while its constraint is instantiated. A generic mapped type over
  `keyof T & L` is not assignable to `T`, and prints its intersection constraint as written
  (`reverse_mapped_limited_keys`).
- **Super calls.** `super(...)` resolves against the base constructor's signatures
  instantiated with the `extends` clause's type arguments (`super_signatures_for_type_args`);
  `super<T>(...)` reports TS2754 and is checked without them. (tsc treats TS2754 like a syntax
  error and then reports no semantic errors in the program; not modelled: ours still reports
  them.) (`super_call_base_type_args`, `super_call_type_args`)
- **Union property accessibility.** A union has a property whose member properties include a
  private or protected one only when they all share a declaration (`union_origins_accessible`):
  `Public | Protected` has no `member`, `Foo | (Foo & Bar)` has `foo`. Indexed access types
  on unions check their keys like those on objects (TS2339) (`union_property_accessibility`).
- **Missing properties of object literal union members.** A union member that is an object
  literal type (not built by a spread, `ObjectFlags::CONTAINS_SPREAD`) and lacks a property
  reads it as `undefined`: `(o || {}).color` is `string | undefined`, not TS2339. The union
  property carries `WRITE_PARTIAL`, and a write does not find it (TS2339 / TS7053). An element
  write first widens its object: a union with an empty object member is subtype-reduced, so
  `(o || {})["color"] = 1` reports on `{}` (`union_object_literal_missing_prop`; tsc:
  checker.go, createUnionOrIntersectionProperty and checkPropertyAccessExpressionOrQualifiedName
  widening an assignment target).
- **Assignment pattern properties.** An object assignment pattern's property that the source
  lacks is TS2339 at its key (`check_destructured_assignment_property`). There is no error
  when the property has a default and the source is a fresh literal, or when a union source
  has an object-literal member (`options || {}`): tsc types those literals from the pattern.
  `[...{ a }] = t` checks the nested pattern against the rest of the source
  (`array_rest_type`). A nested array pattern is only checked against tuple sources. In
  `for (var { x = 0 } of [2, 3])`, the source counts as literal when an element of an
  array-literal right side is a literal. A const enum's object has no `Object` members
  (`destructuring_assignment_properties`, `const_enum_object_members`).
- **Generator expressions without yields.** A contextually typed generator expression with
  no `yield` and no annotated parameter infers no return type (unresolved). tsc takes its
  yield type's arguments from return-type inferences, which are not modelled
  (contextualParamTypeVsNestedReturnTypeInference3/4). Others infer `Generator<Y, R, N>`
  from their yields, returns and context.
- **noCheck.** With `noCheck`, no file is semantically checked (`driver.rs` `should_check`).
- **Read-only writes (TS2540).** Checked for assignments, `++`/`--` operands and `for-in/of`
  expression targets. Exports read through a namespace import are read-only; a class's
  `prototype` is not. In a constructor, only arrows invoked on the spot may assign `this`'s
  read-only properties (`readonly_writes`).
- **Recursive and class expression names.** A function type inside itself prints
  `typeof f`; other types met again while printed print `...` (`() => ...`), or `any` under
  noErrorTruncation, which the conformance harness sets by default as tsc's does. Printing
  nests up to 100 deep (`merge<merge<...>>` chains print in full) and visits at most 100k
  types per outermost print, then elides the same way; a class expression is named after the variable it initializes, the identifier
  it is assigned to, or its object literal key as written, else `(Anonymous class)`.
- **Class base type argument count.** A class without its own constructor inherits only the
  base construct signatures whose type parameter count accepts the `extends` clause's type
  argument count (trailing defaults optional); when none does, the class has no construct
  signatures. A class base reports it like a type reference (TS2314/TS2707/TS2315); any other
  base gets TS2508 at the `extends` expression (not in JS files).
- **`any` keys.** Indexing a type without index signatures by an `any` key is implicitly `any`
  under noImplicitAny (TS7053, no chain, not for a `for…in` key), shown as the apparent type
  (`{}` for `object`, `unknown` for an unconstrained type parameter). An element access
  with no applicable index (TS7053, TS7015, with or without noImplicitAny) is the error type,
  so calling it with type arguments gives no TS2347; any element of `never` is `never`,
  quietly.
- **TS2347 on auto-typed variables.** A call with type arguments of a variable with no type
  or initializer skips TS2347 only when that variable is auto-typed (noImplicitAny or JS,
  not ambient, not exported); `declare var a;` and `var a;` without noImplicitAny are plain
  `any` and get TS2347 (tsc: `getWidenedTypeForVariableLikeDeclaration`).
- **Rest tuple arguments** (`rest_tuple_contexts`). A rest parameter typed as a tuple with a
  rest element (`...args: [number, ...string[], boolean]`): its leading elements are
  parameters of their own; the arguments after them form one tuple, related to the tuple
  from the rest element on and reported at the first of them (TS2345 with the tuple's
  position elaboration). Each argument's context comes from the end for the trailing
  elements; it keeps its literal type where that context has a primitive, else widens.
- **Instantiated merged members.** An instantiated property symbol (a mapper and one origin)
  instantiates its origin's type even when the origin is a member merged across files
  (`MERGED_MEMBER`: a method overloaded in two global augmentations).
- **Conditional key constraints.** `A extends Exclude<keyof T, "x">` is assignable to
  `keyof T` (through the conditional's branches).
- **First context of function arguments.** A function argument of a generic call with no
  unannotated parameters anywhere inside is typed by the contextual type it had during
  inference (`first_arg_context`), in the applicability check and the final check too: in
  `cus(() => attempt(api), p => {})` the inner call sees `Promise<R>` with `R` still open.
- **`any` index signatures.** With a string index on the target, any index signature of type
  `any` accepts every non-primitive source (not under the strict subtype relation). A string
  index of `any` the source lacks also accepts any object, again except under the strict
  subtype relation, so a flow join subtype-reducing `Record<string, any> | any[]` keeps both.
  The `for (k in o) o[k]` number-index rule (`has_numeric_property_names`) needs a number
  signature as the only one; a union has the keys every member declares exactly, so
  `any[] | Record<string, any>` has none and reads by `string` (TS7053)
  (`for_in_numeric_union_strict_subtype`; tsc: relater.go indexSignaturesRelatedTo,
  typeRelatedToIndexInfo; checker.go getUnionIndexInfos, hasNumericPropertyNames).
- **Copied method type parameters.** When a signature is instantiated by an outer mapper,
  copies of its own type parameters whose constraints mention them get constraints over the
  copies (`instantiate_signature`), so explicit type arguments are checked against the
  constraint instantiated with them. A default naming a sibling (`<A = unknown, B = A>`)
  names the copy too, so an uninferred `B` of `cache.build({})` becomes what `A` became
  rather than the original `A`. The copies are interned by the outer mapper with
  aliases dropped from the references it maps to (`mapper_without_reference_aliases`): the
  members of `type A = B<any>` (whose `this` is `A`) and of `B<any>` share them, because
  contextual typing reads a method through the alias and the relation through the plain
  reference. `sig_inst_cache` is keyed by the outer mapper, not the extended one.
- **Contextual generic signatures fix their type parameters.** Return-context inference
  (`mentions_free_type_params`) treats the type parameters of the generic signature that
  contextually types an enclosing function (`const f: <R>() => R = () => make()`) like
  those of enclosing declarations, unless an outer call is inferring them: an uninferred
  `R` of `make` becomes the outer `R`, not its default.
- **Quick initializer types.** An unannotated variable initialized by a call to a callee
  with exactly one non-generic call signature (no properties, index or construct
  signatures; not an optional chain, `super` or JavaScript) takes that signature's return
  type without checking the arguments (`quick_call_type`), as tsc does. A callback argument
  whose body reaches back to the enclosing function is then no circularity; generic or
  overloaded callees still check the call. `await f()` takes the awaited return type the
  same way, so `let y = await f(x); x = y + 1` in a loop is no cycle
  (`await_quick_call_type_loop`; tsc: checker.go getQuickTypeOfExpression).
- **`BuiltinIteratorReturn`.** The lib's parameterless `intrinsic` alias is `undefined`
  under `strictBuiltinIteratorReturn` (part of `strict`), else `any`
  (`alias_declared_type`); any other parameterless intrinsic stays unresolved.
- **Spreads of unions with intersection members** distribute like unions of object types
  (`{ ...(A | ({ id } & B)) }` spreads each member); before, the intersection member made
  the spread unresolved. Property order and the printed optional property types of such
  spreads still differ from tsc in messages.
- **Postfix operands.** A readonly array or tuple before `[]`, `?` or `[K]` prints in
  parentheses (`(readonly [...T])[]`); in an intersection or after `keyof` it does not.
- **Instantiation expressions** (`f<string>`, `typeof f<string>`;
  `instantiation_expression_type`). Each generic call and construct signature whose type
  parameter count fits the arguments is instantiated with them, and the others are dropped. Properties and index
  signatures stay. Unions and intersections map over their members, and a type parameter maps over its constraint.
  Arguments that break a signature's constraints get TS2344, and that signature stays generic.
  TS2635 goes at the first argument. It names the whole type when nothing applies, otherwise the
  first union member with signatures but none applicable. A type-level query whose type changed
  prints as written (`typeof f<string>`, `instantiation_query_names`), so
  `ReturnType<typeof make<T>>` aliases resolve. These names are published before the type
  escapes, in the type's `Layer`, so cached symbol types keep their names across workers
  and checkers sharing a level.
- **Arithmetic with a `null`/`undefined` operand** (`check_arithmetic`). When one operand
  is literally `null` or `undefined` (TS18050, its type becomes the error type), the other
  operand is still checked against `number | bigint`: TS2362 on the left, TS2363 on the
  right. The result stays the error type (tsc: checker.go, the arithmetic branch of the
  binary-expression check, which checks both operands before giving up). An operand that
  is already the error type (an unresolved name) passes like `any`, and the other operand
  is still checked: `Missing ** []` is TS2363, `true * Missing` TS2362, and the result is
  `number` (`bigint` next to an all-bigint operand; `arithmetic_unresolved_operand`).
- **UMD globals read in a module** (`check_umd_global_value`). In a module file, an
  expression identifier whose resolved global consists only of `export as namespace`
  contributions (every part is a file symbol) is TS2686, unless `allowUmdGlobalAccess`
  is set (tsc: checker.go, name resolution for a value meaning in an external module).
  Type and namespace uses (`Mod.T`) are fine; a `declare global { const N }` of the same
  name is a value of its own; `export = N` names its entity with every meaning, so it is
  never checked (`umd_global_value_in_module`).
- **`instanceof` an instantiation expression.** `x instanceof C<T>` (parentheses skipped)
  is TS2848 at `C<T>`; the left side may be one (tsc: checker.go, the expression with type
  arguments check, which looks up through parentheses for an `instanceof` right side;
  `instanceof_instantiation_expr`).
- **`as const` operands** (`is_valid_const_assertion_operand`). `x as const` and
  `<const>x` need a literal (string, template, number, bigint, boolean, array, object,
  `-1`/`+1`/`-1n`) through parentheses, or a property/element access whose object is an
  entity name resolving (through aliases) to an enum; anything else is TS1355 at the
  operand (tsc: checker.go, the const assertion argument check; `const_assertion_operand`).
- **Unassignable `for...in`/`for...of` heads.** An identifier head naming a constant,
  function, class, enum, namespace or import reports only TS2588/TS2628-TS2632 and nothing
  about the key or element type (tsc: checker.go, the identifier check gives an error type
  for an assignment target that can't be written; `for_head_constant_target`).
- **`constructor` as a parameter property.** `constructor(public constructor: T)` in a
  constructor implementation is TS2398 at the name (tsc: checker.go, the parameter check;
  `constructor_parameter_property_name`).
- **Bigint below the target** (`check_arithmetic`, the `BigIntLit` arm of
  `check_expr_inner`). `**`/`**=` with two bigint-like operands is TS2791 at the whole
  expression when the target is below ES2016 (tsc: checker.go, the arithmetic branch when
  both operands are bigint-like). A bigint literal in a value position is TS2737 when the
  target is below ES2020, except in ambient code, in files with syntax errors, and in type
  positions (`type T = 2n`, `-3n` in a literal type are types, not expressions; tsc:
  checker/grammarchecks.go, the bigint-literal grammar check; `bigint_target_checks`).
- **`super.field`** (`check_property_accessibility`). Reading an instance property of the
  base class through `super` when every declaration is a plain field (not an `accessor`)
  is TS2855 at the name instead of any private/protected error; methods and accessors
  stay readable (tsc: checker.go, property accessibility at a location).
- **Private methods and setter-only private accessors** (`check_private_method_write`,
  `check_private_setter_read`, `write_kind`). `write_kind` walks parents from an access to
  classify it: a plain write (`=`, a destructuring target through array/object literals,
  spreads, properties and parentheses, a `for-in`/`for-of` head), a compound write
  (`op=`, `++`, `--`), or a read. Writing to `#m` where the innermost class declaring `#m`
  has it as a method is TS2803 at the name. Reading (including the read half of a compound
  write) a private accessor with a setter and no getter is TS2806 at the whole access
  (tsc: checker.go, property access checks for private identifiers).
- **Protected access through the wrong instance** (`check_protected_instance`). Inside class
  `C`, `obj.p` where `p` is a protected instance member reached from `C` or a class `C`
  derives from must have `obj` typed as `C` or a subclass of `C` (a type parameter is read
  through its constraint); otherwise TS2446 names `p`, `C` and the object type. `super.p`,
  statics, `#names`, and object types that are not class/interface references are skipped,
  and so is anything with an intersection (mixin) base on either side, whose combined
  modifiers are not modelled (tsc: checker.go, property accessibility and the
  enclosing-class base type walk).
- **Symbol operands** (`maybe_symbol_like`, `symbol_operand_error`). A type is symbol-like
  when it, a union or intersection member, or the base constraint of a type parameter (also
  one in a union: `S | string` with `S extends symbol`) is `symbol` or a unique symbol
  (`symbol_operand_union_constraint`). When `+` (or `+=`) has a valid result, and for `<`, `<=`, `>`,
  `>=`, the first symbol-like operand gets TS2469 naming the operator (`+=` for the compound
  form, which then skips the write check); comparisons report it before the null checks.
  Unary `+`, `-` and `~` report TS2469 on a symbol-like operand. A `${...}` span in an
  untagged template whose type is symbol-like is TS2731; tagged templates are exempt
  (tsc: checker.go, the ES symbol operand check in binary and prefix-unary checks, and the
  template expression check).
- **Template constants come first.** A template whose parts evaluate to constants is that
  string literal type before the const-context / template-literal-context rules apply; a
  tagged template is never evaluated (tsc: checker.go, template expression check).
- **delete** of a readonly property (including an enum member) is TS2704 at the access; the
  TS2703 "must be a property reference" error points at the operand with parentheses
  stripped (tsc: checker.go, delete expression check).
- **Dynamic import specifier.** `import(x)` where `x` (not a spread, not an error type) may be
  `null`/`undefined` or is not assignable to `string` is TS7036 naming the type
  (tsc: checker.go, import call check).
- **Enum initializers** (`check_enum_initializer`). A const enum member evaluating to NaN is
  TS2478, to an infinity TS2477. An initializer that names an enum member declared later
  in the same file (`Y`, `E.Z`, `E["Z"]`, also a member of a later block of the merged
  enum) is TS2651 whatever its value, and nothing else is checked. An initializer with no
  constant value in a const enum is TS2474, and in a non-ambient regular enum it must be assignable to `number`
  (TS2322 headed by TS18033). The evaluator reads an unannotated `const` with an initializer
  declared earlier in the same file (not from inside its own initializer), follows import
  aliases, takes earlier members of the enum being evaluated through `E.a` / `E["a"]` /
  ``E[`a`]`` (tsc: checker.go, enum member constant computation and entity evaluation).
  A member without an initializer after one with no numeric value (a string, a computed
  value, or an auto member that is itself TS1061) is TS1061 at its name; a forward
  reference counts as 0 and ambient non-const enums have no auto values
  (`check_enum_auto_members`, `enum_member_needs_initializer`). With `isolatedModules`
  (`enum_init_origin`): a string value whose initializer is not a string by syntax (a
  string literal or template, `+` with one, through parentheses, same-file untyped
  constants and enum members) is TS18055 naming `E.m`; an auto member after an
  initializer that reads an untyped constant of another file is TS18056
  (`enum_isolated_modules_values`).
- **super() position** (`check_super_call_position` in `super_calls.rs`). Without native
  class fields, a derived class with an initialized instance property, any `#private` member
  or a parameter property: the first `super(...)` call in source order (functions skipped)
  must be an expression statement (parentheses allowed) directly in the constructor body,
  else TS2401 at the call; the body statements before it must not mention `this`/`super`
  outside functions and property declarations, else TS2376 at the constructor. For TS17009,
  a nested class's heritage clause and decorators on its members or parameters use the
  constructor's `this`; a parenthesized `(super())` statement counts as a call statement
  (tsc: checker.go, constructor declaration check and the `this` container walk).
- **Private names shared by static and instance members** (`check_private_name_static_clash`).
  Once a class has `#x` both as a static and as an instance member (any kind), every member
  named `#x` gets TS2804 at its name (tsc: checker.go, duplicate declarations of class
  members).
- **Static names of Function's own properties** (`check_static_function_name_conflicts`). In a
  non-ambient class, a static property, method or accessor named `prototype` (also through
  a computed key with a literal value) is TS2699 at the name with the class display name;
  `name`, `length`, `caller` and `arguments` are TS2699 too unless `useDefineForClassFields`
  is in effect. A static method or accessor whose computed key late-binds to `prototype`
  is also TS2300 with the key text, since it collides with the class's `prototype` symbol
  (tsc: checker.go, static property name conflicts and late-bound member binding).
- **Where `await using` may stand** (`check_await_using_context`, statements and `for`-`of`
  heads). It follows the rules for a top-level `await`, at the `await` keyword: TS2852 in a
  function that is not async; at the top level TS2853 when the file is not a module, and
  then TS1309 for a file node16..nodenext emit as CommonJS, or TS2854 when `module` is not
  es2022/esnext/system/node16+/preserve or `target` is below es2017. Nothing in an ambient
  context (TS1546 covers it) or in a file with syntax errors (tsc: grammar.go, grammar
  checks for `await` and `await using`).
- **`using` initializers must be disposable** (`check_disposable_initializer`). The widened
  initializer type of a `using x = e` must be assignable to `Disposable | null | undefined`
  (TS2850 at `e`), of an `await using` to `AsyncDisposable | Disposable | null | undefined`
  (TS2851); nothing when the lib lacks those interfaces. As for the default head, a single
  missing property between the same two types replaces the head (without
  strictNullChecks `using x = {}` is TS2741), while a union target keeps it (tsc:
  checker.go, variable-like declaration checks; relater.go, relation error reporting).
- **Parameter shapes** (`check_param_shape`, every parameter, signatures in types too). An
  optional binding-pattern parameter without a default in a function that has a body is
  TS2463. A rest parameter named by an identifier must have a type assignable to
  `readonly any[]`, else TS2370 at the parameter; generic types are skipped. Under
  strictNullChecks an optional rest parameter is `T | undefined` when annotated, and when
  unannotated only in a function expression, arrow or object-literal method (their
  parameters are typed with the expression, which adds the optionality; a declaration's
  unannotated rest parameter stays `any[]`: destructuringParameterDeclaration4 vs
  parserParameterList11) (tsc: checker.go, parameter checks and parameter type
  assignment).
- **Type parameters in a member's computed name** (static_refs.rs). A class's or
  interface's own type parameter is out of scope inside the computed name of one of its
  members (`class C<T> { [foo<T>()]() {} }`): the reference is TS2467 and the error type,
  never TS2304. A computed name of a nested type literal or class is not covered (tsc:
  binder/nameresolver.go, the computed property name step of the scope walk).
- **`instanceof` operands** (expr.rs `has_instance_method`). When the right side is an
  object type with a callable `[Symbol.hasInstance]` (classes and functions get
  `Function`'s from the ES2015 lib), the expression is the call
  `r[Symbol.hasInstance](l)`: with one non-generic signature, `l` must be assignable to
  its first parameter (TS2860 at `l`, elaborated), and its return type to `boolean`
  (TS2861 at `r`). Only without such a method is a right side that is neither callable,
  constructable nor a `Function` TS2359 (tsc: checker.go, instanceof resolution; flow.go
  for the property lookup).
- `new f()` on a type with call signatures but no construct signatures: with
  `noImplicitAny` TS7009; without it TS2350 at the `new` expression when the call's
  signature has a declaration and does not return `void` (we require every overload to
  qualify, the call is not resolved) (tsc: checker.go, resolveNewExpression).
- A class reached through a namespace (`N.E`) before its declaration runs gets TS2449 at the
  property name under the same timing rules as a plain identifier (tsc: checker.go,
  checkPropertyNotUsedBeforeDeclaration).
- TS2610/2611 (accessor overriding a property and back): an abstract base field with an
  initializer is not exempt; a base member combined from several intersection constituents
  (a mixin's base) is an accessor only when every constituent has the same getter/setter
  set, else a property (tsc: checker.go, createUnionOrIntersectionProperty,
  isPropertyAbstractOrInterface).
- `for (x in o)` with an expression head: TS2405 (the head's type must accept
  `Extract<keyof O, string>`, `string` when `o` is unresolved) is decided before and
  regardless of other errors in the head (`for (a = 1 in b)` with `a` undeclared); only a
  read-only member head (TS2540, an error type) skips it. The reference check (TS2406) runs
  only when TS2405 did not fire (tsc: checker.go, checkForInStatement).
- TS2403 compares an inferred generic type when the declaration sees no type parameters
  but its initializer's own (`var v = <T>() => 1; var v = <T>(a) => 1;` outside any generic
  function, class, interface or alias, and not in JavaScript). A named function
  expression's `return f()` of itself adds nothing to its inferred return type, like a
  function declaration's (the binder puts the name's symbol on the expression)
  (tsc: checker.go, checkVariableLikeDeclaration, checkAndAggregateReturnExpressionTypes).
- `arguments` inside a class nested in a function (not an arrow) is still that function's;
  read in a field initializer or a static block (arrows transparent, another function's
  body ending the search) it is TS2815 and an error type. Unresolved at the top level it
  gets the TS2662 static-member suggestion like any name (`Function.arguments`)
  (tsc: checker.go, checkIdentifier, isInPropertyInitializerOrClassStaticBlock).
- Kinds of overriding members: a base method may be overridden by a method or a property,
  but an accessor overriding it is TS2423; a method overriding a base accessor is TS2426 and
  one overriding a base property TS2425 (base type, member name, derived type). Private
  members on either side are no override. Only bases declared as class members are judged
  (tsc: checker.go, checkKindsOfPropertyMemberOverrides).
- **Abstract members of a union's construct signature.** A union signature that stands for
  several members' signatures carries `SigFlags::UNION_OF_ABSTRACT` when any of them is
  abstract, and `new` through it is TS2511, wherever the abstract member sorts; signatures
  combined member by member keep `ABSTRACT` itself (tsc: checker.go, someSignature,
  combineUnionOrIntersectionMemberSignatures; `abstract_in_union_new`).
- **Object rest from a non-object (TS2700).** An object pattern's rest element is TS2700 at
  its name when the destructured type (reduced) is `unknown` or cannot be spread
  (`Ck::can_spread`: `null | undefined`, primitives, an intersection with `undefined`);
  `any` is never reported (tsc: checker.go, getBindingElementTypeFromParentType;
  `object_rest_from_non_object`).
- **`{} | null | undefined` accepts anything** under strictNullChecks in the assignable and
  comparable relations, `unknown` and `void` included (tsc: relater.go,
  isSimpleTypeRelatedTo; `unknown_like_union_target`).
- **Assigning to the global `undefined`** (not shadowed) is TS2539 at the name, also as a
  destructuring target (tsc: checker.go, checkIdentifier on an assignment target;
  `assign_to_undefined`).
- **Identity pairs signatures by position**: the same count, and each signature identical to
  the one at its index, so overload order matters. A generic mapped type is identical only to
  another generic mapped type (tsc: relater.go, signaturesRelatedTo with identity,
  structuredTypeRelatedToWorker). An element access with a key mixing numbers and strings
  reads the union of the string and number index signatures' types (`P | Q`, tsc distributes
  the access over the key's members; `redeclaration_identity_order`).
- **A negative literal index into a tuple without a rest element** is TS2514 at the index, in
  element accesses (read as `undefined`) and in indexed access types (tsc: checker.go,
  getPropertyTypeForIndexType; `tuple_negative_index`).
- **`intrinsic` as an alias body** is valid only for `BuiltinIteratorReturn` with no type
  parameters or a string mapping name with exactly one; otherwise TS2795 at the body (tsc:
  checker.go, checkTypeAliasDeclaration; `intrinsic_alias_body`).
- **A generic string mapping target** (`Uppercase<T>`) is not permissive: another mapping
  relates only with the same intrinsic, through its inner type (reported); other sources must
  be left unchanged by the mapping and fit the inner type, or relate through their constraint
  (tsc: relater.go, structuredTypeRelatedToWorker, isMemberOfStringMapping;
  `generic_string_mapping_target`).
- **A generic template literal target** (`${T}`) matches a string literal or template source
  by pattern; a type parameter, index, indexed access or string mapping source then relates
  through its constraint, and other concrete sources fail. A generic template source against
  a type parameter relates only through its constraint. `${M}` for a single non-generic
  pattern `M` is `M` itself (tsc: relater.go, structuredTypeRelatedToWorker; checker.go,
  getTemplateLiteralType; `generic_template_target`).
- **A union source against a type parameter** elaborates its first failing member like any
  union, except that the "could be instantiated with an arbitrary type" hint drops what was
  reported inside it (tsc: relater.go, reportRelationError resets the chain). Type argument
  constraints read a mapped type's key over arrays or tuples as `number | `${number}``
  (`with_implied_constraints`).
- **A generic indexed access target** (`T[K]`) accepts a concrete source only through its
  constraint for writing: the object's and the key's base constraints (or themselves when not
  constrained) indexed together, a union key giving the intersection of each key's type, and
  only declared properties counting when the object was a constrained type parameter. When
  either constraint stays generic or a key names no property, nothing concrete is
  assignable. Mapped, union or intersection objects and instantiable sources stay permissive
  for now (`indexed_access_write_constraint`). The relation hint treats `T[K]` like its object
  `T` when the source is not itself an indexed access (tsc: relater.go,
  structuredTypeRelatedToWorker, reportRelationError; `generic_indexed_access_target`).
  Through a constrained object a number index signature still answers numeric keys; only
  string index signatures are left out.
- **`S[K]` against `T[J]`** (objects type parameters or non-generic objects) relates when
  `S` relates to `T` and `K` to `J`, or through the target's write constraint; the source's
  own constraint is not tried, and a failure elaborates the object, then the key. A source
  constrained to `any` or to a constraint we can't finish stays permissive, because tsc
  narrows `S[K] | undefined` to `S[K] & ({} | null)` where we keep `S[K]`. A base
  constraint's symbol keys read a string index signature (`Record<string, any>[keyof T]` is
  `any`; tsc: checker.go, getPropertyTypeForIndexType; `indexed_access_pair`).
- **A generic mapped type indexed by any key** (no `as` clause) relates, and is reported, as
  its template with the key put in: `Partial<T>[K]` is `T[K] | undefined`. This happens on
  both sides of a relation. An intersection that has an instantiable member and a nullable or
  `{}` member gets the same treatment for each member, so `Partial<T>[K] & {}` becomes
  `T[K] & {}`, which relates to `T[K]`. The null-stripped target used for reports
  (`X | undefined` reported as `X`) applies only to sources that are not instantiable. An
  unconstrained `T[K]` against a union target is explained step by step: `T[keyof T]`, then
  `T[string] | T[number] | T[symbol]` (tsc: checker.go, getSimplifiedIndexedAccessType,
  getNormalizedUnionOrIntersectionType; relater.go, isRelatedTo; `mapped_access_relation`).
- **A concrete mapped type indexed by a generic key** (no `as` clause, not `-?`, the object
  itself not generic) has its template with the key put in as its constraint:
  `OptionHandlers[K]` is `(option: Options & { kind: K }) => string`. Under strict null
  checks it gains `| undefined` when the mapped type is optional (`?` or homomorphic over
  optional properties) and some optional property's literal or unique-symbol key fits the
  key's base constraint. This is the base constraint of such an access, the constraint used
  when it is a relation source (also inside a union target and as a member of a source intersection),
  but not the shape it is reported as (tsc: checker.go, getIndexedAccessType's mapped-type
  branch, substituteIndexedMappedType, couldAccessOptionalProperty,
  getEffectiveConstraintOfIntersection; `concrete_mapped_generic_index_substitution`).
- **Constrained type variables in unions.** A union of intersections `("one" & K) | ("two"
  & K)` collapses to `K` when every primitive member of `K`'s base constraint appears as the
  other part of one of the intersections. Done before subtype reduction (tsc: checker.go,
  removeConstrainedTypeVariables; `remove_constrained_type_variables`).
- **A generic key in an element access** must be assignable to `keyof` the object, or to each
  member's `keyof` for a union object (TS2536, and the access is the error type). Writing
  through a mapped type with a `readonly` modifier reports TS2542 (tsc: checker.go,
  checkIndexedAccessIndexType; `mapped_access_relation`).
- **Generic mapped types relate by their parts** instead of always succeeding.
  - Two generic mapped types: the source may not add `?` where the target doesn't. The `?`
    direction counts the mapped type the modifiers are copied from. The target's key set must
    relate to the source's. The `as` types must be equal. The source template, with its key
    parameter read as the target's, must relate to the target template, which includes
    `undefined` when it adds `?`. The key and template failures are elaborated.
  - A concrete object `S` against a generic `{ [P in Q]: X[P] }` without `-?`: `Q` must
    relate to the literal keys of `S`. With `?`, `Q` only has to overlap them. `S` must also
    relate to `X`. If not, only an empty object matches a `?` mapped type, and the failure is
    not elaborated. Templates of other shapes stay permissive. A `-?` mapped target accepts
    no concrete object and no unconstrained type parameter (which reads as `{}`).
  - A generic `{ [P in Q]: X }` without `as` or `?` is a type parameter `T` when `keyof T`
    relates to `Q` and `X` relates to `T[P]`. `Partial<T>` is not a `T`.
  - `keyof U` against `keyof T` (unconstrained `T`) or against a type parameter elaborates
    through `string | number | symbol` down to `string`.
  - Sources: tsc relater.go, structuredTypeRelatedToWorker and mappedTypeRelatedTo; checker.go,
    getCombinedMappedTypeOptionality. Fixture: `generic_mapped_pair_relation`.
- Front-end crates are read-only; parser problems go to
  [../../docs/design/ast-requests.md](../../docs/design/ast-requests.md).

## Fixes made for type-aware lint (`ts_r_lint`, M4 round 2)

Small changes a lint rule needed, each with a `tests/parity` fixture:

- The error type as an arithmetic operand behaves as `any`: `Missing * 7` is `number`
  (`bigint` next to an all-bigint operand), `Missing + 's'` is `string`, `Missing + 1` stays
  the error type (`error_type_arithmetic`). An operand that became an error by a reported
  problem (`null * 1`, an invalid other operand) keeps the error result.
- Inferring a type predicate for a function expression's signature (TypeScript 5.5) checks
  its body with that signature's parameter types, speculatively, so a callback of an
  overloaded generic method (`find`, `filter`) never gets its parameters fixed as implicit
  `any` while the call is still resolving (`overload_callback_predicate`).
- The error type narrows like `any`: a type predicate, `typeof` or `instanceof` on a value
  from an unresolved import gives the narrowed type (`error_type_narrowing`).
- Under `strictBindCallApply`, function types get `call`/`apply`/`bind` from
  `CallableFunction` (construct-only types from `NewableFunction`), falling back to
  `Function`. A signature with a declared `this` parameter infers from the call's receiver
  first; a type parameter the receiver's parameters alone determine (`A` in
  `(this: (this: T, ...args: A) => R)`) is fixed by them (`strict_bind_call_apply`).
  Not done: a rest parameter of a fixed tuple type is not expanded when printing
  (`(...args: []) => string` for tsc's `() => string`), and the `this` argument itself
  is not checked against the declared `this` type.
- A tuple target is not elaborated with missing properties for a source that is not
  array-like: `IArguments` to `[]` reports only the head message
  (`tuple_target_no_elaboration`).
- `&&` keeps an `any` or `unknown` left operand: their definitely-falsy part is themselves,
  so `row && f(row)` with `row: any` is `any`, and with `unknown` it is `unknown`
  (`any_logical_and`).
- An optional link (`?.`) adds `undefined` to the chain's type only when its object may be
  `null` or `undefined`: a nullable union, `any`, `unknown`, or a type variable whose base
  constraint may be. `one?.price` is `number` for a non-nullable `one`, and so is `t?.a` for
  `T extends I` (`optional_link_undefined`).
- `x satisfies T` with an unresolved `T` is the error type, so its members report nothing
  (`satisfies_error_target`).
- A generic object spread stays generic: `{ ...t }` is `T`, `{ ...t, a: 1 }` is
  `T & { a: number }`, `{ x: 1, ...t }` is `{ x: number } & T`; nullish members of a union
  spread nothing, and no empty `{}` is intersected in when no properties follow or precede
  a spread (`generic_spread`).
- The error type is related to everything except under the subtype relations, where it is
  an `any`: overload resolution's subtype pass picks `new Uint8Array(err)`'s generic
  overload, as tsc does (`error_arg_overload`).
- An unresolved type reference is a *named* error type (`TypeStore::named_error`): it
  behaves exactly as `TypeId::ERROR` but prints as written (`Problem`, `z.infer<any>`,
  `N.Q2`). Always test with `store.is_error(t)`, never `t == TypeId::ERROR`. A union whose
  only member is one named error is that type, and a union or intersection mixing it with
  anything else is the plain error type (tsc prints `Pick<any, "a">`); `E[K]` of a named `E`
  is `E`; `Pick<Problem, "a">` is an object with an error-typed `a`, so `{}` misses `a`
  (TS2741) (`unresolved_reference_names`).
- A union written with an aliased union keeps its *origin* (`TypeStore::union_with_origin`)
  and prints as written: other members first, then the aliases, with `null` and `undefined`
  last (`HeadersInit | undefined`, `BodyInit | null | undefined`). Narrowing and mapping a
  union with an origin keep the aliases that survive whole. Instantiating drops the origin.
  Relation elaboration into a union picks the member sharing the most properties with the
  source (index signatures count on either side; arrays are candidates); a source sharing
  none is not elaborated (`union_origin_printing`).
- `...rest` in an object binding pattern (`object_rest_type`) is an object of the remaining
  public properties without `readonly`, plus the index signatures; class instances also lose
  methods and accessors; unions distribute (nullable members dropped first). A generic
  parent gives the global `Omit<T, K>` with `K` the omitted names, plus the public methods
  and accessors when the apparent type is a class instance (`Omit<this, "m">`; tsc:
  `getRestType` in `checker.go`). Arrays, tuples and primitives stay `UNRESOLVED`. The rest of a tuple ending in a rest element is an
  array (`[number, ...string[]]` gives `string[]`). An array binding pattern makes an array
  literal initializer a tuple (`const [a, b] = [1, "x"]`); other initializers get no pattern
  context, since a pattern must not be a call's only inference source (`object_rest_types`).
  An empty array pattern implies `Iterable<any, void, undefined>` (ES2015 on, else `any[]`)
  and a lone rest element `any[]`, so neither makes its initializer a tuple
  (`[...a] = [1, 2]` is `number[]`).
- TS2769 is tsc 7's form: only the last overload's error, under "The last overload gave the
  following error.", elaborated as for a single signature (the elaboration's diagnostics are
  re-headed). Signatures merged from several interface declarations list later declarations
  first, lib files included. An arrow with an annotated parameter is not elaborated into its
  body (`overload_last_error`).

Type-aware lint round 3 (`m4/type-aware-3`):

- Without a `JSX` namespace (React's types not installed), a JSX element or fragment has
  the error type, as in tsc: arithmetic on it
  is `number`, `keyof` it is `keyof any` (`jsx_element_error_type`). With a namespace it
  stays `UNRESOLVED`, and so does an element under the automatic runtime unless the runtime
  module could not be found (TS2875). tsc takes the namespace from the factory's root first (`h.JSX` for
  `jsxFactory: "h"` or a leading `@jsx h` pragma, `React.JSX` by default), then the global
  `JSX` (`Ck::jsx_ns`; `Checker::jsx_namespace` is the lint's cheaper version, without
  the automatic runtime).
- `typeof x.p` where `x` is `any` (or implicitly `any`) is `any` (`typeof_any_property`).
- The plain error type (`TypeId::ERROR`, not a named unresolved reference) infers itself
  into every `infer` parameter of a conditional type, so `Parameters<E>`, `ReturnType<E>`
  and `Awaited<E>` of an error `E` are the error type; re-entering the same conditional root
  while resolving it that way (`Awaited`) gives the error type (`Ck::error_infer_roots`).
  `E[K]` of the plain error type is the error type. `typeof x` of an unresolved import is
  now the plain error type (it was `UNRESOLVED`). A named error (`Parameters<Missing>`)
  still infers nothing: `unknown[]` (`error_type_inference`).
- `import("./m")` of a TypeScript ES module of the program (not JavaScript, no
  `export =`) is `Promise<typeof import("./m")>`; other targets stay `UNRESOLVED`
  (`dynamic_import_type`). Under `module: node16`..`nodenext` a CommonJS-format target
  is wrapped in a synthetic `default`, which is not modelled, so it stays `UNRESOLVED`. Conformance 7879 -> 7881 exact.
- Destructuring a property that `unknown`, or an object type (not a union or
  intersection), lacks gives the error type, next to the TS2339 at the key
  (`destructure_missing_property`).
- Accessor signatures (`get p(): T` / `set p(v: U)` in interfaces and type literals, such
  as lib.dom's `Window.location`) are typed: reads get the getter's annotation (else the
  setter's), and a write to any accessor (class members too) must satisfy the setter
  parameter's annotation, instantiated for an instantiated member; union and intersection
  properties keep the read type for writes (`accessor_signatures`). Conformance
  7881 -> 7887 exact.

## Levels (`layers.rs`, design record 0005)

`Checker::layered(&program, Some(&layer))` builds a checker whose own level holds the
program's files after the layer's (the same `Arc<SourceFile>`s, in order); it sees and keeps
filling every lower level. `Checker::new` is the one-level case. `ts_r_history` plans and
shares levels; the driver takes any checker (`check_program_with`, `check_files`).

- A `Layer` owns its level's symbol table, `SymLinks`, synthetic symbols, shadow tables and
  type store. Symbol ids carry their level like type ids. Merged declarations never span
  levels: `SymbolTable::build` flags it and `layered` refuses (`LayerError::CrossLevelMerge`).
- **Every write into a shared slot goes through a level check.** Read and write `SymLinks`
  only through `sym_links()` (`LinksRef`/`SlotRef`): a value above the symbol's level goes
  to the shadow table of its own level. New synthetic symbols go to the level of their
  highest part (`new_synth`); when you add a field to `SynthSym` that refers to a type or
  symbol, add it there. New checker-side tables that outlive a `Ck` must be per level (put
  them in `Layer`, keyed by level-carrying ids) or per `Checker`, never a single shared table
  holding ids of several levels.
- Program-wide link data (`augmentation_redirects`) is filtered per level: an entry whose
  ends are all in lower levels was applied there and is skipped, and one that crosses levels
  is flagged like a cross-level merge (`symbols.rs`; regression `ts_r_history/tests/layers.rs::lower_level_augmentation_redirect`).
- Lower levels must not depend on higher files. `ts_r_history::levels` guarantees it; the
  checker only detects cross-level merges.
- `PUBLISH_MISSES` (in `ts_r_types`) counts values a level could not keep. It is 0 on every
  private-app project; a non-zero count means a creation path computes its level wrong.

## Dependencies

`ts_r_span`, `ts_r_ast`, `ts_r_parser`, `ts_r_binder`, `ts_r_diagnostics`, `ts_r_config`,
`ts_r_resolver`, `ts_r_types`, `rustc-hash`. Dev: `ts_r_testing` (memory cap), `ts_r_emit`
(the conformance example's declaration diagnostics; a dev-dependency cycle, which examples
and integration tests allow: they link the one library build).

## Tests and harnesses

- `cargo test -p ts_r_checker`: `tests/check.rs` (small programs, expectations from tsc 7),
  `tests/decls.rs` (declaration checks: unused, classes, overloads, decorators, abstract
  members, `override`).
  `tests/type_ops.rs` (mapped, conditional and template literal types, utility types,
  `ThisType`, and the pathological limits: recursive conditionals, tuple doubling, huge
  template unions).
  `tests/bounded.rs` (inputs that used to grow exponentially: locals of speculatively checked
  callbacks, nested generic callbacks, condition chains in callbacks, relations of big
  generic interfaces).
  `tests/parity.rs` runs `tests/parity/<name>.ts` (one reproduction per difference found
  on a real project; `// @filename:` splits files) against `<name>.txt`, tsc 7's output.
  Tests skip when the lib files are missing (`cargo xtask upstream sync --minimal`).
- Conformance (needs `cargo xtask upstream sync`):
  `cargo build --release -j 4 -p ts_r_checker --example conformance` then
  `scripts/limit.sh --mem-mb 4096 --time-s 1800 -- target/release/examples/conformance [--verbose] [--threads N] [filter...]`.
  Runs every test variant as a project and compares all diagnostics (with chains) against the
  `.errors.txt` baselines; prints per-area rates and writes mismatches to
  `target/checker-conformance.txt`. Filters are path substrings (`types/primitives`);
  `--filter-file F` reads whitespace-separated filters from a file. Tests with no tsc 7
  baseline of any kind (mostly `@module: amd`/`system`, which tsc 7 no longer runs) and
  `@pretty: true` tests are skipped and counted, not compared. As in tsc's harness, only
  the last unit is a root under `@noImplicitReferences` or when the last unit has a
  `require(` or a `reference path` (the other units are only on disk), except in tests
  with a `tsconfig.json` unit (not modelled: every unit stays a root). Without `allowJs`
  (and without a `tsconfig.json` unit), JavaScript roots are left out of the program and
  reported as global TS6504 with tsc's "Root file specified for compilation" chain, except
  in a test that is itself a `.js` file. Other roots follow tsc's root rules
  (`tsc/internal/compiler/fileloader.go`): an unsupported extension (JSON without
  `resolveJsonModule`, `.txt`, `.js.map`) is left out with global TS6054 and the same chain;
  an extensionless root is the first existing file of it plus `.ts`, `.tsx`, `.d.ts` (`.js`,
  `.jsx` with `allowJs`). With `declaration` or `composite` the example
  also runs declaration emit (`ts_r_emit`, declarations only, `noEmit` ignored) and adds
  its tsc diagnostics (TS2883, TS4023, TS4094, TS7056, TS9xxx; not TSR codes, not
  TS5055/TS5056): tsc's harness adds `GetDeclarationDiagnostics` whenever declarations
  are on, emitted or not (`tsc/internal/testutil/harnessutil/harnessutil.go`).
  TS5055/TS5056 come from `ts_r_emit::output_path_conflicts` (not under
  `@suppressOutputPathCheck`, a harness-only option); TS5055 gets the "Adding a
  tsconfig.json file" chain line only without a `tsconfig.json` unit. The option-level
  layout errors (TS5009, TS5011, root TS6059) come from `ts_r_emit::root_dir_diagnostics`,
  naming the test's own config rather than the wrapper (its import-located TS6059 are not
  wired in: no baseline needs them yet).
  Unit names with a drive root (`A:/foo/bar.ts`) are absolute, as in tsc's harness, and
  `@useCaseSensitiveFileNames: false` makes the file system case-insensitive (default
  sensitive: `tsc/internal/testutil/harnessutil/harnessutil.go`).
  `TS_R_CONFORMANCE_ROOT` replaces the upstream checkout (a directory with the same
  `tests/cases` and `tests/baselines/reference` layout, for ad hoc tests) and
  `TS_R_CONFORMANCE_OUT` the mismatch file.
  Current (2026-10-26, 687b025, wip/v1-checker on fa446e4 with v1/parser merged: TS1156 and
  TS1084 through syntax errors, missing heritage elements, implied conditional constraints on
  instantiation; 12750 compared variants): 12372 exact (97.0%; all diagnostics and chains),
  12392 (97.2%) exact on first lines; 186 variants with a diagnostic tsc does not report, 10 of
  them tests where tsc reports nothing.
  Earlier: 12365, 191 and 10 (9b5f8f2, parity/checker-2 on 79646f9); 12350, 200 and 17 (3cd16f3, #39 on main); 12360, 194 and 10 (321762c, this branch on 0353caa); 12345, 203 and 17 (48eb582, #32 on main); 12350, 198 and 10 (bad3df6, this branch on 1c10cde); 12335, 207 and 17 (1d94474, #31 on main); 12330, 214 and 22 (775ce97, this branch before #31); 12326, 216 and 22 (on 087afc9, before the merge); 12316, 220 and 22 (47300d06, alpha.4 round 3); 12116, 303 and 37 (0bcd47e3, alpha.4 round 2); 11965, 328 and 43 (7c73600, alpha.4 round 1); 11615, 454 and 54 (0bc6c71, alpha.3); 11111, 703 and 92 (main at 9f23c9d); 8444 exact, 1698 and 320 (2026-10-16, after conformance round 4); 7879, 2319 and 573 (round 3).
  The example checks through syntax errors as tsc's harness does (since m8/parser-diagnostics).
- Single files against tsc: put the file next to a `tsconfig.json` and diff
  `target/release/ts-r-check -p dir --pretty false` with tsc's output.
- private-app (`ts-r-check -p . --no-history` vs tsc 7 `tsc -p .`): over all 25
  tsconfig projects (find them with `find . -name tsconfig.json -not -path '*/node_modules/*'`)
  all 25 match tsc exactly (0 missing, 0 extra).
  0.01-0.04 s and 29-50 MB vs tsc 7's 0.15-0.17 s and 106-132 MB for the apps.

## Known gaps (next layers)

- Type operators: reverse mapped types (inferring `T` from `{ [K in keyof T]: Box<T[K]> }`)
  infer a silent `UNRESOLVED`; generic mapped types stay opaque in relations; `keyof X` and
  indexed-access aliases print expanded (`"a" | "b"` where tsc prints `keyof Foo`); TS2589
  for a recursive alias is reported at the use, not at the recursive reference.
- Variance is measured only for classes, interfaces and aliases compared in
  `structured_related` (an alias of a union is decomposed first); method parameters that
  are callbacks are compared strictly
  (tsc's callback check, return types either way), which keeps `Promise<A>` from relating
  to `Promise<never>`.
- Printing: tsc 7 drops `| undefined` from optional properties of a function expression's
  own annotated parameter (`(x: { a?: 1 } = {}) => x` prints `{ a?: 1; }`); we don't.
  (`?` parameters print `x?: T | undefined`, initialized ones `x?: T`, as tsc does.)
- JSX (`jsx_check.rs`): generic components whose props are indexed access or conditional
  types, or whose type parameters only context-sensitive attributes infer (tsc's second
  inference pass), are not checked; TS2558 on JSX type arguments,
  template-literal index signatures on `IntrinsicElements`, the TS2786 chain layering,
  TS2604 for union tags. TS2698 and TS2783 are reported for JSX spreads only, not for
  object literal spreads. JSX attributes have synthetic symbols without a declared prop
  behind them, so the language server finds no symbol for `<form onSubmit>`. Evolving arrays
  in JavaScript files, TS7005 for closures created after conditional assignments of an auto
  variable, reverse mapped type
  inference (contextual types through it are treated as unknown), `this` parameter checks, modifier order outside classes
  (`declare export`, `out in`), TS2512/TS2513
  across a class merged with an interface, TS2564 on computed names (`["c"]: number`),
  TS2729 through the computed names of object-literal accessors.
- Heritage: TS2612 (redeclared base property under `useDefineForClassFields`), TS2310
  (circular bases: the heritage checks are skipped), discriminated-union decomposition in
  relations (`{ done: boolean }` to `IteratorResult`), generic method parameters inferred
  against a `keyof` constraint.
- Unused checks: write-only private properties through element access in another file,
  overloaded functions and merged declarations are not reported (silent).
- Sibling type parameters in the constraints of a cloned generic method signature keep
  their original identity (`<K extends keyof T, U extends K>` instantiated by a receiver).
- Calls and patterns: TS2769 for tagged-template overloads (only call, `new` and JSX
  arguments are elaborated), TS2671
  for an `export =` that is only a function, TS2300 for duplicate late-bound methods and
  accessors in object literals, TS2465 (`this` in a computed name), implied index signatures merged across
  several declarations of one interface (the first declaration's win).
- Union call signatures: overload lists that match only partly (`(x: string, y?: string)`
  against `(x: string)`, `string` against `any`) and array methods on unions of arrays
  (`(string[] | number[]).map`) are not callable here; `v!` of an unconstrained type
  parameter prints `V` where tsc prints `NonNullable<V>`.
- Printing: a late-bound name whose text is not an identifier prints as its text (`'1'`)
  where tsc prints the key as written (`'[c0]'`) in relation chains.
- Largest false-positive groups left (tests where tsc reports nothing): TS2322 and TS2339
  (33 each: discriminated unions, narrowing through intersections, module resolution
  layouts), TS7006 (27: contextual parameter types through reverse mapped types and
  generic inference), TS2307 (13: `nodenext` self-names and symlinks).
