// @ts-check
// The tsserver bridge (standin/router.js, `ts-r.checker` auto/on) driven like VS Code's
// TypeScript extension drives tsserver (fork + IPC), against a real ts-r-lsp: the ts-r checker
// with the repository's lib files, a fake tsc for the parity check and the fake tsserver
// backend standing in for the user's TypeScript.
//
//   cargo build -p ts_r_lsp     # or set TS_R_LSP_BIN; lib files from target/upstream
//   npm test
'use strict';

const test = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const os = require('os');
const path = require('path');
const childProcess = require('child_process');

const repo = path.resolve(__dirname, '..', '..', '..');
const exe = process.platform === 'win32' ? 'ts-r-lsp.exe' : 'ts-r-lsp';
const bin = [
    process.env.TS_R_LSP_BIN,
    path.join(repo, 'target', 'debug', exe),
    path.join(repo, 'target', 'release', exe),
    path.join(repo, 'target', 'lsp', 'debug', exe),
].find((p) => p && fs.existsSync(p));
const libs = [process.env.TS_R_LIB_DIR, path.join(repo, 'target', 'upstream', 'typescript', 'tsc', 'internal', 'bundled', 'libs')].find(
    (d) => d && fs.existsSync(path.join(d, 'lib.es5.d.ts')),
);
const skip = !bin ? 'build ts-r-lsp first' : !libs ? 'no TypeScript lib files (target/upstream or TS_R_LIB_DIR)' : false;

const GOOD = 'export const bad: number = "x";\n';
const BAD = 'export const other: string = 1;\n';
/** tsc's line for GOOD: the parity check of `good/` matches; `bad/` gets no lines, so differs. */
const TSC_LINE = "good/a.ts(1,14): error TS2322: Type 'string' is not assignable to type 'number'.";

/** A workspace with two projects (`good` passes parity, `bad` does not) and a stand-in tsdk. */
function setup(/** @type {string} */ name, /** @type {string} */ cacheDir) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), `tsr-bridge-${name}-`));
    const ws = path.join(dir, 'ws');
    const tsdk = path.join(dir, 'tsdk');
    const lib = path.join(tsdk, 'lib');
    const bundled = path.join(dir, 'bundled', 'lib');
    const files = {
        'good/tsconfig.json': '{"compilerOptions": {"strict": true, "noEmit": true}}',
        'good/a.ts': GOOD,
        'bad/tsconfig.json': '{"compilerOptions": {"strict": true, "noEmit": true}}',
        'bad/b.ts': BAD,
        'node_modules/typescript/package.json': '{"version": "6.0.0"}',
        'node_modules/typescript/lib/tsserver.js': '',
    };
    for (const [rel, text] of Object.entries(files)) {
        fs.mkdirSync(path.dirname(path.join(ws, rel)), { recursive: true });
        fs.writeFileSync(path.join(ws, rel), text);
    }
    fs.mkdirSync(lib, { recursive: true });
    for (const f of fs.readdirSync(path.join(__dirname, '..', 'standin'))) {
        if (f.endsWith('.js')) fs.copyFileSync(path.join(__dirname, '..', 'standin', f), path.join(lib, f));
    }
    fs.writeFileSync(path.join(tsdk, 'package.json'), JSON.stringify({ name: 'typescript', version: '5.9.3' }));
    fs.writeFileSync(
        path.join(tsdk, 'ts-r.json'),
        JSON.stringify({ bin, fallback: bundled, checker: 'auto', settings: { memoryBudgetMB: 900, idleShutdownMinutes: 5, typeBackend: 'typescript' } }),
    );
    // VS Code's bundled TypeScript: the syntax server answers everything with "bundled".
    fs.mkdirSync(bundled, { recursive: true });
    fs.writeFileSync(path.join(bundled, '..', 'package.json'), JSON.stringify({ name: 'typescript', version: '5.9.3' }));
    fs.writeFileSync(
        path.join(bundled, 'tsserver.js'),
        "process.on('message', (m) => m.command !== 'exit' ? process.send({ seq: 0, type: 'response', request_seq: m.seq, command: m.command, success: true, body: 'bundled' }) : process.exit(0));\n",
    );
    fs.writeFileSync(path.join(bundled, 'typescript.js'), 'exports.getSupportedCodeFixes = () => ["2304", "2322"];\n');
    const tsc = path.join(dir, 'fake-tsc.sh');
    fs.writeFileSync(tsc, `#!/bin/sh\ncase "$2" in *good*) printf '%s\\n' "${TSC_LINE.replace(/"/g, '\\"')}";; esac\nexit 2\n`);
    const root = fs.realpathSync(ws);
    const env = {
        ...process.env,
        TS_R_CACHE_DIR: cacheDir,
        TS_R_WORKSPACE_ROOT: root,
        TS_R_LIB_DIR: libs,
        TS_R_LSP_PARITY: 'on',
        TS_R_LSP_PARITY_TSC: JSON.stringify(['/bin/sh', tsc]),
        TS_R_LSP_TSSERVER_CMD: JSON.stringify([bin, '__fake-backend', 'tsserver']),
        TS_R_LSP_LINGER_MS: '500',
        TS_R_BRIDGE_IDLE_MS: '1500',
    };
    const cleanup = () => {
        childProcess.spawnSync(/** @type {string} */ (bin), ['stop', '--root', root], { env });
        if (!process.env.TS_R_TEST_KEEP) fs.rmSync(dir, { recursive: true, force: true });
    };
    return { dir, root, lib, env, cleanup };
}

/** @type {childProcess.ChildProcess[]} */
const children = [];
test.afterEach(() => {
    for (const c of children.splice(0)) c.kill();
});

/** VS Code's IPC mode, collecting responses and events. */
function forkIpc(/** @type {ReturnType<typeof setup>} */ s) {
    const child = childProcess.fork(path.join(s.lib, 'tsserver.js'), ['--useInferredProjectPerProjectRoot', '--locale', 'en', '--useNodeIpc'], {
        env: s.env,
        stdio: ['pipe', 'pipe', 'pipe', 'ipc'],
    });
    children.push(child);
    /** @type {string[]} */
    const log = [];
    child.stderr?.on('data', (d) => log.push(String(d)));
    /** @type {any[]} */
    const inbox = [];
    /** @type {(() => void)[]} */
    const waiters = [];
    child.on('message', (m) => {
        inbox.push(m);
        waiters.splice(0).forEach((w) => w());
    });
    let seq = 1;
    /** @param {(m: any) => boolean} pred @param {number} ms */
    const waitFor = async (pred, ms = 15000) => {
        const end = Date.now() + ms;
        for (;;) {
            const i = inbox.findIndex(pred);
            if (i >= 0) return inbox.splice(i, 1)[0];
            if (Date.now() > end) throw new Error(`timed out; stderr:\n${log.join('')}`);
            await new Promise((r) => {
                waiters.push(() => r(undefined));
                setTimeout(r, 100);
            });
        }
    };
    return {
        child,
        inbox,
        waitFor,
        /** @returns {number} */
        send(/** @type {string} */ command, /** @type {unknown} */ args) {
            const id = seq++;
            child.send({ seq: id, type: 'request', command, arguments: args });
            return id;
        },
        /** @returns {Promise<any>} */
        call(/** @type {string} */ command, /** @type {unknown} */ args) {
            const id = this.send(command, args);
            return waitFor((m) => m.type === 'response' && m.request_seq === id);
        },
        /** Polls `_ts-r.engines` until `pred` holds. */
        async engines(/** @type {(e: any) => boolean} */ pred, ms = 20000) {
            const end = Date.now() + ms;
            for (;;) {
                const r = await this.call('_ts-r.engines', {});
                if (!r.body?.bridge) throw new Error(`no bridge: ${JSON.stringify(r)}\nstderr:\n${log.join('')}`);
                if (pred(r.body)) return r.body;
                if (Date.now() > end) throw new Error(`engines: ${JSON.stringify(r.body)}\nstderr:\n${log.join('')}`);
                await new Promise((res) => setTimeout(res, 200));
            }
        },
        async exit() {
            const gone = new Promise((r) => child.on('exit', r));
            child.send({ seq: seq++, type: 'request', command: 'exit' });
            await Promise.race([gone, new Promise((r) => setTimeout(r, 5000))]);
            child.kill();
        },
    };
}

/** Stops the workspace daemon and waits until it is gone. */
async function stopDaemon(/** @type {ReturnType<typeof setup>} */ s) {
    const st = childProcess.spawnSync(/** @type {string} */ (bin), ['status', '--root', s.root], { env: s.env, encoding: 'utf8' });
    let pid = 0;
    try {
        pid = JSON.parse(st.stdout).daemonPid || JSON.parse(st.stdout).pid || 0;
    } catch {
        // No daemon.
    }
    childProcess.spawnSync(/** @type {string} */ (bin), ['stop', '--root', s.root], { env: s.env });
    for (let i = 0; pid && i < 50; i++) {
        try {
            process.kill(pid, 0);
        } catch {
            return;
        }
        await new Promise((res) => setTimeout(res, 200));
    }
}

const engineOf = (/** @type {any} */ e, /** @type {string} */ dir) => e.projects.find((/** @type {any} */ p) => p.project.startsWith(`${dir}/`))?.engine;

test('auto: a project that passes parity is served by ts-r, one that fails by TypeScript', { skip, timeout: 90000 }, async () => {
    const cache = fs.mkdtempSync(path.join(os.tmpdir(), 'tsr-bridge-cache-'));
    const s = setup('auto', cache);
    try {
        const good = path.join(s.root, 'good', 'a.ts');
        const bad = path.join(s.root, 'bad', 'b.ts');
        const vs = forkIpc(s);
        vs.send('configure', { preferences: {} });
        vs.send('updateOpen', {
            openFiles: [
                { file: good, fileContent: GOOD, scriptKindName: 'TS', projectRootPath: s.root },
                { file: bad, fileContent: BAD, scriptKindName: 'TS', projectRootPath: s.root },
            ],
        });
        const e = await vs.engines((x) => engineOf(x, 'good') === 'ts-r' && engineOf(x, 'bad') === 'typescript');
        assert.strictEqual(e.checker, 'auto');
        assert.strictEqual(e.typescript.running, true, 'TypeScript runs for the project that failed parity');

        // Diagnostics: ts-r's for good/a.ts, the fake tsserver's for bad/b.ts, never both.
        const seq = vs.send('geterr', { files: [good, bad], delay: 0 });
        const goodDiag = await vs.waitFor((m) => m.event === 'semanticDiag' && m.body.file === good && m.body.diagnostics.length > 0);
        assert.strictEqual(goodDiag.body.diagnostics[0].source, 'ts-r');
        assert.strictEqual(goodDiag.body.diagnostics[0].code, 2322);
        assert.deepStrictEqual(goodDiag.body.diagnostics[0].start, { line: 1, offset: 14 });
        const badDiag = await vs.waitFor((m) => m.event === 'semanticDiag' && m.body.file === bad);
        assert.strictEqual(badDiag.body.diagnostics[0].code, 9999, 'the fake tsserver answered for bad/b.ts');
        await vs.waitFor((m) => m.event === 'requestCompleted' && m.body.request_seq === seq);
        await new Promise((r) => setTimeout(r, 300));
        assert.ok(!vs.inbox.some((m) => /Diag$/.test(m.event ?? '') && m.body.file === bad && m.body.diagnostics.some((/** @type {any} */ d) => d.source === 'ts-r')));
        assert.ok(!vs.inbox.some((m) => /Diag$/.test(m.event ?? '') && m.body.file === good && m.body.diagnostics.some((/** @type {any} */ d) => d.code === 9999)));

        // Hover and completions in good/a.ts come from ts-r, in bad/b.ts from TypeScript.
        const q = await vs.call('quickinfo', { file: good, line: 1, offset: 14 });
        assert.strictEqual(q.success, true);
        assert.match(q.body.displayString, /bad: number/);
        const qb = await vs.call('quickinfo', { file: bad, line: 1, offset: 14 });
        assert.match(qb.body.displayString, /^pid=/);
        vs.send('updateOpen', { changedFiles: [{ fileName: good, textChanges: [{ start: { line: 2, offset: 1 }, end: { line: 2, offset: 1 }, newText: 'bad.to' }] }] });
        const c = await vs.call('completionInfo', { file: good, line: 2, offset: 7 });
        const names = c.body.entries.map((/** @type {any} */ x) => x.name);
        assert.ok(names.includes('toFixed'), JSON.stringify(names));
        assert.strictEqual(c.body.isMemberCompletion, true);
        const details = await vs.call('completionEntryDetails', { file: good, line: 2, offset: 7, entryNames: [c.body.entries.find((/** @type {any} */ x) => x.name === 'toFixed')] });
        assert.match(details.body[0].displayParts.map((/** @type {any} */ p) => p.text).join(''), /toFixed/);
        const def = await vs.call('definitionAndBoundSpan', { file: good, line: 2, offset: 2 });
        assert.deepStrictEqual(def.body.definitions[0].start, { line: 1, offset: 14 });
        assert.deepStrictEqual(def.body.textSpan, { start: { line: 2, offset: 1 }, end: { line: 2, offset: 4 } });

        // Syntax-only requests for a ts-r file go to the syntax server, folding is VS Code's.
        const f = await vs.call('format', { file: good, line: 1, offset: 1, endLine: 2, endOffset: 1 });
        assert.strictEqual(f.body, 'bundled');
        const fold = await vs.call('getOutliningSpans', { file: good });
        assert.strictEqual(fold.success, true);
        assert.strictEqual(fold.body, undefined);
        // What ts-r cannot answer goes to TypeScript, with the file opened there first.
        const fix = await vs.call('getCodeFixes', { file: good, startLine: 1, startOffset: 14, endLine: 1, endOffset: 17, errorCodes: [2322] });
        assert.strictEqual(fix.success, false);
        assert.match(fix.message, /Unrecognized JSON command: getCodeFixes/);

        // `on`: every project on ts-r, switched without a restart.
        const on = await vs.call('_ts-r.configure', { checker: 'on' });
        assert.strictEqual(on.body.ok, true);
        await vs.engines((x) => engineOf(x, 'bad') === 'ts-r');
        await vs.exit();
    } finally {
        await stopDaemon(s); // the daemon writes its log while exiting: wait before removing
        s.cleanup();
        if (!process.env.TS_R_TEST_KEEP) fs.rmSync(cache, { recursive: true, force: true });
    }
});

test('auto: with the parity result cached, a ts-r project never starts TypeScript', { skip, timeout: 90000 }, async () => {
    const cache = fs.mkdtempSync(path.join(os.tmpdir(), 'tsr-bridge-cache-'));
    const s = setup('cached', cache);
    try {
        const good = path.join(s.root, 'good', 'a.ts');
        // First window: computes and caches the parity result; then the daemon stops.
        const first = forkIpc(s);
        first.send('updateOpen', { openFiles: [{ file: good, fileContent: GOOD, scriptKindName: 'TS' }] });
        await first.engines((x) => engineOf(x, 'good') === 'ts-r');
        await first.exit();
        await stopDaemon(s);

        // Second window, new daemon: the cached result routes the file to ts-r at once.
        const vs = forkIpc(s);
        vs.send('updateOpen', { openFiles: [{ file: good, fileContent: GOOD, scriptKindName: 'TS' }] });
        const e = await vs.engines((x) => engineOf(x, 'good') === 'ts-r');
        assert.strictEqual(e.typescript.starts, 0, 'TypeScript never started');
        assert.strictEqual(e.status.projects.find((/** @type {any} */ p) => p.project.startsWith('good/')).parity.cached, true);
        const seq = vs.send('geterr', { files: [good], delay: 0 });
        const d = await vs.waitFor((m) => m.event === 'semanticDiag' && m.body.file === good && m.body.diagnostics.length > 0);
        assert.strictEqual(d.body.diagnostics[0].source, 'ts-r');
        await vs.waitFor((m) => m.event === 'requestCompleted' && m.body.request_seq === seq);
        const fixes = await vs.call('getSupportedCodeFixes', null);
        assert.deepStrictEqual(fixes.body, ['2304', '2322']);
        assert.strictEqual((await vs.call('_ts-r.engines', {})).body.typescript.starts, 0);
        // Implicit refactors (every cursor move) are ts-r's, and so are their edits, organize
        // imports and import updates on a file rename: no TypeScript.
        const implicit = await vs.call('getApplicableRefactors', { file: good, startLine: 1, startOffset: 1, endLine: 1, endOffset: 1, triggerReason: 'implicit' });
        assert.ok(
            implicit.body.some((/** @type {any} */ r) => r.name === 'Move to a new file'),
            JSON.stringify(implicit.body),
        );
        const moved = await vs.call('getEditsForRefactor', {
            file: good, startLine: 1, startOffset: 1, endLine: 1, endOffset: 1, refactor: 'Move to a new file', action: 'Move to a new file',
        });
        assert.ok(
            moved.body.edits.some((/** @type {any} */ e) => e.isNewFile && e.fileName === path.join(s.root, 'good', 'bad.ts')),
            JSON.stringify(moved.body),
        );
        const organized = await vs.call('organizeImports', { scope: { type: 'file', args: { file: good } } });
        assert.deepStrictEqual(organized.body, []);
        const renamed = await vs.call('getEditsForFileRename', { oldFilePath: good, newFilePath: path.join(s.root, 'good', 'c.ts') });
        assert.deepStrictEqual(renamed.body, []);
        assert.strictEqual((await vs.call('_ts-r.engines', {})).body.typescript.starts, 0);
        // The refactor menu is ts-r's too, with Move to file: its target suggestions and, once
        // the user picked a target, its edits.
        const span = { file: good, startLine: 1, startOffset: 1, endLine: 1, endOffset: 5 };
        const menu = await vs.call('getApplicableRefactors', { ...span, triggerReason: 'invoked', includeInteractiveActions: true });
        const names = menu.body.map((/** @type {any} */ r) => r.name);
        assert.ok(names.includes('Move to file') && names.includes('Move to a new file'), JSON.stringify(menu.body));
        const suggested = await vs.call('getMoveToRefactoringFileSuggestions', span);
        assert.deepStrictEqual(suggested.body, { newFileName: path.join(s.root, 'good', 'bad.ts'), files: [] });
        const target = path.join(s.root, 'good', 'lib.ts');
        const toFile = await vs.call('getEditsForRefactor', {
            ...span, refactor: 'Move to file', action: 'Move to file', interactiveRefactorArguments: { targetFile: target },
        });
        assert.ok(
            toFile.body.edits.some((/** @type {any} */ e) => e.fileName === target && e.textChanges.some((/** @type {any} */ t) => t.newText.includes('export const bad'))),
            JSON.stringify(toFile.body),
        );
        assert.strictEqual((await vs.call('_ts-r.engines', {})).body.typescript.starts, 0);
        // A request ts-r cannot answer (inlay hints) starts TypeScript on demand; it stops again
        // when idle.
        await vs.call('provideInlayHints', { file: good, start: 0, length: GOOD.length });
        await vs.engines((x) => x.typescript.running === true);
        await vs.engines((x) => x.typescript.running === false && x.typescript.starts === 1, 15000);
        await vs.exit();
    } finally {
        await stopDaemon(s); // the daemon writes its log while exiting: wait before removing
        s.cleanup();
        if (!process.env.TS_R_TEST_KEEP) fs.rmSync(cache, { recursive: true, force: true });
    }
});

test('TypeScript, started for a ts-r file, sees the unsaved text of the project\'s other open files', async () => {
    const { Router } = require('../standin/router');
    /** @type {any[]} */
    const toT = [];
    class Stubbed extends Router {
        startR() {
            this.rReady = Promise.resolve();
            return /** @type {any} */ ({ request: async () => ({ config: '/w/tsconfig.json', state: 'matched' }), notify() {} });
        }
        ensureT() {
            if (!this.T) this.T = /** @type {any} */ ({ send: (/** @type {any} */ m) => toT.push(m), stop() {} });
            return /** @type {any} */ (this.T);
        }
    }
    const r = new Stubbed({ bin: '', common: [], name: 'test', args: [], mode: 'auto', tsdkDir: '', send() {}, log() {}, exit() {} });
    const req = (/** @type {string} */ command, /** @type {any} */ args) => r.handle({ seq: 1, type: 'request', command, arguments: args });
    const [a, b, c] = ['/w/a.ts', '/w/b.ts', '/w/c.ts'];
    await req('updateOpen', {
        openFiles: [
            { file: a, fileContent: 'import { n } from "./b";\n', scriptKindName: 'TS' },
            { file: b, fileContent: 'export const n = 1;\n', scriptKindName: 'TS' },
        ],
    });
    await req('updateOpen', { changedFiles: [{ fileName: b, textChanges: [{ start: { line: 1, offset: 18 }, end: { line: 1, offset: 19 }, newText: '"x"' }] }] });
    assert.strictEqual(toT.length, 0, 'ts-r serves both files');

    // A request only TypeScript answers (inlay hints) in a.ts: b.ts goes along with its edit.
    await req('provideInlayHints', { file: a, start: 0, length: 24 });
    const open = toT.find((m) => m.command === 'updateOpen');
    assert.deepStrictEqual(
        open.arguments.openFiles.map((/** @type {any} */ f) => [f.file, f.fileContent]),
        [
            [a, 'import { n } from "./b";\n'],
            [b, 'export const n = "x";\n'],
        ],
    );
    assert.strictEqual(toT.at(-1).command, 'provideInlayHints');

    // Later edits, and files of the project opened later, reach TypeScript too.
    toT.length = 0;
    await req('updateOpen', {
        openFiles: [{ file: c, fileContent: 'export {};\n', scriptKindName: 'TS' }],
        changedFiles: [{ fileName: b, textChanges: [{ start: { line: 1, offset: 18 }, end: { line: 1, offset: 21 }, newText: 'true' }] }],
    });
    assert.deepStrictEqual(toT.map((m) => m.command), ['updateOpen']);
    assert.deepStrictEqual(toT[0].arguments.openFiles.map((/** @type {any} */ f) => f.file), [c]);
    assert.deepStrictEqual(toT[0].arguments.changedFiles.map((/** @type {any} */ f) => f.fileName), [b]);
});

test('Reload Projects reloads ts-r as well as TypeScript', async () => {
    const { Router } = require('../standin/router');
    /** @type {any[]} */
    const toT = [];
    /** @type {string[]} */
    const toR = [];
    /** @type {any[]} */
    const toVs = [];
    class Stubbed extends Router {
        startR() {
            this.rReady = Promise.resolve();
            return /** @type {any} */ ({
                request: async () => ({ config: '/w/tsconfig.json', state: 'matched' }),
                notify: (/** @type {string} */ method) => toR.push(method),
            });
        }
        ensureT() {
            if (!this.T) this.T = /** @type {any} */ ({ send: (/** @type {any} */ m) => toT.push(m), stop() {} });
            return /** @type {any} */ (this.T);
        }
    }
    const r = new Stubbed({ bin: '', common: [], name: 'test', args: [], mode: 'auto', tsdkDir: '', send: (m) => toVs.push(m), log() {}, exit() {} });
    await r.handle({ seq: 1, type: 'request', command: 'updateOpen', arguments: { openFiles: [{ file: '/w/a.ts', fileContent: 'export {};\n', scriptKindName: 'TS' }] } });
    // TypeScript not running: ts-r reloads, the bridge answers VS Code.
    toR.length = 0;
    await r.handle({ seq: 2, type: 'request', command: 'reloadProjects', arguments: {} });
    assert.deepStrictEqual(toR, ['ts-r/reloadProjects']);
    assert.ok(toVs.some((m) => m.type === 'response' && m.request_seq === 2 && m.success));
    assert.strictEqual(toT.length, 0);
    // TypeScript running: both reload.
    r.ensureT();
    toR.length = 0;
    await r.handle({ seq: 3, type: 'request', command: 'reloadProjects', arguments: {} });
    assert.deepStrictEqual(toR, ['ts-r/reloadProjects']);
    assert.deepStrictEqual(toT.map((m) => [m.command, m.seq]), [['reloadProjects', 3]]);
});

/**
 * A router with stubbed engines for the reviewer's cross-engine case: `/w/r/` is a ts-r
 * project (parity matched), `/w/t/` a TypeScript one (fallback). `tFiles` are the files
 * TypeScript's project of `/w/t/` lists; `rUses` the files ts-r's `/w/r/` project reads.
 */
function crossEngines(/** @type {{tFiles: string[], rUses: string[]}} */ o) {
    const { Router } = require('../standin/router');
    /** @type {any[]} */
    const toT = [];
    /** @type {{method: string, params: any}[]} */
    const toR = [];
    class Stubbed extends Router {
        startR() {
            this.rReady = Promise.resolve();
            return /** @type {any} */ ({
                request: async (/** @type {string} */ method, /** @type {any} */ p) => {
                    if (method === 'ts-r/projectInfo') {
                        const t = p.textDocument.uri.includes('/w/t/');
                        return { config: t ? '/w/t/tsconfig.json' : '/w/r/tsconfig.json', state: t ? 'fallback' : 'matched' };
                    }
                    if (method === 'ts-r/usedBy') {
                        /** @type {Record<string, string[]>} */
                        const out = {};
                        for (const u of p.uris) {
                            const f = u.replace('file://', '');
                            const users = [f.startsWith('/w/t/') ? '/w/t/tsconfig.json' : '/w/r/tsconfig.json'];
                            if (o.rUses.includes(f)) users.push('/w/r/tsconfig.json');
                            out[u] = users;
                        }
                        return out;
                    }
                    return null;
                },
                notify: (/** @type {string} */ method, /** @type {any} */ params) => toR.push({ method, params }),
            });
        }
        ensureT() {
            if (!this.T) {
                this.T = /** @type {any} */ ({
                    send: (/** @type {any} */ m) => {
                        toT.push(m);
                        if (m.command === 'projectInfo') {
                            const body = { configFileName: '/w/t/tsconfig.json', fileNames: o.tFiles };
                            setImmediate(() => this.onT({ seq: 0, type: 'response', command: 'projectInfo', request_seq: m.seq, success: true, body }));
                        }
                    },
                    stop() {},
                });
            }
            return /** @type {any} */ (this.T);
        }
    }
    /** @type {any[]} */
    const toVs = [];
    const r = new Stubbed({ bin: '', common: [], name: 'test', args: [], mode: 'auto', tsdkDir: '', send: (m) => toVs.push(m), log() {}, exit() {} });
    let seq = 1;
    const req = (/** @type {string} */ command, /** @type {any} */ args) => r.handle({ seq: seq++, type: 'request', command, arguments: args });
    /** Waits (bounded) until `pred` holds. */
    const until = async (/** @type {() => boolean} */ pred, /** @type {string} */ what) => {
        const end = Date.now() + 5000;
        while (!pred()) {
            if (Date.now() > end) throw new Error(`timed out: ${what}`);
            await new Promise((res) => setTimeout(res, 10));
        }
        await r.queue;
    };
    return { r, req, until, toT, toR, toVs };
}

const NUMBER_TO_STRING = [{ start: { line: 1, offset: 17 }, end: { line: 1, offset: 23 }, newText: 'string' }];

test('auto: a TypeScript project that imports an open ts-r file sees its unsaved text', async () => {
    // t/b.ts (TypeScript) imports r/a.ts (ts-r); r/c.ts is open too but nothing on TypeScript uses it.
    const [a, b, c] = ['/w/r/a.ts', '/w/t/b.ts', '/w/r/c.ts'];
    const s = crossEngines({ tFiles: [b, a], rUses: [] });
    await s.req('updateOpen', {
        openFiles: [
            { file: a, fileContent: 'export const v: number = 1;\n', scriptKindName: 'TS' },
            { file: b, fileContent: 'import { v } from "../r/a";\nexport const n: number = v;\n', scriptKindName: 'TS' },
            { file: c, fileContent: 'export const c = 1;\n', scriptKindName: 'TS' },
        ],
    });
    // TypeScript serves b.ts; its project lists a.ts, so a.ts is opened there too, c.ts is not.
    const opened = () => s.toT.filter((m) => m.command === 'updateOpen').flatMap((m) => m.arguments.openFiles || []).map((/** @type {any} */ f) => f.file);
    await s.until(() => opened().includes(a), 'a.ts opened in TypeScript');
    assert.deepStrictEqual(opened().sort(), [a, b]);
    assert.ok(s.toT.some((m) => m.command === 'projectInfo' && m.arguments.file === b && m.arguments.needFileNameList));
    // a.ts stays ts-r's: ts-r answers for it, TypeScript's diagnostics for it are dropped.
    const e = (await s.r.engines()).files;
    assert.deepStrictEqual([e[a], e[b]], ['ts-r', 'typescript']);
    // The unsaved edit of a.ts (number -> string) reaches TypeScript, and ts-r.
    s.toT.length = 0;
    s.toR.length = 0;
    await s.req('updateOpen', { changedFiles: [{ fileName: a, textChanges: NUMBER_TO_STRING }] });
    const sent = s.toT.find((m) => m.command === 'updateOpen');
    assert.deepStrictEqual(sent.arguments.changedFiles, [{ fileName: a, textChanges: NUMBER_TO_STRING }]);
    assert.strictEqual(s.r.docs.get(a)?.text.text, 'export const v: string = 1;\n');
    assert.deepStrictEqual(s.toR.map((m) => m.method), ['textDocument/didChange']);
    // Closing a.ts closes it in both.
    s.toT.length = 0;
    await s.req('updateOpen', { closedFiles: [a] });
    assert.deepStrictEqual(s.toT.find((m) => m.command === 'updateOpen').arguments.closedFiles, [a]);
});

test('auto: a ts-r project that imports an open TypeScript file sees its unsaved text', async () => {
    // r/a.ts (ts-r) imports t/b.ts (TypeScript); t/d.ts is open too but no ts-r project uses it.
    const [a, b, d] = ['/w/r/a.ts', '/w/t/b.ts', '/w/t/d.ts'];
    const s = crossEngines({ tFiles: [b, d], rUses: [b] });
    await s.req('updateOpen', {
        openFiles: [
            { file: a, fileContent: 'import { v } from "../t/b";\nexport const n: number = v;\n', scriptKindName: 'TS' },
            { file: b, fileContent: 'export const v: number = 1;\n', scriptKindName: 'TS' },
            { file: d, fileContent: 'export const d = 1;\n', scriptKindName: 'TS' },
        ],
    });
    const mirrors = () => s.toR.filter((m) => m.method === 'textDocument/didOpen' && m.params.textDocument.languageId === 'ts-r-mirror');
    await s.until(() => mirrors().length > 0, 'b.ts mirrored into ts-r');
    // b.ts is mirrored with its text (read by ts-r's projects, owned by none); d.ts is not.
    assert.deepStrictEqual(
        mirrors().map((m) => [m.params.textDocument.uri, m.params.textDocument.text]),
        [['file:///w/t/b.ts', 'export const v: number = 1;\n']],
    );
    const e = (await s.r.engines()).files;
    assert.deepStrictEqual([e[a], e[b], e[d]], ['ts-r', 'typescript', 'typescript']);
    // The unsaved edit of b.ts (number -> string) reaches ts-r as well as TypeScript.
    s.toR.length = 0;
    s.toT.length = 0;
    await s.req('updateOpen', { changedFiles: [{ fileName: b, textChanges: NUMBER_TO_STRING }] });
    assert.deepStrictEqual(
        s.toR.map((m) => [m.method, m.params.textDocument.uri, m.params.contentChanges?.[0]?.text]),
        [['textDocument/didChange', 'file:///w/t/b.ts', 'string']],
    );
    assert.ok(s.toT.some((m) => m.command === 'updateOpen' && m.arguments.changedFiles?.[0]?.fileName === b));
    // ts-r's diagnostics for the mirror are never shown (TypeScript's are).
    s.toVs.length = 0;
    s.r.onRDiagnostics({ uri: 'file:///w/t/b.ts', diagnostics: [{ range: { start: { line: 0, character: 0 }, end: { line: 0, character: 1 } }, message: 'x', source: 'ts-r' }] });
    assert.strictEqual(s.toVs.length, 0);
    // Closing b.ts closes the mirror.
    s.toR.length = 0;
    await s.req('updateOpen', { closedFiles: [b] });
    assert.deepStrictEqual(s.toR.map((m) => [m.method, m.params.textDocument.uri]), [['textDocument/didClose', 'file:///w/t/b.ts']]);
});
