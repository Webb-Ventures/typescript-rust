// Pushes a fix commit to the PR branch when the branch moved on while Codex worked: the bot
// fetches the new head, merges it into the fix (a merge commit with the fix as first parent,
// never a rebase), and pushes that as a plain fast-forward of the new head. A conflict, a branch
// that was rewritten, or a branch that keeps moving falls back to pushing nothing. Then
// staleFindings names the fixed findings whose lines the incoming commits changed, so their
// threads get a reply but stay open.
//
// Git runs on a bare repository of the caller's (bareGit) with hooks off and no checkout: merges
// are `git merge-tree --write-tree`, so nothing in the incoming commits (hooks, scripts,
// attributes) is ever run or written to disk.
'use strict';

const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');

// How many times a fix is merged with the branch's newest head before giving up on a branch that
// keeps moving.
const MERGE_ATTEMPTS = 3;

// Git on a bare repository at `dir`, created on first use, that borrows the objects in `objects`
// (a directory of git objects, e.g. the job's read-only git directory), fetches from and pushes
// to `url`. `auth`: extra `-c` arguments for the commands that reach `url` (the token header);
// they are never logged. `identity`: { name, email } for the commits it makes.
function bareGit({ dir, objects, url, auth = [], identity, env = process.env }) {
  const run = (args, options = {}) => spawnSync('git', ['--git-dir', dir, '--literal-pathspecs', '-c', 'core.hooksPath=/dev/null', ...args], {
    encoding: 'utf8', maxBuffer: 64 << 20, ...options, env: { ...env, GIT_TERMINAL_PROMPT: '0', ...options.env },
  });
  if (!fs.existsSync(path.join(dir, 'HEAD'))) {
    const made = spawnSync('git', ['init', '--quiet', '--bare', dir], { encoding: 'utf8', env: { ...env, GIT_TERMINAL_PROMPT: '0' } });
    if (made.status !== 0) throw new Error(`git init failed (exit ${made.status})`);
    if (objects) fs.writeFileSync(path.join(dir, 'objects/info/alternates'), `${objects}\n`);
  }
  const ok = (args) => run(args).status === 0;
  const out = (args, what) => {
    const result = run(args);
    if (result.status !== 0) throw new Error(`git ${what} failed (exit ${result.status})`);
    return result.stdout;
  };
  const diff = (from, to, file) => out(['diff', '-U0', '--no-color', '--no-ext-diff', '--no-textconv', '--no-renames', from, to, '--', file], 'diff');
  return {
    // The branch's current commit, fetched into `ref`; null if it cannot be fetched.
    fetch(branch, ref) {
      if (run([...auth, 'fetch', '--quiet', '--no-tags', url, `+refs/heads/${branch}:${ref}`]).status !== 0) return null;
      return run(['rev-parse', '--verify', '--quiet', `${ref}^{commit}`]).stdout.trim() || null;
    },
    // Whether `sha` is `commit` or one of its ancestors.
    contains: (commit, sha) => ok(['merge-base', '--is-ancestor', sha, commit]),
    // Commits on `to`'s first-parent line that `from` does not have (a merge of main counts once).
    count: (from, to) => Number(out(['rev-list', '--count', '--first-parent', `${from}..${to}`, '--'], 'rev-list').trim()),
    merge(ours, theirs) {
      const merged = run(['merge-tree', '--write-tree', '--name-only', '--no-messages', ours, theirs]);
      const lines = merged.stdout.split('\n').filter(Boolean);
      if (merged.status === 0) return { tree: lines[0] };
      if (merged.status === 1) return { conflicts: [...new Set(lines.slice(1))] };
      throw new Error(`git merge-tree failed (exit ${merged.status})`);
    },
    touches: (commit, tree, p) => out(['diff-tree', '-r', '--name-only', commit, tree, '--', p], 'diff-tree').trim() !== '',
    commit(tree, parents, message) {
      const id = { GIT_AUTHOR_NAME: identity.name, GIT_AUTHOR_EMAIL: identity.email, GIT_COMMITTER_NAME: identity.name, GIT_COMMITTER_EMAIL: identity.email };
      const made = run(['commit-tree', tree, ...parents.flatMap((p) => ['-p', p])], { input: message, env: id });
      if (made.status !== 0) throw new Error(`git commit-tree failed (exit ${made.status})`);
      return made.stdout.trim();
    },
    // Not forced: the remote takes it only as a fast-forward of the branch. `moved` when it was
    // refused because the branch has commits `sha` does not.
    push(sha, branch) {
      const pushed = run([...auth, 'push', '--porcelain', url, `${sha}:refs/heads/${branch}`]);
      // Only git's own output is logged: the command line carries the token.
      console.log(pushed.stdout, pushed.stderr);
      const said = pushed.stdout + pushed.stderr;
      return { ok: pushed.status === 0, moved: pushed.status !== 0 && /\[rejected\]|non-fast-forward|fetch first/.test(said) };
    },
    // The old-side line ranges `diff from to` changes in `file` (hunks).
    hunks: (from, to, file) => parseHunks(diff(from, to, file)),
    // Translate an unchanged finding from its review commit to the head Codex fixed.
    mapLines(from, to, file, start, end) {
      if (!from || !ok(['merge-base', '--is-ancestor', from, to]) ||
          !ok(['cat-file', '-e', `${from}:${file}`]) || !ok(['cat-file', '-e', `${to}:${file}`])) return null;
      return mapLines(diff(from, to, file), start, end);
    },
  };
}

// The old-side line ranges, [first, last] inclusive, of a -U0 diff of one file. A pure insertion
// after line n touches lines n and n + 1. A change with no hunks (a binary file) is the whole file.
function parseHunks(diff) {
  if (!diff.trim()) return [];
  const ranges = [];
  for (const m of diff.matchAll(/^@@ -(\d+)(?:,(\d+))? \+\d+(?:,\d+)? @@/gm)) {
    const start = Number(m[1]);
    const count = m[2] === undefined ? 1 : Number(m[2]);
    ranges.push(count === 0 ? [start, start + 1] : [start, start + count - 1]);
  }
  return ranges.length > 0 ? ranges : [[0, Infinity]];
}

// Only unchanged ranges can be mapped with confidence. Edits within a finding, binary diffs
// and metadata-only changes leave its thread open rather than guessing at its new location.
function mapLines(diff, start, end) {
  if (!Number.isInteger(start) || !Number.isInteger(end) || start < 1 || end < start) return null;
  if (!diff.trim()) return [start, end];
  let offset = 0;
  let hunks = 0;
  for (const m of diff.matchAll(/^@@ -(\d+)(?:,(\d+))? \+\d+(?:,(\d+))? @@/gm)) {
    hunks += 1;
    const first = Number(m[1]);
    const removed = m[2] === undefined ? 1 : Number(m[2]);
    const added = m[3] === undefined ? 1 : Number(m[3]);
    if (removed === 0) {
      if (first >= start && first < end) return null;
      if (first < start) offset += added;
    } else {
      if (first <= end && first + removed > start) return null;
      if (first + removed <= start) offset += added - removed;
    }
  }
  return hunks > 0 ? [start + offset, end + offset] : null;
}

const overlaps = (as, bs) => as.some(([a1, a2]) => bs.some(([b1, b2]) => a1 <= b2 && b1 <= a2));

// Pushes `fix` (a commit on `base`, the head Codex fixed) to `branch`. If the branch moved on,
// merges its new head into the fix and pushes the merge, retrying with the newest head up to
// `attempts` times. `message(head, count)` is the merge commit's message. Returns { status, ... }:
//   pushed       { head }                      the fix itself went in (or the branch already has it)
//   merged       { head, incoming, count }     the merge commit `head` of `incoming` went in
//   conflict     { incoming, count, files }    nothing pushed
//   rewritten    { incoming }                  the new head does not contain `base` (force-pushed)
//   workflows    { incoming, count }           the merge would change .github/workflows
//   moved        { attempts }                  the branch moved again after every merge
//   unfetchable, refused                       the new head could not be fetched; another refusal
function pushFix({ git, branch, fix, base, message, attempts = MERGE_ATTEMPTS }) {
  let pushed = git.push(fix, branch);
  if (pushed.ok) return { status: 'pushed', head: fix };
  if (!pushed.moved) return { status: 'refused' };
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    const incoming = git.fetch(branch, 'refs/catchup/head');
    if (!incoming) return { status: 'unfetchable' };
    if (git.contains(incoming, fix)) return { status: 'pushed', head: incoming };
    if (!git.contains(incoming, base)) return { status: 'rewritten', incoming };
    const count = git.count(fix, incoming);
    const result = git.merge(fix, incoming);
    if (!result.tree) return { status: 'conflict', incoming, count, files: result.conflicts };
    if (git.touches(incoming, result.tree, '.github/workflows')) return { status: 'workflows', incoming, count };
    const merge = git.commit(result.tree, [fix, incoming], message(incoming, count));
    pushed = git.push(merge, branch);
    if (pushed.ok) return { status: 'merged', head: merge, incoming, count, attempts: attempt };
    if (!pushed.moved) return { status: 'refused' };
  }
  return { status: 'moved', attempts };
}

// The ids of fixed findings whose lines the incoming commits (`base`..`incoming`) changed: in the
// finding's own lines, or where the fix changed its files (`base`..`fix`). Line numbers are the
// base's; finding locations are first mapped from their original review `commit` to `base`.
// `findings`: [{ id, path, start_line, end_line, commit, files }]. The fix is one commit, so its
// changes are known per file, not per finding: findings fixed in the same file share them, which
// errs towards leaving a thread open.
function staleFindings({ git, base, fix, incoming, findings }) {
  const changed = new Map();
  const incomingIn = (file) => {
    if (!changed.has(file)) changed.set(file, git.hunks(base, incoming, file));
    return changed.get(file);
  };
  const stale = [];
  for (const f of findings) {
    const regions = new Map();
    const add = (file, ranges) => regions.set(file, [...(regions.get(file) ?? []), ...ranges]);
    const mapped = typeof f.path === 'string' && f.path
      ? git.mapLines(f.commit, base, f.path, f.start_line, f.end_line) : null;
    if (!mapped) {
      stale.push(f.id);
      continue;
    }
    add(f.path, [mapped]);
    for (const file of f.files ?? []) add(file, git.hunks(base, fix, file));
    if ([...regions].some(([file, ranges]) => overlaps(ranges, incomingIn(file)))) stale.push(f.id);
  }
  return stale;
}

module.exports = { bareGit, pushFix, staleFindings, parseHunks, mapLines, MERGE_ATTEMPTS };
