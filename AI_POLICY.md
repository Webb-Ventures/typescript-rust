# AI policy

ts-r is built largely with AI coding agents, with people reviewing every change. AI-assisted
contributions are welcome, from a short list of models.

## Models we accept work from

Only frontier models:

| Provider | Models |
|---|---|
| Anthropic | Claude (any version), including Claude Code |
| OpenAI | GPT, the o-series, Codex and ChatGPT |
| Meta | Muse |

Work done with any other model is not accepted. That includes, but is not limited to, Google's
Gemini, Gemma and Jules; DeepSeek; Mistral's models (Mistral, Mixtral, Codestral, Devstral);
Alibaba's Qwen; Meta's Llama; xAI's Grok; Moonshot's Kimi; Zhipu's GLM; MiniMax; and agents'
own in-house models.

Writing code by hand is always fine.

## Why

The compiler has to match TypeScript's behaviour exactly. The project also has a strict
from-scratch rule: no code is copied or translated from other compilers ([AGENTS.md](AGENTS.md)).
Both depend on the model doing the work. The core team uses frontier models under review and
knows how they fail. Limiting contributions to the same models keeps review predictable and the
codebase consistent.

## Saying which model you used

Credit any model that wrote part of your change, in the pull request template's "AI assistance"
section or with a commit trailer, for example:

```
Co-Authored-By: Claude <noreply@anthropic.com>
```

You are responsible for everything you submit, whoever or whatever wrote it. You must have read
and understood it, and it must pass the checks in [CONTRIBUTING.md](CONTRIBUTING.md).

## Enforcement

A workflow (`.github/workflows/ai-policy.yml`) checks every pull request when it is opened,
edited or pushed to. It closes the pull request with a comment if a model outside the list above
is credited anywhere in it. It checks the title, the description, the branch name, the author,
commit messages, commit authors and committers, and `Co-authored-by` trailers.

- **A false match?** For example, a file or a word that only looks like a model name. Say so in
  a comment. A maintainer can reopen the pull request and add the `ai-policy:exempt` label,
  which skips the check.
- **Dependabot.** Its own pull requests are not checked: GitHub writes them, and they quote
  the updated packages' release notes, which can name any model. Commits anyone else pushes
  onto such a pull request are checked as usual.
- **Found later?** Work that turns out to come from a model outside the list is closed or
  reverted, whether or not the check caught it.
- Repeatedly submitting such work after it has been closed is treated as a
  [Code of Conduct](CODE_OF_CONDUCT.md) matter.

The maintainers may update this list as models change. The version on `main` applies.
