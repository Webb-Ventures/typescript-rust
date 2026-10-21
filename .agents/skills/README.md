# Skills

Task playbooks for recurring agent work. Each skill is a directory with a `SKILL.md` whose YAML
front matter has `name` (matching the directory) and `description`; `cargo xtask agents-check`
enforces this and checks that every `cargo xtask` command a skill mentions exists.

People review every change to a skill, because a wrong step is repeated by every agent.

## Available now (M0)

| Skill | Walks an agent through |
|---|---|
| [upstream-sync](upstream-sync/SKILL.md) | Review or prepare a TypeScript bump PR: sync, diff, codegen, oracle, tasks, gates |
| [bench-compare](bench-compare/SKILL.md) | Run the benchmark harness against main and reference tools, apply the 5% gate, report in the PR |
| [minimize-repro](minimize-repro/SKILL.md) | Shrink a failing input to the smallest repro and add it as a test |
| [fix-conformance-diff](fix-conformance-diff/SKILL.md) | Fix one group of differences from tsc 7 and prove nothing else changed |
| release | Open the release PR in the public repo (version, CHANGELOG, release benchmarks); publishing is a manual run of the release workflow on main. Maintainers only: not in the public mirror |
| [add-lint-rule](add-lint-rule/SKILL.md) | Implement an ESLint / typescript-eslint rule from its docs and tests (as data), prove it with the rule oracle |

## Arriving later

Written when the work they describe starts (or once the same task has been done three times):

| Skill | Walks an agent through | Expected |
|---|---|---|
| perf-investigation | Profile with samply, find the hot path, change it, show the benchmark result | after first perf work |
| add-sound-rule | Turn a sound-mode rule's spec into tests, implement it, add the error code, fix and docs page | M6 |
| error-code-page | Write the `/errors/TSRxxxx` page for a new diagnostic in the house style | M6 |
