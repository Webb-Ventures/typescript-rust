// ts-r-run preload (strategy "prebuild"): serves modules ts-r-run compiled before starting
// Node, from one manifest file. Synchronous in-thread hooks (module.registerHooks) cover both
// `import` and `require`.
import { findPackageJSON, registerHooks, setSourceMapsSupport } from 'node:module';
import { readFileSync, statSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';
import './relay.mjs';

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

// ts-r-run decides the format of every file it compiles; Node's hint to add `"type"` to a
// typeless package.json (it reparsed a `.js` file as ESM) is noise here, as it is under tsx.
const emitWarning = process.emitWarning;
process.emitWarning = function (warning, ...rest) {
  const code = typeof rest[0] === 'object' && rest[0] !== null ? rest[0].code : rest[1];
  if (code === 'MODULE_TYPELESS_PACKAGE_JSON') return;
  return emitWarning.call(this, warning, ...rest);
};

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
const ADD = ['.ts', '.tsx', '.js', '.jsx', '.mts', '.cts', '.json'];
const INDEX = ['index.ts', 'index.tsx', 'index.js', 'index.json'];
// Inside node_modules, JavaScript wins over TypeScript sources shipped next to it.
const PACKAGE_ORDER = {
  add: ['.js', '.json', '.jsx', '.ts', '.tsx', '.mts', '.cts'],
  index: ['index.js', 'index.json', 'index.ts', 'index.tsx'],
};
const LOCAL_ORDER = { add: ADD, index: INDEX };

const isFile = (p) => {
  try {
    return statSync(p).isFile();
  } catch {
    return false;
  }
};

const isDir = (p) => {
  try {
    return statSync(p).isDirectory();
  } catch {
    return false;
  }
};

const readPackage = (dir) => {
  try {
    return JSON.parse(readFileSync(`${dir}/package.json`, 'utf8'));
  } catch {
    return null;
  }
};

// Files to try for a path Node couldn't load: `.js` written for a `.ts` file, then (unless
// `swapOnly`) no extension, or a directory: its package.json `main`, then its index.
function candidates(p, swapOnly, order = LOCAL_ORDER, depth = 0) {
  const slash = p.lastIndexOf('/');
  const dot = p.lastIndexOf('.');
  const ext = dot > slash ? p.slice(dot) : '';
  const out = SWAP[ext] ? SWAP[ext].map((e) => p.slice(0, dot) + e) : [];
  if (swapOnly) return out;
  if (ext === '') out.push(...order.add.map((e) => p + e));
  if (isDir(p)) {
    const main = depth === 0 ? readPackage(p)?.main : undefined;
    if (typeof main === 'string' && main !== '') {
      const m = `${p}/${main}`.replace(/\/\.\//g, '/').replace(/\/+$/, '');
      if (isFile(m)) out.push(m);
      out.push(...candidates(m, false, order, depth + 1));
    }
    for (const i of order.index) out.push(`${p}/${i}`);
  }
  return out;
}

// The URL Node failed to load, from the failed specifier or Node's error.
function failedTarget(specifier, parent, error) {
  if (specifier.startsWith('./') || specifier.startsWith('../') || specifier.startsWith('/')) {
    return parent !== undefined && parent.startsWith('file:') ? new URL(specifier, parent).href : null;
  }
  if (typeof error?.url === 'string' && error.url.startsWith('file:')) return error.url;
  // A package `main` that doesn't exist: "Cannot find package '<dir>/<main>' imported from".
  const m = /^Cannot find package '([^']+)' imported from /.exec(String(error?.message ?? ''));
  if (m !== null && (m[1].startsWith('/') || /^[A-Za-z]:[\\/]/.test(m[1]))) return pathToFileURL(m[1]).href;
  return null;
}

// The package directory `p` is in, when it is inside node_modules.
function packageRoot(p) {
  const at = p.lastIndexOf('/node_modules/');
  if (at === -1) return null;
  const rest = p.slice(at + 14).split('/');
  const n = rest[0].startsWith('@') ? 2 : 1;
  return rest.length >= n ? p.slice(0, at + 14) + rest.slice(0, n).join('/') : null;
}

// Specifiers Node can't resolve, the way tsx resolves them: TypeScript files behind `.js`,
// extensionless paths and directories, in relative imports and inside packages. A target
// that came from a package's `exports` or `imports` only gets the `.js` → `.ts` swap.
function fallback(specifier, context, error) {
  const url = failedTarget(specifier, context.parentURL, error);
  if (url === null) return null;
  const cut = url.search(/[?#]/);
  const suffix = cut === -1 ? '' : url.slice(cut);
  const p = toPath(cut === -1 ? url : url.slice(0, cut)).replace(/(.)\/+$/, '$1');
  const relative = specifier.startsWith('.') || specifier.startsWith('/');
  let swapOnly = specifier.startsWith('#');
  const root = relative ? null : packageRoot(p);
  if (!swapOnly && root !== null) swapOnly = readPackage(root)?.exports !== undefined;
  const order = p.includes('/node_modules/') ? PACKAGE_ORDER : LOCAL_ORDER;
  const tries = candidates(p, swapOnly, order);
  // A missing package main falls back to the package's index, as Node's legacy resolution does.
  if (root !== null && error?.message?.startsWith('Cannot find package ')) {
    for (const i of order.index) tries.push(`${root}/${i}`);
  }
  for (const c of tries) {
    if (!DECL.test(c) && isFile(c)) return { url: pathToFileURL(c).href + suffix, shortCircuit: true };
  }
  return null;
}

// A bare specifier from a file outside the graph (plain JavaScript, which ts-r-run doesn't
// compile) still goes through tsconfig `paths` and `baseUrl`, as under tsx; files inside
// node_modules don't. Asked of a short-lived ts-r-run process, once per parent and specifier.
const BARE = /^(?![./#]|[a-z][a-z0-9+.-]*:)/i;
const viaPathsDone = new Map();
function viaPaths(specifier, parent) {
  if (!BARE.test(specifier) || parent === undefined || !parent.startsWith('file:')) return null;
  const from = toPath(parent);
  if (resolved[from] !== undefined || from.includes('/node_modules/')) return null;
  const cut = specifier.search(/[?#]/);
  const suffix = cut === -1 ? '' : specifier.slice(cut);
  const bare = cut === -1 ? specifier : specifier.slice(0, cut);
  const key = `${from}\0${bare}`;
  let target = viaPathsDone.get(key);
  if (target === undefined) {
    const r = spawnSync(bin, ['--resolve', from, bare], {
      encoding: 'utf8',
      env: { ...env, TS_R_RUN_TSCONFIG: tsconfig ?? '', TS_R_RUN_ENTRY: entry ?? '' },
      stdio: ['ignore', 'pipe', 'ignore'],
    });
    target = r.status === 0 ? r.stdout.trim() : '';
    viaPathsDone.set(key, target);
  }
  return target === '' ? null : { url: pathToFileURL(target).href + suffix, shortCircuit: true };
}

// ES module syntax at the start of a line, and the CommonJS globals (a cheap filter: the
// compiler decides).
const ESM_SYNTAX = /^[ \t]*(?:import[ \t]*[\w{*'"]|export[ \t]*(?:[{*]|default\b|const\b|let\b|var\b|function\b|class\b|async\b))/m;
const CJS_GLOBALS = /\brequire\(|\bmodule\.exports\b|\bexports\.|\b__dirname\b|\b__filename\b/;
const packageTypes = new Map();

function packageType(url) {
  const pj = findPackageJSON(url);
  if (pj === undefined) return undefined;
  let type = packageTypes.get(pj);
  if (type === undefined) {
    try {
      type = JSON.parse(readFileSync(pj, 'utf8')).type ?? '';
    } catch {
      type = '';
    }
    packageTypes.set(pj, type);
  }
  return type;
}

// A `.js` file that Node would run as CommonJS but that has ES module syntax (in a `"type":
// "commonjs"` package, or mixed with `require` in a package without a type) is compiled the
// way ts-r-run compiles TypeScript, as tsx does: ES syntax in CommonJS scope runs as CommonJS.
function lenientJs(url, bare, context, nextLoad) {
  const r = nextLoad(url, context);
  if (r.source == null || (r.format !== 'commonjs' && r.format !== 'module' && r.format != null)) return r;
  const text = typeof r.source === 'string' ? r.source : Buffer.from(r.source).toString('utf8');
  if (!ESM_SYNTAX.test(text)) return r;
  const type = packageType(bare);
  if (type === 'module' || (type !== 'commonjs' && !CJS_GLOBALS.test(text))) return r;
  const [format, source] = compileNow(toPath(bare));
  return { format: format === 1 ? 'commonjs' : 'module', source, shortCircuit: true };
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
      if (e?.code !== 'ERR_MODULE_NOT_FOUND' && e?.code !== 'MODULE_NOT_FOUND' && e?.code !== 'ERR_UNSUPPORTED_DIR_IMPORT') throw e;
      const alt = fallback(specifier, context, e) ?? viaPaths(specifier, parent);
      if (alt !== null) return alt;
      // A local directory with nothing to load is a missing module, as tsx reports it (a
      // package's directory keeps Node's error).
      if (e.code === 'ERR_UNSUPPORTED_DIR_IMPORT' && (specifier.startsWith('.') || specifier.startsWith('/'))) {
        const target = e.url ? toPath(e.url) : specifier;
        const from = parent?.startsWith('file:') ? toPath(parent) : parent;
        throw Object.assign(new Error(`Cannot find module '${target}' imported from ${from}`), {
          code: 'ERR_MODULE_NOT_FOUND',
          url: e.url,
        });
      }
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
      if (bare.endsWith('.js')) return lenientJs(url, bare, context, nextLoad);
      // JSON imports need no `with { type: 'json' }` attribute, as under tsx.
      if (bare.endsWith('.json') && context.importAttributes?.type === undefined) {
        return nextLoad(url, { ...context, importAttributes: { ...context.importAttributes, type: 'json' } });
      }
    }
    return nextLoad(url, context);
  },
});
