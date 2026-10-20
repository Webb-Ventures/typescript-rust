// Tests for bench.cjs against an in-memory GitHub: node --test .agents/plugins/code-review/ci/*.test.cjs
'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');
const bench = require('./bench.cjs');

function fakeGitHub() {
  const comments = [];
  return {
    comments,
    paginate: async () => comments,
    rest: {
      issues: {
        listComments: null,
        createComment: async ({ body }) => comments.push({ id: comments.length + 1, body, user: { login: 'ts-r-review[bot]' } }),
        updateComment: async ({ comment_id: id, body }) => {
          comments.find((c) => c.id === id).body = body;
        },
      },
    },
  };
}
const at = { owner: 'o', repo: 'r', number: 7 };
const result = { sha: 'a'.repeat(40), base: 'b'.repeat(40), baseRef: 'm1/x', duration: '31 min', code: 0, log: 'logs/7-bench.log', tables: '| scenario | change |\n|---|---|\n| check | -2% |' };

test('progress, then results, in one comment', async () => {
  const github = fakeGitHub();
  await bench.setBenchStatus({ github, ...at, status: 'Bench of `aaaaaaa` queued.', busy: true });
  assert.equal(github.comments.length, 1);
  assert.match(github.comments[0].body, /octocat-spinner.*Bench of `aaaaaaa` queued\./);
  assert.match(github.comments[0].body, /^<!-- ts-r-bench -->/);

  await bench.setBenchStatus({ github, ...at, status: 'Benchmarked `aaaaaaa`.', result });
  assert.equal(github.comments.length, 1);
  const body = github.comments[0].body;
  assert.doesNotMatch(body, /spinner/);
  assert.match(body, /\*\*Passed\.\*\*/);
  assert.match(body, /\| check \| -2% \|/);
  assert.match(body, /`aaaaaaa` against m1\/x @ `bbbbbbb` in 31 min/);
});

test('a new run keeps the last results under its progress line', async () => {
  const github = fakeGitHub();
  await bench.setBenchStatus({ github, ...at, status: 'done', result: { ...result, code: 1 } });
  await bench.setBenchStatus({ github, ...at, status: 'Bench of `ccccccc` running since 10:00 UTC: building.', busy: true });
  const body = github.comments[0].body;
  assert.match(body, /spinner.*running since/);
  assert.match(body, /\*\*Regression\.\*\*/);
  assert.match(body, /\| check \| -2% \|/);
});

test('errors point at the log, and huge tables are cut', () => {
  const body = bench.renderBench({ ...result, code: 2 }, 'x'.repeat(70_000));
  assert.match(body, /\*\*Error\.\*\*.*exit 2.*logs\/7-bench\.log/);
  assert.ok(body.length < 65_536);
  assert.match(body, /truncated/);
});
