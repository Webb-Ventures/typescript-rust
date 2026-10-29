// Tests for the bench comment: node --test .github/scripts/bench-comment.test.mjs
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import { MARK, buildComment, readLegs } from './bench-comment.mjs';
import { benchTable } from '../../scripts/readme-bench.mjs';

const meta = { head: 'abcdef1234567', baseRef: 'main', base: '7654321fedcba', runUrl: 'https://example/run' };
const table = '### Benchmarks (pr @ abcdef1, linux-x86_64)\n\n| Scenario | Tool | Median |\n|---|---|---:|\n| check-trpc | **ts-r-check** | 400.0 ms |\n| check-trpc | tsc-7 | 900.0 ms |\n';
const legs = [
  { label: 'macos-arm64', runner: 'macos-15', gate: false, threads: 3, code: 1, tables: table.replace('linux-x86_64', 'macos-aarch64').replace('900.0 ms', '300.0 ms') },
  { label: 'linux-x64', runner: 'ubuntu-24.04', gate: true, threads: 4, code: 0, tables: table },
  { label: 'windows-arm64', runner: 'windows-11-arm', gate: false, threads: 4, code: undefined, tables: '' },
];

test('the gate row comes first and its tables are the results the README reads', () => {
  const body = buildComment(legs, meta);
  assert.ok(body.startsWith(`${MARK}\n### Bench`));
  const rows = body.split('\n').filter((l) => /^\| (linux|macos|windows)/.test(l));
  assert.deepEqual(rows, [
    '| linux-x64 (the gate) | 4 | passed |',
    '| macos-arm64 | 3 | **regression** |',
    '| windows-arm64 | 4 | did not run |',
  ]);
  const results = /<!-- ts-r-bench:results -->\n([\s\S]*?)\n<!-- \/ts-r-bench:results -->/.exec(body)[1];
  assert.equal(results, table.trim());
  assert.match(body, /<details><summary>macos-arm64, 3 threads: \*\*regression\*\*<\/summary>/);
  assert.doesNotMatch(body, /<summary>windows-arm64/);
  // scripts/readme-bench.mjs reads the gate's tables and its runner.
  assert.match(benchTable(body, '2026-10-29'), /on linux-x86_64 \(a GitHub-hosted ubuntu-24\.04 runner\), measured 2026-10-29/);
  assert.match(benchTable(body), /\| `check-trpc` \| `ts-r-check` 400 ms \| `tsc-7` 900 ms \| \*\*2\.3× faster\*\* \|/);
});

test('long tables are cut to fit a GitHub comment', () => {
  const big = { ...legs[1], tables: `${'x'.repeat(70_000)}` };
  const body = buildComment([big, { ...legs[0], tables: 'y'.repeat(10_000) }], meta);
  assert.ok(body.length < 65_536, String(body.length));
  assert.match(body, /truncated/);
});

test('legs are read from the artifacts, malformed ones skipped', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'bench-comment-'));
  try {
    fs.mkdirSync(`${dir}/bench-pr-linux-x64`);
    fs.writeFileSync(`${dir}/bench-pr-linux-x64/leg.json`, JSON.stringify({ label: 'linux-x64', runner: 'ubuntu-24.04', gate: true, threads: 4, code: 0 }));
    fs.writeFileSync(`${dir}/bench-pr-linux-x64/comment.md`, table);
    fs.mkdirSync(`${dir}/bench-pr-broken`);
    fs.writeFileSync(`${dir}/bench-pr-broken/leg.json`, '{');
    fs.mkdirSync(`${dir}/bench-pr-no-tables`);
    fs.writeFileSync(`${dir}/bench-pr-no-tables/leg.json`, JSON.stringify({ label: 'linux-arm64', threads: 4 }));
    const read = readLegs(dir);
    assert.deepEqual(read.map((l) => [l.label, l.gate, l.code, l.tables.length > 0]), [['linux-x64', true, 0, true], ['linux-arm64', false, undefined, false]]);
    assert.deepEqual(readLegs(`${dir}/missing`), []);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
