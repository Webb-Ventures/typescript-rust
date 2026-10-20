// Tests for the job's bench key: node --test .agents/plugins/code-review/ci/*.test.cjs
'use strict';

const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const { test } = require('node:test');
const vm = require('node:vm');

const source = fs.readFileSync(require.resolve('../box/job.mjs'), 'utf8');
const keySource = source.slice(source.indexOf('const BENCH_IGNORED'), source.indexOf('// The base is the tip'));
const runner = '.agents/plugins/code-review/box/';
const files = {
  'crates/ts_r_parser/src/lib.rs': 'parser',
  [`${runner}dispatcher.mjs`]: 'suite',
  [`${runner}job.mjs`]: 'commands',
  '.agents/plugins/code-review/ci/github.cjs': 'review',
  '.agents/skills/action-review/SKILL.md': 'instructions',
  'docs/design/bench.md': 'docs',
};

function key(contents) {
  const tree = Object.entries(contents).sort(([a], [b]) => a.localeCompare(b))
    .map(([path, text]) => `100644 blob ${crypto.createHash('sha1').update(text).digest('hex')}\t${path}`).join('\n');
  return vm.runInNewContext(`${keySource}\nbenchKey('rev');`, {
    crypto, PLUGIN: '.agents/plugins/code-review', env: { CACHE_DIR: '/cache' }, git: () => tree,
  });
}

test('suite selection and measurement commands invalidate a cached bench', () => {
  for (const path of [`${runner}dispatcher.mjs`, `${runner}job.mjs`]) {
    assert.notEqual(key(files), key({ ...files, [path]: 'changed' }), path);
  }
});

test('unrelated review code and instructions still reuse a cached bench', () => {
  for (const path of ['.agents/plugins/code-review/ci/github.cjs', '.agents/skills/action-review/SKILL.md', 'docs/design/bench.md']) {
    assert.equal(key(files), key({ ...files, [path]: 'changed' }), path);
  }
  assert.notEqual(key(files), key({ ...files, 'crates/ts_r_parser/src/lib.rs': 'changed' }));
});
