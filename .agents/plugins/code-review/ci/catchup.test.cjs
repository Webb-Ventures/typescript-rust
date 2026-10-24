// Tests for catchup.cjs with real git: a bare "GitHub" repository in a temp dir, a clone where
// people push, and the box's side (the job's git directory and the push step's bare repository).
// node --test .agents/plugins/code-review/ci/*.test.cjs
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
const { test } = require('node:test');

const { bareGit, pushFix, staleFindings, parseHunks, mapLines, MERGE_ATTEMPTS } = require('./catchup.cjs');

const BRANCH = 'feature';
const lines = (n, tag = '') => Array.from({ length: n }, (_, i) => `line ${i + 1}${tag}`).join('\n') + '\n';
// git with no user or system config, so the tests do not depend on the machine's.
const gitEnv = {
  ...process.env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null', GIT_TERMINAL_PROMPT: '0',
  GIT_AUTHOR_NAME: 'Person', GIT_AUTHOR_EMAIL: 'person@example.com', GIT_COMMITTER_NAME: 'Person', GIT_COMMITTER_EMAIL: 'person@example.com',
};
const sh = (cwd, ...args) => execFileSync('git', ['-c', 'init.defaultBranch=main', ...args], { cwd, env: gitEnv, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();

// remote: the bare repository standing in for GitHub. person: a clone that pushes to the PR
// branch. box: the job's clone, where Codex's fix is committed on the head it fetched.
function setup(files) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'catchup-test-'));
  const remote = path.join(root, 'remote.git');
  sh(root, 'init', '--quiet', '--bare', remote);
  const person = path.join(root, 'person');
  sh(root, 'clone', '--quiet', remote, person);
  write(person, files);
  sh(person, 'checkout', '--quiet', '-b', BRANCH);
  sh(person, 'add', '--all');
  sh(person, 'commit', '--quiet', '-m', 'base');
  sh(person, 'push', '--quiet', 'origin', BRANCH);
  const base = sh(person, 'rev-parse', 'HEAD');
  const box = path.join(root, 'box');
  sh(root, 'clone', '--quiet', '--branch', BRANCH, remote, box);
  const git = bareGit({ dir: path.join(root, 'push.git'), objects: path.join(box, '.git/objects'), url: remote, identity: { name: 'ts-r-review[bot]', email: '1+ts-r-review[bot]@users.noreply.github.com' }, env: gitEnv });
  return { root, remote, person, box, base, git, cleanup: () => fs.rmSync(root, { recursive: true, force: true }) };
}
function write(dir, files) {
  for (const [name, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
    fs.writeFileSync(path.join(dir, name), text);
  }
}
function commitIn(dir, files, message, push = false) {
  write(dir, files);
  sh(dir, 'add', '--all');
  sh(dir, 'commit', '--quiet', '-m', message);
  if (push) sh(dir, 'push', '--quiet', 'origin', BRANCH);
  return sh(dir, 'rev-parse', 'HEAD');
}
const remoteHead = (r) => sh(r.remote, 'rev-parse', `refs/heads/${BRANCH}`);
const message = (head, count) => `Merge ${BRANCH} head ${head.slice(0, 7)} into the review fixes\n\n${count} new commits.\n`;
// Replaces line `n` (1-based) of `text`.
const edit = (text, n, value) => text.split('\n').map((l, i) => (i === n - 1 ? value : l)).join('\n');

test('a branch that has not moved gets the fix itself', () => {
  const r = setup({ 'a.txt': lines(20) });
  try {
    const fix = commitIn(r.box, { 'a.txt': edit(lines(20), 3, 'fixed') }, 'fix');
    const out = pushFix({ git: r.git, branch: BRANCH, fix, base: r.base, message });
    assert.deepEqual(out, { status: 'pushed', head: fix });
    assert.equal(remoteHead(r), fix);
  } finally {
    r.cleanup();
  }
});

test('a branch that moved without a conflict is merged into the fix and pushed', () => {
  const r = setup({ 'a.txt': lines(20), 'b.txt': lines(5) });
  try {
    const fix = commitIn(r.box, { 'a.txt': edit(lines(20), 3, 'fixed') }, 'fix');
    commitIn(r.person, { 'b.txt': lines(5, ' (new)') }, 'someone else', true);
    const incoming = commitIn(r.person, { 'c.txt': 'new file\n' }, 'and again', true);
    const out = pushFix({ git: r.git, branch: BRANCH, fix, base: r.base, message });
    assert.equal(out.status, 'merged');
    assert.equal(out.incoming, incoming);
    assert.equal(out.count, 2);
    assert.equal(remoteHead(r), out.head);
    // A merge commit with the fix first, made as the bot, holding both sides.
    assert.equal(sh(r.remote, 'rev-list', '--parents', '-n', '1', out.head), `${out.head} ${fix} ${incoming}`);
    assert.equal(sh(r.remote, 'log', '-1', '--format=%an <%ae>|%cn <%ce>', out.head),
      'ts-r-review[bot] <1+ts-r-review[bot]@users.noreply.github.com>|ts-r-review[bot] <1+ts-r-review[bot]@users.noreply.github.com>');
    assert.match(sh(r.remote, 'log', '-1', '--format=%B', out.head), new RegExp(`^Merge ${BRANCH} head ${incoming.slice(0, 7)} into the review fixes`));
    assert.equal(sh(r.remote, 'show', `${out.head}:a.txt`).split('\n')[2], 'fixed');
    assert.equal(sh(r.remote, 'show', `${out.head}:c.txt`), 'new file');
    // Neither side's history was rewritten.
    sh(r.remote, 'merge-base', '--is-ancestor', incoming, out.head);
    // An unrelated finding stays resolvable.
    const stale = staleFindings({ git: r.git, base: r.base, fix, incoming, findings: [{ id: 'f', path: 'a.txt', start_line: 3, end_line: 3, commit: r.base, files: ['a.txt'] }] });
    assert.deepEqual(stale, []);
  } finally {
    r.cleanup();
  }
});

test('a merge of main into the branch counts as one new commit', () => {
  const r = setup({ 'a.txt': lines(20) });
  try {
    const fix = commitIn(r.box, { 'a.txt': edit(lines(20), 3, 'fixed') }, 'fix');
    sh(r.person, 'checkout', '--quiet', '-b', 'main', r.base);
    commitIn(r.person, { 'm1.txt': '1\n' }, 'main 1');
    commitIn(r.person, { 'm2.txt': '2\n' }, 'main 2');
    sh(r.person, 'checkout', '--quiet', BRANCH);
    sh(r.person, 'merge', '--quiet', '--no-ff', '-m', 'Merge main', 'main');
    sh(r.person, 'push', '--quiet', 'origin', BRANCH);
    const out = pushFix({ git: r.git, branch: BRANCH, fix, base: r.base, message });
    assert.equal(out.status, 'merged');
    assert.equal(out.count, 1);
  } finally {
    r.cleanup();
  }
});

test('a conflict pushes nothing and names the files', () => {
  const r = setup({ 'a.txt': lines(20), 'b.txt': lines(20) });
  try {
    const fix = commitIn(r.box, { 'a.txt': edit(lines(20), 3, 'fixed'), 'b.txt': edit(lines(20), 9, 'fixed') }, 'fix');
    const incoming = commitIn(r.person, { 'a.txt': edit(lines(20), 3, 'theirs'), 'b.txt': edit(lines(20), 9, 'theirs') }, 'clash', true);
    const out = pushFix({ git: r.git, branch: BRANCH, fix, base: r.base, message });
    assert.deepEqual(out, { status: 'conflict', incoming, count: 1, files: ['a.txt', 'b.txt'] });
    assert.equal(remoteHead(r), incoming);
  } finally {
    r.cleanup();
  }
});

test('a fixed finding whose lines the new commits changed is stale; the others are not', () => {
  const r = setup({ 'a.txt': lines(40), 'b.txt': lines(10), 'helper.txt': lines(10) });
  try {
    // f1's finding is at a.txt:20-21 and was fixed in helper.txt; f2 at b.txt:5, fixed there;
    // f3 at a.txt:35, fixed there.
    const fix = commitIn(r.box, { 'helper.txt': edit(lines(10), 4, 'guarded'), 'b.txt': edit(lines(10), 5, 'fixed f2'), 'a.txt': edit(lines(40), 35, 'fixed f3') }, 'fix');
    // The new commit rewrites f1's own line 21 (not a line the fix changed: no textual conflict)
    // and makes f3's change too (the same on both sides: no conflict either).
    const incoming = commitIn(r.person, { 'a.txt': edit(edit(lines(40), 21, 'rewritten'), 35, 'fixed f3') }, 'moved', true);
    const out = pushFix({ git: r.git, branch: BRANCH, fix, base: r.base, message });
    assert.equal(out.status, 'merged');
    const findings = [
      { id: 'f1', path: 'a.txt', start_line: 20, end_line: 21, commit: r.base, files: ['helper.txt'] },
      { id: 'f2', path: 'b.txt', start_line: 5, end_line: 5, commit: r.base, files: ['b.txt'] },
      { id: 'f3', path: 'a.txt', start_line: 35, end_line: 35, commit: r.base, files: ['a.txt'] },
    ];
    assert.deepEqual(staleFindings({ git: r.git, base: r.base, fix, incoming, findings }), ['f1', 'f3']);
  } finally {
    r.cleanup();
  }
});

test('finding locations are mapped from the review commit to the fix base before checking incoming edits', () => {
  const r = setup({ 'a.txt': lines(40), 'helper.txt': lines(10) });
  try {
    const shifted = lines(10, ' (inserted)') + lines(40);
    const base = commitIn(r.box, { 'a.txt': shifted }, 'insert before the findings', true);
    sh(r.person, 'pull', '--quiet', '--ff-only', 'origin', BRANCH);
    const fix = commitIn(r.box, { 'helper.txt': edit(lines(10), 4, 'guarded') }, 'fix through helper');
    const incoming = commitIn(r.person, { 'a.txt': edit(shifted, 30, 'rewritten') }, 'change the shifted finding', true);
    const out = pushFix({ git: r.git, branch: BRANCH, fix, base, message });
    assert.equal(out.status, 'merged');
    const findings = [
      { id: 'shifted', path: 'a.txt', start_line: 20, end_line: 20, commit: r.base, files: ['helper.txt'] },
      { id: 'untouched', path: 'a.txt', start_line: 25, end_line: 26, commit: r.base, files: ['helper.txt'] },
    ];
    assert.deepEqual(staleFindings({ git: r.git, base, fix, incoming, findings }), ['shifted']);
  } finally {
    r.cleanup();
  }
});

test('uncertain finding locations stay open even when the incoming changes are elsewhere', () => {
  const r = setup({ 'a.txt': lines(40), 'helper.txt': lines(10), 'other.txt': lines(10) });
  try {
    const base = commitIn(r.box, { 'a.txt': edit(lines(40), 20, 'changed before fixing') }, 'change the finding', true);
    sh(r.person, 'pull', '--quiet', '--ff-only', 'origin', BRANCH);
    const fix = commitIn(r.box, { 'helper.txt': edit(lines(10), 4, 'guarded') }, 'fix through helper');
    const incoming = commitIn(r.person, { 'other.txt': lines(10, ' (new)') }, 'unrelated change', true);
    const out = pushFix({ git: r.git, branch: BRANCH, fix, base, message });
    assert.equal(out.status, 'merged');
    const finding = { path: 'a.txt', start_line: 20, end_line: 20, files: ['helper.txt'] };
    const findings = [
      { ...finding, id: 'changed', commit: r.base },
      { ...finding, id: 'missing', commit: null },
      { ...finding, id: 'unavailable', commit: 'a'.repeat(40) },
      { ...finding, id: 'not-ancestor', commit: incoming },
      { ...finding, id: 'missing-file', path: 'missing.txt', commit: r.base },
    ];
    assert.deepEqual(staleFindings({ git: r.git, base, fix, incoming, findings }), findings.map((f) => f.id));
  } finally {
    r.cleanup();
  }
});

test('a branch that keeps moving gives up after the bounded retries, pushing nothing of its own', () => {
  const r = setup({ 'a.txt': lines(20) });
  try {
    const fix = commitIn(r.box, { 'a.txt': edit(lines(20), 3, 'fixed') }, 'fix');
    let n = 0;
    // Someone pushes just before each of the bot's pushes.
    const racing = { ...r.git, push: (sha, branch) => {
      n += 1;
      commitIn(r.person, { [`race${n}.txt`]: `${n}\n` }, `race ${n}`, true);
      return r.git.push(sha, branch);
    } };
    const out = pushFix({ git: racing, branch: BRANCH, fix, base: r.base, message });
    assert.deepEqual(out, { status: 'moved', attempts: MERGE_ATTEMPTS });
    assert.equal(n, MERGE_ATTEMPTS + 1);
    assert.equal(sh(r.remote, 'log', '-1', '--format=%s', BRANCH), `race ${n}`);
  } finally {
    r.cleanup();
  }
});

test('a branch rewritten while Codex worked is not merged', () => {
  const r = setup({ 'a.txt': lines(20) });
  try {
    const fix = commitIn(r.box, { 'a.txt': edit(lines(20), 3, 'fixed') }, 'fix');
    sh(r.person, 'commit', '--quiet', '--amend', '-m', 'base, reworded');
    sh(r.person, 'push', '--quiet', '--force', 'origin', BRANCH);
    const rewritten = remoteHead(r);
    const out = pushFix({ git: r.git, branch: BRANCH, fix, base: r.base, message });
    assert.deepEqual(out, { status: 'rewritten', incoming: rewritten });
    assert.equal(remoteHead(r), rewritten);
  } finally {
    r.cleanup();
  }
});

test('nothing in the incoming commits runs: hooks are off and nothing is checked out', () => {
  const r = setup({ 'a.txt': lines(20) });
  try {
    const fix = commitIn(r.box, { 'a.txt': edit(lines(20), 3, 'fixed') }, 'fix');
    const marker = path.join(r.root, 'ran');
    // A hook in the push step's own repository would run on a push without core.hooksPath.
    const hooks = path.join(r.root, 'push.git/hooks');
    fs.mkdirSync(hooks, { recursive: true });
    fs.writeFileSync(path.join(hooks, 'pre-push'), `#!/bin/sh\ntouch '${marker}'\n`, { mode: 0o755 });
    commitIn(r.person, { '.gitattributes': '* filter=evil merge=evil\n', 'hook.sh': `touch '${marker}'\n` }, 'attributes', true);
    const out = pushFix({ git: r.git, branch: BRANCH, fix, base: r.base, message });
    assert.equal(out.status, 'merged');
    assert.equal(fs.existsSync(marker), false);
    assert.deepEqual(fs.readdirSync(path.join(r.root, 'push.git')).filter((f) => f === 'hook.sh' || f === '.gitattributes'), []);
  } finally {
    r.cleanup();
  }
});

test('hunk ranges: changes, deletions, insertions and binary files', () => {
  assert.deepEqual(parseHunks(''), []);
  assert.deepEqual(parseHunks('@@ -3 +3 @@\n-a\n+b\n@@ -10,2 +10,0 @@\n-x\n-y\n@@ -15,0 +14,1 @@\n+z\n'), [[3, 3], [10, 11], [15, 16]]);
  assert.deepEqual(parseHunks('diff --git a/x b/x\nBinary files a/x and b/x differ\n'), [[0, Infinity]]);
});

test('line mapping accounts for insertions and deletions and rejects uncertain ranges', () => {
  assert.deepEqual(mapLines('', 20, 21), [20, 21]);
  assert.deepEqual(mapLines('@@ -19,0 +20,10 @@\n', 20, 21), [30, 31]);
  assert.deepEqual(mapLines('@@ -3,5 +2,0 @@\n', 20, 21), [15, 16]);
  assert.deepEqual(mapLines('@@ -3,2 +3,4 @@\n@@ -10,2 +12,0 @@\n', 20, 21), [20, 21]);
  assert.deepEqual(mapLines('@@ -22 +22,2 @@\n', 20, 21), [20, 21]);
  assert.equal(mapLines('@@ -20 +20 @@\n', 20, 21), null);
  assert.equal(mapLines('@@ -19,2 +18,0 @@\n', 20, 21), null);
  assert.equal(mapLines('@@ -20,0 +21,10 @@\n', 20, 21), null);
  assert.equal(mapLines('Binary files a/x and b/x differ\n', 20, 21), null);
  assert.equal(mapLines('old mode 100644\nnew mode 100755\n', 20, 21), null);
  assert.equal(mapLines('', 0, 1), null);
});
