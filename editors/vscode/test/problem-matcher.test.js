// @ts-check
// The `$ts-r` problem matcher (package.json) and the GitHub Actions matcher shipped in npm
// (npm/ts-r/problem-matcher.json) are the same pattern, and it reads TS and TSR errors in both
// of ts-r-check's output forms (record 0007, "Decisions", question 2).
'use strict';

const test = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const path = require('node:path');

const read = (/** @type {string} */ p) => JSON.parse(fs.readFileSync(path.join(__dirname, p), 'utf8'));
const vscode = read('../package.json').contributes.problemMatchers.find((/** @type {any} */ m) => m.name === 'ts-r');
const actions = read('../../../npm/ts-r/problem-matcher.json').problemMatcher[0].pattern[0];

test('the VS Code and GitHub Actions matchers agree', () => {
    for (const k of ['regexp', 'file', 'line', 'column', 'severity', 'code', 'message']) {
        assert.strictEqual(vscode.pattern[k], actions[k], k);
    }
});

test('TS and TSR errors match, in plain and pretty form', () => {
    const re = new RegExp(actions.regexp);
    const cases = [
        ['src/a.ts(3,7): error TS2322: Type \'string\' is not assignable to type \'number\'.', ['src/a.ts', '3', '7', 'error', 'TS2322']],
        ['src/a.ts(12,1): error TSR0201: Type assertion from \'unknown\' to \'User\' narrows the type.', ['src/a.ts', '12', '1', 'error', 'TSR0201']],
        ['src/a.ts:4:3 - error TSR1205: Overload \'f(x: number): string\' is not satisfied.', ['src/a.ts', '4', '3', 'error', 'TSR1205']],
        ['ts-r.toml(1,1): error TSR0020: Invalid sound-mode configuration: unknown key.', ['ts-r.toml', '1', '1', 'error', 'TSR0020']],
        ['src/a.ts(2,3): error TS100201: Type assertion narrows the type.', ['src/a.ts', '2', '3', 'error', 'TS100201']],
    ];
    for (const [line, want] of cases) {
        const m = re.exec(/** @type {string} */ (line));
        assert.ok(m, `no match: ${line}`);
        assert.deepStrictEqual(m.slice(1, 6), want);
    }
    assert.strictEqual(re.exec('Found 2 errors in 1 file.'), null);
});
