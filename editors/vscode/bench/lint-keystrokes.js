// @ts-check
// Lint latency per keystroke and server memory, measured through LSP with a scripted client
// (no VS Code). Types one character at a time into a trailing `//` comment of a file (so
// every keystroke parses) and times didChange -> diagnostics.
//
//   ts-r:   node bench/lint-keystrokes.js ts-r <ts-r-lsp> <root> <file> [--keys N]
//           [--interval MS] [--lint on|off] [--open-all] [--src-dirs a,b]
//           Lint-only connection to the shared daemon (like the extension); the latency is
//           didChange -> textDocument/publishDiagnostics and includes the daemon's debounce
//           (ts-r.lint.debounceMs, TS_R_LSP_LINT_DEBOUNCE_MS). Memory is the daemon's RSS.
//   eslint: node bench/lint-keystrokes.js eslint <eslintServer.js> <root> <file> [...]
//           The ESLint extension's own language server (dbaeumer.vscode-eslint, pull
//           diagnostics) run headlessly: didChange + textDocument/diagnostic, timed to the
//           response. Memory is the server process's RSS.
//
// Run it under scripts/limit.sh. It prints one JSON line. Stop the ts-r daemon afterwards
// (`ts-r-lsp stop --root <root>`).
'use strict';

const fs = require('fs');
const path = require('path');
const childProcess = require('child_process');
const { LspClient } = require('../standin/lspclient');

const [kind, server, rootArg, fileArg, ...rest] = process.argv.slice(2);
if (!kind || !server || !rootArg || !fileArg) {
    console.error('usage: lint-keystrokes.js ts-r|eslint <server> <root> <file> [--keys N] [--interval MS] [--lint on|off] [--open-all]');
    process.exit(2);
}
/** @param {string} name @param {string} dflt */
function opt(name, dflt) {
    const i = rest.indexOf(name);
    return i >= 0 && i + 1 < rest.length ? rest[i + 1] : dflt;
}
const keys = Number(opt('--keys', '300'));
const interval = Number(opt('--interval', '100'));
const lintEnable = opt('--lint', 'on');
const openAll = rest.includes('--open-all');
// Directories under <root> that --open-all walks.
const srcDirs = opt('--src-dirs', 'src,packages,apps').split(',').filter(Boolean);
const root = fs.realpathSync(rootArg);
const file = path.resolve(root, fileArg);
const rootUri = `file://${root}`;
const uriOf = (/** @type {string} */ p) => `file://${p.split(path.sep).map(encodeURIComponent).join('/')}`;
const sleep = (/** @type {number} */ ms) => new Promise((r) => setTimeout(r, ms));

/** @param {number} pid */
function rssMB(pid) {
    const r = childProcess.spawnSync('ps', ['-o', 'rss=', '-p', String(pid)], { encoding: 'utf8' });
    return Math.round((Number(r.stdout.trim()) / 1024) * 10) / 10;
}

/** @param {number[]} xs @param {number} q */
function pct(xs, q) {
    const s = [...xs].sort((a, b) => a - b);
    return Math.round(s[Math.min(s.length - 1, Math.floor(q * s.length))] * 100) / 100;
}

/** Linted sources under the workspace packages (the files an editor session could open). */
function sources() {
    /** @type {string[]} */
    const out = [];
    /** @param {string} dir */
    const walk = (dir) => {
        for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
            if (['node_modules', 'dist', '.next', '.wrangler', 'coverage', 'generated', '.git'].includes(e.name)) continue;
            const p = path.join(dir, e.name);
            if (e.isDirectory()) walk(p);
            else if (/\.(ts|tsx|js|jsx|mjs)$/.test(e.name) && !/\.(gen|d)\.ts$/.test(e.name)) out.push(p);
        }
    };
    for (const d of srcDirs) if (fs.existsSync(path.join(root, d))) walk(path.join(root, d));
    return out.sort();
}

/** @param {string} p */
function languageId(p) {
    return p.endsWith('.tsx') ? 'typescriptreact' : p.endsWith('.ts') ? 'typescript' : p.endsWith('.jsx') ? 'javascriptreact' : 'javascript';
}

/** An LSP client that answers ESLint's configuration pulls the way the extension does. */
class EslintClient extends LspClient {
    /** @param {any} msg */
    dispatch(msg) {
        if (msg.method === 'workspace/configuration' && msg.id !== undefined) {
            const settings = {
                validate: 'on',
                packageManager: 'npm',
                useESLintClass: false,
                useFlatConfig: true,
                experimental: {},
                codeAction: { disableRuleComment: { enable: true, location: 'separateLine', commentStyle: 'line' }, showDocumentation: { enable: true } },
                codeActionOnSave: { mode: 'all' },
                format: false,
                quiet: false,
                onIgnoredFiles: 'off',
                options: {},
                rulesCustomizations: [],
                run: 'onType',
                problems: { shortenToSingleLine: false },
                nodePath: null,
                workspaceFolder: { name: path.basename(root), uri: rootUri },
                workingDirectory: { mode: 'location' },
                useRealpaths: false,
            };
            this.write({ jsonrpc: '2.0', id: msg.id, result: (msg.params?.items || []).map(() => settings) });
            return;
        }
        if (msg.method === 'workspace/workspaceFolders' && msg.id !== undefined) {
            this.write({ jsonrpc: '2.0', id: msg.id, result: [{ uri: rootUri, name: path.basename(root) }] });
            return;
        }
        super.dispatch(msg);
    }
}

async function main() {
    const isTsr = kind === 'ts-r';
    /** @type {string[]} */
    const log = [];
    const c = isTsr
        ? new LspClient(server, ['--lint-only', '--root', root, '--name', 'lint bench'], { cwd: root, log: (l) => log.push(l) })
        : new EslintClient(process.execPath, [server, '--stdio'], { cwd: root, log: (l) => log.push(l) });
    /** @type {Map<string, (p: any) => void>} */
    const waiters = new Map();
    c.onNotification('textDocument/publishDiagnostics', (p) => {
        const w = waiters.get(p.uri);
        if (w) {
            waiters.delete(p.uri);
            w(p);
        }
    });
    /** @type {any[]} */
    const messages = [];
    c.onNotification('window/showMessage', (p) => messages.push(p.message));
    /**
     * Sends a document notification, then resolves with the diagnostics that follow it:
     * pushed by ts-r, pulled from ESLint.
     * @param {string} uri @param {boolean} lintOn @param {string} method @param {any} params
     * @returns {Promise<any[]>}
     */
    const send = (uri, lintOn, method, params) => {
        if (!lintOn) {
            c.notify(method, params);
            return Promise.resolve([]);
        }
        if (!isTsr) {
            c.notify(method, params);
            return c.request('textDocument/diagnostic', { textDocument: { uri } }).then((r) => r.items);
        }
        const p = new Promise((resolve, reject) => {
            const t = setTimeout(() => reject(new Error(`no diagnostics for ${uri}`)), 60_000);
            waiters.set(uri, (d) => {
                clearTimeout(t);
                resolve(d.diagnostics);
            });
        });
        c.notify(method, params);
        return /** @type {Promise<any[]>} */ (p);
    };

    try {
        await c.request('initialize', {
            processId: process.pid,
            rootUri,
            workspaceFolders: [{ uri: rootUri, name: path.basename(root) }],
            capabilities: {
                workspace: { configuration: true, workspaceFolders: true, didChangeWatchedFiles: { dynamicRegistration: false } },
                textDocument: {
                    synchronization: { didSave: true },
                    publishDiagnostics: { codeDescriptionSupport: true },
                    diagnostic: { dynamicRegistration: false },
                    codeAction: { codeActionLiteralSupport: { codeActionKind: { valueSet: ['quickfix', 'source.fixAll'] } } },
                },
            },
            initializationOptions: isTsr ? { lintOnly: true, 'ts-r': { lint: { enable: lintEnable } } } : {},
        });
        c.notify('initialized', {});
        if (!isTsr) c.notify('workspace/didChangeConfiguration', { settings: null });
        const lintOn = !isTsr || lintEnable !== 'off';

        // Open the file (cold: config evaluation, first lint).
        let text = fs.readFileSync(file, 'utf8');
        const uri = uriOf(file);
        let version = 1;
        let t0 = performance.now();
        const openDiags = await send(uri, lintOn, 'textDocument/didOpen', { textDocument: { uri, languageId: languageId(file), version, text } });
        const openMs = performance.now() - t0;

        // Optionally every other source too (memory with a whole workspace open).
        let opened = 1;
        let openAllMs = 0;
        if (openAll) {
            t0 = performance.now();
            for (const p of sources()) {
                if (p === file) continue;
                const u = uriOf(p);
                await send(u, lintOn, 'textDocument/didOpen', { textDocument: { uri: u, languageId: languageId(p), version: 1, text: fs.readFileSync(p, 'utf8') } });
                opened++;
            }
            openAllMs = performance.now() - t0;
        }

        // Start a trailing comment, then type into it.
        const lines = text.split('\n');
        let line = lines.length - 1;
        let col = lines[line].length;
        /** @param {string} s */
        const insert = async (s) => {
            const start = performance.now();
            await send(uri, lintOn, 'textDocument/didChange', {
                textDocument: { uri, version: ++version },
                contentChanges: [{ range: { start: { line, character: col }, end: { line, character: col } }, text: s }],
            });
            const nl = s.lastIndexOf('\n');
            if (nl >= 0) {
                line += s.split('\n').length - 1;
                col = s.length - nl - 1;
            } else col += s.length;
            text += s;
            return performance.now() - start;
        };
        await insert('\n// ');
        /** @type {number[]} */
        const times = [];
        const alphabet = 'the quick brown fox jumps over the lazy dog ';
        if (lintOn) {
            for (let i = 0; i < keys; i++) {
                const dt = await insert(alphabet[i % alphabet.length]);
                times.push(dt);
                if (dt < interval) await sleep(interval - dt);
            }
        } else {
            await sleep(1000);
        }

        /** @type {any} */
        let status = {};
        let pid = c.child.pid ?? 0;
        if (isTsr) {
            const r = childProcess.spawnSync(server, ['status', '--root', root], { encoding: 'utf8' });
            try {
                status = JSON.parse(r.stdout);
            } catch {
                status = { error: r.stdout + r.stderr };
            }
            pid = status.daemonPid ?? 0;
        }
        const result = {
            server: isTsr ? 'ts-r-lsp --lint-only' : 'vscode-eslint server',
            file: path.relative(root, file),
            bytes: Buffer.byteLength(text),
            lint: lintOn ? 'on' : 'off',
            problemsOnOpen: openDiags.length,
            rulesOnOpen: openDiags.reduce((/** @type {Record<string, number>} */ m, /** @type {any} */ d) => {
                const k = String(typeof d.code === 'object' && d.code ? d.code.value : d.code);
                m[k] = (m[k] || 0) + 1;
                return m;
            }, {}),
            openMs: Math.round(openMs),
            filesOpen: opened,
            openAllMs: Math.round(openAllMs),
            keystrokes: times.length,
            intervalMs: interval,
            p50: times.length ? pct(times, 0.5) : null,
            p95: times.length ? pct(times, 0.95) : null,
            max: times.length ? Math.round(Math.max(...times) * 100) / 100 : null,
            rssMB: pid ? rssMB(pid) : null,
            lintStats: status.lint,
            debounceMs: status.settings?.lintDebounceMs,
            messages,
        };
        console.log(JSON.stringify(result));
    } catch (e) {
        console.error(String(e));
        for (const l of log.slice(-30)) console.error(l);
        process.exitCode = 1;
    } finally {
        await c.stop();
    }
}

main();
