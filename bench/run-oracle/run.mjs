// ts-r-run oracle: tsx 4.23.15's test suite and Node 24's type-stripping tests, used as data.
//
// Source "tsx": tsx's own spec files (target/upstream/tsx/tests/specs, pinned in
// upstream/UPSTREAM.json) run unchanged under Node's type stripping, with the oracle's stand-ins
// for `manten` and tests/utils/tsx (tsx-suite/). Each spec runs twice: once with the subject set
// to tsx 4.23.15 (bench/node_modules/tsx, the reference) and once with ts-r-run. A test that
// passes with tsx is compared; it passes when it also passes with ts-r-run.
//
// Source "node": every `spawnPromisified(process.execPath, [...])` call in Node's
// test/es-module/test-typescript*.mjs, with its arguments evaluated against a copy of
// test/fixtures/typescript/. Each runs under Node itself, under tsx and under ts-r-run (same
// arguments: Node flags before the file pass through). When Node and tsx agree, the case is
// compared: ts-r-run passes when its exit code, stdout and error kind match theirs.
//
// Usage (from the repo root, always under limit.sh):
//   scripts/limit.sh --mem-mb 4096 --time-s 1800 -- node bench/run-oracle/run.mjs \
//     [--source all|tsx|node] [--spec NAME] [--filter REGEX] [--jobs 4] \
//     [--tsr target/release/ts-r-run] [--out FILE]

import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const args = process.argv.slice(2);
const opt = (name, def) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : def;
};
const source = opt('source', 'all');
const onlySpec = opt('spec');
const filter = opt('filter');
const jobs = Math.max(1, Math.min(4, Number(opt('jobs', '4'))));
const targetDir = process.env.CARGO_TARGET_DIR ?? path.join(root, 'target');
const tsr = path.resolve(root, opt('tsr', path.join(targetDir, 'release/ts-r-run')));
const outFile = path.resolve(root, opt('out', 'target/run-oracle/results.json'));
const work = path.join(root, 'target/run-oracle');
const tsxCli = path.join(root, 'bench/node_modules/tsx/dist/cli.mjs');
const tsxUpstream = path.join(root, 'target/upstream/tsx');
const nodeUpstream = path.join(root, 'target/upstream/node/test');
const suiteDir = path.join(root, 'bench/run-oracle/tsx-suite');
const CASE_TIMEOUT_MS = 20_000;

for (const [p, hint] of [
  [tsr, 'cargo build --release -p ts_r_run -j 4'],
  [tsxCli, '(cd bench && ../scripts/limit.sh -- npm ci)'],
  [path.join(root, 'bench/run-oracle/node_modules/expect'), '(cd bench/run-oracle && ../../scripts/limit.sh -- npm ci)'],
  [path.join(tsxUpstream, 'tests/specs'), 'cargo xtask upstream sync --pin tsx'],
  [path.join(nodeUpstream, 'fixtures/typescript'), 'cargo xtask upstream sync --pin node'],
]) {
  if (!fs.existsSync(p)) {
    console.error(`missing ${p} (${hint})`);
    process.exit(2);
  }
}

fs.mkdirSync(work, { recursive: true });
const cacheDir = path.join(work, 'cache');
fs.rmSync(cacheDir, { recursive: true, force: true });
// Every child gets an isolated ts-r cache; nothing reads ~/.cache/ts-r.
const childEnv = { ...process.env, TS_R_CACHE_DIR: cacheDir, NO_COLOR: '1', FORCE_COLOR: '0' };
delete childEnv.NODE_OPTIONS;

const cases = [];

// ---------------------------------------------------------------------------------------------
// Source "tsx": tsx's spec files

const SPECS = ['smoke', 'tsconfig', 'cli', 'commonjs-mode-contracts', 'resolution-priority', 'version-sensitive', 'loaders'];
/** Spec files the oracle doesn't run, and why (they import tsx's source or test tsx-only features). */
const SKIPPED_SPECS = {
  api: "tsx's API (tsImport, register, require)",
  'api-declarations': "tsx's API type declarations",
  'esm-hook-resolve': "imports tsx's source (src/esm/hook)",
  'node-capabilities': "imports tsx's source (src/platform)",
  'process-interact': "tests the suite's own processInteract helper (never runs tsx)",
  'pty-shell': 'interactive pty shell',
  repl: "tsx's REPL",
  transform: "imports tsx's source (src/utils/transform)",
  'transform-cache': "tsx's transform cache (imports src/utils/transform/cache)",
  watch: "`tsx watch` (ts-r-run's --watch is a different CLI)",
};

function runSpec(spec, subject) {
  const out = path.join(work, `tsx-${spec}-${subject}.json`);
  fs.rmSync(out, { force: true });
  const argv = ['--no-warnings', '--import', pathToFileURL(path.join(suiteDir, 'register.mjs')).href,
    path.join(suiteDir, 'spec-runner.mjs'), '--spec', spec, '--out', out];
  if (filter) argv.push('--filter', filter);
  const r = spawnSync(process.execPath, argv, {
    cwd: tsxUpstream,
    env: { ...childEnv, TS_R_ORACLE_SUBJECT: subject, TS_R_ORACLE_TSR: tsr, TS_R_ORACLE_JOBS: String(jobs) },
    encoding: 'utf8',
    timeout: 900_000,
    maxBuffer: 64 << 20,
  });
  if (!fs.existsSync(out)) {
    return [{ name: spec, status: 'error', error: `runner failed (${r.status ?? r.signal}): ${(r.stderr || r.stdout || '').slice(-2000)}` }];
  }
  return JSON.parse(fs.readFileSync(out, 'utf8'));
}

if (source === 'all' || source === 'tsx') {
  for (const [spec, why] of Object.entries(SKIPPED_SPECS)) {
    if (onlySpec && onlySpec !== spec) continue;
    cases.push({ source: 'tsx', file: `${spec}.ts`, name: '(whole spec)', status: 'skip', reason: `spec not run: ${why}` });
  }
  for (const spec of SPECS) {
    if (onlySpec && onlySpec !== spec) continue;
    const t0 = Date.now();
    const ref = runSpec(spec, 'tsx');
    const ours = runSpec(spec, 'ts-r-run');
    const key = (list) => {
      const seen = new Map();
      const map = new Map();
      for (const r of list) {
        const n = seen.get(r.name) ?? 0;
        seen.set(r.name, n + 1);
        map.set(n ? `${r.name} #${n + 1}` : r.name, r);
      }
      return map;
    };
    const refs = key(ref);
    const oursMap = key(ours);
    for (const [name, r] of refs) {
      const o = oursMap.get(name);
      const c = { source: 'tsx', file: `${spec}.ts`, name };
      if (r.status === 'skip') Object.assign(c, { status: 'skip', reason: r.reason });
      else if (r.status !== 'pass') Object.assign(c, { status: 'skip', reason: 'fails with tsx under the oracle', detail: r.error });
      else if (o?.status === 'pass') c.status = 'pass';
      else Object.assign(c, { status: 'fail', detail: o ? o.error : 'not run with ts-r-run', output: o?.output });
      cases.push(c);
    }
    const n = [...refs.values()];
    console.error(`tsx/${spec}: ${n.length} tests (${((Date.now() - t0) / 1000).toFixed(1)} s)`);
  }
}

// ---------------------------------------------------------------------------------------------
// Source "node": Node's test-typescript*.mjs spawn calls

/** Finds `spawnPromisified(process.execPath, [ ... ] [, { ... }])` calls and their test names. */
function nodeCalls(text) {
  const calls = [];
  const re = /spawnPromisified\(process\.execPath,\s*\[/g;
  let m;
  while ((m = re.exec(text))) {
    const open = m.index + m[0].length - 1;
    const close = matching(text, open);
    if (close < 0) continue;
    const arr = text.slice(open, close + 1);
    let rest = close + 1;
    while (/\s/.test(text[rest])) rest++;
    let opts = null;
    if (text[rest] === ',') {
      rest++;
      while (/\s/.test(text[rest])) rest++;
      if (text[rest] === '{') {
        const end = matching(text, rest);
        if (end > 0) opts = text.slice(rest, end + 1);
      }
    }
    const before = text.slice(0, m.index);
    const t = [...before.matchAll(/\btest\((['"`])((?:\\.|(?!\1).)*)\1/g)].pop();
    calls.push({ name: t ? t[2] : '(top level)', line: before.split('\n').length, arr, opts, before });
  }
  return calls;
}

function matching(text, i) {
  const open = text[i];
  const close = open === '[' ? ']' : '}';
  let depth = 0;
  let quote = null;
  for (let j = i; j < text.length; j++) {
    const ch = text[j];
    if (quote) {
      if (ch === '\\') j++;
      else if (ch === quote) quote = null;
      continue;
    }
    if (ch === '"' || ch === "'" || ch === '`') quote = ch;
    else if (ch === open) depth++;
    else if (ch === close && --depth === 0) return j;
  }
  return -1;
}

const EVAL_FLAGS = new Set(['--eval', '-e', '--print', '-p', '--input-type', '--test', '--check', '-c']);

async function runNodeSource() {
  const copy = path.join(work, 'node-fixtures');
  fs.rmSync(copy, { recursive: true, force: true });
  fs.mkdirSync(copy, { recursive: true });
  fs.cpSync(path.join(nodeUpstream, 'fixtures/typescript'), path.join(copy, 'typescript'), { recursive: true });
  const fixtures = {
    path: (...p) => path.join(copy, ...p),
    fileURL: (...p) => pathToFileURL(path.join(copy, ...p)),
  };
  const testsDir = path.join(nodeUpstream, 'es-module');
  const todo = [];
  for (const file of fs.readdirSync(testsDir).filter((f) => /^test-typescript.*\.mjs$/.test(f)).sort()) {
    const text = fs.readFileSync(path.join(testsDir, file), 'utf8');
    for (const call of nodeCalls(text)) {
      const c = { source: 'node', file, name: `${call.name} (line ${call.line})` };
      if (filter && !new RegExp(filter).test(c.name)) continue;
      let argv;
      let options = {};
      try {
        // The argument list is data: string literals, fixtures.path(...) calls, and consts
        // defined from those earlier in the test.
        let prelude = '';
        for (let tries = 0; ; tries++) {
          try {
            argv = new Function('fixtures', 'process', `${prelude}return ${call.arr}`)(fixtures, { execPath: process.execPath });
            if (call.opts) options = new Function('fixtures', 'process', `${prelude}return (${call.opts})`)(fixtures, {});
            break;
          } catch (e) {
            const id = e instanceof ReferenceError && e.message.match(/^(\w+) is not defined/)?.[1];
            const def = id && [...call.before.matchAll(new RegExp(`const ${id} = (fixtures\\.path\\([^;]*\\));`, 'g'))].pop();
            if (!def || tries > 4) throw e;
            prelude = `const ${id} = ${def[1]};\n${prelude}`;
          }
        }
      } catch {
        Object.assign(c, { status: 'skip', reason: 'arguments are not literal data' });
        cases.push(c);
        continue;
      }
      if (argv.some((a) => EVAL_FLAGS.has(String(a)))) {
        Object.assign(c, { status: 'skip', reason: 'not in ts-r-run: --eval / --print / stdin input (ts-r-run runs a file)' });
        cases.push(c);
        continue;
      }
      if (options.env || options.stdio) {
        Object.assign(c, { status: 'skip', reason: 'custom env or stdio' });
        cases.push(c);
        continue;
      }
      todo.push({ c, argv: argv.map(String), cwd: options.cwd ?? copy });
    }
  }
  const norm = (s) => s.split(copy).join('<fixtures>')
    .split(pathToFileURL(copy).href).join('file://<fixtures>')
    .replace(/[ \t]+$/gm, '').trimEnd();
  const runOne = async ({ c, argv, cwd }) => {
    const [node, tsx, ours] = await Promise.all([
      exec(process.execPath, argv, cwd),
      exec(process.execPath, [tsxCli, ...argv], cwd),
      exec(tsr, argv, cwd),
    ]);
    const o = (r) => outcome(r, norm);
    const [n, t, r] = [o(node), o(tsx), o(ours)];
    c.node = n;
    c.tsx = t;
    c.tsr = r;
    if (!same(n, t)) {
      Object.assign(c, { status: 'skip', reason: 'tsx and Node differ', matchesTsx: same(r, t), matchesNode: same(r, n) });
    } else if (same(r, t)) {
      c.status = 'pass';
    } else {
      Object.assign(c, { status: 'fail', detail: diff(r, t) });
    }
    cases.push(c);
  };
  await pool(todo, runOne, Math.max(1, Math.floor(jobs / 3) || 1));
}

/** Runs one process (own process group, killed on timeout), collecting its output. */
function exec(cmd, argv, cwd) {
  return new Promise((resolve) => {
    const child = spawn(cmd, argv, { cwd, env: childEnv, detached: true, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    let timedOut = false;
    child.stdout.on('data', (d) => { stdout += d; });
    child.stderr.on('data', (d) => { stderr += d; });
    const kill = () => {
      try { process.kill(-child.pid, 'SIGKILL'); } catch {}
    };
    const timer = setTimeout(() => { timedOut = true; kill(); }, CASE_TIMEOUT_MS);
    child.on('error', (e) => { stderr += String(e); });
    child.on('close', (code, signal) => {
      clearTimeout(timer);
      kill();
      resolve({ code, signal, stdout, stderr, timedOut });
    });
  });
}

/** What a run did: exit status, stdout, and the kind of error it reported. */
function outcome(r, norm) {
  return {
    code: r.timedOut ? 'timeout' : (r.signal ?? r.code),
    stdout: norm(r.stdout),
    error: r.code === 0 ? null : errorKind(r.stderr),
    stderr: norm(r.stderr).split('\n').slice(0, 6).join('\n'),
  };
}

/** `ERR_*` code if printed, else the error class; syntax errors from any tool are one kind. */
export function errorKind(stderr) {
  const code = stderr.match(/\b(ERR_[A-Z0-9_]+)\b/);
  if (code) return code[1];
  if (/\berror TS\d+\b|\bTransformError\b|^SyntaxError\b|\bSyntaxError:|: error: /m.test(stderr)) return 'SyntaxError';
  const name = stderr.match(/^(?:Uncaught )?([A-Z]\w*(?:Error|Exception))\b/m);
  return name ? name[1] : 'unknown';
}

function same(a, b) {
  return a.code === b.code && a.stdout === b.stdout && a.error === b.error;
}

function diff(ours, ref) {
  const parts = [];
  if (ours.code !== ref.code) parts.push(`exit ${ours.code}, expected ${ref.code}`);
  if (ours.error !== ref.error) parts.push(`error ${ours.error}, expected ${ref.error}`);
  if (ours.stdout !== ref.stdout) parts.push(`stdout ${JSON.stringify(ours.stdout.slice(0, 300))}, expected ${JSON.stringify(ref.stdout.slice(0, 300))}`);
  if (ours.stderr) parts.push(`stderr: ${ours.stderr}`);
  return parts.join('\n');
}

async function pool(items, fn, n) {
  let next = 0;
  await Promise.all(Array.from({ length: Math.min(n, items.length) }, async () => {
    while (next < items.length) await fn(items[next++]);
  }));
}

if (source === 'all' || source === 'node') await runNodeSource();

// ---------------------------------------------------------------------------------------------
// Summary

const summary = {};
for (const c of cases) {
  const s = (summary[c.source] ??= {});
  const f = (s[c.file] ??= { pass: 0, fail: 0, skip: 0 });
  f[c.status === 'pass' ? 'pass' : c.status === 'skip' ? 'skip' : 'fail']++;
}
const commit = spawnSync('git', ['-C', root, 'rev-parse', '--short', 'HEAD'], { encoding: 'utf8' }).stdout.trim();
fs.mkdirSync(path.dirname(outFile), { recursive: true });
fs.writeFileSync(outFile, JSON.stringify({
  commit,
  date: new Date().toISOString().slice(0, 10),
  platform: `${os.platform()} ${os.arch()}`,
  node: process.version,
  tsx: '4.23.15',
  summary,
  cases,
}, null, 1));

for (const [src, files] of Object.entries(summary)) {
  let p = 0, f = 0, s = 0;
  console.log(`\n${src}:`);
  for (const [file, v] of Object.entries(files)) {
    console.log(`  ${file.padEnd(36)} ${String(v.pass).padStart(4)} pass ${String(v.fail).padStart(4)} fail ${String(v.skip).padStart(4)} skip`);
    p += v.pass; f += v.fail; s += v.skip;
  }
  const cmp = p + f;
  console.log(`  ${src}: ${p} of ${cmp} compared cases pass (${cmp ? ((100 * p) / cmp).toFixed(1) : '0.0'}%); ${s} skipped`);
}
console.log(`\nresults: ${path.relative(root, outFile)}`);
fs.rmSync(cacheDir, { recursive: true, force: true });
