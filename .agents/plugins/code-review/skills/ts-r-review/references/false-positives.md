# Known false positives

Findings this reviewer has posted that a person or a later run showed to be wrong, and the
pattern behind each. Before reporting a candidate, check it against every pattern here; a
candidate that matches one needs the evidence the pattern asks for, or it is dropped.

The list is measured: across the bot's first 386 findings (tsc-rust PRs 3-138 and
typescript-rust PRs 23-41), 6 were wrong and 2 partly right; of its 36 fix commits, 1 had a
part reverted and 4 broke tests later (patterns 3 and 4). All 6 wrong findings, and the
reverted fix, are pattern 1.

## 1. "Valid code gets TSxxxx" without asking the oracle

ts-r's job is to match tsc 7.0.2, typescript-eslint/ESLint, esbuild 0.28.2 and Vite (the
versions in `upstream/UPSTREAM.json`), not the ECMAScript or TypeScript specifications, and
not what the base commit did. tsc rejects code the spec allows, accepts code it forbids, and
keeps odd internal names; ESLint fixes code in ways that change its meaning. A finding that
ts-r's output is wrong is only right when the oracle's output differs.

In each case below, the code applied a rule on purpose (a check written to report the
diagnostic, a lint fix) and the finding said the oracle would not apply it to an edge input,
because the input is valid by the specification or the base accepted it.

Each of these was posted at confidence 90-100 and each matched the oracle:

| PR | Claim | What the oracle does |
|---|---|---|
| tsc-rust #30 | `var n: number; var n;` must not report TS2403 | tsc 7.0.2 reports TS2403 at the second `n` (also `var s = ""; var s;`) |
| tsc-rust #46 | `export { value as "export=" }` then `import { "export=" as value }` is valid | tsc 7.0.2 reports TS2305: a string export named `export=` is the export assignment |
| tsc-rust #47 | `prefer-string-starts-ends-with` must not fix `/^1/.test(123 as any)` | typescript-eslint fixes `/^bar/.test(a + b)` to `(a + b).startsWith("bar")` (`crates/ts_r_lint/tests/oracle/rules/ts-prefer-string-starts-ends-with.json`) |
| tsc-rust #72 | `this` aliases must not narrow across `function inner(this: ...)` | tsc 7.0.2 `--strict` accepts both the function and the arrow form |
| tsc-rust #93 | `number[] & { tag: E.A } & { tag: 1 }` must stay iterable | tsc 7.0.2 reports TS2488 (`--lib es2015 --target es2015`, strict or not) |
| typescript-rust #39 | `/(?<a>x)/u` must not report TS1514 ("the base validator accepted it") | tsc 7.0.2 reports TS1514 at the backslash; escapes are accepted later in the name. The bot's fix (4362d15) was reverted in 3cd16f3 |

The #39 fix passed all 266 tests of the crates it touched and left the checker conformance run
unchanged (12,350 exact before and after): none of tsc's baselines has that input. Only running
tsc on the finding's own input shows it.

**Evidence that makes such a finding reportable** (name it in the body):

- a tsc baseline or test fixture in the repo with the oracle's output for this input or one
  that differs from it only in names (`crates/*/tests/`, `tests/oracle/`, `*.baseline`,
  `bench/*-oracle/`);
- a crate `AGENTS.md` "observed rule", a `//!` note or a design record saying what the oracle
  does here;
- a commit message or test name in `git log` recording the oracle's output;
- for an interactive review, running the pinned tool (`bench/node_modules/.bin/tsc`, under
  `scripts/limit.sh`) on the input.

Reasoning from the specification, from "this is valid JavaScript", or from what the base did,
caps the score at 50, which drops it. Say "tsc 7.0.2 reports X here" only when you have seen it.

The cap is for parity claims only: the code applies its rule as written and the finding expects
something else from outside the code. A broken mechanism needs no oracle output: the code's own
rule (comments, names, tests, the rule doc it implements, another path doing the same job) says
one thing and the trace shows it doing another. Comment text scanned as code, a check that
misses a case its own rule covers, two paths that disagree, a cached result that depends on
evaluation order. Two paths that disagree are not a broken mechanism when one of them is a
special case the change adds for that input on purpose (#46 mapped the quoted name `export=`
to the export assignment, #72 added a match arm for `this`): which path is right is the
oracle's call. An early version of this rule capped both kinds; in a replay it dropped all 6
wrong findings but also 10 of 19 confirmed ones (SKILL.md's verify step 5 has the test).

## 2. Security findings past the threat model

tsc-rust #11 (dev-server path containment, critical): the `/@modules/` path escape was real,
but the claimed symlink part was not: symlinks inside the root or `public/` are put there by
the project's owner, and the rule is only that URL paths cannot leave the root. tsc-rust #105:
a CI approval step in `ci.yml` was called the security boundary, but a fork PR can edit that
file; the boundary is the repository setting "require approval for all outside contributors",
and the step only makes an unapproved PR's CI fail. Before a security finding: name the
attacker, what they control and what they gain, and check the plugin README's and the area's
threat model. Report the part that holds, not the whole class.

## 3. Fixes that pass on Linux and break on Windows

Two fix commits (tsc-rust b76733e3, ec2e19a7) added tests that spawned `scripts/limit.sh`
directly (os error 193 on Windows) and compared paths with `/` separators against
platform-native ones. The box runs Linux only, so its test run cannot show this. Tests run
helpers through `ts_r_testing::limited` and compare paths after normalising separators;
platform-only behaviour goes in a gated module (root `AGENTS.md`, "Platform-specific code").

## 4. Fixes that break after the stack merge

Two fix commits (tsc-rust b7a8e028, 0953a9a3) added tests that passed on their branch and
failed after the bot merged the fix up the stack, where a later branch changed the default
libraries (TS2318 under `noLib`). The fix run tests only its own branch; CI on the top PR is
what catches this, and a person fixes it there. A test that builds a program should state the
compiler options it depends on (`lib`, `target`, `noLib`) instead of relying on defaults.

## Adding an entry

When a finding of this reviewer is judged wrong or partly right (by `action-review`, by a
maintainer, or by the fix run reporting `wrong`), add it here in the same change that answers
it: the PR, the claim in a few words, what the oracle or the code actually does, and the
evidence. If it fits a pattern above, add a row; if it is a new pattern, add a section with the
rule that would have stopped it. Keep this file about patterns, not a log of every finding.
