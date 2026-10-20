// Merges a fix the review bot pushed to one PR's branch up the stack, as a person does by hand:
// every open PR from this repository based on that branch gets a merge commit of it, pushed as a
// fast-forward of its branch, and so on up. A conflict, a branch that moved meanwhile, or a merge
// that would carry workflow changes (the App cannot push those) stops that part of the stack,
// with a comment on the PR saying what to merge by hand.
'use strict';

// A bound on the PRs one fix walks through (a stack is a chain; this also bounds a tree of them).
const MAX_PRS = 30;

// `git` does the git work on commits the caller has:
//   fetch(pr, branch)       the branch's current commit, fetched; null if it cannot be fetched
//   contains(commit, sha)   whether `sha` is `commit` or one of its ancestors
//   merge(ours, theirs)     { tree } for a clean merge, { conflicts: [path] } otherwise
//   touches(commit, tree, path)  whether `tree` differs from `commit` under `path`
//   commit(tree, parents, message)  the new commit's sha
//   push(sha, branch)       true when the branch fast-forwarded to `sha`
// Returns { merged: [{ pr, branch, sha }], stopped: [{ pr, branch, why }] }.
async function mergeUpStack({ github, owner, repo, pr, branch, sha, git, core }) {
  const merged = [];
  const stopped = [];
  const seen = new Set([pr]);
  const queue = [{ pr, branch, sha }];
  while (queue.length > 0) {
    const below = queue.shift();
    const { data: above } = await github.rest.pulls.list({ owner, repo, state: 'open', base: below.branch, per_page: 100 });
    for (const up of above.sort((a, b) => a.number - b.number)) {
      if (up.head.repo?.full_name !== `${owner}/${repo}` || seen.has(up.number)) continue;
      seen.add(up.number);
      const target = up.head.ref;
      const stop = async (why) => {
        stopped.push({ pr: up.number, branch: target, why });
        await github.rest.issues.createComment({
          owner, repo, issue_number: up.number,
          body: `The review bot pushed fixes to \`${below.branch}\` (from #${pr}) but did not merge them into \`${target}\`: ${why}. Merge \`${below.branch}\` into \`${target}\` by hand and push, so the stack above it gets them too.`,
        });
      };
      if (seen.size - 1 > MAX_PRS) {
        await stop(`the stack has more than ${MAX_PRS} PRs above #${pr}`);
        continue;
      }
      const head = git.fetch(up.number, target);
      if (!head) {
        await stop('its branch could not be fetched');
        continue;
      }
      // Already merged (by hand, or by an earlier run): carry on above it.
      if (git.contains(head, sha)) {
        queue.push({ pr: up.number, branch: target, sha: head });
        continue;
      }
      const result = git.merge(head, below.sha);
      if (!result.tree) {
        const files = result.conflicts.slice(0, 10).map((f) => `\`${f}\``).join(', ');
        await stop(`they conflict${files ? ` in ${files}` : ''}${result.conflicts.length > 10 ? ' and more' : ''}`);
        continue;
      }
      if (git.touches(head, result.tree, '.github/workflows')) {
        await stop('the merge changes `.github/workflows`, which the review bot may not push');
        continue;
      }
      const message = `Merge ${below.branch} into ${target}\n\nThe review bot's fixes on #${pr}, merged up the stack.\n`;
      const commit = git.commit(result.tree, [head, below.sha], message);
      if (!git.push(commit, target)) {
        await stop(`GitHub refused the push (\`${target}\` may have moved on)`);
        continue;
      }
      core.info(`Merged ${below.branch} into ${target} (#${up.number}) as ${commit}.`);
      merged.push({ pr: up.number, branch: target, sha: commit });
      queue.push({ pr: up.number, branch: target, sha: commit });
    }
  }
  if (merged.length > 0 || stopped.length > 0) {
    const lines = [
      ...merged.map((m) => `- #${m.pr} (\`${m.branch}\`): merged as \`${m.sha.slice(0, 7)}\``),
      ...stopped.map((s) => `- #${s.pr} (\`${s.branch}\`): not merged, ${s.why}`),
    ];
    await github.rest.issues.createComment({
      owner, repo, issue_number: pr,
      body: `The review bot merged its fixes up the stack:\n\n${lines.join('\n')}`,
    });
  }
  return { merged, stopped };
}

module.exports = { mergeUpStack, MAX_PRS };
