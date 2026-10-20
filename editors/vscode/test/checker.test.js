// @ts-check
// Status bar text for the tsserver bridge's engines, the parity issue (checker.js) and the
// tsserver <-> LSP conversions (standin/convert.js).
'use strict';

const test = require('node:test');
const assert = require('node:assert');
const { describeEngines, parityIssue } = require('../checker');
const C = require('../standin/convert');

const status = {
    backend: 'ts-r',
    version: '0.1.0',
    projects: [
        { project: 'packages/a/tsconfig.json', config: '/w/packages/a/tsconfig.json', state: 'matched', parity: { result: 'match' } },
        {
            project: 'apps/web/tsconfig.json',
            config: '/w/apps/web/tsconfig.json',
            state: 'fallback',
            parity: {
                result: 'mismatch',
                detail: '2 only TypeScript 6.0.3 reports',
                typescript: '6.0.3',
                ours: 1,
                theirs: 3,
                onlyOurs: [],
                onlyTheirs: ['apps/web/src/secret-name.tsx(4,2): TS7031', 'apps/web/src/other.ts(9,9): TS7031', 'apps/web/src/x.ts(1,1): TS2741'],
            },
        },
    ],
};

/** @type {any} */
const engines = {
    bridge: true,
    checker: 'auto',
    tsrAvailable: true,
    projects: [
        { project: 'apps/web/tsconfig.json', config: '/w/apps/web/tsconfig.json', state: 'fallback', engine: 'typescript', openFiles: 1 },
        { project: 'packages/a/tsconfig.json', config: '/w/packages/a/tsconfig.json', state: 'matched', engine: 'ts-r', openFiles: 2 },
    ],
    files: { '/w/packages/a/x.ts': 'ts-r', '/w/apps/web/src/x.ts': 'typescript' },
    typescript: { running: true, starts: 1 },
    syntax: { running: false },
    status,
};

test('status bar names the engine of the active file and every project in the tooltip', () => {
    assert.strictEqual(describeEngines(undefined, undefined), null);
    const onTsr = describeEngines(engines, '/w/packages/a/x.ts');
    assert.ok(onTsr);
    assert.strictEqual(onTsr.text, '$(check) ts-r · 1 on TypeScript');
    assert.strictEqual(onTsr.warning, false);
    assert.match(onTsr.tooltip, /packages\/a\/tsconfig.json: ts-r \(parity check passed\)/);
    assert.match(onTsr.tooltip, /apps\/web\/tsconfig.json: TypeScript \(parity difference: 2 only TypeScript 6.0.3 reports\)/);
    const onTs = describeEngines(engines, '/w/apps/web/src/x.ts');
    assert.strictEqual(onTs?.text, '$(server-process) TypeScript');
    assert.strictEqual(onTs?.warning, true);
    const none = describeEngines({ ...engines, projects: [], files: {} }, undefined);
    assert.strictEqual(none?.text, '$(check) ts-r');
    assert.match(describeEngines({ ...engines, tsrAvailable: false }, undefined)?.text || '', /TypeScript/);
});

test('parity issue: codes and counts only, never paths or messages', () => {
    assert.strictEqual(parityIssue({ projects: [status.projects[0]] }, {}), null);
    const issue = parityIssue(status, { editor: 'Code 1.137.0', platform: 'darwin-arm64' });
    assert.ok(issue);
    assert.strictEqual(issue.title, 'Parity mismatch: TS7031, TS2741');
    assert.match(issue.body, /TS7031 ×2, TS2741 ×1/);
    assert.match(issue.body, /TypeScript: 6\.0\.3/);
    assert.ok(!/secret-name|apps\/web|\.tsx|\(4,2\)/.test(issue.body), issue.body);
    assert.ok(issue.url.startsWith('https://github.com/Webb-Ventures/typescript-rust/issues/new?labels=parity&title='));
    assert.ok(!issue.url.includes('secret-name'));
});

test('text edits: tsserver order in, LSP order out', () => {
    const d = new C.TextDoc('ab\ncd\n');
    // VS Code sends multi-cursor edits reversed; tsserver applies them last to first.
    const out = d.applyTsChanges([
        { start: { line: 2, offset: 1 }, end: { line: 2, offset: 1 }, newText: 'Y' },
        { start: { line: 1, offset: 1 }, end: { line: 1, offset: 1 }, newText: 'X' },
    ]);
    assert.strictEqual(d.text, 'Xab\nYcd\n');
    assert.deepStrictEqual(out.map((c) => c.text), ['X', 'Y']);
    assert.deepStrictEqual(d.wordAt({ line: 2, offset: 3 }), { text: 'Ycd', start: { line: 2, offset: 1 }, end: { line: 2, offset: 4 } });
    assert.deepStrictEqual(d.tsPos(d.text.length), { line: 3, offset: 1 });
});

test('LSP answers become tsserver bodies', () => {
    const range = { start: { line: 0, character: 13 }, end: { line: 0, character: 16 } };
    const q = C.quickInfo({ contents: { kind: 'markdown', value: '```typescript\nconst bad: number\n```\nDocs.' }, range }, { line: 1, offset: 14 });
    assert.strictEqual(q?.displayString, 'const bad: number');
    assert.strictEqual(q?.documentation, 'Docs.');
    assert.deepStrictEqual(q?.start, { line: 1, offset: 14 });
    const diags = C.splitDiagnostics([
        { range, message: 'm', code: 2322, severity: 1, source: 'ts-r', data: { kind: 'semantic' } },
        { range, message: 's', code: 1005, severity: 1, source: 'ts-r', data: { kind: 'syntax' } },
        { range, message: 'u', code: 6133, severity: 4, source: 'ts-r', tags: [1] },
        { range, message: 'lint', code: 'no-var', severity: 2, source: 'ts-r-lint' },
        // A fallback project's diagnostic: TypeScript's own, forwarded by ts-r.
        { range, message: 'fallback', code: 2322, severity: 1, source: 'ts' },
    ]);
    assert.deepStrictEqual([diags.syntax.length, diags.semantic.length, diags.suggestion.length], [1, 2, 1]);
    assert.strictEqual(diags.suggestion[0].reportsUnnecessary, true);
    assert.deepStrictEqual(
        diags.semantic.map((/** @type {any} */ d) => d.source),
        ['ts-r', 'ts'],
    );
    const sig = C.signatureHelpBody(
        { signatures: [{ label: 'f(a: string, b?: number): void', parameters: [{ label: [2, 11] }, { label: [13, 23] }] }], activeSignature: 0, activeParameter: 1 },
        { line: 1, offset: 1 },
    );
    assert.strictEqual(sig?.items[0].prefixDisplayParts[0].text, 'f(');
    assert.strictEqual(sig?.items[0].suffixDisplayParts[0].text, '): void');
    assert.deepStrictEqual(
        sig?.items[0].parameters.map((/** @type {any} */ p) => [p.name, p.isOptional]),
        [
            ['a', false],
            ['b', true],
        ],
    );
    assert.strictEqual(sig?.argumentIndex, 1);
    const ren = C.renameBody({ range }, { changes: { 'file:///w/a.ts': [{ range, newText: `bad: ${C.RENAME_PLACEHOLDER}` }] } }, { text: 'bad', start: { line: 1, offset: 14 }, end: { line: 1, offset: 17 } });
    assert.deepStrictEqual(ren.locs, [{ file: '/w/a.ts', locs: [{ start: { line: 1, offset: 14 }, end: { line: 1, offset: 17 }, prefixText: 'bad: ' }] }]);
    const cfg = C.configDiagnostic({ code: 6053, message: "File './x.json' not found.", severity: 1, file: '/w/tsconfig.json', range }, '/w/tsconfig.json');
    assert.deepStrictEqual(cfg, { text: "File './x.json' not found.", code: 6053, category: 'error', start: { line: 1, offset: 14 }, end: { line: 1, offset: 17 } });
    // Pointing into an extended config: shown on the project's config without a range.
    assert.strictEqual(C.configDiagnostic({ code: 5023, message: 'm', severity: 1, file: '/w/base.json', range }, '/w/tsconfig.json').start, undefined);
    const doc = new C.TextDoc('let a = b;\n');
    const legend = { tokenTypes: ['variable', 'function'], tokenModifiers: ['declaration', 'readonly'] };
    const enc = C.encodedClassifications({ data: [0, 4, 1, 0, 1, 0, 4, 1, 1, 0] }, legend, doc, 0, 11);
    // variable is VS Code's type 7, function 10; declaration bit 0.
    assert.deepStrictEqual(enc.spans, [4, 1, (8 << 8) | 1, 8, 1, 11 << 8]);
});
