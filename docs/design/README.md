# Design records

A design record is a short, durable document that fixes one decision about how ts-r works: a data
layout, an algorithm, a public interface, a compatibility promise. Agents write most of the code,
so these records are the spec they build from. Each crate's `AGENTS.md` links the records that
govern it, and agents must follow them.

Write a record when a change:

- sets or changes an invariant other code relies on (AST layout, type IDs, cache keys, spans),
- adds or changes a public interface (CLI flags, config options, diagnostics format, LSP behaviour),
- picks between real alternatives with lasting cost (incremental strategy, threading model),
- deliberately differs from tsc 7, esbuild, Vite or ESLint behaviour.

Bug fixes and changes that keep every existing invariant do not need one.

## Files and numbering

One record per file: `docs/design/NNNN-short-title.md`, numbered in order (`0001-ast-layout.md`).
Never reuse a number. Superseded records stay in place with their status updated and a link to
the record that replaces them.

## Template

```markdown
# NNNN: Title

Status: Proposed | Accepted | Superseded by NNNN | Rejected
Owners: <crate(s)>

## Context

What problem this solves and what constrains it: measurements, upstream behaviour (with links to
TypeScript PRs or test baselines), the roadmap milestone it serves.

## Decision

What we will do, concretely enough that two agents would build the same thing. Data structures,
function signatures, file formats, flag names.

## Invariants agents must keep

A short, checkable list. Each item should be testable and should be copied into the owning crate's
AGENTS.md. For example: "AST nodes are `u32` indices into one arena per file; no `Box` children."

## Alternatives

What else was considered and why it lost (with numbers where possible).

## Status

Current state and history: proposed on, accepted on (by whom), superseded by.
```

## Review rule

People review every change to a design record, to `AGENTS.md` files and to skills, because a
wrong instruction is repeated by every agent that reads it. An agent may draft a record or propose
an edit, but it stays `Proposed` until a person approves the PR. Code that depends on a
`Proposed` record does not merge before the record is `Accepted`.

When an implementation needs to break a recorded invariant, stop and propose a change to the
record first; do not change the code and the record silently in one PR.
