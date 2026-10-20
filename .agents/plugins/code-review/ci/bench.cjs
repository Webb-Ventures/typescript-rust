// The bench comment the review box keeps on a PR (box/dispatcher.mjs, runBench): a progress line
// while a bench is queued or running, like the code review summary's, then the PR-vs-base tables
// from `cargo xtask bench` and the regression gate's verdict. One comment per PR, edited in place;
// a new run keeps the last results under the new progress line until its own replace them.
'use strict';

const { SPINNER, isBot } = require('./github.cjs');

const BENCH_MARK = '<!-- ts-r-bench -->';
const META_RE = /<!-- ts-r-bench:meta ([A-Za-z0-9+/=]+) -->/;
const RESULTS_RE = /<!-- ts-r-bench:results -->\n([\s\S]*?)\n<!-- \/ts-r-bench:results -->/;
// GitHub rejects comments over 65,536 characters.
const MAX_TABLES = 55_000;

const encode = (value) => Buffer.from(JSON.stringify(value)).toString('base64');
const decode = (text) => {
  try {
    return JSON.parse(Buffer.from(text, 'base64').toString('utf8'));
  } catch {
    return {};
  }
};

const VERDICTS = {
  0: '**Passed.** None of our tools is slower than the base beyond the gate (`max_regression` and `noise_sigmas` in bench/scenarios.toml).',
  1: '**Regression.** At least one of our tools is slower than the base beyond the gate, in the first run and in the re-run with the PR measured first.',
};

// meta: { status, busy, sha, base, baseRef, duration, code, log } plus the result tables.
function renderBench(meta, tables) {
  const { status, busy, ...kept } = meta;
  const lines = [BENCH_MARK, `<!-- ts-r-bench:meta ${encode(kept)} -->`, '### Bench', ''];
  if (status) lines.push(`> ${busy ? `${SPINNER} ` : ''}${status}`, '');
  if (meta.sha) {
    lines.push(VERDICTS[meta.code] ?? `**Error.** The bench did not complete (exit ${meta.code}); the log is on the review box (\`${meta.log}\`).`, '');
    if (tables) {
      const text = tables.length > MAX_TABLES ? `${tables.slice(0, MAX_TABLES)}\n\n_(truncated; the full tables are on the review box)_` : tables;
      lines.push('<!-- ts-r-bench:results -->', text.trim(), '<!-- /ts-r-bench:results -->', '');
    }
    lines.push(`<sub>\`${meta.sha.slice(0, 7)}\` against ${meta.baseRef} @ \`${meta.base.slice(0, 7)}\`${meta.duration ? ` in ${meta.duration}` : ''}, on the review box's dedicated bench cores.</sub>`);
  } else if (!status) {
    lines.push('No results yet.');
  }
  return lines.join('\n');
}

async function findBench(github, owner, repo, number) {
  const comments = await github.paginate(github.rest.issues.listComments, { owner, repo, issue_number: number, per_page: 100 });
  return comments.findLast((c) => isBot(c.user?.login) && c.body?.includes(BENCH_MARK)) ?? null;
}

// Updates the progress line, and with `result` ({ sha, base, baseRef, duration, code, log,
// tables }) the results too.
async function setBenchStatus({ github, owner, repo, number, status, busy = false, result }) {
  const existing = await findBench(github, owner, repo, number);
  const before = existing ? decode(META_RE.exec(existing.body)?.[1] ?? '') : {};
  const kept = existing ? RESULTS_RE.exec(existing.body)?.[1] : undefined;
  const { tables, ...rest } = result ?? {};
  const body = renderBench({ ...before, ...rest, status, busy }, result ? tables : kept);
  if (existing) await github.rest.issues.updateComment({ owner, repo, comment_id: existing.id, body });
  else await github.rest.issues.createComment({ owner, repo, issue_number: number, body });
}

module.exports = { BENCH_MARK, renderBench, findBench, setBenchStatus };
