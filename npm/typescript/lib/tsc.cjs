// `tsc` from @ts-r.dev/typescript. Routes each run to ts-r-check or to the user's real TypeScript;
// the rules are in ../README.md ("What `tsc` does"). No dependencies.
'use strict';

const { spawn, spawnSync } = require('child_process');
const crypto = require('crypto');
const fs = require('fs');
const os = require('os');
const path = require('path');

const { OWN, findRealTypeScript, findTsrCheck } = require('./real.cjs');

const PREFIX = '@ts-r.dev/typescript:';
// Flags that only print something: passed to TypeScript without a note.
const INFO_FLAGS = new Set(['-v', '--version', '-h', '--help', '--all', '-?']);

function note(msg) {
  if (!process.env.TS_R_TSC_QUIET) process.stderr.write(`${PREFIX} ${msg}\n`);
}

function verbose(msg) {
  if (process.env.TS_R_TSC_VERBOSE) process.stderr.write(`${PREFIX} ${msg}\n`);
}

/** tsc's command line, reduced to what ts-r-check supports. Option names are case-insensitive. */
function parseArgs(argv) {
  const r = { project: null, noEmit: null, pretty: null, unsupported: [] };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const lower = a.toLowerCase();
    const bool = () => {
      const next = argv[i + 1];
      if (next === 'true' || next === 'false') {
        i++;
        return next === 'true';
      }
      return true;
    };
    if (lower === '-p' || lower === '--project') {
      if (i + 1 < argv.length) r.project = argv[++i];
      else r.unsupported.push(a);
    } else if (lower === '--noemit') {
      r.noEmit = bool();
    } else if (lower === '--pretty') {
      r.pretty = bool();
    } else {
      r.unsupported.push(a);
    }
  }
  return r;
}

/** JSON with comments and trailing commas (tsconfig.json) -> value, or throws. */
function parseJsonc(text) {
  let out = '';
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (c === '"') {
      let j = i + 1;
      while (j < text.length && text[j] !== '"') j += text[j] === '\\' ? 2 : 1;
      out += text.slice(i, j + 1);
      i = j;
    } else if (c === '/' && text[i + 1] === '/') {
      while (i < text.length && text[i] !== '\n') i++;
      out += '\n';
    } else if (c === '/' && text[i + 1] === '*') {
      const end = text.indexOf('*/', i + 2);
      i = end < 0 ? text.length : end + 1;
      out += ' ';
    } else {
      out += c;
    }
  }
  return JSON.parse(out.replace(/^﻿/, '').replace(/,(\s*[}\]])/g, '$1'));
}

function resolveExtends(spec, dir) {
  if (spec.startsWith('.') || path.isAbsolute(spec)) {
    const p = path.resolve(dir, spec);
    if (fs.existsSync(p) && fs.statSync(p).isFile()) return p;
    if (!p.endsWith('.json') && fs.existsSync(`${p}.json`)) return `${p}.json`;
    return null;
  }
  for (const candidate of [spec, `${spec}.json`, `${spec}/tsconfig.json`]) {
    try {
      return require.resolve(candidate, { paths: [dir] });
    } catch (_) {
      // Try the next form.
    }
  }
  return null;
}

/**
 * Reads a tsconfig and everything it extends. Returns {files, options} with options merged in
 * tsc's order (bases first), or null when any file can't be read or the chain is circular: the
 * caller then treats the project as one ts-r-check can't vouch for. Each file is read once
 * (`seen`), so a base extended from many places costs one read, not one per path to it.
 */
function readConfig(file, depth = 0, files = [], seen = new Map()) {
  if (depth > 32) return null;
  if (seen.has(file)) return seen.get(file);
  seen.set(file, null);
  let json;
  try {
    const text = fs.readFileSync(file, 'utf8');
    files.push([file, text]);
    json = parseJsonc(text);
  } catch (_) {
    return null;
  }
  if (!json || typeof json !== 'object') return null;
  let options = {};
  const bases = json.extends === undefined ? [] : Array.isArray(json.extends) ? json.extends : [json.extends];
  for (const spec of bases) {
    if (typeof spec !== 'string') return null;
    const base = resolveExtends(spec, path.dirname(file));
    if (!base) return null;
    const read = readConfig(base, depth + 1, files, seen);
    if (!read) return null;
    options = { ...options, ...read.options };
  }
  const read = { files, options: { ...options, ...(json.compilerOptions || {}) } };
  seen.set(file, read);
  return read;
}

function findConfig(project) {
  if (project !== null) {
    const p = path.resolve(project);
    if (fs.existsSync(p) && fs.statSync(p).isDirectory()) {
      const inDir = path.join(p, 'tsconfig.json');
      return fs.existsSync(inDir) ? inDir : null;
    }
    return fs.existsSync(p) ? p : null;
  }
  for (let dir = process.cwd(); ; dir = path.dirname(dir)) {
    const p = path.join(dir, 'tsconfig.json');
    if (fs.existsSync(p)) return p;
    if (path.dirname(dir) === dir) return null;
  }
}

function nearestLockfile(dir) {
  const names = ['package-lock.json', 'pnpm-lock.yaml', 'yarn.lock', 'bun.lock', 'bun.lockb', 'npm-shrinkwrap.json'];
  for (let d = dir; ; d = path.dirname(d)) {
    for (const n of names) {
      const p = path.join(d, n);
      if (fs.existsSync(p)) return p;
    }
    if (path.dirname(d) === d) return null;
  }
}

// Mirrors ts_r_cache::cache_root().
function cacheRoot() {
  const env = process.env;
  if (env.TS_R_CACHE_DIR) return env.TS_R_CACHE_DIR;
  if (env.XDG_CACHE_HOME && path.isAbsolute(env.XDG_CACHE_HOME)) return path.join(env.XDG_CACHE_HOME, 'ts-r');
  if (process.platform === 'win32' && env.LOCALAPPDATA) return path.join(env.LOCALAPPDATA, 'ts-r');
  return path.join(os.homedir(), '.cache', 'ts-r');
}

/** Parity is cached per tsconfig (and what it extends), lockfile, TypeScript and ts-r-check. */
function parityKey(config, configFiles, real, tsrCheck) {
  const h = crypto.createHash('sha256');
  h.update('parity-v1\0');
  h.update(`${config}\0${real.version}\0${OWN.version}\0`);
  try {
    const st = fs.statSync(tsrCheck);
    h.update(`${st.size}\0${st.mtimeMs}\0`);
  } catch (_) {
    // Hashing the path alone is enough to keep keys distinct.
  }
  for (const [file, text] of configFiles) h.update(`${file}\0${text}\0`);
  const lock = nearestLockfile(path.dirname(config));
  if (lock) {
    h.update(lock);
    h.update(fs.readFileSync(lock));
  }
  return h.digest('hex').slice(0, 32);
}

function capture(cmd, args) {
  return new Promise((resolve) => {
    const child = spawn(cmd, args, { stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
    const out = [];
    const err = [];
    child.stdout.on('data', (d) => out.push(d));
    child.stderr.on('data', (d) => err.push(d));
    child.on('error', (e) => resolve({ code: null, stdout: '', stderr: String(e) }));
    child.on('close', (code, signal) =>
      resolve({ code: signal ? null : code, stdout: Buffer.concat(out).toString('utf8'), stderr: Buffer.concat(err).toString('utf8') }),
    );
  });
}

/** `--pretty false` output -> one string per diagnostic (continuation lines included). */
function diagnostics(text) {
  const blocks = [];
  for (const line of text.replace(/\r\n/g, '\n').split('\n')) {
    if (line === '') continue;
    if (/^\s/.test(line) && blocks.length) blocks[blocks.length - 1] += `\n${line}`;
    else blocks.push(line);
  }
  return blocks;
}

function multisetDiff(a, b) {
  const counts = new Map();
  for (const x of b) counts.set(x, (counts.get(x) || 0) + 1);
  const onlyA = [];
  for (const x of a) {
    const n = counts.get(x) || 0;
    if (n > 0) counts.set(x, n - 1);
    else onlyA.push(x);
  }
  return onlyA;
}

function run(cmd, args) {
  const result = spawnSync(cmd, args, { stdio: 'inherit', windowsHide: true });
  if (result.error) {
    process.stderr.write(`${PREFIX} failed to run ${cmd}: ${result.error.message}\n`);
    process.exit(1);
  }
  if (result.signal) {
    process.kill(process.pid, result.signal);
    return;
  }
  process.exit(result.status === null ? 1 : result.status);
}

function runReal(real, argv, why) {
  if (!real) {
    process.stderr.write(
      `${PREFIX} ${why ? `${why}, which needs` : 'this needs'} your own TypeScript, and it wasn't found.\n` +
        '  Install typescript@7 next to @ts-r.dev/typescript (npm i -D typescript@7), or, when @ts-r.dev/typescript is\n' +
        '  installed under the typescript name, add "typescript-real": "npm:typescript@7" to devDependencies.\n' +
        '  TS_R_TYPESCRIPT=<dir> points at a TypeScript package directory.\n',
    );
    process.exit(1);
  }
  if (why) note(`${why}; running TypeScript ${real.version}'s tsc.`);
  run(process.execPath, [real.tsc, ...argv]);
}

// `-p` exactly as given (or none: both tools then find the nearest tsconfig the same way), so
// diagnostic paths come out relative to the working directory just like tsc's.
function projectArgs(args) {
  return args.project !== null ? ['-p', args.project] : [];
}

// ts-r-check doesn't read `pretty` from tsconfig, so the effective setting is always passed.
function tsrArgs(args, pretty) {
  return [...projectArgs(args), '--noEmit', '--sound=off', '--pretty', String(pretty)];
}

async function main() {
  const argv = process.argv.slice(2);
  const mode = (process.env.TS_R_TSC || 'auto').toLowerCase();
  const real = findRealTypeScript();

  if (mode === 'typescript') return runReal(real, argv, null);
  const args = parseArgs(argv);
  if (args.unsupported.length) {
    if (args.unsupported.every((a) => INFO_FLAGS.has(a.toLowerCase()))) return runReal(real, argv, null);
    const shown = args.unsupported.slice(0, 4).join(' ') + (args.unsupported.length > 4 ? ' ...' : '');
    return runReal(real, argv, `ts-r-check doesn't support ${shown}`);
  }
  const config = findConfig(args.project);
  if (!config) return runReal(real, argv, null);
  const read = readConfig(config);
  if (!read) return runReal(real, argv, `couldn't read ${path.relative(process.cwd(), config) || config} or a config it extends`);
  const noEmit = args.noEmit !== null ? args.noEmit : read.options.noEmit === true;
  if (!noEmit) return runReal(real, argv, 'this project emits files (noEmit is off) and ts-r-check only type-checks');

  const tsrCheck = findTsrCheck();
  if (!tsrCheck) return runReal(real, argv, `the ts-r-check binary for ${process.platform}-${process.arch} isn't installed`);
  const shownConfig = path.relative(process.cwd(), config) || config;
  const pretty = args.pretty !== null ? args.pretty : read.options.pretty === false ? false : Boolean(process.stdout.isTTY);
  if (mode === 'ts-r') return run(tsrCheck, tsrArgs(args, pretty));
  if (!real) {
    note(`TypeScript wasn't found, so ${shownConfig} has no parity check; using ts-r-check.`);
    return run(tsrCheck, tsrArgs(args, pretty));
  }

  const dir = path.join(cacheRoot(), 'typescript', 'parity');
  const key = parityKey(config, read.files, real, tsrCheck);
  const cacheFile = path.join(dir, `${key}.json`);
  let parity = null;
  try {
    parity = JSON.parse(fs.readFileSync(cacheFile, 'utf8'));
  } catch (_) {
    // First run for this project state.
  }

  if (parity) {
    verbose(`${shownConfig}: parity ${parity.result} (cached)`);
    if (parity.result === 'match') return run(tsrCheck, tsrArgs(args, pretty));
    return runReal(real, argv, `${shownConfig} didn't pass the parity check (report: ${path.join(dir, `${key}.txt`)})`);
  }

  // First run: both checkers, side by side, in tsc's plain format; compare; remember.
  const plain = [...projectArgs(args), '--noEmit', '--pretty', 'false'];
  const t0 = Date.now();
  const [ours, theirs] = await Promise.all([
    capture(tsrCheck, [...plain, '--sound=off']),
    capture(process.execPath, [real.tsc, ...plain]),
  ]);
  const a = diagnostics(ours.stdout);
  const b = diagnostics(theirs.stdout);
  const onlyOurs = multisetDiff(a, b);
  const onlyTheirs = multisetDiff(b, a);
  const match = ours.code !== null && ours.code === theirs.code && !onlyOurs.length && !onlyTheirs.length;
  parity = {
    result: match ? 'match' : 'mismatch',
    typescript: real.version,
    tsr: OWN['ts-r'] ? OWN['ts-r'].version : OWN.version,
    exitCodes: { ours: ours.code, theirs: theirs.code },
    ours: a.length,
    theirs: b.length,
    onlyOurs: onlyOurs.length,
    onlyTheirs: onlyTheirs.length,
    sameOrder: match && a.join('\n') === b.join('\n'),
    report: null,
  };
  try {
    fs.mkdirSync(dir, { recursive: true });
    if (!match) {
      parity.report = `${key}.txt`;
      const lines = [
        `ts-r parity report for ${shownConfig}`,
        `TypeScript ${real.version}, ts-r-check ${parity.tsr}`,
        `exit codes: ts-r-check ${ours.code}, tsc ${theirs.code}`,
        '',
        `only ts-r-check (${onlyOurs.length}):`,
        ...onlyOurs.map((d) => `  ${d.replace(/\n/g, '\n  ')}`),
        '',
        `only tsc (${onlyTheirs.length}):`,
        ...onlyTheirs.map((d) => `  ${d.replace(/\n/g, '\n  ')}`),
        '',
      ];
      if (ours.stderr) lines.push('ts-r-check stderr:', ours.stderr);
      fs.writeFileSync(path.join(dir, parity.report), lines.join('\n'));
    }
    fs.writeFileSync(cacheFile, JSON.stringify(parity, null, 2) + '\n');
  } catch (e) {
    note(`couldn't save the parity result in ${dir}: ${e.message}`);
  }
  verbose(`${shownConfig}: parity ${parity.result} in ${Date.now() - t0} ms`);

  if (!match) {
    note(
      `ts-r-check's results differ from TypeScript ${real.version}'s on ${shownConfig} ` +
        `(${onlyOurs.length} only ts-r, ${onlyTheirs.length} only tsc; report: ${path.join(dir, `${key}.txt`)}). ` +
        'This project uses TypeScript until its tsconfig, lockfile or TypeScript version changes.',
    );
  }
  const chosen = match ? ours : theirs;
  if (!pretty) {
    // The parity run already used the user's output format: print what it printed.
    process.stdout.write(chosen.stdout);
    if (chosen.stderr && !match) process.stderr.write(chosen.stderr);
    process.exit(chosen.code === null ? 1 : chosen.code);
  }
  if (match) return run(tsrCheck, tsrArgs(args, pretty));
  return run(process.execPath, [real.tsc, ...argv]);
}

function cli() {
  main().catch((e) => {
    process.stderr.write(`${PREFIX} ${e && e.stack ? e.stack : e}\n`);
    process.exit(1);
  });
}

module.exports = { cli, parseArgs, parseJsonc, diagnostics, multisetDiff, readConfig };

if (require.main === module) cli();
