---
name: ts-r-review
description: Review a ts-r change (the working-tree diff, a branch, or a pull request into main) for real bugs and breaches of the repo's rules (AGENTS.md, design records, the from-scratch rule, resource limits, determinism, hot-path performance), verify every candidate, and report only high-confidence findings. Works in Claude Code and Codex, locally and in the code-review GitHub workflow. Use when asked to review a PR, a branch or the current diff in this repo.
---

# ts-r code review

ts-r is a from-scratch TypeScript toolchain in Rust (lexer, parser, binder, checker, transform,
codegen, minifier, bundler, linter, dev server, language server) built by many agents working
in parallel tracks, one branch per track, merged into `main` through reviewed pull requests.
Your review is one of those gates. A finding costs a person's attention, so report only what
you have verified: a real bug the change introduces, or a concrete breach of a written rule.

This skill runs in two places with the same rules:

- **Interactive** (Claude Code `/ts-r-code-review:code-review`, or Codex "use ts-r-review"):
  print the report described in [Output](#output).
- **CI** (`.github/workflows/code-review.yml`, Codex on gpt-6.1-sol): the prompt in
  `ci/prompt.md` points here. Return JSON matching `ci/findings.schema.json`; the workflow
  posts each finding as a resolvable review thread.

## 1. Scope the change

1. Work out the base and head:
   - a PR number: `gh pr view <n> --json baseRefOid,headRefOid,title,body,files` (interactive only;
     in CI the prompt gives you the SHAs and there is no network);
   - a branch: base is `git merge-base origin/main <branch>`;
   - nothing given: the working tree against `git merge-base origin/main HEAD`.
2. `git diff --stat <base>...<head>` and `git log --oneline <base>..<head>`. Commit messages in
   this repo state what works and measured results; claims there are checkable.
3. Decide what to read closely. Skip as review targets (but use as evidence):
   - generated files (`*_generated.rs`, `crates/ts_r_checker/src/diagnostics_generated.rs`,
     anything `cargo xtask codegen` writes): flag only *hand edits* to them;
   - synced upstream data (`upstream/`, copied TypeScript tests and libs), lockfiles,
     `node_modules` under `crates/*/tests/fixtures/` (committed on purpose);
   - snapshot/baseline files: read them only to check that a diff in expected output is
     intended by the change.
4. Very large PRs (track merges): prioritise shared crates (`ts_r_span`, `ts_r_ast`,
   `ts_r_diagnostics`, `ts_r_lexer`, `ts_r_parser`, `ts_r_binder`, `ts_r_types`,
   `ts_r_checker`, `ts_r_cache`), then the track's own crates, then tests and docs.

## 2. Load the rules that apply

Read, in this order, and keep the exact wording you may need to quote:

1. Root `AGENTS.md` (`CLAUDE.md` just includes it). If it is missing (the site-only `main`),
   use the rules in [references/ts-r-rules.md](references/ts-r-rules.md), which mirror it.
2. The `AGENTS.md` of every crate or directory the change touches, and the design records in
   `docs/design/` they link. Crate AGENTS.md files hold invariants and "observed rules" that
   are as binding as the root file.
3. [references/ts-r-rules.md](references/ts-r-rules.md): the checklist, with how to spot each
   breach in a diff.
4. [references/areas.md](references/areas.md): what matters in each area and track (checker
   parity, lint oracle, bundler, dev server, LSP, sound mode, ecosystem, site, CI).
5. [references/false-positives.md](references/false-positives.md): the findings this reviewer
   got wrong before, and the evidence each pattern needs. Every candidate is checked against it
   in step 4.
6. Code comments and module docs in the files changed (`//!` docs often list observed tsc or
   ESLint behaviour the code must keep).

## 3. Review through these lenses

Each lens returns candidate issues with: file, line range in the new version, what is wrong,
the concrete input or sequence that triggers it, and the rule or code that shows it.

| Lens | Looks for |
|---|---|
| A. Correctness | Logic errors in the changed lines: wrong condition, off-by-one on byte offsets, missed case in a `match` over node/type kinds, broken error recovery, cache keys missing an input, state not reset between files, thread-safety of shared caches. Read only as much surrounding code as needed to confirm. |
| B. Repo rules | Root and crate AGENTS.md, design records: from-scratch rule, panics on user input, determinism, `unsafe`, deps in the wrong `Cargo.toml`, crate created without a target, AGENTS.md not updated when an invariant changed, code relying on a `Proposed` design record. |
| C. Robustness and limits | Loops that may not consume input, recursion without a depth limit, unbounded growth, tests over generated/real input without `ts_r_testing::cap_memory!()`, fuzz/corpus runs without bounds, more than 4 threads in harnesses, scripts running real projects without `scripts/limit.sh`. |
| D. Performance | Hot paths (lexer, parser, binder, checker relations, transform, codegen, minifier, bundler graph, lint visitors): per-node heap allocation, `clone` of large structures, `Rc`/`Box` graphs instead of `u32` ids, `String` where an interned atom exists, O(n²) over files or nodes. A perf claim in a commit message without numbers from the bench harness. |
| E. History and intent | `git log -L` / `git blame` on modified lines: does the change undo an earlier fix, drop a regression test, or contradict a commit that explained why the code is the way it is? |
| F. Cross-track impact | Changes to shared crates or public APIs that other tracks build on (AST node layout, `TypeId`/`SymId` meaning, diagnostic codes and texts, cache format/versioning, LSP protocol, CLI flags). Check callers in other crates still hold. |

**Interactive Claude Code:** run lenses A-F as parallel subagents (Task tool); a fast model is
fine for B and E, use a strong model for A, C, D and F. **Codex:** if subagents are available
(feature `multi_agent`), spawn one per lens; otherwise run them one after another yourself.
Either way give each lens the base/head SHAs, the changed-file list and the rule files from
step 2, and tell it to return candidates, not conclusions.

## 4. Verify and score every candidate

For each candidate, independently (a subagent per candidate when available):

1. Re-read the code at head. Confirm the lines are added or changed by this PR, not
   pre-existing. A pre-existing bug is out of scope unless the change makes it reachable.
2. Build the failure scenario: concrete input (a TS snippet, a tsconfig, a sequence of edits,
   two threads) → the wrong output, panic, hang or blow-up. If you cannot, it is not a bug.
3. For a rule finding, find the rule text. Quote it. If the rule does not say it, it is not a
   rule finding.
4. Check it is not silenced on purpose: a `// SAFETY:` comment with a crate AGENTS.md note,
   an `#[allow]` with a reason, an explicit "observed tsc behaviour" note, a design record.
5. **Behaviour the oracle defines.** ts-r's reference is what the pinned tool does on the
   input: tsc 7.0.2, typescript-eslint/ESLint, esbuild 0.28.2, Vite (`upstream/UPSTREAM.json`).
   Not the ECMAScript or TypeScript specification, not "this is valid code", and not what the
   base commit did. For a candidate about ts-r's output (a diagnostic reported or missing,
   emitted code, a lint report or fix, a bundle, dev-server behaviour), ask where the output
   it expects comes from:
   - **From the code itself: a broken mechanism.** The code's own rule (its comments, names,
     tests, the rule or design note it implements, another path doing the same job) or a plain
     requirement on any output (valid code, deterministic, no panic) says one thing, and the
     trace shows the code doing another. Comment text scanned as code, a check that misses a
     case its own rule covers, two paths that disagree, a cached result that depends on
     evaluation order, a feature only one build enables, an error path that skips the retry.
     The trace is the evidence; score it normally.
   - **From outside the code: a parity claim.** The code applies its rule as written (a check
     reports this diagnostic, a fix rewrites this input) and the candidate says the pinned tool
     would not, because the specification allows the input, it is valid JavaScript, the fix
     looks unsafe, or the base accepted it. A special case the change adds for this very input
     (a match arm, a name it maps, a flag it sets) is the code's rule too: a candidate saying
     that special case is wrong is a parity claim, even when another path disagrees with it.
     Only the oracle's output settles that. Find it for the failure input (a baseline or fixture
     in the repo, an AGENTS.md "observed rule", a test or commit recording it; interactively,
     run `bench/node_modules/.bin/tsc` under `scripts/limit.sh`) and state it in the body.
     Without it the score is at most 50. Six of this reviewer's wrong findings were parity
     claims posted at 90-100 ([references/false-positives.md](references/false-positives.md),
     pattern 1).
6. **Try to refute it.** Write down the strongest reason the code could be right as it is (the
   oracle does the same, an earlier commit chose it, a caller guarantees the input never
   occurs, the threat model excludes the attacker) and check that reason. Compare the
   candidate with every pattern in [references/false-positives.md](references/false-positives.md).
   A candidate that survives keeps its score; one that does not is dropped.
7. **Security**: name who controls the input, what they gain and why the existing boundary
   (README threat model, area notes) does not already stop it. Report only the part that
   holds.
8. Score with this rubric (verbatim):
   - **0**: Not confident at all. This is a false positive that doesn't stand up to light
     scrutiny, or is a pre-existing issue.
   - **25**: Somewhat confident. This might be a real issue, but may also be a false positive.
     The agent wasn't able to verify that it's a real issue. If the issue is stylistic, it is
     one that was not explicitly called out in the relevant AGENTS.md.
   - **50**: Moderately confident. The agent was able to verify this is a real issue, but it
     might be a nitpick or not happen very often in practice. Relative to the rest of the PR,
     it's not very important.
   - **75**: Highly confident. The agent double checked the issue, and verified that it is very
     likely it is a real issue that will be hit in practice. The existing approach in the PR is
     insufficient. The issue is very important and will directly impact the code's
     functionality, or it is an issue that is directly mentioned in the relevant AGENTS.md.
   - **100**: Absolutely certain. The agent double checked the issue, and confirmed that it is
     definitely a real issue, that will happen frequently in practice. The evidence directly
     confirms this.
9. Drop everything below **80**. Merge duplicates (same root cause → one finding at the most
   useful location).

### Not findings

- Anything CI already catches: `cargo fmt --check`, `cargo clippy -D warnings`,
  `cargo test --workspace`, `cargo xtask agents-check` (missing crate AGENTS.md, broken links,
  skill front matter), `cargo xtask codegen --check`, the Pages build. Do not build or run
  tests to look for these.
- Differences from tsc 7, ESLint or esbuild that the change documents as intended (design
  record, AGENTS.md "observed rules", test baseline updated on purpose).
- Pedantic style, naming, comment wording, missing docs or tests in general (unless a rule
  requires them, for example pathological-input regression tests for lexer/parser loops).
- Speculative issues ("could be slow", "might race") without a concrete scenario. A
  performance finding names the input size where it matters and a fix that keeps the output
  identical.
- Issues in lines the PR did not touch.

## Output

Every finding has: a short title; severity (`critical` data loss, crash/hang on user input,
security, wrong output shipped by default · `high` wrong diagnostics/output in common code,
broken rule with real cost · `medium` real but narrower · `low` minor but real); category; the
file and line range **in the head version**; a body that states the problem, the failure
scenario and the fix; the rule quoted when it is a rule finding; for behaviour the oracle
defines, what the oracle does on the failure input and where that is recorded; and confidence.

- **Interactive:** print `### Code review` then a numbered list, each item
  `**severity** title`, `path:start-end`, the body, and the quoted rule. If nothing survives:
  "No issues found. Checked for bugs and AGENTS.md compliance." Do not post to GitHub; the
  workflow does that for PRs into `main`.
- **CI:** follow `ci/prompt.md` and return only the JSON object.
