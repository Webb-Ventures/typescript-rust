#!/usr/bin/env node
// Shared `gh` plumbing for the action-review scripts.
//
// Everything that reads or writes a comment body goes through `gh`, not
// `gh-axi`: gh-axi renders TOON, which collapses a multi-line body to `@-`.
import { execFileSync } from "node:child_process";
import { unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// Review bodies run long, and --paginate --slurp returns every page at once.
const MAX_BUFFER = 64 * 1024 * 1024;

export function gh(args) {
  try {
    return execFileSync("gh", args, { encoding: "utf8", maxBuffer: MAX_BUFFER });
  } catch (error) {
    const detail = (error.stderr || error.message || "").toString().trim();
    const failure = new Error(`gh ${args.join(" ")} failed: ${detail}`);
    failure.detail = detail;
    throw failure;
  }
}

// A 404, a revoked token or a typo'd slug will never succeed, so a poll loop has
// to stop rather than report the silence as "no review yet". A 403 is left out:
// gh returns it for rate limiting too, which does clear on its own.
export function isPermanentGhError(error) {
  const detail = error?.detail ?? "";
  return /HTTP 40[14]\b/.test(detail) || /Not Found|Bad credentials|Resource protected/i.test(detail);
}

export function ghJson(path) {
  return JSON.parse(gh(["api", path]));
}

// --slurp wraps the pages in an array; the callers all want the flat list.
export function ghJsonAll(path) {
  return JSON.parse(gh(["api", path, "--paginate", "--slurp"])).flat();
}

// Answered means answered *last*. The reduce is seeded with the root's own
// timestamp because a reply only answers a finding if it postdates it.
//
// When the review came from your own account — every review on this repo does,
// since `/code-review --comment` runs as the token's user — reviewer and
// responder share a login and nothing in the payload separates "I answered the
// finding" from "I added another finding". The rule below resolves that
// ambiguity towards answered, which keeps a thread you have dealt with from
// returning on every run; `--all`, and resolving the threads you fixed, are the
// way back to one you want to see again. `collect.test.mjs` pins the cases.
export function isAnsweredByYou(root, replies, you) {
  const lastOther = [root, ...replies]
    .filter((comment) => comment.user?.login !== you)
    .reduce((latest, comment) => (comment.created_at > latest ? comment.created_at : latest), root.created_at);
  return replies.some((comment) => comment.user?.login === you && comment.created_at > lastOther);
}

// Via a file, never `--field`: gh reads a leading `@` as a file reference, so a
// body can post as a filename. This is the whole reason these scripts exist.
export function ghPostJson(path, payload) {
  const file = join(tmpdir(), `action-review-${process.pid}-${Date.now()}.json`);
  writeFileSync(file, JSON.stringify(payload));
  try {
    return JSON.parse(gh(["api", "--method", "POST", path, "--input", file]));
  } finally {
    unlinkSync(file);
  }
}

export function ghGraphql(query, variables) {
  const args = ["api", "graphql", "-f", `query=${query}`];
  for (const [key, value] of Object.entries(variables)) {
    args.push("-F", `${key}=${value}`);
  }
  return JSON.parse(gh(args)).data;
}

export function fail(message) {
  console.error(message);
  process.exit(1);
}

// A review left by one of these is repository furniture, not a code review.
export const NOISE_LOGINS = new Set([
  "linear-code[bot]",
  "github-actions[bot]",
  "vercel[bot]",
  "codecov[bot]",
  "sonarcloud[bot]",
]);

// The ts-r review bot reports every review in its summary comment, and submits it as a GitHub
// review (changes requested, or approved when clean); a failed review shows up only in the
// summary. Its meta marker carries when the last review finished (or failed). Returns
// "reviewed" or "failed" for a summary comment that records a review after `since`, else null.
const SUMMARY_MARK = "<!-- ts-r-review:summary -->";

// A PR comment from a trusted user that carries this asks the ts-r review bot to re-read its
// threads and update its verdict (it approves once every finding is resolved). GitHub sends no
// event for a thread resolution, so the skill's summary comment carries it.
export const REFRESH_MARK = "<!-- ts-r-review:refresh -->";
export function withRefreshMark(body) {
  return body.includes(REFRESH_MARK) ? body : `${body}\n\n${REFRESH_MARK}`;
}
const META_RE = /<!-- ts-r-review:meta ([A-Za-z0-9+/=]+) -->/;
export function summaryReviewSince(comment, since) {
  const body = comment.body ?? "";
  const match = body.includes(SUMMARY_MARK) && META_RE.exec(body);
  if (!match) return null;
  let meta;
  try {
    meta = JSON.parse(Buffer.from(match[1], "base64").toString("utf8"));
  } catch {
    return null;
  }
  // Floored to seconds like `since` (see watch.mjs), so a review in the cut-off's second counts.
  const after = (at) => (at ?? "").replace(/\.\d+Z$/, "Z") >= since;
  if (after(meta.reviewed_at)) return "reviewed";
  if (after(meta.failed_at)) return "failed";
  return null;
}

export const NOISE_MARKERS = ["<!-- linear-linkback -->"];

export function isNoise({ user, body }) {
  const login = user?.login ?? "";
  const text = body ?? "";
  return NOISE_LOGINS.has(login) || NOISE_MARKERS.some((marker) => text.includes(marker));
}

const THREAD_LOOKUP = `
query($owner:String!,$repo:String!,$number:Int!,$cursor:String){
  repository(owner:$owner,name:$repo){
    pullRequest(number:$number){
      reviewThreads(first:100, after:$cursor){
        pageInfo{hasNextPage endCursor}
        nodes{id comments(first:1){nodes{databaseId}}}
      }
    }
  }
}`;

const RESOLVE = `
mutation($threadId:ID!){
  resolveReviewThread(input:{threadId:$threadId}){thread{isResolved}}
}`;

function postJson(path, payload) {
  const file = join(tmpdir(), `action-review-${process.pid}-${Date.now()}.json`);
  writeFileSync(file, JSON.stringify(payload));
  try {
    return JSON.parse(gh(["api", "--method", "POST", path, "--input", file]));
  } finally {
    unlinkSync(file);
  }
}

// The REST reply carries no thread id, so the thread has to be found by the
// database id of the comment it starts with. Returns false when there is no
// such thread — `--thread` given a reply's id rather than a root's is the easy
// way to get there, since the REST reply endpoint accepts it and re-parents.
export function resolveReviewThread(slug, number, rootId) {
  const [owner, repo] = slug.split("/");
  let cursor = null;
  for (;;) {
    const variables = { owner, repo, number };
    if (cursor) variables.cursor = cursor;
    const page = ghGraphql(THREAD_LOOKUP, variables).repository.pullRequest.reviewThreads;
    for (const node of page.nodes) {
      if (node.comments.nodes[0]?.databaseId === rootId) {
        ghGraphql(RESOLVE, { threadId: node.id });
        return true;
      }
    }
    if (!page.pageInfo.hasNextPage) return false;
    cursor = page.pageInfo.endCursor;
  }
}
