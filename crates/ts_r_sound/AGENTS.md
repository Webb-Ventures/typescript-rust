# ts_r_sound

Sound mode (milestone M6): the 14 `TSR` rules, `@ts-r-unsafe` escape hatches, `@ts-r` level
pragmas, the `[sound]` configuration and the coverage report. Runs after the regular check and
reads types only through `ts_r_checker`'s public query API. Root rules:
[../../AGENTS.md](../../AGENTS.md). Design record (status: Accepted 2026-10-15):
[../../docs/design/0007-sound-mode.md](../../docs/design/0007-sound-mode.md); its cases:
[../../docs/design/0007-sound-mode/cases/](../../docs/design/0007-sound-mode/cases/README.md).

## API in one breath

```rust
let (cfg, errors) = config::discover(&fs, &tsconfig_path);        // ts-r.toml, tsrconfig.json or "ts-r" key
let g = config::guard_options(&fs, &tsconfig_path); // [sound.guard] for ts-r-run/build/dev: g.options, g.source, g.errors
let base = check_program(&program, CheckRun { threads: 4 });
let sound = check_sound(&SoundRun { program: &program, config: &cfg, base: &base.diagnostics,
                                    threads: 4, case_sensitive: true });
sound.diagnostics;            // TSR codes (100_000 + n), sorted; merge with base.diagnostics
report::text(&sound, &show);  // --sound-report;  report::json for --sound-report=json
// The editor: one file, forced checkers prepared (layered) by the caller.
check_sound_files(&run, &[file_index], Some(&|p: &mut ProgramFiles| layer_for(p)));
file_level(&program, file_index, &cfg, case_sensitive);   // pragma, else configuration
```

## How a pass runs

1. Every project file gets a level: its `// @ts-r <level>` pragma, else `exclude` (off), else
   `include` (the configured level), else off. `.d.ts`, JS and `node_modules` files are off.
2. If any file is `strict` or above, a second checker is built over the same parsed files
   with the forced flags on (`forced.rs`); each sound/strict file is checked by it. Errors
   it reports that the regular check did not are the forced flags' (TSR0011, TSR0401,
   TSR0402, TSR0010). The rules then query that checker (its caches are warm).
3. Rules (`rules/`) report `Raw` diagnostics through `Ctx::report`, which drops rules the
   file's level or `[sound.rules]` turns off.
4. Precedence keeps one diagnostic per node (same span), in the record's order. Hatches
   suppress TSR codes starting on the next line; unused valid hatches are TSR0002.

## Invariants

- Sound mode only adds diagnostics: it never changes or drops a regular check diagnostic.
- No rule needs more than the file and the declarations it imports (per-file passes).
- Never panics on user input; every walk is bounded by the AST or an explicit depth limit.
- Output is deterministic: 1 and 4 threads give the same diagnostics (harness checks it).
- The checker is read through its public API plus the additive queries in
  `ts_r_checker/src/query_sound.rs` and `ts_r_checker/src/flow/query_sound_flow.rs`.

## Decisions (record 0007, "Decisions", 2026-10-15)

The owner answered every open question on 2026-10-15. The numbers are the record's (its
question 3 covers both the unchecked list and the `@throws` requirement). Where a decision
keeps a switch, it is a key in `[sound]` (`config.rs`, `Policy`) defaulting to the decision.

| Q | Decision | Switch |
|---|---|---|
| 1 loose packages | `any` from `loose` packages stays `any` (S-11 accept case) | `loose-values = "unknown"` |
| 2 code prefix | print `TSR0201`; ts-r ships problem matchers that know `TSR` (`editors/vscode`, `npm/ts-r/problem-matcher.json`) | `code-prefix = "TS"` prints `TS100201` |
| 3 unchecked exceptions | only `panic()`/`expect()` are unchecked, plus a configurable list | `unchecked-errors = [...]` |
| 3 `@throws` on exports | only functions that already have `@throws` tags are held to them (TSR1301); TSR1302 is opt-in | `exports-declare-throws = true` |
| 4 overloads | the body is checked once per overload signature (`rules/overloads.rs`, TSR1205); TSR1203 (the ban) is retired | none |
| 5 mutable properties | object properties stay covariant; only built-in containers are invariant | `invariant-properties = true` |
| 6 S-09 severity | an error (D2) | `[sound.rules] "S-09" = "off"` |
| 7 S-06 scope | lib and `.d.ts` methods stay bivariant | `strict-lib-methods = true` |
| 8 TSR0803 | on: `this` escapes after field init are errors too | `constructor-escape = "before-init"` |
| 9 suppressions | reported (TSR1201) and still suppress | none |
| 10 per-rule switches | allowed; the report marks the guarantee partial | `[sound.rules]` |
| 11 `parse<T>` extra properties | kept; `parse` returns its input | none (the old `extra-properties` key is unknown) |
| 12 TSR1003 | everywhere, JSX handlers included | `async-void-callbacks = "allow-jsx-handlers"` |
| 13 classes in `parse<T>` | `instanceof` against the class, which the validator imports (`ts_r_guard`); abstract, generic, type-only and `declare class` classes are TSR1501 | none (the old `classes` key is unknown) |
| 14 branded/refined types | `[sound.guard] refinements = { Email = "./validators#isEmail" }`: the base type, then the predicate; unregistered brands are TSR1501 with a hint | `[sound.guard] refinements` |
| 15 building without a check | `ts-r-build` warns and continues; `--require-check` fails (not built yet: no `ts-r-build` integration) | none |
| 16 configuration home | `ts-r.toml`, `tsrconfig.json` or the tsconfig `"ts-r"` key; more than one is TSR0020 naming every file | none |
| 17 S-07 false side | required | `guard-false-side = false` |
| 18 forced-flag codes | re-coded as TSR0011/0401/0402/0010 | `forced-flag-codes = "TS"` |

Measured before the decisions, from private-app: Q3 (`exports-declare-throws` produced 70
TSR1302, every exported domain function that throws `DomainError`; now opt-in), Q6 (S-09:
rule-correct but 9 of 10 sampled hits are safe in context), Q5/S-05 (all 10 hits are arrays
that are never written), Q4 (all 37 TSR1203 were generated Pulumi constructors; the per-overload
check does not report them: constructors return no value).

### The per-overload check (`rules/overloads.rs`, TSR1205)

- An implementation and the overload signatures right before it form a group (functions,
  methods; constructors and generators are skipped). For each overload, its parameter types
  replace the implementation's (a missing parameter is `undefined` unless the implementation
  gives it a default or it is a rest parameter); a parameter that is destructured, reassigned or
  rest on one side only is not tracked.
- The body's statements are walked with those types, narrowing through `if` tests and loop
  conditions (`switch` and labels are opaque). A `return` where a tracked parameter narrowed
  to `never` is unreachable for that overload and skipped.
- `return e` reports when `e`'s type (awaited in async functions) is not assignable to the
  overload's return type and the type is exact for the overload: a bare parameter has its
  overload type; otherwise every parameter `e` reads must already have a type within the
  overload's (no names derived from parameters). `?:` is checked per branch. `return;` and a
  reachable end are `undefined`. This keeps false positives out at the price of missed cases
  (`xs.length` where `xs` is narrower in the overload).

Other decisions made here, not in the record:

- `@throws` tags in lib files are ignored (lib's `JSON.parse` says `@throws {SyntaxError}`), so
  calls to lib functions add nothing to a throws set.
- TSR codes are `100_000 + n`. tsc's own message catalogue uses 100000-100019 for a few
  non-error messages, so a numeric `TS1000nn` (OQ2 `"TS"`) can collide with those.
- History (`ts_r_history`, record 0005) does not save sound results. `ts-r-check` skips
  history whenever sound mode may apply (a flag, `--sound-config` or a discovered
  configuration), and falls back to a full check when a source mentions `@ts-r`, so a warm run
  never misses sound diagnostics. See [../ts_r_check/AGENTS.md](../ts_r_check/AGENTS.md).

## private-app (read-only; measured on a copy with its workspace packages linked into `node_modules`)

`ts-r-check --sound=sound` on all 25 projects (release, `scripts/limit.sh`, 0-1 s each). Third-
party packages are not installed, so the base check has ~940 TS2307 and their values are
`any`/error types (which the rules skip): counts are a lower bound. 783 TSR diagnostics after
deduplicating files shared between projects. Measured before the 2026-10-15 decisions: the
S-12 row was TSR1203 (retired) and the S-13 row is now opt-in (`exports-declare-throws`).

| Rule | Count | Sampled | Rule misapplied (FP) | Rule-correct, safe in context | Notes |
|---|---|---|---|---|---|
| S-01 | 269 (266 TSR0101, 3 TSR0102) | 10 + all 0102 | 0 | 0 | 265 TSR0101 in generated Pulumi SDK (`null as any`, `<any>state`). One TSR0102 FP (an `any` index key) fixed. |
| S-02 | 278 | 10 | 0 | 5 | 175 from `unknown` (`.json() as T`); generated `argsOrState as Args`. |
| S-03 | 28 | 10 | 0 | 10 | all in tests |
| S-04 | 3 | 3 | 0 | 0 | was 12 with 9 FPs before the EOPT narrowing workaround |
| S-05 | 10 | 10 | 0 | 10 | literal-union arrays passed as `string[]`, never written |
| S-07 | 15 | 10 | 3 | 5 | FPs: `(e): e is [string, string] => typeof e[1] === 'string'` (tuple element narrowing not used as proof). Safe: `includes`/`Boolean` guards |
| S-08 | 1 | 1 | 0 | 0 | `let connected!: () => void` |
| S-09 | 16 | 10 | 0 | 9 | 5 vitest `let pump` assigned in other `it` closures; 3 pure helpers (`iso`, `longDate`, `this.now`) between check and use; 1 real (`await` between) |
| S-10 | 23 | 10 | 0 | ~4 | `void p.then(...)` without `.catch`; 4 wrap `attempt(...)`, which probably never rejects |
| S-11 | 23 | 10 | 0 | 0 | generated `export declare const`, `declare module 'vitest'` in global-setup.ts |
| S-12 | 37 | 10 | 0 | 0 | generated Pulumi constructor overloads |
| S-13 | 70 | 10 | 0 | 0 | TSR1302: exported functions that throw without `@throws` (OQ3) |
| S-14 | 10 | 10 | 0 | 0 | `JSON.parse(...) as T` |

False-positive rate (rule misapplied) over the samples: 3 of 114 (S-07 only; S-09 0 of 10).
"Safe in context" is the rule's own conservatism: S-09 9 of 10, S-05 10 of 10.

## In the editor (`ts_r_lsp`, `tsr/project/sound.rs`)

Done (see [../ts_r_lsp/AGENTS.md](../ts_r_lsp/AGENTS.md), "Sound mode", for numbers):
`check_sound_files` after the regular check of each open file, with the forced checkers
layered on shared levels (`PrepareForced`); TSR diagnostics with `source: "ts-r sound"`, the
code label (numeric under `code-prefix = "TS"`) and a rule-page link; quick fixes for the
hatch (every rule), TSR0002, S-02/S-14 `parse<T>`, S-03 `expect`, S-10 `await` / `.catch`, S-13
`@throws`; `ts-r/soundLevel` for the status bar; `ts-r.toml` and `tsrconfig.json` changes reload the
configuration; `parse<T>` fixes use the project's refinements.
Remaining: S-09 copy-to-`const`, S-04/S-05/S-07/S-08 fixes, the VS Code bridge showing sound
diagnostics, a status-bar item, the fallback-backend notice.

## Dependencies

`ts_r_span`, `ts_r_ast`, `ts_r_binder`, `ts_r_diagnostics`, `ts_r_config`, `ts_r_types`,
`ts_r_checker`, `ts_r_guard` (TSR1501 asks the same declaration-based evaluator that generates
`parse<T>` validators), `rustc-hash`, and `toml` (already in the workspace for `xtask`; reading
`ts-r.toml` needs a real TOML parser). Dev: `ts_r_testing` (memory cap).

## Tests

- `cargo test -p ts_r_sound`: unit tests (codes, config, pragmas and hatches) and
  `tests/cases.rs`, the case harness. `-- --nocapture` prints the per-rule table and every
  failing case. The harness fails when fewer cases pass than `PINNED_PASSING` or when 1 and 4
  threads disagree, or a `_shared/` file has diagnostics. A second run turns on
  `exports-declare-throws` and checks the S-13 `// @opt-in` markers. Needs the lib files
  (skipped otherwise). 234 of 234 cases pass.
