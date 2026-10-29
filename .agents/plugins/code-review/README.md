# ts-r code review

Pull request review for ts-r that runs the same way on Claude and OpenAI models: locally in
Claude Code or Codex, and on every pull request (stacked ones too) through
[`.github/workflows/code-review.yml`](../../../.github/workflows/code-review.yml) (Codex,
`gpt-6.1-sol`, on our ChatGPT subscription).

It reviews a change through six lenses (correctness, repo rules, robustness and resource
limits, hot-path performance, git history, cross-track impact), verifies every candidate
with a concrete failure scenario, scores it 0-100 and keeps only findings scoring 80 or more.
The rules come from the repo itself: root and crate `AGENTS.md` files, `docs/design/`
records, and the checklists in `skills/ts-r-review/references/`.

## Layout

| Path | What it is |
|---|---|
| `skills/ts-r-review/SKILL.md` | The review procedure. One source of truth for every model and harness |
| `skills/ts-r-review/references/ts-r-rules.md` | Root AGENTS.md rules, each with how a breach shows up in a diff |
| `skills/ts-r-review/references/areas.md` | Where real bugs hide per area: front end, transform, dev server, bundler, lint, checker, sound mode, LSP, ecosystem, site, CI |
| `skills/ts-r-review/references/false-positives.md` | The findings this reviewer got wrong before, the pattern behind each and the evidence it now needs; the review and fix runs read it, `action-review` adds to it |
| `commands/code-review.md` | Claude Code entry point: `/ts-r-code-review:code-review [PR \| branch]` |
| `.claude-plugin/plugin.json` | Claude Code manifest (marketplace: `../.claude-plugin/marketplace.json`) |
| `.codex-plugin/plugin.json` | Codex manifest (marketplace: `../marketplace.json`) |
| `ci/prompt.md`, `ci/findings.schema.json` | CI prompt and the JSON shape findings come back in |
| `ci/github.cjs` | Posts findings as review threads, keeps the summary comment, resolves in bulk |
| `ci/bench.cjs` | Keeps the bench comment: progress line, PR-vs-base tables, gate verdict |
| `ci/catchup.cjs`, `ci/stack.cjs` | After a fix: merge a branch that moved on into it before pushing, then merge it up the stack |
| `ci/eval/` | Past findings with a person's verdict (`cases.json`) and the scorer for replayed reviews (`score.cjs`); see [Measuring accuracy](#measuring-accuracy) |
| `box/` | The review box: dispatcher, job containers, egress proxy, compose file, install script, GitHub App setup |

## Use it locally

**Claude Code.** Add the repo marketplace once, then install:

```text
/plugin marketplace add ./.agents/plugins
/plugin install ts-r-code-review@ts-r
/ts-r-code-review:code-review            # working tree vs main
/ts-r-code-review:code-review 42         # PR #42
/ts-r-code-review:code-review m5/checker-parity
```

**Codex.** The repo marketplace is `.agents/plugins/marketplace.json`. Add it once and install:

```bash
codex plugin marketplace add ./.agents/plugins
codex plugin add ts-r-code-review@ts-r
```

Then ask: "use ts-r-review to review my branch against main" (or `$ts-r-review`). Any model
works; `gpt-6.1-sol` at high effort matches CI.

Locally the review prints its findings; it does not post to GitHub.

## In pull requests

Reviews run on the **review box** (`<box>`: its tailnet hostname is the `CODE_REVIEW_BOX` repo
variable), not on Actions runners. One box serves this repository and its public mirror,
`Webb-Ventures/typescript-rust` (`REPOS` in its `config.env`); everything below applies to each.
[`.github/workflows/code-review.yml`](../../../.github/workflows/code-review.yml) only joins
the tailnet, hands the job to the box and exits, so it takes seconds and never waits on a
review. The box does the work in fresh Docker containers and posts as the **ts-r review**
GitHub App.

**Automatically, once per PR**: when a non-draft pull request from a branch of this repo (not a
`sync/` branch: those are the mirror's sync PRs, reviewed in the private repo) is
opened or marked ready, whatever its base (each PR of a stack is reviewed against the PR below
it), and both the author and whoever triggered the run are
trusted (see below). Pushes, reopening and marking a PR ready again do not start another.
**Every other review is on request**, on any PR (any author, any base, forks too): a trusted
user comments `@autoreview` on it, or runs the workflow from the Actions tab with its number.
A Codex fix commit does not start a review either; comment `@autoreview` to review it.

The PR's **summary comment** shows progress: queued (and behind how many), running since when,
then the results. A newer request replaces a review of the same PR still waiting in the queue. Each review:

1. lists this reviewer's earlier findings on the PR, so the model can match the same issue
   across pushes instead of repeating it;
2. runs Codex (`gpt-6.1-sol`, high effort, on our ChatGPT subscription) on a read-only checkout
   of the PR head with the CI prompt, which follows the skill and returns JSON;
3. posts each new finding as an **inline review thread** on the diff (a committable suggestion
   when the fix is exact). Findings on a file but outside its hunks become file-level threads;
   findings in files the PR does not touch go in the summary;
4. resolves its own open threads that the new review no longer reports, with a reply saying so;
5. updates the summary comment: the verdict, a table of findings linked to their threads with
   open/resolved state, how long the review took, and a **Fix and resolve all findings**
   checkbox;
6. submits a GitHub review with a verdict (below): **Request changes** while any finding is
   open, **Approve** when none is. New inline findings ride in that review.

The verdict follows the findings:

- **Any open finding requests changes**, whatever its severity. Severity is the model's
  estimate; a low finding the author disagrees with costs one click to resolve, while waving
  one through on a guess costs a bug. Findings outside the diff count too (they have no thread;
  a later review that no longer reports them clears them).
- **Once every finding is resolved, the bot approves**, at the PR's head: after a fix run, after
  `/resolve-all` or the checkbox, and after a refresh that sees threads resolved by hand (for
  example by the [action-review skill](../../skills/action-review/SKILL.md)). It approves once:
  while its latest verdict is an approval it does not approve again, not even for a new head.
- **Reopening goes back to Request changes**: `/unresolve-all`, unticking the box, or a later
  review that reports new or worse findings.
- **Threads resolved by someone without write access** (a PR's author can resolve threads on
  their own PR) do not count towards the approval; the summary says so.
- **Pushes are not re-reviewed.** GitHub keeps an approval across pushes unless branch
  protection's "Dismiss stale pull request approvals when new commits are pushed" is on. With it
  on, a push dismisses the bot's approval and the bot leaves a dismissed verdict alone until the
  next review (`@autoreview`). With it off, commits pushed after the approval are merged on it,
  so comment `@autoreview` when they matter.
- If branch protection has "Require approval of the most recent reviewable push", an approval
  right after a Codex fix does not count: the bot pushed that commit itself.

Resolving:

- **One finding:** "Resolve conversation" on its thread.
- **Fix all findings:** tick **Fix and resolve all findings** in the summary comment, or
  comment `/fix-all`. Codex fixes the open findings on the PR branch and pushes one commit as
  `ts-r-review[bot]`. Each fixed finding gets a reply with the commit, what changed and the
  diff, what it ran (the oracle on the finding's input, builds and tests), and is resolved. A
  finding about output tsc, ESLint or esbuild defines is checked against the pinned tool first
  (`bench/node_modules`, through the runner); one Codex judges wrong gets a reply saying so with
  the evidence and stays open for a person to agree, and one that needs a person's decision or a
  workflow change gets a reply saying why and stays open. The push does not start a review;
  comment `@autoreview` to have the fixes checked. Only for open PRs from this repository (the App cannot push to forks;
  there, ticking the box just resolves). The dependencies the lockfiles pin are installed
  first; Codex then builds, tests, lints and formats what it touched before reporting a fix,
  through a separate runner container (below).
  It cannot add a new dependency (it reports that finding as not fixed and names it).
- **The branch moved on while Codex worked** (someone pushed, or merged `main` in): the bot
  fetches the new head, merges it into its fix commit (`Merge <branch> head <sha> into the
  review fixes`, a merge commit with the fix as first parent, as `ts-r-review[bot]` like the
  fix itself) and pushes that with a plain push. It never rebases or force-pushes. If the
  branch moves again before the push lands, it fetches and merges again, up to 3 times
  (`MERGE_ATTEMPTS` in `ci/catchup.cjs`). The summary says the fix was pushed and how many new
  commits were merged (a merge of `main` counts as one). Before resolving, each fixed thread
  is checked against the merged commits: if they changed the finding's own lines, or the
  lines the fix changed in its files (an identical change on both sides counts, even without
  a textual conflict), the thread gets the usual reply, says the branch changed those lines,
  and stays open for a person to check. Nothing is pushed, and nothing is resolved, when:
  - merging conflicts (the summary lists the files);
  - the branch was rewritten (its new head no longer contains the head Codex fixed);
  - it kept moving through every retry;
  - the merge would change `.github/workflows`.

  Tick the box again to fix the new head. The merge is done with `git merge-tree` in a bare
  repository, with hooks off and nothing checked out, so nothing in the incoming commits runs.
- **Stacks:** after a fix is pushed, the bot merges it up the stack, as a person would by
  hand. Every open PR from this repository whose base is the fixed branch gets a merge commit
  of it (`Merge <below> into <branch>`, as `ts-r-review[bot]`), pushed as a fast-forward of
  its branch. Then the PRs above that one get the same, up to 30 PRs (`ci/stack.cjs`). A
  branch that already has the fix is skipped, and the merges carry on above it. The bot
  stops at a PR and comments there, naming what to merge by hand, when:
  - the merge conflicts (the comment lists the files);
  - the branch moved on since it was fetched;
  - the merge would change `.github/workflows`, which the App may not push.

  The PRs above a stopped PR are not merged into either. The fixed PR gets one comment
  listing what was merged where. These pushes, like the fix's, do not start a review, and CI
  runs only on the top PR.
- **Resolve without fixing:** comment `/resolve-all`; `/unresolve-all` (or unticking the box)
  reopens them. Each thread gets a reply saying who resolved or reopened it.
- **Approve anyway:** comment `/approve` and the bot approves the PR at its head, whatever its
  findings (threads are left as they are; the review body counts any still open). Until the
  next completed review or `/unresolve-all` (or unticking the box), refreshes do not request
  changes again over it, and the summary says who approved it and at which commit. A later push is not approved by
  itself: comment `/approve` again.
- All of these need a trusted user with write access.
- A finding you resolve stays resolved on later pushes. If a later review reports it again
  because the code got worse, the thread is reopened with the new explanation.
- GitHub does not tell Actions when a single thread is resolved, so the summary table and the
  verdict catch up on the next review, thread reply or bulk action, or a trusted user's PR
  comment carrying `<!-- ts-r-review:refresh -->` (the action-review skill's summary comment
  does). The box waits about 20 seconds (`REFRESH_DELAY_MS`) after each such event, and up to
  two minutes for a burst, then refreshes once. After resolving threads by hand in the UI,
  reply on any thread to get the approval.

## The review box

```text
GitHub event -> Actions job (joins tailnet, POSTs {run_id, pr, comment_id, repo}, exits)
             -> https://<box>.ts.net/code-review/jobs (tailscale serve, tailnet only)
             -> dispatcher (re-checks the run on GitHub, queues, updates the summary)
             -> one container per step: prepare -> codex -> publish          (review)
                                           prepare -> deps -> codex (+ runner) -> commit -> push  (fix)
                                           resolve
                                           bench-prepare -> bench-checkout -> bench-deps -> bench-build (base, PR) -> bench-measure  (bench, one per slot)
```

| Step | GitHub token | Network | Does |
|---|---|---|---|
| `prepare` | read | internet | refreshes the repo cache, checks out the PR head, writes the prompt |
| `review` | none | OpenAI only, through the egress proxy | runs Codex on the read-only checkout, writes `findings.json` |
| `publish` | write | internet | posts threads, the summary and the verdict |
| `deps` (fix) | none, and no Codex login | package registries only | `cargo fetch --locked` and `npm ci --ignore-scripts` into the job's own directories |
| `codex` (fix) | none | OpenAI only | edits a writable checkout (its git directory stays read-only); no toolchain, npm or installed dependencies, so it builds and tests through `box-run` |
| `runner` (fix) | none, and no Codex login | none | runs the commands Codex sends with `box-run` (builds, tests, lints, formatters, package scripts) on the shared checkout, offline |
| `commit` | none | none | commits the edits as `ts-r-review[bot]` (never workflow files) |
| `push` | write | internet | pushes the commit as a fast-forward of the fixed head (or, if the branch moved on, a merge of its new head into the commit), replies on and resolves threads, approves when none is left, merges the pushed head up the stack |
| `resolve` | write | internet | resolves or reopens every finding, or refreshes the summary; brings the verdict in line |
| `bench-prepare` | read | internet | refreshes the repo cache, picks the base commit (the base branch's tip) |
| `bench-checkout` | none | none | checks the PR and its base out in the slot's directory (git directories outside the checkouts), lists the binaries `bench/scenarios.toml` runs |
| `bench-deps` | none, and no Codex login | package registries only | crates, the pinned reference tools, `upstream sync --minimal`, the PR repo set and its packages (`--ignore-scripts`) |
| `bench-build` | none, and no Codex login | none | builds one side in release (base, then PR; each sees only its own checkout writable), only the binaries the scenarios run |
| `bench-measure` | none, and no Codex login | none | runs `cargo xtask bench` base then PR, re-measures flagged scenarios PR first, puts back the kept reference-tool numbers |

Everything lives in `/srv/ts-r-review` on the box:

| Path | What it is |
|---|---|
| `config.env` | `REPOS` (a JSON array of `owner/name`; requests without a `repo` are for the first; `REPO` alone still works), `TRUSTED_USERS`, `MODEL`, `EFFORT`, `GITHUB_APP_ID` (optionally `GITHUB_APP_INSTALLATION_ID`, the installation on the first repo's account); for benches, `LANES`, `BENCH_KEEP_BUILDS`, `BENCH_SLOTS`, `BENCH_SLOT_CPUS`, `BENCH_CPUS`, `REVIEW_CPUS`, `BENCH_MEASURE`, `BENCH_MEMORY`, `BENCH_BUILD_JOB_MEMORY` and `BENCH_REFERENCE_DAYS` (see [Benches](#benches)); for deploys, `DEPLOY_REPO` and `DEPLOY_WAIT_MINUTES` (see [Deploys](#deploys)) |
| `secrets/github-app.pem` | The App's private key |
| `secrets/dispatch-token` | Shared with the workflow as `CODE_REVIEW_DISPATCH_TOKEN` |
| `codex-home/` | The box's Codex login (`auth.json`); Codex refreshes it in place |
| `cache/` | A bare clone of each repo (`<owner>__<name>.git`), fetched before every job |
| `targets/` | A fix run's build directory while it runs (deleted when it ends) |
| `jobs/`, `logs/` | Per-job working directories (deleted after each job) and container logs (last 200) |
| `bench/` | Per repo (`<owner>__<name>/`): one directory per slot (`slot-<n>/`: its PR and base checkouts, sources only between runs unless `BENCH_KEEP_BUILDS=1`), the crates and package stores the slots share (`cargo/`, `pm/`, `npm/`), the kept reference-tool numbers (`reference/`) and `passed.json` |
| `deploy/` | The plugin directory of the deployed commit (`<sha>/`, and the one waiting to deploy), and `current`, the deployed commit |
| `queue.json` | Both lanes' queues; a restart (a redeploy) picks them up, restarting the running jobs (a bench stopped as moot is not restarted) |

### Setup

1. **Deploy.** Copy this plugin directory to the box and run the install script as `ubuntu`
   (uid 1000, in the docker group) or as root; `/srv/ts-r-review` is owned by uid 1000 either way. Re-run it after syncing a new version:

   ```bash
   rsync -a --delete .agents/plugins/code-review/ <box>:ts-r-review-src/
   ssh <box> 'cd ts-r-review-src && bash box/install.sh'
   ```

   It creates `/srv/ts-r-review`, the dispatch token and `config.env`, builds the image, starts
   the dispatcher and egress proxy, and serves the dispatcher on the tailnet at
   `/code-review`.

2. **Dispatch token.** Store it as a repo secret without printing it:

   ```bash
   ssh <box> cat /srv/ts-r-review/secrets/dispatch-token \
     | gh secret set CODE_REVIEW_DISPATCH_TOKEN --repo <owner>/<name>
   ```

   Do this for each repo in `REPOS`.

3. **GitHub App.** `node .agents/plugins/code-review/box/create-github-app.mjs <private-dir>`
   opens a page that creates the App from a manifest (one click), then links to installing it:
   install it on the repos in `REPOS` only. Copy `<private-dir>/github-app.pem` to
   `/srv/ts-r-review/secrets/github-app.pem` (mode 600), put the id from `github-app.json` in
   `GITHUB_APP_ID` in `config.env`, delete the local copy, and re-run `box/install.sh` (a plain
   restart does not reload `config.env`).

4. **Codex login.** Sign in a ChatGPT login for the box with a device code (don't name the
   container `tsr-*`: the dispatcher removes those when it starts):

   ```bash
   docker run --rm -it -v /srv/ts-r-review/codex-home:/codex -e CODEX_HOME=/codex -e HOME=/tmp \
     ts-r-review:latest codex login --device-auth -c 'cli_auth_credentials_store="file"'
   ```

5. **Tailscale.** Create an OAuth client that can create ephemeral nodes tagged `tag:ci` (or
   set the `CODE_REVIEW_TS_TAG` repo variable), allow that tag to reach the box on port 443 in
   the tailnet policy, and store the client as the `TS_OAUTH_CLIENT_ID` and `TS_OAUTH_SECRET`
   repo secrets, in each repo. The `CODE_REVIEW_BOX` repo variable is the box's tailnet hostname
   (`<box>` above).

Logs: `docker logs ts-r-review-dispatcher-1` and `/srv/ts-r-review/logs/`.

### Deploys

After the first deploy by hand, the box deploys itself: a merge into `main` that changes this
plugin runs [deploy-box.yml](../../../.github/workflows/deploy-box.yml), which POSTs
`{run_id, repo}` to `/code-review/deploy` with the dispatch token, like the code review. The
dispatcher checks the run on GitHub: that workflow, in `DEPLOY_REPO` (one of `REPOS`; unset, the
box refuses deploys), a push to its default branch or a manual run on it by a trusted user, at the
branch's current head. Then it:

1. fetches this plugin directory of that commit (no history, no other files) into
   `deploy/<sha>/`;
2. starts no new job and waits for the running ones (at most `DEPLOY_WAIT_MINUTES`, default 90;
   after that it deploys anyway, and the stopped jobs start again from the beginning);
3. runs [box/deploy.sh](box/deploy.sh) in a container of its own (`ts-r-deploy`), from the image
   running now: it tags that image `ts-r-review:previous`, builds the new one, restarts the box
   with it, and checks that the new dispatcher answers `/healthz`. If it does not within three
   minutes, the previous image comes back. If the build fails nothing is restarted and the jobs
   carry on.

The restarted dispatcher picks both queues up from `queue.json`. A newer merge replaces a deploy
still waiting; one that arrives while a deploy runs gets a 409 (run the workflow again after it).
`/healthz` ends with the deployed commit (`deployed by hand` before the first deploy) and any
deploy waiting or running; each deploy's log is `logs/deploy-<sha>.log`. To roll back by hand:
`docker tag ts-r-review:previous ts-r-review:latest` and `docker compose -f <plugin>/box/compose.yaml up -d --no-build`.

Setting it up: deploy once by hand with this version (`box/install.sh`), set
`DEPLOY_REPO=<owner>/<name>` in `config.env` and re-run `box/install.sh`, then set the
`CODE_REVIEW_DEPLOY` repo variable to `true` in that repository (the workflow skips everywhere
else). It uses the code review's secrets (`TS_OAUTH_CLIENT_ID`, `TS_OAUTH_SECRET`,
`CODE_REVIEW_DISPATCH_TOKEN`, `CODE_REVIEW_BOX`). A change to `install.sh` itself (the
directories, `tailscale serve`) still needs a run by hand.

### Why it is set up this way

- The Action is only a trigger, so reviews cost seconds of Actions time, and the summary
  comment, not a waiting job, shows whether a review is running.
- The dispatcher trusts nothing in the request except the run id: it loads that workflow run
  from GitHub and acts on its event, triggering user and PR, and only for runs of this
  workflow that are under 15 minutes old and not yet finished. Run ids are used once.
- The entry point is tailnet only (`tailscale serve`, not Funnel) and also needs the dispatch
  token.
- PR code is only read, never run. The review instructions come from `main`'s copy of this
  plugin (the image's copy until the plugin is merged), so a PR that edits the plugin cannot
  change how its own review runs. Rule files the PR changes are judged at their base versions.
- Codex gets no GitHub token and can reach only `chatgpt.com` (the Codex backend) and
  `auth.openai.com` (token refresh) through an egress proxy, reviewing or fixing; the proxy
  logs every host it allows or denies (`docker logs ts-r-review-egress-1`); its container has a read-only
  root, no capabilities and a non-root user. A fix's dependencies are installed before Codex
  runs, by a container that holds neither the Codex login nor a token, through a second proxy
  that allows only the package registries (crates.io, `registry.npmjs.org`, `npm.jsr.io`, Yarn's,
  GitHub for git dependencies), and runs no install or build scripts. A tunnel to those hosts would also
  carry authenticated uploads, so it is never given to a container with a secret in it.
  Dependencies are per job: nothing one PR installs is reused by another (allowlists in
  `box/egress-proxy.mjs`). Codex's own Linux sandbox cannot create namespaces inside Docker, so the
  container is the sandbox and Codex runs with `danger-full-access` inside it.
- No code from a PR runs next to the Codex login. Build scripts, proc macros, tests and package
  scripts could read the login and send it to OpenAI's API under someone else's key, which no
  output scan would see. So the Codex containers have the Rust toolchain, npm and (in a fix)
  the installed `node_modules` hidden, and a fix's builds and tests run in the fix's runner
  container: same checkout, the toolchain and dependencies, no network and no login. Codex
  reaches it with `box-run <command>` (`box/run-client.mjs`), through a directory both
  containers mount; the runner runs one command at a time and is stopped when Codex finishes.
  Node itself stays in the Codex container (Codex runs on it), so the prompt also tells Codex
  not to run code from the checkout with it.
- The dispatcher reads the login before Codex runs (Codex's container can rewrite it) and
  checks Codex's output, and a fix's patch and the raw bytes of every file it changes, contain
  none of its tokens before anything is posted or pushed. If the login cannot be read, nothing
  runs.
- For fixes, git runs with the git directory named explicitly and hooks off, so nothing Codex
  writes into the checkout can change what the commit and push steps do. A push is never
  forced: it fast-forwards the head Codex fixed or, if the branch moved on, the branch's new
  head, merged into the fix with `git merge-tree` in a bare repository (no checkout, hooks off,
  so nothing from the new commits runs). The cache holds full history, so no deeper fetch is
  needed.
- GitHub App installation tokens are minted per step with only that step's permissions and
  expire within the hour. The App needs Contents write only because GitHub requires it to
  resolve review threads; `GITHUB_TOKEN` cannot. Approving and requesting changes need only
  Pull requests write, which it already has; an App may approve because it is never the PR's
  author. Whether its approval satisfies "Require approvals" in branch protection depends on
  GitHub counting an App with write access as a qualifying reviewer; check it on the first PR
  it approves.
- Trusted users are the logins in the `CODE_REVIEW_USERS` repository variable (default
  `["riley-wv"]`), which gates the Action, and `TRUSTED_USERS` in the box's `config.env`, which
  the dispatcher enforces (one list for every repo the box serves). Keep them the same.
  Automatic reviews need both the PR author and the triggering user in it; `@autoreview`,
  manual runs, resolve-all and `/approve` need the person starting them in it. The rules are
  pure functions in [`ci/trust.cjs`](ci/trust.cjs), tested in `ci/trust.test.cjs`.
- A request names its repo, and the dispatcher loads the workflow run from that repo, checks
  the run belongs to it and that the box serves it, then mints every token for that repo alone
  (`repositories: [name]`). Comments, statuses, reviews, the bench's "already passed" record and
  stack checks all use the job's repo. Each repo has its own git cache and bench directory, and
  a job's containers mount only its own repo's: a PR in one repo never sees another's objects.
- Fork PRs are fetched as `refs/pull/<n>/head`. Nothing is ever pushed to a fork: `/fix-all` and
  the summary's fix box are refused for fork PRs, and the push step refuses them again.
- One review runs at a time, so the single ChatGPT login is never refreshed by two runs at once.

### Benches

PR benches run on GitHub-hosted runners (`.github/workflows/bench-actions.yml`; bench/README.md,
"Benches on GitHub-hosted runners"), which are free in the public repository. Since 2026-10-29
no workflow hands benches to a box, and the review box runs `LANES=["review"]`. The bench lane
below stays in the dispatcher for a dedicated bench machine, if one is set up again: a request
that names `bench.yml` as its workflow is benched as described here, and a box without the lane
refuses it (421).


The box runs what the old Actions job ran, on a quieter machine:

- **Its own box.** Benches can run on a second machine with the same install (`box/install.sh`,
  the same App key and dispatch token; no Codex login): set `LANES=["bench"]` and
  `BENCH_KEEP_BUILDS=1` in its `config.env`, `LANES=["review"]` in the review box's, and the
  `BENCH_BOX` variable (a secret in a public repo) to its tailnet name. `bench.yml` uses
  `BENCH_BOX`, else `CODE_REVIEW_BOX`; a box refuses a lane it does not run (421). Both boxes need
  the same tailnet tag rule for `tag:ci`.

- **Own lane, own cores.** Benches queue separately from Codex jobs. Each bench runs in a
  **slot**, a cpuset of its own: the PR and its base are built and measured on the same slot's
  cores, so the comparison never crosses slots. Codex reviews and fixes run on `REVIEW_CPUS`
  (`0-1`), so a bench never waits behind reviews and nothing else is scheduled on its cores. A
  fix's runner is not pinned (its builds need the cores); a fix that overlaps a bench can add
  noise, which the confirming re-run catches.
- **Several at once.** Without `BENCH_SLOTS` there is one slot on `BENCH_CPUS` (default `2-5`):
  one bench at a time, as before. `BENCH_SLOTS=auto` sizes the slots from the cores Docker
  reports at startup, `BENCH_SLOT_CPUS` (default 4) each, after `REVIEW_CPUS` on a box that also
  runs reviews (on a 16-core bench box with `LANES=["bench"]`: `0-3`, `4-7`, `8-11`, `12-15`);
  `BENCH_SLOTS=2` asks for that many; `BENCH_SLOTS=["2-5","6-9"]` lists them. Slots never share
  a core, and a setting that does not fit (overlapping cpusets, cores the box does not have, a
  count without a core count) falls back to the one default slot; the dispatcher logs which at
  startup and `/healthz` shows the slots, their benches and any error. One PR has at most one
  bench running, and a slot goes back to the PR it ran last (its builds are there), else to one
  with the same base. Each container gets `BENCH_MEMORY` (default `6g`): slots times that must
  fit in the box's memory. The release builds compile as many crates at once as that memory
  holds at `BENCH_BUILD_JOB_MEMORY` (default `4608m`) each, at most one per core of the slot and
  at most 4. The release profile (fat LTO, one codegen unit) puts the large crates and the
  binaries' links at GBs each: measured one crate at a time in a 6g container, `ts_r_lint`'s
  library peaks at 4.3 GB, the `ts-r-lint` link at 3.1 GB, `ts-r-check`'s at 2.2 GB and
  `ts_r_checker` at 1.7 GB. So a 6g container builds one crate at a time (about 23 minutes for a
  cold build of the bench binaries on four cores; with `BENCH_KEEP_BUILDS=1` only what changed);
  four at once were killed for memory on every bench of #104 and #105, and a 16g one builds three. If the kernel still kills a compile for memory, the build goes on one crate at a
  time from where it stopped. The startup log line shows the jobs per slot. Each slot keeps its
  own checkouts and, with
  `BENCH_KEEP_BUILDS=1`, its own two release builds (a few GB) on disk.
- **Measured alone.** Slots share the memory bus, the last-level cache and the disk even on
  disjoint cores. With `BENCH_MEASURE=alone` (the default) a slot measures only while no other
  slot installs, builds or measures: new work waits for the measurement, the measurement waits
  for the work already running, so the numbers come from a box as quiet as with one slot, and
  what overlaps is the fetching, installing and building (most of a run). `BENCH_MEASURE=parallel`
  lets slots measure side by side for more throughput, at the cost of noise between them: the
  gate and its confirming re-run stay the same, so that noise can only make a regression harder
  to confirm or a flaky one easier to flag, never pass a run the gate would fail. Reference
  numbers are kept per mode, so the two never mix.
- **Progress.** The PR's bench comment shows the same kind of progress line as the review
  summary: queued (behind how many), running since when (and on which cores, with several
  slots) and the phase (fetching, installing, building the base, building the PR, waiting for
  another slot, measuring, re-measuring), then the tables and the verdict.
  The result is also the `Bench (review box)` commit status when the App has the "Commit
  statuses: write" permission.
- **Same gate.** `max_regression` and `noise_sigmas` from `bench/scenarios.toml`, every one of
  our tools measured on both sides with the scenarios' own run counts, and a flagged scenario
  fails only if the re-run with the PR measured first flags it too.
- **Caches and cleanup.** The checkouts, crates, pinned repos and package stores stay in
  `bench/` between runs, so a repeat bench downloads nothing it already has; the slots share the
  crates and package stores, and installs (which write them) run one at a time. Builds, bench
  output and results are deleted when a run ends (and at startup, after a crash), so a finished
  bench takes no build space; each run builds the base and the PR in release from the cached
  crates. With `BENCH_KEEP_BUILDS=1` (a box with the disk) both `target/` directories stay: an
  unchanged base is a no-op build (the checkout leaves unchanged files' mtimes alone) and the PR
  build recompiles only what changed. Below 8 GB free the idle slots' builds go first.
- **Only what is measured is built.** `bench-checkout` reads the PR's `bench/scenarios.toml`
  (from git, before any PR code runs) for the binaries it runs (`{bin}/<name>`, and `ts-r-<sub>`
  for the launcher's subcommands), and each side builds `cargo build --release --workspace` with
  a `--bin` for each of those it has. The release profile's whole-program link is what the
  binaries nobody measures (the language server, the dev server, xtask) cost: about 1.5 to 2
  minutes a build. Features resolve as for the whole workspace, so the binaries are the same.
- **Reference tools once.** The base run measures only our tools (`cargo xtask bench --ours-only`);
  tsc, ESLint, esbuild and the rest are measured in the PR's run, which is where the table's
  "vs fastest" column comes from. The confirming re-runs are ours-only too. Their numbers are
  kept (`bench/<repo>/reference/`) and a later run reuses them, measuring only ours on both
  sides, while nothing that could change them changed: the key covers `bench/`, `xtask/`,
  `upstream/` and `scripts/` at the head, the job image, the slot's cores, the repo set and
  `BENCH_MEASURE`, and a kept set is used for `BENCH_REFERENCE_DAYS` (default 7; `0` measures
  them every run). That saves about 5.5 to 6 minutes a run. A reference tool whose scenario uses
  our binaries (a setup that asks `ts-r-lint` for its rules) is always measured. A run where one
  failed keeps nothing, a fork's run keeps nothing, and the comment says which run the reused
  numbers come from. Reference tools are never gated, so a reused row cannot change a verdict.
  Code that already passed against the same base commit (ignoring `.lavish/`, `landing/`, `docs/`
  and the other non-code paths) is not run again. Fix runs delete their build directory when they end too.
- **A newer push wins.** Every push hands the new head to the box, which drops the PR's queued
  bench and stops its running one (its container is removed, and the old head's commit status
  says it was superseded), then queues the new head. Closing or merging the PR hands it over too, and the
  box drops its benches without benching anything; a bench that reaches the front of the queue
  for a PR that closed or moved on meanwhile is not run either. Before, the workflow's single
  concurrency group per PR let any review or comment event after a push cancel that push's
  hand-off, so the box kept benching the old head.
- **PR code runs here.** Unlike a review, a bench builds and runs the PR: `cargo xtask` in
  `bench-deps` (with registry access through the deps proxy) and everything in `bench-build`
  and `bench-measure` (no network). None of them holds a token or the Codex login; all get the
  checkouts' git directories, the repo cache and the job's inputs read-only, and only the
  checkout they need writable (the base's build never sees the PR's checkout writable, and no
  PR step can write the base's). `passed.json` and the reference store are never mounted, and
  the dispatcher reads what a step wrote without following links. That is why only trusted
  authors' PRs are benched automatically, and anyone else's only when a trusted user vouches for
  them. The crates and package stores are still shared between a repo's runs, and the kept
  reference numbers come from a PR's run (checked to be names and numbers, never from a fork),
  so a malicious PR could still skew that repo's later benches or their reference rows; read a
  stranger's PR before you approve it or comment `/bench`.

## Changing the review

- Rules and focus: edit `skills/ts-r-review/` (people review skill changes, like AGENTS.md).
- Threshold: `MIN_CONFIDENCE` in `box/dispatcher.mjs` (default 80) and the rubric in the skill.
- Model and effort: `MODEL` / `EFFORT` in the box's `config.env`; Codex and Rust versions:
  `CODEX_VERSION` and `RUST_TOOLCHAIN` in `box/Dockerfile` (keep the latter in step with
  `rust-toolchain.toml`, then re-run `box/install.sh`).
- Output shape: `ci/findings.schema.json` (and `ci/fixes.schema.json` for fixes) and
  `ci/github.cjs` together. The schemas are read from `main`, `github.cjs` from the box image,
  so `github.cjs` accepts results without the newer fields (`checks`, status `wrong`).
- A finding judged wrong goes into `skills/ts-r-review/references/false-positives.md` (and its
  review run into `ci/eval/cases.json`) in the change that answers it; `ci/eval.test.cjs` checks
  that every eval case is in the catalogue.
- Tests: `node --test .agents/plugins/code-review/ci/*.test.cjs` (an in-memory GitHub and git for
  `github.test.cjs` and `stack.test.cjs`; real git against a bare repository in a temp dir for
  `catchup.test.cjs`).

### Measuring accuracy

Every finding a person answers is a measurement: `action-review` replies confirmed, partly
right, wrong, already handled or out of scope, and the bot's own fix runs reply fixed, not fixed
or judged wrong. Measured on the first 386 findings (tsc-rust PRs 3-138, typescript-rust PRs
23-41): 247 confirmed, 110 fixed by the bot with no person checking them, 6 wrong (one of them
fixed by the bot, then reverted), 2 partly right, 18 unanswered, 1 out of scope and 2 no longer
reported; of the bot's 36 fix commits, 1 had a
part reverted (it disagreed with tsc) and 4 broke tests later (2 on Windows only, 2 after the
merge up the stack). The wrong findings all claimed tsc or ESLint behaviour without evidence of
what the tool does, which is why the skill now asks for it
(`skills/ts-r-review/references/false-positives.md`).

To check a change to the skill or prompts against that history, replay the review on each case
in `ci/eval/cases.json` (the PR head that was reviewed, with **no** previous findings, which
would hand the model the old ids), save each run's JSON as `<dir>/<repo>-<pr>-<head7>.json`, and
score them:

```sh
node .agents/plugins/code-review/ci/eval/score.cjs .agents/plugins/code-review/ci/eval/cases.json <dir>
```

A better change reports fewer of the wrong or partly right findings again and keeps the
confirmed ones. Replays vary between runs; compare several runs per variant, not one.

The change that added the oracle rule was measured more cheaply: only the verify-and-score step
(SKILL.md section 4) was replayed on each of the 27 judged findings in `ci/eval/cases.json`
(Codex, high effort, read-only, code read through git at the reviewed head), with the case rows
removed from `false-positives.md` so the rule had to generalise. Kept means confidence 80 or
more:

| Skill | Wrong kept (of 6) | Partly right kept (of 2) | Confirmed kept (of 19) |
|---|---|---|---|
| before this change | 4 | 2 | 19 |
| first draft: any claim about oracle output needs it recorded | 0 | 1 | 9 |
| final: only parity claims need it (2 runs) | 2, 2 | 2, 2 | 18, 17 |

The first draft dropped real broken-mechanism findings, which is why the rule now separates
the two kinds. The parity claims that still pass verification (tsc-rust #46, #72, #93 across
the runs) are what the fix run's oracle check is for: it runs tsc before changing anything and
reports `wrong` when tsc agrees with ts-r.
