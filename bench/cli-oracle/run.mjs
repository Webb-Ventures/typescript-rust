// Launcher smoke matrix for `ts-r`.
//
// The launcher has no compiler code: `ts-r <tool> args` must behave exactly like the
// standalone `ts-r-<tool> args` it execs. The standalone binaries are the oracle. For every
// case in TOOL_CASES this runs both forms in the same fixture project and compares exit code,
// stdout and stderr (the build directory and the fixture path are normalized). LAUNCHER_CASES
// check the launcher's own commands against what its AGENTS.md promises (`--version`,
// `--help`, `doctor`, `ci`, unknown commands, a missing tool, a `.ts-r-version` pin).
//
// Usage (from the repo root, always under limit.sh):
//   cargo build --release -j 4 -p ts_r_cli -p ts_r_run -p ts_r_dev -p ts_r_build \
//     -p ts_r_check -p ts_r_lint_cli -p ts_r_lsp
//   scripts/limit.sh --mem-mb 2048 --time-s 600 -- node bench/cli-oracle/run.mjs \
//     [--bin target/release] [--out target/cli-oracle/results.json] [--keep]
//
// No servers are started (dev and lsp only get flag cases); every case has a 60 s timeout
// and runs one at a time.

import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const args = process.argv.slice(2);
const opt = (name, def) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : def;
};
const bin = path.resolve(root, opt('bin', process.env.CARGO_TARGET_DIR ? path.join(process.env.CARGO_TARGET_DIR, 'release') : 'target/release'));
const outFile = path.resolve(root, opt('out', 'target/cli-oracle/results.json'));
const keep = args.includes('--keep');
const TOOLS = ['run', 'dev', 'build', 'lint', 'check', 'lsp'];

for (const t of ['ts-r', ...TOOLS.map((t) => `ts-r-${t}`)]) {
  if (!fs.existsSync(path.join(bin, t))) {
    console.error(`missing ${path.join(bin, t)} (build the release binaries, see the header)`);
    process.exit(2);
  }
}

const scratch = fs.mkdtempSync(path.join(fs.realpathSync(os.tmpdir()), 'ts-r-cli-oracle-'));
const project = path.join(scratch, 'project');
const cache = path.join(scratch, 'cache');
const files = {
  'package.json': '{ "name": "cli-oracle", "type": "module", "private": true }\n',
  'tsconfig.json': '{ "compilerOptions": { "strict": true, "module": "nodenext", "target": "es2022", "noEmit": true }, "include": ["src/ok.ts"] }\n',
  'bad/tsconfig.json': '{ "compilerOptions": { "strict": true, "noEmit": true }, "include": ["bad.ts"] }\n',
  'bad/bad.ts': 'const n: number = "one";\nexport {};\n',
  'eslint.config.js': 'export default [{ files: ["**/*.ts"], rules: { "no-debugger": "error", "no-var": "error" } }];\n',
  'src/ok.ts': 'const greet = (name: string): string => `hello ${name}`;\nconsole.log(greet("ts-r"));\n',
  'src/args.ts': 'console.log(JSON.stringify(process.argv.slice(2)));\n',
  'src/exit3.ts': 'console.error("leaving with 3");\nprocess.exit(3);\n',
  'src/throws.ts': 'throw new Error("boom");\n',
  'src/syntax.ts': 'const x = ;\n',
  'src/lint-bad.ts': 'var a = 1;\ndebugger;\nexport { a };\n',
  'src/env.ts': 'console.log(process.env.CLI_ORACLE ?? "unset");\n',
};
for (const [name, text] of Object.entries(files)) {
  fs.mkdirSync(path.dirname(path.join(project, name)), { recursive: true });
  fs.writeFileSync(path.join(project, name), text);
}

const env = {
  PATH: `${path.dirname(process.execPath)}:/usr/bin:/bin`,
  HOME: scratch,
  TS_R_CACHE_DIR: cache,
  TS_R_LIB_DIR: process.env.TS_R_LIB_DIR ?? '',
  TS_R_LSP_PARITY: 'off',
  NO_COLOR: '1',
  CLI_ORACLE: 'passed',
};
if (!env.TS_R_LIB_DIR) delete env.TS_R_LIB_DIR;

function exec(file, argv, { cwd = project, extraEnv = {} } = {}) {
  const r = spawnSync(file, argv, { cwd, env: { ...env, ...extraEnv }, encoding: 'utf8', timeout: 60_000 });
  const norm = (s) => (s ?? '').split(bin).join('<bin>').split(scratch).join('<tmp>').replace(/\b\d+(\.\d+)? ?(ms|s)\b/g, '<time>');
  return { code: r.status ?? (r.error?.code === 'ETIMEDOUT' ? 'timeout' : r.signal), stdout: norm(r.stdout), stderr: norm(r.stderr) };
}

// [tool, args]: each runs as `ts-r <tool> ...args` and `ts-r-<tool> ...args`.
const TOOL_CASES = [
  ...TOOLS.flatMap((t) => [[t, ['--help']], [t, ['--version']], [t, ['--definitely-not-a-flag']]]),
  ['run', ['src/ok.ts']],
  ['run', ['src/args.ts', 'a', '--b', 'c d', '--', '-e']],
  ['run', ['src/exit3.ts']],
  ['run', ['src/throws.ts']],
  ['run', ['src/syntax.ts']],
  ['run', ['src/missing.ts']],
  ['run', ['src/env.ts']],
  ['run', ['--no-source-maps', 'src/ok.ts']],
  ['check', []],
  ['check', ['-p', 'bad']],
  ['check', ['-p', 'missing']],
  ['check', ['--pretty', 'false', '-p', 'bad/tsconfig.json']],
  ['build', ['src/ok.ts']],
  ['build', ['src/ok.ts', '--bundle', '--outfile=out/ok.js']],
  ['build', ['src/syntax.ts']],
  ['build', ['src/missing.ts']],
  ['lint', ['src/ok.ts']],
  ['lint', ['src/lint-bad.ts']],
  ['lint', ['src/missing.ts']],
];

const results = [];
for (const [tool, argv] of TOOL_CASES) {
  const name = `ts-r ${tool} ${argv.join(' ')}`.trim();
  // Runs that write files (build --outfile) must start from the same state.
  fs.rmSync(path.join(project, 'out'), { recursive: true, force: true });
  const want = exec(path.join(bin, `ts-r-${tool}`), argv);
  fs.rmSync(path.join(project, 'out'), { recursive: true, force: true });
  const got = exec(path.join(bin, 'ts-r'), [tool, ...argv]);
  const diff = ['code', 'stdout', 'stderr'].filter((k) => String(want[k]) !== String(got[k]));
  results.push({ group: 'passthrough', name, pass: diff.length === 0, diff, want, got });
}

// The launcher's own commands, against the rules in crates/ts_r_cli/AGENTS.md.
const version = JSON.parse(spawnSync('cargo', ['metadata', '--no-deps', '--format-version', '1'], { cwd: root, encoding: 'utf8', maxBuffer: 64 << 20 }).stdout)
  .packages.find((p) => p.name === 'ts_r_cli').version;
const lonely = path.join(scratch, 'lonely');
fs.mkdirSync(lonely);
fs.copyFileSync(path.join(bin, 'ts-r'), path.join(lonely, 'ts-r'));
fs.chmodSync(path.join(lonely, 'ts-r'), 0o755);
const pinned = path.join(scratch, 'pinned');
fs.mkdirSync(pinned);
fs.writeFileSync(path.join(pinned, '.ts-r-version'), '# pinned\nv0.0.1\n');

const checkAlone = exec(path.join(bin, 'ts-r-check'), []).code;
const lintAlone = exec(path.join(bin, 'ts-r-lint'), []).code;
const buildAlone = exec(path.join(bin, 'ts-r-build'), []).code;
const LAUNCHER_CASES = [
  ['--version prints the launcher version', ['--version'], {}, (r) => r.code === 0 && r.stdout.includes(version)],
  ['-V is --version', ['-V'], {}, (r) => r.code === 0 && r.stdout.includes(version)],
  ['--help lists every tool', ['--help'], {}, (r) => r.code === 0 && TOOLS.every((t) => r.stdout.includes(t))],
  ['no arguments prints usage', [], {}, (r) => (r.code === 0 || r.code === 2) && /usage/i.test(r.stdout + r.stderr)],
  ['unknown command exits 2', ['frobnicate'], {}, (r) => r.code === 2],
  ['unknown flag exits 2', ['--bogus'], {}, (r) => r.code === 2],
  ['doctor finds every tool', ['doctor'], {}, (r) => r.code === 0 && TOOLS.every((t) => r.stdout.includes(`ts-r-${t}`)) && !/not (found|installed)/i.test(r.stdout)],
  [`ci exits as check/lint/build do alone (${checkAlone}/${lintAlone}/${buildAlone})`, ['ci'], {},
    (r) => r.code === ([checkAlone, lintAlone, buildAlone].every((c) => c === 0) ? 0 : 1)],
  ['missing tool exits 127 and names its milestone', ['run', 'x.ts'], { file: path.join(lonely, 'ts-r'), cwd: lonely },
    (r) => r.code === 127 && /M1/.test(r.stderr)],
  ['ci with a missing tool runs nothing and fails', ['ci'], { file: path.join(lonely, 'ts-r'), cwd: lonely }, (r) => r.code !== 0],
  ['a .ts-r-version pin of another version is not satisfied by siblings', ['run', 'x.ts'], { cwd: pinned }, (r) => r.code === 127 && r.stderr.includes('0.0.1')],
  ['TS_R_VERSION beats .ts-r-version', ['run', 'x.ts'], { cwd: pinned, extraEnv: { TS_R_VERSION: '0.0.2' } }, (r) => r.code === 127 && r.stderr.includes('0.0.2')],
  ['a malformed pin exits 2', ['run', 'x.ts'], { cwd: pinned, extraEnv: { TS_R_VERSION: 'not a version' } }, (r) => r.code === 2],
];
for (const [name, argv, o, ok] of LAUNCHER_CASES) {
  const r = exec(o.file ?? path.join(bin, 'ts-r'), argv, { cwd: o.cwd ?? project, extraEnv: o.extraEnv });
  results.push({ group: 'launcher', name, pass: !!ok(r), got: r });
}

const summary = {};
for (const r of results) {
  const s = (summary[r.group] ??= { pass: 0, fail: 0 });
  s[r.pass ? 'pass' : 'fail']++;
}
for (const r of results.filter((r) => !r.pass)) {
  console.log(`FAIL [${r.group}] ${r.name}${r.diff ? ` (differs: ${r.diff.join(', ')})` : ''}`);
  if (r.diff) for (const k of r.diff) console.log(`  ${k}: standalone ${JSON.stringify(r.want[k]).slice(0, 300)}\n  ${' '.repeat(k.length)}  launcher   ${JSON.stringify(r.got[k]).slice(0, 300)}`);
  else console.log(`  got ${JSON.stringify(r.got).slice(0, 400)}`);
}
for (const [g, s] of Object.entries(summary)) console.log(`${g}: ${s.pass} of ${s.pass + s.fail} pass`);
fs.mkdirSync(path.dirname(outFile), { recursive: true });
fs.writeFileSync(outFile, JSON.stringify({ version, summary, results }, null, 2) + '\n');
if (!keep) fs.rmSync(scratch, { recursive: true, force: true });
process.exit(Object.values(summary).every((s) => s.fail === 0) ? 0 : 1);
