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
  const sizing = source.slice(source.indexOf('const BENCH_BUILD_JOB_MEMORY'), source.indexOf('// Reference tools (tsc'));
  const args = (size, env) => vm.runInNewContext(`${sizing}\n${mounts}\nmounts;`, {
    slot: { size }, SLOT: 'slot', BENCH: 'bench', dir: 'job', repo: 'o/r', cacheDir: () => 'cache',
    env, BENCH_MEMORY: env.BENCH_MEMORY, benchLanesLib: require('./bench-lanes.cjs'),
  });
  for (const size of [1, 2, 4, 8]) {
    const roomy = args(size, { BENCH_MEMORY: '64g' });
    for (const name of ['CARGO_BUILD_JOBS', 'RAYON_NUM_THREADS']) assert.ok(roomy.includes(`${name}=${Math.min(4, size)}`), `${name} ${size}`);
  }
  // The default 6g container builds fewer crates at once than its 4 cores, and measures on all 4.
  const tight = args(4, { BENCH_MEMORY: '6g' });
  assert.ok(tight.includes('RAYON_NUM_THREADS=4'));
  const jobs = Number(tight.find((a) => a.startsWith('CARGO_BUILD_JOBS=')).split('=')[1]);
  assert.ok(jobs >= 1 && jobs < 4, String(jobs));
  assert.ok(args(4, { BENCH_MEMORY: '6g', BENCH_BUILD_JOB_MEMORY: '1g' }).includes('CARGO_BUILD_JOBS=4'));
  assert.ok(args(4, { BENCH_MEMORY: '6g', BENCH_BUILD_JOB_MEMORY: '6g' }).includes('CARGO_BUILD_JOBS=1'));
});

test('a build directory that moved with its checkout is dropped, its data kept', () => {
  const os = require('node:os');
  const path = require('node:path');
  const job = fs.readFileSync(require.resolve('../box/job.mjs'), 'utf8');
  const helper = job.slice(job.indexOf('const MOVED_KEPT'), job.indexOf('function benchCheckout'));
  const logs = [];
  const drop = vm.runInNewContext(`${helper}\ndropMovedBuilds;`, { fs, path, console: { log: (m) => logs.push(m) } });
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'bench-moved-'));
  try {
    const listing = (checkout) => fs.readdirSync(`${checkout}/target`).sort();
    // The single bench directory of before, its builds made without a recorded path.
    for (const dir of ['debug', 'release', 'bench-repos', 'upstream']) fs.mkdirSync(`${root}/pr/target/${dir}`, { recursive: true });
    fs.mkdirSync(`${root}/slot-0`);
    fs.renameSync(`${root}/pr`, `${root}/slot-0/pr`);
    const checkout = `${root}/slot-0/pr`;
    drop(checkout);
    assert.deepEqual(listing(checkout), ['.checkout-path', 'bench-repos', 'upstream']);
    assert.match(logs.pop(), /dropped \(debug, release\)/);
    // Built where it is: kept.
    fs.mkdirSync(`${checkout}/target/release`);
    drop(checkout);
    assert.deepEqual(listing(checkout), ['.checkout-path', 'bench-repos', 'release', 'upstream']);
    assert.equal(logs.length, 0);
    // Moved again: dropped, and the new path recorded.
    fs.renameSync(`${root}/slot-0`, `${root}/slot-1`);
    drop(`${root}/slot-1/pr`);
    assert.deepEqual(listing(`${root}/slot-1/pr`), ['.checkout-path', 'bench-repos', 'upstream']);
    assert.equal(fs.readFileSync(`${root}/slot-1/pr/target/.checkout-path`, 'utf8'), `${root}/slot-1/pr\n`);
    // No build directory yet: nothing to do.
    drop(`${root}/none`);
    assert.equal(fs.existsSync(`${root}/none`), false);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});
