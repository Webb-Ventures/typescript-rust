// @ts-check
// ts-r tsserver stand-in. The extension copies this file (and the other files of standin/) to
// <globalStorage>/tsdk/lib/ and points VS Code's `typescript.tsdk` (or `js/ts.tsdk.path`) at
// that lib dir, so VS Code's own TypeScript extension starts it exactly as it would start
// tsserver.
//
// - `ts-r.checker` `auto` / `on` (L3): router.js answers from the ts-r checker for projects it
//   serves and from `ts-r-lsp tsserver` (the workspace daemon's shared tsserver) for the rest.
// - `off`: everything goes to `ts-r-lsp tsserver` unchanged (L1).
//
// Settings come from ../ts-r.json (written by the extension). When anything is missing (no
// ts-r-lsp binary, a syntax-only server request, the daemon cannot start), this file loads
// VS Code's bundled tsserver in-process instead, so TypeScript keeps working.
'use strict';

const fs = require('fs');
const path = require('path');
const childProcess = require('child_process');

/** @typedef {{bin?: string, fallback?: string, settings?: Record<string, unknown>, checker?: string}} StandInConfig */

/** @returns {StandInConfig} */
function readConfig() {
    try {
        return JSON.parse(fs.readFileSync(path.join(__dirname, '..', 'ts-r.json'), 'utf8'));
    } catch {
        return {};
    }
}

const config = readConfig();
const args = process.argv.slice(2);

/** Runs VS Code's bundled tsserver in this process (same argv, same IPC channel). */
function fallback(/** @type {string} */ why) {
    process.stderr.write(`[ts-r] using the bundled tsserver: ${why}\n`);
    if (!config.fallback) {
        process.stderr.write('[ts-r] no bundled tsserver recorded in ts-r.json\n');
        process.exit(1);
    }
    require(path.join(config.fallback, 'tsserver.js'));
}

function argValue(/** @type {string} */ name) {
    const i = args.indexOf(name);
    return i >= 0 ? args[i + 1] : undefined;
}

/** CLI flags for the user's ts-r.* settings. */
function settingFlags() {
    const s = config.settings || {};
    /** @type {string[]} */
    const out = [];
    if (typeof s.memoryBudgetMB === 'number') out.push('--memory-budget-mb', String(Math.round(s.memoryBudgetMB)));
    if (typeof s.idleShutdownMinutes === 'number') out.push('--idle-shutdown-minutes', String(s.idleShutdownMinutes));
    if (typeof s.lint === 'string') out.push('--lint', s.lint);
    return out;
}

function main() {
    const mode = argValue('--serverMode');
    if (mode && mode !== 'semantic') return fallback(`--serverMode ${mode} is not shared`);
    if (process.env.TS_R_STANDIN_DISABLE) return fallback('TS_R_STANDIN_DISABLE is set');
    const bin = config.bin;
    if (!bin || !fs.existsSync(bin)) return fallback(`ts-r-lsp not found (${bin || 'unset'})`);

    const root = process.env.TS_R_WORKSPACE_ROOT;
    const common = [...settingFlags(), '--node', process.execPath];
    if (config.fallback) common.push('--fallback-tsserver', path.join(config.fallback, 'tsserver.js'));
    if (root) {
        common.unshift('--root', root);
        // Start (or find) the daemon now, so a broken install falls back before any message
        // has been exchanged.
        try {
            childProcess.execFileSync(bin, ['ensure-daemon', ...common], { stdio: ['ignore', 'ignore', 'pipe'], timeout: 20000 });
        } catch (e) {
            return fallback(`daemon did not start: ${e instanceof Error ? e.message : e}`);
        }
    }

    const ipc = args.includes('--useNodeIpc') && typeof process.send === 'function';
    const name = `VS Code ${process.pid}`;
    const checker = process.env.TS_R_CHECKER || config.checker || 'off';
    if (root && (checker === 'auto' || checker === 'on')) return route(bin, common, name, checker, ipc);
    const front = childProcess.spawn(bin, [...common, '--name', name, 'tsserver', ...args.filter((a) => a !== '--useNodeIpc')], {
        stdio: ['pipe', 'pipe', 'inherit'],
        windowsHide: true,
    });
    front.on('error', (e) => {
        process.stderr.write(`[ts-r] ${e.message}\n`);
        process.exit(1);
    });
    front.on('exit', (code, signal) => process.exit(code ?? (signal ? 1 : 0)));

    if (!ipc) {
        process.stdin.pipe(front.stdin);
        front.stdout.pipe(process.stdout);
        return;
    }
    const send = /** @type {(m: unknown) => boolean} */ (process.send).bind(process);
    process.on('message', (m) => front.stdin.write(JSON.stringify(m) + '\n'));
    process.on('disconnect', () => front.stdin.end());
    // The front answers with `Content-Length` frames; IPC wants one object per message.
    let buf = Buffer.alloc(0);
    front.stdout.on('data', (/** @type {Buffer} */ chunk) => {
        buf = Buffer.concat([buf, chunk]);
        for (;;) {
            const headerEnd = buf.indexOf('\r\n\r\n');
            if (headerEnd < 0) return;
            const header = buf.subarray(0, headerEnd).toString('ascii');
            const m = /Content-Length:\s*(\d+)/i.exec(header);
            if (!m) {
                buf = buf.subarray(headerEnd + 4);
                continue;
            }
            const len = Number(m[1]);
            if (buf.length < headerEnd + 4 + len) return;
            const body = buf.subarray(headerEnd + 4, headerEnd + 4 + len).toString('utf8');
            buf = buf.subarray(headerEnd + 4 + len);
            try {
                send(JSON.parse(body));
            } catch (e) {
                process.stderr.write(`[ts-r] bad message from ts-r-lsp: ${e}\n`);
            }
        }
    });
}

/**
 * `ts-r.checker` auto / on: the tsserver bridge (router.js) between VS Code and two engines.
 * @param {string} bin
 * @param {string[]} common
 * @param {string} name
 * @param {'auto' | 'on'} mode
 * @param {boolean} ipc
 */
function route(bin, common, name, mode, ipc) {
    const { Router } = require('./router');
    /** @param {unknown} m */
    const send = ipc
        ? (/** @type {unknown} */ m) => /** @type {(m: unknown) => boolean} */ (process.send)(m)
        : (/** @type {unknown} */ m) => {
              const json = JSON.stringify(m);
              process.stdout.write(`Content-Length: ${Buffer.byteLength(json, 'utf8')}\r\n\r\n${json}`);
          };
    const idle = Number(process.env.TS_R_BRIDGE_IDLE_MS);
    const router = new Router({
        bin,
        common,
        name,
        args,
        mode,
        bundled: config.fallback,
        tsdkDir: path.join(__dirname, '..'),
        send,
        log: (line) => process.stderr.write(line + '\n'),
        exit: (code) => process.exit(code),
        idleMs: Number.isFinite(idle) && idle > 0 ? idle : undefined,
    });
    if (ipc) {
        process.on('message', (m) => router.onClientMessage(m));
        process.on('disconnect', () => router.shutdown());
        return;
    }
    let pending = '';
    process.stdin.setEncoding('utf8');
    process.stdin.on('data', (/** @type {string} */ chunk) => {
        pending += chunk;
        let nl;
        while ((nl = pending.indexOf('\n')) >= 0) {
            const line = pending.slice(0, nl).trim();
            pending = pending.slice(nl + 1);
            if (!line) continue;
            try {
                router.onClientMessage(JSON.parse(line));
            } catch {
                // Not a message.
            }
        }
    });
    process.stdin.on('end', () => router.shutdown());
}

main();
