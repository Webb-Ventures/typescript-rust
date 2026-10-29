// Tests for the review box's trust rules: node --test .agents/plugins/code-review/ci/*.test.cjs
'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');

const { REFRESH_MARK } = require('./github.cjs');
const { parseRepos, parseRequest, parseDeployRequest, checkRun, reviewDecision, benchNeeds, benchDecision, deployDecision } = require('./trust.cjs');

const PRIVATE = 'Webb-Ventures/tsc-rust';
const PUBLIC = 'Webb-Ventures/typescript-rust';
const REPOS = [PRIVATE, PUBLIC];
const trusted = new Set(['owner']);
const HEAD = 'a'.repeat(40);

const pull = (over = {}) => ({
  number: 7, state: 'open', draft: false, user: { login: 'owner' },
  head: { sha: HEAD, ref: 'm8/feature', repo: { full_name: PUBLIC } }, base: { ref: 'main' }, ...over,
});
const fork = (over = {}) => pull({ user: { login: 'stranger' }, head: { sha: HEAD, ref: 'patch-1', repo: { full_name: 'stranger/typescript-rust' } }, ...over });
const run = (event, over = {}) => ({ event, head_sha: HEAD, path: '.github/workflows/bench.yml', status: 'in_progress', repository: { full_name: PUBLIC }, created_at: new Date(0).toISOString(), ...over });
const comment = (body, login = 'owner', over = {}) => ({ id: 99, body, user: { login, type: 'User' }, issue_url: `https://api.github.com/repos/${PUBLIC}/issues/7`, ...over });
const review = (sd = {}) => ({ state: 'APPROVED', commit_id: HEAD, user: { login: 'owner', type: 'User' }, ...sd });
const status = (decision) => decision.reject?.[0] ?? 'ok';

test('REPOS: a JSON list, or the single REPO, or the private repo', () => {
  assert.deepEqual(parseRepos(JSON.stringify(REPOS), 'x/y'), REPOS);
  assert.deepEqual(parseRepos(undefined, 'x/y'), ['x/y']);
  assert.deepEqual(parseRepos(undefined, undefined), [PRIVATE]);
  assert.deepEqual(parseRepos(JSON.stringify([PRIVATE, PRIVATE]), undefined), [PRIVATE]);
  for (const bad of ['[]', '"x/y"', '["x"]', '["x/y/z"]', '[1]', 'nope']) assert.throws(() => parseRepos(bad, undefined), bad);
});

test('requests: repo defaults to the first, must be served, numbers must be numbers', () => {
  assert.deepEqual(parseRequest({ run_id: 5, pr: 7, comment_id: null }, REPOS), { runId: 5, pr: 7, repo: PRIVATE, commentId: null });
  assert.deepEqual(parseRequest({ run_id: '5', pr: '7', comment_id: '9', repo: PUBLIC }, REPOS), { runId: 5, pr: 7, repo: PUBLIC, commentId: 9 });
  assert.deepEqual(parseRequest({ run_id: 5, pr: 7, repo: 'evil/fork' }, REPOS).error, [403, 'not a repository this box serves']);
  assert.equal(parseRequest({ run_id: 5, pr: 7, repo: ['x'] }, REPOS).error[0], 403);
  assert.equal(parseRequest({ run_id: 5, pr: 7, repo: PUBLIC.toLowerCase() }, REPOS).error[0], 403);
  assert.equal(parseRequest({ pr: 7 }, REPOS).error[0], 400);
  assert.equal(parseRequest({ run_id: 5, pr: 0 }, REPOS).error[0], 400);
  assert.equal(parseRequest({ run_id: 5, pr: 7, comment_id: 'x' }, REPOS).error[0], 400);
  assert.equal(parseRequest(null, REPOS).error[0], 400);
  assert.equal(parseRequest([], REPOS).error[0], 400);
});

test('runs: our workflows, in the requested repository, still current', () => {
  const workflows = ['.github/workflows/code-review.yml', '.github/workflows/bench.yml'];
  const now = Date.parse(run('x').created_at) + 60_000;
  assert.equal(checkRun(run('pull_request'), { repo: PUBLIC, workflows, now }), null);
  assert.equal(checkRun(run('pull_request'), { repo: PRIVATE, workflows, now })[0], 403);
  assert.equal(checkRun(run('pull_request', { path: '.github/workflows/ci.yml' }), { repo: PUBLIC, workflows, now })[0], 403);
  assert.equal(checkRun(run('pull_request', { status: 'completed' }), { repo: PUBLIC, workflows, now })[0], 403);
  assert.equal(checkRun(run('pull_request'), { repo: PUBLIC, workflows, now: now + 15 * 60_000 })[0], 403);
});

test('automatic review: trusted author and trigger, same repo, not draft, not a sync PR', () => {
  const decide = (p, actor = 'owner') => reviewDecision({ run: run('pull_request'), actor, pull: p, repo: PUBLIC, trusted });
  assert.equal(decide(pull()).action, 'auto-review');
  assert.equal(status(decide(pull({ draft: true }))), 403);
  assert.equal(status(decide(pull(), 'stranger')), 403);
  assert.equal(status(decide(pull({ user: { login: 'stranger' } }))), 403);
  assert.equal(status(decide(fork({ user: { login: 'owner' } }))), 403);
  assert.equal(status(decide(pull({ head: { ...pull().head, sha: 'b'.repeat(40) } }))), 409);
  const sync = decide(pull({ head: { ...pull().head, ref: 'sync/2026-10-20' } }));
  assert.equal(status(sync), 409);
  assert.match(sync.reject[1], /mirror sync PR/);
  // The same PR number in the other repository is not this PR.
  assert.equal(status(reviewDecision({ run: run('pull_request'), actor: 'owner', pull: pull(), repo: PRIVATE, trusted })), 403);
});

test('review on request: trusted commenters, forks included; fixes same-repo only', () => {
  const decide = (c, p = pull(), actor = c.user.login) => reviewDecision({ run: run('issue_comment'), actor, pull: p, comment: c, repo: PUBLIC, trusted });
  assert.deepEqual(decide(comment('@autoreview please'), fork()), { action: 'review', by: 'owner', react: true, canFix: false });
  assert.equal(decide(comment('@AutoReview'), fork()).action, 'review');
  assert.equal(status(decide(comment('@autoreview', 'stranger'), fork())), 403);
  assert.equal(decide(comment('/fix-all')).action, 'fix');
  assert.equal(status(decide(comment('/fix-all'), fork())), 403);
  assert.equal(status(decide(comment('/fix-all'), pull({ state: 'closed' }))), 403);
  assert.equal(decide(comment('/approve'), fork()).action, 'approve');
  assert.equal(status(decide(comment('/approve'), pull({ state: 'closed' }))), 403);
  assert.equal(decide(comment('/resolve-all')).action, 'resolve');
  assert.equal(decide(comment('/unresolve-all')).action, 'resolve');
  assert.equal(decide(comment(`Done.\n${REFRESH_MARK}`)).action, 'refresh');
  assert.equal(status(decide(comment(`Done.\n${REFRESH_MARK}`, 'stranger'))), 403);
  // A comment on another PR, or the same number in the other repository.
  assert.equal(status(decide(comment('@autoreview', 'owner', { issue_url: `https://api.github.com/repos/${PUBLIC}/issues/77` }))), 400);
  assert.equal(status(decide(comment('@autoreview', 'owner', { issue_url: `https://api.github.com/repos/${PRIVATE}/issues/7` }))), 400);
  // The bot's summary, edited by a trusted user (ticking the box).
  const summary = comment('<!-- ts-r-review:summary -->', 'ts-r-review[bot]', { user: { login: 'ts-r-review[bot]', type: 'Bot' } });
  assert.deepEqual(decide(summary, fork(), 'owner'), { action: 'summary-edit', by: 'owner', canFix: false });
  assert.equal(decide(summary, pull(), 'owner').canFix, true);
  assert.equal(status(decide(summary, pull(), 'stranger')), 403);
  assert.equal(status(decide(comment('nice work'))), 403);
});

test('manual review and review refreshes', () => {
  assert.equal(reviewDecision({ run: run('workflow_dispatch'), actor: 'owner', pull: fork(), repo: PUBLIC, trusted }).action, 'review');
  assert.equal(status(reviewDecision({ run: run('workflow_dispatch'), actor: 'stranger', pull: pull(), repo: PUBLIC, trusted })), 403);
  assert.equal(reviewDecision({ run: run('pull_request_review'), actor: 'stranger', pull: fork(), repo: PUBLIC, trusted }).action, 'refresh');
  assert.equal(status(reviewDecision({ run: run('push'), actor: 'owner', pull: pull(), repo: PUBLIC, trusted })), 400);
});

test('which facts a bench decision needs', () => {
  assert.deepEqual(benchNeeds(run('pull_request')), { above: true, reviews: false, comment: false });
  assert.deepEqual(benchNeeds(run('pull_request_review')), { above: false, reviews: true, comment: false });
  assert.deepEqual(benchNeeds(run('issue_comment')), { above: false, reviews: false, comment: true });
});

test('automatic bench: trusted author, same repo, top of the stack, not a sync PR', () => {
  const decide = (p, facts = {}) => benchDecision({ run: run('pull_request'), actor: 'owner', pull: p, repo: PUBLIC, trusted, ...facts });
  assert.deepEqual(decide(pull()), { by: 'owner' });
  assert.equal(status(decide(pull({ user: { login: 'stranger' } }))), 403);
  assert.equal(status(decide(fork())), 403);
  assert.equal(status(decide(fork({ user: { login: 'owner' } }))), 403);
  assert.equal(status(decide(pull({ state: 'closed' }))), 409);
  assert.equal(status(decide(pull({ head: { ...pull().head, sha: 'b'.repeat(40) } }))), 409);
  assert.equal(status(decide(pull(), { above: [{ number: 8 }] })), 409);
  assert.equal(status(decide(pull({ head: { ...pull().head, ref: 'sync/main' } }))), 409);
});

test('bench after approval: a trusted approval of the current head, forks too', () => {
  const decide = (p, reviews) => benchDecision({ run: run('pull_request_review'), actor: 'stranger', pull: p, repo: PUBLIC, trusted, reviews });
  assert.deepEqual(decide(fork(), [review({ state: 'COMMENTED' }), review()]), { by: 'owner' });
  assert.deepEqual(decide(pull({ user: { login: 'stranger' } }), [review()]), { by: 'owner' });
  assert.equal(status(decide(fork(), [])), 403);
  assert.equal(status(decide(fork(), [review({ commit_id: 'b'.repeat(40) })])), 403);
  assert.equal(status(decide(fork(), [review({ user: { login: 'stranger', type: 'User' } })])), 403);
  assert.equal(status(decide(fork(), [review({ state: 'COMMENTED' }), review({ state: 'DISMISSED' })])), 403);
  assert.equal(status(decide(fork(), [review({ user: { login: 'owner', type: 'Bot' } })])), 403);
  assert.equal(status(decide(fork({ state: 'closed' }), [review()])), 409);
  assert.equal(status(decide(pull({ head: { ...pull().head, ref: 'sync/main' } }), [review()])), 409);
});

test('bench on /bench: a trusted commenter on this PR, forks and sync PRs too', () => {
  const decide = (c, p = fork()) => benchDecision({ run: run('issue_comment'), actor: c?.user.login, pull: p, repo: PUBLIC, trusted, comment: c });
  assert.deepEqual(decide(comment('/bench')), { by: 'owner', react: true });
  assert.deepEqual(decide(comment('/BENCH now please')), { by: 'owner', react: true });
  assert.deepEqual(decide(comment('/bench'), pull({ head: { ...pull().head, ref: 'sync/main' } })), { by: 'owner', react: true });
  assert.equal(status(decide(comment('/bench', 'stranger'))), 403);
  assert.equal(status(decide(comment('please /bench'))), 403);
  assert.equal(status(decide(comment('/benchmark'))), 403);
  assert.equal(status(decide(comment('/bench', 'owner', { issue_url: `https://api.github.com/repos/${PUBLIC}/issues/8` }))), 400);
  assert.equal(status(decide(comment('/bench', 'owner', { issue_url: `https://api.github.com/repos/${PRIVATE}/issues/7` }))), 400);
  assert.equal(status(decide(null)), 400);
});

test('manual bench: a trusted user, any PR', () => {
  assert.deepEqual(benchDecision({ run: run('workflow_dispatch'), actor: 'owner', pull: fork(), repo: PUBLIC, trusted }), { by: 'owner' });
  assert.equal(status(benchDecision({ run: run('workflow_dispatch'), actor: 'stranger', pull: pull(), repo: PUBLIC, trusted })), 403);
  assert.equal(status(benchDecision({ run: run('push'), actor: 'owner', pull: pull(), repo: PUBLIC, trusted })), 403);
});

test('deploy requests: a run id and a served repository, no PR', () => {
  assert.deepEqual(parseDeployRequest({ run_id: 5, repo: PUBLIC }, REPOS), { runId: 5, pr: 1, repo: PUBLIC, commentId: null });
  assert.equal(parseDeployRequest({ run_id: 5, repo: 'evil/fork' }, REPOS).error[0], 403);
  assert.equal(parseDeployRequest({ repo: PUBLIC }, REPOS).error[0], 400);
  assert.equal(parseDeployRequest(null, REPOS).error[0], 400);
});

test('deploys: a trusted push or manual run on the deploy repo\'s default branch, at its head', () => {
  const deployRun = (event, over = {}) => run(event, { path: '.github/workflows/deploy-box.yml', head_branch: 'main', ...over });
  const decide = (r, over = {}) => deployDecision({ run: r, actor: 'owner', repo: PUBLIC, deployRepo: PUBLIC, branch: 'main', branchSha: HEAD, trusted, ...over });
  assert.deepEqual(decide(deployRun('push')), { sha: HEAD, by: 'owner' });
  assert.deepEqual(decide(deployRun('workflow_dispatch')), { sha: HEAD, by: 'owner' });
  assert.equal(status(decide(deployRun('push'), { deployRepo: null })), 421);
  assert.equal(status(decide(deployRun('push'), { repo: PRIVATE })), 403);
  assert.equal(status(decide(deployRun('pull_request'))), 403);
  assert.equal(status(decide(deployRun('push', { head_branch: 'm8/feature' }))), 403);
  assert.equal(status(decide(deployRun('workflow_dispatch', { head_branch: 'm8/feature' }))), 403);
  assert.equal(status(decide(deployRun('push'), { actor: 'stranger' })), 403);
  assert.equal(status(decide(deployRun('push'), { branchSha: 'b'.repeat(40) })), 409);
});
