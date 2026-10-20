## What this changes

<!-- What works now that didn't, or what changed, and why. Link the issue: "Fixes #123". -->

## How it was tested

<!-- The tests you added or ran, and any oracle or benchmark numbers that moved
     (for example: conformance 11,111 -> 11,115 exact; check-rxjs unchanged). -->

- [ ] `cargo fmt --all`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cargo xtask agents-check`
- [ ] Updated the `AGENTS.md` of any crate whose invariants changed

## AI assistance

<!-- Required. Pick one, and name every model that wrote part of this change.
     Accepted models are listed in AI_POLICY.md (Anthropic's Claude, OpenAI's GPT / o-series /
     Codex, Meta's Muse). A pull request that credits any other model is closed automatically. -->

- [ ] None: written by hand
- [ ] AI-assisted, with: <!-- e.g. Claude Opus 5.5, GPT-6.1 Codex -->

## Checklist

- [ ] No code copied or translated from tsc, esbuild, ESLint or any other tool (the from-scratch rule in `AGENTS.md`)
- [ ] I have read [CONTRIBUTING.md](../blob/main/CONTRIBUTING.md) and agree to license this contribution under MIT OR Apache-2.0
