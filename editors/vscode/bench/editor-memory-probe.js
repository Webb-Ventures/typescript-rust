// @ts-check
// Runs in VS Code's extension host for bench/editor-memory.js: opens the files, waits until the
// TypeScript engines are done, samples memory. Writes one JSON object to TS_R_MEM_OUT.
'use strict';

const fs = require('fs');
const path = require('path');
const childProcess = require('child_process');
const vscode = require('vscode');

/** @param {number} ms */
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function processes() {
    const out = childProcess.execFileSync('ps', ['-axo', 'pid=,ppid=,rss=,command='], { encoding: 'utf8', maxBuffer: 64 << 20 });
    return out
        .split('\n')
        .map((l) => /^\s*(\d+)\s+(\d+)\s+(\d+)\s+(.*)$/.exec(l))
        .filter((m) => m !== null)
        .map((m) => ({ pid: Number(m[1]), ppid: Number(m[2]), rss: Number(m[3]), cmd: m[4] }));
}

/** @param {ReturnType<typeof processes>} all @param {number} root */
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

/** A short name for a process. @param {string} cmd */
function kind(cmd) {
    if (/ts-r-lsp\S*\s+daemon\b/.test(cmd)) return 'ts-r-lsp daemon';
    if (/ts-r-lsp\S*\s.*--lint-only/.test(cmd)) return 'ts-r-lsp lint front';
    if (/ts-r-lsp\S*\s.*--type-backend ts-r/.test(cmd)) return 'ts-r-lsp checker front (bridge)';
    if (/ts-r-lsp\S*\s.*\stsserver(\s|$)/.test(cmd)) return 'ts-r-lsp tsserver front';
    if (/ts-r-lsp/.test(cmd)) return 'ts-r-lsp other';
    if (/tsdk\/lib\/tsserver\.js/.test(cmd)) return 'stand-in (router)';
    if (/tsc\.js/.test(cmd)) return 'tsc (parity check)';
    if (/typingsInstaller/.test(cmd)) return 'typingsInstaller';
    if (/tsserver\.js.*--serverMode (syntactic|partialSemantic)/.test(cmd)) return 'tsserver (syntax)';
    if (/tsserver\.js/.test(cmd)) return 'tsserver (semantic)';
    if (/extension-host|--type=extensionHost|extensionHost/.test(cmd)) return 'extension host';
    if (/jsonServer|json-language-features/.test(cmd)) return 'json server';
    if (/--type=renderer/.test(cmd)) return 'renderer';
    if (/--type=gpu-process/.test(cmd)) return 'gpu';
    if (/--type=utility/.test(cmd)) return 'utility';
    return path.basename(cmd.split(' ')[0]);
}

/** @param {string} root */
function daemonPid(root) {
    const bin = process.env.TS_R_E2E_BIN;
    if (!bin) return 0;
    const r = childProcess.spawnSync(bin, ['status', '--root', root], { encoding: 'utf8' });
    try {
        const st = JSON.parse(r.stdout);
        return Number(st.daemonPid || st.pid || 0);
    } catch {
        return 0;
    }
}

/** @param {string} root */
function sample(root) {
    const all = processes().filter((p) => !/^ps\s/.test(p.cmd)); // not our own sampling
    const dpid = daemonPid(root);
    // The daemon may be a child of the front that started it: count each process once, the
    // daemon's tree apart from the extension host's.
    const daemon = dpid ? tree(all, dpid) : [];
    const editorSide = tree(all, process.pid).filter((p) => !daemon.some((q) => q.pid === p.pid));
    const me = all.find((p) => p.pid === process.pid);
    const app = me ? tree(all, me.ppid).filter((p) => !editorSide.some((q) => q.pid === p.pid) && !daemon.some((q) => q.pid === p.pid)) : [];
    const mb = (/** @type {typeof all} */ ps) => Math.round(ps.reduce((n, p) => n + p.rss, 0) / 1024);
    /** @type {Record<string, number>} */
    const byKind = {};
    for (const p of [...editorSide, ...daemon]) byKind[kind(p.cmd)] = (byKind[kind(p.cmd)] || 0) + Math.round(p.rss / 1024);
    // Per process: kind, MB, parent kind, and the tsserver flags that tell instances apart.
    const all2 = [...editorSide, ...daemon];
    const detail = all2.map((p) => ({
        pid: p.pid,
        kind: kind(p.cmd),
        mb: Math.round(p.rss / 1024),
        parent: kind(all.find((q) => q.pid === p.ppid)?.cmd || '?'),
        flags: (p.cmd.match(/--(serverMode|max-old-space-size|useInferredProjectPerProjectRoot|disableAutomaticTypingAcquisition|cancellationPipeName|useNodeIpc|stdio|lsp)\S*( [^-]\S*)?/g) || []).join(' '),
    }));
    return { editorMB: mb(editorSide) + mb(daemon), extHostTreeMB: mb(editorSide), daemonTreeMB: mb(daemon), vscodeOtherMB: mb(app), byKind, processes: all2.length, detail };
}

/** A fingerprint of every diagnostic, to see when the engines stop publishing. */
function diagnosticsKey() {
    return vscode.languages
        .getDiagnostics()
        .map(([u, ds]) => `${u.fsPath}:${ds.map((d) => `${d.source}${typeof d.code === 'object' ? d.code.value : d.code}@${d.range.start.line}`).join(',')}`)
        .sort()
        .join('|');
}

async function main() {
    const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    if (!root) throw new Error('no folder');
    const mode = process.env.TS_R_MEM_MODE;
    const files = /** @type {string[]} */ (JSON.parse(process.env.TS_R_MEM_FILES || '[]'));
    const settleMs = Number(process.env.TS_R_MEM_SETTLE_S || '20') * 1000;
    const t0 = Date.now();
    /** @type {any} */
    let api;
    if (mode === 'ts-r') {
        const ext = vscode.extensions.getExtension('ts-r.ts-r');
        if (!ext) throw new Error('ts-r extension not loaded');
        api = await ext.activate();
        for (let i = 0; i < 100 && !vscode.workspace.getConfiguration().get('js/ts.tsdk.path') && !vscode.workspace.getConfiguration().get('typescript.tsdk'); i++) await sleep(100);
    }
    let peak = 0;
    const peakTimer = setInterval(() => {
        try {
            peak = Math.max(peak, sample(root).editorMB);
        } catch {
            // ps failed once: skip the sample.
        }
    }, 1000);
    try {
        for (const rel of files) {
            const uri = vscode.Uri.file(path.join(root, rel));
            const doc = await vscode.workspace.openTextDocument(uri);
            await vscode.window.showTextDocument(doc, { preview: false });
        }
        // A hover per file: every file's project is loaded and checked, in either engine.
        /** @type {Record<string, number>} */
        const hoverMs = {};
        for (const rel of files) {
            const uri = vscode.Uri.file(path.join(root, rel));
            await vscode.window.showTextDocument(uri, { preview: false });
            const h0 = Date.now();
            await vscode.commands.executeCommand('vscode.executeHoverProvider', uri, new vscode.Position(0, 9));
            hoverMs[rel] = Date.now() - h0;
        }
        // Settled: no diagnostics change, no parity check running, for settleMs.
        let key = '';
        let quietSince = Date.now();
        const deadline = Date.now() + 600000;
        /** @type {any} */
        let engines;
        while (Date.now() - quietSince < settleMs && Date.now() < deadline) {
            await sleep(1000);
            engines = api ? await api.refreshEngines() : undefined;
            const checking = (engines?.status?.projects || []).some((/** @type {any} */ p) => p.state === 'checking');
            const k = diagnosticsKey() + (checking ? '#checking' : '');
            if (k !== key || checking) {
                key = k;
                quietSince = Date.now();
            }
        }
        const samples = [];
        for (let i = 0; i < 5; i++) {
            samples.push(sample(root));
            await sleep(2000);
        }
        const last = samples[samples.length - 1];
        const diags = vscode.languages.getDiagnostics().filter(([, ds]) => ds.length);
        /** @type {Record<string, number>} */
        const bySource = {};
        /** @type {Record<string, number>} diagnostic codes with severity: what the engines disagree on */
        const byCode = {};
        for (const [uri, ds] of diags) {
            for (const d of ds) {
                bySource[d.source || '?'] = (bySource[d.source || '?'] || 0) + 1;
                // Diagnostics without a code (tsconfig problems) also name the file they are on.
                const code = typeof d.code === 'object' ? d.code.value : d.code;
                const k = `${d.source || '?'} ${code ?? `${path.basename(uri.fsPath)}: ${d.message.slice(0, 80)}`} ${vscode.DiagnosticSeverity[d.severity]}`;
                byCode[k] = (byCode[k] || 0) + 1;
            }
        }
        const result = {
            mode,
            vscode: vscode.version,
            files: files.length,
            settledAfterS: Math.round((Date.now() - t0) / 1000),
            editorMB: last.editorMB,
            editorMBMedian: samples.map((s) => s.editorMB).sort((x, y) => x - y)[2],
            peakMB: Math.max(peak, ...samples.map((s) => s.editorMB)),
            extHostTreeMB: last.extHostTreeMB,
            daemonTreeMB: last.daemonTreeMB,
            vscodeOtherMB: last.vscodeOtherMB,
            byKind: last.byKind,
            processes: last.processes,
            detail: last.detail,
            hoverMs,
            diagnostics: bySource,
            diagnosticCodes: byCode,
            engines: engines && {
                checker: engines.checker,
                projects: engines.projects.map((/** @type {any} */ p) => ({ project: p.project, state: p.state, engine: p.engine, openFiles: p.openFiles })),
                typescript: engines.typescript,
                syntax: engines.syntax,
            },
        };
        const out = process.env.TS_R_MEM_OUT;
        if (out) fs.writeFileSync(out, JSON.stringify(result, null, 2));
    } finally {
        clearInterval(peakTimer);
        await vscode.commands.executeCommand('workbench.action.closeAllEditors');
    }
}

module.exports = { run: main };
