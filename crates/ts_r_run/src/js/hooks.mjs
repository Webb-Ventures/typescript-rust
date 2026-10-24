// ts-r-run preload (strategy "prebuild"): serves modules ts-r-run compiled before starting
// Node, from one manifest file. Synchronous in-thread hooks (module.registerHooks) cover both
// `import` and `require`.
import Module, { findPackageJSON, registerHooks, setSourceMapsSupport } from 'node:module';
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
let failed = Object.create(null);
let blob = null;
let base = 0;
if (manifestPath) {
  blob = readFileSync(manifestPath);
  const nl = blob.indexOf(10);
  const index = JSON.parse(blob.toString('utf8', 0, nl));
  files = index.f;
  resolved = index.r;
  failed = index.e ?? failed;
  base = nl + 1;
}
if (sourceMaps) setSourceMapsSupport(true);

const COMPILED = /\.(?:[cm]?ts|tsx|jsx)$/i;
const DECL = /\.d\.[cm]?ts$|\.d\.[^/]*\.ts$/i;

// A module that can't be compiled throws when it is loaded, so `import()` rejects and
// `require()` throws with an error the program can catch, named as under tsx.
class TransformError extends Error {}
Object.defineProperty(TransformError.prototype, 'name', { value: 'TransformError', configurable: true, writable: true });
const transformError = (errors) => new TransformError(`Transform failed:\n${errors.trimEnd()}`);

// A file the graph walk didn't reach (computed specifier, require from plain JS): compile
// it now with a short-lived ts-r-run process.
function compileNow(path) {
  const r = spawnSync(bin, sourceMaps ? ['--compile', path] : ['--no-source-maps', '--compile', path], {
    encoding: 'utf8',
    maxBuffer: 1 << 30,
    env: { ...env, TS_R_RUN_TSCONFIG: tsconfig ?? '', TS_R_RUN_ENTRY: entry ?? '' },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  if (r.status !== 0) throw transformError(String(r.stderr || r.error || `ts-r-run could not compile ${path}`));
  const nl = r.stdout.indexOf('\n');
  return [r.stdout.slice(0, nl) === 'cjs' ? 1 : 0, r.stdout.slice(nl + 1)];
}

// CommonJS output emulates `import.meta` with the file's URL (compile.rs, IMPORT_META); a
// module loaded with a query or hash gets its own URL.
const META_URL = '/*ts-r-run:url*/require("node:url").pathToFileURL(__filename).href';
// Node keys CommonJS modules by file name, so `./config.ts?x=1` and `./config.ts?x=2` would
// share one instance; under tsx each query is a module of its own. A CommonJS file imported
// with a query or hash is therefore resolved to a URL whose path keeps them (`config.ts%3Fx=1`,
// file name `config.ts?x=1`), and loaded from the real file. So is any file `require()`d
// with a query (Node's `require()` would not find it).
const aliases = new Map();
function queryAlias(url, required = false) {
  const cut = url.search(/[?#]/);
  const path = toPath(url.slice(0, cut));
  if (!required && files[path]?.[0] !== 1) return { url, shortCircuit: true };
  const name = path + url.slice(cut);
  const alias = pathToFileURL(name).href;
  aliases.set(alias, url);
  return { url: alias, format: 'commonjs', shortCircuit: true };
}

function withUrl(source, url) {
  const at = source.lastIndexOf(META_URL);
  return at === -1 ? source : source.slice(0, at) + JSON.stringify(url) + source.slice(at + META_URL.length);
}

const SWAP = { '.js': ['.ts', '.tsx'], '.jsx': ['.tsx'], '.mjs': ['.mts'], '.cjs': ['.cts'] };
// Not `.mts` or `.cts`: an extensionless path never finds those, under tsx.
const ADD = ['.ts', '.tsx', '.js', '.jsx', '.json'];
const INDEX = ['index.ts', 'index.tsx', 'index.js', 'index.json'];
// Inside node_modules, JavaScript wins over TypeScript sources shipped next to it.
const PACKAGE_ORDER = {
  add: ['.js', '.json', '.jsx', '.ts', '.tsx'],
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
  // `require()` takes no file URL; tsx's does.
  if (specifier.startsWith('file:')) return specifier;
  if (/^\.\.?(?:\/|$)/.test(specifier) || specifier.startsWith('/')) {
    return parent !== undefined && parent.startsWith('file:') ? new URL(specifier, parent).href : null;
  }
  if (typeof error?.url === 'string' && error.url.startsWith('file:')) return error.url;
  // A package `main` that doesn't exist: "Cannot find package '<dir>/<main>' imported from".
  const m = /^Cannot find package '([^']+)' imported from /.exec(String(error?.message ?? ''));
  if (m !== null && (m[1].startsWith('/') || /^[A-Za-z]:[\\/]/.test(m[1]))) return pathToFileURL(m[1]).href;
  // `require()` says only "Cannot find module": the path is the package's directory in the
  // nearest node_modules that has it, joined with the subpath.
  if (error?.code === 'MODULE_NOT_FOUND' && BARE.test(specifier) && parent?.startsWith('file:')) {
    const parts = specifier.split('/');
    const name = parts.slice(0, specifier.startsWith('@') ? 2 : 1).join('/');
    let dir = toPath(parent);
    for (let up = dir.lastIndexOf('/'); up > 0; up = dir.lastIndexOf('/')) {
      dir = dir.slice(0, up);
      if (dir.endsWith('/node_modules')) continue;
      if (isDir(`${dir}/node_modules/${name}`)) return pathToFileURL(`${dir}/node_modules/${specifier}`).href;
    }
  }
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
  // The file itself, where `require()` failed only on a file URL or a query: a query makes
  // a module of its own, as for `import`.
  if (isFile(p) && !DECL.test(p)) {
    if (suffix === '') return { url, shortCircuit: true };
    if (isRequire(context)) return queryAlias(url, true);
  }
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
  if (!ESM_SYNTAX.test(text)) return interopJs(r, url, bare, text);
  const type = packageType(bare);
  if (type === 'module' || (type !== 'commonjs' && !CJS_GLOBALS.test(text))) return interopJs(r, url, bare, text);
  let [format, source] = compileNow(toPath(bare));
  if (format === 1 && url !== bare) source = withUrl(source, url);
  return { format: format === 1 ? 'commonjs' : 'module', source, shortCircuit: true };
}

const isRequire = (context) => {
  const c = context.conditions ?? [];
  return c.includes('require') && !c.includes('import');
};

// An ES module in plain JavaScript that calls `import()` is compiled too, so that the result
// goes through the same interop as in TypeScript (compile.rs, IMPORT_INTEROP), as tsx does.
// Packages in node_modules are left as they are: compiling one costs a process.
function interopJs(r, url, bare, text) {
  if (r.format !== 'module' || r.source == null || bare.includes('/node_modules/')) return r;
  text ??= typeof r.source === 'string' ? r.source : Buffer.from(r.source).toString('utf8');
  if (!/\bimport\s*\(/.test(text)) return r;
  let compiled;
  try {
    compiled = compileNow(toPath(bare));
  } catch {
    // Node runs it as it is, and reports its own errors.
    return r;
  }
  return compiled[0] === 1 ? r : { format: 'module', source: compiled[1], shortCircuit: true };
}

// A JSON object imported as an ES module: its keys are named exports as well as `default`
// (the names that can be export names: `default` and lone surrogates can't).
function jsonModule(r) {
  if (r.format !== 'json' || r.source == null) return r;
  const text = typeof r.source === 'string' ? r.source : Buffer.from(r.source).toString('utf8');
  let value;
  try {
    value = JSON.parse(text);
  } catch {
    return r;
  }
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return r;
  const keys = Object.keys(value).filter((k) => k !== 'default' && k.isWellFormed());
  if (keys.length === 0) return r;
  const names = keys.map((k, i) => `v${i} as ${JSON.stringify(k)}`).join(', ');
  const locals = keys.map((k, i) => `v${i} = d[${JSON.stringify(k)}]`).join(', ');
  const source = `const d = JSON.parse(${JSON.stringify(text)});\nconst ${locals};\nexport default d;\nexport { ${names} };\n`;
  return { format: 'module', source, shortCircuit: true };
}

registerHooks({
  resolve(specifier, context, nextResolve) {
    const parent = context.parentURL && aliases.get(context.parentURL);
    return resolveModule(specifier, parent ? { ...context, parentURL: parent } : context, nextResolve);
  },
  load(url, context, nextLoad) {
    return loadModule(aliases.get(url) ?? url, context, nextLoad);
  },
});

// `require()` of a module with top-level await: tsx compiles it to CommonJS, which can't
// hold one, and throws a TransformError saying so (with Node's code). Node's own error is
// renamed to match; its message is kept after tsx's reason.
const nodeRequire = Module.prototype.require;
Module.prototype.require = function require(id) {
  try {
    return Reflect.apply(nodeRequire, this, arguments);
  } catch (e) {
    if (e?.code !== 'ERR_REQUIRE_ASYNC_MODULE' || e.name === 'TransformError') throw e;
    Object.setPrototypeOf(e, TransformError.prototype);
    e.message = `Top-level await is currently not supported with the "cjs" output format: ${e.message}`;
    const at = typeof e.stack === 'string' ? e.stack.indexOf('\n    at ') : -1;
    if (at !== -1) e.stack = `TransformError [${e.code}]: ${e.message}${e.stack.slice(at)}`;
    throw e;
  }
};

// A TypeScript file Node's CommonJS loader reaches after an `import` (one this file's resolve
// fallback found) can keep the format Node gave it from its extension, and Node would strip
// types from the JavaScript the load hook already returned (and refuses to inside
// node_modules). Every TypeScript source here comes from that hook, so it runs as it is.
const TS_FORMAT = { 'commonjs-typescript': 'commonjs', 'module-typescript': 'module', typescript: undefined };
const nodeCompile = Module.prototype._compile;
Module.prototype._compile = function _compile(content, filename, format) {
  if (Object.hasOwn(TS_FORMAT, format) && COMPILED.test(filename) && !DECL.test(filename)) {
    return Reflect.apply(nodeCompile, this, [content, filename, TS_FORMAT[format]]);
  }
  return Reflect.apply(nodeCompile, this, arguments);
};

function resolveModule(specifier, context, nextResolve) {
  const parent = context.parentURL;
  if (parent !== undefined && parent.startsWith('file:')) {
    const map = resolved[toPath(parent)];
    if (map !== undefined) {
      const target = map[specifier];
      if (target !== undefined) {
        const cut = specifier.search(/[?#]/);
        const url = pathToFileURL(target).href;
        return cut === -1 ? { url, shortCircuit: true } : queryAlias(new URL(specifier.slice(cut), url).href);
      }
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
}

function loadModule(url, context, nextLoad) {
  if (url.startsWith('file:')) {
    let end = url.length;
    const q = url.indexOf('?');
    if (q !== -1) end = q;
    const h = url.indexOf('#');
    if (h !== -1 && h < end) end = h;
    const bare = end === url.length ? url : url.slice(0, end);
    if (COMPILED.test(bare) && !DECL.test(bare)) {
      const path = toPath(bare);
      if (failed[path] !== undefined) throw transformError(failed[path]);
      const f = files[path];
      let format;
      let source;
      if (f !== undefined) {
        format = f[0];
        source = blob.toString('utf8', base + f[1], base + f[2]);
      } else {
        [format, source] = compileNow(path);
      }
      if (format === 1 && end !== url.length) source = withUrl(source, url);
      return { format: format === 1 ? 'commonjs' : 'module', source, shortCircuit: true };
    }
    if (bare.endsWith('.js')) return lenientJs(url, bare, context, nextLoad);
    if (bare.endsWith('.mjs')) return interopJs(nextLoad(url, context), url, bare);
    // JSON imports need no `with { type: 'json' }` attribute, as under tsx, and an object's
    // keys are named exports too.
    if (bare.endsWith('.json') && context.importAttributes?.type === undefined) {
      const r = nextLoad(url, { ...context, importAttributes: { ...context.importAttributes, type: 'json' } });
      return isRequire(context) ? r : jsonModule(r);
    }
  }
  return nextLoad(url, context);
}
