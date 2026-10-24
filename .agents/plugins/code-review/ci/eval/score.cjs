// Scores review runs replayed on past PR heads against the findings people judged
// (eval/cases.json): a good run reports the confirmed findings again and not the wrong ones.
//
//   node ci/eval/score.cjs <cases.json> <dir>
//
// <dir> holds one review result per case, named by caseKey ("tsc-rust-93-2464f92.json"), each
// the JSON a run returns (ci/findings.schema.json). A missing file is reported, not scored.
'use strict';

const fs = require('node:fs');
const path = require('node:path');

const caseKey = (c) => `${c.repo.split('/')[1]}-${c.pr}-${c.head.slice(0, 7)}`;

// A reported finding is the same issue as a judged one when it is on the same file and its lines
// are within SLACK of the judged lines: a replay words things differently and may anchor a few
// lines away.
const SLACK = 15;
const sameIssue = (judged, reported) =>
  reported.path === judged.path &&
  Number(reported.start_line) <= judged.end_line + SLACK &&
  Number(reported.end_line ?? reported.start_line) >= judged.start_line - SLACK;

function scoreCase(c, result) {
  const reported = Array.isArray(result?.findings) ? result.findings : [];
  const rows = c.findings.map((judged) => ({ ...judged, reported: reported.some((r) => sameIssue(judged, r)) }));
  return {
    key: caseKey(c),
    wrong: rows.filter((r) => r.verdict !== 'confirmed'),
    confirmed: rows.filter((r) => r.verdict === 'confirmed'),
    reported: reported.length,
  };
}

// Totals over the cases that have a result: wrong or partly-right findings reported again (want
// 0), confirmed findings reported again (want all), and how many findings the runs posted.
function score(cases, results) {
  const scored = [];
  const missing = [];
  for (const c of cases) {
    const key = caseKey(c);
    if (results[key] === undefined) missing.push(key);
    else scored.push(scoreCase(c, results[key]));
  }
  const count = (list, pick) => list.reduce((n, s) => n + s[pick].filter((r) => r.reported).length, 0);
  const total = (list, pick) => list.reduce((n, s) => n + s[pick].length, 0);
  return {
    scored,
    missing,
    wrong_repeated: count(scored, 'wrong'),
    wrong_total: total(scored, 'wrong'),
    confirmed_found: count(scored, 'confirmed'),
    confirmed_total: total(scored, 'confirmed'),
    reported: scored.reduce((n, s) => n + s.reported, 0),
  };
}

function render(summary) {
  const lines = ['| case | wrong or partly right, reported again | confirmed, reported again | findings posted |', '|---|---|---|---|'];
  for (const s of summary.scored) {
    const n = (list) => `${list.filter((r) => r.reported).length} of ${list.length}`;
    lines.push(`| ${s.key} | ${n(s.wrong)} | ${n(s.confirmed)} | ${s.reported} |`);
  }
  lines.push(`| all | ${summary.wrong_repeated} of ${summary.wrong_total} | ${summary.confirmed_found} of ${summary.confirmed_total} | ${summary.reported} |`);
  if (summary.missing.length) lines.push('', `No result for: ${summary.missing.join(', ')}`);
  return lines.join('\n');
}

if (require.main === module) {
  const [casesPath, dir] = process.argv.slice(2);
  if (!casesPath || !dir) {
    console.error('usage: score.cjs <cases.json> <results dir>');
    process.exit(2);
  }
  const { cases } = JSON.parse(fs.readFileSync(casesPath, 'utf8'));
  const results = {};
  for (const c of cases) {
    const file = path.join(dir, `${caseKey(c)}.json`);
    if (fs.existsSync(file)) results[caseKey(c)] = JSON.parse(fs.readFileSync(file, 'utf8'));
  }
  console.log(render(score(cases, results)));
}

module.exports = { caseKey, sameIssue, score, render };
