# 0007: Sound mode: the 14 rules, `parse<T>` validators, configuration and reporting

Status: Accepted (2026-10-15; every open question resolved, see "Decisions")
Owners: `ts_r_sound` (rule passes, escape hatches, coverage report), `ts_r_guard` (validator
evaluator and emitter; new), `ts_r_checker` (sound relation mode, flow hooks, tagged `unknown`),
`ts_r_config` (`[sound]` configuration); consumers: `ts-r-check`, `ts-r-lsp`, `ts-r-build`,
`ts-r-dev`, `ts-r-run`
Test cases: [0007-sound-mode/cases/](0007-sound-mode/cases/README.md)

## Context

The plan (Sound mode tab; decision D2) adds an opt-in **sound mode**: in files that opt in, a
static type is a guarantee about the runtime value, unless a written escape hatch says otherwise.
D2 puts all 14 rules (S-01 to S-14) into v1, which runs in M6 (days 6 to 13). Sound mode has no
reference implementation to test against, so per K-16 people write the rules and test cases
first; this record and its cases are that spec. An agent implementing M6 builds from them, and
the `add-sound-rule` skill turns each rule section into tests, code, fixes and a docs page.

Constraints from the plan:

- **P7, no new syntax.** Every sound file is ordinary TypeScript that tsc 7 accepts. Sound mode
  only adds errors. `@ts-r.dev/guard` is type declarations plus a stub that throws when not expanded.
- **P4, validators from declared types.** `parse<T>` is expanded from explicit type declarations
  (the `ts_r_decls` index), never from inference, so builds stay per-file.
- **Per-file checking.** Sound files require explicit types on exports (`isolatedDeclarations`),
  so every rule below is decidable for one file given the declarations it imports. No rule needs
  whole-program analysis. That keeps checking parallel and history-friendly
  ([0004](0004-types-and-checker.md), [0005](0005-history.md)).
- **Dependencies are excluded automatically.** Packages get a mode (`trusted`, `loose`,
  `verify`, `sound`); only the user's opted-in files are held to the rules.
- **Levels.** `off` (plain TypeScript), `strict` (strict flags, `noUncheckedIndexedAccess`,
  `exactOptionalPropertyTypes`, `isolatedDeclarations`, S-01, S-04, S-10), `sound` (`strict` plus
  S-02, S-03, S-05 to S-09, S-11 to S-14), `contracts` (`sound` plus runtime checks in dev builds).

Unlike the rest of ts-r, tsc 7 is not the spec here: it is the **lower bound**. Every case file
must be clean under tsc 7 with `strict: true`, and sound mode may only add diagnostics.

## Decision

### Diagnostic codes

Sound-mode diagnostics use their own prefix and range: **`TSR0001` to `TSR9999`**, rendered with
the `TSR` prefix and four digits (`error TSR0201: ...`). tsc codes are rendered `TS<n>`, and the
existing ts-r-only codes are `TS99001` to `TS99003` (see `crates/ts_r_diagnostics/AGENTS.md`), so
neither the printed form nor the numeric value can collide:

- **Numeric value.** In `DiagnosticMessage` the code is stored as `100_000 + n` (`TSR0201` is
  `100201`). The renderer prints codes `>= 100_000` as `TSR{n:04}`. tsc's code families stop
  well below that (the highest we know of is 95xxx, code-fix descriptions), and ts-r's own TS
  codes are 99xxx. The implementing agent adds a unit test that every code in the synced
  `diagnosticMessages.json` is `< 100_000`.
- **Layout.** `TSR{rule:02}{item:02}`: the first two digits are the rule number, so `TSR0201` is
  S-02 and `TSR1401` is S-14 (this matches the plan's `TSR1401`/`TSR1402` example).
  `TSR00xx` is general (escape hatches, pragmas, configuration, forced flags). `TSR15xx` is
  `parse<T>` validator generation. `TSR16xx` to `TSR99xx` are reserved.
- **Category.** Every sound diagnostic is an `Error`. Sound mode has no warnings. Something that
  should not fail a build doesn't belong in sound mode; it belongs in `ts-r-lint`.
- **Message style** follows tsc: one sentence or two, quoted names in `'...'`, `{0}` arguments,
  ends with `.`, and says what to write instead.
- **One diagnostic per node.** When several rules fire on the same expression, only the first in
  this order is reported: S-14, S-01, S-11, S-12, S-02, S-03, S-07, S-08, S-13, S-10, S-06,
  S-05, S-09, S-04. More specific advice wins (`JSON.parse(s) as User` reports TSR1401, not
  TSR0201).

General codes:

| Code | Message |
|---|---|
| TSR0001 | `'@ts-r-unsafe' needs a reason: write '// @ts-r-unsafe: <why this is safe>'.` |
| TSR0002 | `Unused '@ts-r-unsafe' directive: the next line has no sound-mode error.` |
| TSR0003 | `Unknown sound-mode rule '{0}' in '@ts-r-unsafe'. Rules are S-01 to S-14.` |
| TSR0004 | `Unknown level '{0}' in '@ts-r' pragma. Use 'off', 'strict', 'sound' or 'contracts'.` |
| TSR0005 | `The '@ts-r' pragma must come before the first statement; this one is ignored.` |
| TSR0010 | `{0} (sound mode enables 'isolatedDeclarations').` (wraps the tsc message) |
| TSR0011 | `{0} (sound mode enables '{1}').` (wraps the tsc message for a forced strict-family flag) |
| TSR0020 | `Invalid sound-mode configuration: {0}.` (reported on `ts-r.toml`, `tsrconfig.json` or `tsconfig.json`; when more than one of them configures sound mode, `{0}` names every file) |

**Forced flags.** A level forces some tsc flags. If `tsconfig.json` already sets a flag, its
diagnostics keep tsc's own code (tsc reports them too, so parity holds). If only sound mode turns
it on, the diagnostic gets a TSR code and the wrapped tsc message, so a user comparing with tsc
sees why ts-r reports more. `noUncheckedIndexedAccess` and `exactOptionalPropertyTypes` belong to
S-04 (`TSR0401`, `TSR0402`); `isolatedDeclarations` gives `TSR0010`; the `strict` family gives
`TSR0011`.

### How rules see types

Three mechanisms are shared by several rules. They change the checker as described in
"Changes to other records" below.

1. **Tagged `unknown`.** Two new fixed intrinsics, `UNKNOWN_FROM_ANY` and `UNKNOWN_EXTERNAL`,
   behave exactly like `unknown` in every relation, narrowing and printing (they print as
   `unknown`). They exist only so an error on such a value can say where it came from:
   TSR0102 (S-01) or TSR1401 (S-14) instead of tsc's TS18046/TS2322/TS2339. Union and
   intersection reduction treat them as `unknown`; the tag of the reduced type is the first
   tagged member in canonical order, so it is deterministic.
2. **`any` becomes `unknown` at sound read sites.** When a sound file reads the type of a symbol
   declared outside the sound set (lib files, `.d.ts` files, `trusted` and `verify` packages,
   non-sound local files), each `any` in that type is replaced by `UNKNOWN_FROM_ANY`, deeply
   (`Promise<any>` becomes `Promise<unknown>`). This is a cached type mapper (`AnyToUnknown`)
   applied at the read site, so declared types in the shared store are never forked and
   non-sound files still see `any`. External sources (S-14) map to `UNKNOWN_EXTERNAL` instead.
   `loose` packages are not mapped (S-11).
3. **Sound relations.** Comparisons made while checking a sound file use a separate relation
   mode with three differences: mutable built-in containers are invariant (S-05), a target method
   declared in a sound file is compared like a function property (S-06), and in type assertions
   an `any` source counts as `unknown` (S-02). Sound relation results are cached under their own
   relation kinds (`SoundAssignable`, `SoundComparable`); variance is measured per
   (generic declaration, relation mode).

### Escape hatches

- **`// @ts-r-unsafe: <reason>`** on its own line suppresses every sound diagnostic (`TSR` codes
  only, never `TS` codes) that starts on the next line. **`// @ts-r-unsafe S-02 S-03: <reason>`**
  limits it to those rules. The reason is required (TSR0001). A directive that suppresses nothing
  is TSR0002, so hatches can't outlive the code they excuse. A hatch with an error (TSR0001,
  TSR0003) suppresses nothing and is not also reported as unused. Every hatch is listed in the
  coverage report with file, line, rules and reason.
- **Per file:** `// @ts-r off` (or `strict`) in the file's leading comments lowers that file's
  level (see Configuration).
- **Per package:** `[sound.packages]` modes (S-11).
- `@ts-ignore`, `@ts-expect-error` and `@ts-nocheck` are not escape hatches in sound files (S-12).

### Where checks run

Every rule runs in `ts-r-check` and `ts-r-lsp` on the same `Program` and query API
([0004](0004-types-and-checker.md) invariant 5). `ts-r-build`, `ts-r-dev` and `ts-r-run` don't run
the checker; they read the last check result (see Reporting) and expand `parse<T>`. `.d.ts` files,
JavaScript files and everything under `node_modules` are never sound files, whatever the
configuration says; rules check only how their values are *used* in sound files.

---

## The rules

Each rule below gives the exact rule, its diagnostics, examples, its interaction with `any`,
`unknown`, declaration files and packages, escape hatches, false-positive risk and migration.
The case directories hold the full accept and reject sets.

### S-01: No `any` (level: strict)

**Rule.** In a sound or strict file: (a) the type `any` may not be written anywhere (annotations,
type arguments, defaults, assertions, `extends` clauses, index signatures); (b) a value whose
declared type contains `any` and comes from outside the sound set is seen as `unknown`
(mechanism 2), and using it as a more specific type is an error; (c) calling or constructing a
value of type `Function` (whose result is `any`) is an error. Implicit `any` is already an error
under `noImplicitAny`, which the level forces.

| Code | Message | Reported at |
|---|---|---|
| TSR0101 | `Type 'any' is not allowed in sound mode. Use 'unknown', a type parameter or a specific type.` | the `any` keyword |
| TSR0102 | `'{0}' is 'unknown' in sound mode because '{1}' declares it as 'any'. Narrow it or parse it with 'parse<T>'.` | where tsc would report the `unknown` misuse |
| TSR0103 | `Calling a value of type 'Function' returns 'any'. Give '{0}' a function type.` | the call |

```ts
let raw: any = load();                         // TSR0101
function f(x: Record<string, any>): void {}    // TSR0101
if (Array.isArray(input)) { const s: string = input[0]; } // TSR0102: lib says any[]
p.catch((e) => console.log(e.message));        // TSR0102: lib says reason: any
const fn: Function = pick(); fn();             // TSR0103

let raw: unknown = load();                     // ok
function f<T>(x: Record<string, T>): void {}   // ok
p.catch((e: unknown) => report(e));            // ok
```

**`any`, `unknown`, `.d.ts`, `node_modules`.** `any` written in `.d.ts` files and packages is
fine; it becomes `unknown` where a sound file reads it. Lib functions that return `any`
(`Array.isArray` narrowing to `any[]`, `Promise.prototype.catch`, `Reflect.get`, `new Map()`
without type arguments, `JSON.parse`, `Response.json`) are covered by mechanism 2; the ones that
are external sources (`JSON.parse`, `Response.json`, `MessageEvent.data`, ...) report through
S-14 instead. `loose` packages keep `any` (S-11). Published `.d.ts` output is never changed.

**Escape hatches.** `// @ts-r-unsafe S-01: reason`. There is no `any`-allowing config flag.

**False-positive risk: low.** TSR0101 only fires on written `any`, which `unknown` or generics
replace. TSR0102 fires where code relied on a lib `any`; each is a real hole.

**Migration.** Auto-fix `any` to `unknown` and let the follow-on errors guide narrowing.
`ts-r-check --sound-report` lists files by `any` count to pick the order of adoption.

### S-02: No narrowing type assertions (level: sound)

**Rule.** `e as T` and `<T>e` are allowed only when the type of `e` is assignable to `T` under
sound relations (an upcast). `as const`, `satisfies T`, and assertions to `unknown` are always
allowed. A source of type `any` counts as `unknown`, so `x as User` with `x: any` is a narrowing.
Each assertion in a chain is checked on its own: in `x as unknown as T` the outer one fails.

| Code | Message | Reported at |
|---|---|---|
| TSR0201 | `Type assertion from '{0}' to '{1}' narrows the type. Use 'parse<{1}>', 'is<{1}>' or a narrowing check instead.` | the assertion expression |

```ts
const user = data as User;                  // TSR0201 (data: unknown)
const cfg = {} as Config;                   // TSR0201 (Config has required keys)
const input = el as HTMLInputElement;       // TSR0201 (el: HTMLElement)
function cast<T>(x: unknown): T { return x as T; } // TSR0201

const a = dog as Animal;                    // ok: upcast
const point = { x: 1, y: 2 } as const;      // ok
const conf = { port: 80 } satisfies Partial<Config>; // ok
const empty = [] as string[];               // ok: never[] is assignable to string[]
```

**`any`/`unknown`/`.d.ts`.** Treats `any` sources as `unknown` (above). Assertions inside `.d.ts`
files and packages are not checked. A value from an external source (S-14) cast to a type reports
TSR1401 instead (precedence).

**Escape hatches.** `@ts-r-unsafe S-02: reason`, or replace the cast with `parse<T>`, `is<T>`,
`assert<T>`, or `instanceof`/discriminant narrowing.

**False-positive risk: medium.** DOM code (`querySelector(...) as HTMLInputElement`) and test
fixtures (`{} as Deps`) cast often. Every one is unchecked in TypeScript, so they are real,
but the volume is high.

**Migration.** Quick fixes: `x as T` where `T` is validatable becomes `parse<T>(x)`; DOM
element casts become an `instanceof` check with `expect`; `{} as T` in tests becomes a typed
builder. Adopt test folders last (`exclude = ["**/*.test.ts"]` at first).

### S-03: No non-null assertions (level: sound)

**Rule.** The postfix non-null assertion `e!` is not allowed in sound files, whatever the type
of `e`. (Definite assignment `x!: T` is S-08.)

| Code | Message | Reported at |
|---|---|---|
| TSR0301 | `Non-null assertion is not allowed in sound mode. Narrow '{0}' with a check, or use 'expect({0}, "why")'.` | the `!` expression |

```ts
const el = document.getElementById("app")!;    // TSR0301
const first = names[0]!.trim();                 // TSR0301
this.socket!.send(msg);                         // TSR0301

const el = document.getElementById("app");
if (el) el.textContent = "hi";                  // ok
const first = expect(names[0], "names is never empty"); // ok: throws ExpectError at runtime
```

`expect(value, message)` from `@ts-r.dev/guard` returns `NonNullable<T>` and throws if the value is
`null` or `undefined`. It is a panic helper: unchecked for S-13.

**`any`/`.d.ts`.** Not affected; `!` in packages and `.d.ts` is irrelevant (it can't appear in
`.d.ts`).

**Escape hatches.** `@ts-r-unsafe S-03: reason`.

**False-positive risk: low.** Every `!` is an unchecked claim. A `!` on a non-nullable type is
also reported; it is harmless today but becomes a lie after a refactor, and the fix (delete it)
is automatic.

**Migration.** Auto-fixes: delete `!` on non-nullable operands; replace `x!` with
`expect(x, "<x> is set")` elsewhere (a message to refine later); in statements, offer an `if`
guard.

### S-04: Checked index access and exact optional properties (level: strict)

**Rule.** Sound and strict files are checked with `noUncheckedIndexedAccess` and
`exactOptionalPropertyTypes` on. Reading `a[i]` on an array or an index signature has type
`T | undefined`; an optional property `p?: T` accepts a missing key, not `undefined`, unless its
type includes `undefined`.

| Code | Message | Reported at |
|---|---|---|
| TSR0401 | `{0} (sound mode enables 'noUncheckedIndexedAccess': indexed reads can be 'undefined').` | where tsc with the flag reports |
| TSR0402 | `{0} (sound mode enables 'exactOptionalPropertyTypes': write '{1}?: T \| undefined' to allow 'undefined').` | where tsc with the flag reports |

`{0}` is tsc's own message for the same error. If the tsconfig already sets the flag, tsc's code
is reported instead (see Forced flags).

```ts
const first: string = names[0];              // TSR0401
scores[name].toFixed(1);                     // TSR0401 (Record<string, number>)
const o: Options = { timeout: undefined };   // TSR0402 (timeout?: number)

const first = names[0] ?? "none";            // ok
for (const n of names) n.trim();             // ok: iteration is not indexed access
const t: [number, string] = [1, "a"]; t[0].toFixed(); // ok: tuple element in range
```

**`any`/`.d.ts`.** Applies to every indexed read in a sound file, including on types declared in
packages. `.d.ts` declarations are read as written: an optional property declared `p?: T` in a
package means "missing or `T`", which is how most packages are typed already.

**Escape hatches.** `@ts-r-unsafe S-04: reason`; or `expect(xs[i], "...")`.

**False-positive risk: medium.** Loops over indexes (`for (let i = 0; i < xs.length; i++)
xs[i]`) are flagged although safe. The fix is `for...of`, `entries()`, or `expect`.

**Migration.** Turn on both flags in tsconfig first (ordinary tsc errors, ordinary tooling);
auto-fixes add `?? default`, `?.` or `expect`; for `exactOptionalPropertyTypes`, the fix adds
`| undefined` to the property where callers pass `undefined`.

### S-05: Mutable built-in containers are invariant (level: sound)

**Rule.** Under sound relations, `T[]`, `Array<T>`, mutable tuples, `Map<K, V>`, `Set<T>`,
`WeakMap<K, V>` and `WeakSet<T>` are **invariant** in their type parameters: `Dog[]` is not
assignable to `Animal[]`. `readonly T[]`, `ReadonlyArray<T>`, readonly tuples, `ReadonlyMap`,
`ReadonlySet` and `Iterable<T>` stay **covariant**, so `Dog[]` and `readonly Dog[]` are assignable
to `readonly Animal[]`. The variance is declared by ts-r for these lib types, not measured
(measuring with S-06 would make `ReadonlyArray` invariant because of `indexOf(x: T)`).
User-declared generic types get their variance measured under sound relations, which with S-06
already makes a `Box<T>` with a `set(v: T)` method invariant.

| Code | Message | Reported at |
|---|---|---|
| TSR0501 | `'{0}' is not assignable to '{1}': mutable '{2}' is invariant in sound mode, so writes through '{1}' could break '{0}'. Use '{3}' if it is only read.` | where tsc reports assignability errors |

`{3}` is the readonly form (`readonly Animal[]`, `ReadonlyMap<string, Animal>`).

```ts
const animals: Animal[] = dogs;              // TSR0501
feed(dogs);                                  // TSR0501 (feed(xs: Animal[]))
const byName: Map<string, Animal> = dogMap;  // TSR0501
const xs: (string | number)[] = strings;     // TSR0501

const animals: readonly Animal[] = dogs;     // ok
const copy: Animal[] = [...dogs];            // ok: fresh array literal is contextually typed
function count(xs: readonly Animal[]): number { return xs.length; }
count(dogs);                                 // ok
```

**`any`/`.d.ts`.** Applies to comparisons made in sound files, including between types declared
in packages (`Promise<Dog[]>` to `Promise<Animal[]>` fails). Package-internal assignments are not
checked.

**Escape hatches.** `@ts-r-unsafe S-05: reason`; copy (`[...xs]`, `new Map(m)`) when ownership
moves.

**False-positive risk: medium.** Passing a mutable array to a parameter typed `T[]` that is only
read is common; the fix (make the parameter `readonly`) improves the API. `xs.slice()` copies have
the source's element type and need `[...xs]` or a type argument.

**Migration.** The quick fix adds `readonly` to parameters of functions in sound files that never
write to them (checked by the fix). Do libraries first, call sites follow.

### S-06: Methods are checked contravariantly (level: sound)

**Rule.** Under sound relations, when the **target** of a comparison is a method declared in a
sound file (a method signature or method declaration in an interface, class, object type or
object literal), its parameters are compared contravariantly, exactly like function-typed
properties under `strictFunctionTypes`. This covers assignments, arguments, returns, and class
members checked against the base class or implemented interface. When the target method is
declared in a lib file, a `.d.ts` file or a non-sound package, TypeScript's bivariant check is
kept (decided 2026-10-15, "Decisions" question 7; `strict-lib-methods = true` opts in), except
for the containers fixed by S-05.

| Code | Message | Reported at |
|---|---|---|
| TSR0601 | `Method '{0}' is not compatible: parameter '{1}' of type '{2}' would receive '{3}'. Methods are checked like functions in sound mode.` | the member name for `extends`/`implements` and object-literal members; otherwise where tsc reports the assignability error |

```ts
interface Handler { handle(e: AppEvent): void }
class ClickOnly implements Handler { handle(e: ClickEvent): void {} } // TSR0601
class Sub extends Base { override accept(v: string): void {} }      // TSR0601 (Base: string | number)
const h: Handler = new ClickLogger();                               // TSR0601

class Any implements Handler { handle(e: AppEvent | null): void {} } // ok: wider parameter
const s: Sink<Dog> = animalSink;                                    // ok: contravariant
```

**`any`/`.d.ts`.** Methods from `.d.ts` and lib are left bivariant to avoid mass false positives
in framework types (for example a component base class whose lifecycle methods take props). Sound
workspace packages (`mode = "sound"`) count as sound files.

**Escape hatches.** `@ts-r-unsafe S-06: reason`.

**False-positive risk: low to medium.** Real event-handler hierarchies sometimes rely on
bivariance; each case is a real hole where a caller can pass the wider type.

**Migration.** Widen the parameter and narrow inside with a check; or make the interface generic
(`Handler<E extends AppEvent>`).

### S-07: Type guards must be proved (level: sound)

**Rule.** A user-defined type predicate (`x is T`, `this is T`) or assertion signature
(`asserts x is T`, `asserts x`) on a function with a body in a sound file must be **proved by
the checker** from its body. Let `P` be the parameter's declared type and `T` the predicate
type. For each `return e`:

- *true side*: the flow type of `x` where `e` is truthy must be assignable to `T`;
- *false side*: the flow type of `x` where `e` is falsy must be assignable to the type a caller
  gets in the false branch (`P` narrowed by "not `T`", as TypeScript computes it).

`return true` and `return false` use the current flow type of `x`. For `asserts x is T`, the
flow type of `x` at every normal completion must be assignable to `T`; for `asserts x`, `x` must
be narrowed to truthy at every normal completion. Predicates TypeScript infers (5.5+ inferred
type predicates) are proved by construction. Generated guards (`is<T>`, `assert<T>`) are proved
by construction. Predicates declared without a body (interfaces, overload signatures, abstract
methods, `.d.ts`) are trusted; implementations in sound files are checked.

| Code | Message | Reported at |
|---|---|---|
| TSR0701 | `Type predicate '{0} is {1}' is not proved: when it returns true, '{0}' can be '{2}'.` | the predicate in the signature (related info: the return) |
| TSR0702 | `Type predicate '{0} is {1}' is not proved: when it returns false, '{0}' can still be '{1}'.` | the predicate in the signature (related info: the return) |
| TSR0703 | `Assertion '{0}' is not proved: the function can return normally with '{1}' of type '{2}'.` | the assertion in the signature |

```ts
function isUser(x: unknown): x is User { return typeof x === "object" && x !== null; } // TSR0701
function isA(x: string | number): x is string { return x === "a"; }               // TSR0702
function assertNum(x: unknown): asserts x is number { if (typeof x !== "number") log(x); } // TSR0703

function isString(x: unknown): x is string { return typeof x === "string"; }     // ok
function isCat(a: Cat | Dog): a is Cat { return a.kind === "cat"; }             // ok
const isUser = (x: unknown): x is User => is<User>(x);                           // ok: generated
```

**`any`/`unknown`/`.d.ts`.** Predicates in `.d.ts` files, packages and non-sound files are
trusted, including lib ones (`Array.isArray`, whose `any[]` becomes `unknown[]` by S-01).

**Escape hatches.** `is<T>` / `assert<T>` generate a guard from the type; or
`@ts-r-unsafe S-07: reason` on the line before the signature (diagnostics are reported on the
signature so the hatch sits there).

**False-positive risk: medium.** Guards that check a few fields to recognise a larger type
(`"id" in x`) fail; that is exactly the hole. Guards for types the checker narrows poorly (deep
structural checks) need `is<T>`.

**Migration.** Quick fix: replace the body with `return is<T>(x)` when `T` is validatable; the
coverage report lists remaining hand-written guards.

### S-08: Fields are initialised before `this` escapes (level: sound)

**Rule.** (a) Definite assignment assertions (`field!: T`, `let x!: T`) are not allowed, nor are
`declare` field redeclarations whose type differs from the inherited one. (b) In a constructor and
in field initialisers, until every field is definitely assigned, `this` may only be used to assign
a field (`this.f = ...`) or to read a field already assigned. Calling a method or accessor on
`this`, passing `this` as an argument, returning or storing it, or creating an arrow function that
mentions `this` is an escape. Optional fields and fields whose type includes `undefined` count as
assigned from the start. (c) After all of its own fields are assigned, a constructor may still not
let `this` escape to code a subclass can change: calling a method or accessor that can be
overridden (anything except `#private` and `private` members, whose bodies are checked by the
same rule), passing `this` out, or creating an arrow function that mentions `this`. A subclass's
fields are not initialised until the base constructor returns.

| Code | Message | Reported at |
|---|---|---|
| TSR0801 | `Definite assignment assertion on '{0}' is not allowed in sound mode. Initialise it in its declaration or the constructor.` | the `!` |
| TSR0802 | `'this' escapes before '{0}' is initialised. Assign every field first.` | the escaping use of `this` |
| TSR0803 | `'this' escapes from the constructor through '{0}' before subclass fields are initialised. Make it '#private', or do this after construction (a static factory).` | the escaping use |

```ts
class View { el!: HTMLElement; }                                  // TSR0801
class A { items: string[]; constructor() { this.render(); this.items = []; } } // TSR0802
class B { constructor() { this.setup(); } setup(): void {} }       // TSR0803

class C { items: string[] = []; size: number; constructor() { this.size = this.items.length; } } // ok
class D { #n: number; constructor() { this.#n = 0; this.#log(); } #log(): void {} } // ok
```

**`any`/`.d.ts`.** Only classes declared in sound files are checked. Subclassing a package class
is fine; its constructor is trusted.

**Escape hatches.** `@ts-r-unsafe S-08: reason`. Frameworks that initialise fields from outside
(dependency injection, decorators that set properties) need a hatch per field or the file kept at
`strict`.

**False-positive risk: medium.** TSR0803 is the noisy one: `constructor() { this.init(); }` is
common. It stays on (decided 2026-10-15, "Decisions" question 8);
`constructor-escape = "before-init"` limits S-08 to TSR0802.

**Migration.** Quick fixes: move `!` fields into constructor assignments, or make the type
`T | undefined`; make helper methods `#private`; replace two-phase init with a static factory.

### S-09: Narrowings are dropped when they could be invalidated (level: sound)

**Rule.** A narrowing of a **property reference** (`o.p`, `o["p"]`, `this.p`, `o.p.q`) is
dropped at the first *effect point* after it on the flow path:

- a call or `new` (including tagged templates; property reads that run getters are not counted,
  see Known gaps), except calls to the curated list of lib
  functions that cannot run user code or mutate objects (methods of `string`, `number`, `bigint`,
  `boolean`, `Math.*`, `Number.*`, `String.*`, `Object.is`, `Array.isArray`, and so on, but not
  `JSON.stringify`, which calls `toJSON` methods; the list lives in `ts_r_sound` and is
  fixture-tested);
- `await` and `yield` (other code runs in between);
- an assignment to any property or element of any object (it could be an alias), and `delete`.

A property reference stays narrowed across effect points only if every property in its chain is
declared `readonly`. A narrowing of a local `let`/`var`/parameter that is **assigned inside any
function or class body other than its own** (captured and mutated) is dropped at every call,
`await` and `yield`. Locals never assigned from a closure keep TypeScript's behaviour.

| Code | Message | Reported at |
|---|---|---|
| TSR0901 | `'{0}' may have changed since it was narrowed: '{1}' on line {2} can modify it. Copy it to a 'const' before that point.` | the use that only type-checks with the dropped narrowing |
| TSR0902 | `'{0}' may have changed since it was narrowed: it is assigned in a closure, and '{1}' on line {2} can run that closure.` | same |

The checker computes the flow type twice where it matters (with and without dropped narrowings)
and reports only when the result differs in a way that fails a check, so code that never relies
on the narrowing is untouched.

```ts
if (conn.socket !== null) { await flush(); conn.socket.send(m); }  // TSR0901
if (typeof cfg.port === "number") { log("x"); use(cfg.port); }      // TSR0901
let cur: string | null = "a"; const reset = (): void => { cur = null; };
if (cur !== null) { reset(); cur.length; }                          // TSR0902

const s = conn.socket; if (s !== null) { await flush(); s.send(m); } // ok
if (frozen.port !== undefined) { log("x"); use(frozen.port); }      // ok: readonly port
if (o.name !== null) { o.name.trim(); o.name.length; }             // ok: string methods are on the list
```

**`any`/`.d.ts`.** Calls to package functions are effect points (they are not on the list).
Narrowings of values typed by packages behave the same as local ones.

**Escape hatches.** `@ts-r-unsafe S-09: reason`; the idiomatic fix is a `const` copy.

**False-positive risk: high** (the plan marks S-09 with `?`). Class code that narrows
`this.x` and then calls helpers or awaits is common; most of those are fine in practice because
the helper does not touch `x`. It ships as an error (decided 2026-10-15, "Decisions" question 6);
`[sound.rules] "S-09" = "off"` turns it off and marks the guarantee partial.

**Migration.** Auto-fix: introduce `const x = this.x;` before the narrowing and rewrite uses in
the narrowed region. This is mechanical and safe (it only changes reads).

### S-10: No floating promises (level: strict)

**Rule.** In a sound or strict file, an expression statement whose value is promise-like
(has a callable `then`), or an array of promise-likes, must be awaited, returned, or handled:
`.catch(h)` or `.then(f, h)` with a rejection handler. `void p` is allowed only when `p` is a
handled chain (`void p.catch(h)`). An `async` function or a function returning a promise may not
be passed where the parameter's type returns `void` (its promise would float).

| Code | Message | Reported at |
|---|---|---|
| TSR1001 | `Promise from '{0}' is not handled. Await it, return it, or attach '.catch'.` | the expression statement |
| TSR1002 | `'void' discards a promise that can reject. Attach '.catch' first: 'void {0}.catch(...)'.` | the `void` expression |
| TSR1003 | `Async function passed where '{0}' expects a function returning 'void'; its promise is never handled.` | the function argument |

```ts
save(user);                                    // TSR1001
fetch("/ping").then(log);                      // TSR1001: no rejection handler
void save(user);                               // TSR1002
items.forEach(async (i) => { await save(i); }); // TSR1003

await save(user);                              // ok
void save(user).catch(report);                 // ok
fetch("/ping").then(log, report);              // ok
await Promise.all(items.map(async (i) => save(i))); // ok
```

**`any`/`.d.ts`.** Promise-likeness uses the declared types from packages as written.
`UNKNOWN_FROM_ANY` results are not promise-like (can't float). Shares its implementation with the
`no-floating-promises` lint rule; in sound files it is an error whatever the lint configuration.

**Escape hatches.** `@ts-r-unsafe S-10: reason`.

**False-positive risk: low** for TSR1001/TSR1002, **medium** for TSR1003 (event handlers in UI
frameworks: `onClick={async () => ...}` is common and usually wrapped by the framework). See open
question 12.

**Migration.** Auto-fixes: add `await` in async functions, otherwise `void x.catch(reportError)`
with a project-chosen reporter.

### S-11: Dependency trust boundaries (level: sound)

**Rule.** Trust in dependencies is declared by package mode and by `.d.ts` files, never inline in
a sound file. Package modes:

| Mode | Default for | Where its values enter sound code |
|---|---|---|
| `trusted` | packages with types | `.d.ts` types used as written; `any` becomes `unknown` (S-01) |
| `loose` | packages without types (shorthand `declare module "x";`, JS with `allowJs`) | `any` stays `any`; listed as an unchecked boundary in the report |
| `verify` | none (opt in) | like `trusted`, plus dev builds validate returned values against the declared types (`contracts` machinery) |
| `sound` | none (opt in) | the package's own source is held to sound rules (workspace packages) |

Modes are found automatically from `package.json`, the lockfile and `node_modules`, and only
changed in `[sound.packages]`. Non-sound local files are treated like `trusted` packages. In a
sound file, the following are errors because they change what is trusted without a check
(TSR1103 covers `declare` variables, functions, classes, enums and namespaces; `declare module`
is TSR1101 or TSR1102, `declare global` is S-12, and `declare` class fields are S-08):

| Code | Message | Reported at |
|---|---|---|
| TSR1101 | `Augmenting module '{0}' in a sound file changes its trusted types without a check. Move the augmentation to a '.d.ts' file.` | the module name |
| TSR1102 | `Shorthand ambient module '{0}' makes every import 'any'. Declare its types in a '.d.ts' file.` | the module name |
| TSR1103 | `Ambient declaration '{0}' in a sound file asserts a value exists without checking it. Move it to a '.d.ts' file or import the real value.` | the declaration name |

```ts
declare module "typed-pkg" { interface Item { readonly discount: number } } // TSR1101
declare module "tsr-assets-*";                                            // TSR1102 (script file)
declare const API_URL: string;                                            // TSR1103

import { getItem } from "typed-pkg";       // ok: trusted
import { compute } from "loose-pkg";       // ok: loose, listed in the report
const n: number = compute(2);              // ok ("Decisions" question 1; loose-values = "unknown" rejects it)
```

**`any`/`unknown`/`.d.ts`/`node_modules`.** This rule is the definition of the boundary. `.d.ts`
files in the project are trust declarations: they are never sound files and the report lists them
with the packages. `verify` checks happen in dev builds at the import boundary: each call to a
function exported by the package gets its result validated with the validator of the declared
return type (types that can't be validated are skipped and listed in the report).

**Escape hatches.** Package modes; moving declarations to `.d.ts`.

**False-positive risk: low.** Projects keep `declare` and augmentations in `.d.ts` files already
(`env.d.ts`, `global.d.ts`).

**Migration.** Move ambient declarations from sound `.ts` files into one `types/ambient.d.ts`.
The report's "unchecked boundaries" list shows which `loose` packages deserve types or `verify`.

### S-12: No suppression comments, unchecked overload implementations or global augmentation (level: sound)

**Rule.** In sound files: `@ts-ignore`, `@ts-expect-error` and `@ts-nocheck` directives are errors
(they still suppress, so the underlying error is not reported twice); a function or method with
overload signatures and an implementation has its body checked once per overload signature
(decided 2026-10-15, see "Decisions" question 4), because tsc checks the body only against the
implementation signature: called through each overload, every `return` the body can reach, and a
reachable end or bare `return;` as `undefined`, must be assignable to that overload's return
type (constructors return no value, so they have nothing to check); `declare global { ... }` and
global declarations merged from a script file (`interface Array<T> { ... }` at the top level of a
non-module file) are errors.

| Code | Message | Reported at |
|---|---|---|
| TSR1201 | `'@{0}' is not allowed in sound mode. Fix the error, or use '// @ts-r-unsafe: <reason>' for sound-mode errors.` | the directive comment |
| TSR1202 | `'@ts-nocheck' cannot be used in a sound file. Set the file to '// @ts-r off' instead.` | the directive comment |
| TSR1204 | `Global augmentation in a sound file changes types for the whole program without a check. Move it to a '.d.ts' file.` | `global`, or the merged declaration's name |
| TSR1205 | `Overload '{0}' is not satisfied by its implementation: called through it, the body can return '{1}', which is not assignable to '{2}'.` | the `return` (or the implementation's name for a reachable end) |

TSR1203 (the earlier blanket ban on overload implementations) is retired and its number is not
reused.

```ts
// @ts-ignore                                                    // TSR1201
function f(x: string): string;
function f(x: number): string;
function f(x: string | number): string | number { return x; }    // TSR1205: 'f(x: number): string' returns 'number'
declare global { interface Window { appState: string } }         // TSR1204

function h(x: string): string;
function h(x: number): number;
function h(x: string | number): string | number {
  if (typeof x === "string") return x.trim();                    // ok: only the first overload reaches it
  return x + 1;                                                   // ok: only the second
}
interface Format { (x: string): string; (x: number): number }     // ok: no implementation
function g<T extends string | number>(x: T): T { return x; }      // ok
```

How the check works: for each overload, the body is walked statement by statement with every
parameter at the overload's declared type, narrowed by `if` tests, loop conditions and `?:`
branches through the checker's own condition narrowing. A `return` on a path where a parameter
has narrowed to `never` cannot be reached through that overload and is skipped. A mismatch is
reported only when the returned type is exact for the overload: a bare parameter takes its
narrowed overload type, and any other expression is reported only if every parameter it reads is
already within the overload's type there and it reads no local computed from a parameter (nor
`arguments`). Returns inside `switch` and labelled statements are not checked yet, and
constructors have no return to check. So the check reports only what it can show (Known gaps).

**`any`/`.d.ts`.** Overloads and global augmentation in `.d.ts` files and packages are trusted.

**Escape hatches.** `@ts-r-unsafe S-12: reason` above a `return` the per-overload check reports.
Suppression comments have no hatch: use `@ts-r-unsafe` for sound errors and fix tsc errors.

**False-positive risk: low.** The per-overload check reports a return only when its type is
exact for the overload, so correct overloads that narrow their parameters pass unchanged.
Measured on private-app: the 37 implementations the earlier ban (TSR1203) reported were
generated Pulumi constructors, which the per-overload check does not report.

**Migration.** `ts-r-check --sound-report` lists every directive with the error it hides. A
TSR1205 is a real mismatch: fix the branch, or narrow the overload's return type; move
`declare global` blocks to `.d.ts`.

### S-13: Checked exceptions (level: sound)

**Rule.** Throwing is part of a function's signature.

- **Checked exceptions** are the values thrown by `throw` statements in sound files, and the
  types declared in `@throws {T}` JSDoc tags on called functions (from any file or package). A
  callee declared in another file or a package with no `@throws` tag throws nothing checked (lib
  functions, packages, non-sound files, and sound functions in other files without tags). The
  panic helpers `panic()` and `expect()` from `@ts-r.dev/guard` are unchecked, and so are the
  classes listed in `[sound] unchecked-errors` (empty by default).
- A function's **throws set** is: checked exceptions escaping its body (a `throw` or a call not
  inside a `try` whose `catch` handles them), plus the throws set of called functions in the same
  file without a tag (inferred, same-file only, so checking stays per-file). An `async` function's
  throws set is its rejection set; `await f()` propagates `f`'s set.
- `catch (e)` keeps `e: unknown` (runtime errors can arrive), but the checker tracks the checked
  part of `e` separately: narrowing `e` with `instanceof C` removes `C` (and subclasses) from what
  `throw e` re-throws in the other branch. Re-throwing the catch variable is allowed.
- **(a)** A function with `@throws` tags must not let anything escape that is not assignable to
  the union of their types. **(b)** Opt-in, with `[sound] exports-declare-throws = true` (off
  by default, decided 2026-10-15, "Decisions" question 3): an exported function, or a public
  method of an exported class, whose throws set is not empty must declare it. Without it, only
  functions that already have `@throws` tags are held to them, by (a). **(c)** Only `Error` instances may be thrown or
  passed to `Promise.reject` (re-throwing the catch variable excepted). **(d)** An overriding or
  implementing method may not throw more than the member it overrides declares.

| Code | Message | Reported at |
|---|---|---|
| TSR1301 | `'{0}' can throw '{1}', which is not in its '@throws' tags. Catch it or add a '@throws' tag for it.` | the `throw`, or the call it escapes from |
| TSR1302 | `Exported '{0}' can throw '{1}' but has no '@throws' tag. Document it with a '@throws' tag.` | the declaration name (only with `exports-declare-throws = true`) |
| TSR1303 | `Only Error objects can be thrown in sound mode; '{0}' is not assignable to 'Error'.` | the thrown or rejected expression |
| TSR1304 | `'{0}' can throw '{1}', which '{2}' (the member it overrides or implements) does not declare.` | the member name |

```ts
export function port(s: string): number { if (!s) throw new RangeError("empty"); return +s; } // TSR1302 when opted in
/** @throws {NotFound} */ function find(id: string): Row { throw new Timeout(); }            // TSR1301
throw "oops";                                                                                // TSR1303

/** @throws {RangeError} */
export function port(s: string): number { if (!s) throw new RangeError("empty"); return +s; } // ok
export function load(): Row | null { try { return find("a"); } catch { return null; } }       // ok
export function first(xs: readonly number[]): number { return expect(xs[0], "non-empty"); }   // ok
```

`@throws` types resolve in the file's scope like type references (a type-only import is enough).
An unresolved name is TSR1305, `Cannot find type '{0}' in '@throws'.`, reported at the tag's type.
(tsc doesn't resolve JSDoc in `.ts` files, so it reports nothing there.)

**`any`/`unknown`/`.d.ts`.** `@throws` tags in `.d.ts` files and packages are read and trusted
(so `@ts-r.dev/guard`'s `parse` declares `@throws {ParseError}`). Thrown values typed `unknown` (other
than the catch variable) fail (c). Callbacks don't carry throws sets: a callback that throws is
unchecked (Known gaps).

**Escape hatches.** `panic(message)` for bugs; `@ts-r-unsafe S-13: reason`;
`[sound] unchecked-errors` ("Decisions" question 3).

**False-positive risk: low by default, medium with `exports-declare-throws = true`.** With it on,
exported functions that call `parse<T>` must declare `ParseError`, which spreads tags up the call
graph of exported functions, and `throw new Error("unreachable")` in exported code needs
`panic()`. Measured on private-app before the decision: 70 TSR1302, every exported domain
function that throws `DomainError`.

**Migration.** The quick fix inserts the missing `@throws` tags from the computed throws set;
`ts-r fix --sound S-13` applies it across a folder, bottom-up.

### S-14: Data from outside the program is typed `unknown` until parsed (level: sound)

**Rule.** Values from **external sources** have type `UNKNOWN_EXTERNAL` in sound files. A value
of that type may be passed where `unknown` is expected, stored in an `unknown` variable, and
narrowed with `typeof`, `instanceof`, `in`, `Array.isArray` and equality checks (all sound on
`unknown`); any other use (property access, call, assignment to a typed target, return as a
declared type, spread, destructuring, a type assertion) is an error. The typed helpers from
`@ts-r.dev/guard` require an explicit type argument.

External sources (built in; extended with `[sound.sources]`): `JSON.parse`; `.json()` on
`Response`, `Request` and `Body`; `MessageEvent.data` (`postMessage`, `WebSocket`, workers,
`BroadcastChannel`); request bodies, queries and params of the supported server frameworks (by
their package declarations, listed in `ts_r_sound`); and `structuredClone` of an external value.
Strings from `localStorage`, files and `process.env` keep their lib types (`string | null`,
`string | undefined`); they become external when passed to `JSON.parse`, and `env<E>()`,
`stored<T>()` and `readJson<T>()` read and validate them in one step.

| Code | Message | Reported at |
|---|---|---|
| TSR1401 | `Value from external source '{0}' must be parsed before use: write 'parse<T>({1})' with the type you expect.` | the use |
| TSR1402 | `'{0}' needs an explicit type argument in sound mode, e.g. '{0}<User>(...)'.` | the call |

```ts
const user: User = JSON.parse(text);            // TSR1401
const data = await res.json(); data.total;       // TSR1401
const s = JSON.parse(text) as Settings;          // TSR1401 (precedence over S-02)
const u = await fetchJson("/api/user");          // TSR1402

const user = parse<User>(JSON.parse(text));      // ok
const u = await fetchJson<User>("/api/user");    // ok
const raw: unknown = JSON.parse(text);           // ok: no claim made
if (typeof raw === "object" && raw !== null && "v" in raw) log(raw.v); // ok: sound narrowing
```

**`any`/`unknown`/`.d.ts`/`node_modules`.** The lib declares these sources as `any`; S-14 takes
precedence over S-01's mapping for them. Package APIs typed `any` (`axios` `.data`) are S-01
(`UNKNOWN_FROM_ANY`); listing them in `[sound.sources]` makes them S-14 sources with the clearer
message. `verify` packages' returns are validated in dev builds (S-11).

**Escape hatches.** Manual narrowing (allowed); `@ts-r-unsafe S-14: reason`; `safeParse<T>` for
a `Result` instead of an exception.

**False-positive risk: low to medium.** Every hit is a real unchecked boundary; volume is
proportional to the number of `JSON.parse`/`res.json()` sites.

**Migration.** Quick fix: `x as T` / `const v: T = <source>` becomes `parse<T>(<source>)`; the
fix is offered only when `T` is validatable (TSR15xx otherwise).

---

## `parse<T>` runtime validators

### API (`@ts-r.dev/guard`)

`@ts-r.dev/guard` is an npm package with type declarations and a JS stub whose functions throw
`Error("@ts-r.dev/guard: parse<T> was not expanded. Build with ts-r or add the @ts-r.dev/guard plugin.")`.
So code built by another tool without the plugin fails loudly instead of trusting the data.

| Function | Returns | Throws (`@throws`) |
|---|---|---|
| `parse<T>(value: unknown)` | `T` | `ParseError` |
| `safeParse<T>(value: unknown)` | `Result<T>` (`{ ok: true, value } \| { ok: false, error }`) | nothing |
| `is<T>(value: unknown)` | `value is T` | nothing |
| `assert<T>(value: unknown)` | `asserts value is T` | `ParseError` |
| `parseJson<T>(text: string)` | `T` | `ParseError`, `SyntaxError` |
| `fetchJson<T>(input, init?)` | `Promise<T>` | `ParseError` (network errors unchecked) |
| `env<E>()` | `E` (strings converted to `number`/`boolean`/literals) | `ParseError` |
| `body<T>(req)`, `query<T>(req)`, `stored<T>(key)`, `readJson<T>(path)`, `args<T>()` | `T` | `ParseError` |
| `guard<T>()` | `Guard<T>` (`{ parse, is }`), for generic code | nothing |
| `expect<T>(value, message)` | `NonNullable<T>` | unchecked `ExpectError` |
| `panic(message)` | `never` | unchecked `PanicError` |

Recognition is syntactic: a call whose callee resolves through an import from `@ts-r.dev/guard`
(named, renamed or namespace import), with an explicit type argument. Instantiation expressions
(`items.map(parse<User>)`) are supported and become a validator function value. Any other use of
these functions as values is an error (TSR1503). Every value import from `@ts-r.dev/guard`
(`ParseError` for `instanceof`) is rewritten to the inlined runtime, so output has no runtime
dependency on the package.

### Supported types

`T` is resolved from declarations only (`ts_r_decls` index and the resolver), never from the
checker (P4). One evaluator in `ts_r_guard` serves both `ts-r-check` (which reports TSR15xx in
the editor) and the transforms (which report the same errors when building), so they cannot
disagree.

| Supported | Check |
|---|---|
| `string`, `number`, `boolean`, `bigint`, `null`, `undefined`, `symbol` | `typeof` / `===` |
| literal types, `enum` and `const enum` members | `===` against each member (numeric enums only accept their members) |
| object types, interfaces, type literals (incl. `extends`, optional, readonly) | `typeof v === "object" && v !== null`, then each property; optional means missing or `T` (exact optional semantics) |
| index signatures, `Record<K, V>` | every own enumerable key and value |
| arrays, `ReadonlyArray`, tuples (optional and rest elements) | `Array.isArray`, length, each element |
| unions | members in order; **discriminated unions** compile to a `switch` on the discriminant |
| intersections of object types | merged into one object check |
| template literal types | a compiled regular expression |
| generic instantiations of declared types (`Page<User>`) | higher-order validator `$is_Page(v, $is_User)` |
| recursive types | hoisted function declarations; a depth limit (`max-depth`, default 512) reports a `ParseError` instead of overflowing |
| `Partial`, `Required`, `Readonly`, `Pick`, `Omit`, `Record`, `NonNullable`, `Exclude`/`Extract` over literal unions, `keyof` and indexed access on declared object types | evaluated on declarations |
| `Date`, `Map`, `Set`, `RegExp`, `URL`, typed arrays | `instanceof` (plus element checks for `Map`/`Set`) |
| user classes (exported, concrete, non-generic) | `instanceof` against the class, which the validator imports |
| refined types registered in `[sound.guard] refinements` (`Email`, `string & { __brand: "Id" }`) | the base type, then the registered predicate |
| `unknown` | no check |

**User classes** (decided 2026-10-15, "Decisions" question 13) are checked with `v instanceof C`,
so the validator needs the class as a value. The generated code imports it
(`import { <export> as $c_<Name> } from "<spec>"`): from the module name for a class in
`declare module "x"`, else from a specifier the program already uses for the class's file (a
package name as is; the user's relative specifier re-based onto the output file; an absolute path
in shared validator modules). In the declaring file, inline output uses the class's own name.
`instanceof` is exact for classes with private members and stricter than TypeScript's structural
types for others (a plain object with the same shape is rejected, as is an instance from another
realm). TSR1501, each with its own reason: abstract classes (validate a concrete subclass),
generic classes, classes reached only through `import type`/`export type`, `declare class` outside
a `.d.ts`, global classes from script files, and unexported classes (a class must be exported even
when validated only in its own file, so the editor and every output layout agree).

**Refined and branded types** (decided 2026-10-15, "Decisions" question 14) are registered by name:

```toml
[sound.guard]
refinements = { Email = "./validators#isEmail", UserId = "@acme/ids#isUserId" }
```

The key is the name of a type alias or interface; the value is `<module>#<export>`, a relative
path (from the configuration file's directory) or a package name, naming a predicate
`(v: unknown) => v is T`. The validator checks the base type first (for an alias of an
intersection, its non-object members: `string` for `string & { readonly __brand: "Email" }`),
then calls the predicate (`import { <export> as $r_<Type> }`). A failed predicate reports the
refined type's name in the `ParseError`. S-07 cannot prove a predicate that gives a brand (nothing
in the body produces the brand), so the predicate carries the hatch that says it is the
definition: `// @ts-r-unsafe S-07: a string matching the pattern is an Email by definition.`
A primitive intersected with object types with no registered refinement is TSR1501 naming the
alias and the configuration key (written in place, it asks for an alias first). The registered
name matches any declaration with that name; the predicate's existence and signature are not
checked (a missing export fails when the generated import runs).

Not supported, each a TSR1501 with the reason: function and constructor types, `Promise`,
classes other than the ones above, unregistered branded types, `this` types, conditional types
that don't resolve on declarations, mapped types other than the ones above, `typeof` queries and
other types that need the checker (`ReturnType<typeof f>`), unique symbols, `any` and `never`.

| Code | Message |
|---|---|
| TSR1501 | `Type '{0}' cannot be checked at runtime: {1}.` |
| TSR1502 | `'{0}<{1}>' needs a concrete type, but '{1}' is a type parameter. Take a 'Guard<{1}>' argument and call 'guard<User>()' at the call site.` |
| TSR1503 | `'{0}' from '@ts-r.dev/guard' must be called with a type argument; it can't be used as a value.` |
| TSR1504 | `The validator for '{0}' is {1} KB, over the {2} KB budget. Validate a smaller type or raise '[sound.guard] max-validator-kb'.` |

### Generated code

For each validated type the transform emits a **predicate** (fast path, compiled checks, no
allocation) and a **descriptor** (a small array the shared runtime walks only after the predicate
fails, to build the error). The happy path is as fast as hand-written checks (M6 gate: at least
typia's speed); the error path is small. Sketches (names are illustrative; real names are
`$is_<Name>_<8 hex>`):

```ts
interface User {
  id: number; name: string; email?: string;
  role: "admin" | "member"; tags: string[]; manager: User | null;
}
const user = parse<User>(data);
```

```js
import { $parse, $has, $isArr } from "\0ts-r-guard:rt";
function $is_User(v, d = 0) {
  return typeof v === "object" && v !== null && !Array.isArray(v) && d < 512
    && typeof v.id === "number"
    && typeof v.name === "string"
    && (v.email === undefined ? !$has(v, "email") : typeof v.email === "string")
    && (v.role === "admin" || v.role === "member")
    && $isArr(v.tags, $is_string)
    && (v.manager === null || $is_User(v.manager, d + 1));
}
const $d_User = ["o", "User", ["id", "n"], ["name", "s"], ["email", "s", 1],
  ["role", ["l", "admin", "member"]], ["tags", ["a", "s"]], ["manager", ["u", "z", () => $d_User]]];
const user = $parse(data, $is_User, $d_User);
```

Discriminated union:

```ts
type Event = { type: "click"; x: number; y: number } | { type: "key"; key: string };
```

```js
function $is_Event(v) {
  if (typeof v !== "object" || v === null) return false;
  switch (v.type) {
    case "click": return typeof v.x === "number" && typeof v.y === "number";
    case "key": return typeof v.key === "string";
    default: return false;
  }
}
```

Tuple with an optional element, `Record`, template literal, generic:

```js
// [string, number?]
const $is_Pair = (v) => Array.isArray(v) && (v.length === 1 || v.length === 2)
  && typeof v[0] === "string" && (v.length === 1 || typeof v[1] === "number");
// Record<string, number>
function $is_Scores(v) {
  if (typeof v !== "object" || v === null || Array.isArray(v)) return false;
  for (const k of Object.keys(v)) if (typeof v[k] !== "number") return false;
  return true;
}
// `user_${number}`
const $is_UserKey = (v) => typeof v === "string" && /^user_.../.test(v); // regex per tsc's `${number}` rule
// interface Page<T> { items: T[]; next: string | null }
const $is_Page = (v, t) => typeof v === "object" && v !== null
  && $isArr(v.items, t) && (v.next === null || typeof v.next === "string");
const page = $parse(data, (v) => $is_Page(v, $is_User), [/* descriptor */]);
```

`is<T>(v)` emits only the predicate call. `env<E>()` emits a converter over `process.env` or
`import.meta.env` (by target) that reads each declared key once, converts `number` with
`Number(s)` (must be finite and match `/^-?\d+(\.\d+)?$/`) and `boolean` from
`"true"|"false"|"1"|"0"`, and throws one `ParseError` listing every bad key.

Semantics: `parse<T>` returns the **same object** it was given (no copy, no stripping); extra
properties are kept, as TypeScript's structural types allow ("Decisions" question 11). Each property is
read once per predicate run except optional ones (read, then `$has` if `undefined`).

### Error shape

```ts
class ParseError extends Error {
  readonly name: "ParseError";
  readonly type: string;                     // "User"
  readonly issues: readonly ParseIssue[];    // at most 20, in property order
}
interface ParseIssue {
  readonly path: readonly (string | number)[]; // ["roles", 0, "name"]
  readonly expected: string;                   // "string"
  readonly received: string;                   // "number" | "null" | "array" | "object" | "missing" | ...
}
```

`message` is `Invalid User: $.roles[0].name: expected string, received number (+2 more)`.
Received **values are never included**, only their kinds, so logs don't leak user data.
`[sound.guard] explain = "first"` keeps only the first issue and `"none"` drops descriptors
entirely (`Invalid User`), for size-sensitive bundles.

### Size budget

- Runtime (`$parse`, `$has`, `$isArr`, the descriptor walker, `ParseError`): **at most 1.0 KB
  minified, 0.5 KB gzip**, emitted once per output chunk.
- Per type (proposed, to be confirmed by measurement in M6): predicate plus descriptor at most
  **about twice the minified size of the type declaration**; a 10-property flat interface at most
  400 bytes minified. A validator over 8 KB (configurable) is TSR1504.
- Gate: `crates/ts_r_guard/tests/size/` pins the minified size of a fixed set of types; more
  than 5% growth fails like any benchmark gate.

### Bundling and tree shaking

- Each validated type becomes a **virtual module** `\0ts-r-guard:<relative decl file>#<Name><args>`
  exporting `$is_X` and `$d_X`; the runtime is `\0ts-r-guard:rt`. The id is the declaration's
  identity, not a content hash, so editing `User` in dev replaces one module and HMR updates its
  importers without re-transforming every call site.
- `ts-r-build` treats them as ordinary side-effect-free modules: one copy per output, shared by
  every call site in that chunk graph; validators for types nobody parses never exist, and
  `parse<T>` calls in dead code take their validator with them.
- Library mode (unbundled output) and `ts-r-run` inline the validators into each file that uses
  them (one copy per file), since published JS can't import virtual modules.
- The transform cache key of a file with `parse<T>` calls includes the decls hashes
  ([0005](0005-history.md)) of every declaration its validators read, so changing `User`
  re-generates the validator.
- The Vite, Rollup and esbuild plugins implement the same virtual modules.

## Configuration

`ts-r.toml` (canonical, per the plan):

```toml
[sound]
level = "sound"               # level for included files: "strict" | "sound" | "contracts"
include = ["src/**"]
exclude = ["src/legacy/**", "**/*.test.ts"]
unchecked-errors = []         # S-13: extra error classes treated as unchecked
exports-declare-throws = false # S-13: true makes exported functions declare '@throws' (TSR1302)

[sound.packages]
default = "trusted"           # typed packages
untyped = "loose"             # packages without types
"lodash" = "verify"
"@acme/core" = "sound"

[sound.sources]               # extra S-14 external sources: package = [exported paths]
"@acme/rpc" = ["Client.call"]

[sound.guard]
explain = "full"              # "full" | "first" | "none"
max-depth = 512
max-validator-kb = 8
refinements = { Email = "./src/validators#isEmail" } # predicates for refined types (S-14 validators)
```

The policy switches the decisions keep (each defaulting to the decision; see "Decisions") are
further `[sound]` keys: `loose-values`, `code-prefix`, `exports-declare-throws`,
`invariant-properties`, `strict-lib-methods`, `constructor-escape`, `async-void-callbacks`,
`guard-false-side`, `forced-flag-codes`, and `[sound.rules]` for per-rule switches.

The configuration lives in one of three places (decided 2026-10-15, "Decisions" question 16), all
read into one tree, so they accept exactly the same keys:

- the `[sound]` table of `ts-r.toml` (the nearest in the tsconfig's directory or an ancestor);
- a `tsrconfig.json` (the nearest, likewise): JSONC, `{ "sound": { ... } }`;
- a top-level `"ts-r": { "sound": { ... } }` key in `tsconfig.json` (tsc ignores unknown
  top-level keys, unlike unknown `compilerOptions`, which are TS5023).

Configuring sound mode in more than one of them is TSR0020, one error naming every file involved
(`sound mode is configured in more than one place: the "ts-r" key of 'tsconfig.json' and
'ts-r.toml'; keep one`), reported on the one still used: the tsconfig key, else `tsrconfig.json`.
Relative paths (`refinements` modules) are relative to the file they are written in. Nothing goes
in `compilerOptions`.

**A file's level** is the first that applies: (1) its pragma, `// @ts-r off | strict | sound |
contracts`, in the leading comments before the first statement (TSR0004, TSR0005); (2) `exclude`
gives `off`; (3) `include` gives `level`; (4) otherwise `off`. `.d.ts`, `.js`/`.jsx`/`.mjs`/
`.cjs` files and `node_modules` are always `off` (a pragma there is ignored). Project references
use each project's own configuration.

Forced compiler options per level: `strict`, `noUncheckedIndexedAccess`,
`exactOptionalPropertyTypes`, `isolatedDeclarations` (and `useUnknownInCatchVariables` through
`strict`). They are forced per file, not for the program, so non-sound files keep the tsconfig's
behaviour.

## Reporting

**ts-r-check.** Sound diagnostics are in the same stream and format as tsc's:

```text
src/users.ts(12,17): error TSR0201: Type assertion from 'unknown' to 'User' narrows the type. Use 'parse<User>', 'is<User>' or a narrowing check instead.
```

Tools that only know `error TS\d+` (VS Code's `$tsc`, CI log parsers) don't match `TSR`, so ts-r
ships its own problem matchers (decided 2026-10-15, "Decisions" question 2): `$ts-r` in the VS
Code extension and `problem-matcher.json` in the `ts-r` npm package for GitHub Actions
(`echo "::add-matcher::node_modules/ts-r/problem-matcher.json"`). Both read `TS` and `TSR` codes
in the plain and the pretty form. `[sound] code-prefix = "TS"` prints the numeric form
(`TS100201`) for tools that can't be given a matcher.

The summary line counts them separately (`Found 7 errors (3 sound-mode) in 2 files.`). Exit codes
match tsc's. `--sound=off` checks every file at `off`, for tsc parity runs.
`--sound-report[=json]` prints coverage: files and lines per level, every `@ts-r-unsafe` (file,
line, rules, reason), unchecked boundaries (`loose` packages used, project `.d.ts` files, non-sound
local imports used by sound files), validators generated, and per-rule diagnostic counts.

**ts-r-lsp.** `publishDiagnostics` with `source: "ts-r sound"`, `code: "TSR0201"` and
`codeDescription.href` pointing at the rule's docs page (`/sound/rules/S-02` on the site). Quick
fixes per rule as listed under Migration, plus "Add `@ts-r-unsafe` with a reason" (the cursor lands
in the reason). The file's level shows in the status bar. When a project runs on the tsc fallback
backend (D6), sound mode is unavailable and the editor says so once.

**ts-r-build / ts-r-dev / ts-r-run.** Builds don't run the checker. `ts-r-build` reads the check
history ([0005](0005-history.md)): if the last result for a sound file at its current semantic hash
has errors, the build fails and prints them; if there is no up-to-date result it prints a "not
checked" warning and continues, or fails with `--require-check` (decided 2026-10-15, "Decisions"
question 15; the check-history integration in `ts-r-build` is not built yet). `ts-r-dev`
shows sound errors in the overlay but keeps serving.

**CI.** `ts-r ci` runs check with history, fails on any sound error, emits GitHub annotations
(`--format github`) and uploads the JSON report. `--sound-baseline <file>` fails if the number
of `@ts-r-unsafe` hatches or unchecked boundaries per file increases, or coverage drops (a ratchet,
not a suppression list).

## Changes to other records

These need human approval along with this record:

- **[0004](0004-types-and-checker.md)**: two more fixed intrinsics (`UNKNOWN_FROM_ANY`,
  `UNKNOWN_EXTERNAL`); sound relation kinds with their own caches; variance per (declaration,
  relation mode); the `AnyToUnknown` mapper; flow-type queries with "drop property narrowings"
  (S-09) and "type of `x` when `e` is true/false" (S-07); query API additions
  `is_assignable_sound`, `narrowed_type_at`, `throws_set_of`.
- **[0005](0005-history.md)**: the semantic file hash must include directive comments (`@ts-r`
  pragmas, `@ts-r-unsafe`, and `@ts-ignore`/`@ts-expect-error`/`@ts-nocheck`, which already
  change tsc's diagnostics today, so this is a fix for 0005 regardless of sound mode) and
  `@throws` tags in every file (S-13 reads a callee's tags whatever its file's level, and 0005
  recomputes signature hashes only for files whose semantic hash changed); the export signature
  hashes must include `@throws` tags of exported declarations and the effective level of their
  file (S-06 depends on the level of the file a method is declared in, so a pragma change must
  reach the files that use its exports); the manifest key must include the canonical
  `[sound]` configuration and resolved package modes. Reading a callee's `@throws` goes through
  the dependency recorder.

## Invariants agents must keep

1. Sound mode only adds diagnostics: every case file is clean under tsc 7 `strict: true`, and a
   sound file's tsc diagnostics are a subset of ts-r's for that file (CI test over the cases).
2. Sound diagnostics are `TSR` codes, numerically `100_000 + n`, always errors, at most one per
   node, chosen by the precedence order above.
3. `@ts-r-unsafe` suppresses only `TSR` codes, needs a reason, and is reported when unused.
4. No rule needs more than the file and the declarations it imports; no whole-program pass.
5. `any` in shared type store entries is never rewritten; the `any`-to-`unknown` view is a mapper
   applied at sound read sites.
6. `parse<T>` expansion never runs the checker; check and build use the one `ts_r_guard`
   evaluator, so they report the same TSR15xx errors.
7. Generated validators are deterministic (same input, same bytes), side-effect free at module
   level, and never include received values in errors.
8. Published output (`.js`, `.d.ts`) is identical in content to what non-sound compilation emits,
   except expanded `parse<T>` calls.

## Known gaps (not closed by v1)

Recorded so nobody claims more than the rules give: mutable **object properties** are still
covariant (`{ pet: Dog }` to `{ pet: Animal }`; "Decisions" question 5, `invariant-properties =
true` closes it); `readonly` properties can be aliased by mutable types; writes to a property of a
union-typed object; `in` narrowing with width subtyping (an object with an extra `swim`
property); the false branch of `instanceof` with structurally equal classes; getters with side
effects (S-09); callbacks don't carry `@throws` (S-13); exported functions need not declare what
they throw unless `exports-declare-throws` is on (S-13); the per-overload check skips returns
whose type it can't make exact for the overload (a value derived from a parameter, such as
`xs.length` under a narrower rest parameter) and returns inside `switch` and labelled statements
(S-12); refinement predicates are trusted, not proved (S-14 validators); `eval`, `Proxy`,
prototype mutation, native addons (plan: limits of the guarantee).

## Alternatives

- **Rules as lint rules only (typescript-eslint's `no-unsafe-*`, `no-non-null-assertion`, ...).**
  They cover S-01, S-03, S-10 and part of S-02, but not relations (S-05, S-06), flow (S-07, S-09)
  or validators, and they can be turned off one at a time. Rejected: the guarantee needs the
  checker's relations and one switch per file.
- **A sound dialect with new syntax (`x is! T`, `throws` clauses).** Rejected by P7.
- **Banning `unknown` narrowing until `parse<T>` (plan wording for S-14).** Manual narrowing on
  `unknown` is already sound; banning it adds friction without closing a hole.
- **Content-hash validator modules.** Better cross-type dedup, but every edit to a type
  re-transforms every call site. Declaration-identity ids keep HMR to one module.
- **Validators as interpreted descriptors only.** Smallest output, but every value pays for
  interpretation, which is unlikely to meet the typia-speed gate (to be measured in M6). Compiled
  predicate plus descriptor for errors is the middle.
- **Banning overload implementations (the first S-12 proposal, TSR1203).** Rejected 2026-10-15
  ("Decisions" question 4): library authors would have to rewrite every overload set; checking
  the body once per overload keeps them and closes the hole it was meant to close.
- **Rendering sound codes as `TS100201` by default.** Rejected 2026-10-15 ("Decisions" question
  2): `TSR` says where an error comes from; problem matchers are cheap to ship.

## Decisions

The owner answered every open question on 2026-10-15. Each entry keeps the question, then the
decision. Where a decision keeps a switch, it is a `[sound]` key defaulting to the decision
(`crates/ts_r_sound/src/config.rs`, `Policy`).

1. **`loose` packages in sound files.** The plan says `any` from untyped packages stays `any`
   ("nothing breaks"), so a loose value can flow into `User` unchecked. Should sound files see
   loose values as `unknown` too, or keep the plan?
   **Resolved 2026-10-15: keep the plan.** `any` from `loose` packages stays `any` (the S-11
   accept case `loose-value-flows.ts` pins it), and every loose package used is listed in the
   report. `loose-values = "unknown"` opts in to the stricter view.
2. **Code prefix.** `TSR####` doesn't match existing `error TS\d+` problem matchers (VS Code's
   `$tsc`, CI log parsers). Keep `TSR` and ship our own matcher, or render the numeric form?
   **Resolved 2026-10-15: keep `TSR####` and ship our own problem matchers.** `$ts-r` is
   contributed by the VS Code extension (`editors/vscode/package.json`), and
   `problem-matcher.json` ships in the `ts-r` npm package for GitHub Actions; both read `TS` and
   `TSR` codes in the plain and pretty forms (see Reporting). `code-prefix = "TS"` prints
   `TS100201` for tools that can't take a matcher.
3. **Unchecked exceptions (S-13).** Only `panic()`/`expect()`, or also `TypeError`/`RangeError`
   or a configurable list? Must exported functions declare `@throws`, or is that only an error
   when the function already has tags?
   **Resolved 2026-10-15, in two parts.** (a) Unchecked exceptions: only `panic()` and
   `expect()`, plus the configurable `unchecked-errors` list (empty by default). (b) Exported
   functions: **only functions that already have `@throws` tags are held to them** (TSR1301).
   Requiring every exported function to declare what it throws (TSR1302, rule (b) of S-13) is
   opt-in with `exports-declare-throws = true`. This reverses this record's first proposal, which
   made (b) always on; on private-app it produced 70 TSR1302, one for every exported domain
   function that throws `DomainError`. The S-13 TSR1302 cases moved to `accept/` with an
   `// @opt-in TSR1302` marker, and the case harness checks them again with the switch on.
4. **Overloads (S-12).** Ban implementations, or check the body once per overload signature?
   **Resolved 2026-10-15: check the body once per overload signature** (S-12, TSR1205
   `Overload '{0}' is not satisfied by its implementation: ...`, naming the overload). The ban
   (TSR1203) is retired and its number is not reused.
5. **Mutable object properties (S-05 scope).** Make mutable properties of types declared in sound
   files invariant too?
   **Resolved 2026-10-15: no (the proposal).** Only built-in containers are invariant; object
   properties stay covariant (Known gaps). `invariant-properties = true` opts in.
6. **S-09 severity.** Ship S-09 as an error in v1 (D2), or ship its auto-fix first?
   **Resolved 2026-10-15: an error in v1 (D2).** `[sound.rules] "S-09" = "off"` turns it off,
   and the report marks the guarantee partial.
7. **S-06 scope.** Keep lib and package methods bivariant, or apply strict method checks to all
   comparisons in sound files?
   **Resolved 2026-10-15: keep lib, `.d.ts` and package methods bivariant (the proposal).**
   `strict-lib-methods = true` opts in to the strict check.
8. **TSR0803.** Is "no `this` escape to overridable code in constructors, even after the class's
   own fields are set" worth its noise, or should only escapes before field initialisation
   (TSR0802) be errors?
   **Resolved 2026-10-15: keep TSR0803 on (the proposal).** `constructor-escape =
   "before-init"` limits S-08 to TSR0802.
9. **Suppression directives in sound files.** Report and still suppress, or stop suppressing?
   **Resolved 2026-10-15: report (TSR1201) and still suppress (the proposal)**, so the hidden
   error is not reported twice.
10. **Per-rule switches.** Allow `[sound.rules] "S-09" = "off"`, or only levels and hatches?
    **Resolved 2026-10-15: allowed.** The report marks the guarantee as partial for every rule
    turned off.
11. **`parse<T>` and extra properties.** Keep, strip into a copy, or reject? Return the input or
    a copy?
    **Resolved 2026-10-15: keep extra properties and return the input (the proposal)**, as
    TypeScript's structural types allow. There is no `exact` option.
12. **TSR1003 (async callbacks where `void` is expected).** Keep, or exempt known framework
    event-handler props?
    **Resolved 2026-10-15: keep it everywhere, JSX handlers included (the proposal).**
    `async-void-callbacks = "allow-jsx-handlers"` exempts JSX event-handler props.
13. **Classes in `parse<T>`.** Reject (plan), or validate with `instanceof`?
    **Resolved 2026-10-15: validate with `instanceof`** against the class, which the generated
    validator imports (Supported types). Abstract classes, generic classes, classes reached only
    through `import type`/`export type`, `declare class` outside a `.d.ts`, global classes and
    unexported classes are TSR1501, each with its own reason.
14. **Branded and refined types** (`Email`, `Int`, `string & { __brand: "Id" }`): support
    registered refinement functions in `[sound.guard]`, or leave them as TSR1501 in v1?
    **Resolved 2026-10-15: registered refinements**, `[sound.guard] refinements = { Email =
    "./validators#isEmail" }`: the validator checks the base type, then calls the predicate
    (Supported types). An unregistered brand is TSR1501 with a hint naming the alias and the
    configuration key.
15. **Building without a check result.** Should `ts-r-build` warn and continue, or fail when a
    sound file has no up-to-date check result?
    **Resolved 2026-10-15: warn and continue (the proposal); `--require-check` fails.** Not
    built yet: `ts-r-build` does not read the check history.
16. **Configuration home.** `ts-r.toml` only, or also the `"ts-r"` key in `tsconfig.json`?
    **Resolved 2026-10-15: three places**, `ts-r.toml`, `tsrconfig.json` (JSONC, the shape of the
    tsconfig key) and the tsconfig `"ts-r"` key. More than one is TSR0020 naming every file
    (Configuration).
17. **S-07 strictness.** Proof requires both the true and false side; is requiring the false side
    acceptable?
    **Resolved 2026-10-15: yes, both sides (the proposal).** `guard-false-side = false` checks
    only the true side.
18. **Forced-flag codes.** Re-code forced `noUncheckedIndexedAccess`/`exactOptionalPropertyTypes`/
    `isolatedDeclarations` errors as TSR, or always report tsc's codes?
    **Resolved 2026-10-15: re-code them (the proposal)** as TSR0401, TSR0402, TSR0010 and TSR0011.
    `forced-flag-codes = "TS"` keeps tsc's codes.

## Status

Proposed 2026-10-15 by the M6 sound-spec agent. Accepted 2026-10-15: the owner resolved every
open question (Decisions), and the record is implemented in `ts_r_sound`, `ts_r_guard`,
`ts-r-check`, `ts-r-lsp`, `ts-r-dev` and `ts-r-run`. The cases in
[0007-sound-mode/cases/](0007-sound-mode/cases/README.md) are 113 accept and 121 reject files over
the 14 rules plus escape hatches; all are clean under tsc 7.0.2 with `strict: true` (checked by
the cases' `check-with-tsc.mjs`), and `ts_r_sound`'s case harness passes all 234. Not built yet:
`ts-r-build`'s check-history integration (question 15).
