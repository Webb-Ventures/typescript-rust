// @ts-check
// Drives standin/tsserver.js the way VS Code's TypeScript extension does (fork + --useNodeIpc,
// and plain stdio), against a real ts-r-lsp binary with its fake tsserver backend.
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

const repo = path.resolve(__dirname, '..', '..', '..');
const exe = process.platform === 'win32' ? 'ts-r-lsp.exe' : 'ts-r-lsp';
const bin = [
    process.env.TS_R_LSP_BIN,
    path.join(repo, 'target', 'lsp', 'debug', exe),
    path.join(repo, 'target', 'debug', exe),
    path.join(repo, 'target', 'lsp', 'release', exe),
].find((p) => p && fs.existsSync(p));

/** A temp workspace + stand-in tsdk dir. */
function setup(/** @type {string} */ name, /** @type {string | undefined} */ binary) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), `tsr-standin-${name}-`));
    const proj = path.join(dir, 'proj');
    const lib = path.join(dir, 'tsdk', 'lib');
    const fallback = path.join(dir, 'bundled', 'lib');
    fs.mkdirSync(proj, { recursive: true });
    fs.mkdirSync(lib, { recursive: true });
    fs.mkdirSync(fallback, { recursive: true });
    fs.writeFileSync(path.join(proj, 'a.ts'), 'export const a = 1;\n');
    fs.copyFileSync(path.join(__dirname, '..', 'standin', 'tsserver.js'), path.join(lib, 'tsserver.js'));
    fs.writeFileSync(path.join(dir, 'tsdk', 'package.json'), JSON.stringify({ name: 'typescript', version: '5.9.3' }));
    fs.writeFileSync(
        path.join(dir, 'tsdk', 'ts-r.json'),
        JSON.stringify({ bin: binary, fallback, settings: { memoryBudgetMB: 900, idleShutdownMinutes: 5, typeBackend: 'typescript', lint: 'off' } }),
    );
    // Stand-in for VS Code's bundled TypeScript: the daemon's fallback (no TypeScript in the
    // project), and what the stand-in loads in-process when ts-r cannot run.
    fs.writeFileSync(path.join(fallback, '..', 'package.json'), JSON.stringify({ name: 'typescript', version: '5.9.3' }));
    fs.writeFileSync(
        path.join(fallback, 'tsserver.js'),
        "process.on('message', (m) => process.send({ seq: 0, type: 'response', request_seq: m.seq, command: m.command, success: true, body: 'bundled' }));\n",
    );
    const env = {
        ...process.env,
        TS_R_CACHE_DIR: path.join(dir, 'cache'),
        TS_R_WORKSPACE_ROOT: fs.realpathSync(proj),
        TS_R_LSP_TSSERVER_CMD: JSON.stringify([binary, '__fake-backend', 'tsserver']),
    };
    const cleanup = () => {
        if (binary) childProcess.spawnSync(binary, ['stop', '--root', env.TS_R_WORKSPACE_ROOT], { env });
        fs.rmSync(dir, { recursive: true, force: true });
    };
    return { dir, proj: env.TS_R_WORKSPACE_ROOT, lib, env, cleanup };
}

const vscodeArgs = (/** @type {string} */ dir) => [
    '--useInferredProjectPerProjectRoot',
    '--enableTelemetry',
    '--cancellationPipeName',
    path.join(dir, 'cancel', 'tscancellation-x.tmp*'),
    '--locale',
    'en',
    '--noGetErrOnBackgroundUpdate',
    '--validateDefaultNpmLocation',
];

/** VS Code's IPC mode: `fork(tsserver.js, [...args, '--useNodeIpc'])`. */
function forkIpc(/** @type {ReturnType<typeof setup>} */ s) {
    const child = childProcess.fork(path.join(s.lib, 'tsserver.js'), [...vscodeArgs(s.dir), '--useNodeIpc'], {
        env: s.env,
        stdio: ['pipe', 'pipe', 'inherit', 'ipc'],
    });
    /** @type {any[]} */
    const inbox = [];
    /** @type {(() => void)[]} */
    const waiters = [];
    child.on('message', (m) => {
        inbox.push(m);
        waiters.splice(0).forEach((w) => w());
    });
    let seq = 1;
    return {
        child,
        /** @returns {Promise<any>} */
        async call(/** @type {string} */ command, /** @type {unknown} */ args) {
            const id = seq++;
            child.send({ seq: id, type: 'request', command, arguments: args });
            for (;;) {
                const i = inbox.findIndex((m) => m.type === 'response' && m.request_seq === id);
                if (i >= 0) return inbox.splice(i, 1)[0];
                await new Promise((r) => waiters.push(() => r(undefined)));
            }
        },
    };
}

/** VS Code's stdio mode (typescript.tsserver.nodePath set): JSON lines in, frames out. */
function spawnStdio(/** @type {ReturnType<typeof setup>} */ s) {
    const child = childProcess.spawn(process.execPath, [path.join(s.lib, 'tsserver.js'), ...vscodeArgs(s.dir)], {
        env: s.env,
        stdio: ['pipe', 'pipe', 'inherit'],
    });
    let buf = '';
    /** @type {any[]} */
    const inbox = [];
    /** @type {(() => void)[]} */
    const waiters = [];
    child.stdout.on('data', (d) => {
        buf += d.toString('utf8');
        for (;;) {
            const m = /^\s*Content-Length: (\d+)\r\n\r\n/.exec(buf);
            if (!m || buf.length < m[0].length + Number(m[1])) break;
            inbox.push(JSON.parse(buf.slice(m[0].length, m[0].length + Number(m[1]))));
            buf = buf.slice(m[0].length + Number(m[1]));
        }
        waiters.splice(0).forEach((w) => w());
    });
    let seq = 1;
    return {
        child,
        /** @returns {Promise<any>} */
        async call(/** @type {string} */ command, /** @type {unknown} */ args) {
            const id = seq++;
            child.stdin.write(JSON.stringify({ seq: id, type: 'request', command, arguments: args }) + '\r\n');
            for (;;) {
                const i = inbox.findIndex((m) => m.type === 'response' && m.request_seq === id);
                if (i >= 0) return inbox.splice(i, 1)[0];
                await new Promise((r) => waiters.push(() => r(undefined)));
            }
        },
    };
}

test('an IPC window and a stdio window share one backend through the stand-in', { skip: !bin && 'build ts-r-lsp first', timeout: 20000 }, async () => {
    const s = setup('share', bin);
    try {
        const file = path.join(s.proj, 'a.ts');
        const a = forkIpc(s);
        const opened = await a.call('updateOpen', { openFiles: [{ file, fileContent: 'export const a = 1;\n', projectRootPath: s.proj }] });
        assert.strictEqual(opened.success, true);
        const qa = await a.call('quickinfo', { file, line: 1, offset: 14 });
        assert.match(qa.body.displayString, /text=export const a = 1;/);
        const backendPid = /pid=(\d+)/.exec(qa.body.displayString)?.[1];

        const b = spawnStdio(s);
        await b.call('updateOpen', { openFiles: [{ file, projectRootPath: s.proj }] });
        const qb = await b.call('quickinfo', { file, line: 1, offset: 14 });
        assert.strictEqual(/pid=(\d+)/.exec(qb.body.displayString)?.[1], backendPid, 'same backend process');

        const status = JSON.parse(childProcess.execFileSync(/** @type {string} */ (bin), ['status', '--root', s.proj], { env: s.env, encoding: 'utf8' }));
        assert.strictEqual(status.slots.length, 1);
        assert.strictEqual(status.clients.length, 2);
        assert.strictEqual(status.settings.memoryBudgetMB, 900);
        a.child.kill();
        b.child.kill();
    } finally {
        s.cleanup();
    }
});

test('falls back to the bundled tsserver when ts-r-lsp is missing', { timeout: 30000 }, async () => {
    const s = setup('fallback', path.join(os.tmpdir(), 'no-such-ts-r-lsp'));
    try {
        const a = forkIpc(s);
        const r = await a.call('configure', {});
        assert.strictEqual(r.body, 'bundled');
        a.child.kill();
    } finally {
        s.cleanup();
    }
});
