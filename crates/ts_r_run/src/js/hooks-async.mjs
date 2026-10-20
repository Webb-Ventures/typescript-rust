// Hooks for strategy "hook" (see preload-async.mjs). Protocol with `ts-r-run --serve`, one
// request per line, answered in order:
//   C\t<path>\n                    -> <ok|err>\t<esm|cjs|->\t<byte length>\n<code or error>
//   R\t<0|1 require>\t<parent>\t<spec>\n -> ok\t-\t<byte length>\n<resolved path or empty>
import { spawn } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';

let child;
const waiting = [];
let chunks = [];
let have = 0;

function pump() {
  for (;;) {
    if (waiting.length === 0 || have === 0) return;
    const buf = chunks.length === 1 ? chunks[0] : Buffer.concat(chunks, have);
    chunks = [buf];
    const nl = buf.indexOf(10);
    if (nl === -1) return;
    const [status, format, len] = buf.toString('utf8', 0, nl).split('\t');
    const n = Number(len);
    if (buf.length < nl + 1 + n) return;
    const body = buf.toString('utf8', nl + 1, nl + 1 + n);
    const rest = buf.subarray(nl + 1 + n);
    chunks = rest.length ? [rest] : [];
    have = rest.length;
    waiting.shift()({ ok: status === 'ok', format, body });
  }
}

export function initialize(data) {
  child = spawn(data.bin, data.sourceMaps ? ['--serve'] : ['--serve', '--no-source-maps'], {
    stdio: ['pipe', 'pipe', 'inherit'],
    env: { ...process.env, TS_R_RUN_TSCONFIG: data.tsconfig, TS_R_RUN_ENTRY: data.entry },
  });
  child.stdout.on('data', (d) => {
    chunks.push(d);
    have += d.length;
    pump();
  });
}

function ask(line) {
  return new Promise((resolve) => {
    waiting.push(resolve);
    child.stdin.write(line);
  });
}

const COMPILED = /\.(?:[cm]?ts|tsx|jsx)$/i;
const DECL = /\.d\.[cm]?ts$/i;

export async function resolve(specifier, context, nextResolve) {
  const parent = context.parentURL;
  if (parent !== undefined && parent.startsWith('file:') && COMPILED.test(parent) && !specifier.startsWith('node:')) {
    const req = context.conditions?.includes('require') ? 1 : 0;
    const r = await ask(`R\t${req}\t${fileURLToPath(parent)}\t${specifier}\n`);
    if (r.ok && r.body !== '') return { url: pathToFileURL(r.body).href, shortCircuit: true };
  }
  return nextResolve(specifier, context);
}

export async function load(url, context, nextLoad) {
  if (url.startsWith('file:') && COMPILED.test(url) && !DECL.test(url)) {
    const r = await ask(`C\t${fileURLToPath(url)}\n`);
    if (!r.ok) throw new SyntaxError(r.body);
    return { format: r.format === 'cjs' ? 'commonjs' : 'module', source: r.body, shortCircuit: true };
  }
  return nextLoad(url, context);
}
