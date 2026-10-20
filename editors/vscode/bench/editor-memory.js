// @ts-check
// Editor-side memory of a real VS Code on a real project: stock VS Code (its own TypeScript
// server, syntax server and type acquisition) against VS Code with the ts-r extension
// (`ts-r.checker: auto`). Each run gets a fresh user data dir; the probe
// (editor-memory-probe.js) runs in the extension host, opens the files (pinned tabs, never
// edited or saved), asks for a hover in each so every project loads, waits until nothing changes
// and sums the RSS of the extension host's process tree plus the ts-r daemon's tree (the
// language servers live there). VS Code's main, renderer and GPU processes are reported apart.
//
//   cargo build --release -p ts_r_lsp && npm install
//   scripts/limit.sh --mem-mb 6144 --time-s 900 -- node editors/vscode/bench/editor-memory.js \
//       --project <checkout> --files a.ts,b.ts --mode stock    # then --mode ts-r
//   (add `--cache /tmp/tsrm-cache` to two ts-r runs: the first is cold, the second warm)
//
// The pinned public repos (bench/repos.toml) each name their own file set: `cargo xtask bench
// --editor --repos <names>` prints these commands with `--project` and `--files` filled in
// (add `--run` to run them; results go to bench/results/editor/).
//
// The project is opened read-only in spirit: git is disabled in the window, nothing is saved,
// and the ts-r cache (parity results, daemon socket) goes to a fresh temp dir. Output: one JSON
// object on stdout. Not packaged; not a test.
'use strict';

const fs = require('fs');
const path = require('path');
const childProcess = require('child_process');
const { runTests, downloadAndUnzipVSCode } = require('@vscode/test-electron');

const ext = path.resolve(__dirname, '..');
const repo = path.resolve(ext, '..', '..');

function args() {
    const a = process.argv.slice(2);
    /** @param {string} k @param {string} d */
    const get = (k, d) => {
        const i = a.indexOf(k);
        return i >= 0 && a[i + 1] ? a[i + 1] : d;
    };
    return {
        project: get('--project', '') && path.resolve(get('--project', '')),
        mode: get('--mode', 'ts-r'),
        files: get('--files', '') ? get('--files', '').split(',') : [],
        settleS: Number(get('--settle-s', '20')),
        // Reuse a ts-r cache (parity results) between runs: the second run is the warm state
        // every later window sees. Keep the path short (/tmp/...).
        cache: get('--cache', ''),
    };
}

async function main() {
    const o = args();
    if (o.mode !== 'stock' && o.mode !== 'ts-r') throw new Error('--mode stock | ts-r');
    if (!o.project || !fs.existsSync(o.project)) throw new Error(`--project <dir>: not found (${o.project})`);
    if (!o.files.length) throw new Error('--files a.ts,b.ts: the files to open, relative to --project');
    const bin = process.env.TS_R_LSP_BIN || path.join(repo, 'target', 'release', 'ts-r-lsp');
    const libs = process.env.TS_R_LIB_DIR || path.join(repo, 'target', 'upstream', 'typescript', 'tsc', 'internal', 'bundled', 'libs');
    // A short path: the daemon's socket lives in the cache dir and must not be shared with
    // another ts-r on the same project (a long path falls back to /tmp/ts-r-lsp-<user>/).
    const dir = fs.mkdtempSync('/tmp/tsrm-');
    const userData = path.join(dir, 'u');
    fs.mkdirSync(path.join(userData, 'User'), { recursive: true });
    fs.writeFileSync(
        path.join(userData, 'User', 'settings.json'),
        JSON.stringify({
            'workbench.startupEditor': 'none',
            'telemetry.telemetryLevel': 'off',
            'update.mode': 'none',
            'extensions.autoUpdate': false,
            'git.enabled': false,
            'git.autoRepositoryDetection': false,
            'chat.disableAIFeatures': true,
            'security.workspace.trust.enabled': false,
        }),
    );
    // runTests needs an extension under development: the stock run gets an empty one.
    const probeExt = path.join(dir, 'probe-ext');
    fs.mkdirSync(probeExt);
    fs.writeFileSync(path.join(probeExt, 'package.json'), JSON.stringify({ name: 'probe', publisher: 'ts-r-bench', version: '0.0.0', engines: { vscode: '*' } }));
    const out = path.join(dir, 'result.json');
    const env = {
        TS_R_MEM_MODE: o.mode,
        TS_R_MEM_FILES: JSON.stringify(o.files),
        TS_R_MEM_SETTLE_S: String(o.settleS),
        TS_R_MEM_OUT: out,
        TS_R_E2E_BIN: bin,
        TS_R_LSP_BINARY: bin,
        TS_R_LIB_DIR: libs,
        TS_R_CACHE_DIR: o.cache ? path.resolve(o.cache) : path.join(dir, 'c'),
    };
    let failed = false;
    try {
        let vscodeExecutablePath = await downloadAndUnzipVSCode({ version: process.env.TS_R_VSCODE_VERSION || 'stable', cachePath: path.join(repo, 'target', 'vscode-test') });
        const renamed = path.join(path.dirname(vscodeExecutablePath), 'Code');
        if (!fs.existsSync(vscodeExecutablePath) && fs.existsSync(renamed)) vscodeExecutablePath = renamed;
        await runTests({
            vscodeExecutablePath,
            extensionDevelopmentPath: o.mode === 'ts-r' ? [ext] : [probeExt],
            extensionTestsPath: path.join(__dirname, 'editor-memory-probe.js'),
            launchArgs: [o.project, '--user-data-dir', userData, '--extensions-dir', path.join(dir, 'x'), '--disable-extensions', '--skip-welcome', '--skip-release-notes', '--disable-workspace-trust'],
            extensionTestsEnv: env,
        });
    } catch (e) {
        failed = true;
        console.error(`run failed: ${e instanceof Error ? e.message : e}`);
    } finally {
        if (fs.existsSync(out)) console.log(fs.readFileSync(out, 'utf8'));
        if (o.mode === 'ts-r') childProcess.spawnSync(bin, ['stop', '--root', o.project], { env: { ...process.env, ...env } });
        fs.rmSync(dir, { recursive: true, force: true });
    }
    if (failed) process.exit(1);
}

main().catch((e) => {
    console.error(e);
    process.exit(1);
});
