// @ts-check
// End-to-end test in a real VS Code (downloaded by @vscode/test-electron into
// target/vscode-test), with a fresh user data dir, extensions dir and workspace in a temp dir.
// The workspace has two projects: `pass/` (the parity check passes with the real tsc) and
// `fail/` (its tsc output gets one injected extra line, so parity always fails there). The
// assertions live in suite.js, which runs inside the extension host.
//
//   cargo build --release -p ts_r_lsp && npm install
//   scripts/limit.sh --mem-mb 6144 --time-s 600 -- node editors/vscode/test/vscode/run.js
//
// Env: TS_R_LSP_BIN (default target/release/ts-r-lsp), TS_R_LIB_DIR (default
// target/upstream/.../libs), TS_R_VSCODE_VERSION (default `stable`).
'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const childProcess = require('child_process');
const { runTests, downloadAndUnzipVSCode } = require('@vscode/test-electron');

const ext = path.resolve(__dirname, '..', '..');
const repo = path.resolve(ext, '..', '..');
const bin = process.env.TS_R_LSP_BIN || path.join(repo, 'target', 'release', process.platform === 'win32' ? 'ts-r-lsp.exe' : 'ts-r-lsp');
const libs = process.env.TS_R_LIB_DIR || path.join(repo, 'target', 'upstream', 'typescript', 'tsc', 'internal', 'bundled', 'libs');
const typescript = path.join(ext, 'node_modules', 'typescript');

/** Writes the two-project workspace; returns its root. */
function fixture(/** @type {string} */ dir) {
    const ws = path.join(dir, 'ws');
    const files = {
        // A missing `extends` (TS6053 from tsc and ts-r alike): shown on the tsconfig.
        'pass/tsconfig.json': JSON.stringify({ extends: './missing-base.json', compilerOptions: { strict: true, noEmit: true, target: 'es2022', module: 'esnext', moduleResolution: 'bundler', lib: ['es2022'], types: [] } }),
        'pass/a.ts': 'export const bad: number = "x";\nexport function twice(n: number): number {\n    return n * 2;\n}\n',
        'pass/c.ts': "import { bad } from './a';\nbad.toFixed();\n",
        'fail/tsconfig.json': JSON.stringify({ compilerOptions: { strict: true, noEmit: true, target: 'es2022', module: 'esnext', moduleResolution: 'bundler', lib: ['es2022'], types: [] } }),
        'fail/b.ts': 'export const other: string = 1;\n',
    };
    for (const [rel, text] of Object.entries(files)) {
        fs.mkdirSync(path.dirname(path.join(ws, rel)), { recursive: true });
        fs.writeFileSync(path.join(ws, rel), text);
    }
    fs.mkdirSync(path.join(ws, 'node_modules'), { recursive: true });
    fs.symlinkSync(typescript, path.join(ws, 'node_modules', 'typescript'), 'dir');
    return fs.realpathSync(ws);
}

/** The parity check's tsc: the real one, plus an extra diagnostic for `fail/`. */
function tscWrapper(/** @type {string} */ dir) {
    const script = path.join(dir, 'tsc-with-injected-difference.sh');
    fs.writeFileSync(
        script,
        [
            '#!/bin/sh',
            `"${process.execPath}" "${path.join(typescript, 'lib', 'tsc.js')}" "$@"`,
            'code=$?',
            'case "$2" in *fail/tsconfig.json) echo "fail/b.ts(1,1): error TS9999: Injected difference."; code=2;; esac',
            'exit $code',
            '',
        ].join('\n'),
    );
    fs.chmodSync(script, 0o755);
    return script;
}

async function main() {
    for (const [what, p] of [
        ['ts-r-lsp', bin],
        ['lib files', path.join(libs, 'lib.es5.d.ts')],
        ['typescript (npm install)', path.join(typescript, 'lib', 'tsc.js')],
    ]) {
        if (!fs.existsSync(p)) throw new Error(`missing ${what}: ${p}`);
    }
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'tsr-vscode-e2e-'));
    const ws = fixture(dir);
    const userData = path.join(dir, 'user');
    fs.mkdirSync(path.join(userData, 'User'), { recursive: true });
    fs.writeFileSync(
        path.join(userData, 'User', 'settings.json'),
        JSON.stringify({
            'workbench.startupEditor': 'none',
            'telemetry.telemetryLevel': 'off',
            'update.mode': 'none',
            'extensions.autoUpdate': false,
            'security.workspace.trust.enabled': false,
            'typescript.tsserver.trace': 'messages',
        }),
    );
    const out = path.join(dir, 'results.json');
    const env = {
        TS_R_LSP_BINARY: bin,
        TS_R_LIB_DIR: libs,
        TS_R_CACHE_DIR: path.join(dir, 'cache'),
        TS_R_LSP_PARITY_TSC: JSON.stringify(['/bin/sh', tscWrapper(dir)]),
        TS_R_LSP_LINGER_MS: '1000',
        TS_R_BRIDGE_IDLE_MS: '4000',
        TS_R_E2E_OUT: out,
        TS_R_E2E_BIN: bin,
    };
    let failed = false;
    try {
        let vscodeExecutablePath = await downloadAndUnzipVSCode({
            version: process.env.TS_R_VSCODE_VERSION || 'stable',
            cachePath: path.join(repo, 'target', 'vscode-test'),
        });
        // macOS builds since 1.14x name the binary `Code`; test-electron still expects `Electron`.
        const renamed = path.join(path.dirname(vscodeExecutablePath), 'Code');
        if (!fs.existsSync(vscodeExecutablePath) && fs.existsSync(renamed)) vscodeExecutablePath = renamed;
        await runTests({
            vscodeExecutablePath,
            extensionDevelopmentPath: ext,
            extensionTestsPath: path.join(__dirname, 'suite.js'),
            launchArgs: [ws, '--user-data-dir', userData, '--extensions-dir', path.join(dir, 'extensions'), '--disable-extensions', '--skip-welcome', '--skip-release-notes', '--disable-workspace-trust'],
            extensionTestsEnv: env,
        });
    } catch (e) {
        failed = true;
        console.error(`VS Code tests failed: ${e instanceof Error ? e.message : e}`);
    } finally {
        if (fs.existsSync(out)) console.log(fs.readFileSync(out, 'utf8'));
        childProcess.spawnSync(bin, ['stop', '--root', ws], { env: { ...process.env, ...env } });
        // VS Code's own logs, kept when the run failed.
        if (failed) console.error(`kept ${dir} for its logs`);
        else fs.rmSync(dir, { recursive: true, force: true });
    }
    if (failed) process.exit(1);
}

main().catch((e) => {
    console.error(e);
    process.exit(1);
});
