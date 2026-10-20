// ts-r-run preload (strategy "prebuild"): serves modules ts-r-run compiled before starting
// Node, from one manifest file. Synchronous in-thread hooks (module.registerHooks) cover both
// `import` and `require`.
import { registerHooks, setSourceMapsSupport } from 'node:module';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';

const env = process.env;
const manifestPath = env.TS_R_RUN_MANIFEST;
const bin = env.TS_R_RUN_BIN;
const tsconfig = env.TS_R_RUN_TSCONFIG;
const entry = env.TS_R_RUN_ENTRY;
const watchParent = Number(env.TS_R_RUN_WATCH_PARENT);
const sourceMaps = env.TS_R_RUN_SOURCE_MAPS !== '0';
for (const k of ['TS_R_RUN_MANIFEST', 'TS_R_RUN_BIN', 'TS_R_RUN_TSCONFIG', 'TS_R_RUN_ENTRY', 'TS_R_RUN_SOURCE_MAPS', 'TS_R_RUN_WATCH_PARENT']) delete env[k];

// Under `--watch`, exit when ts-r-run itself goes away (it can't forward every signal).
if (watchParent > 0) {
  setInterval(() => {
    if (process.ppid !== watchParent) process.exit(130);
  }, 500).unref();
}

const win = process.platform === 'win32';
const toPath = win ? (u) => fileURLToPath(u).replaceAll('\\', '/') : fileURLToPath;

let files = Object.create(null);
let resolved = Object.create(null);
let blob = null;
let base = 0;
if (manifestPath) {
  blob = readFileSync(manifestPath);
  const nl = blob.indexOf(10);
  const index = JSON.parse(blob.toString('utf8', 0, nl));
  files = index.f;
  resolved = index.r;
  base = nl + 1;
}
if (sourceMaps) setSourceMapsSupport(true);

const COMPILED = /\.(?:[cm]?ts|tsx|jsx)$/i;
const DECL = /\.d\.[cm]?ts$|\.d\.[^/]*\.ts$/i;

// A file the graph walk didn't reach (computed specifier, require from plain JS): compile
// it now with a short-lived ts-r-run process.
function compileNow(path) {
  const r = spawnSync(bin, sourceMaps ? ['--compile', path] : ['--no-source-maps', '--compile', path], {
    encoding: 'utf8',
    maxBuffer: 1 << 30,
    env: { ...env, TS_R_RUN_TSCONFIG: tsconfig ?? '', TS_R_RUN_ENTRY: entry ?? '' },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  if (r.status !== 0) {
    const err = new SyntaxError(`ts-r-run could not compile ${path}\n${r.stderr || r.error || ''}`);
    throw err;
  }
  const nl = r.stdout.indexOf('\n');
  return [r.stdout.slice(0, nl) === 'cjs' ? 1 : 0, r.stdout.slice(nl + 1)];
}

const SWAP = { '.js': ['.ts', '.tsx'], '.jsx': ['.tsx'], '.mjs': ['.mts'], '.cjs': ['.cts'] };
const ADD = ['.ts', '.tsx', '.mts', '.cts', '/index.ts', '/index.tsx'];

// Relative specifiers Node can't find: `.js` written for a `.ts` file, or no extension.
function fallback(specifier, context, nextResolve) {
  if (!(specifier.startsWith('./') || specifier.startsWith('../') || specifier.startsWith('/'))) return null;
  const dot = specifier.lastIndexOf('.');
  const ext = dot > specifier.lastIndexOf('/') ? specifier.slice(dot) : '';
  const candidates = SWAP[ext]
    ? SWAP[ext].map((e) => specifier.slice(0, dot) + e)
    : ext === '' ? ADD.map((e) => specifier + e) : [];
  for (const c of candidates) {
    try {
      return nextResolve(c, context);
    } catch {}
  }
  return null;
}

registerHooks({
  resolve(specifier, context, nextResolve) {
    const parent = context.parentURL;
    if (parent !== undefined && parent.startsWith('file:')) {
      const map = resolved[toPath(parent)];
      if (map !== undefined) {
        const target = map[specifier];
        if (target !== undefined) return { url: pathToFileURL(target).href, shortCircuit: true };
      }
    }
    try {
      return nextResolve(specifier, context);
    } catch (e) {
      const alt = fallback(specifier, context, nextResolve);
      if (alt !== null) return alt;
      throw e;
    }
  },
  load(url, context, nextLoad) {
    if (url.startsWith('file:')) {
      let end = url.length;
      const q = url.indexOf('?');
      if (q !== -1) end = q;
      const h = url.indexOf('#');
      if (h !== -1 && h < end) end = h;
      const bare = end === url.length ? url : url.slice(0, end);
      if (COMPILED.test(bare) && !DECL.test(bare)) {
        const path = toPath(bare);
        const f = files[path];
        let format;
        let source;
        if (f !== undefined) {
          format = f[0];
          source = blob.toString('utf8', base + f[1], base + f[2]);
        } else {
          [format, source] = compileNow(path);
        }
        return { format: format === 1 ? 'commonjs' : 'module', source, shortCircuit: true };
      }
    }
    return nextLoad(url, context);
  },
});
