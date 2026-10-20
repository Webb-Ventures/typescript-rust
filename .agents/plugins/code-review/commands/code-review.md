---
allowed-tools: Bash(git diff:*), Bash(git log:*), Bash(git show:*), Bash(git blame:*), Bash(git merge-base:*), Bash(git rev-parse:*), Bash(git status:*), Bash(gh pr view:*), Bash(gh pr diff:*), Bash(gh pr list:*), Bash(gh api:*), Read, Grep, Glob, Task
argument-hint: "[PR number | branch | (empty for the working tree)]"
description: Review a ts-r change for verified bugs and AGENTS.md / design-record breaches
---

Review this ts-r change: $ARGUMENTS

Follow the `ts-r-review` skill from this plugin exactly (`skills/ts-r-review/SKILL.md`, with
its `references/`). Run the lenses as parallel Task subagents, verify and score every
candidate, keep only findings scoring 80 or more, and print the interactive report. Do not
post anything to GitHub: the code-review workflow posts reviews for pull requests into `main`.
