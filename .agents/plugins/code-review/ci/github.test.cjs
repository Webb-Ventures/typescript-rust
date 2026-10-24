// Tests for github.cjs against an in-memory GitHub: node --test .agents/plugins/code-review/ci/*.test.cjs
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');

process.env.TS_R_REVIEW_RETRY_MS = '0';
const review = require('./github.cjs');

const encode = (value) => Buffer.from(JSON.stringify(value)).toString('base64');
const repo = { owner: 'o', repo: 'r' };

const findingBody = (id) => `<!-- ts-r-review:finding ${encode({ id, title: `Finding ${id}`, severity: 'high', category: 'correctness', path: 'a.rs', start_line: 1, end_line: 1 })} -->\n**High** · \`correctness\` · Finding ${id}\n\nWhy ${id} is wrong.\n\n<sub>Confidence 90/100 · m · Resolve this conversation once it is handled, or if you disagree.</sub>`;

// A PR with review threads opened by the bot. `threads`: { id: { resolved, body?, resolvedBy? } }
// (resolvedBy: the person who resolved it; none means the App did). `opts`: reviews (the PR's
// bot reviews so far, oldest first), summaryMeta (posts a summary comment with that meta),
// permissions ({ login: level }, default write), prState, head, refuse (commits GitHub will not
// take a review on, as after a force-push).
function fakeGitHub(threads, opts = {}) {
  let nextComment = 100;
  const state = Object.fromEntries(Object.entries(threads).map(([id, t]) => [id, {
    id, isResolved: t.resolved, resolvedBy: t.resolvedBy ?? null, commentId: nextComment++, body: t.body ?? findingBody(id), commit: t.commit ?? null,
  }]));
  // Comments posted by the code under test open new threads, as on GitHub.
  const addThread = (body) => {
    const id = `new${nextComment}`;
    state[id] = { id, isResolved: false, resolvedBy: null, commentId: nextComment++, body };
  };
  const replies = [];
  const comments = [];
  if (opts.summaryMeta) {
    comments.push({ id: 1, html_url: 'https://github.com/o/r/pull/3#issuecomment-1', user: { login: 'ts-r-review[bot]' }, body: review.renderSummary(opts.summaryMeta, []) });
  }
  const reviews = (opts.reviews ?? []).map((r) => ({ user: { login: 'ts-r-review[bot]' }, ...r }));
  const submitted = [];
  const STATES = { APPROVE: 'APPROVED', REQUEST_CHANGES: 'CHANGES_REQUESTED', COMMENT: 'COMMENTED' };
  const github = {
    graphql: async (query, vars) => {
      if (query.includes('reviewThreads')) {
        const nodes = Object.values(state).map((t) => ({
          id: t.id, isResolved: t.isResolved, resolvedBy: t.isResolved && t.resolvedBy ? { login: t.resolvedBy } : null,
          comments: { nodes: [{ databaseId: t.commentId, url: `u/${t.id}`, body: t.body, author: { login: 'ts-r-review' }, originalCommit: t.commit ? { oid: t.commit } : null }] },
        }));
        return { repository: { pullRequest: { reviewThreads: { pageInfo: { hasNextPage: false }, nodes } } } };
      }
      // The App resolving or reopening: GraphQL reports no resolvedBy for a bot.
      state[vars.id].isResolved = !query.includes('unresolveReviewThread');
      state[vars.id].resolvedBy = null;
      return {};
    },
    paginate: async (method) => {
      if (method === github.rest.pulls.listFiles) return [{ filename: 'a.rs', status: 'modified', patch: '@@ -1,1 +1,1 @@\n-x\n+y' }];
      if (method === github.rest.pulls.listReviews) return reviews;
      return comments;
    },
    rest: {
      issues: {
        listComments: {},
        createComment: async ({ body }) => comments.push({ id: comments.length + 1, user: { login: 'ts-r-review[bot]' }, body }),
        updateComment: async ({ comment_id, body }) => { comments.find((c) => c.id === comment_id).body = body; },
      },
      reactions: { createForIssueComment: async () => {} },
      repos: {
        getCollaboratorPermissionLevel: async ({ username }) => ({ data: { permission: opts.permissions?.[username] ?? 'write' } }),
      },
      pulls: {
        listFiles: {},
        listReviews: {},
        get: async () => ({ data: { state: opts.prState ?? 'open', head: { sha: opts.head ?? 'feedface99' } } }),
        createReview: async ({ event, commit_id, body, comments: inline = [] }) => {
          if (commit_id && opts.refuse?.includes(commit_id)) throw new Error(`commit ${commit_id} is not part of the pull request`);
          for (const c of inline) addThread(c.body);
          reviews.push({ user: { login: 'ts-r-review[bot]' }, state: STATES[event], commit_id: commit_id ?? opts.head ?? 'feedface99', body });
          submitted.push({ event, commit_id, inline: inline.length, body });
        },
        createReviewComment: async ({ body }) => addThread(body),
        createReplyForReviewComment: async ({ comment_id, body }) => replies.push({ thread: Object.values(state).find((t) => t.commentId === comment_id).id, body }),
      },
    },
  };
  return { github, state, replies, submitted, comments };
}
const core = { info() {}, notice() {}, warning() {}, error() {}, setFailed(m) { throw new Error(m); } };

function withEnv(values, fn) {
  const saved = { ...process.env };
  Object.assign(process.env, values);
  return fn().finally(() => { process.env = saved; });
}

// A review run: writes the model's result and the previous findings, then publishes.
async function publish(github, findings, previous = []) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'review-test-'));
  fs.writeFileSync(`${dir}/previous.json`, JSON.stringify(previous));
  fs.writeFileSync(`${dir}/result.json`, JSON.stringify({ summary: 'ok', findings }));
  try {
    await withEnv({
      PR_NUMBER: '3', HEAD_SHA: 'abcdef1234', REVIEW_OUTCOME: 'success', MODEL: 'm', EFFORT: 'high', MIN_CONFIDENCE: '80',
      FINDINGS_PATH: `${dir}/result.json`, PREVIOUS_PATH: `${dir}/previous.json`,
    }, () => review.postReview({ github, context: { repo, payload: {} }, core }));
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
}
const newFinding = { title: 'Off by one', body: 'The loop skips the last byte.', severity: 'low', category: 'correctness', path: 'a.rs', start_line: 1, end_line: 1, confidence: 90 };
const refresh = (github) => review.resolveAll({ github, context: { repo, eventName: 'refresh', payload: { issue: { number: 3 } } }, core });
const bulk = (github, command) => review.resolveAll({
  github, core,
  context: { repo, eventName: 'issue_comment', payload: { action: 'created', comment: { id: 9, body: command, user: { login: 'riley' } }, issue: { number: 3 }, sender: { login: 'riley' } } },
});
const reviewed = { sha: 'abcdef1234', model: 'm (high)' };
const changesRequested = [{ state: 'CHANGES_REQUESTED', commit_id: 'abcdef1234' }];

test('listThreads keeps the comment text, without the markers, for the fix prompt', async () => {
  const { github } = fakeGitHub({ a: { resolved: false, commit: 'abcdef1234' } });
  const [thread] = await review.listThreads(github, 'o', 'r', 3);
  assert.equal(thread.finding.id, 'a');
  assert.equal(thread.commit, 'abcdef1234');
  assert.equal(thread.text, '**High** · `correctness` · Finding a\n\nWhy a is wrong.');
});

test('listThreads leaves an unavailable original commit unknown', async () => {
  const { github } = fakeGitHub({ a: { resolved: false } });
  const [thread] = await review.listThreads(github, 'o', 'r', 3);
  assert.equal(thread.commit, null);
});

test('a review does not auto-resolve a finding that was resolved when it started and reopened since', async () => {
  // a: resolved when the review started, reopened during it, not reported (the prompt says not to
  //    repeat resolved findings), so its silence says nothing about a fix.
  // b: open when the review started and not reported: fixed, so auto-resolved.
  const { github, state, replies } = fakeGitHub({ a: { resolved: false }, b: { resolved: false } });
  await publish(github, [], [{ id: 'a', state: 'resolved' }, { id: 'b', state: 'open' }]);
  assert.equal(state.a.isResolved, false);
  assert.equal(state.b.isResolved, true);
  assert.deepEqual(replies.map((r) => r.thread), ['b']);
});

test('a review with no findings approves the PR', async () => {
  const { github, submitted } = fakeGitHub({});
  await publish(github, []);
  assert.deepEqual(submitted.map((r) => [r.event, r.commit_id, r.inline]), [['APPROVE', 'abcdef1234', 0]]);
});

test('a review whose old findings are all fixed approves the PR', async () => {
  const { github, submitted } = fakeGitHub({ a: { resolved: false } });
  await publish(github, [], [{ id: 'a', state: 'open' }]);
  assert.deepEqual(submitted.map((r) => r.event), ['APPROVE']);
});

test('a review with new findings requests changes in the review that carries them, even when only low', async () => {
  const { github, submitted } = fakeGitHub({});
  await publish(github, [newFinding]);
  assert.deepEqual(submitted.map((r) => [r.event, r.inline]), [['REQUEST_CHANGES', 1]]);
  assert.match(submitted[0].body, /1 open finding \(1 new inline\)/);
});

test('a review that only carries an open finding forward still requests changes', async () => {
  const { github, submitted } = fakeGitHub({ a: { resolved: false } });
  await publish(github, [{ ...newFinding, id: 'a', title: 'Finding a' }], [{ id: 'a', state: 'open' }]);
  assert.deepEqual(submitted.map((r) => [r.event, r.inline]), [['REQUEST_CHANGES', 0]]);
});

test('a review with a finding outside the diff requests changes', async () => {
  const { github, submitted } = fakeGitHub({});
  await publish(github, [{ ...newFinding, path: 'b.rs' }]);
  assert.deepEqual(submitted.map((r) => r.event), ['REQUEST_CHANGES']);
  assert.match(submitted[0].body, /1 finding outside the diff/);
});

test('a later review with new findings requests changes after an approval', async () => {
  const { github, submitted } = fakeGitHub({}, { reviews: [{ state: 'APPROVED', commit_id: 'abcdef1234' }] });
  await publish(github, [newFinding]);
  assert.deepEqual(submitted.map((r) => r.event), ['REQUEST_CHANGES']);
});

test('a refresh approves once every finding is resolved, and only once', async () => {
  // a: resolved by the App (a fix or /resolve-all); b: resolved by a maintainer, as the
  // action-review skill does.
  const { github, submitted, comments } = fakeGitHub(
    { a: { resolved: true }, b: { resolved: true, resolvedBy: 'riley' } },
    { summaryMeta: reviewed, reviews: changesRequested },
  );
  await refresh(github);
  assert.deepEqual(submitted.map((r) => [r.event, r.commit_id]), [['APPROVE', 'feedface99']]);
  await refresh(github);
  await refresh(github);
  assert.equal(submitted.length, 1);
  assert.doesNotMatch(comments[0].body, /Not approved/);
});

test('an approval at an older head is not repeated for a new head', async () => {
  const { github, submitted } = fakeGitHub(
    { a: { resolved: true } },
    { summaryMeta: reviewed, reviews: [{ state: 'APPROVED', commit_id: 'abcdef1234' }], head: 'beadbead00' },
  );
  await refresh(github);
  assert.deepEqual(submitted, []);
});

test('a refresh with a finding still open neither approves nor requests changes again', async () => {
  const { github, submitted } = fakeGitHub(
    { a: { resolved: true }, b: { resolved: false } },
    { summaryMeta: reviewed, reviews: changesRequested },
  );
  await refresh(github);
  assert.deepEqual(submitted, []);
});

test('a finding resolved by someone without write access holds the approval back', async () => {
  const { github, submitted, comments } = fakeGitHub(
    { a: { resolved: true, resolvedBy: 'outsider' } },
    { summaryMeta: reviewed, reviews: changesRequested, permissions: { outsider: 'read' } },
  );
  await refresh(github);
  assert.deepEqual(submitted, []);
  assert.match(comments[0].body, /Not approved: 1 finding was resolved by someone without write access/);
});

test('/resolve-all approves, and /unresolve-all goes back to requesting changes', async () => {
  const { github, submitted } = fakeGitHub(
    { a: { resolved: false }, b: { resolved: false } },
    { summaryMeta: reviewed, reviews: changesRequested },
  );
  await bulk(github, '/resolve-all');
  assert.deepEqual(submitted.map((r) => r.event), ['APPROVE']);
  await bulk(github, '/unresolve-all');
  assert.deepEqual(submitted.map((r) => r.event), ['APPROVE', 'REQUEST_CHANGES']);
  assert.match(submitted[1].body, /2 open findings at feedfac/);
  // Reopened and still open: no further verdicts.
  await refresh(github);
  assert.equal(submitted.length, 2);
});

const approve = (github, login = 'riley') => review.approveOnRequest({
  github, core,
  context: { repo, eventName: 'issue_comment', payload: { action: 'created', comment: { id: 9, body: '/approve', user: { login } }, issue: { number: 3 }, sender: { login } } },
});

test('/approve approves at the head with findings open, and refreshes do not take it back', async () => {
  const { github, submitted, comments, state } = fakeGitHub(
    { a: { resolved: false }, b: { resolved: true } },
    { summaryMeta: reviewed, reviews: changesRequested },
  );
  await approve(github);
  assert.deepEqual(submitted.map((r) => [r.event, r.commit_id]), [['APPROVE', 'feedface99']]);
  assert.match(submitted[0].body, /for @riley with `\/approve`, with 1 finding still open/);
  assert.match(comments[0].body, /Approved at `feedfac` by @riley with `\/approve`/);
  assert.equal(state.a.isResolved, false);
  await refresh(github);
  assert.equal(submitted.length, 1);
});

test('/unresolve-all takes back an /approve', async () => {
  const { github, submitted, comments } = fakeGitHub(
    { a: { resolved: false } },
    { summaryMeta: reviewed, reviews: changesRequested },
  );
  await approve(github);
  await bulk(github, '/unresolve-all');
  assert.deepEqual(submitted.map((r) => r.event), ['APPROVE', 'REQUEST_CHANGES']);
  assert.doesNotMatch(comments[0].body, /with `\/approve`/);
});

test('a later review with findings requests changes over an /approve', async () => {
  const { github, submitted } = fakeGitHub({}, { summaryMeta: reviewed, reviews: changesRequested });
  await approve(github);
  await publish(github, [newFinding]);
  assert.deepEqual(submitted.map((r) => r.event), ['APPROVE', 'REQUEST_CHANGES']);
});

test('/approve needs write access and an open PR', async () => {
  const readOnly = fakeGitHub({ a: { resolved: false } }, { summaryMeta: reviewed, permissions: { outsider: 'read' } });
  await approve(readOnly.github, 'outsider');
  assert.deepEqual(readOnly.submitted, []);
  const closed = fakeGitHub({}, { summaryMeta: reviewed, prState: 'closed' });
  await approve(closed.github);
  assert.deepEqual(closed.submitted, []);
});

test('a fix run that resolves every finding approves at the commit it pushed', async () => {
  const { github, state, submitted } = fakeGitHub({ a: { resolved: false } }, { summaryMeta: reviewed, reviews: changesRequested });
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'review-test-'));
  fs.writeFileSync(`${dir}/fixes.json`, JSON.stringify({ summary: 's', commit_message: 'm', results: [{ id: 'a', status: 'fixed', explanation: 'Fixed.', files: ['a.rs'] }] }));
  fs.writeFileSync(`${dir}/diffs.json`, '{}');
  try {
    await withEnv({ PR_NUMBER: '3', ACTOR: 'riley', FIX_COMMIT: 'c0ffee5678', FIXES_PATH: `${dir}/fixes.json`, FIX_DIFFS_PATH: `${dir}/diffs.json` },
      () => review.applyFixes({ github, context: { repo, payload: {} }, core }));
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
  assert.equal(state.a.isResolved, true);
  assert.deepEqual(submitted.map((r) => [r.event, r.commit_id]), [['APPROVE', 'c0ffee5678']]);
});

// A fix run whose branch moved on: the push step merged the new commits in (FIX_MERGE_PATH).
async function fixAfterMerge(github, results, merge) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'review-test-'));
  fs.writeFileSync(`${dir}/fixes.json`, JSON.stringify({ summary: 's', commit_message: 'm', results }));
  fs.writeFileSync(`${dir}/diffs.json`, '{}');
  if (merge) fs.writeFileSync(`${dir}/merge.json`, JSON.stringify(merge));
  try {
    await withEnv({
      PR_NUMBER: '3', ACTOR: 'riley', FIX_COMMIT: 'c0ffee5678', FIX_HEAD: 'beef000001', FIXES_PATH: `${dir}/fixes.json`,
      FIX_DIFFS_PATH: `${dir}/diffs.json`, FIX_MERGE_PATH: `${dir}/merge.json`,
    }, () => review.applyFixes({ github, context: { repo, payload: {} }, core }));
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
}
const fixedAll = (...ids) => ids.map((id) => ({ id, status: 'fixed', explanation: `Fixed ${id}.`, files: ['a.rs'] }));

test('after merging a moved branch, threads are resolved and the verdict is given at the merge', async () => {
  const { github, state, submitted, comments } = fakeGitHub({ a: { resolved: false } }, { summaryMeta: reviewed, reviews: changesRequested });
  await fixAfterMerge(github, fixedAll('a'), { branch: 'feat', head: 'beef000001', incoming: 'dad0000001', count: 1, stale: [] });
  assert.equal(state.a.isResolved, true);
  assert.deepEqual(submitted.map((r) => [r.event, r.commit_id]), [['APPROVE', 'beef000001']]);
  assert.match(comments[0].body, /Codex fixed 1 finding in \[`c0ffee5`\].*; `feat` had moved on meanwhile, so its 1 new commit was merged into the fixes \(\[`beef000`\]/);
});

test('a fixed thread whose lines the merged commits changed gets a reply and stays open', async () => {
  const { github, state, replies, submitted, comments } = fakeGitHub({ a: { resolved: false }, b: { resolved: false } }, { summaryMeta: reviewed, reviews: changesRequested });
  await fixAfterMerge(github, fixedAll('a', 'b'), { branch: 'feat', head: 'beef000001', incoming: 'dad0000001', count: 2, stale: ['b'] });
  assert.equal(state.a.isResolved, true);
  assert.equal(state.b.isResolved, false);
  const onB = replies.filter((r) => r.thread === 'b');
  assert.equal(onB.length, 1);
  assert.match(onB[0].body, /Fixed in \[`c0ffee5`\].*but `feat` changed these lines while Codex worked .*so this stays open/);
  assert.match(comments[0].body, /its 2 new commits were merged.*; 1 fixed but left open because the branch changed its lines/);
  assert.deepEqual(submitted, []);
});

test('an unreadable merge record leaves every fixed thread open', async () => {
  const { github, state } = fakeGitHub({ a: { resolved: false } }, { summaryMeta: reviewed, reviews: changesRequested });
  await fixAfterMerge(github, fixedAll('a'), null);
  assert.equal(state.a.isResolved, false);
});

test('a conflict with the moved branch is reported and nothing is resolved', async () => {
  const { github, state, comments } = fakeGitHub({ a: { resolved: false } }, { summaryMeta: reviewed });
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'review-test-'));
  fs.writeFileSync(`${dir}/fixes.json`, JSON.stringify({ summary: 's', commit_message: 'm', results: fixedAll('a') }));
  const failing = { ...core, setFailed() {} };
  try {
    await withEnv({ PR_NUMBER: '3', ACTOR: 'riley', FIXES_PATH: `${dir}/fixes.json`, FIX_ERROR: '`feat` moved on while Codex worked (1 new commit, up to `dad0000`), and merging it into the fixes conflicts in `a.rs`; tick the box again to fix the new head.' },
      () => review.applyFixes({ github, context: { repo, payload: {} }, core: failing }));
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
  assert.equal(state.a.isResolved, false);
  assert.match(comments[0].body, /could not be pushed: `feat` moved on .*conflicts in `a\.rs`.* Nothing was resolved\./);
});

test('a dismissed verdict, a closed PR or a PR without a completed review is left alone', async () => {
  for (const opts of [
    { summaryMeta: reviewed, reviews: [{ state: 'DISMISSED', commit_id: 'abcdef1234' }] },
    { summaryMeta: reviewed, prState: 'closed' },
    { summaryMeta: { status: 'Review queued.' } },
  ]) {
    const { github, submitted } = fakeGitHub({ a: { resolved: true } }, opts);
    await refresh(github);
    assert.deepEqual(submitted, [], JSON.stringify(opts));
  }
});

test('an approval GitHub refuses at the reviewed commit is not moved to the head', async () => {
  const { github, submitted } = fakeGitHub({}, { head: 'b0b0b0b0b0', refuse: ['abcdef1234'] });
  await publish(github, []);
  assert.deepEqual(submitted, []);
});

test('changes requested on a refused commit fall back to the head', async () => {
  const { github, submitted } = fakeGitHub({}, { head: 'b0b0b0b0b0', refuse: ['abcdef1234'] });
  await publish(github, [{ ...newFinding, path: 'elsewhere.rs' }]);
  assert.deepEqual(submitted.map((r) => [r.event, r.commit_id]), [['REQUEST_CHANGES', undefined]]);
});
