// @ts-check
// ts-r for VS Code and Cursor. VS Code's own TypeScript extension keeps doing all the type UI
// work, but its tsserver is replaced by a stand-in (standin/tsserver.js). With `ts-r.checker`
// auto/on (L3) the stand-in answers from the ts-r checker for projects that passed the parity
// check (standin/router.js) and from the workspace daemon's shared tsserver for the rest; with
// `off` everything goes to that shared tsserver (L1). Lint (L2) is a small lint-only LSP
// connection to the same daemon (lint/client.js), off while the ESLint extension is enabled
// unless the user opts in. See AGENTS.md.
'use strict';

const vscode = require('vscode');
const fs = require('fs');
const path = require('path');
const os = require('os');
const childProcess = require('child_process');
const { LintController } = require('./lint/client');
const { describeStatus } = require('./lint/status');
const { describeEngines, parityIssue, reason } = require('./checker');

const STATE_KEY = 'ts-r.savedTypeScriptSettings';

/**
 * Settings we change in the user's (global) VS Code settings. VS Code >= 1.10x reads the
 * unified `js/ts.*` keys first; older builds (and Cursor) only know the `typescript.*` ones.
 * @type {{unified: string, legacy: string, value: (lib: string) => unknown, legacyValue?: (lib: string) => unknown}[]}
 */
const MANAGED = [
    { unified: 'js/ts.tsdk.path', legacy: 'typescript.tsdk', value: (lib) => lib },
    { unified: 'js/ts.tsserver.useSyntaxServer', legacy: 'typescript.tsserver.useSyntaxServer', value: () => 'never' },
    {
        unified: 'js/ts.tsserver.automaticTypeAcquisition.enabled',
        legacy: 'typescript.disableAutomaticTypeAcquisition',
        value: () => false,
        legacyValue: () => true,
    },
];

/** @type {vscode.OutputChannel} */
let output;
/** @type {LintController | undefined} */
let lint;
/** @type {vscode.StatusBarItem | undefined} */
let statusItem;
/** Last `ts-r/status` (ts-r checker state, K-17 parity). @type {any} */
let lastTsrStatus;
/** Last `_ts-r.engines` from the tsserver bridge (undefined: no bridge running). @type {any} */
let lastEngines;
/** `ts-r.checker` written to the stand-in's ts-r.json. @type {string | undefined} */
let appliedChecker;

/** Settings whose change rewrites the stand-in and restarts the TS server. */
const TYPESCRIPT_SETTINGS = ['ts-r.memoryBudgetMB', 'ts-r.idleShutdownMinutes', 'ts-r.serverPath', 'ts-r.manageTypeScript'];
const TS_LANGUAGES = ['typescript', 'typescriptreact', 'javascript', 'javascriptreact'];

/** @param {vscode.ExtensionContext} context */
async function activate(context) {
    output = vscode.window.createOutputChannel('ts-r');
    context.subscriptions.push(output);
    const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    // Inherited by the tsserver VS Code forks from this extension host: the stand-in uses it
    // to pick the daemon before the first request arrives.
    if (root) process.env.TS_R_WORKSPACE_ROOT = root;

    statusItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
    statusItem.name = 'ts-r checker';
    statusItem.command = 'ts-r.showEngines';
    context.subscriptions.push(statusItem);
    lint = new LintController(output, () => findBinary(vscode.workspace.getConfiguration('ts-r').get('serverPath', '')), onTsrStatus);
    const lintRef = lint;
    context.subscriptions.push(
        vscode.commands.registerCommand('ts-r.reportParity', (/** @type {{open?: boolean} | undefined} */ opts) => reportParity(opts)),
        vscode.commands.registerCommand('ts-r.showEngines', () => showEngines()),
        vscode.commands.registerCommand('ts-r.restartLint', () => lintRef.update(true)),
        vscode.extensions.onDidChange(() => lintRef.update()),
        vscode.commands.registerCommand('ts-r.showStatus', () => showStatus()),
        vscode.commands.registerCommand('ts-r.stopDaemon', () => runCli(['stop', ...rootArgs()])),
        vscode.commands.registerCommand('ts-r.restoreTypeScript', async () => {
            await vscode.workspace.getConfiguration('ts-r').update('manageTypeScript', false, vscode.ConfigurationTarget.Global);
        }),
        vscode.window.onDidChangeActiveTextEditor(() => scheduleEngines(50)),
        vscode.workspace.onDidOpenTextDocument(() => scheduleEngines(500)),
        vscode.workspace.onDidChangeConfiguration(async (e) => {
            if (TYPESCRIPT_SETTINGS.some((k) => e.affectsConfiguration(k))) await apply(context, true);
            else if (e.affectsConfiguration('ts-r.checker')) await switchChecker(context);
            if (e.affectsConfiguration('ts-r.lint') || e.affectsConfiguration('eslint.enable')) await lintRef.update();
            else if (e.affectsConfiguration('ts-r.serverPath')) await lintRef.update(true);
        }),
    );
    await apply(context, false);
    await lint.update();
    const poll = setInterval(() => {
        if (vscode.window.state.focused) void refreshEngines();
    }, 3000);
    context.subscriptions.push({ dispose: () => clearInterval(poll) });
    scheduleEngines(1000);
    return {
        /** For tests: the status bar item's text and tooltip. */
        statusBar: () => ({ text: statusItem?.text, tooltip: statusItem?.tooltip, visible: statusShown }),
        refreshEngines,
    };
}

/**
 * Writes the stand-in and points VS Code's TypeScript at it (or undoes that).
 * @param {vscode.ExtensionContext} context
 * @param {boolean} changed
 */
async function apply(context, changed) {
    const cfg = vscode.workspace.getConfiguration('ts-r');
    if (!cfg.get('manageTypeScript', true)) {
        if (await restore(context)) await restartTsServer();
        return;
    }
    const bin = findBinary(cfg.get('serverPath', ''));
    if (!bin) {
        output.appendLine('ts-r-lsp not found: set ts-r.serverPath, add @ts-r.dev/lsp-<platform> to the project, or put ts-r-lsp on PATH.');
        if (await restore(context)) await restartTsServer();
        return;
    }
    const lib = writeStandIn(context, bin, cfg);
    const touched = await manage(context, lib);
    output.appendLine(`ts-r-lsp: ${bin}\nstand-in tsdk: ${lib}`);
    if (touched || changed) await restartTsServer();
}

/**
 * @param {vscode.ExtensionContext} context
 * @param {string} bin
 * @param {vscode.WorkspaceConfiguration} cfg
 * @returns {string} the stand-in lib dir
 */
function writeStandIn(context, bin, cfg) {
    const dir = path.join(context.globalStorageUri.fsPath, 'tsdk');
    const lib = path.join(dir, 'lib');
    fs.mkdirSync(lib, { recursive: true });
    const bundled = path.join(vscode.env.appRoot, 'extensions', 'node_modules', 'typescript');
    let version = '5.9.0';
    try {
        version = JSON.parse(fs.readFileSync(path.join(bundled, 'package.json'), 'utf8')).version || version;
    } catch {
        // Keep the default: VS Code only uses it to gate protocol features.
    }
    const settings = {
        memoryBudgetMB: cfg.get('memoryBudgetMB', 1500),
        idleShutdownMinutes: cfg.get('idleShutdownMinutes', 10),
    };
    const checker = cfg.get('checker', 'auto');
    appliedChecker = checker;
    // VS Code reads the version from <tsdk>/../package.json; claim the bundled one, which is
    // also what the stand-in falls back to.
    writeIfChanged(path.join(dir, 'package.json'), JSON.stringify({ name: 'typescript', version, private: true }, null, 2));
    writeIfChanged(path.join(dir, 'ts-r.json'), JSON.stringify({ bin, fallback: path.join(bundled, 'lib'), checker, settings }, null, 2));
    // tsserver.js (what VS Code starts), router.js, convert.js and lspclient.js (the bridge).
    const src = path.join(__dirname, 'standin');
    for (const f of fs.readdirSync(src)) {
        if (f.endsWith('.js')) writeIfChanged(path.join(lib, f), fs.readFileSync(path.join(src, f), 'utf8'));
    }
    return lib;
}

/** @param {string} file @param {string} text */
function writeIfChanged(file, text) {
    try {
        if (fs.readFileSync(file, 'utf8') === text) return;
    } catch {
        // Missing: write it.
    }
    fs.writeFileSync(file, text);
}

/**
 * Sets the managed TypeScript settings globally, remembering the user's values once.
 * @param {vscode.ExtensionContext} context
 * @param {string} lib
 * @returns {Promise<boolean>} whether anything changed
 */
async function manage(context, lib) {
    const conf = vscode.workspace.getConfiguration();
    const known = typeScriptSettingKeys();
    /** @type {Record<string, unknown>} */
    const saved = context.globalState.get(STATE_KEY) || {};
    let changed = false;
    for (const m of MANAGED) {
        const unified = known.has(m.unified);
        const key = unified ? m.unified : m.legacy;
        const want = unified ? m.value(lib) : (m.legacyValue || m.value)(lib);
        const current = conf.inspect(key)?.globalValue;
        if (current === want) continue;
        if (!(key in saved)) saved[key] = current === undefined ? null : current;
        await conf.update(key, want, vscode.ConfigurationTarget.Global);
        changed = true;
    }
    await context.globalState.update(STATE_KEY, saved);
    return changed;
}

/** Setting keys the installed TypeScript extension declares. */
function typeScriptSettingKeys() {
    /** @type {Set<string>} */
    const keys = new Set();
    const ext = vscode.extensions.getExtension('vscode.typescript-language-features');
    const contributed = ext?.packageJSON?.contributes?.configuration;
    for (const section of Array.isArray(contributed) ? contributed : contributed ? [contributed] : []) {
        for (const key of Object.keys(section.properties || {})) keys.add(key);
    }
    return keys;
}

/**
 * Puts back the user's own TypeScript settings.
 * @param {vscode.ExtensionContext} context
 * @returns {Promise<boolean>} whether anything changed
 */
async function restore(context) {
    /** @type {Record<string, unknown> | undefined} */
    const saved = context.globalState.get(STATE_KEY);
    if (!saved || Object.keys(saved).length === 0) return false;
    const conf = vscode.workspace.getConfiguration();
    for (const [key, value] of Object.entries(saved)) {
        await conf.update(key, value === null ? undefined : value, vscode.ConfigurationTarget.Global);
    }
    await context.globalState.update(STATE_KEY, undefined);
    output.appendLine('Restored your TypeScript settings.');
    return true;
}

async function restartTsServer() {
    try {
        await vscode.commands.executeCommand('typescript.restartTsServer');
    } catch {
        // The TypeScript extension is not active yet; it will start the stand-in itself.
    }
}

/**
 * ts-r.serverPath, then the project's @ts-r.dev/lsp-<platform> package, then PATH.
 * @param {string} configured
 * @returns {string | undefined}
 */
function findBinary(configured) {
    const exe = process.platform === 'win32' ? 'ts-r-lsp.exe' : 'ts-r-lsp';
    /** @type {string[]} */
    const candidates = [];
    if (configured) candidates.push(configured.replace(/^~(?=$|[\\/])/, os.homedir()));
    if (process.env.TS_R_LSP_BINARY) candidates.push(process.env.TS_R_LSP_BINARY);
    const libc = process.platform === 'linux' ? '-gnu' : process.platform === 'win32' ? '-msvc' : '';
    const platformPkg = `${process.platform}-${process.arch}${libc}`;
    for (const folder of vscode.workspace.workspaceFolders || []) {
        candidates.push(path.join(folder.uri.fsPath, 'node_modules', '@ts-r.dev', `lsp-${platformPkg}`, 'bin', exe));
    }
    for (const dir of (process.env.PATH || '').split(path.delimiter)) {
        if (dir) candidates.push(path.join(dir, exe));
    }
    candidates.push(path.join(os.homedir(), '.cargo', 'bin', exe));
    return candidates.find((c) => {
        try {
            return fs.statSync(c).isFile();
        } catch {
            return false;
        }
    });
}

function rootArgs() {
    const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    return root ? ['--root', root] : [];
}

/** @param {string[]} args @returns {Promise<string>} */
function runCli(args) {
    const bin = findBinary(vscode.workspace.getConfiguration('ts-r').get('serverPath', ''));
    if (!bin) return Promise.resolve('ts-r-lsp not found');
    return new Promise((resolve) => {
        childProcess.execFile(bin, args, { timeout: 10000 }, (err, stdout, stderr) => {
            resolve(err ? `${stderr || err.message}` : stdout);
        });
    });
}

async function showStatus() {
    const text = await runCli(['status', ...rootArgs()]);
    output.appendLine(text);
    output.show(true);
}

/** @param {any} st `ts-r/status` from the lint connection */
function onTsrStatus(st) {
    lastTsrStatus = st;
    renderStatus();
}

let statusShown = false;

/** The status bar: the bridge's engines when it runs, else the lint connection's ts-r status. */
function renderStatus() {
    if (!statusItem) return;
    const active = vscode.window.activeTextEditor?.document.uri;
    const d = describeEngines(lastEngines, active?.scheme === 'file' ? active.fsPath : undefined) || describeStatus(lastTsrStatus);
    if (!d) {
        statusItem.hide();
        statusShown = false;
        return;
    }
    statusItem.text = d.text;
    statusItem.tooltip = d.tooltip;
    statusItem.backgroundColor = d.warning ? new vscode.ThemeColor('statusBarItem.warningBackground') : undefined;
    statusItem.show();
    statusShown = true;
}

/** Whether VS Code's TypeScript server is (or should be) running: a JS/TS file is open. */
function hasTsDocuments() {
    return vscode.workspace.textDocuments.some((d) => d.uri.scheme === 'file' && TS_LANGUAGES.includes(d.languageId));
}

/** Whether the stand-in runs the bridge (`ts-r.checker` auto/on with the stand-in in place). */
function bridgeConfigured() {
    const cfg = vscode.workspace.getConfiguration('ts-r');
    return cfg.get('manageTypeScript', true) && appliedChecker !== undefined && appliedChecker !== 'off';
}

/**
 * Sends a `_`-prefixed request to the stand-in through VS Code's TypeScript extension
 * (`typescript.tsserverRequest` forwards those to the server unchanged).
 * @param {string} command
 * @param {unknown} args
 * @returns {Promise<any>} the response body, or undefined
 */
async function standInRequest(command, args) {
    try {
        /** @type {any} */
        const r = await vscode.commands.executeCommand('typescript.tsserverRequest', command, args, { lowPriority: true });
        return r?.body;
    } catch {
        return undefined;
    }
}

/** @type {NodeJS.Timeout | undefined} */
let enginesTimer;
/** @param {number} ms */
function scheduleEngines(ms) {
    if (enginesTimer) clearTimeout(enginesTimer);
    enginesTimer = setTimeout(() => void refreshEngines(), ms);
}

/** Asks the bridge which engine serves which project. */
async function refreshEngines() {
    if (!bridgeConfigured() || !hasTsDocuments()) {
        lastEngines = undefined;
    } else {
        const body = await standInRequest('_ts-r.engines', {});
        lastEngines = body?.bridge ? body : undefined;
    }
    renderStatus();
    return lastEngines;
}

/**
 * `ts-r.checker` changed: auto <-> on switches live in the running bridge; to or from `off`
 * restarts VS Code's TypeScript server (not the window).
 * @param {vscode.ExtensionContext} context
 */
async function switchChecker(context) {
    const prev = appliedChecker;
    const next = vscode.workspace.getConfiguration('ts-r').get('checker', /** @type {string} */ ('auto'));
    await apply(context, false);
    if (prev === next) return;
    output.appendLine(`ts-r.checker: ${prev} -> ${next}`);
    if (prev !== 'off' && next !== 'off' && hasTsDocuments()) {
        const r = await standInRequest('_ts-r.configure', { checker: next });
        if (r?.ok) {
            scheduleEngines(200);
            return;
        }
    }
    await restartTsServer();
    scheduleEngines(1500);
}

/** "ts-r: Show Engines": which engine serves each project, and what to do about it. */
async function showEngines() {
    await refreshEngines();
    const checker = vscode.workspace.getConfiguration('ts-r').get('checker', 'auto');
    /** @type {(vscode.QuickPickItem & {id?: string})[]} */
    const items = [];
    const e = lastEngines;
    if (e) {
        const parity = new Map((e.status?.projects || []).map((/** @type {any} */ p) => [p.config || p.project, p.parity]));
        for (const p of e.projects) items.push({ label: p.project, description: reason(p, e.checker, parity.get(p.config || p.project)) });
        items.push({ label: '', kind: vscode.QuickPickItemKind.Separator });
    }
    items.push({ label: '$(report) Report Parity Mismatch', id: 'report' });
    for (const [mode, what] of [
        ['auto', 'ts-r where the parity check passes, your TypeScript elsewhere'],
        ['on', 'ts-r for every project (your TypeScript only for differing errors)'],
        ['off', 'your TypeScript for everything'],
    ]) {
        items.push({ label: `${mode === checker ? '$(check) ' : '$(blank) '}ts-r.checker: ${mode}`, description: what, id: `mode:${mode}` });
    }
    items.push({ label: '$(output) Show ts-r Output', id: 'output' });
    const pick = await vscode.window.showQuickPick(items, { title: 'ts-r: which engine serves each project' });
    if (!pick?.id) return;
    if (pick.id === 'report') await reportParity();
    else if (pick.id === 'output') output.show(true);
    else if (pick.id.startsWith('mode:')) {
        await vscode.workspace.getConfiguration('ts-r').update('checker', pick.id.slice(5), vscode.ConfigurationTarget.Global);
    }
}

/**
 * "ts-r: Report Parity Mismatch": opens a prefilled GitHub issue with versions and diagnostic
 * codes with counts (never file names, messages or source). Returns the issue URL.
 * @param {{open?: boolean}} [opts]
 */
async function reportParity(opts) {
    await refreshEngines();
    let st = lastEngines?.status || lastTsrStatus;
    if (!st) {
        // Neither the bridge nor a lint connection: ask the daemon (it keeps the last `ts-r/status`).
        try {
            const daemon = JSON.parse(await runCli(['status', ...rootArgs()]));
            st = (daemon.slots || []).map((/** @type {any} */ s) => s.tsr).find(Boolean);
        } catch {
            st = undefined;
        }
    }
    const issue = parityIssue(st, { editor: `${vscode.env.appName} ${vscode.version}`, platform: `${process.platform}-${process.arch}` });
    if (!issue) {
        void vscode.window.showInformationMessage('ts-r: no parity mismatch to report: every checked project gives the same errors as your TypeScript.');
        return undefined;
    }
    output.appendLine(`Parity mismatch report:\n${issue.body}`);
    if (opts?.open !== false) await vscode.env.openExternal(vscode.Uri.parse(issue.url, true));
    return issue.url;
}

function deactivate() {
    return lint?.dispose();
}

module.exports = { activate, deactivate };
