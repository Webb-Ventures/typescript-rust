// @ts-check
// The ts-r checker's status bar text from the lint connection (lint/status.js).
'use strict';

const test = require('node:test');
const assert = require('node:assert');
const { describeStatus } = require('../lint/status');

const matched = {
    backend: 'ts-r',
    version: '0.0.0',
    summary: 'ts-r checker on 1 project(s)',
    projects: [{ project: 'tsconfig.json', state: 'matched', parity: { result: 'match', detail: 'same 2 diagnostic(s) as TypeScript 6.0.3' } }],
};

const fallback = {
    backend: 'ts-r',
    version: '0.0.0',
    summary: 'ts-r checker on 1 of 2 project(s); 1 use your TypeScript for errors (parity difference)',
    fallbackTypeScript: 'tsserver 6.0.3',
    projects: [
        matched.projects[0],
        {
            project: 'apps/web/tsconfig.json',
            state: 'fallback',
            parity: {
                result: 'mismatch',
                detail: '1 diagnostic(s) only ts-r reports, 0 only TypeScript 6.0.3 reports',
                typescript: '6.0.3',
                ours: 3,
                theirs: 2,
                onlyOurs: ['apps/web/src/a.ts(1,14): TS2322'],
                onlyTheirs: [],
                report: '/tmp/ts-r/parity/web.txt',
            },
        },
    ],
};

test('status bar: checker, parity running, fallback', () => {
    assert.strictEqual(describeStatus(null), null);
    assert.strictEqual(describeStatus({ backend: 'tsserver' }), null);
    const ok = describeStatus(matched);
    assert.ok(ok);
    assert.strictEqual(ok.text, '$(check) ts-r');
    assert.strictEqual(ok.warning, false);
    assert.match(ok.tooltip, /tsconfig.json: matched \(same 2/);
    const running = describeStatus({ backend: 'ts-r', projects: [{ project: 'p', state: 'checking' }] });
    assert.match(running?.text || '', /parity check/);
    const fb = describeStatus(fallback);
    assert.ok(fb);
    assert.strictEqual(fb.text, '$(warning) ts-r: 1 on TypeScript');
    assert.strictEqual(fb.warning, true);
    assert.match(fb.tooltip, /Click to see and report/);
});
