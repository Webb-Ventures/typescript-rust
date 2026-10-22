#!/usr/bin/env node
// Runs the examples on error-code pages and records what ts-r reports.
//
//   node scripts/verify-examples.mjs [--bin target/release] [--data <file>] [CODE...]
//
// --bin and --data are relative to the repo root. --data writes to another file than
// landing/data/error-examples.json (for parallel runs; merge
// the files afterwards).
//
// For each content/errors/<CODE>.md with ```<lang> example blocks it writes the files and a
// tsconfig.json to a temporary directory, runs `ts-r-check -p tsconfig.json --pretty false`
// under scripts/limit.sh, and stores the output in data/error-examples.json with
// `triggered: true` when the output reports CODE. TS8xxx codes (TypeScript syntax in .js
// files) are reported by ts-r's parser but not yet by ts-r-check; when ts-r-check doesn't report
// one, the parser's `dump` example (`--ts-in-js report`) is run instead and recorded as such.
//
// Needs release builds: `cargo build --release -p ts_r_check` (and
// `cargo build --release -p ts_r_parser --example dump` for TS8xxx), and the TypeScript lib
// files (`cargo xtask upstream sync --minimal`, or TS_R_LIB_DIR).

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { frontMatter } from '../lib/content/markdown.mjs';
import { exampleBlocks, exampleHash, effectiveTsconfig } from '../lib/content/examples.mjs';

const LANDING = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const ROOT = path.dirname(LANDING);
const ERRORS = path.join(LANDING, 'content/errors');
const args = process.argv.slice(2);
const opt = (name, def) => { const i = args.indexOf(name); return i >= 0 ? args.splice(i, 2)[1] : def; };
const BIN = path.resolve(ROOT, opt('--bin', 'target/release'));
const DATA = path.resolve(ROOT, opt('--data', 'landing/data/error-examples.json'));
const only = new Set(args.map(a => a.toUpperCase()));
const LIMIT = path.join(ROOT, 'scripts/limit.sh');

const check = path.join(BIN, 'ts-r-check');
const dump = path.join(BIN, 'examples/dump');
if (!fs.existsSync(check)) { console.error(`missing ${check}: cargo build --release -p ts_r_check`); process.exit(2); }

const records = fs.existsSync(DATA) ? JSON.parse(fs.readFileSync(DATA, 'utf8')) : {};
// Real path: ts-r-check prints resolved paths (/private/var/... on macOS), and we strip the prefix.
const tmpRoot = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'ts-r-examples-')));
// The commit the binaries were built from: the working tree's HEAD when this runs.
const COMMIT = execFileSync('git', ['rev-parse', '--short', 'HEAD'], { cwd: ROOT, encoding: 'utf8' }).trim();
let ran = 0, triggered = 0;
const misses = [];

// stdout and stderr of `cmd`, without scripts/limit.sh's own notes.
function run(cmd, argv, cwd) {
  let text;
  try {
    text = execFileSync(LIMIT, ['--mem-mb', '1024', '--time-s', '30', '--', cmd, ...argv], { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
  } catch (e) {
    text = (e.stdout ?? '') + (e.stderr ?? '');
  }
  return text.split('\n').filter(l => !l.startsWith('limit.sh:')).join('\n');
}

for (const name of fs.readdirSync(ERRORS).sort()) {
  if (!name.endsWith('.md')) continue;
  const code = name.slice(0, -3);
  if (only.size && !only.has(code)) continue;
  const { body } = frontMatter(fs.readFileSync(path.join(ERRORS, name), 'utf8'));
  const ex = exampleBlocks(body);
  if (!ex.files.length) { delete records[code]; continue; }
  const dir = path.join(tmpRoot, code);
  fs.mkdirSync(dir, { recursive: true });
  for (const f of ex.files) {
    fs.mkdirSync(path.dirname(path.join(dir, f.file)), { recursive: true });
    fs.writeFileSync(path.join(dir, f.file), f.text + '\n');
  }
  fs.writeFileSync(path.join(dir, 'tsconfig.json'), effectiveTsconfig(ex));
  let tool = 'ts-r-check';
  let command = 'ts-r-check -p tsconfig.json --pretty false';
  let output = run(check, ['-p', 'tsconfig.json', '--pretty', 'false'], dir);
  const re = new RegExp(`error ${code}:`);
  if (!re.test(output) && /^TS8\d{3}$/.test(code) && fs.existsSync(dump)) {
    const js = ex.files.filter(f => /\.jsx?$/.test(f.file));
    const out = js.map(f => run(dump, [f.file, '--ts-in-js', 'report'], dir).split('\n').filter(l => /: error TS\d+:/.test(l)).join('\n')).join('\n');
    if (re.test(out)) {
      tool = 'ts_r_parser';
      command = `cargo run -p ts_r_parser --example dump -- ${js.map(f => f.file).join(' ')} --ts-in-js report`;
      output = out + '\n';
    }
  }
  output = output.split(dir + path.sep).join('').replace(/\s+$/, '') + '\n';
  const ok = re.test(output);
  records[code] = { hash: exampleHash(code, ex), triggered: ok, tool, command, commit: COMMIT, output };
  ran++;
  if (ok) triggered++; else misses.push(code);
}

fs.rmSync(tmpRoot, { recursive: true, force: true });
const sorted = Object.fromEntries(Object.keys(records).sort().map(k => [k, records[k]]));
fs.writeFileSync(DATA, JSON.stringify(sorted, null, 2) + '\n');
console.log(`examples: ${ran} run, ${triggered} report their code`);
if (misses.length) console.log(`not reported (the page will omit the example): ${misses.join(' ')}`);
