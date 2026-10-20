// GitHub side of the ts-r code review: works out which PR to review, reads previous findings,
// posts new ones as resolvable review threads, keeps the summary comment current, and resolves
// or reopens every finding when someone ticks the summary's checkbox or comments /resolve-all
// (/unresolve-all), and approves on a trusted user's /approve. Every completed review counts as a GitHub review: it requests changes while
// any finding is open and approves when none is, and the verdict follows the findings as they are
// fixed, resolved or reopened (syncVerdict).
// Called from the review box's job containers (box/job.mjs), which pass { github, context, core }
// shaped like actions/github-script's. Plain Node, no dependencies.
'use strict';

const fs = require('fs');
const crypto = require('crypto');

const SUMMARY_MARK = '<!-- ts-r-review:summary -->';
// In a trusted user's PR comment: asks for a refresh (the action-review skill's summary carries it).
const REFRESH_MARK = '<!-- ts-r-review:refresh -->';
const META_RE = /<!-- ts-r-review:meta ([A-Za-z0-9+/=]+) -->/;
const FINDING_RE = /<!-- ts-r-review:finding ([A-Za-z0-9+/=]+) -->/;
const CHECKBOX_RE = /^- \[( |x|X)\] \*\*(?:Fix and resolve|Resolve) all findings\*\*/m;
const SEVERITY_ORDER = { critical: 0, high: 1, medium: 2, low: 3 };
// The review box posts as the ts-r review GitHub App; reviews from before it ran on Actions.
// GraphQL reports bots without the "[bot]" suffix, REST with it.
// GitHub's own animated spinner, shown while a review is queued or running.
const SPINNER = '<img src="https://github.githubassets.com/images/spinners/octocat-spinner-32.gif" width="16" height="16" alt="In progress">';
const BOT_LOGINS = new Set(['ts-r-review', 'ts-r-review[bot]', 'github-actions', 'github-actions[bot]']);

const encode = (value) => Buffer.from(JSON.stringify(value)).toString('base64');
const decode = (text) => {
  try {
    return JSON.parse(Buffer.from(text, 'base64').toString('utf8'));
  } catch {
    return null;
  }
};
const capitalize = (s) => s.charAt(0).toUpperCase() + s.slice(1);
const fence = (text) => '`'.repeat(Math.max(3, ...[...text.matchAll(/`+/g)].map((m) => m[0].length + 1)));
const location = (f) => (f.end_line > f.start_line ? `${f.path}:${f.start_line}-${f.end_line}` : `${f.path}:${f.start_line}`);
const isBot = (login) => BOT_LOGINS.has(login);
const duration = (ms) => {
  const s = Math.max(0, Math.round(ms / 1000));
  return s >= 60 ? `${Math.floor(s / 60)}m ${s % 60}s` : `${s}s`;
};
// How long the review has run, from REVIEW_STARTED (ISO time the dispatcher started it).
const elapsed = () => (process.env.REVIEW_STARTED ? duration(Date.now() - Date.parse(process.env.REVIEW_STARTED)) : null);

// Stable id for a finding the model did not match to a previous one.
function fingerprint(f) {
  const title = f.title.toLowerCase().replace(/[^a-z0-9]+/g, ' ').trim();
  return crypto.createHash('sha1').update(`${f.path}\n${f.category}\n${title}`).digest('hex').slice(0, 12);
}

const THREADS_QUERY = `query($owner: String!, $repo: String!, $number: Int!, $cursor: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id
          isResolved
          resolvedBy { login }
          comments(first: 1) { nodes { databaseId url body author { login } } }
        }
      }
    }
  }
}`;

// Review threads this reviewer opened, with the finding stored in each first comment.
async function listThreads(github, owner, repo, number) {
  const threads = [];
  let cursor = null;
  for (;;) {
    const data = await github.graphql(THREADS_QUERY, { owner, repo, number, cursor });
    const page = data.repository.pullRequest.reviewThreads;
    for (const node of page.nodes) {
      const first = node.comments.nodes[0];
      if (!first || !isBot(first.author?.login)) continue;
      const match = FINDING_RE.exec(first.body);
      const finding = match && decode(match[1]);
      if (!finding) continue;
      // text: the comment as people read it (explanation, rule, suggestion), without the markers.
      const text = first.body.replace(FINDING_RE, '').replace(/\n*<sub>[^\n]*<\/sub>\s*$/, '').trim();
      // resolvedBy: null when the App resolved it (GraphQL types it as a User, which a bot is not).
      threads.push({ id: node.id, resolved: node.isResolved, resolvedBy: node.resolvedBy?.login ?? null, url: first.url, commentId: first.databaseId, finding, text });
    }
    if (!page.pageInfo.hasNextPage) return threads;
    cursor = page.pageInfo.endCursor;
  }
}

async function setResolved(github, threadId, resolved) {
  const name = resolved ? 'resolveReviewThread' : 'unresolveReviewThread';
  await github.graphql(`mutation($id: ID!) { ${name}(input: { threadId: $id }) { thread { id } } }`, { id: threadId });
}

// Puts each thread in the wanted state, then replies saying why. The state change comes first
// so a reply never claims a resolution that did not happen. GitHub's record is then re-read:
// threads still in the wrong state are retried once, and any left over are returned.
async function settleThreads(github, owner, repo, number, changes, core) {
  if (changes.length === 0) return [];
  for (const { thread, resolve, reply } of changes) {
    try {
      await setResolved(github, thread.id, resolve);
      await github.rest.pulls.createReplyForReviewComment({ owner, repo, pull_number: number, comment_id: thread.commentId, body: reply });
    } catch (error) {
      core.warning(`Could not ${resolve ? 'resolve' : 'reopen'} ${thread.url}: ${error.message}`);
    }
  }
  const want = new Map(changes.map((c) => [c.thread.id, c]));
  const wrong = async () => (await listThreads(github, owner, repo, number)).filter((t) => want.has(t.id) && t.resolved !== want.get(t.id).resolve);
  const retry = await wrong();
  if (retry.length === 0) return [];
  for (const thread of retry) {
    const { resolve, reply } = want.get(thread.id);
    try {
      await setResolved(github, thread.id, resolve);
      await github.rest.pulls.createReplyForReviewComment({ owner, repo, pull_number: number, comment_id: thread.commentId, body: reply });
    } catch (error) {
      core.warning(`Retry failed for ${thread.url}: ${error.message}`);
    }
  }
  const left = await wrong();
  for (const thread of left) core.error(`${thread.url} is still ${thread.resolved ? 'resolved' : 'open'}.`);
  return left;
}

const unsettledNote = (left) =>
  left.length === 0 ? undefined : `${left.length} thread${left.length === 1 ? '' : 's'} could not be ${left[0].resolved ? 'reopened' : 'resolved'} on GitHub; comment \`/${left[0].resolved ? 'unresolve' : 'resolve'}-all\` to retry.`;

// The verdict. Any open finding, whatever its severity, requests changes: severity is the
// model's estimate, and a low finding the author disagrees with costs one click to resolve,
// while letting one through on a guess costs a bug. What blocks the approval:
//   open        open threads
//   outside     findings in files the PR does not touch (no thread to resolve; a later review
//               that no longer reports them clears them)
//   unvetted    threads resolved by someone without write access (a PR's author can resolve
//               threads on their own PR), so a bot approval cannot be had by dismissing findings
async function blockingFindings(github, owner, repo, threads, outside = []) {
  const vetted = new Map();
  const canResolve = async (login) => {
    // null: the App resolved it (a fix, a bulk resolve by a trusted user, or a later review).
    if (!login || isBot(login)) return true;
    if (!vetted.has(login)) {
      let ok = false;
      try {
        const { data } = await github.rest.repos.getCollaboratorPermissionLevel({ owner, repo, username: login });
        ok = ['admin', 'write'].includes(data.permission);
      } catch {}
      vetted.set(login, ok);
    }
    return vetted.get(login);
  };
  const counts = { open: 0, outside: outside.length, unvetted: 0, total: 0 };
  for (const t of threads) {
    if (!t.resolved) counts.open += 1;
    else if (!(await canResolve(t.resolvedBy))) counts.unvetted += 1;
  }
  counts.total = counts.open + counts.outside + counts.unvetted;
  return counts;
}

const summaryLink = (url) => (url ? `[summary comment](${url})` : 'summary comment in the conversation');
const plural = (n, word) => `${n} ${word}${n === 1 ? '' : 's'}`;
function describeBlocking({ open, outside, unvetted }) {
  return [
    open && `${plural(open, 'open finding')}`,
    outside && `${plural(outside, 'finding')} outside the diff`,
    unvetted && `${plural(unvetted, 'finding')} resolved by someone without write access`,
  ].filter(Boolean).join(', ');
}

// The bot's standing verdict: its latest review that approved, requested changes or was
// dismissed (comment-only reviews do not count). Reviews come oldest first.
const VERDICT_STATES = new Set(['APPROVED', 'CHANGES_REQUESTED', 'DISMISSED']);
async function latestVerdict(github, owner, repo, number) {
  const reviews = await github.paginate(github.rest.pulls.listReviews, { owner, repo, pull_number: number, per_page: 100 });
  const last = reviews.filter((r) => isBot(r.user?.login) && VERDICT_STATES.has(r.state)).at(-1);
  return last ? { state: last.state, sha: last.commit_id } : null;
}

// Submits a verdict-only review pinned to `sha`, the commit the verdict is about. GitHub can lag
// a push by a few seconds and refuse a commit it has not seen on the PR yet, so it retries that
// same commit after a pause. An approval never falls back to the PR's head, which could be a
// commit nobody reviewed (a force-push while the review ran); requesting changes may.
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));
async function submitVerdict(github, owner, repo, number, sha, event, body, core) {
  const send = (commit) => github.rest.pulls.createReview({ owner, repo, pull_number: number, ...(commit ? { commit_id: commit } : {}), event, body });
  const pause = Number(process.env.TS_R_REVIEW_RETRY_MS ?? 5000);
  let failure;
  for (let attempt = 0; attempt < 3; attempt += 1) {
    if (attempt > 0) await sleep(pause);
    try {
      await send(sha);
      core.info(`Submitted ${event} on #${number}.`);
      return true;
    } catch (error) {
      failure = error;
    }
  }
  if (event !== 'APPROVE') {
    try {
      await send(null);
      core.info(`Submitted ${event} on #${number} (at its head).`);
      return true;
    } catch (error) {
      failure = error;
    }
  }
  core.warning(`Could not submit the ${event} review at ${String(sha).slice(0, 7)}: ${failure.message}${event === 'APPROVE' ? '; not approving the PR\'s head instead' : ''}`);
  return false;
}

// Keeps the verdict in step with the findings after a fix, a bulk resolve or reopen, or a refresh
// (someone resolved threads by hand). Approves when nothing blocks and the standing verdict is not
// already an approval, whatever head it was given on: approvals stay valid across pushes unless
// the repo dismisses stale ones, and re-approving each new head would claim a review of commits
// nobody reviewed. Requests changes again when something blocks and the standing verdict is an
// approval. A dismissed verdict is left alone (a person dismissed it, or a push did under "dismiss
// stale approvals"); the next review sets one again. Only after a completed review (meta.sha) and
// on open PRs. Returns a note for the summary when an approval is held back, else undefined.
async function syncVerdict({ github, owner, repo, number, meta, threads, core, sha, summaryUrl }) {
  if (!meta.sha) return undefined;
  const { data: pr } = await github.rest.pulls.get({ owner, repo, pull_number: number });
  if (pr.state !== 'open') return undefined;
  const head = sha || pr.head.sha;
  const blocking = await blockingFindings(github, owner, repo, threads, meta.outside);
  const verdict = await latestVerdict(github, owner, repo, number);
  if (blocking.total === 0) {
    if (verdict?.state !== 'APPROVED' && verdict?.state !== 'DISMISSED') {
      const n = threads.length;
      await submitVerdict(github, owner, repo, number, head, 'APPROVE',
        `ts-r code review: ${n === 0 ? 'no findings are open' : n === 1 ? 'the finding is resolved' : `all ${n} findings are resolved`}, so this is approved at ${head.slice(0, 7)}. Later pushes are not reviewed by themselves; comment \`@autoreview\` for that.`, core);
    }
    return undefined;
  }
  // An /approve stands over open findings until the next completed review.
  if (verdict?.state === 'APPROVED' && !meta.approved) {
    await submitVerdict(github, owner, repo, number, head, 'REQUEST_CHANGES',
      `ts-r code review: ${describeBlocking(blocking)} at ${head.slice(0, 7)}, so changes are requested again. The list is in the ${summaryLink(summaryUrl)}.`, core);
  }
  return heldNote(blocking);
}

const heldNote = ({ unvetted }) => (unvetted
  ? `Not approved: ${plural(unvetted, 'finding')} ${unvetted === 1 ? 'was' : 'were'} resolved by someone without write access. A trusted user can comment \`/unresolve-all\` and then \`/resolve-all\` to resolve ${unvetted === 1 ? 'it' : 'them'} on their behalf.`
  : undefined);

async function findSummary(github, owner, repo, number) {
  const comments = await github.paginate(github.rest.issues.listComments, { owner, repo, issue_number: number, per_page: 100 });
  return comments.findLast((c) => isBot(c.user?.login) && c.body?.includes(SUMMARY_MARK)) ?? null;
}

function readMeta(comment) {
  const match = comment && META_RE.exec(comment.body);
  return (match && decode(match[1])) || {};
}

function renderSummary(meta, threads) {
  const lines = [SUMMARY_MARK, `<!-- ts-r-review:meta ${encode(meta)} -->`, '### Code review', ''];
  if (meta.status) lines.push(`> ${meta.busy ? `${SPINNER} ` : ''}${meta.status}`, '');
  if (meta.warning) lines.push(`> [!WARNING]`, `> ${meta.warning}`, '');
  if (meta.held) lines.push(`> [!NOTE]`, `> ${meta.held}`, '');
  if (meta.approved) lines.push(`> Approved at \`${meta.approved.sha.slice(0, 7)}\` by @${meta.approved.by} with \`/approve\`; open findings no longer request changes until the next review.`, '');
  if (meta.summary) lines.push(meta.summary, '');

  const sorted = [...threads].sort(
    (a, b) => a.resolved - b.resolved || SEVERITY_ORDER[a.finding.severity] - SEVERITY_ORDER[b.finding.severity],
  );
  const outside = meta.outside ?? [];
  if (sorted.length === 0 && outside.length === 0) {
    lines.push(meta.sha ? 'No issues found. Checked for bugs and AGENTS.md compliance.' : 'No findings yet.', '');
  }
  if (sorted.length > 0) {
    const open = sorted.filter((t) => !t.resolved).length;
    lines.push(`**${open} open** of ${sorted.length} finding${sorted.length === 1 ? '' : 's'}. Resolve each conversation inline, or all of them here. The review requests changes while any is open and approves the PR once all are resolved. States are as of the last push, review comment or bulk action.`, '');
    lines.push('| State | Severity | Finding | Location |', '|---|---|---|---|');
    for (const t of sorted) {
      const f = t.finding;
      const title = t.resolved ? `~~${f.title}~~` : f.title;
      lines.push(`| ${t.resolved ? 'Resolved' : 'Open'} | ${capitalize(f.severity)} | [${title.replace(/\|/g, '\\|')}](${t.url}) | \`${location(f)}\` |`);
    }
    lines.push('');
    const allResolved = open === 0;
    lines.push(
      `- [${allResolved ? 'x' : ' '}] **Fix and resolve all findings** (tick to have Codex fix every open finding, reply on its thread with the change and resolve it; untick to reopen them. \`/resolve-all\` resolves without fixing, \`/unresolve-all\` reopens)`,
      '',
    );
    if (meta.bulk) lines.push(`<sub>${meta.bulk}</sub>`, '');
  }
  if (outside.length > 0) {
    lines.push('**Outside the diff** (no thread to resolve; they clear when a later review no longer reports them):', '');
    outside.forEach((f, i) => {
      lines.push(`${i + 1}. **${capitalize(f.severity)}** ${f.title} (\`${location(f)}\`)`, '');
      lines.push('<details><summary>Details</summary>', '', f.body, '');
      if (f.rule) lines.push(`> ${f.rule}`, '');
      lines.push('</details>', '');
    });
  }
  const footer = [];
  if (meta.sha) footer.push(`Reviewed ${meta.sha.slice(0, 7)}${meta.duration ? ` in ${meta.duration}` : ''} with ${meta.model}`);
  if (meta.runUrl) footer.push(`[run](${meta.runUrl})`);
  if (meta.dropped) footer.push(`${meta.dropped} lower-confidence candidate${meta.dropped === 1 ? '' : 's'} not shown`);
  if (footer.length) lines.push(`<sub>${footer.join(' · ')}</sub>`);
  return lines.join('\n');
}

async function upsertSummary(github, owner, repo, number, existing, body) {
  if (existing) await github.rest.issues.updateComment({ owner, repo, comment_id: existing.id, body });
  else await github.rest.issues.createComment({ owner, repo, issue_number: number, body });
}

// Right-side lines GitHub accepts review comments on, and the hunk each belongs to.
function commentableLines(patch) {
  const lines = new Map();
  let line = 0;
  let hunk = -1;
  for (const text of (patch ?? '').split('\n')) {
    const header = /^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(text);
    if (header) {
      line = Number(header[1]);
      hunk += 1;
    } else if (hunk >= 0 && (text.startsWith('+') || text.startsWith(' '))) {
      lines.set(line, hunk);
      line += 1;
    }
  }
  return lines;
}

function renderFinding(f, id, model, withSuggestion) {
  const parts = [`<!-- ts-r-review:finding ${encode({ id, title: f.title, severity: f.severity, category: f.category, path: f.path, start_line: f.start_line, end_line: f.end_line })} -->`];
  parts.push(`**${capitalize(f.severity)}** · \`${f.category}\` · ${f.title}`, '', f.body);
  if (f.rule) parts.push('', `> ${f.rule}`);
  if (withSuggestion && f.suggestion != null) {
    const ticks = fence(f.suggestion);
    parts.push('', `${ticks}suggestion`, f.suggestion.replace(/\n$/, ''), ticks);
  }
  parts.push('', `<sub>Confidence ${f.confidence}/100 · ${model} · Resolve this conversation once it is handled, or if you disagree.</sub>`);
  return parts.join('\n');
}

function validFinding(f) {
  return (
    f && typeof f.title === 'string' && typeof f.body === 'string' && typeof f.path === 'string' &&
    Number.isInteger(f.start_line) && Number.isInteger(f.end_line) && f.start_line >= 1 &&
    f.severity in SEVERITY_ORDER && Number.isInteger(f.confidence)
  );
}

// The PR to review: the pull_request event's, the one an @autoreview comment is on, or the
// workflow_dispatch input. Exports PR_NUMBER, BASE_SHA and HEAD_SHA, and writes the title and
// description to PR_PATH for the prompt.
async function resolvePr({ github, context, core }) {
  const { owner, repo } = context.repo;
  const { payload } = context;
  const number = Number(payload.pull_request?.number ?? payload.issue?.number ?? payload.inputs?.pr);
  if (!Number.isInteger(number) || number < 1) {
    core.setFailed(`Not a pull request number: ${payload.inputs?.pr}`);
    return;
  }
  const { data: pr } = await github.rest.pulls.get({ owner, repo, pull_number: number });
  if (context.eventName === 'issue_comment') {
    await github.rest.reactions.createForIssueComment({ owner, repo, comment_id: payload.comment.id, content: 'eyes' });
  }
  core.exportVariable('PR_NUMBER', String(number));
  core.exportVariable('BASE_SHA', pr.base.sha);
  core.exportVariable('HEAD_SHA', pr.head.sha);
  fs.writeFileSync(process.env.PR_PATH, JSON.stringify({
    title: pr.title, body: pr.body ?? '', author: pr.user.login, base_ref: pr.base.ref,
    head_ref: pr.head.ref, head_repo: pr.head.repo?.full_name ?? '(deleted fork)',
  }));
  core.info(`Reviewing #${number} by @${pr.user.login}: ${pr.base.ref} <- ${pr.head.label} at ${pr.head.sha}`);
}

// Before the review: write this reviewer's previous findings for the prompt.
async function collectPrevious({ github, context }) {
  const { owner, repo } = context.repo;
  const number = Number(process.env.PR_NUMBER);
  const threads = await listThreads(github, owner, repo, number);
  const meta = readMeta(await findSummary(github, owner, repo, number));
  const previous = [
    ...threads.map((t) => ({ ...t.finding, state: t.resolved ? 'resolved' : 'open' })),
    ...(meta.outside ?? []).map((f) => ({ id: f.id, title: f.title, severity: f.severity, category: f.category, path: f.path, start_line: f.start_line, end_line: f.end_line, state: 'open (outside the diff)' })),
  ];
  fs.writeFileSync(process.env.PREVIOUS_PATH, JSON.stringify(previous, null, 2));
}

// After the review: post new findings, close the ones no longer reported, refresh the summary.
async function postReview({ github, context, core }) {
  const { owner, repo } = context.repo;
  const number = Number(process.env.PR_NUMBER);
  const sha = process.env.HEAD_SHA;
  const model = process.env.MODEL;
  const minConfidence = Number(process.env.MIN_CONFIDENCE ?? 80);
  const runUrl = process.env.RUN_URL || (context.runId ? `${context.serverUrl}/${owner}/${repo}/actions/runs/${context.runId}` : '');

  const existing = await findSummary(github, owner, repo, number);
  const previousMeta = readMeta(existing);
  let threads = await listThreads(github, owner, repo, number);

  let result = null;
  if (process.env.REVIEW_OUTCOME === 'success') {
    try {
      result = JSON.parse(fs.readFileSync(process.env.FINDINGS_PATH, 'utf8'));
      if (typeof result.summary !== 'string' || !Array.isArray(result.findings)) throw new Error('unexpected shape');
    } catch (error) {
      core.error(`Could not read the review output: ${error.message}`);
      result = null;
    }
  }
  if (!result) {
    const took = elapsed();
    const meta = { ...previousMeta, busy: false, failed_at: new Date().toISOString(), status: `The review of \`${sha.slice(0, 7)}\` did not complete${took ? ` (stopped after ${took})` : ''}${runUrl ? ` ([run](${runUrl}))` : ''}. Findings below are from the last completed review.` };
    await upsertSummary(github, owner, repo, number, existing, renderSummary(meta, threads));
    core.setFailed('Code review did not produce findings.');
    return;
  }

  const valid = result.findings.filter(validFinding);
  const kept = valid.filter((f) => f.confidence >= minConfidence)
    .sort((a, b) => SEVERITY_ORDER[a.severity] - SEVERITY_ORDER[b.severity]);
  const dropped = result.findings.length - kept.length;

  const threadIds = new Map(threads.map((t) => [t.finding.id, t]));
  const outsideIds = new Map((previousMeta.outside ?? []).map((f) => [f.id, f]));
  const files = await github.paginate(github.rest.pulls.listFiles, { owner, repo, pull_number: number, per_page: 100 });
  const diff = new Map(files.filter((f) => f.status !== 'removed').map((f) => [f.filename, commentableLines(f.patch)]));

  const reported = new Set();
  const changes = [];
  const resolvedBefore = new Set();
  if (process.env.PREVIOUS_PATH && fs.existsSync(process.env.PREVIOUS_PATH)) {
    for (const f of JSON.parse(fs.readFileSync(process.env.PREVIOUS_PATH, 'utf8'))) if (f.state === 'resolved') resolvedBefore.add(f.id);
  }
  const inline = [];
  const fileLevel = [];
  const outside = [];
  for (const f of kept) {
    const id = f.id && (threadIds.has(f.id) || outsideIds.has(f.id)) ? f.id : fingerprint(f);
    if (reported.has(id)) continue;
    reported.add(id);
    const thread = threadIds.get(id);
    if (thread) {
      // Reported again after a person resolved it: the prompt only allows that when the new
      // code made it worse, so reopen the thread with the new explanation. Only when the model
      // was told it was resolved; a thread resolved while the review ran stays resolved.
      if (thread.resolved && resolvedBefore.has(id)) {
        changes.push({
          thread, resolve: false,
          reply: `Reported again in the review of ${sha.slice(0, 7)} (\`${location(f)}\`), so this is reopened:\n\n${f.body}`,
        });
      }
      continue;
    }
    const lines = diff.get(f.path);
    if (!lines) {
      outside.push({ ...f, id });
      continue;
    }
    const end = Math.max(f.start_line, f.end_line);
    if (lines.has(end) && lines.has(f.start_line) && lines.get(end) === lines.get(f.start_line)) {
      const comment = { path: f.path, line: end, side: 'RIGHT', body: renderFinding(f, id, model, true) };
      if (f.start_line < end) Object.assign(comment, { start_line: f.start_line, start_side: 'RIGHT' });
      inline.push({ comment, finding: { ...f, id } });
    } else if (lines.has(end) || lines.has(f.start_line)) {
      const line = lines.has(end) ? end : f.start_line;
      inline.push({ comment: { path: f.path, line, side: 'RIGHT', body: renderFinding(f, id, model, false) }, finding: { ...f, id } });
    } else {
      fileLevel.push({ ...f, id });
    }
  }

  // The review is a GitHub review with a verdict (see blockingFindings): new inline findings go
  // in a REQUEST_CHANGES review; otherwise a verdict-only review follows once the threads are
  // settled. Open after this review: the new findings, and the reported ones whose threads are
  // open or being reopened.
  const carried = threads.filter((t) => reported.has(t.finding.id) && (!t.resolved || resolvedBefore.has(t.finding.id))).length;
  const openAfter = inline.length + fileLevel.length + outside.length + carried;
  let verdict = null;
  if (inline.length > 0) {
    try {
      await github.rest.pulls.createReview({
        owner, repo, pull_number: number, commit_id: sha, event: 'REQUEST_CHANGES',
        body: `ts-r code review of ${sha.slice(0, 7)}: ${plural(openAfter, 'open finding')} (${inline.length} new inline), so changes are requested until they are fixed or resolved. The list and "resolve all" are in the ${summaryLink(existing?.html_url)}.`,
        comments: inline.map((i) => i.comment),
      });
      verdict = 'REQUEST_CHANGES';
    } catch (error) {
      core.warning(`Batch review failed (${error.message}); posting findings one by one.`);
      for (const { comment, finding } of inline) {
        try {
          await github.rest.pulls.createReviewComment({ owner, repo, pull_number: number, commit_id: sha, ...comment });
        } catch (inner) {
          core.warning(`Could not attach ${location(finding)}: ${inner.message}`);
          outside.push(finding);
        }
      }
    }
  }
  for (const finding of fileLevel) {
    try {
      await github.rest.pulls.createReviewComment({
        owner, repo, pull_number: number, commit_id: sha, path: finding.path, subject_type: 'file',
        body: renderFinding(finding, finding.id, model, false),
      });
    } catch (error) {
      core.warning(`Could not attach ${location(finding)} to the file: ${error.message}`);
      outside.push(finding);
    }
  }
  // Open threads the new review no longer reports were fixed by the new commits.
  for (const thread of threads) {
    // Not reported because it was resolved when the review started (the prompt says not to repeat
    // those): if someone reopened it since, that is no evidence it was fixed.
    if (thread.resolved || reported.has(thread.finding.id) || resolvedBefore.has(thread.finding.id)) continue;
    changes.push({
      thread, resolve: true,
      reply: `No longer reported in the review of ${sha.slice(0, 7)}, so this looks fixed. Unresolve if it still applies.`,
    });
  }
  const left = await settleThreads(github, owner, repo, number, changes, core);

  threads = await listThreads(github, owner, repo, number);
  const meta = {
    sha, model: `${model} (${process.env.EFFORT})`, duration: elapsed(), reviewed_at: new Date().toISOString(), runUrl, summary: result.summary, dropped, warning: unsettledNote(left),
    outside: outside.map(({ id, title, severity, category, path, start_line, end_line, body, rule }) => ({ id, title, severity, category, path, start_line, end_line, body, rule })),
  };
  // Every completed review submits a verdict, unless the inline batch already carried the right one.
  const blocking = await blockingFindings(github, owner, repo, threads, meta.outside);
  // Threads this run just opened are open whether or not GitHub lists them yet.
  const posted = inline.length + fileLevel.length;
  if (blocking.total === 0 && posted > 0) Object.assign(blocking, { open: posted, total: posted });
  const event = blocking.total > 0 ? 'REQUEST_CHANGES' : 'APPROVE';
  meta.held = heldNote(blocking);
  if (verdict !== event) {
    await submitVerdict(github, owner, repo, number, sha, event, event === 'APPROVE'
      ? `ts-r code review of ${sha.slice(0, 7)}: no open findings, so this is approved. Checked for bugs and AGENTS.md compliance.`
      : `ts-r code review of ${sha.slice(0, 7)}: ${describeBlocking(blocking)}, so changes are requested until they are fixed or resolved. The list and "resolve all" are in the ${summaryLink(existing?.html_url)}.`, core);
  }
  await upsertSummary(github, owner, repo, number, existing, renderSummary(meta, threads));
  core.notice(`${inline.length + fileLevel.length} new findings posted, ${outside.length} outside the diff, ${dropped} dropped.`);
}

// Progress line at the top of the summary comment (queued, running, ...); a completed review
// replaces it. `fresh` (a new review was requested) posts the summary as a new comment at the
// bottom of the conversation, where people will see it, and deletes the old one.
async function setStatus({ github, context, status, fresh = false, busy = false }) {
  const { owner, repo } = context.repo;
  const number = Number(process.env.PR_NUMBER);
  const existing = await findSummary(github, owner, repo, number);
  const body = renderSummary({ ...readMeta(existing), status, busy }, await listThreads(github, owner, repo, number));
  if (!fresh || !existing) {
    await upsertSummary(github, owner, repo, number, existing, body);
    return;
  }
  await github.rest.issues.createComment({ owner, repo, issue_number: number, body });
  try {
    await github.rest.issues.deleteComment({ owner, repo, comment_id: existing.id });
  } catch {
    // Not ours to delete (e.g. posted by an earlier setup): retire it instead.
    await github.rest.issues.updateComment({ owner, repo, comment_id: existing.id, body: '_Superseded by the latest code review summary below._' });
  }
}

// What a bulk command or a summary edit asks for: true to resolve every finding, false to reopen
// them, null for nothing.
function bulkIntent({ action, comment, changes }, threads) {
  if (action === 'created') {
    const command = comment.body.trim().split(/\s/)[0];
    if (command === '/resolve-all') return true;
    if (command === '/unresolve-all') return false;
    return null;
  }
  if (action !== 'edited' || !isBot(comment.user?.login) || !comment.body.includes(SUMMARY_MARK)) return null;
  const after = CHECKBOX_RE.exec(comment.body);
  const before = changes?.body?.from === undefined ? undefined : CHECKBOX_RE.exec(changes.body.from);
  const checked = after && after[1] !== ' ';
  if (before !== undefined) {
    // The event says what changed.
    return before && after && (before[1] === ' ') === !checked ? checked : null;
  }
  if (!after) return null;
  // Only the current text: the box renders the box ticked exactly when every finding is
  // resolved, so ticked with open findings means "resolve", and unticked with none open
  // means "reopen". Anything else was an unrelated edit.
  const open = threads.filter((t) => !t.resolved).length;
  if (checked && open > 0) return true;
  if (!checked && threads.length > 0 && open === 0) return false;
  return null;
}

// Results of a Codex fix run (box/job.mjs push): replies on every open finding, with the commit
// and diff when it was fixed (then resolves it) or the reason when it was not (left open).
//   FIXES_PATH   Codex's result ({ summary, commit_message, results: [{ id, status, explanation, files }] })
//   FIX_COMMIT   the pushed commit, if any; FIX_DIFFS_PATH: { [finding id]: diff text }
//   FIX_ERROR    why nothing could be pushed; ACTOR: who asked
async function applyFixes({ github, context, core }) {
  const { owner, repo } = context.repo;
  const number = Number(process.env.PR_NUMBER);
  const actor = process.env.ACTOR;
  const commit = process.env.FIX_COMMIT || null;
  const took = elapsed();
  const summary = await findSummary(github, owner, repo, number);
  const meta = { ...readMeta(summary), busy: false, status: undefined, warning: undefined };
  const threads = await listThreads(github, owner, repo, number);
  const save = async ({ verdict = false } = {}) => {
    const now = await listThreads(github, owner, repo, number);
    // After a fix: approve once Codex (and anyone before it) resolved every finding, at the commit it pushed.
    if (verdict) meta.held = await syncVerdict({ github, owner, repo, number, meta, threads: now, core, sha: commit, summaryUrl: summary?.html_url });
    await upsertSummary(github, owner, repo, number, summary, renderSummary(meta, now));
  };

  let result = null;
  try {
    result = JSON.parse(fs.readFileSync(process.env.FIXES_PATH, 'utf8'));
    if (!Array.isArray(result.results)) throw new Error('unexpected shape');
  } catch (error) {
    core.error(`Could not read the fix output: ${error.message}`);
  }
  if (!result || process.env.FIX_ERROR) {
    meta.status = process.env.FIX_ERROR
      ? `Codex prepared fixes for @${actor} but they could not be pushed: ${process.env.FIX_ERROR} Nothing was resolved.`
      : `The fix run for @${actor} did not complete${took ? ` (stopped after ${took})` : ''}. Nothing was changed.`;
    await save();
    core.setFailed(meta.status);
    return;
  }

  let diffs = {};
  try {
    diffs = JSON.parse(fs.readFileSync(process.env.FIX_DIFFS_PATH, 'utf8'));
  } catch {}
  const commitUrl = commit && `https://github.com/${owner}/${repo}/commit/${commit}`;
  const byId = new Map(result.results.map((r) => [r.id, r]));
  const updates = [];
  let declined = 0;
  for (const thread of threads.filter((t) => !t.resolved)) {
    const r = byId.get(thread.finding.id);
    if (r?.status === 'fixed' && commit) {
      const diff = diffs[thread.finding.id];
      const lines = [`Fixed in [\`${commit.slice(0, 7)}\`](${commitUrl}) by Codex for @${actor}.`, '', r.explanation];
      if (diff) lines.push('', '<details><summary>Change</summary>', '', `${fence(diff)}diff`, diff, fence(diff), '', '</details>');
      updates.push({ thread, resolve: true, reply: lines.join('\n') });
      continue;
    }
    declined += 1;
    const why = !r ? 'Codex did not report on this finding.' : r.status === 'fixed' ? `Codex changed nothing that could be pushed for it. ${r.explanation}` : r.explanation;
    try {
      await github.rest.pulls.createReplyForReviewComment({
        owner, repo, pull_number: number, comment_id: thread.commentId,
        body: `Not fixed by Codex (asked by @${actor}), so this stays open: ${why}`,
      });
    } catch (error) {
      core.warning(`Could not reply on ${thread.url}: ${error.message}`);
    }
  }
  const left = await settleThreads(github, owner, repo, number, updates, core);
  const fixed = updates.length - left.length;
  meta.bulk = [
    commit ? `Codex fixed ${fixed} finding${fixed === 1 ? '' : 's'} in [\`${commit.slice(0, 7)}\`](${commitUrl}) for @${actor}` : `Codex changed nothing for @${actor}`,
    took && ` in ${took}`,
    declined ? `; ${declined} not fixed and left open (the reason is on ${declined === 1 ? 'its thread' : 'each thread'}).` : '.',
  ].filter(Boolean).join('');
  meta.warning = unsettledNote(left);
  await save({ verdict: true });
  core.notice(meta.bulk);
}

// context.eventName:
//   issue_comment  the summary checkbox was toggled (action "edited"), or /resolve-all or
//                  /unresolve-all was posted (action "created"); payload.sender is who did it
//   refresh        re-render the summary from the threads' current state (someone reviewed or
//                  replied, which is the closest signal to a thread being resolved by hand, or a
//                  trusted user's comment carried REFRESH_MARK)
// Either way the verdict then follows the findings (syncVerdict).
async function resolveAll({ github, context, core }) {
  const { owner, repo } = context.repo;
  const { action, comment, issue, sender, changes } = context.payload;
  const number = issue.number;

  const summary = await findSummary(github, owner, repo, number);
  const meta = readMeta(summary);
  const threads = await listThreads(github, owner, repo, number);
  const verdict = async (now) => {
    meta.held = await syncVerdict({ github, owner, repo, number, meta, threads: now, core, summaryUrl: summary?.html_url });
  };
  if (context.eventName === 'refresh') {
    if (!summary) return;
    await verdict(threads);
    await upsertSummary(github, owner, repo, number, summary, renderSummary(meta, threads));
    return;
  }

  const resolve = bulkIntent({ action, comment, changes }, threads);
  if (resolve === null) {
    core.info('Nothing to do for this comment.');
    return;
  }

  const { data: permission } = await github.rest.repos.getCollaboratorPermissionLevel({ owner, repo, username: sender.login });
  if (!['admin', 'write'].includes(permission.permission)) {
    core.notice(`@${sender.login} has ${permission.permission} access; resolving findings needs write.`);
    if (summary) await upsertSummary(github, owner, repo, number, summary, renderSummary(meta, threads));
    return;
  }

  const updates = threads.filter((t) => t.resolved !== resolve).map((thread) => ({
    thread, resolve,
    reply: resolve ? `Resolved by @${sender.login} with "resolve all".` : `Reopened by @${sender.login} with "unresolve all".`,
  }));
  const left = await settleThreads(github, owner, repo, number, updates, core);
  const changed = updates.length - left.length;
  if (action === 'created') {
    await github.rest.reactions.createForIssueComment({ owner, repo, comment_id: comment.id, content: '+1' });
  }
  meta.bulk = `${resolve ? 'Resolved' : 'Reopened'} ${changed} finding${changed === 1 ? '' : 's'} for @${sender.login}.`;
  // Reopening is a person's call too: it takes back an /approve.
  if (!resolve) delete meta.approved;
  meta.warning = unsettledNote(left);
  const now = await listThreads(github, owner, repo, number);
  await verdict(now);
  if (summary) await upsertSummary(github, owner, repo, number, summary, renderSummary(meta, now));
  core.notice(meta.bulk);
  if (left.length > 0) core.setFailed(meta.warning);
}

// /approve from a trusted user (the dispatcher checks the list) with write access: approves the
// PR at its head now, whatever the findings say, for a person's call that they do not block (for
// example findings judged wrong but left open for the record). Threads are left as they are. The
// approval stands until the next completed review or a bulk reopen: refreshes and resolves do not
// request changes again over it (meta.approved). A later push is not approved by itself.
async function approveOnRequest({ github, context, core }) {
  const { owner, repo } = context.repo;
  const { comment, issue, sender } = context.payload;
  const number = issue.number;
  const react = (content) => (comment
    ? github.rest.reactions.createForIssueComment({ owner, repo, comment_id: comment.id, content }).catch(() => {})
    : undefined);
  const { data: permission } = await github.rest.repos.getCollaboratorPermissionLevel({ owner, repo, username: sender.login });
  if (!['admin', 'write'].includes(permission.permission)) {
    core.notice(`@${sender.login} has ${permission.permission} access; /approve needs write.`);
    await react('-1');
    return;
  }
  const { data: pr } = await github.rest.pulls.get({ owner, repo, pull_number: number });
  if (pr.state !== 'open') {
    core.notice(`#${number} is ${pr.state}; nothing to approve.`);
    await react('-1');
    return;
  }
  const head = pr.head.sha;
  const threads = await listThreads(github, owner, repo, number);
  const open = threads.filter((t) => !t.resolved).length;
  const approved = await submitVerdict(github, owner, repo, number, head, 'APPROVE',
    `Approved at ${head.slice(0, 7)} for @${sender.login} with \`/approve\`${open ? `, with ${plural(open, 'finding')} still open` : ''}. Later pushes are not approved by themselves; comment \`/approve\` again or \`@autoreview\` for a review.`, core);
  await react(approved ? '+1' : 'confused');
  if (!approved) return;
  const summary = await findSummary(github, owner, repo, number);
  if (summary) {
    const meta = { ...readMeta(summary), approved: { by: sender.login, sha: head } };
    await upsertSummary(github, owner, repo, number, summary, renderSummary(meta, threads));
  }
  core.notice(`Approved #${number} at ${head.slice(0, 7)} for @${sender.login}.`);
}

module.exports = {
  SPINNER, REFRESH_MARK, isBot, resolvePr, findSummary, setStatus, collectPrevious, postReview, resolveAll, approveOnRequest, bulkIntent, applyFixes, listThreads, commentableLines, renderSummary, fingerprint,
};
