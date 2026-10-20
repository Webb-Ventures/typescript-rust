# Sound-mode test cases

The cases for [0007: Sound mode](../../0007-sound-mode.md). People review them with the record;
the M6 implementation agent turns them into a test harness (the `add-sound-rule` skill). They are
ordinary TypeScript files: every one is valid under tsc 7 with `strict: true`, and sound mode only
adds errors.

## Layout

```text
cases/
  tsconfig.json              pass 1 of the tsc check: strict: true
  tsconfig.sound-flags.json  pass 2: plus the flags sound mode forces
  check-with-tsc.mjs         runs both passes and checks the layout
  _shared/
    sound.toml               the ts-r.toml every case is checked with
    models.ts                shared types (a sound file, clean)
    legacy.ts                a non-sound local file (// @ts-r off)
    guard.d.ts               declarations of @ts-r.dev/guard
    validators.ts            the refinement predicate sound.toml registers (isEmail)
    packages.d.ts            stand-ins for node_modules packages
    ambient.d.ts             a project .d.ts with a global
  s01-no-any/                one directory per rule, S-01 to S-14
    accept/*.ts              must produce no diagnostics at all
    reject/*.ts              must produce exactly the marked diagnostics
  ...
  s14-external-data/         also holds the parse<T> generation cases (TSR15xx)
  general/                   escape hatches and pragmas (TSR00xx)
```

| Directory | Rule | Accept | Reject |
|---|---|---:|---:|
| `s01-no-any` | S-01 no `any` | 7 | 9 |
| `s02-no-narrowing-casts` | S-02 no narrowing assertions | 7 | 9 |
| `s03-no-non-null` | S-03 no `!` | 7 | 7 |
| `s04-index-and-optional` | S-04 checked index, exact optional | 7 | 8 |
| `s05-container-variance` | S-05 invariant mutable containers | 7 | 9 |
| `s06-method-variance` | S-06 contravariant methods | 6 | 7 |
| `s07-proved-guards` | S-07 proved type guards | 8 | 7 |
| `s08-initialisation` | S-08 fields before `this` escapes | 6 | 8 |
| `s09-narrowing-invalidation` | S-09 narrowing dropped by effects | 7 | 8 |
| `s10-floating-promises` | S-10 no floating promises | 7 | 8 |
| `s11-trust-boundaries` | S-11 dependency trust | 6 | 7 |
| `s12-no-suppression` | S-12 no suppression, overloads checked per signature, global augmentation | 12 | 9 |
| `s13-checked-exceptions` | S-13 checked exceptions | 11 | 5 |
| `s14-external-data` | S-14 typed external data, `parse<T>` | 11 | 15 |
| `general` | escape hatches and pragmas | 4 | 5 |
| **total** | | **113** | **121** |

## File format

- The first comment line says what the case shows. If the file sets its level, the pragma
  (`// @ts-r strict`, `// @ts-r off`) comes first, as in real code.
- Every file is a module (it has an import or export) so names don't clash, except
  `s11-trust-boundaries/reject/wildcard-shorthand.ts` and
  `s12-no-suppression/reject/script-global-merge.ts`, which test script-only constructs.
- A reject file marks each expected diagnostic at the end of the line where it **starts**:

  ```ts
  const user = data as User; // @expect TSR0201
  ```

  Several codes on one line are separated by spaces: `// @expect TSR0201 TSR0301`. Only `TSR`
  codes are used, because every case is clean under tsc.
- Where a diagnostic is reported (which line) follows the "Reported at" column of each rule in the
  record. Cases keep the reported construct on one line so the line is unambiguous.
- `// @opt-in TSR1302` marks a diagnostic that only an opt-in switch reports: with
  `exports-declare-throws = true` (open question 3, off by default). The file stays in
  `accept/` (it is clean with the defaults); the harness's second run turns the switch on and
  expects the `@opt-in` markers there, in `s13-checked-exceptions/` only.

## Harness semantics (for the M6 agent)

1. Build **one program** from every `.ts` file under `cases/` (including `_shared/`), with
   `tsconfig.json` as the compiler options and `_shared/sound.toml` as the sound configuration.
   Each ambient module in `_shared/packages.d.ts` stands for a package of that name in
   `node_modules`, with the mode from `sound.toml` or detected (a shorthand declaration means
   `loose`). `@ts-r.dev/guard` resolves to `_shared/guard.d.ts`.
2. Before checking, **blank out** every `// @expect ...` suffix (replace it with spaces, so
   offsets don't move). The markers are not part of the code: `// @ts-r-unsafe // @expect TSR0001`
   is a hatch without a reason, and `// @ts-ignore // @expect TSR1201` is a plain `@ts-ignore`.
3. For each case file, collect every diagnostic (`TS` and `TSR`) whose start is in that file, as
   a multiset of `(line, code)`.
   - `accept/`: the multiset must be empty.
   - `reject/`: the multiset must equal the `@expect` markers exactly; no extra diagnostics,
     including follow-on errors.
   - `_shared/`: must be empty.
4. Run with 1 and N threads and compare (determinism, [0004](../../0004-types-and-checker.md)).
5. Check precedence too: a reject line lists one code per node, chosen by the record's order.
6. Run once more with `exports-declare-throws = true`: every `s13-checked-exceptions/` file
   must report its `@expect` markers plus its `@opt-in` markers, and nothing else.

When the record changes a rule, change its cases in the same PR. If an open question in the
record is decided differently from the proposal, the affected cases move between `accept/` and
`reject/` (for example `s11-trust-boundaries/accept/loose-value-flows.ts` for open question 1).

## The tsc check

`check-with-tsc.mjs` proves that every case is ordinary TypeScript and that the S-04 cases are
exactly what tsc reports with the flags turned on:

```sh
scripts/limit.sh --time-s 300 -- node docs/design/0007-sound-mode/cases/check-with-tsc.mjs
```

It uses the TypeScript version pinned in `upstream/UPSTREAM.json` through `npx` (or the binary in
`$TSC`), and fails unless:

- **Pass 1** (`tsconfig.json`: `strict: true`, `lib: ["es2023", "dom"]`, `module: "preserve"`):
  tsc reports nothing in any file.
- **Pass 2** (`tsconfig.sound-flags.json`: adds `noUncheckedIndexedAccess`,
  `exactOptionalPropertyTypes`, `isolatedDeclarations`, `declaration`): tsc reports only in
  `s04-index-and-optional/reject/`, on exactly the `@expect` lines. So the accept files of every
  rule are clean under the flags sound mode forces, and the S-04 rejects are real.
- Every reject file has at least one `@expect`, no accept file has one, each code belongs to its
  directory's rule (`TSR15xx` also in `s14`), and each rule has at least 4 accepts and 4 rejects.

Last run, 2026-10-16 (after the open questions were decided), tsc 7.0.2 (`Version 7.0.2`):
pass 1 had 0 diagnostics; pass 2 had 8 diagnostics, all on `s04` reject `@expect` lines; 113
accept files, 121 reject files, 123 expected diagnostics; `ok`.
