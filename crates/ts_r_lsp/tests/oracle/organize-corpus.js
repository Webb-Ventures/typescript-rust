// @ts-check
// Organize imports on every source file of a real project, by tsserver and by ts-r-lsp's
// ts-r backend (`source.organizeImports`), compared file by file. Read-only: edits are applied
// in memory. Prints one JSON line: files, equal, differing (first few with both outputs),
// latency p50/p95 for each server.
//
//   TS_R_LSP_PARITY=off scripts/limit.sh -- node crates/ts_r_lsp/tests/oracle/organize-corpus.js \
//     <ts-r-lsp> <typescript/lib dir> <root> [--from N] [--max N] [--show N] [--debug]
//
// Stop the daemon afterwards: `ts-r-lsp stop --root <root>`.
'use strict';

const fs = require('fs');
const path = require('path');
const childProcess = require('child_process');
const { LspClient } = require('../../../../editors/vscode/standin/lspclient');

const [bin, libDir, rootArg, ...rest] = process.argv.slice(2);
if (!bin || !libDir || !rootArg) {
    console.error('usage: organize-corpus.js <ts-r-lsp> <typescript/lib dir> <root> [--from N] [--max N] [--show N] [--debug]');
    process.exit(2);
}
const opt = (/** @type {string} */ n, /** @type {string} */ d) => {
    const i = rest.indexOf(n);
    return i >= 0 ? rest[i + 1] : d;
};
const max = Number(opt('--max', '100000'));
const from = Number(opt('--from', '0'));
const debug = rest.includes('--debug');
const show = Number(opt('--show', '5'));
const root = fs.realpathSync(rootArg);
const uriOf = (/** @type {string} */ p) => `file://${p.split(path.sep).map(encodeURIComponent).join('/')}`;

function sources() {
    /** @type {string[]} */
    const out = [];
    const walk = (/** @type {string} */ dir) => {
        for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
            if (['node_modules', 'dist', '.next', '.wrangler', 'coverage', 'generated', '.git', '.turbo'].includes(e.name)) continue;
            const p = path.join(dir, e.name);
            if (e.isDirectory()) walk(p);
            else if (/\.(ts|tsx)$/.test(e.name) && !/\.d\.ts$/.test(e.name)) out.push(p);
        }
    };
    // Not `infra/`: tsserver stops answering while loading the Pulumi project there.
    for (const d of ['apps', 'domains', 'e2e', 'packages', 'src']) if (fs.existsSync(path.join(root, d))) walk(path.join(root, d));
    return out.sort();
}

class TsServer {
    constructor() {
        this.seq = 0;
        /** @type {Map<number, (m: any) => void>} */
        this.pending = new Map();
        this.buffer = Buffer.alloc(0);
        this.child = childProcess.spawn(process.execPath, ['--max-old-space-size=3072', path.join(libDir, 'tsserver.js'), '--disableAutomaticTypingAcquisition'], {
            cwd: root,
            stdio: ['pipe', 'pipe', 'ignore'],
        });
        this.child.stdout.on('data', (/** @type {Buffer} */ c) => this.onData(c));
    }
    onData(/** @type {Buffer} */ chunk) {
        this.buffer = Buffer.concat([this.buffer, chunk]);
        for (;;) {
            const h = this.buffer.indexOf('\r\n\r\n');
            if (h < 0) return;
            const m = /Content-Length:\s*(\d+)/i.exec(this.buffer.subarray(0, h).toString('ascii'));
            const len = m ? Number(m[1]) : 0;
            if (this.buffer.length < h + 4 + len) return;
            const msg = JSON.parse(this.buffer.subarray(h + 4, h + 4 + len).toString('utf8'));
            this.buffer = this.buffer.subarray(h + 4 + len);
            if (msg.type === 'response') {
                const p = this.pending.get(msg.request_seq);
                if (p) {
                    this.pending.delete(msg.request_seq);
                    p(msg);
                }
            }
        }
    }
    /** @returns {Promise<any>} */
    request(/** @type {string} */ command, /** @type {any} */ args) {
        const seq = ++this.seq;
        this.child.stdin.write(JSON.stringify({ seq, type: 'request', command, arguments: args }) + '\n');
        return new Promise((resolve) => {
            const t = setTimeout(() => {
                this.pending.delete(seq);
                resolve({ success: false, timedOut: true });
            }, 60000);
            this.pending.set(seq, (/** @type {any} */ m) => {
                clearTimeout(t);
                resolve(m);
            });
        });
    }
    stop() {
        this.child.stdin.write(JSON.stringify({ seq: ++this.seq, type: 'request', command: 'exit' }) + '\n');
        setTimeout(() => this.child.kill(), 500);
    }
}

/** Index of a tsserver 1-based line/offset. */
function tsIndex(/** @type {string} */ text, /** @type {{line: number, offset: number}} */ p) {
    let i = 0;
    for (let l = 1; l < p.line; l++) {
        const n = text.indexOf('\n', i);
        if (n < 0) return text.length;
        i = n + 1;
    }
    return i + p.offset - 1;
}

/** Index of an LSP position (UTF-16, lines split at \n). */
function lspIndex(/** @type {string} */ text, /** @type {{line: number, character: number}} */ p) {
    let i = 0;
    for (let l = 0; l < p.line; l++) {
        const n = text.indexOf('\n', i);
        if (n < 0) return text.length;
        i = n + 1;
    }
    return i + p.character;
}

function applyEdits(/** @type {string} */ text, /** @type {{s: number, e: number, t: string}[]} */ edits) {
    for (const x of [...edits].sort((a, b) => b.s - a.s || b.e - a.e)) text = text.slice(0, x.s) + x.t + text.slice(x.e);
    return text;
}

const pct = (/** @type {number[]} */ xs, /** @type {number} */ q) => {
    if (!xs.length) return null;
    const s = [...xs].sort((a, b) => a - b);
    return Math.round(s[Math.min(s.length - 1, Math.floor(q * s.length))] * 100) / 100;
};

async function main() {
    const files = sources().slice(from, from + max);
    const ts = new TsServer();
    // The project's formatting (2-space indentation), as an editor would send it.
    await ts.request('configure', {
        preferences: { quotePreference: 'auto' },
        formatOptions: { indentSize: Number(opt('--indent', '2')), tabSize: Number(opt('--indent', '2')), convertTabsToSpaces: true },
    });
    const lsp = new LspClient(bin, ['--root', root, '--name', 'organize corpus', '--lint', 'off'], {
        cwd: root,
        env: { ...process.env, TS_R_LSP_TYPE_BACKEND: 'ts-r' },
    });
    await lsp.request('initialize', { processId: process.pid, rootUri: uriOf(root), capabilities: {} });
    lsp.notify('initialized', {});
    let equal = 0;
    let changedByTs = 0;
    /** @type {any[]} */
    const diffs = [];
    /** @type {number[]} */
    const tsMs = [];
    /** @type {number[]} */
    const ourMs = [];
    let errors = 0;
    let done = 0;
    for (const f of files) {
        if (++done % 50 === 0) process.stderr.write(`${done}/${files.length}\n`);
        if (debug) process.stderr.write(`${f}\n`);
        const text = fs.readFileSync(f, 'utf8');
        await ts.request('updateOpen', { openFiles: [{ file: f, fileContent: text, projectRootPath: root }] });
        let t0 = performance.now();
        const r = await ts.request('organizeImports', { scope: { type: 'file', args: { file: f } } });
        tsMs.push(performance.now() - t0);
        await ts.request('updateOpen', { closedFiles: [f] });
        const tsEdits = (r.body ?? []).filter((/** @type {any} */ c) => c.fileName === f).flatMap((/** @type {any} */ c) => c.textChanges);
        const a = applyEdits(text, tsEdits.map((/** @type {any} */ e) => ({ s: tsIndex(text, e.start), e: tsIndex(text, e.end), t: e.newText })));
        const uri = uriOf(f);
        lsp.notify('textDocument/didOpen', { textDocument: { uri, languageId: f.endsWith('x') ? 'typescriptreact' : 'typescript', version: 1, text } });
        t0 = performance.now();
        /** @type {any} */
        let ca = null;
        try {
            ca = await Promise.race([
                lsp.request('textDocument/codeAction', {
                    textDocument: { uri },
                    range: { start: { line: 0, character: 0 }, end: { line: 0, character: 0 } },
                    context: { diagnostics: [], only: ['source.organizeImports'] },
                }),
                new Promise((_, reject) => setTimeout(() => reject(new Error('timeout')), 60000)),
            ]);
        } catch {
            errors++;
        }
        ourMs.push(performance.now() - t0);
        lsp.notify('textDocument/didClose', { textDocument: { uri } });
        const action = Array.isArray(ca) ? ca.find((x) => x.kind === 'source.organizeImports') : null;
        const ourEdits = action?.edit?.changes?.[uri] ?? [];
        const b = applyEdits(text, ourEdits.map((/** @type {any} */ e) => ({ s: lspIndex(text, e.range.start), e: lspIndex(text, e.range.end), t: e.newText })));
        if (a !== text) changedByTs++;
        if (a === b) equal++;
        else if (diffs.length < show) diffs.push({ file: path.relative(root, f), tsserver: a.slice(0, 1500), ours: b.slice(0, 1500), original: text.slice(0, 1500) });
    }
    ts.stop();
    await lsp.stop();
    console.log(
        JSON.stringify(
            {
                files: files.length,
                changedByTsserver: changedByTs,
                equal,
                differing: files.length - equal,
                errors,
                tsserverMs: { p50: pct(tsMs, 0.5), p95: pct(tsMs, 0.95) },
                tsrMs: { p50: pct(ourMs, 0.5), p95: pct(ourMs, 0.95) },
                diffs,
            },
            null,
            1,
        ),
    );
}

main();
