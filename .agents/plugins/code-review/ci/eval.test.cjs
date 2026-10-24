// Run with: node --test .agents/plugins/code-review/ci/*.test.cjs
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');

const { caseKey, render, score } = require('./eval/score.cjs');

const { cases } = JSON.parse(fs.readFileSync(path.join(__dirname, 'eval/cases.json'), 'utf8'));
const catalogue = fs.readFileSync(path.join(__dirname, '../skills/ts-r-review/references/false-positives.md'), 'utf8');

test('every eval case is a review run with a judged wrong or partly right finding', () => {
  assert.ok(cases.length >= 8);
  const keys = new Set();
  for (const c of cases) {
    assert.match(c.repo, /^Webb-Ventures\/(tsc-rust|typescript-rust)$/);
    assert.match(c.head, /^[0-9a-f]{40}$/);
    assert.ok(!keys.has(caseKey(c)), `duplicate case ${caseKey(c)}`);
    keys.add(caseKey(c));
    assert.ok(c.findings.some((f) => f.verdict !== 'confirmed'), `${caseKey(c)} has no judged mistake`);
    for (const f of c.findings) {
      assert.ok(['wrong', 'partly', 'confirmed'].includes(f.verdict));
      assert.ok(f.path && f.start_line <= f.end_line, `${caseKey(c)} ${f.id} has no location`);
      if (f.verdict !== 'confirmed') assert.ok(f.evidence, `${caseKey(c)} ${f.id} says why it was wrong`);
    }
  }
});

test('every judged mistake is in the false-positive catalogue the reviewer reads', () => {
  for (const c of cases) {
    const name = `${c.repo.split('/')[1]} #${c.pr}`;
    assert.ok(catalogue.includes(name), `false-positives.md does not mention ${name}`);
  }
});

const judged = {
  repo: 'Webb-Ventures/tsc-rust', pr: 93, head: '2464f927f'.padEnd(40, '0'),
  findings: [
    { id: 'w', path: 'crates/a/src/x.rs', start_line: 100, end_line: 104, verdict: 'wrong', evidence: 'tsc agrees' },
    { id: 'c', path: 'crates/a/src/y.rs', start_line: 10, end_line: 10, verdict: 'confirmed' },
  ],
};

test('a replay that drops the wrong finding and keeps the confirmed one scores clean', () => {
  const summary = score([judged], { [caseKey(judged)]: { summary: '', findings: [{ path: 'crates/a/src/y.rs', start_line: 12, end_line: 12 }] } });
  assert.equal(summary.wrong_repeated, 0);
  assert.equal(summary.confirmed_found, 1);
  assert.match(render(summary), /\| tsc-rust-93-2464f92 \| 0 of 1 \| 1 of 1 \| 1 \|/);
});

test('a finding near the judged lines of the same file counts as the same issue', () => {
  const near = { path: 'crates/a/src/x.rs', start_line: 112, end_line: 113 };
  const far = { path: 'crates/a/src/x.rs', start_line: 200, end_line: 201 };
  const other = { path: 'crates/a/src/z.rs', start_line: 100, end_line: 104 };
  assert.equal(score([judged], { [caseKey(judged)]: { findings: [near] } }).wrong_repeated, 1);
  assert.equal(score([judged], { [caseKey(judged)]: { findings: [far, other] } }).wrong_repeated, 0);
});

test('nearby PR 39 findings are matched once, preferring the closest range', () => {
  const c = cases.find((c) => c.repo === 'Webb-Ventures/typescript-rust' && c.pr === 39);
  const wrong = c.findings.find((f) => f.verdict === 'wrong');
  const confirmed = c.findings.find((f) => f.start_line === 701);
  const replay = (findings) => score([c], { [caseKey(c)]: { findings } });
  for (const findings of [[confirmed], [{ ...confirmed, start_line: 703, end_line: 703 }]]) {
    const summary = replay(findings);
    assert.equal(summary.wrong_repeated, 0);
    assert.equal(summary.confirmed_found, 1);
  }
  const wrongOnly = replay([wrong]);
  assert.equal(wrongOnly.wrong_repeated, 1);
  assert.equal(wrongOnly.confirmed_found, 0);
  for (const findings of [[wrong, confirmed], [confirmed, wrong]]) {
    const summary = replay(findings);
    assert.equal(summary.wrong_repeated, 1);
    assert.equal(summary.confirmed_found, 1);
  }
});

test('an exact match takes precedence over another report near the same judged finding', () => {
  const c = { ...judged, findings: [
    judged.findings[0],
    { ...judged.findings[0], id: 'c', start_line: 110, end_line: 114, verdict: 'confirmed' },
  ] };
  const exact = { path: 'crates/a/src/x.rs', start_line: 100, end_line: 104 };
  const near = { ...exact, start_line: 101, end_line: 105 };
  for (const findings of [[near, exact], [exact, near]]) {
    const summary = score([c], { [caseKey(c)]: { findings } });
    assert.equal(summary.wrong_repeated, 1);
    assert.equal(summary.confirmed_found, 1);
  }
});

test('equally close judged findings are flagged instead of counted', () => {
  const c = { ...judged, findings: [
    { ...judged.findings[0], start_line: 100, end_line: 100 },
    { ...judged.findings[1], path: 'crates/a/src/x.rs', start_line: 110, end_line: 110 },
  ] };
  const findings = [{ path: 'crates/a/src/x.rs', start_line: 105 }];
  const summary = score([c], { [caseKey(c)]: { findings } });
  assert.equal(summary.wrong_repeated, 0);
  assert.equal(summary.confirmed_found, 0);
  assert.deepEqual(summary.scored[0].ambiguous, [{ reported: 1, judged: ['w', 'c'] }]);
  assert.match(render(summary), /Ambiguous matches \(excluded from verdict counts\):/);
  assert.match(render(summary), /tsc-rust-93-2464f92 report 1: w, c/);
});

test('equally close reports cannot both claim a judged finding', () => {
  const findings = [
    { path: 'crates/a/src/x.rs', start_line: 99, end_line: 103 },
    { path: 'crates/a/src/x.rs', start_line: 101, end_line: 105 },
  ];
  const summary = score([judged], { [caseKey(judged)]: { findings } });
  assert.equal(summary.wrong_repeated, 0);
  assert.deepEqual(summary.scored[0].ambiguous, [
    { reported: 1, judged: ['w'] },
    { reported: 2, judged: ['w'] },
  ]);
});

test('a case without a result is listed, not scored as clean', () => {
  const summary = score([judged], {});
  assert.deepEqual(summary.missing, ['tsc-rust-93-2464f92']);
  assert.equal(summary.wrong_total, 0);
  assert.match(render(summary), /No result for: tsc-rust-93-2464f92/);
});
