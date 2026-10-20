// Unit tests for the @ts-r.dev/typescript `tsc` (npm/typescript/lib/tsc.cjs).
// Run with: node --test npm/tests/*.test.cjs
'use strict';

const assert = require('node:assert');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');

const { readConfig } = require('../typescript/lib/tsc.cjs');

function tempDir() {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'ts-r-tsc-'));
}

test('readConfig reads a base extended along many paths once', () => {
  // c0 extends c1 twice, c1 extends c2 twice, ...: 2^31 paths to c31 if each one is followed.
  const dir = tempDir();
  const n = 31;
  for (let i = 0; i < n; i++) {
    const next = `./c${i + 1}.json`;
    fs.writeFileSync(path.join(dir, `c${i}.json`), JSON.stringify({ extends: [next, next] }));
  }
  fs.writeFileSync(path.join(dir, `c${n}.json`), JSON.stringify({ compilerOptions: { noEmit: true } }));
  const read = readConfig(path.join(dir, 'c0.json'));
  assert.ok(read);
  assert.strictEqual(read.options.noEmit, true);
  assert.strictEqual(read.files.length, n + 1);
});

test('readConfig merges a shared base in tsc order', () => {
  const dir = tempDir();
  fs.writeFileSync(path.join(dir, 'base.json'), JSON.stringify({ compilerOptions: { strict: true, noEmit: true } }));
  fs.writeFileSync(path.join(dir, 'a.json'), JSON.stringify({ extends: './base.json', compilerOptions: { strict: false } }));
  fs.writeFileSync(path.join(dir, 'tsconfig.json'), JSON.stringify({ extends: ['./a.json', './base.json'] }));
  const read = readConfig(path.join(dir, 'tsconfig.json'));
  assert.deepStrictEqual(read.options, { strict: true, noEmit: true });
});

test('readConfig rejects a circular chain', () => {
  const dir = tempDir();
  fs.writeFileSync(path.join(dir, 'a.json'), JSON.stringify({ extends: './b.json' }));
  fs.writeFileSync(path.join(dir, 'b.json'), JSON.stringify({ extends: './a.json' }));
  assert.strictEqual(readConfig(path.join(dir, 'a.json')), null);
});

test('tsc passes the tsconfig pretty setting on to ts-r-check', { skip: process.platform === 'win32' }, () => {
  // A stand-in ts-r-check that prints its arguments; ts-r-check itself ignores tsconfig's pretty.
  const dir = tempDir();
  const bin = path.join(dir, 'bin');
  fs.mkdirSync(bin);
  fs.writeFileSync(path.join(bin, 'ts-r-check'), '#!/bin/sh\necho "$@"\n', { mode: 0o755 });
  fs.writeFileSync(path.join(dir, 'tsconfig.json'), JSON.stringify({ compilerOptions: { noEmit: true, pretty: false } }));
  const tsc = path.join(__dirname, '..', 'typescript', 'lib', 'tsc.cjs');
  const out = spawnSync(process.execPath, [tsc], {
    cwd: dir,
    encoding: 'utf8',
    env: { ...process.env, TS_R_TSC: 'ts-r', TS_R_BIN_DIR: bin },
  });
  assert.strictEqual(out.status, 0, out.stderr);
  assert.match(out.stdout, /--pretty false/);
});
