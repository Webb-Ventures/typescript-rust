// @ts-check
// The tsserver bridge (L3): the stand-in speaks tsserver to VS Code's TypeScript extension and
// picks an engine per project.
//
// - **ts-r** (an LSP connection to `ts-r-lsp --type-backend ts-r`, the workspace daemon's
//   checker): projects whose parity check passed (`ts-r.checker: auto`) or every project
//   (`on`). Diagnostics, hover, completions, definitions, references, highlights, rename,
//   signature help, semantic tokens, outline and project info are translated here.
// - **TypeScript** (`ts-r-lsp tsserver`, the daemon's shared tsserver, today's L1 path):
//   every other project, and, *on demand*, whatever ts-r cannot answer yet for a ts-r file
//   (quick fixes, refactors, organize imports, inlay hints, call hierarchy, ...). It starts at
//   the first such request and stops after `idleMs` without one when no file needs it.
// - **syntax** (VS Code's bundled tsserver, `--serverMode syntactic`, no program, no libs): on
//   demand for formatting, selection ranges, JSX closing tags, linked editing and doc comment
//   templates in ts-r files. Folding in ts-r files is VS Code's indentation folding (with
//   `#region` markers): asking a server for it on every open would keep one running.
//
// VS Code's TypeScript extension stays the only UI, so nothing is shown twice. See ../AGENTS.md.
'use strict';

const fs = require('fs');
const path = require('path');
const childProcess = require('child_process');
const { LspClient } = require('./lspclient');
const C = require('./convert');

/** Sequence numbers of the bridge's own tsserver requests (VS Code's never get this high). */
const INTERNAL = 1_000_000_000;
const INFERRED = '(inferred)';

/** Commands translated to the ts-r checker for ts-r files. */
const TSR_COMMANDS = new Set([
    'quickinfo',
    'completionInfo',
    'completionEntryDetails',
    'definition',
    'definitionAndBoundSpan',
    'typeDefinition',
    'references',
    'documentHighlights',
    'rename',
    'signatureHelp',
    'encodedSemanticClassifications-full',
    'navtree',
    'projectInfo',
]);

/** Syntax-only commands: the syntax server for ts-r files. */
const SYNTAX_COMMANDS = new Set([
    'format',
    'formatonkey',
    'selectionRange',
    'linkedEditingRange',
    'jsxClosingTag',
    'docCommentTemplate',
    'getSpanOfEnclosingComment',
    'indentation',
    'braceCompletion',
    'brace',
    'todoComments',
    'toggleLineComment',
    'toggleMultilineComment',
    'commentSelection',
    'uncommentSelection',
]);

const DIAG_EVENTS = new Set(['syntaxDiag', 'semanticDiag', 'suggestionDiag', 'regionSemanticDiag']);

/** `languageId` of a TypeScript file opened in ts-r only for its text (crates/ts_r_lsp/src/tsr/bridge.rs). */
const MIRROR_LANGUAGE_ID = 'ts-r-mirror';

/**
 * @typedef {{
 *   file: string, uri: string, text: import('./convert').TextDoc, version: number,
 *   scriptKindName?: string, projectRootPath?: string, engine: 'tsr' | 'ts', key?: string,
 *   inR: boolean, inT: boolean, inS: boolean, diags: any[] | null,
 *   rMirror?: boolean, tProject?: string,
 * }} Doc
 * `rMirror`: a TypeScript file open in ts-r as a mirror (a ts-r project uses it). `tProject`:
 * for a TypeScript file, its TypeScript project (`projectInfo`'s configFileName), if known.
 * @typedef {{project?: string, config?: string | null, state?: string, parity?: any, openFiles?: number, configDiagnostics?: any[]}} ProjectInfo
 */

/** A tsserver child: the `ts-r-lsp tsserver` front (JSON lines in, frames out) or an IPC fork. */
class TsChild {
    /**
     * @param {childProcess.ChildProcess} child
     * @param {'frames' | 'ipc'} kind
     * @param {(m: any) => void} onMessage
     * @param {(code: number | null) => void} onExit
     */
    constructor(child, kind, onMessage, onExit) {
        this.child = child;
        this.kind = kind;
        this.exited = false;
        child.on('exit', (code) => {
            this.exited = true;
            onExit(code);
        });
        child.on('error', () => {
            if (!this.exited) {
                this.exited = true;
                onExit(null);
            }
        });
        if (kind === 'ipc') {
            child.on('message', onMessage);
            return;
        }
        child.stdin?.on('error', () => {});
        let buf = Buffer.alloc(0);
        child.stdout?.on('data', (/** @type {Buffer} */ chunk) => {
            buf = Buffer.concat([buf, chunk]);
            for (;;) {
                const headerEnd = buf.indexOf('\r\n\r\n');
                if (headerEnd < 0) return;
                const m = /Content-Length:\s*(\d+)/i.exec(buf.subarray(0, headerEnd).toString('ascii'));
                if (!m) {
                    buf = buf.subarray(headerEnd + 4);
                    continue;
                }
                const len = Number(m[1]);
                if (buf.length < headerEnd + 4 + len) return;
                const body = buf.subarray(headerEnd + 4, headerEnd + 4 + len).toString('utf8');
                buf = buf.subarray(headerEnd + 4 + len);
                try {
                    onMessage(JSON.parse(body));
                } catch {
                    // Not JSON: skip it.
                }
            }
        });
    }

    /** @param {any} msg */
    send(msg) {
        if (this.exited) return;
        if (this.kind === 'ipc') this.child.send?.(msg);
        else this.child.stdin?.write(JSON.stringify(msg) + '\n');
    }

    stop() {
        if (this.exited) return;
        this.send({ seq: INTERNAL - 1, type: 'request', command: 'exit' });
        if (this.kind === 'frames') this.child.stdin?.end();
        const t = setTimeout(() => this.child.kill(), 3000);
        t.unref();
    }

    get pid() {
        return this.child.pid;
    }
}

class Router {
    /**
     * @param {{
     *   bin: string, common: string[], name: string, args: string[], mode: 'auto' | 'on', bundled?: string,
     *   tsdkDir: string, send: (m: any) => void, log: (line: string) => void, exit: (code: number) => void,
     *   idleMs?: number, settleMs?: number,
     * }} o
     */
    constructor(o) {
        this.o = o;
        this.mode = o.mode;
        /** @type {Map<string, Doc>} */
        this.docs = new Map();
        /** @type {Map<string, ProjectInfo>} */
        this.projects = new Map();
        /** @type {any} */
        this.lastStatus = null;
        /** @type {Promise<void>} */
        this.queue = Promise.resolve();
        this.nextInternal = INTERNAL;
        /** @type {TsChild | undefined} */
        this.T = undefined;
        /** @type {TsChild | undefined} */
        this.S = undefined;
        this.tStarts = 0;
        /** Why TypeScript last started: a command ts-r has no answer for, or `a TypeScript project`. */
        this.tStartedFor = '';
        /** @type {Record<string, number>} requests for ts-r files answered without TypeScript */
        this.declined = {};
        /** Which engine listed a file's last refactors (`getEditsForRefactor` asks the same one). @type {Map<string, 'tsr' | 'ts'>} */
        this.refactorEngine = new Map();
        /** ts-r's fix-all edits from the last `getCodeFixes`, by file and fix id. @type {Map<string, {version: number, changes: any[]}>} */
        this.fixAlls = new Map();
        /** @type {string | undefined} the command being routed to TypeScript */
        this.why = undefined;
        this.lastT = 0;
        this.lastS = 0;
        /** @type {Set<number>} */
        this.watchers = new Set();
        /** The bridge's own TypeScript requests waiting for an answer. @type {Map<number, (m: any) => void>} */
        this.tWaiting = new Map();
        /** TypeScript's projects (of the files it serves) and their files. @type {Map<string, Set<string>>} */
        this.tProjects = new Map();
        /** @type {NodeJS.Timeout | undefined} */
        this.mirrorTimer = undefined;
        this.mirrorDue = 0;
        /** @type {any} */
        this.configureArgs = undefined;
        /** `configure`'s format options, by file (`''`: the default), for ts-r's refactors. @type {Map<string, any>} */
        this.formatOptions = new Map();
        /** @type {any} */
        this.inferredArgs = undefined;
        /** @type {Map<string, any>} */
        this.pluginConfigs = new Map();
        /** @type {(() => void)[]} */
        this.statusWaiters = [];
        this.statusCount = 0;
        /** Number of the last `ts-r/status` that listed each project. @type {Map<string, number>} */
        this.statusSeen = new Map();
        this.rBroken = false;
        this.stopping = false;
        /** @type {{tokenTypes: string[], tokenModifiers: string[]}} */
        this.legend = { tokenTypes: [], tokenModifiers: [] };
        this.idleMs = o.idleMs ?? 180_000;
        this.settleMs = o.settleMs ?? 1500;
        this.R = this.startR();
        const timer = setInterval(() => this.reapIdle(), Math.min(15_000, Math.max(250, this.idleMs / 4)));
        timer.unref();
    }

    // -----------------------------------------------------------------------------------------
    // Engines
    // -----------------------------------------------------------------------------------------

    startR() {
        const r = new LspClient(this.o.bin, [...this.o.common, '--name', `${this.o.name} (ts-r)`, '--type-backend', 'ts-r', '--lint', 'off'], {
            log: (line) => this.o.log(`[ts-r] ${line}`),
        });
        r.onNotification('textDocument/publishDiagnostics', (p) => this.onRDiagnostics(p));
        r.onNotification('ts-r/status', (st) => this.onStatus(st));
        // A ts-r project's file set changed: it may now use (or no longer use) TypeScript files.
        r.onNotification('ts-r/inputsChanged', () => this.scheduleMirrors());
        r.onNotification('window/logMessage', (p) => this.o.log(`[ts-r] ${p?.message}`));
        r.onDidExit(() => {
            if (this.stopping) return;
            this.o.log('[ts-r] the ts-r checker connection closed; using TypeScript for every file');
            this.rBroken = true;
            this.enqueue(() => this.rerouteAll());
        });
        this.rReady = r
            .request('initialize', {
                processId: process.pid,
                rootUri: null,
                capabilities: { general: { positionEncodings: ['utf-16'] }, textDocument: { publishDiagnostics: { relatedInformation: true, tagSupport: { valueSet: [1, 2] } } } },
                initializationOptions: { bridge: 'vscode-tsserver' },
            })
            .then((res) => {
                const legend = res?.capabilities?.semanticTokensProvider?.legend;
                if (legend) this.legend = legend;
                const backend = res?.serverInfo?.backend?.name || res?.serverInfo?.name;
                if (backend !== 'ts-r') throw new Error(`the daemon gave this connection ${backend || 'another backend'}, not the ts-r checker`);
                r.notify('initialized', {});
            })
            .catch((e) => {
                this.o.log(`[ts-r] checker unavailable (${e instanceof Error ? e.message : e}); using TypeScript`);
                this.rBroken = true;
            });
        return r;
    }

    ensureT(/** @type {string} */ reason = '') {
        if (this.T) return this.T;
        this.tStarts++;
        this.tStartedFor = reason || this.why || 'a TypeScript project';
        const args = this.o.args.filter((a) => a !== '--useNodeIpc');
        const child = childProcess.spawn(this.o.bin, [...this.o.common, '--name', this.o.name, 'tsserver', ...args], {
            stdio: ['pipe', 'pipe', 'inherit'],
            windowsHide: true,
        });
        const t = new TsChild(
            child,
            'frames',
            (m) => this.onT(m),
            (code) => this.onTExit(t, code),
        );
        this.T = t;
        this.o.log(`[ts-r] started TypeScript (pid ${child.pid}) for ${this.tStartedFor}`);
        this.replayConfig(t);
        return t;
    }

    ensureS() {
        if (this.S) return this.S;
        if (!this.o.bundled) return undefined;
        const args = ['--serverMode', 'syntactic', '--disableAutomaticTypingAcquisition', '--useNodeIpc'];
        const li = this.o.args.indexOf('--locale');
        if (li >= 0 && this.o.args[li + 1]) args.push('--locale', this.o.args[li + 1]);
        const child = childProcess.fork(path.join(this.o.bundled, 'tsserver.js'), args, { stdio: ['ignore', 'ignore', 'inherit', 'ipc'], execArgv: [] });
        const s = new TsChild(
            child,
            'ipc',
            (m) => this.onS(m),
            () => {
                if (this.S === s) this.S = undefined;
                for (const d of this.docs.values()) d.inS = false;
            },
        );
        this.S = s;
        this.o.log(`[ts-r] started the syntax server (pid ${child.pid})`);
        this.replayConfig(s);
        return s;
    }

    /** @param {TsChild} t */
    replayConfig(t) {
        if (this.configureArgs) this.internal(t, 'configure', this.configureArgs);
        if (this.inferredArgs) this.internal(t, 'compilerOptionsForInferredProjects', this.inferredArgs);
        for (const args of this.pluginConfigs.values()) this.internal(t, 'configurePlugin', args);
    }

    /** @param {TsChild} t @param {string} command @param {any} args */
    internal(t, command, args) {
        t.send({ seq: this.nextInternal++, type: 'request', command, arguments: args });
    }

    stopT(/** @type {string} */ why) {
        const t = this.T;
        if (!t) return;
        this.o.log(`[ts-r] stopping TypeScript (${why})`);
        this.T = undefined;
        this.forgetT();
        t.stop();
    }

    forgetT() {
        for (const id of this.watchers) this.o.send({ seq: 0, type: 'event', event: 'closeFileWatcher', body: { id } });
        this.watchers.clear();
        for (const w of this.tWaiting.values()) w(null);
        this.tWaiting.clear();
        this.tProjects.clear();
        for (const d of this.docs.values()) {
            d.inT = false;
            d.tProject = undefined;
        }
    }

    /** @param {TsChild} t @param {number | null} code */
    onTExit(t, code) {
        if (this.T !== t || this.stopping) return;
        this.T = undefined;
        this.forgetT();
        const needed = [...this.docs.values()].some((d) => d.engine === 'ts');
        this.o.log(`[ts-r] TypeScript exited (${code})`);
        // VS Code restarts its TypeScript server (and reopens every file) when this exits.
        if (needed) this.o.exit(1);
    }

    reapIdle() {
        const now = Date.now();
        if (this.T && now - this.lastT > this.idleMs && ![...this.docs.values()].some((d) => d.engine === 'ts')) {
            this.stopT('idle, no file needs it');
        }
        if (this.S && now - this.lastS > this.idleMs) {
            this.o.log('[ts-r] stopping the syntax server (idle)');
            const s = this.S;
            this.S = undefined;
            for (const d of this.docs.values()) d.inS = false;
            s.stop();
        }
    }

    // -----------------------------------------------------------------------------------------
    // Messages from VS Code
    // -----------------------------------------------------------------------------------------

    /** @param {() => unknown} f */
    enqueue(f) {
        this.queue = this.queue
            .then(f)
            .then(() => {})
            .catch((e) => this.o.log(`[ts-r] bridge error: ${e instanceof Error ? e.stack : e}`));
    }

    /** @param {any} msg */
    onClientMessage(msg) {
        if (!msg || msg.type !== 'request') return;
        this.enqueue(() => this.handle(msg));
    }

    /** @param {any} msg @param {any} [body] */
    respond(msg, body) {
        /** @type {any} */
        const r = { seq: 0, type: 'response', command: msg.command, request_seq: msg.seq, success: true };
        if (body !== undefined) r.body = body;
        this.o.send(r);
    }

    /** @param {any} msg @param {string} message */
    fail(msg, message) {
        this.o.send({ seq: 0, type: 'response', command: msg.command, request_seq: msg.seq, success: false, message });
    }

    /** @param {any} msg */
    async handle(msg) {
        const args = msg.arguments || {};
        switch (msg.command) {
            case 'configure':
                this.configureArgs = args;
                if (args.formatOptions) this.formatOptions.set(args.file || '', args.formatOptions);
                this.toRunning(msg);
                return this.respond(msg);
            case 'compilerOptionsForInferredProjects':
                this.inferredArgs = args;
                this.toRunning(msg);
                return this.respond(msg, true);
            case 'configurePlugin':
                this.pluginConfigs.set(String(args.pluginName), args);
                this.toRunning(msg);
                return this.respond(msg);
            case 'updateOpen':
                await this.updateOpen(args);
                return this.respond(msg, true);
            case 'geterr':
                return this.geterr(msg);
            case 'reloadProjects':
                // "TypeScript: Reload Project": ts-r reads its configs and closed inputs again
                // (otherwise its own watcher keeps them current), and so does TypeScript.
                await this.rReady;
                if (!this.rBroken) this.R.notify('ts-r/reloadProjects', {});
                if (this.T) return this.toT(msg);
                return this.respond(msg);
            case 'geterrForProject':
            case 'watchChange':
                // `watchChange` answers TypeScript's own `createFileWatcher`s; ts-r watches the
                // inputs of its projects itself (crates/ts_r_lsp/src/tsr/watch.rs).
                if (this.T) return this.toT(msg);
                if (msg.command === 'geterrForProject') this.completed(msg.seq);
                return this.respond(msg);
            case 'exit':
                return this.shutdown();
            case '_ts-r.engines':
                return this.respond(msg, this.engines());
            case '_ts-r.configure':
                if (args.checker === 'auto' || args.checker === 'on') {
                    this.mode = args.checker;
                    await this.rerouteAll();
                    return this.respond(msg, { ok: true, checker: this.mode });
                }
                return this.respond(msg, { ok: false, restart: true });
            case 'getSupportedCodeFixes':
                if (this.T) return this.toT(msg);
                return this.supportedCodeFixes(msg);
            case 'getOutliningSpans':
                if (this.isTsr(args.file)) return this.respond(msg); // indentation folding
                return this.toT(msg);
            case 'preparePasteEdits':
                // Asked on every copy: do not start TypeScript for it.
                if (this.isTsr(args.file) && !this.T) return this.decline(msg, false);
                break;
            case 'getApplicableRefactors':
                // Asked on every cursor move (`implicit`) and for the refactor menu (`invoked`).
                // ts-r has every refactor tsserver 6 offers in TypeScript files, so both come
                // from ts-r; a JavaScript file's menu stays TypeScript's (Extract to typedef).
                if (this.isTsr(args.file)) {
                    if (args.triggerReason !== 'invoked' || !isJsFile(args.file)) return this.viaTsrActions(msg, /** @type {Doc} */ (this.docs.get(args.file)));
                    this.refactorEngine.set(args.file, 'ts');
                }
                break;
            case 'getEditsForRefactor':
            case 'getMoveToRefactoringFileSuggestions':
                if (this.isTsr(args.file) && this.refactorEngine.get(args.file) === 'tsr') return this.viaTsrActions(msg, /** @type {Doc} */ (this.docs.get(args.file)));
                break;
            case 'getCodeFixes':
                if (this.isTsr(args.file)) return this.viaTsrActions(msg, /** @type {Doc} */ (this.docs.get(args.file)));
                break;
            case 'organizeImports':
            case 'getCombinedCodeFix': {
                const file = args.scope?.args?.file;
                if (this.isTsr(file)) return this.viaTsrActions(msg, /** @type {Doc} */ (this.docs.get(file)));
                break;
            }
            case 'getEditsForFileRename':
                // Import updates need resolution, not the checker: ts-r answers unless the file is
                // open on TypeScript.
                if (!this.rBroken && this.docs.get(args.oldFilePath)?.engine !== 'ts') return this.fileRenameViaTsr(msg);
                break;
            default:
                break;
        }
        const doc = typeof args.file === 'string' ? this.docs.get(args.file) : undefined;
        if (doc && doc.engine === 'tsr' && !this.rBroken) {
            if (TSR_COMMANDS.has(msg.command)) return this.viaTsr(msg, doc);
            if (SYNTAX_COMMANDS.has(msg.command) && this.ensureS()) return this.toS(msg, doc);
            this.why = msg.command;
            this.openInT(doc);
            this.why = undefined;
        }
        return this.toT(msg);
    }

    /** Answers a request for a ts-r file without starting TypeScript. @param {any} msg @param {unknown} body */
    decline(msg, body) {
        this.declined[msg.command] = (this.declined[msg.command] || 0) + 1;
        return this.respond(msg, body);
    }

    /** Sends a configuration request to every running tsserver (as the bridge's own request). */
    toRunning(/** @type {any} */ msg) {
        for (const t of [this.T, this.S]) if (t) this.internal(t, msg.command, msg.arguments);
    }

    /** @param {any} msg */
    toT(msg) {
        this.lastT = Date.now();
        this.ensureT(msg.command).send(msg);
    }

    /** @param {any} msg @param {Doc} doc */
    toS(msg, doc) {
        const s = this.ensureS();
        if (!s) return this.toT(msg);
        this.lastS = Date.now();
        if (!doc.inS) {
            this.internal(s, 'updateOpen', { openFiles: [this.openArgs(doc)] });
            doc.inS = true;
        }
        s.send(msg);
    }

    /** @param {number} seq */
    completed(seq) {
        this.o.send({ seq: 0, type: 'event', event: 'requestCompleted', body: { request_seq: seq } });
    }

    /** @param {string | undefined} file */
    isTsr(file) {
        return typeof file === 'string' && this.docs.get(file)?.engine === 'tsr' && !this.rBroken;
    }

    // -----------------------------------------------------------------------------------------
    // Documents
    // -----------------------------------------------------------------------------------------

    /** @param {Doc} d */
    openArgs(d) {
        /** @type {any} */
        const a = { file: d.file, fileContent: d.text.text };
        if (d.scriptKindName) a.scriptKindName = d.scriptKindName;
        if (d.projectRootPath) a.projectRootPath = d.projectRootPath;
        return a;
    }

    /** @param {any} args */
    async updateOpen(args) {
        /** @type {{openFiles: any[], changedFiles: any[], closedFiles: string[]}} */
        const toT = { openFiles: [], changedFiles: [], closedFiles: [] };
        /** @type {{changedFiles: any[], closedFiles: string[]}} */
        const toS = { changedFiles: [], closedFiles: [] };
        /** @type {Doc[]} */
        const fresh = [];
        for (const o of args.openFiles || []) {
            const old = this.docs.get(o.file);
            if (old) this.closeDoc(old, toT, toS);
            const text = typeof o.fileContent === 'string' ? o.fileContent : readText(o.file);
            /** @type {Doc} */
            const d = {
                file: o.file,
                uri: C.pathToUri(o.file),
                text: new C.TextDoc(text),
                version: 1,
                scriptKindName: o.scriptKindName,
                projectRootPath: o.projectRootPath,
                engine: 'ts',
                inR: false,
                inT: false,
                inS: false,
                diags: null,
                rMirror: false,
            };
            this.docs.set(o.file, d);
            fresh.push(d);
        }
        /** Delay of the next cross-engine mirror check (`Infinity`: none needed). */
        let mirrorsIn = fresh.length || (args.closedFiles || []).length ? 50 : Infinity;
        const engines = await Promise.all(fresh.map((d) => this.decide(d)));
        fresh.forEach((d, i) => {
            if (this.docs.get(d.file) !== d) return;
            d.engine = engines[i];
            if (d.engine === 'tsr') {
                this.openInR(d);
                // TypeScript already has files of this project open: keep it seeing this one's edits.
                const inT = [...this.docs.values()].some((x) => x.inT && x.engine === 'tsr' && x.key === d.key);
                if (this.T && inT && this.mirrorsInT(d, d.key)) {
                    toT.openFiles.push(this.openArgs(d));
                    d.inT = true;
                }
            } else {
                toT.openFiles.push(this.openArgs(d));
                d.inT = true;
            }
        });
        for (const c of args.changedFiles || []) {
            const d = this.docs.get(c.fileName);
            if (!d) {
                if (this.T) toT.changedFiles.push(c);
                continue;
            }
            const changes = d.text.applyTsChanges(c.textChanges || []);
            d.version++;
            if (d.inR) this.R.notify('textDocument/didChange', { textDocument: { uri: d.uri, version: d.version }, contentChanges: changes });
            if (d.inT) toT.changedFiles.push(c);
            if (d.inS) toS.changedFiles.push(c);
            // An import edited in a TypeScript file can change which ts-r files its project uses.
            if (d.engine === 'ts' && touchesImports(d.text, c.textChanges || [])) {
                d.tProject = undefined;
                mirrorsIn = Math.min(mirrorsIn, 1000);
            }
        }
        for (const file of args.closedFiles || []) {
            const d = this.docs.get(file);
            if (!d) {
                if (this.T) toT.closedFiles.push(file);
                continue;
            }
            this.closeDoc(d, toT, toS);
            this.docs.delete(file);
        }
        if (toT.openFiles.length || ((toT.changedFiles.length || toT.closedFiles.length) && this.T)) {
            this.internal(this.ensureT(), 'updateOpen', toT);
        }
        if (this.S && (toS.changedFiles.length || toS.closedFiles.length)) this.internal(this.S, 'updateOpen', toS);
        if (mirrorsIn !== Infinity) this.scheduleMirrors(mirrorsIn);
    }

    /** @param {Doc} d @param {{closedFiles: string[]}} toT @param {{closedFiles: string[]}} toS */
    closeDoc(d, toT, toS) {
        if (d.inR) this.R.notify('textDocument/didClose', { textDocument: { uri: d.uri } });
        if (d.inT) toT.closedFiles.push(d.file);
        if (d.inS) toS.closedFiles.push(d.file);
        d.inR = d.inT = d.inS = false;
        d.rMirror = false;
    }

    /** @param {Doc} d */
    openInR(d) {
        if (d.inR) return;
        this.R.notify('textDocument/didOpen', {
            textDocument: { uri: d.uri, languageId: C.languageId(d.scriptKindName, d.file), version: d.version, text: d.text.text },
        });
        d.inR = true;
        this.configDiag(d);
    }

    /**
     * tsserver reports a project's tsconfig problems (`extends` not found, unknown options, no
     * inputs) with `configFileDiag` when a file loads the project: the same for ts-r projects.
     * @param {Doc} d
     */
    configDiag(d) {
        const p = d.key ? this.projects.get(d.key) : undefined;
        if (!p?.config || !Array.isArray(p.configDiagnostics)) return;
        const diagnostics = p.configDiagnostics.map((/** @type {any} */ x) => C.configDiagnostic(x, String(p.config)));
        this.o.send({ seq: 0, type: 'event', event: 'configFileDiag', body: { triggerFile: d.file, configFile: p.config, diagnostics } });
    }

    /**
     * Opens a ts-r file in TypeScript (started if needed) for a request ts-r cannot answer, with
     * the other open ts-r files of its project: TypeScript reads files that are not open from
     * disk, so it would miss their unsaved edits.
     */
    openInT(/** @type {Doc} */ d) {
        const t = this.ensureT();
        if (d.inT) return;
        const docs = [d, ...[...this.docs.values()].filter((x) => x !== d && this.mirrorsInT(x, d.key))];
        this.internal(t, 'updateOpen', { openFiles: docs.map((x) => this.openArgs(x)) });
        for (const x of docs) x.inT = true;
    }

    /** Whether ts-r file `d` should be open in TypeScript too, beside its project `key`'s files there. */
    mirrorsInT(/** @type {Doc} */ d, /** @type {string | undefined} */ key) {
        return !d.inT && d.engine === 'tsr' && key !== undefined && d.key === key;
    }

    // -----------------------------------------------------------------------------------------
    // Cross-engine mirroring (`auto`): a project on one engine that imports an open file of the
    // other engine's project would read that file's saved text. Such a file is also opened in
    // the other engine (its later edits and its close follow through `inT` / `inR`), but only
    // when that engine's projects use it: for TypeScript, the file lists of the projects of the
    // files it serves (`projectInfo`); for ts-r, `ts-r/usedBy` (the programs of its loaded
    // projects). In ts-r it is a mirror (`MIRROR_LANGUAGE_ID`): read, not owned, so it gets no
    // ts-r diagnostics and starts nothing there.
    // -----------------------------------------------------------------------------------------

    /** Checks the mirrors in `ms` (sooner if a check is already due sooner). */
    scheduleMirrors(ms = 50) {
        const due = Date.now() + ms;
        if (this.mirrorTimer && this.mirrorDue <= due) return;
        if (this.mirrorTimer) clearTimeout(this.mirrorTimer);
        this.mirrorDue = due;
        this.mirrorTimer = setTimeout(() => {
            this.mirrorTimer = undefined;
            this.refreshMirrors().catch((e) => this.o.log(`[ts-r] mirror check failed: ${e instanceof Error ? e.stack : e}`));
        }, ms);
        this.mirrorTimer.unref?.();
    }

    /** Learns what each engine's projects use (outside the request queue), then mirrors. */
    async refreshMirrors() {
        if (this.rBroken || this.stopping) return;
        // TypeScript: the project of each file it serves, once (again after an import edit or
        // a background project update).
        for (const d of [...this.docs.values()]) {
            if (!this.T || d.engine !== 'ts' || !d.inT || d.tProject !== undefined) continue;
            const r = await this.tRequest('projectInfo', { file: d.file, needFileNameList: true });
            const body = r?.success ? r.body : undefined;
            if (this.docs.get(d.file) !== d) continue;
            if (typeof body?.configFileName !== 'string' || !Array.isArray(body.fileNames)) {
                // No usable answer: asked again only after the next import edit or project update.
                d.tProject = '';
                continue;
            }
            d.tProject = body.configFileName;
            this.tProjects.set(body.configFileName, new Set(body.fileNames.map((/** @type {string} */ f) => samePath(f))));
        }
        // ts-r: which of its loaded projects use the files TypeScript serves.
        const rKeys = new Set([...this.docs.values()].filter((d) => d.engine === 'tsr' && d.key).map((d) => d.key));
        const candidates = [...this.docs.values()].filter((d) => d.engine === 'ts' && !d.inR && path.isAbsolute(d.file));
        /** @type {Record<string, string[]>} */
        let used = {};
        if (rKeys.size && candidates.length) {
            used = (await this.rRequest('ts-r/usedBy', { uris: candidates.map((d) => d.uri) }).catch(() => null)) || {};
        }
        this.enqueue(() => this.applyMirrors(used));
    }

    /** Opens the files each engine's projects use from the other engine's files. @param {Record<string, string[]>} used */
    applyMirrors(used) {
        if (this.rBroken) return;
        const docs = [...this.docs.values()];
        if (this.T) {
            /** @type {Set<string>} */
            const tProjects = new Set();
            for (const d of docs) if (d.engine === 'ts' && d.tProject) tProjects.add(d.tProject);
            const add = docs.filter((d) => d.engine === 'tsr' && !d.inT && [...tProjects].some((p) => this.tProjects.get(p)?.has(samePath(d.file))));
            if (add.length) {
                this.internal(this.T, 'updateOpen', { openFiles: add.map((d) => this.openArgs(d)) });
                for (const d of add) d.inT = true;
            }
        }
        const rKeys = new Set(docs.filter((d) => d.engine === 'tsr' && d.key).map((d) => d.key));
        for (const d of docs) {
            if (d.engine !== 'ts' || d.inR) continue;
            const users = used[d.uri];
            if (Array.isArray(users) && users.some((k) => k !== d.key && rKeys.has(k))) this.mirrorInR(d);
        }
    }

    /** Opens a TypeScript file in ts-r as a mirror. @param {Doc} d */
    mirrorInR(d) {
        this.R.notify('textDocument/didOpen', { textDocument: { uri: d.uri, languageId: MIRROR_LANGUAGE_ID, version: d.version, text: d.text.text } });
        d.inR = true;
        d.rMirror = true;
    }

    /**
     * Asks the running TypeScript a question of the bridge's own; the answer (null: none, or
     * TypeScript stopped) never reaches VS Code.
     * @param {string} command @param {any} args @returns {Promise<any>}
     */
    tRequest(command, args) {
        const t = this.T;
        if (!t) return Promise.resolve(null);
        const seq = this.nextInternal++;
        const answer = new Promise((resolve) => this.tWaiting.set(seq, resolve));
        t.send({ seq, type: 'request', command, arguments: args });
        return withTimeout(answer, 10_000)
            .catch(() => null)
            .finally(() => this.tWaiting.delete(seq));
    }

    /**
     * The engine for a newly opened file.
     * @param {Doc} d
     * @returns {Promise<'tsr' | 'ts'>}
     */
    async decide(d) {
        if (!path.isAbsolute(d.file) || d.file.startsWith('^')) return 'ts';
        await this.rReady;
        if (this.rBroken) return 'ts';
        const mark = this.statusCount;
        let info = await withTimeout(this.R.request('ts-r/projectInfo', { textDocument: { uri: d.uri } }), 5000).catch((e) => {
            this.o.log(`[ts-r] projectInfo failed: ${e instanceof Error ? e.message : e}`);
            return null;
        });
        if (!info) return 'ts';
        const key = String(info.config || INFERRED);
        d.key = key;
        const known = this.projects.get(key);
        // A `ts-r/status` handled after this request was sent is at least as new as the answer
        // (a cached parity result often arrives right behind it).
        if (known && (this.statusSeen.get(key) ?? -1) >= mark) info = { ...info, state: known.state, parity: known.parity };
        this.projects.set(key, { ...known, ...info });
        if (this.mode === 'auto' && info.state === 'checking') info = await this.settled(key);
        return this.engineFor(key);
    }

    /** Waits (briefly) for a project's parity result: a cached one arrives at once. */
    settled(/** @type {string} */ key) {
        return new Promise((resolve) => {
            const done = () => {
                clearTimeout(timer);
                this.statusWaiters = this.statusWaiters.filter((w) => w !== check);
                resolve(this.projects.get(key) || {});
            };
            const check = () => {
                if (this.projects.get(key)?.state !== 'checking') done();
            };
            const timer = setTimeout(done, this.settleMs);
            this.statusWaiters.push(check);
            check();
        });
    }

    /** @param {string | undefined} key @returns {'tsr' | 'ts'} */
    engineFor(key) {
        if (this.rBroken || !key) return 'ts';
        if (this.mode === 'on') return 'tsr';
        return this.projects.get(key)?.state === 'matched' ? 'tsr' : 'ts';
    }

    /** Moves open files whose project's engine changed (parity result, `ts-r.checker`). */
    async rerouteAll() {
        let moved = false;
        for (const d of [...this.docs.values()]) {
            const want = this.engineFor(d.key);
            if (want !== d.engine) {
                this.switchDoc(d, want);
                moved = true;
            }
        }
        if (moved) this.scheduleMirrors();
    }

    /** @param {Doc} d @param {'tsr' | 'ts'} to */
    switchDoc(d, to) {
        this.o.log(`[ts-r] ${d.file}: ${d.engine === 'tsr' ? 'ts-r' : 'TypeScript'} -> ${to === 'tsr' ? 'ts-r' : 'TypeScript'}`);
        d.engine = to;
        d.tProject = undefined;
        if (to === 'tsr') {
            d.diags = null;
            if (d.rMirror) {
                // Open in ts-r as a mirror: re-open it as a file ts-r serves.
                this.R.notify('textDocument/didClose', { textDocument: { uri: d.uri } });
                d.inR = d.rMirror = false;
            }
            this.openInR(d);
            if (d.inT && this.T) {
                this.internal(this.T, 'updateOpen', { closedFiles: [d.file] });
                d.inT = false;
            }
            return;
        }
        this.openInT(d);
        if (d.inR && !this.rBroken) this.R.notify('textDocument/didClose', { textDocument: { uri: d.uri } });
        d.inR = d.rMirror = false;
        d.diags = null;
        if (this.T) this.internal(this.T, 'geterr', { files: [d.file], delay: 0 });
    }

    // -----------------------------------------------------------------------------------------
    // Diagnostics
    // -----------------------------------------------------------------------------------------

    /** @param {any} msg */
    geterr(msg) {
        const files = /** @type {string[]} */ (msg.arguments?.files || []);
        const ts = files.filter((f) => !this.isTsr(f));
        for (const f of files) {
            const d = this.docs.get(f);
            if (d && this.isTsr(f) && d.diags) this.emitDiagnostics(d);
        }
        if (ts.length && this.T) return this.toT({ ...msg, arguments: { ...msg.arguments, files: ts } });
        this.completed(msg.seq);
    }

    /** @param {any} p */
    onRDiagnostics(p) {
        const file = C.uriToPath(p.uri);
        const d = this.docs.get(file) || [...this.docs.values()].find((x) => x.uri === p.uri);
        if (!d || d.engine !== 'tsr' || !d.inR) return;
        d.diags = p.diagnostics || [];
        this.emitDiagnostics(d);
    }

    /** @param {Doc} d */
    emitDiagnostics(d) {
        const split = C.splitDiagnostics(d.diags || []);
        for (const [event, list] of /** @type {const} */ ([
            ['syntaxDiag', split.syntax],
            ['semanticDiag', split.semantic],
            ['suggestionDiag', split.suggestion],
        ])) {
            this.o.send({ seq: 0, type: 'event', event, body: { file: d.file, diagnostics: list } });
        }
    }

    /** @param {any} st */
    onStatus(st) {
        this.lastStatus = st;
        const n = this.statusCount++;
        /** @type {Set<string>} */
        const changed = new Set();
        for (const p of st?.projects || []) {
            const key = p.config || INFERRED;
            const prev = this.projects.get(key);
            if (prev?.state !== p.state) changed.add(key);
            this.projects.set(key, { ...prev, ...p });
            this.statusSeen.set(key, n);
        }
        for (const w of [...this.statusWaiters]) w();
        if (changed.size) this.enqueue(() => this.rerouteAll());
    }

    // -----------------------------------------------------------------------------------------
    // tsserver children's messages
    // -----------------------------------------------------------------------------------------

    /** @param {any} m */
    onT(m) {
        if (m.type === 'response') {
            const waiting = this.tWaiting.get(m.request_seq);
            if (waiting) {
                this.tWaiting.delete(m.request_seq);
                return waiting(m);
            }
            if (m.request_seq >= INTERNAL - 1) return;
            return this.o.send(m);
        }
        if (m.type !== 'event') return;
        if (m.event === 'projectsUpdatedInBackground' || m.event === 'projectLoadingFinish') {
            // TypeScript's file lists may have changed: ask again.
            for (const d of this.docs.values()) if (d.engine === 'ts') d.tProject = undefined;
            this.scheduleMirrors(200);
        }
        if (DIAG_EVENTS.has(m.event) && this.isTsr(m.body?.file)) return;
        if (m.event === 'requestCompleted' && m.body?.request_seq >= INTERNAL) return;
        if (m.event === 'createFileWatcher' || m.event === 'createDirectoryWatcher') this.watchers.add(m.body?.id);
        if (m.event === 'closeFileWatcher') this.watchers.delete(m.body?.id);
        this.o.send(m);
    }

    /** @param {any} m */
    onS(m) {
        if (m.type === 'response' && m.request_seq < INTERNAL - 1) this.o.send(m);
    }

    // -----------------------------------------------------------------------------------------
    // ts-r requests
    // -----------------------------------------------------------------------------------------

    /**
     * @param {string} method
     * @param {any} params
     */
    rRequest(method, params) {
        return withTimeout(this.R.request(method, params), 30_000);
    }

    /** @param {any} msg @param {Doc} d */
    viaTsr(msg, d) {
        const a = msg.arguments || {};
        const at = { line: a.line, offset: a.offset };
        const textDocument = { uri: d.uri };
        const position = C.toLsp(at);
        /** @param {Promise<any>} p @param {(r: any) => any} f */
        const answer = (p, f) => {
            p.then((r) => {
                const body = f(r);
                if (body === undefined) this.fail(msg, 'No content available.');
                else this.respond(msg, body);
            }).catch((e) => this.fail(msg, `ts-r: ${e instanceof Error ? e.message : e}`));
        };
        switch (msg.command) {
            case 'quickinfo':
                return answer(this.rRequest('textDocument/hover', { textDocument, position }), (r) => C.quickInfo(r, at));
            case 'completionInfo': {
                const word = d.text.wordAt(at);
                const before = d.text.text[d.text.indexOfTs(word.start) - 1];
                /** @type {any} */
                const context = a.triggerCharacter ? { triggerKind: 2, triggerCharacter: a.triggerCharacter } : { triggerKind: 1 };
                return answer(this.rRequest('textDocument/completion', { textDocument, position, context }), (r) =>
                    C.completionInfo(r, { isMemberCompletion: before === '.' }),
                );
            }
            case 'completionEntryDetails': {
                const names = /** @type {any[]} */ (a.entryNames || []);
                const p = Promise.all(
                    names.map((n) => {
                        const item = typeof n === 'object' ? n.data?.tsr : undefined;
                        const name = typeof n === 'object' ? n.name : n;
                        if (!item) return Promise.resolve({ name, kind: '', kindModifiers: '', displayParts: [], documentation: [], tags: [] });
                        return this.rRequest('completionItem/resolve', item)
                            .catch(() => item)
                            .then((r) => C.entryDetails(r || item, d.file));
                    }),
                );
                return answer(p, (r) => r);
            }
            case 'definition':
                return answer(this.rRequest('textDocument/definition', { textDocument, position }), (r) => C.fileSpans(r));
            case 'definitionAndBoundSpan': {
                const word = d.text.wordAt(at);
                return answer(this.rRequest('textDocument/definition', { textDocument, position }), (r) => ({
                    definitions: C.fileSpans(r),
                    textSpan: { start: word.start, end: word.end },
                }));
            }
            case 'typeDefinition':
                return answer(this.rRequest('textDocument/typeDefinition', { textDocument, position }), (r) => C.fileSpans(r));
            case 'references': {
                const word = d.text.wordAt(at);
                const p = Promise.all([
                    this.rRequest('textDocument/references', { textDocument, position, context: { includeDeclaration: true } }),
                    this.rRequest('textDocument/definition', { textDocument, position }).catch(() => null),
                ]);
                return answer(p, ([refs, defs]) => C.referencesBody(refs, defs, word));
            }
            case 'documentHighlights':
                return answer(this.rRequest('textDocument/documentHighlight', { textDocument, position }), (r) => C.highlightsBody(r, d.file));
            case 'rename': {
                const word = d.text.wordAt(at);
                const p = Promise.all([
                    this.rRequest('textDocument/prepareRename', { textDocument, position }).catch(() => null),
                    this.rRequest('textDocument/rename', { textDocument, position, newName: C.RENAME_PLACEHOLDER }).catch(() => null),
                ]);
                return answer(p, ([prep, edit]) => C.renameBody(prep, edit, word));
            }
            case 'signatureHelp':
                return answer(this.rRequest('textDocument/signatureHelp', { textDocument, position }), (r) => C.signatureHelpBody(r, at));
            case 'encodedSemanticClassifications-full':
                if (a.format !== '2020') {
                    this.openInT(d);
                    return this.toT(msg);
                }
                return answer(this.rRequest('textDocument/semanticTokens/full', { textDocument }), (r) =>
                    C.encodedClassifications(r, this.legend, d.text, a.start ?? 0, a.length ?? d.text.text.length),
                );
            case 'navtree':
                return answer(this.rRequest('textDocument/documentSymbol', { textDocument }), (r) => C.navTree(r, d.text));
            case 'projectInfo': {
                const key = d.key || INFERRED;
                return this.respond(msg, {
                    configFileName: key === INFERRED ? '/dev/null/inferredProject1*' : key,
                    languageServiceDisabled: false,
                    fileNames: a.needFileNameList ? [] : undefined,
                });
            }
            default:
                this.openInT(d);
                return this.toT(msg);
        }
    }

    /**
     * Code fixes, refactors and organize imports for a ts-r file from ts-r's code actions. What
     * ts-r has no answer for (no fix for these codes, a fix-all it did not list) goes to
     * TypeScript, with the file opened there first.
     * @param {any} msg @param {Doc} d
     */
    viaTsrActions(msg, d) {
        const a = msg.arguments || {};
        const textDocument = { uri: d.uri };
        const toT = () => {
            this.openInT(d);
            this.toT(msg);
        };
        const range =
            a.startLine !== undefined
                ? { start: C.toLsp({ line: a.startLine, offset: a.startOffset }), end: C.toLsp({ line: a.endLine ?? a.startLine, offset: a.endOffset ?? a.startOffset }) }
                : { start: { line: 0, character: 0 }, end: { line: 0, character: 0 } };
        /** @param {any} context */
        const actions = (context) => this.rRequest('textDocument/codeAction', { textDocument, range, context });
        /** @param {Promise<any>} p @param {(r: any) => any} f @param {() => void} otherwise */
        const answer = (p, f, otherwise) => {
            p.then((r) => {
                const body = f(r);
                if (body === undefined) otherwise();
                else this.respond(msg, body);
            }).catch(() => otherwise());
        };
        switch (msg.command) {
            case 'getApplicableRefactors': {
                this.refactorEngine.set(d.file, 'tsr');
                const invoked = a.triggerReason === 'invoked';
                const tsr = { interactive: !!a.includeInteractiveActions, format: this.formatOptions.get(d.file) ?? this.formatOptions.get('') };
                return answer(
                    actions({ diagnostics: [], only: ['refactor'], triggerKind: invoked ? 1 : 2, tsr }),
                    (r) => C.refactorsBody(r),
                    // The menu falls back to TypeScript (its edits too); a cursor move gets nothing.
                    invoked
                        ? () => {
                              this.refactorEngine.set(d.file, 'ts');
                              toT();
                          }
                        : () => this.decline(msg, []),
                );
            }
            case 'getEditsForRefactor': {
                // Move to file: the target the user picked.
                const tsr = { targetFile: a.interactiveRefactorArguments?.targetFile, format: this.formatOptions.get(d.file) ?? this.formatOptions.get('') };
                return answer(
                    actions({ diagnostics: [], only: ['refactor'], triggerKind: 1, tsr }),
                    (r) => {
                        const act = (r || []).find((/** @type {any} */ x) => x.data?.refactor === `${a.refactor}/${a.action}` && x.edit);
                        return act ? { edits: C.fileCodeEdits(act.edit) } : undefined;
                    },
                    toT,
                );
            }
            case 'getMoveToRefactoringFileSuggestions':
                return answer(
                    this.rRequest('ts-r/moveToFileSuggestions', { textDocument, range }),
                    (r) => (r && typeof r.newFileName === 'string' ? { newFileName: r.newFileName, files: r.files || [] } : undefined),
                    toT,
                );
            case 'getCodeFixes': {
                const diagnostics = (a.errorCodes || []).map((/** @type {number} */ code) => ({ range, code: Number(code), source: 'ts-r', message: '' }));
                const version = d.version;
                return answer(
                    actions({ diagnostics, only: ['quickfix'] }),
                    (r) => {
                        for (const x of r || []) {
                            if (x.data?.fixAll && x.data.fixId) this.fixAlls.set(`${d.file}\0${x.data.fixId}`, { version, changes: C.fileCodeEdits(x.edit) });
                        }
                        const fixes = C.codeFixesBody(r);
                        return fixes.length ? fixes : undefined;
                    },
                    toT,
                );
            }
            case 'getCombinedCodeFix': {
                const hit = this.fixAlls.get(`${d.file}\0${a.fixId}`);
                if (hit && hit.version === d.version) return this.respond(msg, { changes: hit.changes });
                return toT();
            }
            case 'organizeImports': {
                const mode = a.skipDestructiveCodeActions ? 'SortAndCombine' : a.mode;
                const kind = mode === 'SortAndCombine' ? 'source.sortImports' : mode === 'RemoveUnused' ? 'source.removeUnusedImports' : 'source.organizeImports';
                return answer(
                    actions({ diagnostics: [], only: [kind] }),
                    (r) => {
                        const act = (r || [])[0];
                        return act ? C.fileCodeEdits(act.edit) : undefined;
                    },
                    toT,
                );
            }
            default:
                return toT();
        }
    }

    /**
     * `getEditsForFileRename` from `workspace/willRenameFiles`. VS Code asks once the file has
     * moved: edits for the moved file go to its new path.
     * @param {any} msg
     */
    fileRenameViaTsr(msg) {
        const a = msg.arguments || {};
        const from = String(a.oldFilePath || '');
        const to = String(a.newFilePath || '');
        /** @param {string} f */
        const moved = (f) => (f === from ? to : f.startsWith(`${from}/`) ? to + f.slice(from.length) : f);
        const files = [{ oldUri: C.pathToUri(from), newUri: C.pathToUri(to) }];
        this.rRequest('workspace/willRenameFiles', { files })
            .then((r) => this.respond(msg, C.fileCodeEdits(r, moved)))
            .catch(() => this.toT(msg));
    }

    /** `getSupportedCodeFixes` without starting TypeScript: computed once per version, cached. */
    async supportedCodeFixes(/** @type {any} */ msg) {
        const bundled = this.o.bundled;
        if (!bundled) return this.toT(msg);
        let version = 'unknown';
        try {
            version = JSON.parse(fs.readFileSync(path.join(bundled, '..', 'package.json'), 'utf8')).version;
        } catch {
            // Keep "unknown".
        }
        const cache = path.join(this.o.tsdkDir, `code-fixes-${version}.json`);
        try {
            return this.respond(msg, JSON.parse(fs.readFileSync(cache, 'utf8')));
        } catch {
            // Compute it below.
        }
        const script = `process.stdout.write(JSON.stringify(require(${JSON.stringify(path.join(bundled, 'typescript.js'))}).getSupportedCodeFixes()))`;
        const out = await new Promise((resolve) => {
            childProcess.execFile(process.execPath, ['-e', script], { env: { ...process.env, ELECTRON_RUN_AS_NODE: '1' }, timeout: 20000, maxBuffer: 1 << 22 }, (err, stdout) =>
                resolve(err ? null : stdout),
            );
        });
        try {
            const codes = JSON.parse(String(out));
            if (!Array.isArray(codes)) throw new Error('not a list');
            try {
                fs.writeFileSync(cache, JSON.stringify(codes));
            } catch {
                // Read-only storage: compute it again next time.
            }
            return this.respond(msg, codes);
        } catch {
            return this.toT(msg);
        }
    }

    // -----------------------------------------------------------------------------------------
    // Status and shutdown
    // -----------------------------------------------------------------------------------------

    /** What `_ts-r.engines` reports to the extension (status bar, parity report). */
    engines() {
        /** @type {Map<string, {project: string, config: string | null, state: string, engine: string, openFiles: number}>} */
        const byKey = new Map();
        let other = 0;
        /** @type {Record<string, string>} */
        const files = {};
        for (const d of this.docs.values()) {
            files[d.file] = d.engine === 'tsr' ? 'ts-r' : 'typescript';
            if (!d.key) {
                other++;
                continue;
            }
            const p = this.projects.get(d.key) || {};
            const e = byKey.get(d.key) || {
                project: p.project || d.key,
                config: p.config ?? null,
                state: p.state || 'unknown',
                engine: d.engine === 'tsr' ? 'ts-r' : 'typescript',
                openFiles: 0,
            };
            e.openFiles++;
            byKey.set(d.key, e);
        }
        return {
            bridge: true,
            checker: this.mode,
            tsrAvailable: !this.rBroken,
            projects: [...byKey.values()].sort((a, b) => a.project.localeCompare(b.project)),
            otherFiles: other,
            files,
            typescript: { running: !!this.T, pid: this.T?.pid, starts: this.tStarts, startedFor: this.tStartedFor || undefined, declined: this.declined },
            syntax: this.S ? { running: true, pid: this.S.pid } : { running: false },
            status: this.lastStatus,
        };
    }

    async shutdown() {
        if (this.stopping) return;
        this.stopping = true;
        this.S?.stop();
        this.T?.stop();
        await Promise.race([this.R.stop(1500), new Promise((r) => setTimeout(r, 2000))]);
        this.o.exit(0);
    }
}

/** Whether `file` is JavaScript (its refactor menu stays TypeScript's). @param {string} file */
function isJsFile(file) {
    return /\.(js|jsx|mjs|cjs)$/i.test(file);
}

/** A path as compared with TypeScript's file lists (forward slashes; case-folded on Windows). @param {string} file */
function samePath(file) {
    const p = file.replace(/\\/g, '/');
    return process.platform === 'win32' ? p.toLowerCase() : p;
}

/**
 * Whether tsserver `textChanges` (already applied to `text`) touch a line that imports or
 * re-exports (so the file's project may now use other files).
 * @param {import('./convert').TextDoc} text @param {{start: {line: number}}[]} changes
 */
function touchesImports(text, changes) {
    return changes.some((c) => {
        const start = text.indexOf(c.start.line - 1, 0);
        const end = text.indexOf(c.start.line, 0);
        return /\b(import|require|from|export)\b|<reference/.test(text.text.slice(start, end));
    });
}

/** @param {string} file */
function readText(file) {
    try {
        return fs.readFileSync(file, 'utf8');
    } catch {
        return '';
    }
}

/**
 * @template T
 * @param {Promise<T>} p
 * @param {number} ms
 * @returns {Promise<T>}
 */
function withTimeout(p, ms) {
    return new Promise((resolve, reject) => {
        const t = setTimeout(() => reject(new Error(`no answer in ${ms} ms`)), ms);
        p.then(
            (v) => {
                clearTimeout(t);
                resolve(v);
            },
            (e) => {
                clearTimeout(t);
                reject(e);
            },
        );
    });
}

module.exports = { Router, INTERNAL };
