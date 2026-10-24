// Tests for what a bench run skips: node --test .agents/plugins/code-review/ci/*.test.cjs
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const vm = require('node:vm');
const reuse = require('./bench-reuse.cjs');

const TOML = String.raw`
[settings]
max_regression = 0.05

[[scenario]]
id = "parse-small"
title = "Parse"

  [[scenario.tool]]
  name = "ts-r"
  ours = true
  cmd = "{bin}/ts-r parse {input}"

  [[scenario.tool]]
  name = "oxc"
  cmd = "node bench/js/oxc.mjs {input}"

[[scenario]]
id = "lint-same-rules"
setup = "node bench/js/eslint-same-rules.mjs {bin}/ts-r-lint eslint.config.js"

  [[scenario.tool]]
  name = "ts-r-lint"
  ours = true
  cmd = "{bin}/ts-r-lint ."

  [[scenario.tool]]
  name = "eslint"
  cmd = "node_modules/.bin/eslint ."

[[scenario]]
id = "check"

  [[scenario.tool]]
  name = "ts-r-check"
  ours = true
  cmd = "{bin}/ts-r-check -p ."

  [[scenario.tool]]
  name = "tsc"
  cmd = "node_modules/.bin/tsc -p ."

  [[scenario.tool]]
  name = "tsc-after-ours"
  cmd = "{bin}/ts-r-check -p . && tsc"

  [[scenario.tool]]
  name = "loop"
  ours = true
  cmd = "{bin}/examples/build_loop 20"
  requires = ["{bin}/ts-r-build"]
`;

test('the scenarios name the binaries to build', () => {
  assert.deepEqual(reuse.scanBins(TOML), ['ts-r', 'ts-r-build', 'ts-r-check', 'ts-r-lint', 'ts-r-parse']);
  assert.deepEqual(reuse.scanBins('cmd = "{bin}/ts-r run x.ts" "{bin}/ts-r --version"'), ['ts-r', 'ts-r-run']);
  assert.deepEqual(reuse.scanBins(''), []);
});

test('the repo\'s own scenarios name the bench binaries and none of the others', () => {
  const file = path.join(__dirname, '../../../../bench/scenarios.toml');
  const bins = reuse.scanBins(fs.readFileSync(file, 'utf8'));
  for (const bin of ['ts-r', 'ts-r-check', 'ts-r-lint', 'ts-r-build']) assert.ok(bins.includes(bin), bin);
  for (const bin of ['ts-r-lsp', 'ts-r-dev', 'xtask']) assert.ok(!bins.includes(bin), bin);
});

test('reference tools that depend on our binaries are never reused', () => {
  assert.deepEqual([...reuse.reusableTools(TOML)].sort(), ['check\ntsc', 'parse-small\noxc']);
  // Something unexpected in a scenario (here an inline table header) ends it: nothing reused.
  assert.deepEqual([...reuse.reusableTools('[[scenario]]\nid = "a"\n[odd]\n  [[scenario.tool]]\n  name = "tsc"\n')], []);
  assert.deepEqual([...reuse.reusableTools('[[scenario]]\nid = "a"\n  [[scenario.tool]]\n  name = "x"\n  ours = "yes"\n')], []);
});

const stats = (median) => ({ median, mean: median, stddev: 0.01, min: median, max: median, p95: median, runs: 5 });
const full = {
  name: 'pr', git: 'abc', os: 'linux', arch: 'x86_64',
  scenarios: [
    { id: 'parse-small', title: 'Parse', tools: [
      { name: 'ts-r', ours: true, status: 'ok', stats: stats(0.1) },
      { name: 'oxc', ours: false, status: 'ok', stats: stats(0.2), max_rss_mb: 40 },
    ] },
    { id: 'check', title: 'Check', tools: [
      { name: 'ts-r-check', ours: true, status: 'ok', stats: stats(1) },
      { name: 'tsc', ours: false, status: 'skipped', reason: 'missing input x' },
      { name: 'tsc-after-ours', ours: false, status: 'ok', stats: stats(3) },
    ] },
  ],
};

test('only reusable, measured reference tools are stored, and nothing after a failure', () => {
  const reusable = reuse.reusableTools(TOML);
  assert.deepEqual(reuse.referenceOf(full, reusable), [{ id: 'parse-small', tools: [full.scenarios[0].tools[1]] }]);
  const failed = structuredClone(full);
  failed.scenarios[0].tools[1] = { name: 'oxc', ours: false, status: 'failed', reason: 'exit 1' };
  assert.equal(reuse.referenceOf(failed, reusable), null);
});

test('runs with references that depend on our binaries still measure them on a cache hit', () => {
  const reusable = reuse.reusableTools(TOML);
  assert.equal(reuse.canReuseReference({ scenarios: [full.scenarios[0]] }, reusable), true);
  assert.equal(reuse.canReuseReference(full, reusable), false);
  const toml = fs.readFileSync(path.join(__dirname, '../../../../bench/scenarios.toml'), 'utf8');
  const repoRun = { scenarios: [{ id: 'lint-trpc-same-rules', tools: [
    { name: 'ts-r-lint', ours: true }, { name: 'eslint', ours: false }, { name: 'eslint-typed', ours: false },
  ] }] };
  assert.equal(reuse.canReuseReference(repoRun, reuse.reusableTools(toml)), false);
  assert.equal(reuse.canReuseReference(null, reusable), false);
});

test('the job checks each baseline before skipping that run\'s reference tools', () => {
  const source = fs.readFileSync(require.resolve('../box/job.mjs'), 'utf8');
  const reference = source.slice(source.indexOf('function benchReference()'), source.indexOf('\nconst steps ='));
  for (const [save, results, expected] of [['base', { scenarios: [full.scenarios[0]] }, 0], ['base-repos', full, 1], ['base', null, 1]]) {
    const proc = { argv: ['node', 'job.mjs', 'bench-reference', 'check', save] };
    vm.runInNewContext(`${reference}\nbenchReference();`, {
      process: proc, benchReuse: reuse,
      fs: { readFileSync: (file) => {
        if (file === 'bench/scenarios.toml') return TOML;
        assert.equal(file, `bench/results/${save}.json`);
        return JSON.stringify(results);
      } },
    });
    assert.equal(proc.exitCode, expected, save);
  }
});

test('an ours-only run gets the stored reference tools back, and its own tools stay', () => {
  const reusable = reuse.reusableTools(TOML);
  const oursOnly = structuredClone(full);
  for (const s of oursOnly.scenarios) {
    for (const t of s.tools) if (!t.ours) Object.assign(t, { status: 'skipped', reason: reuse.OURS_ONLY, stats: undefined, max_rss_mb: undefined });
  }
  oursOnly.scenarios[0].tools[0].stats = stats(0.15);
  const stored = [{ id: 'parse-small', tools: [{ name: 'oxc', status: 'ok', stats: stats(0.2), max_rss_mb: 40 }, { name: 'ts-r', status: 'ok', stats: stats(9) }] }];
  const { results, reused } = reuse.mergeReference(oursOnly, stored, reusable, 'not measured');
  assert.equal(reused, 1);
  const [parse, check] = results.scenarios;
  assert.deepEqual(parse.tools[0].stats, stats(0.15), 'ours is never replaced');
  assert.deepEqual(parse.tools[1], { name: 'oxc', ours: false, status: 'ok', stats: stats(0.2), max_rss_mb: 40 });
  assert.equal(check.tools[1].reason, 'not measured', 'reusable, but not in the store');
  assert.equal(check.tools[2].reason, reuse.OURS_ONLY, 'not reusable: left as the run left it');
});

test('the merged tables keep the gate paragraph of the run itself', () => {
  const rendered = '### Benchmarks (pr @ abc)\n\n| a | b |\n| x | y |\n\nGate: ... No regressions.\n';
  const original = '### Benchmarks (pr @ abc)\n\n| a |\n\nGate: ... Regressions: x/ts-r +6.0%.\n';
  assert.equal(reuse.spliceTables(rendered, original, 'Reused.'),
    '### Benchmarks (pr @ abc)\n\n| a | b |\n| x | y |\n\nGate: ... Regressions: x/ts-r +6.0%.\n\nReused.\n');
  assert.equal(reuse.spliceTables('no table', original, 'n'), null);
  assert.equal(reuse.spliceTables(rendered, 'no gate', 'n'), null);
});
