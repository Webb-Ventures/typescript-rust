// @ts-check
// A minimal LSP client over a child process's stdio (Content-Length frames both ways). It is
// all the extension needs for lint: requests with responses, notifications both ways, and
// answers to the few server-to-client requests. No VS Code imports, so tests drive it with
// plain Node.
'use strict';

const childProcess = require('child_process');

/**
 * @typedef {{ jsonrpc: '2.0', id?: number | string | null, method?: string, params?: any, result?: any, error?: { code: number, message: string } }} Message
 */

class LspClient {
    /**
     * @param {string} bin
     * @param {string[]} args
     * @param {{ cwd?: string, env?: NodeJS.ProcessEnv, log?: (line: string) => void }} [opts]
     */
    constructor(bin, args, opts = {}) {
        /** @type {Map<number, { resolve: (v: any) => void, reject: (e: Error) => void }>} */
        this.pending = new Map();
        /** @type {Map<string, (params: any) => void>} */
        this.handlers = new Map();
        this.nextId = 1;
        this.closed = false;
        /** @type {Buffer} */
        this.buffer = Buffer.alloc(0);
        this.log = opts.log || (() => {});
        /** @type {((code: number | null) => void)[]} */
        this.exitListeners = [];
        this.child = childProcess.spawn(bin, args, {
            cwd: opts.cwd,
            env: opts.env || process.env,
            stdio: ['pipe', 'pipe', 'pipe'],
        });
        this.child.stdout.on('data', (/** @type {Buffer} */ chunk) => this.onData(chunk));
        this.child.stderr.on('data', (/** @type {Buffer} */ chunk) => {
            for (const line of chunk.toString('utf8').split('\n')) if (line) this.log(line);
        });
        this.child.on('error', (e) => {
            this.log(`ts-r-lsp: ${e.message}`);
            this.onExit(null);
        });
        this.child.on('exit', (code) => this.onExit(code));
        this.child.stdin.on('error', () => {});
    }

    /** @param {number | null} code */
    onExit(code) {
        if (this.closed) return;
        this.closed = true;
        for (const p of this.pending.values()) p.reject(new Error('ts-r-lsp exited'));
        this.pending.clear();
        for (const l of this.exitListeners) l(code);
    }

    /** @param {(code: number | null) => void} f */
    onDidExit(f) {
        this.exitListeners.push(f);
    }

    /** @param {Buffer} chunk */
    onData(chunk) {
        this.buffer = Buffer.concat([this.buffer, chunk]);
        for (;;) {
            const headerEnd = this.buffer.indexOf('\r\n\r\n');
            if (headerEnd < 0) return;
            const header = this.buffer.subarray(0, headerEnd).toString('ascii');
            const m = /Content-Length:\s*(\d+)/i.exec(header);
            if (!m) {
                // Not a frame: drop through the header and resynchronise.
                this.buffer = this.buffer.subarray(headerEnd + 4);
                continue;
            }
            const len = Number(m[1]);
            const start = headerEnd + 4;
            if (this.buffer.length < start + len) return;
            const body = this.buffer.subarray(start, start + len).toString('utf8');
            this.buffer = this.buffer.subarray(start + len);
            /** @type {Message} */
            let msg;
            try {
                msg = JSON.parse(body);
            } catch {
                continue;
            }
            this.dispatch(msg);
        }
    }

    /** @param {Message} msg */
    dispatch(msg) {
        if (msg.method !== undefined && msg.id !== undefined && msg.id !== null) {
            // Server-to-client request: answer what an editor would.
            let result = null;
            if (msg.method === 'workspace/configuration') {
                result = (msg.params?.items || []).map(() => null);
            }
            const h = this.handlers.get(msg.method);
            if (h) h(msg.params);
            this.write({ jsonrpc: '2.0', id: msg.id, result });
            return;
        }
        if (msg.method !== undefined) {
            const h = this.handlers.get(msg.method);
            if (h) h(msg.params);
            return;
        }
        if (typeof msg.id === 'number') {
            const p = this.pending.get(msg.id);
            if (!p) return;
            this.pending.delete(msg.id);
            if (msg.error) p.reject(new Error(msg.error.message));
            else p.resolve(msg.result);
        }
    }

    /** @param {Message} msg */
    write(msg) {
        if (this.closed) return;
        const body = Buffer.from(JSON.stringify(msg), 'utf8');
        this.child.stdin.write(`Content-Length: ${body.length}\r\n\r\n`);
        this.child.stdin.write(body);
    }

    /**
     * @param {string} method
     * @param {any} params
     * @returns {Promise<any>}
     */
    request(method, params) {
        if (this.closed) return Promise.reject(new Error('ts-r-lsp is not running'));
        const id = this.nextId++;
        return new Promise((resolve, reject) => {
            this.pending.set(id, { resolve, reject });
            this.write({ jsonrpc: '2.0', id, method, params });
        });
    }

    /** @param {string} method @param {any} params */
    notify(method, params) {
        this.write({ jsonrpc: '2.0', method, params });
    }

    /** @param {string} method @param {(params: any) => void} handler */
    onNotification(method, handler) {
        this.handlers.set(method, handler);
    }

    /** `shutdown` + `exit`, then kills the process if it is still there after `ms`. */
    async stop(ms = 2000) {
        if (this.closed) return;
        const exited = new Promise((resolve) => this.onDidExit(resolve));
        try {
            await Promise.race([this.request('shutdown', null), new Promise((r) => setTimeout(r, ms))]);
        } catch {
            // Already gone.
        }
        this.notify('exit', null);
        const timer = setTimeout(() => this.child.kill(), ms);
        await exited;
        clearTimeout(timer);
    }
}

module.exports = { LspClient };
