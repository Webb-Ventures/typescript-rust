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

test('a case without a result is listed, not scored as clean', () => {
  const summary = score([judged], {});
  assert.deepEqual(summary.missing, ['tsc-rust-93-2464f92']);
  assert.equal(summary.wrong_total, 0);
  assert.match(render(summary), /No result for: tsc-rust-93-2464f92/);
});
