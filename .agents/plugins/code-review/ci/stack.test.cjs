// Tests for stack.cjs against an in-memory GitHub and git: node --test .agents/plugins/code-review/ci/*.test.cjs
'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');

const { mergeUpStack, MAX_PRS } = require('./stack.cjs');

const owner = 'o';
const repo = 'r';
const core = { info: () => {} };

// PRs: [{ number, base, head, fork? }]. Branch tips: { branch: sha }. Commits form a DAG in
// `parents`; `opts.conflict` lists branches whose merge conflicts, `opts.workflows` branches
// whose merge touches workflows, `opts.moved` branches whose push is refused.
function fake(prs, tips, opts = {}) {
  const parents = { ...opts.parents };
  const comments = [];
  const pushes = [];
  let next = 0;
  const github = {
    rest: {
      pulls: {
        list: async ({ base }) => ({
          data: prs.filter((p) => p.base === base).map((p) => ({
            number: p.number, head: { ref: p.head, repo: { full_name: p.fork ? 'x/r' : 'o/r' } },
          })),
        }),
      },
      issues: { createComment: async ({ issue_number, body }) => comments.push({ pr: issue_number, body }) },
    },
  };
  const ancestors = (c) => {
    const out = new Set();
    const walk = (x) => {
      if (out.has(x)) return;
      out.add(x);
      for (const p of parents[x] ?? []) walk(p);
    };
    walk(c);
    return out;
  };
  const branchOf = (head) => Object.keys(tips).find((b) => tips[b] === head);
  const git = {
    fetch: (_pr, branch) => tips[branch] ?? null,
    contains: (commit, sha) => ancestors(commit).has(sha),
    merge: (ours) => (opts.conflict?.includes(branchOf(ours)) ? { conflicts: ['a.rs'] } : { tree: `tree-of-${ours}` }),
    touches: (commit) => Boolean(opts.workflows?.includes(branchOf(commit))),
    commit: (_tree, ps) => {
      const sha = `m${(next += 1)}`;
      parents[sha] = ps;
      return sha;
    },
    push: (sha, branch) => {
      if (opts.moved?.includes(branch)) return false;
      assert.ok(parents[sha].includes(tips[branch]), 'a push fast-forwards the branch');
      tips[branch] = sha;
      pushes.push({ sha, branch });
      return true;
    },
  };
  return { github, git, comments, pushes, tips, parents };
}

const stack = [
  { number: 2, base: 'a', head: 'b' },
  { number: 3, base: 'b', head: 'c' },
  { number: 4, base: 'c', head: 'd' },
];

test('a fix is merged into every PR above, each merge on the one below', async () => {
  const f = fake(stack, { a: 'A', b: 'B', c: 'C', d: 'D' }, { parents: { FIX: ['A'] } });
  const { merged, stopped } = await mergeUpStack({ github: f.github, owner, repo, pr: 1, branch: 'a', sha: 'FIX', git: f.git, core });
  assert.deepEqual(merged.map((m) => m.pr), [2, 3, 4]);
  assert.deepEqual(stopped, []);
  assert.deepEqual(f.parents[f.tips.b], ['B', 'FIX']);
  assert.deepEqual(f.parents[f.tips.c], ['C', f.tips.b]);
  assert.deepEqual(f.parents[f.tips.d], ['D', f.tips.c]);
  assert.equal(f.comments.length, 1);
  assert.equal(f.comments[0].pr, 1);
  assert.match(f.comments[0].body, /#2 .*merged as/);
});

test('a conflict stops that PR and the ones above it, with a comment on it', async () => {
  const f = fake(stack, { a: 'A', b: 'B', c: 'C', d: 'D' }, { parents: { FIX: ['A'] }, conflict: ['c'] });
  const { merged, stopped } = await mergeUpStack({ github: f.github, owner, repo, pr: 1, branch: 'a', sha: 'FIX', git: f.git, core });
  assert.deepEqual(merged.map((m) => m.pr), [2]);
  assert.deepEqual(stopped.map((s) => s.pr), [3]);
  assert.equal(f.tips.d, 'D');
  const onThree = f.comments.find((c) => c.pr === 3);
  assert.match(onThree.body, /conflict in `a\.rs`/);
  assert.match(onThree.body, /Merge `b` into `c` by hand/);
  assert.match(f.comments.find((c) => c.pr === 1).body, /#3 .*not merged/);
});

test('a branch that already has the fix is skipped, and the PRs above it still get it', async () => {
  const f = fake(stack, { a: 'A', b: 'B', c: 'C', d: 'D' }, { parents: { FIX: ['A'], B: ['FIX'] } });
  const { merged } = await mergeUpStack({ github: f.github, owner, repo, pr: 1, branch: 'a', sha: 'FIX', git: f.git, core });
  assert.deepEqual(merged.map((m) => m.pr), [3, 4]);
  assert.equal(f.tips.b, 'B');
  assert.deepEqual(f.parents[f.tips.c], ['C', 'B']);
});

test('a merge carrying workflow changes, or a refused push, is left for a person', async () => {
  const f = fake(stack, { a: 'A', b: 'B', c: 'C', d: 'D' }, { parents: { FIX: ['A'] }, workflows: ['b'] });
  const r = await mergeUpStack({ github: f.github, owner, repo, pr: 1, branch: 'a', sha: 'FIX', git: f.git, core });
  assert.deepEqual(r.merged, []);
  assert.match(r.stopped[0].why, /workflows/);
  const g = fake(stack, { a: 'A', b: 'B', c: 'C', d: 'D' }, { parents: { FIX: ['A'] }, moved: ['b'] });
  const s = await mergeUpStack({ github: g.github, owner, repo, pr: 1, branch: 'a', sha: 'FIX', git: g.git, core });
  assert.deepEqual(s.merged, []);
  assert.match(s.stopped[0].why, /refused/);
});

test('forks, and a PR with nothing stacked on it, are left alone', async () => {
  const f = fake([{ number: 2, base: 'a', head: 'b', fork: true }], { a: 'A', b: 'B' }, { parents: { FIX: ['A'] } });
  const r = await mergeUpStack({ github: f.github, owner, repo, pr: 1, branch: 'a', sha: 'FIX', git: f.git, core });
  assert.deepEqual(r, { merged: [], stopped: [] });
  assert.deepEqual(f.comments, []);
});

test('a stack taller than the bound stops at the bound', async () => {
  const prs = [];
  const tips = { b0: 'T0' };
  for (let i = 1; i <= MAX_PRS + 2; i += 1) {
    prs.push({ number: i + 1, base: `b${i - 1}`, head: `b${i}` });
    tips[`b${i}`] = `T${i}`;
  }
  const f = fake(prs, tips, { parents: { FIX: ['T0'] } });
  const r = await mergeUpStack({ github: f.github, owner, repo, pr: 1, branch: 'b0', sha: 'FIX', git: f.git, core });
  assert.equal(r.merged.length, MAX_PRS);
  assert.equal(r.stopped.length, 1);
});
