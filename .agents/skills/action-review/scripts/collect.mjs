#!/usr/bin/env node
// Collects the review material on a pull request as one JSON payload: every
// review summary, every inline thread with its replies, its resolved/outdated
// flags and whether you already answered it, and the top-level comments that
// are not known bot noise.
//
// Threads you have answered and resolved threads are left out, so re-running
// after a second review round only surfaces what is new. --all keeps them.
// Threads the ts-r review bot fixed and resolved itself are listed apart, in
// bot_fixes, until someone replies after the bot's fix: those fixes need checking too.
import { parseArgs } from "node:util";

import { botFixToCheck, fail, ghGraphql, ghJson, ghJsonAll, isAnsweredByYou, isNoise } from "./gh.mjs";

const THREAD_QUERY = `
query($owner:String!,$repo:String!,$number:Int!,$cursor:String){
  repository(owner:$owner,name:$repo){
    pullRequest(number:$number){
      reviewThreads(first:100, after:$cursor){
        pageInfo{hasNextPage endCursor}
        nodes{
          isResolved
          isOutdated
          comments(first:1){nodes{databaseId}}
        }
      }
    }
  }
}`;

// databaseId of each thread's first comment -> its thread-level flags. The REST
// comment payload carries neither, and "already resolved" is half of what makes
// a second run quiet.
function threadState(owner, repo, number) {
  const state = new Map();
  let cursor = null;
  for (;;) {
    const variables = { owner, repo, number };
    if (cursor) variables.cursor = cursor;
    const page = ghGraphql(THREAD_QUERY, variables).repository.pullRequest.reviewThreads;
    for (const node of page.nodes) {
      const first = node.comments.nodes[0];
      if (first) {
        state.set(first.databaseId, { resolved: node.isResolved, outdated: node.isOutdated });
      }
    }
    if (!page.pageInfo.hasNextPage) return state;
    cursor = page.pageInfo.endCursor;
  }
}

const { values, positionals } = parseArgs({
  options: { all: { type: "boolean", default: false } },
  allowPositionals: true,
});

if (positionals.length !== 2) {
  fail("usage: collect.mjs <owner/repo> <pr-number> [--all]");
}

const slug = positionals[0];
const number = Number(positionals[1]);
const [owner, repo] = slug.split("/");
if (!owner || !repo || !Number.isInteger(number)) {
  fail("usage: collect.mjs <owner/repo> <pr-number> [--all]");
}

const you = ghJson("/user").login;
const pr = ghJson(`/repos/${slug}/pulls/${number}`);
const reviews = ghJsonAll(`/repos/${slug}/pulls/${number}/reviews?per_page=100`);
const comments = ghJsonAll(`/repos/${slug}/pulls/${number}/comments?per_page=100`);
const issueComments = ghJsonAll(`/repos/${slug}/issues/${number}/comments?per_page=100`);
const states = threadState(owner, repo, number);

const roots = new Map(comments.filter((c) => !c.in_reply_to_id).map((c) => [c.id, c]));
const replies = new Map([...roots.keys()].map((id) => [id, []]));
for (const comment of comments) {
  replies.get(comment.in_reply_to_id)?.push(comment);
}

const threads = [];
const botFixes = [];
for (const [rootId, root] of roots) {
  const thread = replies.get(rootId).sort((a, b) => a.created_at.localeCompare(b.created_at));
  const answered = isAnsweredByYou(root, thread, you);
  const state = states.get(rootId) ?? {};
  const fixCommit = botFixToCheck(thread);
  if (fixCommit) {
    botFixes.push({
      root_id: rootId,
      url: root.html_url,
      path: root.path,
      line: root.line ?? root.original_line,
      start_line: root.start_line,
      commit_id: root.commit_id,
      fix_commit: fixCommit,
      resolved: state.resolved ?? false,
      body: root.body,
      fix_reply: thread.at(-1).body,
    });
  }

  if (!values.all && (answered || state.resolved)) continue;

  threads.push({
    root_id: rootId,
    url: root.html_url,
    author: root.user?.login,
    created: root.created_at,
    path: root.path,
    line: root.line ?? root.original_line,
    start_line: root.start_line,
    side: root.side,
    commit_id: root.commit_id,
    diff_hunk: root.diff_hunk,
    body: root.body,
    resolved: state.resolved ?? false,
    outdated: state.outdated ?? false,
    answered_by_you: answered,
    replies: thread.map((c) => ({
      author: c.user?.login,
      created: c.created_at,
      body: c.body,
    })),
  });
}

threads.sort((a, b) => a.path.localeCompare(b.path) || (a.line ?? 0) - (b.line ?? 0));

console.log(
  JSON.stringify(
    {
      repo: slug,
      you,
      pull_request: {
        number: pr.number,
        title: pr.title,
        state: pr.state,
        draft: pr.draft,
        author: pr.user?.login,
        head_ref: pr.head.ref,
        head_sha: pr.head.sha,
        base_ref: pr.base.ref,
        url: pr.html_url,
      },
      counts: { threads_total: roots.size, threads_returned: threads.length, bot_fixes: botFixes.length },
      review_summaries: reviews
        .filter((r) => (r.body ?? "").trim() && !isNoise(r))
        .map((r) => ({
          id: r.id,
          author: r.user?.login,
          state: r.state,
          submitted: r.submitted_at,
          body: r.body,
        })),
      issue_comments: issueComments
        .filter((c) => !isNoise(c))
        .map((c) => ({
          id: c.id,
          author: c.user?.login,
          created: c.created_at,
          body: c.body,
        })),
      threads,
      bot_fixes: botFixes,
    },
    null,
    2,
  ),
);
