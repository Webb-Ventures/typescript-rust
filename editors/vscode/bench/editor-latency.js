// @ts-check
// Type-checking editor latency and memory, measured with a scripted client (no VS Code), for
// ts-r-lsp's ts-r backend, the project's own tsserver, and TypeScript 7's native LSP.
//
//   node bench/editor-latency.js ts-r     <ts-r-lsp>        <root> <file> [options]
//   node bench/editor-latency.js tsserver <tsserver.js>     <root> <file> [options]
//   node bench/editor-latency.js tsgo     <native tsc exe>  <root> <file> [options]
//
// Options: --keys N (100) keystrokes, --interval MS (150) between keystrokes, --samples N (40)
// hover/definition/completion positions, --open-all (open every source file at the end and
// report memory with all of them open, --settle MS (1000) after the last one), --dump FILE (per-sample results as JSON),
// --sound LEVEL (prepend `// @ts-r LEVEL` to the opened text, in memory only: sound mode for that file),
// --src-dirs a,b (directories under <root> that --open-all walks; default src,packages,apps.
// `cargo xtask bench --editor --repos <name>` passes each pinned repo's own).
//
// Measured, in this order:
//   1. firstDiagMs: open <file> cold -> its semantic diagnostics.
//   2. hover / definition / completion latency at sampled identifiers (completion right
//      after a `.`), p50/p95.
//   3. keystrokes: append `__bench_` on a new last line, then type letters onto it; each
//      keystroke is timed from the change to the diagnostics that name the new identifier
//      (TS2304 "Cannot find name '__bench_ab'"), so syntax-only or stale publishes don't count.
//   4. code actions: at each sampled identifier, the quick fixes and refactors for that
//      position (LSP `textDocument/codeAction` with the diagnostics on that line; tsserver
//      `getCodeFixes` for them plus `getApplicableRefactors`), then organize imports
//      (`source.organizeImports` / `organizeImports`) codeActionMs p50/p95; and the quick
//      fixes on the last keystroke's TS2304 (fixAfterKeyMs, typed identifier, so it is cold).
//   5. rssMB with that one file open; with --open-all, open every source (as one batch) and
//      wait until each has diagnostics, then rssAllMB.
// ts-r's memory is the daemon's RSS (the checker runs in the daemon); tsserver's and tsgo's is
// their process. Run under scripts/limit.sh, with TS_R_LSP_PARITY=off for ts-r (the parity
// check runs the project's tsc in the background). Stop the daemon afterwards
// (`ts-r-lsp stop --root <root>`).
'use strict';

const fs = require('fs');
const path = require('path');
const childProcess = require('child_process');
const { LspClient } = require('../standin/lspclient');

const [kind, server, rootArg, fileArg, ...rest] = process.argv.slice(2);
if (!['ts-r', 'tsserver', 'tsgo'].includes(kind) || !server || !rootArg || !fileArg) {
    console.error('usage: editor-latency.js ts-r|tsserver|tsgo <server> <root> <file> [--keys N] [--interval MS] [--samples N] [--open-all] [--settle MS] [--src-dirs a,b] [--dump FILE]');
    process.exit(2);
}
/** @param {string} name @param {string} dflt */
function opt(name, dflt) {
    const i = rest.indexOf(name);
    return i >= 0 && i + 1 < rest.length ? rest[i + 1] : dflt;
}
const keys = Number(opt('--keys', '100'));
const interval = Number(opt('--interval', '150'));
const samples = Number(opt('--samples', '40'));
const dumpFile = opt('--dump', '');
const openAll = rest.includes('--open-all');
const settleMs = Number(opt('--settle', '1000'));
const soundLevel = opt('--sound', '');
const srcDirs = opt('--src-dirs', 'src,packages,apps').split(',').filter(Boolean);
const root = fs.realpathSync(rootArg);
const file = path.resolve(root, fileArg);
const rootUri = `file://${root}`;
const uriOf = (/** @type {string} */ p) => `file://${p.split(path.sep).map(encodeURIComponent).join('/')}`;
const sleep = (/** @type {number} */ ms) => new Promise((r) => setTimeout(r, ms));
const TIMEOUT = 120_000;

/** @param {number} pid */
function rssMB(pid) {
    const r = childProcess.spawnSync('ps', ['-o', 'rss=', '-p', String(pid)], { encoding: 'utf8' });
    return Math.round((Number(r.stdout.trim()) / 1024) * 10) / 10;
}

/** @param {number[]} xs @param {number} q */
function pct(xs, q) {
    if (!xs.length) return null;
    const s = [...xs].sort((a, b) => a - b);
    return Math.round(s[Math.min(s.length - 1, Math.floor(q * s.length))] * 100) / 100;
}

/** TypeScript sources under the workspace packages (the files an editor session could open). */
function sources() {
    /** @type {string[]} */
    const out = [];
    /** @param {string} dir */
    const walk = (dir) => {
        for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
            if (['node_modules', 'dist', '.next', '.wrangler', 'coverage', 'generated', '.git', '.turbo'].includes(e.name)) continue;
            const p = path.join(dir, e.name);
            if (e.isDirectory()) walk(p);
            else if (/\.(ts|tsx)$/.test(e.name) && !/\.d\.ts$/.test(e.name)) out.push(p);
        }
    };
    for (const d of srcDirs) if (fs.existsSync(path.join(root, d))) walk(path.join(root, d));
    return out.sort();
}

/** @param {string} p */
function languageId(p) {
    return p.endsWith('.tsx') ? 'typescriptreact' : p.endsWith('.ts') ? 'typescript' : p.endsWith('.jsx') ? 'javascriptreact' : 'javascript';
}

const KEYWORDS = new Set(
    ('break case catch class const continue debugger default delete do else enum export extends false finally for function if import in ' +
        'instanceof new null return super switch this throw true try typeof var void while with as implements interface let package private ' +
        'protected public static yield any boolean constructor declare get module require number set string symbol type from of async await ' +
        'readonly unknown never undefined keyof infer is satisfies object bigint unique abstract override accessor asserts out using').split(' '),
);

/** Offsets (UTF-16, as JS strings index) of sampled identifiers and of `.` member positions. */
function samplePositions(/** @type {string} */ text) {
    // Blank out comments and strings so samples land on code.
    const code = text.replace(/\/\*[\s\S]*?\*\/|\/\/[^\n]*|'(?:\\.|[^'\\\n])*'|"(?:\\.|[^"\\\n])*"|`(?:\\.|[^`\\])*`/g, (m) => m.replace(/[^\n]/g, ' '));
    /** @type {number[]} */
    const idents = [];
    for (const m of code.matchAll(/[A-Za-z_$][\w$]*/g)) {
        if (KEYWORDS.has(m[0]) || m.index === undefined) continue;
        if (m.index > 0 && /[\w$.]/.test(code[m.index - 1]) && code[m.index - 1] !== '.') continue;
        idents.push(m.index + Math.floor(m[0].length / 2));
    }
    /** @type {number[]} */
    const dots = [];
    for (const m of code.matchAll(/[\w$)\]]\.([A-Za-z_$])/g)) if (m.index !== undefined) dots.push(m.index + 2);
    /** @param {number[]} xs */
    const pick = (xs) => {
        if (xs.length <= samples) return xs;
        const step = xs.length / samples;
        return Array.from({ length: samples }, (_, i) => xs[Math.floor(i * step)]);
    };
    return { idents: pick(idents), dots: pick(dots) };
}

/** Line/character (0-based, UTF-16) of a string index. */
function lineCol(/** @type {string} */ text, /** @type {number} */ index) {
    let line = 0;
    let start = 0;
    for (let i = text.indexOf('\n'); i >= 0 && i < index; i = text.indexOf('\n', i + 1)) {
        line++;
        start = i + 1;
    }
    return { line, character: index - start };
}

/** A tsserver client: newline-terminated JSON in, Content-Length frames out. */
class TsServer {
    /** @param {string} tsserverJs */
    constructor(tsserverJs) {
        this.seq = 0;
        /** @type {Map<number, (m: any) => void>} */
        this.pending = new Map();
        /** @type {((m: any) => void)[]} */
        this.eventListeners = [];
        this.buffer = Buffer.alloc(0);
        this.child = childProcess.spawn(
            process.execPath,
            ['--max-old-space-size=3072', tsserverJs, '--disableAutomaticTypingAcquisition', '--useInferredProjectPerProjectRoot', '--noGetErrOnBackgroundUpdate'],
            { cwd: root, stdio: ['pipe', 'pipe', 'pipe'] },
        );
        this.child.stdout.on('data', (/** @type {Buffer} */ c) => this.onData(c));
        this.child.stderr.on('data', () => {});
    }
    /** @param {Buffer} chunk */
    onData(chunk) {
        this.buffer = Buffer.concat([this.buffer, chunk]);
        for (;;) {
            const h = this.buffer.indexOf('\r\n\r\n');
            if (h < 0) return;
            const m = /Content-Length:\s*(\d+)/i.exec(this.buffer.subarray(0, h).toString('ascii'));
            if (!m) {
                this.buffer = this.buffer.subarray(h + 4);
                continue;
            }
            const len = Number(m[1]);
            if (this.buffer.length < h + 4 + len) return;
            const body = this.buffer.subarray(h + 4, h + 4 + len).toString('utf8');
            this.buffer = this.buffer.subarray(h + 4 + len);
            let msg;
            try {
                msg = JSON.parse(body);
            } catch {
                continue;
            }
            if (msg.type === 'response') {
                const p = this.pending.get(msg.request_seq);
                if (p) {
                    this.pending.delete(msg.request_seq);
                    p(msg);
                }
            } else if (msg.type === 'event') {
                for (const l of [...this.eventListeners]) l(msg);
            }
        }
    }
    /** @param {string} command @param {any} args */
    send(command, args) {
        const seq = ++this.seq;
        this.child.stdin.write(JSON.stringify({ seq, type: 'request', command, arguments: args }) + '\n');
        return seq;
    }
    /** @param {string} command @param {any} args @returns {Promise<any>} */
    request(command, args) {
        return new Promise((resolve, reject) => {
            const t = setTimeout(() => reject(new Error(`tsserver: ${command} timed out`)), TIMEOUT);
            const seq = this.send(command, args);
            this.pending.set(seq, (m) => {
                clearTimeout(t);
                resolve(m);
            });
        });
    }
    /** Resolves with the first event satisfying `pred`. @param {(m: any) => boolean} pred @returns {Promise<any>} */
    waitEvent(pred) {
        return new Promise((resolve, reject) => {
            const t = setTimeout(() => reject(new Error('tsserver: event timed out')), TIMEOUT);
            const l = (/** @type {any} */ m) => {
                if (!pred(m)) return;
                clearTimeout(t);
                this.eventListeners.splice(this.eventListeners.indexOf(l), 1);
                resolve(m);
            };
            this.eventListeners.push(l);
        });
    }
    async stop() {
        this.send('exit', {});
        await sleep(200);
        this.child.kill();
    }
}

/**
 * The common surface the measurements use.
 * @typedef {{
 *   open: (p: string, text: string) => Promise<any[]>,
 *   openMany: (ps: string[]) => Promise<void>,
 *   change: (p: string, line: number, ch: number, insert: string, until: (diags: string[]) => boolean) => Promise<any>,
 *   hover: (p: string, line: number, ch: number) => Promise<string>,
 *   definition: (p: string, line: number, ch: number) => Promise<number>,
 *   completion: (p: string, line: number, ch: number) => Promise<number>,
 *   codeActions: (p: string, line: number, ch: number, diags: any[]) => Promise<number>,
 *   organize: (p: string) => Promise<number>,
 *   pid: () => number,
 *   stop: () => Promise<void>,
 *   label: string,
 * }} Driver
 */

/** @returns {Promise<Driver>} */
async function lspDriver() {
    const isTsr = kind === 'ts-r';
    const env = { ...process.env };
    if (isTsr) env.TS_R_LSP_TYPE_BACKEND = 'ts-r';
    const c = isTsr
        ? new LspClient(server, ['--root', root, '--name', 'editor bench', '--lint', 'off'], { cwd: root, env })
        : new LspClient(server, ['--lsp', '--stdio'], { cwd: root, env });
    /** @type {Map<string, (d: any[]) => void>} */
    const waiters = new Map();
    /** @type {Map<string, any[]>} */
    const lastDiags = new Map();
    c.onNotification('textDocument/publishDiagnostics', (p) => {
        lastDiags.set(p.uri, p.diagnostics);
        const w = waiters.get(p.uri);
        if (w) w(p.diagnostics);
    });
    const init = await c.request('initialize', {
        processId: process.pid,
        rootUri,
        workspaceFolders: [{ uri: rootUri, name: path.basename(root) }],
        capabilities: {
            general: { positionEncodings: ['utf-16'] },
            textDocument: {
                synchronization: { didSave: true },
                publishDiagnostics: { versionSupport: true },
                diagnostic: { dynamicRegistration: false },
                hover: { contentFormat: ['markdown', 'plaintext'] },
                completion: { completionItem: { snippetSupport: true } },
            },
            workspace: { configuration: true, workspaceFolders: true },
        },
        initializationOptions: isTsr ? { 'ts-r': { lint: { enable: 'off' } } } : {},
    });
    c.notify('initialized', {});
    const pull = !isTsr && !!init?.capabilities?.diagnosticProvider;
    /** @type {Map<string, number>} */
    const versions = new Map();
    /** Diagnostics of `uri` after sending a notification: pulled, or the next push satisfying `until`. */
    const diagsAfter = (/** @type {string} */ uri, /** @type {() => void} */ sendIt, /** @type {(d: any[]) => boolean} */ until) => {
        if (pull) {
            sendIt();
            return (async () => {
                for (let i = 0; i < 1000; i++) {
                    const r = await c.request('textDocument/diagnostic', { textDocument: { uri } });
                    const d = r?.items ?? [];
                    if (until(d)) return d;
                    await sleep(5);
                }
                throw new Error(`no matching diagnostics for ${uri}`);
            })();
        }
        return new Promise((resolve, reject) => {
            const t = setTimeout(() => {
                waiters.delete(uri);
                reject(new Error(`no diagnostics for ${uri}`));
            }, TIMEOUT);
            waiters.set(uri, (d) => {
                if (!until(d)) return;
                clearTimeout(t);
                waiters.delete(uri);
                resolve(d);
            });
            sendIt();
        });
    };
    /** @param {string} p @param {string} text */
    const openNote = (p, text) => {
        versions.set(p, 1);
        c.notify('textDocument/didOpen', { textDocument: { uri: uriOf(p), languageId: languageId(p), version: 1, text } });
    };
    /** @param {any} h */
    const hoverText = (h) => {
        const v = h?.contents;
        if (!v) return '';
        if (typeof v === 'string') return v;
        if (Array.isArray(v)) return v.map((x) => (typeof x === 'string' ? x : x.value)).join('\n');
        return v.value ?? '';
    };
    /** @param {string} p @param {number} line @param {number} character */
    const tdp = (p, line, character) => ({ textDocument: { uri: uriOf(p) }, position: { line, character } });
    return {
        label: isTsr ? 'ts-r-lsp (ts-r backend)' : `TypeScript native LSP (${server})`,
        open: (p, text) => diagsAfter(uriOf(p), () => openNote(p, text), () => true),
        openMany: async (ps) => {
            const all = [];
            for (const p of ps) {
                const uri = uriOf(p);
                if (pull) {
                    openNote(p, fs.readFileSync(p, 'utf8'));
                    all.push(c.request('textDocument/diagnostic', { textDocument: { uri } }));
                } else {
                    all.push(
                        new Promise((resolve) => {
                            if (lastDiags.has(uri)) return resolve(null);
                            const t = setTimeout(resolve, TIMEOUT);
                            const prev = waiters.get(uri);
                            waiters.set(uri, (d) => {
                                if (prev) prev(d);
                                clearTimeout(t);
                                resolve(d);
                            });
                        }),
                    );
                    openNote(p, fs.readFileSync(p, 'utf8'));
                }
            }
            await Promise.all(all);
        },
        change: async (p, line, character, insert, until) => {
            const uri = uriOf(p);
            const version = (versions.get(p) ?? 1) + 1;
            versions.set(p, version);
            return await diagsAfter(
                uri,
                () =>
                    c.notify('textDocument/didChange', {
                        textDocument: { uri, version },
                        contentChanges: [{ range: { start: { line, character }, end: { line, character } }, text: insert }],
                    }),
                (d) => until(d.map((x) => String(x.message))),
            );
        },
        hover: async (p, line, ch) => hoverText(await c.request('textDocument/hover', tdp(p, line, ch))),
        definition: async (p, line, ch) => {
            const r = await c.request('textDocument/definition', tdp(p, line, ch));
            return Array.isArray(r) ? r.length : r ? 1 : 0;
        },
        completion: async (p, line, ch) => {
            const r = await c.request('textDocument/completion', { ...tdp(p, line, ch), context: { triggerKind: 2, triggerCharacter: '.' } });
            return Array.isArray(r) ? r.length : (r?.items?.length ?? 0);
        },
        codeActions: async (p, line, ch, diags) => {
            const here = diags.filter((x) => x.range && x.range.start.line <= line && x.range.end.line >= line);
            const r = await c.request('textDocument/codeAction', {
                textDocument: { uri: uriOf(p) },
                range: { start: { line, character: ch }, end: { line, character: ch } },
                context: { diagnostics: here, triggerKind: 1 },
            });
            return Array.isArray(r) ? r.length : 0;
        },
        organize: async (p) => {
            const r = await c.request('textDocument/codeAction', {
                textDocument: { uri: uriOf(p) },
                range: { start: { line: 0, character: 0 }, end: { line: 0, character: 0 } },
                context: { diagnostics: [], only: ['source.organizeImports'], triggerKind: 1 },
            });
            return Array.isArray(r) ? r.length : 0;
        },
        pid: () => {
            if (!isTsr) return c.child.pid ?? 0;
            const r = childProcess.spawnSync(server, ['status', '--root', root], { encoding: 'utf8' });
            try {
                return JSON.parse(r.stdout).daemonPid ?? 0;
            } catch {
                return 0;
            }
        },
        stop: () => c.stop(),
    };
}

/** @returns {Promise<Driver>} */
async function tsserverDriver() {
    const s = new TsServer(server);
    await s.request('configure', { preferences: { includeCompletionsForModuleExports: true, includeCompletionsWithInsertText: true } });
    /** Runs geterr for `files` and resolves when each has a semanticDiag event satisfying `until`. */
    const errs = async (/** @type {string[]} */ files, /** @type {(d: string[]) => boolean} */ until) => {
        const left = new Set(files);
        /** @type {any[]} */
        let last = [];
        const done = s.waitEvent((m) => {
            if (m.event !== 'semanticDiag' || !left.has(m.body.file)) return false;
            if (!until((m.body.diagnostics || []).map((/** @type {any} */ d) => String(d.text)))) return false;
            left.delete(m.body.file);
            last = m.body.diagnostics || [];
            return left.size === 0;
        });
        s.send('geterr', { files, delay: 0 });
        await done;
        return last;
    };
    return {
        label: `tsserver (${server})`,
        open: async (p, text) => {
            await s.request('updateOpen', { openFiles: [{ file: p, fileContent: text, scriptKindName: p.endsWith('.tsx') ? 'TSX' : 'TS', projectRootPath: root }] });
            let diags = [];
            const ev = s.waitEvent((m) => m.event === 'semanticDiag' && m.body.file === p);
            s.send('geterr', { files: [p], delay: 0 });
            diags = (await ev).body.diagnostics;
            return diags;
        },
        openMany: async (ps) => {
            for (let i = 0; i < ps.length; i += 50) {
                await s.request('updateOpen', {
                    openFiles: ps.slice(i, i + 50).map((p) => ({ file: p, fileContent: fs.readFileSync(p, 'utf8'), projectRootPath: root })),
                });
            }
            // Diagnostics of every file, as an editor showing all problems would request.
            for (let i = 0; i < ps.length; i += 50) await errs(ps.slice(i, i + 50), () => true);
        },
        change: async (p, line, character, insert, until) => {
            s.send('change', { file: p, line: line + 1, offset: character + 1, endLine: line + 1, endOffset: character + 1, insertString: insert });
            return await errs([p], until);
        },
        hover: async (p, line, ch) => {
            const r = await s.request('quickinfo', { file: p, line: line + 1, offset: ch + 1 });
            return r.success ? `${r.body.displayString}\n${typeof r.body.documentation === 'string' ? r.body.documentation : ''}` : '';
        },
        definition: async (p, line, ch) => {
            const r = await s.request('definition', { file: p, line: line + 1, offset: ch + 1 });
            return r.success ? r.body.length : 0;
        },
        completion: async (p, line, ch) => {
            const r = await s.request('completionInfo', { file: p, line: line + 1, offset: ch + 1, triggerCharacter: '.', triggerKind: 2 });
            return r.success && r.body ? r.body.entries.length : 0;
        },
        codeActions: async (p, line, ch, diags) => {
            let n = 0;
            const here = diags.filter((x) => x.start && x.start.line - 1 <= line && x.end.line - 1 >= line);
            for (const x of here) {
                const r = await s.request('getCodeFixes', { file: p, startLine: x.start.line, startOffset: x.start.offset, endLine: x.end.line, endOffset: x.end.offset, errorCodes: [x.code] });
                n += r.success && r.body ? r.body.length : 0;
            }
            const r = await s.request('getApplicableRefactors', { file: p, line: line + 1, offset: ch + 1, triggerReason: 'invoked' });
            return n + (r.success && r.body ? r.body.length : 0);
        },
        organize: async (p) => {
            const r = await s.request('organizeImports', { scope: { type: 'file', args: { file: p } } });
            return r.success && r.body ? r.body.length : 0;
        },
        pid: () => s.child.pid ?? 0,
        stop: () => s.stop(),
    };
}

async function main() {
    const d = kind === 'tsserver' ? await tsserverDriver() : await lspDriver();
    /** @type {any} */
    const dump = { hover: [], definition: [], completion: [] };
    let errors = 0;
    /** @template T @param {() => Promise<T>} f @param {T} dflt @returns {Promise<T>} */
    const tryReq = async (f, dflt) => {
        try {
            return await f();
        } catch {
            errors++;
            return dflt;
        }
    };
    try {
        let text = fs.readFileSync(file, 'utf8');
        if (soundLevel) text = `// @ts-r ${soundLevel}\n${text}`;
        let t0 = performance.now();
        const openDiags = await d.open(file, text);
        const firstDiagMs = performance.now() - t0;

        // Requests at sampled positions.
        const { idents, dots } = samplePositions(text);
        /** @type {number[]} */
        const hoverMs = [];
        /** @type {number[]} */
        const defMs = [];
        /** @type {number[]} */
        const complMs = [];
        let hoverHits = 0;
        let defHits = 0;
        let complHits = 0;
        for (const i of idents) {
            const { line, character } = lineCol(text, i);
            t0 = performance.now();
            const h = await tryReq(() => d.hover(file, line, character), '');
            hoverMs.push(performance.now() - t0);
            if (h) hoverHits++;
            t0 = performance.now();
            const n = await tryReq(() => d.definition(file, line, character), 0);
            defMs.push(performance.now() - t0);
            if (n) defHits++;
            dump.hover.push({ line, character, hover: h.split('\n').slice(0, 3).join('\n'), definitions: n });
        }
        for (const i of dots) {
            const { line, character } = lineCol(text, i);
            t0 = performance.now();
            const n = await tryReq(() => d.completion(file, line, character), 0);
            complMs.push(performance.now() - t0);
            if (n) complHits++;
            dump.completion.push({ line, character, items: n });
        }

        // Code actions at the sampled identifiers, then organize imports.
        /** @type {number[]} */
        const actionMs = [];
        let actionHits = 0;
        const diagList = Array.isArray(openDiags) ? openDiags : [];
        for (const i of idents) {
            const { line, character } = lineCol(text, i);
            t0 = performance.now();
            const n = await tryReq(() => d.codeActions(file, line, character, diagList), 0);
            actionMs.push(performance.now() - t0);
            if (n) actionHits++;
        }
        /** @type {number[]} */
        const organizeMs = [];
        for (let k = 0; k < 10; k++) {
            t0 = performance.now();
            await tryReq(() => d.organize(file), 0);
            organizeMs.push(performance.now() - t0);
        }

        // Keystrokes.
        const lines = text.split('\n');
        let line = lines.length - 1;
        let col = lines[line].length;
        let name = '__bench_';
        await d.change(file, line, col, '\n' + name, (ds) => ds.some((m) => m.includes(`'${name}'`)));
        line++;
        col = name.length;
        /** @type {number[]} */
        const keyMs = [];
        /** @type {any} */
        let lastDiags = null;
        const alphabet = 'abcdefghijklmnopqrstuvwxyz';
        for (let k = 0; k < keys; k++) {
            const ch = alphabet[k % alphabet.length];
            name += ch;
            const want = `'${name}'`;
            t0 = performance.now();
            lastDiags = await d.change(file, line, col, ch, (ds) => ds.some((m) => m.includes(want)));
            const dt = performance.now() - t0;
            keyMs.push(dt);
            col++;
            if (dt < interval) await sleep(interval - dt);
        }
        // Quick fixes for the typed name's TS2304 (cold: a new identifier).
        let fixAfterKeyMs = null;
        if (Array.isArray(lastDiags)) {
            t0 = performance.now();
            await tryReq(() => d.codeActions(file, line, 2, lastDiags), 0);
            fixAfterKeyMs = Math.round((performance.now() - t0) * 100) / 100;
        }
        await sleep(500);
        const pid = d.pid();
        const rssOne = pid ? rssMB(pid) : null;

        let rssAll = null;
        let openAllMs = null;
        let filesOpen = 1;
        if (openAll) {
            const all = sources().filter((p) => p !== file);
            t0 = performance.now();
            await d.openMany(all);
            openAllMs = Math.round(performance.now() - t0);
            filesOpen += all.length;
            await sleep(settleMs);
            rssAll = pid ? rssMB(pid) : null;
        }
        const r2 = (/** @type {number} */ x) => Math.round(x * 10) / 10;
        const result = {
            server: d.label,
            file: path.relative(root, file),
            bytes: Buffer.byteLength(text),
            diagnosticsOnOpen: openDiags.length,
            sound: soundLevel || null,
            soundDiagnosticsAfterKeys: Array.isArray(lastDiags) ? lastDiags.filter((/** @type {any} */ x) => x && x.source === 'ts-r sound').length : null,
            firstDiagMs: Math.round(firstDiagMs),
            hover: { n: hoverMs.length, hits: hoverHits, p50: pct(hoverMs, 0.5), p95: pct(hoverMs, 0.95) },
            definition: { n: defMs.length, hits: defHits, p50: pct(defMs, 0.5), p95: pct(defMs, 0.95) },
            completion: { n: complMs.length, hits: complHits, p50: pct(complMs, 0.5), p95: pct(complMs, 0.95) },
            codeAction: { n: actionMs.length, hits: actionHits, p50: pct(actionMs, 0.5), p95: pct(actionMs, 0.95) },
            organizeImports: { n: organizeMs.length, p50: pct(organizeMs, 0.5), p95: pct(organizeMs, 0.95) },
            fixAfterKeyMs,
            keystrokes: { n: keyMs.length, intervalMs: interval, p50: pct(keyMs, 0.5), p95: pct(keyMs, 0.95), max: keyMs.length ? r2(Math.max(...keyMs)) : null },
            rssMB: rssOne,
            filesOpen,
            openAllMs,
            rssAllMB: rssAll,
            requestErrors: errors,
        };
        console.log(JSON.stringify(result));
        if (dumpFile) fs.writeFileSync(dumpFile, JSON.stringify(dump, null, 1));
    } catch (e) {
        console.error(String(e && /** @type {any} */ (e).stack ? /** @type {any} */ (e).stack : e));
        process.exitCode = 1;
    } finally {
        await d.stop();
    }
}

main();
