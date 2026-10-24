#!/usr/bin/env node
// Run with: node --test .agents/skills/action-review/scripts/gh.test.mjs
//
// Covers the two decisions that are easy to get wrong and impossible to see in
// the output: when a thread counts as answered, and which gh failures a poll
// loop is allowed to ride out.
import assert from "node:assert/strict";
import { test } from "node:test";

import { botFixToCheck, isAnsweredByYou, isPermanentGhError, REFRESH_MARK, REVIEW_BOT, summaryReviewSince, withRefreshMark } from "./gh.mjs";

const YOU = "reece-bts";
const comment = (author, created) => ({ user: { login: author }, created_at: created });

test("a thread nobody has replied to is unanswered", () => {
  assert.equal(isAnsweredByYou(comment(YOU, "T1"), [], YOU), false);
  assert.equal(isAnsweredByYou(comment("riley", "T1"), [], YOU), false);
});

test("your reply answers a finding, whoever raised it", () => {
  assert.equal(isAnsweredByYou(comment(YOU, "T1"), [comment(YOU, "T2")], YOU), true);
  assert.equal(isAnsweredByYou(comment("riley", "T1"), [comment(YOU, "T2")], YOU), true);
});

test("a reviewer coming back after your reply reopens the thread", () => {
  const replies = [comment(YOU, "T2"), comment("riley", "T3")];
  assert.equal(isAnsweredByYou(comment("riley", "T1"), replies, YOU), false);
});

test("a reply of yours that predates the reviewer's last word does not count", () => {
  const replies = [comment(YOU, "T2"), comment("riley", "T3"), comment("riley", "T4")];
  assert.equal(isAnsweredByYou(comment("riley", "T1"), replies, YOU), false);
});

// The honest limit of the rule, pinned so a future change is a decision rather
// than an accident. Reviewer and responder share a login on a self-authored
// review, so a second thought you add to your own thread is indistinguishable
// from more of your answer, and it reads as answered. Resolving the threads you
// fixed, or --all, is how you get back to one you want to see again.
test("a follow-up on your own review reads as answered, by choice", () => {
  const replies = [comment(YOU, "T2"), comment(YOU, "T3")];
  assert.equal(isAnsweredByYou(comment(YOU, "T1"), replies, YOU), true);
});

test("permanent gh failures are told apart from transient ones", () => {
  const ghError = (detail) => Object.assign(new Error(detail), { detail });

  assert.equal(isPermanentGhError(ghError("gh: Not Found (HTTP 404)")), true);
  assert.equal(isPermanentGhError(ghError("gh: Bad credentials (HTTP 401)")), true);

  assert.equal(isPermanentGhError(ghError("gh: Server Error (HTTP 500)")), false);
  assert.equal(isPermanentGhError(ghError("gh: Bad Gateway (HTTP 502)")), false);
  // 403 is rate limiting as often as it is permission, and rate limiting clears.
  assert.equal(isPermanentGhError(ghError("API rate limit exceeded (HTTP 403)")), false);
  assert.equal(isPermanentGhError(new Error("socket hang up")), false);
});

test("a ts-r summary comment reports a review that finished or failed after the cut-off", () => {
  const summary = (meta) => ({
    body: `<!-- ts-r-review:summary -->\n<!-- ts-r-review:meta ${Buffer.from(JSON.stringify(meta)).toString("base64")} -->\n### Code review`,
  });
  const since = "2026-10-15T10:00:05Z";
  // A clean review: no GitHub review submission, only the summary.
  assert.equal(summaryReviewSince(summary({ reviewed_at: "2026-10-15T10:04:00.120Z" }), since), "reviewed");
  assert.equal(summaryReviewSince(summary({ reviewed_at: "2026-10-15T10:00:05.900Z" }), since), "reviewed");
  assert.equal(summaryReviewSince(summary({ reviewed_at: "2026-10-15T09:00:00.000Z", failed_at: "2026-10-15T10:03:00.000Z" }), since), "failed");
  // Still running, or edited since by a thread refresh: the last review is older than the cut-off.
  assert.equal(summaryReviewSince(summary({ busy: true, reviewed_at: "2026-10-15T09:00:00.000Z" }), since), null);
  assert.equal(summaryReviewSince({ body: "a plain comment" }, since), null);
});

test("the skill's summary carries the ts-r review bot's refresh marker, once", async () => {
  // The workflow and the box match the marker the review plugin defines; keep the two in step.
  const { createRequire } = await import("node:module");
  const plugin = createRequire(import.meta.url)("../../../plugins/code-review/ci/github.cjs");
  assert.equal(REFRESH_MARK, plugin.REFRESH_MARK);
  const once = withRefreshMark("Fixed 2 of 3 findings.");
  assert.equal(once, `Fixed 2 of 3 findings.\n\n${REFRESH_MARK}`);
  assert.equal(withRefreshMark(once), once);
});

const SHA = "4362d15a9c9c56a75168b7240f2fcb67b1ade061";
const botFix = (created) => ({
  user: { login: REVIEW_BOT },
  created_at: created,
  body: `Fixed in [\`4362d15\`](https://github.com/Webb-Ventures/typescript-rust/commit/${SHA}) by Codex for @riley-wv.\n\nInitial escapes now decode.`,
});

test("a thread the review bot fixed and resolved is a fix to check", () => {
  assert.equal(botFixToCheck([botFix("T2")]), SHA);
});

test("a reply after the bot's fix means someone checked it", () => {
  assert.equal(botFixToCheck([botFix("T2"), comment(YOU, "T3")]), null);
  assert.equal(botFixToCheck([comment(YOU, "T3"), botFix("T2")]), null);
});

test("the bot's other replies, and look-alikes from people, are not fixes", () => {
  const notFixed = { user: { login: REVIEW_BOT }, created_at: "T2", body: "Not fixed by Codex (asked by @riley-wv), so this stays open: ..." };
  assert.equal(botFixToCheck([notFixed]), null);
  assert.equal(botFixToCheck([{ ...botFix("T2"), user: { login: "riley" } }]), null);
  assert.equal(botFixToCheck([]), null);
});
