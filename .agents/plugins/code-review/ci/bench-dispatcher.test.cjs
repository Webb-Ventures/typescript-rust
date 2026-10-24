// Tests for optional reference storage and slot limits: node --test .agents/plugins/code-review/ci/*.test.cjs
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const { test } = require('node:test');
const vm = require('node:vm');

const source = fs.readFileSync(require.resolve('../box/dispatcher.mjs'), 'utf8');
const store = source.slice(source.indexOf('const SAFE_NAME'), source.indexOf('// The binaries the PR'));
const stats = { median: 1, mean: 1, stddev: 0, min: 1, max: 1, p95: 1, runs: 5 };
const reference = JSON.stringify({ main: [{ id: 'parse', tools: [{ name: 'oxc', status: 'ok', stats }] }], repos: [] });

function save(text, fail) {
  const writes = [];
  const logs = [];
  const io = Object.fromEntries(['mkdirSync', 'writeFileSync', 'renameSync', 'readdirSync', 'statSync', 'rmSync'].map((method) => [method, (...args) => {
    if (fail === method) throw new Error('cache unavailable');
    if (method === 'writeFileSync') writes.push(JSON.parse(args[1]));
    if (method === 'readdirSync') return Array.from({ length: 51 }, (_, i) => `${i}.json`);
    if (method === 'statSync') return { mtimeMs: 0 };
  }]));
  vm.runInNewContext(`${store}\nstoreReference('o/r', 'key', 'out/reference.json', 'abc');`, {
    fs: io, readOutFile: () => text, referenceDir: () => 'reference', log: (message) => logs.push(message),
  });
  return { writes, logs };
}

test('missing, null and malformed reference output is ignored', () => {
  for (const text of [null, 'null', '', '{', '[]', '{}', '{"main":[]}']) {
    assert.deepEqual(save(text).writes, [], String(text));
  }
});

test('valid reference output is stored, and cache failures never escape', () => {
  const { writes } = save(reference);
  assert.equal(writes.length, 1);
  assert.equal(writes[0].sha, 'abc');
  assert.deepEqual(writes[0].main, JSON.parse(reference).main);
  for (const method of ['mkdirSync', 'writeFileSync', 'renameSync', 'readdirSync', 'statSync', 'rmSync']) {
    assert.match(save(reference, method).logs[0], /cache unavailable/, method);
  }
});

test('worker counts follow small slots and stay capped at four on larger slots', () => {
  const mounts = source.slice(source.indexOf('  const mounts ='), source.indexOf('  const heavy ='));
  for (const size of [1, 2, 4, 8]) {
    const args = vm.runInNewContext(`${mounts}\nmounts;`, {
      slot: { size }, SLOT: 'slot', BENCH: 'bench', dir: 'job', repo: 'o/r', cacheDir: () => 'cache',
    });
    for (const name of ['CARGO_BUILD_JOBS', 'RAYON_NUM_THREADS']) assert.ok(args.includes(`${name}=${Math.min(4, size)}`));
  }
});
