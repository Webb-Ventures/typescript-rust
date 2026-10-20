// @ts-check
// The extension's lint pieces without VS Code: the `ts-r.lint.enable` decision (coexistence
// with the ESLint extension) and the lint-only LSP connection (standin/lspclient.js) against a
// real ts-r-lsp binary, the way lint/client.js drives it.
//
//   cargo build -p ts_r_lsp            # or set TS_R_LSP_BIN
//   npm test
'use strict';

const test = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const os = require('os');
const path = require('path');
const childProcess = require('child_process');
const { resolveLintEnable } = require('../lint/mode');
const { LspClient } = require('../standin/lspclient');

const repo = path.resolve(__dirname, '..', '..', '..');
const exe = process.platform === 'win32' ? 'ts-r-lsp.exe' : 'ts-r-lsp';
const bin = [
    process.env.TS_R_LSP_BIN,
    path.join(repo, 'target', 'lsp', 'debug', exe),
    path.join(repo, 'target', 'debug', exe),
    path.join(repo, 'target', 'lsp', 'release', exe),
].find((p) => p && fs.existsSync(p));

test('auto turns lint off while the ESLint extension is enabled', () => {
    const withEslint = { installed: true, eslintEnable: true };
    const eslintOff = { installed: true, eslintEnable: false };
    const none = { installed: false, eslintEnable: true };
    assert.strictEqual(resolveLintEnable('auto', withEslint).lint, false);
    assert.match(resolveLintEnable('auto', withEslint).reason, /dbaeumer\.vscode-eslint/);
    assert.strictEqual(resolveLintEnable('auto', eslintOff).lint, true);
    assert.strictEqual(resolveLintEnable('auto', none).lint, true);
    // Opting in wins over the ESLint extension; opting out wins over everything.
    assert.strictEqual(resolveLintEnable('on', withEslint).lint, true);
    assert.strictEqual(resolveLintEnable('off', none).lint, false);
    // Unknown values (e.g. L1's "syntax") behave like auto.
    assert.strictEqual(resolveLintEnable('syntax', withEslint).lint, false);
    assert.strictEqual(resolveLintEnable(undefined, none).lint, true);
});

/** @param {string} name */
function workspace(name) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), `tsr-lint-${name}-`));
    const proj = path.join(dir, 'proj');
    fs.mkdirSync(proj, { recursive: true });
    fs.writeFileSync(path.join(proj, '.oxlintrc.json'), JSON.stringify({ categories: { correctness: 'off' }, rules: { 'no-var': 'error', 'react/jsx-key': 'error' } }));
    fs.writeFileSync(path.join(proj, 'a.ts'), 'var x = 1;\nexport { x };\n');
    const root = fs.realpathSync(proj);
    /** @type {NodeJS.ProcessEnv} */
    const env = { ...process.env, TS_R_CACHE_DIR: path.join(dir, 'cache'), TS_R_LSP_LINGER_MS: '300', TS_R_LSP_LINT_DEBOUNCE_MS: '0' };
    delete env.TS_R_WORKSPACE_ROOT;
    const cleanup = () => {
        if (bin) childProcess.spawnSync(bin, ['stop', '--root', root], { env });
        fs.rmSync(dir, { recursive: true, force: true });
    };
    return { root, env, cleanup };
}

/**
 * Resolves with the first notification `method` whose params satisfy `pred`.
 * @param {LspClient} c @param {string} method @param {(p: any) => boolean} pred
 * @returns {Promise<any>}
 */
function next(c, method, pred) {
    return new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error(`timed out waiting for ${method}`)), 10_000);
        c.onNotification(method, (p) => {
            if (pred(p)) {
                clearTimeout(timer);
                resolve(p);
            }
        });
    });
}

test('lint-only connection: diagnostics, edits, code actions, no TypeScript needed', { skip: !bin && 'build ts-r-lsp first' }, async () => {
    const ws = workspace('conn');
    const c = new LspClient(/** @type {string} */ (bin), ['--lint-only', '--root', ws.root, '--name', 'test lint'], { cwd: ws.root, env: ws.env });
    try {
        const rootUri = `file://${ws.root}`;
        const init = await c.request('initialize', {
            processId: process.pid,
            rootUri,
            capabilities: {},
            initializationOptions: { lintOnly: true, 'ts-r': { lint: { enable: 'on' } } },
        });
        assert.deepStrictEqual(init.capabilities.codeActionProvider.codeActionKinds, ['quickfix', 'source.fixAll.ts-r']);
        c.notify('initialized', {});
        const uri = `${rootUri}/a.ts`;
        const note = next(c, 'window/showMessage', () => true);
        const first = next(c, 'textDocument/publishDiagnostics', (p) => p.uri === uri && p.diagnostics.length === 1);
        c.notify('textDocument/didOpen', { textDocument: { uri, languageId: 'typescript', version: 1, text: 'var x = 1;\nexport { x };\n' } });
        const d = (await first).diagnostics[0];
        assert.strictEqual(d.code, 'no-var');
        assert.strictEqual(d.source, 'ts-r-lint');
        assert.strictEqual((await note).type, 3, 'unsupported rules are an info message');

        const actions = await c.request('textDocument/codeAction', {
            textDocument: { uri },
            range: { start: { line: 0, character: 1 }, end: { line: 0, character: 1 } },
            context: { diagnostics: [d] },
        });
        assert.deepStrictEqual(
            actions.map((/** @type {any} */ a) => a.title),
            ['Fix this no-var problem', 'Disable no-var for this line', 'Disable no-var for the entire file'],
        );
        // Apply the fix the way VS Code would, then see the problem go.
        const fix = actions[0].edit.changes[uri][0];
        const cleared = next(c, 'textDocument/publishDiagnostics', (p) => p.uri === uri && p.diagnostics.length === 0);
        c.notify('textDocument/didChange', { textDocument: { uri, version: 2 }, contentChanges: [{ range: fix.range, text: fix.newText }] });
        await cleared;
    } finally {
        await c.stop();
        ws.cleanup();
    }
});
