#!/usr/bin/env node
// Replies to a pull request review thread, or posts a top-level PR comment.
//
// The body is always sent as JSON, so a body starting with `@`, or carrying
// backticks, newlines or quotes, posts verbatim. Never pass a review reply
// through `gh api --field body=…`: gh reads a leading `@` as a file reference,
// and six replies on #2104 are the literal string `@-` because of it.
import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";

import { fail, ghPostJson, resolveReviewThread, withRefreshMark } from "./gh.mjs";

const USAGE = [
  "usage: reply.mjs <owner/repo> <pr> --thread <root-comment-id> --body-file <path> [--resolve]",
  "       reply.mjs <owner/repo> <pr> --summary --body-file <path>",
  "       add --dry-run to print what would be sent",
].join("\n");

const { values, positionals } = parseArgs({
  options: {
    thread: { type: "string" },
    summary: { type: "boolean", default: false },
    "body-file": { type: "string" },
    resolve: { type: "boolean", default: false },
    "dry-run": { type: "boolean", default: false },
  },
  allowPositionals: true,
});

if (positionals.length !== 2 || !values["body-file"]) fail(USAGE);
if (Boolean(values.thread) === values.summary) {
  fail("pass exactly one of --thread or --summary");
}

const slug = positionals[0];
const number = Number(positionals[1]);
const rootId = values.thread ? Number(values.thread) : null;
if (!slug.includes("/") || !Number.isInteger(number)) fail(USAGE);
if (values.thread && !Number.isInteger(rootId)) fail("--thread takes a review comment id");

const text = readFileSync(values["body-file"] === "-" ? 0 : values["body-file"], "utf8").trim();
if (!text) fail("refusing to post an empty body");
// The summary asks the ts-r review bot to re-read the threads (and approve once all are
// resolved). The marker is an HTML comment, so it is invisible, here and on other repos.
const body = values.summary ? withRefreshMark(text) : text;

const path = values.summary
  ? `/repos/${slug}/issues/${number}/comments`
  : `/repos/${slug}/pulls/${number}/comments/${rootId}/replies`;

if (values["dry-run"]) {
  if (values.resolve && rootId) console.log(`resolve the thread starting at comment ${rootId}`);
  console.log(`POST ${path}`);
  console.log(body);
  process.exit(0);
}

// Resolve first, then reply. The reply is what tells the ts-r review bot to look again (GitHub
// sends no event for a resolution), so the thread has to be resolved by the time it lands. A
// failed resolve is a warning and the reply still goes out; resolving twice is harmless, so a
// retry after a failed post repeats nothing visible.
if (values.resolve && rootId) {
  try {
    if (resolveReviewThread(slug, number, rootId)) {
      console.log(`resolved thread ${rootId}`);
    } else {
      console.warn(`warning: no thread starts at comment ${rootId}; replying without resolving`);
    }
  } catch (error) {
    console.warn(`warning: resolving thread ${rootId} failed (${error.message}); replying anyway`);
  }
}

const created = ghPostJson(path, { body });
console.log(created.html_url ?? created.url ?? "posted");
