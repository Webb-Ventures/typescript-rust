// @ts-check
// ts-r-lint in VS Code (L2): a lint-only LSP connection to the workspace's shared ts-r-lsp
// daemon. Type answers keep coming from VS Code's TypeScript extension through the tsserver
// stand-in; this connection only carries open documents one way and lint diagnostics and
// code actions the other. See ../AGENTS.md.
'use strict';

const vscode = require('vscode');
const { LspClient } = require('../standin/lspclient');
const { resolveLintEnable, ESLINT_EXTENSION } = require('./mode');

const LANGUAGES = ['typescript', 'typescriptreact', 'javascript', 'javascriptreact'];
const SELECTOR = LANGUAGES.map((language) => ({ language, scheme: 'file' }));
const CONFIG_GLOB = '**/{eslint.config.{js,mjs,cjs,ts,mts,cts},.oxlintrc.json,oxlintrc.json,biome.json,biome.jsonc}';
const FIX_ALL = vscode.CodeActionKind.SourceFixAll.append('ts-r');

/** @param {vscode.TextDocument} doc */
function tracked(doc) {
    return doc.uri.scheme === 'file' && LANGUAGES.includes(doc.languageId);
}

/** @param {any} r LSP range */
function toRange(r) {
    return new vscode.Range(r.start.line, r.start.character, r.end.line, r.end.character);
}

/** @param {vscode.Range} r */
function fromRange(r) {
    return { start: { line: r.start.line, character: r.start.character }, end: { line: r.end.line, character: r.end.character } };
}

/** @param {any} d LSP diagnostic */
function toDiagnostic(d) {
    const severity = d.severity === 1 ? vscode.DiagnosticSeverity.Error : d.severity === 2 ? vscode.DiagnosticSeverity.Warning : vscode.DiagnosticSeverity.Information;
    const out = new vscode.Diagnostic(toRange(d.range), d.message, severity);
    out.source = d.source;
    if (d.code !== undefined) {
        out.code = d.codeDescription?.href ? { value: d.code, target: vscode.Uri.parse(d.codeDescription.href) } : d.code;
    }
    return out;
}

/** @param {vscode.Diagnostic} d */
function codeOf(d) {
    return typeof d.code === 'object' && d.code !== null ? d.code.value : d.code;
}

/** One lint connection (one window, one workspace root). */
class LintClient {
    /**
     * @param {string} bin
     * @param {string} root
     * @param {vscode.OutputChannel} output
     * @param {(status: any) => void} onStatus `ts-r/status` from the daemon (ts-r checker)
     */
    constructor(bin, root, output, onStatus) {
        this.bin = bin;
        this.root = root;
        this.output = output;
        this.onStatus = onStatus;
        this.diagnostics = vscode.languages.createDiagnosticCollection('ts-r-lint');
        /** @type {vscode.Disposable[]} */
        this.disposables = [this.diagnostics];
        /** @type {LspClient | undefined} */
        this.lsp = undefined;
        this.stopping = false;
        /** @type {number[]} */
        this.crashes = [];
        /** @type {Set<string>} */
        this.open = new Set();
    }

    async start() {
        const lsp = new LspClient(this.bin, ['--lint-only', '--root', this.root, '--name', `vscode lint ${process.pid}`], {
            cwd: this.root,
            log: (line) => this.output.appendLine(line),
        });
        this.lsp = lsp;
        lsp.onNotification('textDocument/publishDiagnostics', (p) => {
            this.diagnostics.set(vscode.Uri.parse(p.uri), (p.diagnostics || []).map(toDiagnostic));
        });
        lsp.onNotification('window/logMessage', (p) => this.output.appendLine(String(p.message)));
        lsp.onNotification('ts-r/status', (p) => this.onStatus(p));
        lsp.onNotification('window/showMessage', (p) => {
            const show = p.type === 1 ? vscode.window.showErrorMessage : p.type === 2 ? vscode.window.showWarningMessage : vscode.window.showInformationMessage;
            void show(String(p.message));
        });
        lsp.onDidExit(() => this.onExit());
        const rootUri = vscode.Uri.file(this.root).toString();
        await lsp.request('initialize', {
            processId: process.pid,
            rootUri,
            workspaceFolders: [{ uri: rootUri, name: 'root' }],
            capabilities: {
                textDocument: {
                    publishDiagnostics: { codeDescriptionSupport: true },
                    codeAction: { codeActionLiteralSupport: { codeActionKind: { valueSet: ['quickfix', 'source.fixAll'] } }, isPreferredSupport: true },
                    synchronization: { didSave: true },
                },
                workspace: { didChangeWatchedFiles: { dynamicRegistration: false } },
            },
            initializationOptions: { lintOnly: true, 'ts-r': { lint: { enable: 'on' } } },
        });
        lsp.notify('initialized', {});
        this.open.clear();
        for (const doc of vscode.workspace.textDocuments) this.didOpen(doc);
        if (this.disposables.length === 1) this.subscribe();
    }

    subscribe() {
        const watcher = vscode.workspace.createFileSystemWatcher(CONFIG_GLOB);
        /** @param {vscode.Uri} uri @param {number} type */
        const watched = (uri, type) => this.lsp?.notify('workspace/didChangeWatchedFiles', { changes: [{ uri: uri.toString(), type }] });
        this.disposables.push(
            vscode.workspace.onDidOpenTextDocument((d) => this.didOpen(d)),
            vscode.workspace.onDidChangeTextDocument((e) => this.didChange(e)),
            vscode.workspace.onDidSaveTextDocument((d) => {
                if (this.open.has(d.uri.toString())) this.lsp?.notify('textDocument/didSave', { textDocument: { uri: d.uri.toString() } });
            }),
            vscode.workspace.onDidCloseTextDocument((d) => this.didClose(d)),
            watcher,
            watcher.onDidCreate((u) => watched(u, 1)),
            watcher.onDidChange((u) => watched(u, 2)),
            watcher.onDidDelete((u) => watched(u, 3)),
            vscode.languages.registerCodeActionsProvider(SELECTOR, this, {
                providedCodeActionKinds: [vscode.CodeActionKind.QuickFix, FIX_ALL],
            }),
        );
    }

    /** @param {vscode.TextDocument} doc */
    didOpen(doc) {
        if (!tracked(doc) || !this.lsp) return;
        const uri = doc.uri.toString();
        if (this.open.has(uri)) return;
        this.open.add(uri);
        this.lsp.notify('textDocument/didOpen', {
            textDocument: { uri, languageId: doc.languageId, version: doc.version, text: doc.getText() },
        });
    }

    /** @param {vscode.TextDocumentChangeEvent} e */
    didChange(e) {
        const uri = e.document.uri.toString();
        if (!this.open.has(uri) || e.contentChanges.length === 0) return;
        this.lsp?.notify('textDocument/didChange', {
            textDocument: { uri, version: e.document.version },
            contentChanges: e.contentChanges.map((c) => ({ range: fromRange(c.range), text: c.text })),
        });
    }

    /** @param {vscode.TextDocument} doc */
    didClose(doc) {
        const uri = doc.uri.toString();
        if (!this.open.delete(uri)) return;
        this.lsp?.notify('textDocument/didClose', { textDocument: { uri } });
        this.diagnostics.delete(doc.uri);
    }

    /**
     * @param {vscode.TextDocument} document
     * @param {vscode.Range | vscode.Selection} range
     * @param {vscode.CodeActionContext} context
     * @returns {Promise<vscode.CodeAction[]>}
     */
    async provideCodeActions(document, range, context) {
        if (!this.lsp || !this.open.has(document.uri.toString())) return [];
        /** @type {any[]} */
        let actions;
        try {
            actions = await this.lsp.request('textDocument/codeAction', {
                textDocument: { uri: document.uri.toString() },
                range: fromRange(range),
                context: { diagnostics: [], only: context.only ? [context.only.value] : undefined },
            });
        } catch {
            return [];
        }
        const mine = context.diagnostics.filter((d) => d.source === 'ts-r-lint');
        return (actions || []).map((a) => {
            const kind = a.kind === 'quickfix' ? vscode.CodeActionKind.QuickFix : a.kind ? vscode.CodeActionKind.Empty.append(a.kind) : undefined;
            const action = new vscode.CodeAction(a.title, kind);
            action.isPreferred = !!a.isPreferred;
            const edit = new vscode.WorkspaceEdit();
            for (const [uri, edits] of Object.entries(a.edit?.changes || {})) {
                for (const e of /** @type {any[]} */ (edits)) edit.replace(vscode.Uri.parse(uri), toRange(e.range), e.newText);
            }
            action.edit = edit;
            action.diagnostics = mine.filter((d) =>
                (a.diagnostics || []).some((/** @type {any} */ x) => x.code === codeOf(d) && toRange(x.range).isEqual(d.range)),
            );
            return action;
        });
    }

    onExit() {
        this.lsp = undefined;
        if (this.stopping) return;
        const now = Date.now();
        this.crashes = this.crashes.filter((t) => now - t < 60_000);
        this.crashes.push(now);
        if (this.crashes.length > 3) {
            this.output.appendLine('ts-r-lint: the lint connection keeps exiting; run "ts-r: Restart Lint" to try again.');
            return;
        }
        this.output.appendLine('ts-r-lint: lint connection lost; reconnecting.');
        this.start().catch((e) => this.output.appendLine(`ts-r-lint: ${e.message}`));
    }

    async stop() {
        this.stopping = true;
        const lsp = this.lsp;
        this.lsp = undefined;
        for (const d of this.disposables) d.dispose();
        this.disposables = [];
        if (lsp) await lsp.stop();
    }
}

/** Starts and stops the lint connection as settings and installed extensions change. */
class LintController {
    /**
     * @param {vscode.OutputChannel} output
     * @param {() => string | undefined} findBin
     * @param {(status: any) => void} [onStatus] `ts-r/status` notifications
     */
    constructor(output, findBin, onStatus) {
        this.output = output;
        this.findBin = findBin;
        this.onStatus = onStatus || (() => {});
        /** @type {LintClient | undefined} */
        this.client = undefined;
        this.lastReason = '';
    }

    eslintState() {
        const ext = vscode.extensions.getExtension(ESLINT_EXTENSION);
        const eslintEnable = /** @type {unknown} */ (vscode.workspace.getConfiguration('eslint').get('enable')) !== false;
        return { installed: !!ext, eslintEnable };
    }

    /** Applies `ts-r.lint.enable` now. */
    async update(restart = false) {
        const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
        const setting = vscode.workspace.getConfiguration('ts-r').get('lint.enable', 'auto');
        const { lint, reason } = resolveLintEnable(setting, this.eslintState());
        if (reason !== this.lastReason) {
            this.output.appendLine(`ts-r-lint ${lint ? 'on' : 'off'}: ${reason}`);
            this.lastReason = reason;
        }
        const bin = this.findBin();
        const want = lint && !!root && !!bin;
        if (this.client && (!want || restart)) {
            const c = this.client;
            this.client = undefined;
            await c.stop();
        }
        if (want && !this.client && root && bin) {
            const c = new LintClient(bin, root, this.output, this.onStatus);
            this.client = c;
            try {
                await c.start();
            } catch (e) {
                this.output.appendLine(`ts-r-lint: cannot start: ${e instanceof Error ? e.message : e}`);
            }
        }
    }

    async dispose() {
        const c = this.client;
        this.client = undefined;
        if (c) await c.stop();
    }
}

module.exports = { LintController, LintClient };
