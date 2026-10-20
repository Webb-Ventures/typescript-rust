// Who may start what on the review box (box/dispatcher.mjs), as pure functions over what GitHub
// says, so the rules are testable without GitHub. "Trusted" is the box's TRUSTED_USERS.
//
// Reviews: automatic for a trusted author's non-draft PR from a branch of the same repository
//   (once per PR; the dispatcher checks for an earlier summary), skipped for the mirror's `sync/`
//   PRs. On request, on any PR (forks too): `@autoreview`, `/approve`, `/resolve-all`,
//   `/unresolve-all` or the refresh marker from a trusted user, or a manual run by one.
//   `/fix-all` and ticking the summary's box push to the PR branch: same-repository PRs only.
// Benches build and run the PR's code (offline, with no token): automatic for a trusted author's
//   same-repository PR at the top of its stack (not `sync/`); after a trusted user approves the
//   PR's current head; on a trusted user's `/bench` comment; or a manual run by one. The last
//   three work for forks too.
'use strict';

const { REFRESH_MARK } = require('./github.cjs');

const SUMMARY_MARK = '<!-- ts-r-review:summary -->';
// The public mirror's sync PRs carry code already reviewed and benched in the private repo.
const SYNC_PREFIX = 'sync/';
const REPO_RE = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;

// REPOS (a JSON array of owner/name), or the single REPO. Throws on a malformed setting: the
// dispatcher must not start with a configuration it cannot enforce.
function parseRepos(reposJson, repo) {
  const repos = reposJson ? JSON.parse(reposJson) : [repo ?? 'Webb-Ventures/tsc-rust'];
  if (!Array.isArray(repos) || repos.length === 0 || !repos.every((r) => typeof r === 'string' && REPO_RE.test(r))) {
    throw new Error('REPOS must be a non-empty JSON array of "owner/name" strings');
  }
  return [...new Set(repos)];
}

// The request body is only a pointer: { run_id, pr, comment_id, repo }. A missing repo means the
// first configured one (workflows from before the box served several).
function parseRequest(body, repos) {
  if (!body || typeof body !== 'object') return { error: [400, 'malformed request'] };
  const runId = Number(body.run_id);
  if (!Number.isSafeInteger(runId) || runId < 1) return { error: [400, 'no run id'] };
  const repo = body.repo ?? repos[0];
  if (typeof repo !== 'string' || !repos.includes(repo)) return { error: [403, 'not a repository this box serves'] };
  const pr = Number(body.pr);
  if (!Number.isSafeInteger(pr) || pr < 1) return { error: [400, 'no pull request'] };
  let commentId = null;
  if (body.comment_id !== undefined && body.comment_id !== null && body.comment_id !== '') {
    commentId = Number(body.comment_id);
    if (!Number.isSafeInteger(commentId) || commentId < 1) return { error: [400, 'malformed comment id'] };
  }
  return { runId, pr, repo, commentId };
}

// The workflow run must be one of ours, in the requested repository, and current.
function checkRun(run, { repo, workflows, now }) {
  if (!workflows.includes(run.path) || run.repository?.full_name !== repo) return [403, 'not a code-review or bench run'];
  if (!['queued', 'in_progress'].includes(run.status) || now - Date.parse(run.created_at) > 15 * 60_000) return [403, 'run is not current'];
  return null;
}

const sameRepo = (pull, repo) => pull.head.repo?.full_name === repo;
const isSync = (pull) => (pull.head.ref ?? '').startsWith(SYNC_PREFIX);
const firstWord = (comment) => (comment.body ?? '').trim().split(/\s/)[0].toLowerCase();
const onPr = (comment, repo, pr) => typeof comment.issue_url === 'string' && comment.issue_url.endsWith(`/repos/${repo}/issues/${pr}`);
const reject = (code, text) => ({ reject: [code, text] });

// What a code-review run asks for. Returns { reject: [status, text] } or { action, by, react }:
//   auto-review   review, unless the PR already has a summary (the dispatcher checks)
//   review | fix  queue a Codex job; approve | resolve | refresh  run the resolve step
//   summary-edit  the bot's summary was edited: a fix when its box was ticked (and canFix), else a resolve
// `react` asks for an eyes reaction on the comment.
function reviewDecision({ run, actor, pull, comment, repo, trusted }) {
  const pr = pull.number;
  // Fixes are pushed to the PR branch, so only for open PRs from this repository.
  const canFix = pull.state === 'open' && sameRepo(pull, repo);
  switch (run.event) {
    case 'pull_request':
      if (pull.head.sha !== run.head_sha) return reject(409, 'the PR moved on; a newer run will follow');
      if (pull.draft || !sameRepo(pull, repo)) return reject(403, 'not an automatic review');
      if (isSync(pull)) return reject(409, `\`${pull.head.ref}\` is a mirror sync PR, reviewed in the private repo; comment @autoreview to review it here`);
      if (!trusted.has(pull.user?.login) || !trusted.has(actor)) return reject(403, 'author or trigger not trusted');
      return { action: 'auto-review', by: actor, canFix };
    case 'workflow_dispatch':
      if (!trusted.has(actor)) return reject(403, 'not trusted');
      return { action: 'review', by: actor, canFix };
    case 'issue_comment': {
      if (!comment) return reject(400, 'no comment');
      if (!onPr(comment, repo, pr)) return reject(400, 'comment is not on this PR');
      const by = comment.user?.login;
      const command = firstWord(comment);
      const human = trusted.has(by) && comment.user?.type !== 'Bot';
      if (command === '@autoreview' && human) return { action: 'review', by, react: true, canFix };
      if (command === '/fix-all' && human) {
        if (!canFix) return reject(403, 'fixes can only be pushed to open PRs from this repository');
        return { action: 'fix', by, react: true, canFix };
      }
      if (command === '/approve' && human) {
        if (pull.state !== 'open') return reject(403, 'only open PRs can be approved');
        return { action: 'approve', by, react: true, canFix };
      }
      if ((command === '/resolve-all' || command === '/unresolve-all') && human) return { action: 'resolve', by, canFix };
      // The action-review skill's summary comment, posted after it resolved the threads it fixed.
      if ((comment.body ?? '').includes(REFRESH_MARK) && human) return { action: 'refresh', by, canFix };
      if (comment.user?.type === 'Bot' && (comment.body ?? '').includes(SUMMARY_MARK) && trusted.has(actor)) {
        return { action: 'summary-edit', by: actor, canFix };
      }
      return reject(403, 'nothing to do for this comment');
    }
    case 'pull_request_review':
    case 'pull_request_review_comment':
      // Anyone's review or reply (the workflow drops bots'); a refresh only re-reads GitHub, and
      // the verdict it may change ignores resolutions by people without write access.
      return { action: 'refresh', by: actor, canFix };
    default:
      return reject(400, `unsupported event ${run.event}`);
  }
}

// Which GitHub data benchDecision needs for this run, so the dispatcher fetches only that.
function benchNeeds(run) {
  return { above: run.event === 'pull_request', reviews: run.event === 'pull_request_review', comment: run.event === 'issue_comment' };
}

// Whether a bench run may bench this PR. Returns { reject } or { by, react }.
//   above    open PRs whose base is this PR's head branch (pull_request only)
//   reviews  the PR's reviews from the API, never from the request (pull_request_review only)
//   comment  the comment the request points at, from the API (issue_comment only)
function benchDecision({ run, actor, pull, repo, trusted, above = [], reviews = [], comment = null }) {
  if (pull.state !== 'open') return reject(409, 'the PR is not open');
  switch (run.event) {
    case 'pull_request':
      if (pull.head.sha !== run.head_sha) return reject(409, 'the PR moved on; a newer run will follow');
      if (!sameRepo(pull, repo) || !trusted.has(pull.user?.login)) {
        return reject(403, 'not an automatic bench; a trusted user can approve the PR, comment /bench or run the Bench workflow by hand');
      }
      if (isSync(pull)) return reject(409, `\`${pull.head.ref}\` is a mirror sync PR, benched in the private repo; comment /bench to bench it here`);
      if (above.length > 0) return reject(409, `#${above[0].number} is stacked on this PR; the bench runs there`);
      return { by: pull.user.login };
    case 'pull_request_review': {
      if (isSync(pull)) return reject(409, `\`${pull.head.ref}\` is a mirror sync PR, benched in the private repo; comment /bench to bench it here`);
      // The approval must be of the head being benched, so later pushes need another approval.
      const approval = reviews.find((r) => r.state === 'APPROVED' && r.commit_id === pull.head.sha && trusted.has(r.user?.login) && r.user?.type !== 'Bot');
      if (!approval) return reject(403, 'no approval of this head by a trusted user');
      return { by: approval.user.login };
    }
    case 'issue_comment': {
      if (!comment) return reject(400, 'no comment');
      if (!onPr(comment, repo, pull.number)) return reject(400, 'comment is not on this PR');
      if (firstWord(comment) !== '/bench') return reject(403, 'not a /bench comment');
      if (!trusted.has(comment.user?.login) || comment.user?.type === 'Bot') return reject(403, 'not trusted');
      return { by: comment.user.login, react: true };
    }
    case 'workflow_dispatch':
      if (!trusted.has(actor)) return reject(403, 'not trusted');
      return { by: actor };
    default:
      return reject(403, 'not trusted');
  }
}

module.exports = { SYNC_PREFIX, parseRepos, parseRequest, checkRun, reviewDecision, benchNeeds, benchDecision };
