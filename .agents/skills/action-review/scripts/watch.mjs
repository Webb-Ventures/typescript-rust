#!/usr/bin/env node
// Polls a pull request until a review lands, printing one line per event.
//
// Written for the Monitor tool: each stdout line becomes a notification, and
// the script exits once there is something to act on, so the watch ends by
// itself. Closed and merged are emitted too, and so is a PR that cannot be
// read — silence should never have to be read as "still waiting".
import { setTimeout as sleep } from "node:timers/promises";
import { parseArgs } from "node:util";

import { fail, gh, isPermanentGhError, NOISE_LOGINS, summaryReviewSince } from "./gh.mjs";

// A watch that has lost the API for five minutes is not watching anything, and
// saying so is more useful than running out the clock in silence.
const MAX_CONSECUTIVE_FAILURES = 5;

let consecutiveFailures = 0;

// Transient failures are worth riding out; a 404, a revoked token or a typo'd
// slug never clears, so those end the watch with the reason instead.
function read(path, { paginate = false } = {}) {
  const args = paginate ? ["api", path, "--paginate", "--slurp"] : ["api", path];
  try {
    const data = JSON.parse(gh(args));
    return paginate ? data.flat() : data;
  } catch (error) {
    if (isPermanentGhError(error)) fail(`cannot read ${path}: ${error.detail ?? error.message}`);
    return null;
  }
}

const USAGE = "usage: watch.mjs <owner/repo> <pr-number> [--interval 60] [--max-minutes 180] [--since <iso>]";

const { values, positionals } = parseArgs({
  options: {
    interval: { type: "string", default: "60" },
    "max-minutes": { type: "string", default: "180" },
    // Anything already posted is old news, so the default cut-off is now.
    // Pass an ISO timestamp to resume a watch across a monitor's expiry.
    since: { type: "string" },
  },
  allowPositionals: true,
});

if (positionals.length !== 2) fail(USAGE);

const slug = positionals[0];
const number = Number(positionals[1]);
const intervalMs = Number(values.interval) * 1000;
const maxMinutes = Number(values["max-minutes"]);
if (!slug.includes("/") || !Number.isInteger(number)) fail(USAGE);
// Unvalidated, `--interval 60s` is NaN, which sleep() coerces to a 1 ms timer
// while `Date.now() > NaN` disables the deadline: an unbounded poll loop three
// API calls wide, from one typo.
if (!Number.isFinite(intervalMs) || intervalMs <= 0) fail(`--interval takes seconds: ${USAGE}`);
if (!Number.isFinite(maxMinutes) || maxMinutes < 0) fail(`--max-minutes takes minutes: ${USAGE}`);

// GitHub timestamps have second precision and `new Date()` has milliseconds, and
// these are compared as strings: at index 19 a bare `Z` sorts above `.314Z`, so
// an unfloored cut-off both reports the second it started in and would hide a
// comment posted later in that same second. Flooring errs towards reporting.
const since = values.since ?? new Date().toISOString().replace(/\.\d+Z$/, "Z");
const deadline = Date.now() + maxMinutes * 60 * 1000;
console.log(`watching ${slug}#${number} for a review since ${since}`);

for (;;) {
  const pr = read(`/repos/${slug}/pulls/${number}`);
  if (pr && pr.state !== "open") {
    console.log(`pr ${number} is ${pr.merged ? "merged" : "closed"} — nothing to action`);
    break;
  }

  // The comment list defaults to oldest-first, so on a PR past 100 comments a
  // single page is frozen history that no new review can ever appear on.
  // Reviews take no `direction`, so those are paginated instead.
  const reviews = read(`/repos/${slug}/pulls/${number}/reviews?per_page=100`, { paginate: true });
  const comments = read(`/repos/${slug}/pulls/${number}/comments?per_page=100&sort=created&direction=desc`);
  // `since` filters on the last edit, which is how a summary comment reports a review.
  const conversation = read(`/repos/${slug}/issues/${number}/comments?per_page=100&since=${since}`, { paginate: true });

  if (pr === null || reviews === null || comments === null || conversation === null) {
    consecutiveFailures += 1;
    if (consecutiveFailures >= MAX_CONSECUTIVE_FAILURES) {
      console.log(
        `giving up on #${number}: ${MAX_CONSECUTIVE_FAILURES} consecutive failed reads — the watch is not running`,
      );
      break;
    }
  } else {
    consecutiveFailures = 0;

    const newReviews = reviews.filter(
      (r) =>
        (r.submitted_at ?? "") > since &&
        !NOISE_LOGINS.has(r.user?.login) &&
        ((r.body ?? "").trim() || r.state === "CHANGES_REQUESTED" || r.state === "APPROVED"),
    );
    const newComments = comments.filter(
      (c) => c.created_at > since && !c.in_reply_to_id && !NOISE_LOGINS.has(c.user?.login),
    );

    const summaries = conversation.map((c) => summaryReviewSince(c, since)).filter(Boolean);

    if (newReviews.length || newComments.length || summaries.length) {
      for (const review of newReviews) {
        console.log(`review ${review.state.toLowerCase()} by ${review.user?.login} on #${number}`);
      }
      if (newComments.length) {
        console.log(`${newComments.length} new inline review comment(s) on #${number}`);
      }
      for (const outcome of summaries) {
        console.log(outcome === "reviewed" ? `the ts-r review finished on #${number} (see its summary comment)` : `the ts-r review failed on #${number} (see its summary comment)`);
      }
      console.log(`run collect.mjs ${slug} ${number} to action them`);
      break;
    }
  }

  if (Date.now() > deadline) {
    console.log(`no review on #${number} after ${maxMinutes} minutes — still waiting`);
    break;
  }

  await sleep(intervalMs);
}
