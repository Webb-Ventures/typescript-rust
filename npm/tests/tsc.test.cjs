// Unit tests for the @ts-r.dev/typescript `tsc` (npm/typescript/lib/tsc.cjs).
// Run with: node --test npm/tests/*.test.cjs
'use strict';

const assert = require('node:assert');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');

const { defaultPretty, parseArgs, readConfig } = require('../typescript/lib/tsc.cjs');

const temps = [];
process.on('exit', () => {
  for (const d of temps) fs.rmSync(d, { recursive: true, force: true });
});

function tempDir() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'ts-r-tsc-'));
  temps.push(dir);
  return dir;
}

test('readConfig reads a base extended along many paths once', () => {
  // c0 extends c1 twice, c1 extends c2 twice, ...: 2^31 paths to c31 if each one is followed.
  const dir = tempDir();
  const n = 31;
  for (let i = 0; i < n; i++) {
    const next = `./c${i + 1}.json`;
    fs.writeFileSync(path.join(dir, `c${i}.json`), JSON.stringify({ extends: [next, next] }));
  }
  fs.writeFileSync(path.join(dir, `c${n}.json`), JSON.stringify({ compilerOptions: { noEmit: true } }));
  const read = readConfig(path.join(dir, 'c0.json'));
  assert.ok(read);
  assert.strictEqual(read.options.noEmit, true);
  assert.strictEqual(read.files.length, n + 1);
});

test('readConfig merges a shared base in tsc order', () => {
  const dir = tempDir();
  fs.writeFileSync(path.join(dir, 'base.json'), JSON.stringify({ compilerOptions: { strict: true, noEmit: true } }));
  fs.writeFileSync(path.join(dir, 'a.json'), JSON.stringify({ extends: './base.json', compilerOptions: { strict: false } }));
  fs.writeFileSync(path.join(dir, 'tsconfig.json'), JSON.stringify({ extends: ['./a.json', './base.json'] }));
  const read = readConfig(path.join(dir, 'tsconfig.json'));
  assert.deepStrictEqual(read.options, { strict: true, noEmit: true });
});

test('readConfig rejects a circular chain', () => {
  const dir = tempDir();
  fs.writeFileSync(path.join(dir, 'a.json'), JSON.stringify({ extends: './b.json' }));
  fs.writeFileSync(path.join(dir, 'b.json'), JSON.stringify({ extends: './a.json' }));
  assert.strictEqual(readConfig(path.join(dir, 'a.json')), null);
});

const TSC = path.join(__dirname, '..', 'typescript', 'lib', 'tsc.cjs');
// Next.js 16's type check (next build): its --showConfig call, then the check itself (with
// --tsBuildInfoFile only for incremental or composite projects).
const NEXT_SHOW_CONFIG = (project) => ['--showConfig', '--project', project, '--pretty', 'false'];
const NEXT_CHECK = (project, buildInfo) => [
  '--project', project, '--noEmit', '--declarationMap', 'false', '--emitDeclarationOnly', 'false',
  '--tsBuildInfoFile', buildInfo,
];

// Stand-ins for ts-r-check and TypeScript's tsc: each appends its arguments to <dir>/<name>.log
// and prints `output` (a diagnostic in tsc's plain format) with exit code 1, or nothing and 0.
function standIns(dir, { ours = '', theirs = '' } = {}) {
  const bin = path.join(dir, 'bin');
  fs.mkdirSync(bin);
  const script = (log, output) =>
    `#!/usr/bin/env node\nrequire('fs').appendFileSync(${JSON.stringify(log)}, JSON.stringify(process.argv.slice(2)) + '\\n');\n` +
    (output ? `process.stdout.write(${JSON.stringify(output + '\n')});\nprocess.exitCode = 1;\n` : '');
  fs.writeFileSync(path.join(bin, 'ts-r-check'), script(path.join(dir, 'ts-r-check.log'), ours), { mode: 0o755 });
  const ts = path.join(dir, 'typescript');
  fs.mkdirSync(path.join(ts, 'bin'), { recursive: true });
  fs.writeFileSync(path.join(ts, 'package.json'), JSON.stringify({ name: 'typescript', version: '7.0.2', bin: { tsc: './bin/tsc' } }));
  fs.writeFileSync(path.join(ts, 'bin', 'tsc'), script(path.join(dir, 'tsc.log'), theirs), { mode: 0o755 });
  return { bin, ts };
}

function calls(dir, name) {
  const file = path.join(dir, `${name}.log`);
  if (!fs.existsSync(file)) return [];
  return fs.readFileSync(file, 'utf8').trim().split('\n').map((l) => JSON.parse(l));
}

function runTsc(dir, argv, env = {}) {
  const base = { ...process.env };
  for (const k of ['TS_R_TSC', 'TS_R_TSC_QUIET', 'TS_R_TSC_VERBOSE', 'NO_COLOR', 'FORCE_COLOR']) delete base[k];
  return spawnSync(process.execPath, [TSC, ...argv], {
    cwd: dir,
    encoding: 'utf8',
    env: { ...base, TS_R_BIN_DIR: path.join(dir, 'bin'), TS_R_TYPESCRIPT: path.join(dir, 'typescript'), TS_R_CACHE_DIR: path.join(dir, 'cache'), ...env },
  });
}

function project(compilerOptions = { noEmit: true }) {
  const dir = tempDir();
  fs.writeFileSync(path.join(dir, 'tsconfig.json'), JSON.stringify({ compilerOptions }));
  return dir;
}

test('parseArgs takes the emit options Next.js turns off, and --tsBuildInfoFile', () => {
  const r = parseArgs(NEXT_CHECK('/p/tsconfig.json', '/p/.next/cache/.tsbuildinfo'));
  assert.deepStrictEqual(r.unsupported, []);
  assert.strictEqual(r.project, '/p/tsconfig.json');
  assert.strictEqual(r.noEmit, true);
  assert.deepStrictEqual(r.passed, ['--declarationMap', 'false', '--emitDeclarationOnly', 'false', '--tsBuildInfoFile', '/p/.next/cache/.tsbuildinfo']);
  assert.deepStrictEqual(parseArgs(['--SourceMap', 'false', '-d', 'false']).passed, ['--SourceMap', 'false', '-d', 'false']);
});

test('parseArgs leaves emit options that are on to TypeScript', () => {
  assert.deepStrictEqual(parseArgs(['--declarationMap']).unsupported, ['--declarationMap']);
  assert.deepStrictEqual(parseArgs(['--emitDeclarationOnly', 'true']).unsupported, ['--emitDeclarationOnly', 'true']);
  assert.deepStrictEqual(parseArgs(['--tsBuildInfoFile']).unsupported, ['--tsBuildInfoFile']);
  assert.deepStrictEqual(parseArgs(NEXT_SHOW_CONFIG('tsconfig.json')).unsupported, ['--showConfig']);
});

test('defaultPretty follows tsc 7: NO_COLOR, then FORCE_COLOR, then the terminal', () => {
  assert.strictEqual(defaultPretty({}, false), false);
  assert.strictEqual(defaultPretty({}, true), true);
  assert.strictEqual(defaultPretty({ FORCE_COLOR: '1' }, false), true);
  assert.strictEqual(defaultPretty({ FORCE_COLOR: '0' }, false), true);
  assert.strictEqual(defaultPretty({ FORCE_COLOR: '' }, false), false);
  assert.strictEqual(defaultPretty({ NO_COLOR: '1', FORCE_COLOR: '1' }, true), false);
  assert.strictEqual(defaultPretty({ NO_COLOR: '', FORCE_COLOR: '1' }, false), true);
});

const posix = { skip: process.platform === 'win32' };

test('tsc ignores pretty in tsconfig, like tsc 7, and honours FORCE_COLOR and NO_COLOR', posix, () => {
  const dir = project({ noEmit: true, pretty: true });
  standIns(dir);
  const pretty = (env) => {
    const out = runTsc(dir, [], { TS_R_TSC: 'ts-r', ...env });
    assert.strictEqual(out.status, 0, out.stderr);
    const args = calls(dir, 'ts-r-check').pop();
    return args[args.indexOf('--pretty') + 1];
  };
  assert.strictEqual(pretty({}), 'false');
  assert.strictEqual(pretty({ FORCE_COLOR: '1' }), 'true');
  assert.strictEqual(pretty({ FORCE_COLOR: '1', NO_COLOR: '1' }), 'false');
});

test("Next.js's type check runs on ts-r-check when the parity check passes", posix, () => {
  const dir = project({ noEmit: true, incremental: true });
  standIns(dir);
  const config = path.join(dir, 'tsconfig.json');
  const buildInfo = path.join(dir, '.next', 'cache', '.tsbuildinfo');
  const passed = ['--declarationMap', 'false', '--emitDeclarationOnly', 'false', '--tsBuildInfoFile', buildInfo];
  for (let i = 0; i < 2; i++) {
    const out = runTsc(dir, NEXT_CHECK(config, buildInfo), { FORCE_COLOR: '1' });
    assert.strictEqual(out.status, 0, out.stderr);
    assert.strictEqual(out.stderr, '');
  }
  // Run 1: the parity run on both sides, with Next's options, then ts-r-check again (pretty
  // output). Run 2: ts-r-check only.
  const plain = ['-p', config, ...passed, '--noEmit', '--pretty', 'false'];
  assert.deepStrictEqual(calls(dir, 'tsc'), [[...plain]]);
  const pretty = ['-p', config, ...passed, '--noEmit', '--sound=off', '--pretty', 'true'];
  assert.deepStrictEqual(calls(dir, 'ts-r-check'), [[...plain, '--sound=off'], pretty, pretty]);
});

test("Next.js's type check falls back to TypeScript when the parity check fails", posix, () => {
  const dir = project({ noEmit: true });
  const diag = "app/page.tsx(1,7): error TS2322: Type 'string' is not assignable to type 'number'.";
  standIns(dir, { ours: diag.replace('1,7', '1,8'), theirs: diag });
  const argv = NEXT_CHECK(path.join(dir, 'tsconfig.json'), path.join(dir, 'b.tsbuildinfo'));
  const out = runTsc(dir, argv);
  assert.strictEqual(out.status, 1);
  assert.strictEqual(out.stdout, `${diag}\n`);
  assert.match(out.stderr, /ts-r-check's results differ from TypeScript 7\.0\.2's/);
  const again = runTsc(dir, argv);
  assert.strictEqual(again.status, 1);
  assert.match(again.stderr, /didn't pass the parity check/);
  // The fallback runs TypeScript with Next's arguments unchanged.
  assert.deepStrictEqual(calls(dir, 'tsc').pop(), argv);
});

test('parity is cached per set of passed options', posix, () => {
  const dir = project({ noEmit: true });
  standIns(dir);
  assert.strictEqual(runTsc(dir, []).status, 0);
  assert.strictEqual(runTsc(dir, ['--declarationMap', 'false']).status, 0);
  assert.strictEqual(runTsc(dir, []).status, 0);
  assert.strictEqual(calls(dir, 'tsc').length, 2);
});

test("Next.js's --showConfig goes to TypeScript without a note", posix, () => {
  const dir = project({ noEmit: true });
  standIns(dir);
  const argv = NEXT_SHOW_CONFIG(path.join(dir, 'tsconfig.json'));
  const out = runTsc(dir, argv);
  assert.strictEqual(out.status, 0, out.stderr);
  assert.strictEqual(out.stderr, '');
  assert.deepStrictEqual(calls(dir, 'tsc'), [argv]);
  assert.deepStrictEqual(calls(dir, 'ts-r-check'), []);
});

test("the drop-in's typescript peer range accepts its own prerelease version", async () => {
  const { dropInPeerRange, dropInVersion } = await import('../scripts/build-packages.mjs');
  const alpha = dropInVersion('7.0.2', '0.1.0-alpha.3');
  assert.strictEqual(alpha, '7.0.1000-alpha.3');
  // Installed as "typescript": "npm:@ts-r.dev/typescript@...", the peer resolves to the drop-in.
  assert.strictEqual(dropInPeerRange('^7.0.0', alpha), '^7.0.0 || 7.0.1000-alpha.3');
  assert.strictEqual(dropInPeerRange('^7.0.0', dropInVersion('7.0.2', '1.2.3')), '^7.0.0');
});
