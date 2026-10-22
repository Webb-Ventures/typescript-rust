---
title: Sound mode
description: An opt-in mode where a static type is a guarantee about the runtime value. 14 rules, parse<T> validators, escape hatches. Pre-alpha.
nav: Sound mode
section: Reference
order: 1
badges: ["ok:Accepted", "warn:Pre-alpha"]
---

## Status

Its design record, [0007: Sound mode](../../docs/design/0007-sound-mode.md), was accepted on
2026-10-15, once the owner had answered all of its open questions (see
[Decisions](#decisions) below). `ts-r-check` and `ts-r-lsp` report the 14 rules. `ts-r-build`,
`ts-r-dev` and `ts-r-run` expand `parse<T>` calls. It is pre-alpha.
Parts not built yet are marked below.

## The problem

TypeScript's types describe what a value probably is. Many common patterns let a value's real
shape drift away from its type without an error: `any`, `x as User`, `x!`, `JSON.parse`,
unchecked index access, array covariance, type guards that lie, and so on. The compiler is fine
with all of them, and the bug shows up at run time.

In a file that opts in to sound mode, a static type is a guarantee about the runtime value,
unless a written escape hatch says otherwise.

Sound mode adds no syntax. Every sound file is ordinary TypeScript that tsc accepts, and sound
mode only adds errors. tsc with `strict: true` is the lower bound: code tsc rejects stays rejected.

## Levels

| Level | What it turns on |
| --- | --- |
| `off` | Plain TypeScript, the tsconfig as written |
| `strict` | `strict`, `noUncheckedIndexedAccess`, `exactOptionalPropertyTypes`, `isolatedDeclarations`, and rules S-01, S-04, S-10 |
| `sound` | `strict` plus S-02, S-03, S-05 to S-09, and S-11 to S-14 |
| `contracts` | `sound` plus runtime checks in dev builds |

The forced compiler options apply per file, not to the whole program, so files that don't opt
in keep your tsconfig's behaviour.

## Opting in

Nothing changes until you opt in. A file's level is the first of these that applies:

1. A pragma in the file's leading comments: `// @ts-r off`, `// @ts-r strict`,
   `// @ts-r sound` or `// @ts-r contracts`.
2. The file matches `exclude`: `off`.
3. The file matches `include`: the configured `level`.
4. Otherwise `off`.

`.d.ts` files, JavaScript files and everything under `node_modules` are always `off`, whatever
the configuration says. The rules check how their values are used in sound files.

## Dependencies

Your dependencies are never held to the rules. Each package gets a mode, found from
`package.json`, the lockfile and `node_modules`:

| Mode | Default for | What it means in sound files |
| --- | --- | --- |
| `trusted` | packages with types | Their `.d.ts` types are used as written; `any` becomes `unknown` |
| `loose` | packages without types | `any` stays `any`; listed as an unchecked boundary in the report |
| `verify` | none (opt in) | Like `trusted`, and dev builds check returned values against the declared types |
| `sound` | none (opt in) | The package's own source is held to the rules (workspace packages) |

## The 14 rules

Each rule has its own diagnostic codes. The table links the main one.

| Rule | Level | Main code |
| --- | --- | --- |
| S-01: No `any` | strict | [TSR0101](errors/TSR0101.md) |
| S-02: No narrowing type assertions | sound | [TSR0201](errors/TSR0201.md) |
| S-03: No non-null assertions | sound | [TSR0301](errors/TSR0301.md) |
| S-04: Checked index access and exact optional properties | strict | [TSR0401](errors/TSR0401.md), [TSR0402](errors/TSR0402.md) |
| S-05: Mutable built-in containers are invariant | sound | [TSR0501](errors/TSR0501.md) |
| S-06: Methods are checked contravariantly | sound | [TSR0601](errors/TSR0601.md) |
| S-07: Type guards must be proved | sound | [TSR0701](errors/TSR0701.md) |
| S-08: Fields are initialised before `this` escapes | sound | [TSR0801](errors/TSR0801.md), [TSR0802](errors/TSR0802.md) |
| S-09: Narrowings are dropped when they could be invalidated | sound | [TSR0901](errors/TSR0901.md) |
| S-10: No floating promises | strict | [TSR1001](errors/TSR1001.md) |
| S-11: Dependency trust boundaries | sound | [TSR1101](errors/TSR1101.md), [TSR1102](errors/TSR1102.md), [TSR1103](errors/TSR1103.md) |
| S-12: No suppression comments, unchecked overload implementations or global augmentation | sound | [TSR1201](errors/TSR1201.md), [TSR1205](errors/TSR1205.md) |
| S-13: Checked exceptions | sound | [TSR1301](errors/TSR1301.md), [TSR1302](errors/TSR1302.md) (opt-in) |
| S-14: Data from outside the program is `unknown` until parsed | sound | [TSR1401](errors/TSR1401.md), [TSR1402](errors/TSR1402.md) |

Problems with generating a `parse<T>` validator are [TSR1501](errors/TSR1501.md) to
[TSR1504](errors/TSR1504.md). Each rule's full text, with its false-positive risk and migration
path, is in [the design record](../../docs/design/0007-sound-mode.md#the-rules).

A short example of S-14:

```ts
const user: User = JSON.parse(text);            // TSR1401
const u = await fetchJson("/api/user");          // TSR1402: needs a type argument

const user = parse<User>(JSON.parse(text));      // ok
const u = await fetchJson<User>("/api/user");    // ok
const raw: unknown = JSON.parse(text);           // ok: no claim made
```

## Escape hatches

- `// @ts-r-unsafe: <reason>` on its own line suppresses every sound diagnostic that starts on
  the next line. It never suppresses tsc's own `TS` errors.
- `// @ts-r-unsafe S-02 S-03: <reason>` limits it to those rules.
- The reason is required ([TSR0001](errors/TSR0001.md)). A hatch that suppresses nothing is
  [TSR0002](errors/TSR0002.md), so hatches can't outlive the code they excuse. An unknown rule
  name is [TSR0003](errors/TSR0003.md).
- A file pragma (`// @ts-r off` or `// @ts-r strict`) lowers one file's level.
- Package modes change how much a dependency is trusted.

`@ts-ignore`, `@ts-expect-error` and `@ts-nocheck` are not escape hatches in sound files; they
are errors under S-12. Every hatch is listed in the coverage report with its file, line, rules
and reason.

## parse<T> and @ts-r.dev/guard

`@ts-r.dev/guard` is an npm package of type declarations and a small stub (not published yet). ts-r expands
each call into a validator generated from the type you name, so `parse<User>(data)` checks
`data` against `User` at run time and returns it typed. The generated code is inlined, so the
output doesn't depend on the package at run time.

| Function | Returns | Throws |
| --- | --- | --- |
| `parse<T>(value)` | `T` | `ParseError` |
| `safeParse<T>(value)` | `{ ok: true, value } \| { ok: false, error }` | nothing |
| `is<T>(value)` | `value is T` | nothing |
| `assert<T>(value)` | `asserts value is T` | `ParseError` |
| `parseJson<T>(text)` | `T` | `ParseError`, `SyntaxError` |
| `fetchJson<T>(input, init?)` | `Promise<T>` | `ParseError` |
| `env<E>()` | `E`, with strings converted to numbers, booleans and literals | `ParseError` |
| `body<T>(req)`, `query<T>(req)`, `stored<T>(key)`, `readJson<T>(path)`, `args<T>()` | `T` | `ParseError` |
| `guard<T>()` | `{ parse, is }` for generic code | nothing |
| `expect<T>(value, message)` | the value, not null or undefined | unchecked `ExpectError` |
| `panic(message)` | `never` | unchecked `PanicError` |

The validator is built from declared types only, never from inference, and the call needs an
explicit type argument. If code that uses `@ts-r.dev/guard` is built by another tool without the
plugin, the stub throws instead of trusting the data.

Besides primitives, literals, enums, objects, arrays, tuples, unions, template literals and the
common utility types, `parse<T>` handles:

- **Your classes**, checked with `instanceof`. The validator imports the class, so it must be
  exported, concrete and not generic. Abstract classes, classes imported with `import type`,
  and unexported classes are [TSR1501](errors/TSR1501.md).
- **Branded and refined types**, such as `type Email = string & { readonly __brand: "Email" }`,
  when you register a predicate for them (below). A branded type with no registered predicate
  is [TSR1501](errors/TSR1501.md).

```toml
[sound.guard]
refinements = { Email = "./src/validators#isEmail" }
```

```ts
// src/validators.ts
export type Email = string & { readonly __brand: "Email" };

// @ts-r-unsafe S-07: a string matching the pattern is an Email by definition.
export function isEmail(v: unknown): v is Email {
  return typeof v === "string" && /^[^@\s]+@[^@\s]+$/.test(v);
}
```

`parse<Email>(x)` checks the base type (`string`), then calls `isEmail`. The path is relative to
the configuration file. Nothing in the predicate's body can prove a brand (S-07), so the
predicate carries an `@ts-r-unsafe` hatch that says why it is the definition.

## Configuration

In `ts-r.toml`:

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
refinements = { Email = "./src/validators#isEmail" }
```

The same settings can live in one of three places, with the same keys in each:

- the `[sound]` table of `ts-r.toml`, as above;
- a `tsrconfig.json` next to your tsconfig, with comments allowed:
  `{ "sound": { "level": "sound", "include": ["src/**"] } }`;
- a top-level `"ts-r": { "sound": { ... } }` key in `tsconfig.json` (tsc ignores unknown
  top-level keys).

Use only one. Configuring sound mode in more than one of them is
[TSR0020](errors/TSR0020.md), which names every file involved. Nothing goes in
`compilerOptions`.

## Reporting

- **ts-r-check** prints sound diagnostics in the same stream and format as tsc's errors, for
  example `src/users.ts(12,17): error TSR0201: ...`. The summary line counts them separately,
  and exit codes match tsc's. `--sound=off` checks every file at `off`. `--sound-report` (or
  `--sound-report=json`) prints coverage: files and lines per level, every `@ts-r-unsafe`,
  unchecked boundaries, validators generated, and counts per rule.
- **ts-r-lsp** publishes them with source `ts-r sound`, a link to the rule's page, and quick
  fixes for several rules, including "Add `@ts-r-unsafe` with a reason".
- **Problem matchers.** Matchers written for tsc look for `error TS` and miss `TSR` codes, so
  ts-r ships its own. In VS Code tasks, use `"problemMatcher": "$ts-r"` (from the ts-r
  extension). In GitHub Actions, add `echo "::add-matcher::node_modules/ts-r/problem-matcher.json"`
  before running `ts-r-check`. Both read `TS` and `TSR` errors.
- **ts-r-build, ts-r-dev and ts-r-run** don't run the checker; they expand `parse<T>` calls.
  Planned, not built yet: `ts-r-build` reads the last check result, fails when a sound file's
  last result has errors, and warns (or fails with `--require-check`) when there is none.
  `ts-r-dev` shows them in the overlay and keeps serving.
- **ts-r ci** (planned) fails on any sound error. `--sound-baseline <file>` fails when hatches or
  unchecked boundaries per file go up or coverage goes down.

## Decisions

The owner answered the record's 18 open questions on 2026-10-15. The ones that change what you
write:

- **Overloads (S-12).** Overload implementations are allowed. The body is checked once per
  overload signature, and a branch that returns the wrong type for an overload is
  [TSR1205](errors/TSR1205.md), which names the overload.
- **`@throws` (S-13).** Only functions that already have `@throws` tags are held to them.
  Requiring exported functions to declare what they throw ([TSR1302](errors/TSR1302.md)) is
  opt-in with `exports-declare-throws = true`. Only `panic()`, `expect()` and the classes in
  `unchecked-errors` are unchecked.
- **Classes and branded types in `parse<T>`.** Classes are checked with `instanceof`. Branded
  types need a registered refinement (see above).
- **Configuration.** `ts-r.toml`, `tsrconfig.json` or the tsconfig `"ts-r"` key, one of them.
- **Codes.** Sound errors keep the `TSR` prefix, and ts-r ships problem matchers for it.
  `[sound] code-prefix = "TS"` prints the numeric form (`TS100201`) instead.
- **Defaults kept as proposed.** Values from untyped (`loose`) packages stay `any`. Mutable
  object properties stay covariant (only built-in containers are invariant). S-09 is an error.
  Single rules can be switched off in `[sound.rules]`, and the report marks the guarantee as
  partial. `parse<T>` keeps extra properties and returns its input. A build without an
  up-to-date check result warns and continues.

Each decision, with its switch, is in
[the design record](../../docs/design/0007-sound-mode.md#decisions).

## Test cases

The rules come with test cases written before any code: 111 files that must be accepted and
120 that must be rejected, all clean under tsc 7.0.2 with `strict: true`. All 231 pass. They are
described in [the cases README](../../docs/design/0007-sound-mode/cases/README.md).
