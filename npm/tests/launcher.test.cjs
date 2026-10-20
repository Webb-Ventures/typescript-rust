// Unit tests for the npm bin shim (npm/ts-r/bin/ts-r.js).
// Run with: node --test npm/tests/*.test.cjs
'use strict';

const assert = require('node:assert');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');

test('the source-tree launcher runs the ts-r in TS_R_BIN_DIR', { skip: process.platform === 'win32' }, () => {
  // npm/ts-r/package.json has no "ts-r" key until build-packages.mjs adds one.
  const bin = fs.mkdtempSync(path.join(os.tmpdir(), 'ts-r-launcher-'));
  fs.writeFileSync(path.join(bin, 'ts-r'), '#!/bin/sh\necho "ts-r $@"\n', { mode: 0o755 });
  const out = spawnSync(process.execPath, [path.join(__dirname, '..', 'ts-r', 'bin', 'ts-r.js'), '--version'], {
    encoding: 'utf8',
    env: { ...process.env, TS_R_BIN_DIR: bin },
  });
  assert.strictEqual(out.status, 0, out.stderr);
  assert.strictEqual(out.stdout, 'ts-r --version\n');
});
