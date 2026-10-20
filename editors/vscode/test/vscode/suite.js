// @ts-check
// Runs inside VS Code's extension host (started by run.js). Plain assertions, no test framework:
// each step is logged to the results file run.js prints.
'use strict';

const fs = require('fs');
const path = require('path');
const assert = require('assert');
const childProcess = require('child_process');
const vscode = require('vscode');

/** @type {{steps: {step: string, ms: number, note?: unknown}[], ok?: boolean, error?: string}} */
const results = { steps: [] };
const t0 = Date.now();

/** @param {string} step @param {unknown} [note] */
function log(step, note) {
    results.steps.push({ step, ms: Date.now() - t0, ...(note === undefined ? {} : { note }) });
}

/** @param {number} ms */
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/**
 * Polls `f` until it returns a truthy value.
 * @template T
 * @param {string} what
 * @param {() => T | Promise<T>} f
 * @param {number} [ms]
 * @returns {Promise<Exclude<T, undefined | null | false | '' | 0>>}
 */
async function waitFor(what, f, ms = 60000) {
    const end = Date.now() + ms;
    for (;;) {
        const v = await f();
        if (v) return /** @type {any} */ (v);
        if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
        await sleep(200);
    }
}

/** Every process as {pid, ppid, rss (KB), cmd}. */
function processes() {
    const out = childProcess.execFileSync('ps', ['-axo', 'pid=,ppid=,rss=,command='], { encoding: 'utf8', maxBuffer: 64 << 20 });
    return out
        .split('\n')
        .map((l) => /^\s*(\d+)\s+(\d+)\s+(\d+)\s+(.*)$/.exec(l))
        .filter((m) => m !== null)
        .map((m) => ({ pid: Number(m[1]), ppid: Number(m[2]), rss: Number(m[3]), cmd: m[4] }));
}

/** `root` and all its descendants. @param {ReturnType<typeof processes>} all @param {number} root */
function tree(all, root) {
    const keep = new Set([root]);
    for (let grew = true; grew; ) {
        grew = false;
        for (const p of all) {
            if (!keep.has(p.pid) && keep.has(p.ppid)) {
                keep.add(p.pid);
                grew = true;
            }
        }
    }
    return all.filter((p) => keep.has(p.pid));
}

/** The workspace daemon's pid (`ts-r-lsp status`). @param {string} root */
function daemonPid(root) {
    const r = childProcess.spawnSync(/** @type {string} */ (process.env.TS_R_E2E_BIN), ['status', '--root', root], { encoding: 'utf8' });
    try {
        const st = JSON.parse(r.stdout);
        return Number(st.daemonPid || st.pid || 0);
    } catch {
        return 0;
    }
}

/**
 * Real tsserver processes started for this window (VS Code's extension host tree and the
 * workspace daemon's tree): `tsserver.js` other than our stand-in, and `ts-r-lsp tsserver`
 * fronts. Never looks at processes outside those two trees.
 * @param {string} root
 */
function tsservers(root) {
    const all = processes();
    const mine = [...tree(all, process.pid), ...tree(all, daemonPid(root) || -1)];
    const standIn = `${path.sep}tsdk${path.sep}lib${path.sep}tsserver.js`;
    const servers = mine.filter((p) =>
        // ts-r-lsp processes: only the `tsserver` subcommand (other fronts name a fallback tsserver.js in their flags).
        /(^|\/)ts-r-lsp\s/.test(p.cmd) ? /\stsserver(\s|$)/.test(p.cmd) : /tsserver\.js/.test(p.cmd) && !p.cmd.includes(standIn),
    );
    return { servers, rssMB: Math.round(mine.reduce((n, p) => n + p.rss, 0) / 1024), count: mine.length };
}

/** Opens a file in a pinned tab (a preview tab is replaced, and VS Code then closes the file). @param {vscode.Uri} uri */
async function show(uri) {
    return vscode.window.showTextDocument(await vscode.workspace.openTextDocument(uri), { preview: false });
}

/** @param {vscode.Uri} uri */
function diags(uri) {
    return vscode.languages.getDiagnostics(uri).map((d) => ({ source: d.source, code: typeof d.code === 'object' ? d.code.value : d.code, line: d.range.start.line, message: d.message }));
}

/** @param {any} item */
function label(item) {
    return typeof item.label === 'string' ? item.label : item.label.label;
}

async function main() {
    const folder = vscode.workspace.workspaceFolders?.[0];
    assert.ok(folder, 'no workspace folder');
    const root = folder.uri.fsPath;
    const ext = vscode.extensions.getExtension('ts-r.ts-r');
    assert.ok(ext, 'ts-r extension not loaded');
    /** @type {{statusBar: () => {text?: string, tooltip?: unknown, visible: boolean}, refreshEngines: () => Promise<any>}} */
    const api = await ext.activate();
    const tsdk = await waitFor('the stand-in tsdk setting', () => {
        const ts = vscode.workspace.getConfiguration();
        return ts.get('js/ts.tsdk.path') || ts.get('typescript.tsdk');
    });
    log('activated', { tsdk, checker: vscode.workspace.getConfiguration('ts-r').get('checker') });
    assert.strictEqual(vscode.workspace.getConfiguration('ts-r').get('checker'), 'auto');

    // --- A project that passes the parity check: ts-r answers, no tsserver ---------------------
    const a = vscode.Uri.file(path.join(root, 'pass', 'a.ts'));
    const c = vscode.Uri.file(path.join(root, 'pass', 'c.ts'));
    await show(a);
    const dA = await waitFor('ts-r diagnostics in pass/a.ts', () => {
        const d = diags(a);
        return d.some((x) => x.source === 'ts-r' && x.code === 2322) ? d : undefined;
    });
    log('pass/a.ts diagnostics', dA);
    assert.deepStrictEqual(
        dA.map((d) => [d.source, d.code, d.line]),
        [['ts-r', 2322, 0]],
    );
    const engines = await waitFor('the bridge to route pass/a.ts to ts-r', async () => {
        const e = await api.refreshEngines();
        return e?.files?.[a.fsPath] === 'ts-r' && e.projects.some((/** @type {any} */ p) => p.state === 'matched') && e;
    });
    log('engines after open', { projects: engines.projects, typescript: engines.typescript, syntax: engines.syntax });
    // A cold open (no cached parity result) may hand the file to TypeScript while the check
    // runs; that TypeScript stops when idle (run.js sets a short idle time).
    await waitFor('the on-demand TypeScript server to stop', async () => !(await api.refreshEngines())?.typescript?.running, 30000);
    const starts = (await api.refreshEngines()).typescript.starts;

    const hover = /** @type {vscode.Hover[]} */ (await vscode.commands.executeCommand('vscode.executeHoverProvider', a, new vscode.Position(0, 14)));
    const hoverText = hover.flatMap((h) => h.contents.map((m) => (typeof m === 'string' ? m : m.value))).join('\n');
    log('hover', hoverText);
    assert.match(hoverText, /const bad: number/);

    await show(c);
    const completions = /** @type {vscode.CompletionList} */ (await vscode.commands.executeCommand('vscode.executeCompletionItemProvider', c, new vscode.Position(1, 4), '.'));
    const labels = completions.items.map(label);
    log('completions after `bad.`', labels.slice(0, 12));
    assert.ok(labels.includes('toFixed'), `no toFixed in ${labels.join(', ')}`);
    const defs = /** @type {(vscode.Location | vscode.LocationLink)[]} */ (await vscode.commands.executeCommand('vscode.executeDefinitionProvider', c, new vscode.Position(1, 1)));
    const defTargets = defs.map((d) => ('targetUri' in d ? d.targetUri : d.uri).fsPath);
    log('definition of bad', defTargets);
    assert.ok(defTargets.includes(a.fsPath), `definition went to ${defTargets.join(', ')}`);

    // Cursor moves make VS Code ask for code actions (implicit refactors, the lightbulb); they
    // must not start TypeScript.
    await show(a);
    for (const [line, col] of [
        [1, 4],
        [1, 20],
        [2, 11],
        [3, 0],
    ]) {
        const ed = vscode.window.activeTextEditor;
        if (ed) ed.selection = new vscode.Selection(line, col, line, col);
        await sleep(400);
    }
    await sleep(1500);
    const afterUse = await api.refreshEngines();
    const procs = tsservers(root);
    log('pass/: processes', { tsservers: procs.servers.map((p) => p.cmd.slice(0, 160)), treeRssMB: procs.rssMB, processes: procs.count, typescript: afterUse.typescript, syntax: afterUse.syntax });
    assert.strictEqual(afterUse.typescript.running, false, 'TypeScript server running for a ts-r project');
    assert.strictEqual(afterUse.typescript.starts, starts, 'TypeScript server started by hover / completions / definition / cursor moves');
    assert.deepStrictEqual(procs.servers, [], 'a tsserver process exists');
    // The tsconfig's own problems, as tsserver reports them (`configFileDiag`).
    const cfgUri = vscode.Uri.file(path.join(root, 'pass', 'tsconfig.json'));
    const dCfg = await waitFor('the tsconfig diagnostic', () => {
        const d = diags(cfgUri).filter((x) => x.source === 'ts');
        return d.length ? d : undefined;
    });
    log('pass/tsconfig.json diagnostics', dCfg);
    assert.strictEqual(dCfg.length, 1);
    assert.match(dCfg[0].message, /missing-base\.json/);
    assert.strictEqual(dCfg[0].line, 0);
    const bar = await waitFor('the status bar', () => {
        const s = api.statusBar();
        return s.visible && s.text && /ts-r/.test(s.text) && s;
    });
    log('status bar on pass/a.ts', bar);

    // The refactor menu (asked explicitly) is ts-r's: no TypeScript.
    const menu = /** @type {vscode.CodeAction[]} */ (
        await vscode.commands.executeCommand('vscode.executeCodeActionProvider', a, new vscode.Range(1, 16, 1, 21), vscode.CodeActionKind.Refactor.value)
    );
    const menuTitles = menu.map((x) => x.title);
    log('refactor menu on pass/a.ts `twice`', menuTitles);
    for (const t of ['Convert named export to default export', 'Move to a new file']) assert.ok(menuTitles.includes(t), `no ${t} in ${menuTitles.join(', ')}`);
    await sleep(1500);
    const afterMenu = await api.refreshEngines();
    assert.strictEqual(afterMenu.typescript.starts, starts, 'TypeScript server started for the refactor menu');

    // An explicit request ts-r has no answer for (call hierarchy) starts TypeScript on demand.
    await vscode.commands.executeCommand('vscode.prepareCallHierarchy', a, new vscode.Position(1, 18));
    const onDemand = await waitFor('TypeScript on demand', async () => {
        const e = await api.refreshEngines();
        return e?.typescript?.running && e.typescript;
    });
    log('call hierarchy on pass/a.ts', onDemand);
    assert.strictEqual(onDemand.starts, starts + 1);
    assert.deepStrictEqual(
        diags(a).map((d) => [d.source, d.code]),
        [['ts-r', 2322]],
        'pass/a.ts got TypeScript diagnostics too',
    );

    // --- A project that fails the parity check: TypeScript's diagnostics, no duplicates ---------
    const b = vscode.Uri.file(path.join(root, 'fail', 'b.ts'));
    await show(b);
    await waitFor('TypeScript diagnostics in fail/b.ts', () => diags(b).some((x) => x.source === 'ts' && x.code === 2322));
    await waitFor('fail/ to be served by TypeScript', async () => (await api.refreshEngines())?.files?.[b.fsPath] === 'typescript');
    await sleep(3000); // let any second engine publish, if there were one
    const dB = diags(b);
    log('fail/b.ts diagnostics', dB);
    assert.deepStrictEqual(
        dB.map((d) => [d.source, d.code, d.line]),
        [['ts', 2322, 0]],
    );
    assert.deepStrictEqual(
        diags(a).map((d) => [d.source, d.code]),
        [['ts-r', 2322]],
        'pass/a.ts changed engine or got duplicates',
    );
    const barB = await waitFor('the status bar on fail/b.ts', () => {
        const s = api.statusBar();
        return s.text && /TypeScript/.test(s.text) && s;
    });
    log('status bar on fail/b.ts', barB);
    const procsB = tsservers(root);
    log('fail/: processes', { tsservers: procsB.servers.map((p) => p.cmd.slice(0, 160)), treeRssMB: procsB.rssMB });
    assert.ok(procsB.servers.length > 0, 'no tsserver for the project on TypeScript');

    const url = /** @type {string} */ (await vscode.commands.executeCommand('ts-r.reportParity', { open: false }));
    log('parity issue url', url);
    assert.ok(url && url.startsWith('https://github.com/Webb-Ventures/typescript-rust/issues/new'), String(url));
    const decoded = decodeURIComponent(url);
    assert.match(decoded, /TS9999/);
    for (const leak of ['b.ts', 'a.ts', 'fail/', 'pass/', root, 'other', 'Injected']) assert.ok(!decoded.includes(leak), `issue leaks ${leak}`);

    // --- Switching: auto -> on is live, -> off restarts the TS server (no window reload) -------
    const cfg = () => vscode.workspace.getConfiguration('ts-r');
    await cfg().update('checker', 'on', vscode.ConfigurationTarget.Global);
    await waitFor('checker on: fail/b.ts on ts-r', async () => {
        const e = await api.refreshEngines();
        return e?.checker === 'on' && e.files?.[b.fsPath] === 'ts-r';
    });
    /** @type {any} */
    let lastOn = [];
    const dOn = await waitFor('one diagnostic in fail/b.ts with checker on', async () => {
        const d = diags(b);
        lastOn = { diags: d, engines: await api.refreshEngines() };
        return d.length === 1 && d[0].code === 2322 && d;
    }).catch((e) => {
        throw new Error(`${e.message}; last seen: ${JSON.stringify(lastOn)}`);
    });
    log('checker on: fail/b.ts diagnostics', dOn);
    await cfg().update('checker', 'off', vscode.ConfigurationTarget.Global);
    await waitFor('checker off: pass/a.ts from TypeScript', () => diags(a).some((d) => d.source === 'ts' && d.code === 2322));
    assert.deepStrictEqual(
        diags(a).map((d) => [d.source, d.code]),
        [['ts', 2322]],
    );
    log('checker off: pass/a.ts diagnostics', diags(a));
    await cfg().update('checker', undefined, vscode.ConfigurationTarget.Global);
    await waitFor('checker auto again: pass/a.ts from ts-r', () => diags(a).some((d) => d.source === 'ts-r'), 60000);
    log('checker auto again', diags(a));
    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
}

/** Called by @vscode/test-electron. */
async function run() {
    try {
        await main();
        results.ok = true;
    } catch (e) {
        results.ok = false;
        results.error = e instanceof Error ? e.stack : String(e);
        throw e;
    } finally {
        const out = process.env.TS_R_E2E_OUT;
        if (out) fs.writeFileSync(out, JSON.stringify(results, null, 2));
    }
}

module.exports = { run };
