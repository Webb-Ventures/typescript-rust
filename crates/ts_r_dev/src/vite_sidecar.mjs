// ts-r-dev's Vite plugin sidecar (crates/ts_r_dev/src/vite_plugins.rs starts it).
//
// Written for ts-r from Vite's and Rollup's documented plugin API (not derived from their
// source). Loads vite.config.*, runs the `config`/`configResolved`/`configureServer`/
// `buildStart` hooks once, then answers hook calls from ts-r-dev: one JSON object per line
// on stdin, one per line on stdout. Plugin output to stdout is moved to stderr so it can't
// corrupt the protocol.
//
// Requests:  {id, op: "resolve"|"load"|"transform"|"html"|"hot"|"middleware"|"proxyRewrite", ...}
//            {op: "event", event, data}            (no reply)
//            {rid, result}                         (reply to a resolve callback)
// Replies:   {ready: manifest} | {fatal: message}  (first line)
//            {id, ok} | {id, error}
//            {ws: payload}                         (server.ws.send: broadcast to pages)
//            {rid, resolve: {specifier, importer}} (this.resolve: ask the built-in resolver)

import { EventEmitter } from 'node:events'
import fs from 'node:fs'
import * as nodeModule from 'node:module'
import path from 'node:path'
import readline from 'node:readline'
import { Readable, Writable } from 'node:stream'
import { pathToFileURL } from 'node:url'

// V8 code cache for the config and the plugins it imports (Node 22.1+; the same cache the
// `vite` CLI turns on), in the OS temp dir: later starts skip compiling them.
const { createRequire } = nodeModule
try {
  nodeModule.enableCompileCache?.()
} catch {}

const writeOut = process.stdout.write.bind(process.stdout)
process.stdout.write = process.stderr.write.bind(process.stderr)
const send = (msg) => writeOut(JSON.stringify(msg) + '\n')

const root = process.env.TS_R_ROOT
const configFile = process.env.TS_R_CONFIG
process.env.NODE_ENV ??= 'development'
// The environment before the config file runs: what it sets (`process.env.X = ...`) is
// reported to the server, which loads `.env` files with it.
const startEnv = { ...process.env }

// Plugins ts-r-dev replaces with built-ins: their `config` hooks run (for their options),
// nothing else does.
const isReplaced = (p) =>
  typeof p.name === 'string' &&
  (p.name.startsWith('vite:react') || p.name.startsWith('@vitejs/plugin-react'))

const mode = process.env.TS_R_MODE || 'development'
const env = { command: 'serve', mode, isSsrBuild: false, isPreview: false }

function isPlainObject(v) {
  return v !== null && typeof v === 'object' && Object.getPrototypeOf(v) === Object.prototype
}

// Deep merge: objects recursively, arrays concatenated, anything else replaced.
function merge(a, b) {
  if (Array.isArray(a) && Array.isArray(b)) return [...a, ...b]
  if (!isPlainObject(a) || !isPlainObject(b)) return b === undefined ? a : b
  const out = { ...a }
  for (const [k, v] of Object.entries(b)) out[k] = k in out ? merge(out[k], v) : v
  return out
}

async function flatten(list) {
  const out = []
  for (let p of list ?? []) {
    p = await p
    if (!p) continue
    if (Array.isArray(p)) out.push(...(await flatten(p)))
    else out.push(p)
  }
  return out
}

// ---- hooks ----

function hookOf(plugin, name) {
  const h = plugin[name]
  if (!h) return null
  if (typeof h === 'function') return { fn: h, order: null, filter: null }
  if (typeof h.handler === 'function') return { fn: h.handler, order: h.order ?? null, filter: h.filter ?? null }
  // transformIndexHtml's older object form.
  if (typeof h.transform === 'function') return { fn: h.transform, order: h.order ?? h.enforce ?? null, filter: null }
  return null
}

// Every plugin's hook `name`, in Vite's order: `order: 'pre'` hooks, then the rest in
// plugin order (enforce pre, normal, post), then `order: 'post'`.
function sorted(plugins, name) {
  const pre = []
  const normal = []
  const post = []
  for (const plugin of plugins) {
    const h = hookOf(plugin, name)
    if (!h) continue
    const entry = { plugin, ...h, test: compileFilter(h.filter) }
    ;(h.order === 'pre' ? pre : h.order === 'post' ? post : normal).push(entry)
  }
  return [...pre, ...normal, ...post]
}

// Which side of ts-r-dev's built-in step a hook runs on (resolveId/load/transform).
const stageOf = (h) =>
  h.order === 'pre' || (h.order !== 'post' && h.plugin.enforce === 'pre') ? 'pre' : 'post'

function globToRegExp(glob) {
  let re = ''
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i]
    if (c === '*') {
      if (glob[i + 1] === '*') {
        i++
        if (glob[i + 1] === '/') {
          i++
          re += '(?:.*/)?'
        } else re += '.*'
      } else re += '[^/]*'
    } else if (c === '?') re += '[^/]'
    else if (c === '{') {
      const end = glob.indexOf('}', i)
      if (end < 0) re += '\\{'
      else {
        re += '(?:' + glob.slice(i + 1, end).split(',').map((s) => s.replace(/[.+^$()|[\]\\]/g, '\\$&')).join('|') + ')'
        i = end
      }
    } else re += c.replace(/[.+^$()|[\]\\/]/g, '\\$&')
  }
  return new RegExp('^' + re + '$')
}

function idMatcher(p) {
  if (p instanceof RegExp) return (s) => { p.lastIndex = 0; return p.test(s) }
  const glob = p.startsWith('/') || p.startsWith('*') ? p : path.posix.join(root.replaceAll('\\', '/'), p)
  const re = globToRegExp(glob)
  return (s) => re.test(s)
}

function codeMatcher(p) {
  if (p instanceof RegExp) return (s) => { p.lastIndex = 0; return p.test(s) }
  return (s) => s.includes(p)
}

function stringFilter(f, make) {
  if (f == null) return null
  const list = (v) => (v == null ? [] : Array.isArray(v) ? v : [v]).map(make)
  const isIncEx = isPlainObject(f) && ('include' in f || 'exclude' in f)
  const include = list(isIncEx ? f.include : f)
  const exclude = list(isIncEx ? f.exclude : null)
  return (s) => !exclude.some((m) => m(s)) && (include.length === 0 || include.some((m) => m(s)))
}

// Hook filters (`{ filter: { id, code, moduleType }, handler }`). A `moduleType` filter is a
// list (or `{ include }`) of the types a hook takes; only `transform` has a module type.
function compileFilter(filter) {
  if (!filter) return () => true
  const id = stringFilter(filter.id, idMatcher)
  const code = stringFilter(filter.code, codeMatcher)
  const mt = filter.moduleType
  const types = mt == null ? null : Array.isArray(mt) ? mt : (mt.include ?? [])
  return (moduleId, src, moduleType) =>
    (!types || moduleType == null || types.includes(moduleType)) &&
    (!id || id(moduleId)) &&
    (!code || src == null || code(src))
}

// The module type Vite guesses from an id (vite: packages/vite/src/node/server/
// transformRequest.ts): the first `.ext` up to the end that names a known type.
const moduleTypes = { js: 'js', mjs: 'js', cjs: 'js', jsx: 'jsx', ts: 'ts', mts: 'ts', cts: 'ts', tsx: 'tsx', json: 'json', txt: 'text', css: 'css' }
function guessModuleType(id) {
  for (let i = id.indexOf('.'); i >= 0; i = id.indexOf('.', i + 1)) {
    const t = moduleTypes[id.slice(i + 1)]
    if (t) return t
  }
  return undefined
}

// ---- config ----

function packageVersion(require, name) {
  try {
    const file = require.resolve(`${name}/package.json`)
    return { file, version: JSON.parse(fs.readFileSync(file, 'utf8')).version }
  } catch {
    return null
  }
}

let ready = false
process.on('uncaughtException', (e) => {
  if (!ready) {
    send({ fatal: e?.stack ?? String(e) })
    process.exit(1)
  }
  console.error('[ts-r-dev plugin sidecar] uncaught:', e)
})
process.on('unhandledRejection', (e) => console.error('[ts-r-dev plugin sidecar] unhandled rejection:', e))

const vite = packageVersion(createRequire(path.resolve(configFile)), 'vite')
const rolldown = vite && packageVersion(createRequire(vite.file), 'rolldown')
// The `this.meta` the project's installed Vite would give (plugins pick APIs by it).
const meta = { watchMode: true }
if (vite) meta.viteVersion = vite.version
if (rolldown) meta.rolldownVersion = rolldown.version

let config
let resolved
let all = []
let active = []
const warnings = []
const logger = {
  info: (m) => console.error(m),
  warn: (m) => console.error(m),
  warnOnce: (m) => console.error(m),
  error: (m) => console.error(m),
  clearScreen() {},
  hasWarned: false,
  hasErrorLogged: () => false,
}

// ---- this.resolve and the resolver callback ----

let nextRid = 1
const callbacks = new Map()
function builtinResolve(specifier, importer) {
  const rid = nextRid++
  return new Promise((resolve) => {
    callbacks.set(rid, resolve)
    send({ rid, resolve: { specifier, importer: importer ?? '' } })
  })
}

function normalizeResolution(r, specifier) {
  if (r == null) return null
  if (r === false) return { id: specifier, external: true }
  if (typeof r === 'string') return { id: r, external: false }
  if (typeof r === 'object' && typeof r.id === 'string') return { id: r.id, external: !!r.external }
  return null
}

function context(plugin) {
  const name = plugin?.name ?? 'ts-r-dev'
  return {
    meta,
    // `hot` lets hotUpdate hooks send custom events (`this.environment.hot.send`).
    environment: { name: 'client', mode: 'dev', config: resolved, logger, get hot() { return ws }, get moduleGraph() { return moduleGraph } },
    async resolve(source, importer, options = {}) {
      const skip = options.skipSelf === false ? null : plugin
      for (const h of sorted(active, 'resolveId')) {
        if (h.plugin === skip || !h.test(source)) continue
        const r = normalizeResolution(await h.fn.call(context(h.plugin), source, importer, { isEntry: false, attributes: {}, ...options }), source)
        if (r) return r
      }
      const r = await builtinResolve(aliased(source) ?? source, importer)
      return r ? { id: r[0], external: r[1] } : null
    },
    addWatchFile() {},
    getWatchFiles: () => [],
    emitFile: () => '',
    getFileName: () => '',
    setAssetSource() {},
    getModuleInfo: () => null,
    getModuleIds: () => [][Symbol.iterator](),
    getCombinedSourcemap: () => null,
    parse() {
      throw new Error('this.parse() is not supported by ts-r-dev')
    },
    load() {
      throw new Error('this.load() is not supported by ts-r-dev')
    },
    warn: (m) => console.error(`[${name}] ${m?.message ?? m}`),
    info: (m) => console.error(`[${name}] ${m?.message ?? m}`),
    debug() {},
    error(e) {
      throw e instanceof Error ? e : new Error(e?.message ?? String(e))
    },
  }
}

function hookError(plugin, e) {
  const where = e?.loc ? ` (${e.loc.file ?? e.id ?? ''}:${e.loc.line}:${e.loc.column})` : e?.id ? ` (${e.id})` : ''
  return `[plugin ${plugin.name ?? 'unnamed'}] ${e?.message ?? String(e)}${where}`
}

// ---- resolve.alias ----

let aliases = []
function aliased(specifier) {
  for (const { find, replacement } of aliases) {
    if (find instanceof RegExp) {
      find.lastIndex = 0
      if (find.test(specifier)) return specifier.replace(find, replacement)
    } else if (specifier === find || specifier.startsWith(find + '/')) {
      return replacement + specifier.slice(find.length)
    }
  }
  return null
}

// ---- ops ----

async function opResolve({ stage, importer, specifiers }) {
  const hooks = sorted(active, 'resolveId').filter((h) => stageOf(h) === stage)
  const out = []
  for (const original of specifiers) {
    const specifier = (stage === 'pre' && aliased(original)) || original
    let answer = null
    for (const h of hooks) {
      if (!h.test(specifier)) continue
      let r
      try {
        r = await h.fn.call(context(h.plugin), specifier, importer || undefined, { isEntry: false, attributes: {}, ssr: false })
      } catch (e) {
        throw new Error(hookError(h.plugin, e))
      }
      r = normalizeResolution(r, specifier)
      if (r) {
        answer = r.external ? { external: r.id } : { id: r.id }
        break
      }
    }
    if (!answer && specifier !== original) answer = { rewrite: specifier }
    out.push(answer)
  }
  return out
}

async function opLoad({ moduleId }) {
  for (const h of sorted(active, 'load')) {
    if (!h.test(moduleId)) continue
    let r
    try {
      r = await h.fn.call(context(h.plugin), moduleId, { ssr: false })
    } catch (e) {
      throw new Error(hookError(h.plugin, e))
    }
    if (r != null) {
      loadedModule(moduleId)
      return typeof r === 'string' ? r : (r.code ?? null)
    }
  }
  return null
}

// A hook's `map` as JSON text; null when there is none (`{ mappings: '' }` included).
function mapText(map) {
  if (map == null) return null
  if (typeof map === 'string') return map
  if (typeof map === 'object' && map.mappings === '') return null
  try {
    return JSON.stringify(map)
  } catch {
    return null
  }
}

// Module types: before ts-r-dev's built-in step the type guessed from the id (`json` for
// JSON, `ts` for TypeScript), after it `js`, as Vite's own JSON and TS plugins return. A hook
// may change it (`{ code, moduleType }`).
async function opTransform({ stage, moduleId, code }) {
  let changed = false
  const maps = []
  const options = { ssr: false, moduleType: (stage === 'pre' ? guessModuleType(moduleId) : null) ?? 'js' }
  for (const h of sorted(active, 'transform')) {
    if (stageOf(h) !== stage || !h.test(moduleId, code, options.moduleType)) continue
    let r
    try {
      r = await h.fn.call(context(h.plugin), code, moduleId, options)
    } catch (e) {
      throw new Error(hookError(h.plugin, e))
    }
    if (r == null) continue
    if (typeof r === 'object' && typeof r.moduleType === 'string') options.moduleType = r.moduleType
    const next = typeof r === 'string' ? r : r.code
    if (typeof next === 'string' && next !== code) {
      code = next
      changed = true
      maps.push(typeof r === 'string' ? null : mapText(r.map))
    }
  }
  return changed ? { code, maps } : null
}

// ---- dev server options (define, env, base, server.*) ----

const pem = (v) => {
  if (v == null) return null
  if (Array.isArray(v)) return v.map(pem).filter(Boolean).join('\n')
  if (typeof v === 'string') return v
  if (Buffer.isBuffer(v) || v instanceof Uint8Array) return Buffer.from(v).toString('utf8')
  return null
}
const regex = (r) => ({ regex: r.source, flags: r.flags })

function corsOptions(c) {
  if (c === undefined || c === null) return null
  if (typeof c === 'boolean') return c
  if (typeof c !== 'object') return null
  const list = (v) => (Array.isArray(v) ? v.join(',') : typeof v === 'string' ? v : null)
  let origin
  if (c.origin === undefined || c.origin === '*') origin = '*'
  else if (typeof c.origin === 'boolean') origin = c.origin
  else if (typeof c.origin === 'string') origin = c.origin
  else if (c.origin instanceof RegExp) origin = [regex(c.origin)]
  else if (Array.isArray(c.origin)) origin = c.origin.map((o) => (o instanceof RegExp ? regex(o) : String(o)))
  else {
    warnings.push('`server.cors.origin` as a function is not supported by ts-r-dev; using the default')
    return null
  }
  return {
    origin,
    methods: list(c.methods),
    allowedHeaders: list(c.allowedHeaders ?? c.headers),
    exposedHeaders: list(c.exposedHeaders),
    credentials: c.credentials === true,
    maxAge: typeof c.maxAge === 'number' ? c.maxAge : null,
  }
}

let proxyRules = []
function proxyOptions(p) {
  if (!p || typeof p !== 'object') return []
  const out = []
  proxyRules = []
  for (const [key, value] of Object.entries(p)) {
    const o = typeof value === 'string' ? { target: value, changeOrigin: true } : value
    if (!o || typeof o !== 'object') continue
    let target = o.target
    if (target && typeof target === 'object') {
      const proto = target.protocol ?? 'http:'
      target = `${proto}//${target.host ?? target.hostname ?? 'localhost'}${target.port ? `:${target.port}` : ''}${target.path ?? ''}`
    }
    if (typeof target !== 'string') {
      warnings.push(`\`server.proxy['${key}']\` has no target; skipped`)
      continue
    }
    if (o.configure) warnings.push(`\`server.proxy['${key}'].configure\` is not run by ts-r-dev`)
    if (o.bypass) warnings.push(`\`server.proxy['${key}'].bypass\` is not run by ts-r-dev`)
    proxyRules.push(o)
    out.push({
      key,
      target,
      changeOrigin: o.changeOrigin === true,
      ws: o.ws === true,
      secure: o.secure !== false,
      rewrite: typeof o.rewrite === 'function',
      headers: Object.entries(o.headers ?? {}).map(([k, v]) => [k, String(v)]),
      index: proxyRules.length - 1,
    })
  }
  return out
}

function opProxyRewrite({ index, url }) {
  const rule = proxyRules[index]
  if (!rule || typeof rule.rewrite !== 'function') return null
  const r = rule.rewrite(url)
  return typeof r === 'string' ? r : null
}

// A directory relative to the root when it is given relative or lies inside the root: the
// manifest cache is shared by checkouts with the same config, and ts-r-dev resolves it
// against its own root.
function rootRelative(dir) {
  if (!path.isAbsolute(dir)) return dir
  const rel = path.relative(root, dir)
  return rel.startsWith('..') || path.isAbsolute(rel) ? dir : rel || '.'
}

/**
 * `assetsInclude` as data: a glob that doesn't start with `**` is resolved from the working
 * directory (as Vite's filter does) and sent root-relative; a RegExp as its source and flags.
 */
function assetsIncludePatterns(value) {
  const out = []
  for (const p of [value ?? []].flat()) {
    if (p instanceof RegExp) out.push({ regex: p.source, flags: p.flags })
    else if (typeof p === 'string') {
      const glob = p.startsWith('**') ? p : rootRelative(path.resolve(p)).split(path.sep).join('/')
      out.push({ glob })
    }
  }
  return out
}

function devOptions() {
  const s = resolved.server ?? {}
  const define = {}
  // The client environment's own defines (`environments.client.define`) add to the top-level
  // ones, as Vite merges them for the browser.
  const defines = { ...(config.define ?? {}), ...(config.environments?.client?.define ?? {}) }
  for (const [k, v] of Object.entries(defines)) {
    define[k] = typeof v === 'string' ? v : JSON.stringify(v) ?? 'undefined'
  }
  const host = s.host === true ? '0.0.0.0' : typeof s.host === 'string' ? s.host : null
  const https = s.https && typeof s.https === 'object' ? { cert: pem(s.https.cert), key: pem(s.https.key) } : null
  if (s.https && !(https?.cert && https?.key)) warnings.push('`server.https` needs `cert` and `key` (PEM) for ts-r-dev; serving http')
  const envPrefix = config.envPrefix ?? 'VITE_'
  return {
    mode,
    base: config.base ?? '/',
    define,
    processEnv: Object.fromEntries(
      Object.entries(process.env).filter(([k, v]) => startEnv[k] !== v && typeof v === 'string'),
    ),
    envPrefix: Array.isArray(envPrefix) ? envPrefix.map(String) : [String(envPrefix)],
    envDir: config.envDir === false ? false : typeof config.envDir === 'string' ? rootRelative(config.envDir) : null,
    publicDir: config.publicDir === false ? false : typeof config.publicDir === 'string' ? rootRelative(config.publicDir) : null,
    assetsInclude: assetsIncludePatterns(config.assetsInclude),
    assetsInlineLimit: typeof config.build?.assetsInlineLimit === 'number' ? config.build.assetsInlineLimit : null,
    hmrPartialAccept: config.experimental?.hmrPartialAccept === true,
    server: {
      host,
      port: typeof s.port === 'number' ? s.port : null,
      strictPort: s.strictPort === true,
      open: typeof s.open === 'string' ? s.open : s.open === true,
      headers: Object.entries(s.headers ?? {}).map(([k, v]) => [k, Array.isArray(v) ? v.join(', ') : String(v)]),
      cors: corsOptions(s.cors),
      proxy: proxyOptions(s.proxy),
      https: https?.cert && https?.key ? https : null,
    },
  }
}

const VOID = new Set(['area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input', 'link', 'meta', 'source', 'track', 'wbr'])
const escapeAttr = (v) => String(v).replaceAll('&', '&amp;').replaceAll('"', '&quot;')

function renderTag(t) {
  if (typeof t === 'string') return t
  let attrs = ''
  for (const [k, v] of Object.entries(t.attrs ?? {})) {
    if (v === true) attrs += ` ${k}`
    else if (v !== false && v != null) attrs += ` ${k}="${escapeAttr(v)}"`
  }
  if (VOID.has(t.tag)) return `<${t.tag}${attrs}>`
  const children = Array.isArray(t.children) ? t.children.map(renderTag).join('') : (t.children ?? '')
  return `<${t.tag}${attrs}>${children}</${t.tag}>`
}

function injectTags(html, tags) {
  const groups = { 'head-prepend': [], head: [], 'body-prepend': [], body: [] }
  for (const t of tags) (groups[t.injectTo] ?? groups['head-prepend']).push(renderTag(t))
  const before = (re, s) => (re.test(html) ? html.replace(re, (m) => s + m) : html)
  const after = (re, s) => (re.test(html) ? html.replace(re, (m) => m + s) : null)
  if (groups['head-prepend'].length) {
    const s = groups['head-prepend'].join('\n')
    html = after(/<head[^>]*>/i, s) ?? after(/<html[^>]*>/i, s) ?? s + html
  }
  if (groups.head.length) {
    const s = groups.head.join('\n')
    html = /<\/head>/i.test(html) ? before(/<\/head>/i, s) : after(/<body[^>]*>/i, s) ?? s + html
  }
  if (groups['body-prepend'].length) {
    const s = groups['body-prepend'].join('\n')
    html = after(/<body[^>]*>/i, s) ?? html + s
  }
  if (groups.body.length) {
    const s = groups.body.join('\n')
    html = /<\/body>/i.test(html) ? before(/<\/body>/i, s) : html + s
  }
  return html
}

async function opHtml({ url, file, html }) {
  const tags = []
  for (const h of sorted(active, 'transformIndexHtml')) {
    let r
    try {
      r = await h.fn.call(context(h.plugin), html, { path: url, filename: file, originalUrl: url, server })
    } catch (e) {
      throw new Error(hookError(h.plugin, e))
    }
    if (r == null) continue
    if (typeof r === 'string') html = r
    else if (Array.isArray(r)) tags.push(...r)
    else {
      if (typeof r.html === 'string') html = r.html
      if (Array.isArray(r.tags)) tags.push(...r.tags)
    }
  }
  return tags.length ? injectTags(html, tags) : html
}

async function opHot({ file, modules, timestamp }) {
  watcher.emit('change', file)
  watcher.emit('all', 'change', file)
  let mods = modules.map((url) => ({ url, id: url, file, type: 'js', importers: new Set(), importedModules: new Set() }))
  let narrowed = false
  const read = () => fs.promises.readFile(file, 'utf8')
  for (const name of ['hotUpdate', 'handleHotUpdate']) {
    for (const h of sorted(active, name)) {
      const ctx = { type: 'update', file, timestamp, modules: mods, read, server }
      let r
      try {
        r = await h.fn.call(context(h.plugin), ctx)
      } catch (e) {
        throw new Error(hookError(h.plugin, e))
      }
      if (Array.isArray(r)) {
        mods = r
        narrowed = true
      }
    }
  }
  return narrowed ? mods.map((m) => m.url).filter((u) => typeof u === 'string') : null
}

// ---- configureServer ----

const stack = []
const watcher = Object.assign(new EventEmitter(), { add() {}, unwatch() {}, close: async () => {}, options: {} })
const wsListeners = new Map()
const ws = {
  send(payload, data) {
    if (typeof payload === 'string') payload = { type: 'custom', event: payload, data }
    send({ ws: payload })
  },
  on(event, cb) {
    if (!wsListeners.has(event)) wsListeners.set(event, new Set())
    wsListeners.get(event).add(cb)
  },
  off(event, cb) {
    wsListeners.get(event)?.delete(cb)
  },
  listen() {},
  close: async () => {},
  clients: new Set(),
}
// Modules a plugin's `load` produced, for `moduleGraph.getModuleById` and `reloadModule`.
const loadedModules = new Map()
function loadedModule(id) {
  let mod = loadedModules.get(id)
  if (!mod) {
    const url = '/@id/' + id.replaceAll('\0', '__x00__')
    mod = { id, url, file: null, type: 'js', importers: new Set(), importedModules: new Set(), transformResult: null }
    loadedModules.set(id, mod)
  }
  return mod
}
async function reloadModule(mod) {
  if (typeof mod?.id === 'string') send({ reload: mod.id })
  else ws.send({ type: 'full-reload' })
}
const moduleGraph = {
  getModuleById: (id) => loadedModules.get(id),
  getModuleByUrl: async () => undefined,
  getModulesByFile: () => undefined,
  invalidateModule(mod) {
    if (typeof mod?.id === 'string') send({ forget: mod.id })
  },
  invalidateAll() {
    send({ forget_all: true })
  },
  idToModuleMap: new Map(),
  urlToModuleMap: new Map(),
  fileToModulesMap: new Map(),
}
const server = {
  get config() {
    return resolved
  },
  middlewares: {
    use(route, fn) {
      if (typeof route === 'function') stack.push({ route: '', fn: route })
      else stack.push({ route: route.replace(/\/$/, ''), fn })
      return this
    },
    stack,
  },
  ws,
  hot: ws,
  watcher,
  moduleGraph,
  environments: { client: { name: 'client', hot: ws, moduleGraph, reloadModule, get config() { return resolved } } },
  httpServer: null,
  resolvedUrls: null,
  transformIndexHtml: (url, html) => opHtml({ url, file: path.join(root, 'index.html'), html }),
  transformRequest: async () => null,
  warmupRequest: async () => {},
  reloadModule,
  ssrLoadModule: async () => {
    throw new Error('ssrLoadModule is not supported by ts-r-dev')
  },
  printUrls() {},
  bindCLIShortcuts() {},
  restart: async () => {},
  close: async () => {},
}

class FakeResponse extends Writable {
  constructor(done) {
    super()
    this.statusCode = 200
    this.statusMessage = ''
    this.headersSent = false
    this.chunks = []
    this.headerMap = new Map()
    this.on('finish', () => done(this))
  }
  _write(chunk, encoding, cb) {
    this.headersSent = true
    this.chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk, encoding))
    cb()
  }
  setHeader(k, v) {
    this.headerMap.set(k.toLowerCase(), [k, v])
    return this
  }
  getHeader(k) {
    return this.headerMap.get(k.toLowerCase())?.[1]
  }
  getHeaders() {
    return Object.fromEntries([...this.headerMap.values()].map(([k, v]) => [k.toLowerCase(), v]))
  }
  getHeaderNames() {
    return [...this.headerMap.keys()]
  }
  hasHeader(k) {
    return this.headerMap.has(k.toLowerCase())
  }
  removeHeader(k) {
    this.headerMap.delete(k.toLowerCase())
  }
  writeHead(status, message, headers) {
    if (typeof message !== 'string') [message, headers] = [undefined, message]
    this.statusCode = status
    if (message) this.statusMessage = message
    if (Array.isArray(headers)) for (let i = 0; i + 1 < headers.length; i += 2) this.setHeader(headers[i], headers[i + 1])
    else for (const [k, v] of Object.entries(headers ?? {})) this.setHeader(k, v)
    return this
  }
  flushHeaders() {}
}

function opMiddleware({ method, url, headers, body }) {
  return new Promise((resolve) => {
    const req = new Readable({ read() {} })
    req.push(Buffer.from(body ?? '', 'base64'))
    req.push(null)
    Object.assign(req, {
      method,
      url,
      originalUrl: url,
      headers,
      httpVersion: '1.1',
      socket: { remoteAddress: '127.0.0.1', encrypted: false },
    })
    req.connection = req.socket
    const res = new FakeResponse((r) => {
      const out = []
      for (const [k, v] of r.headerMap.values()) {
        for (const one of Array.isArray(v) ? v : [v]) out.push([k, String(one)])
      }
      resolve({ status: r.statusCode, headers: out, body: Buffer.concat(r.chunks).toString('base64') })
    })
    let i = 0
    const next = (err) => {
      if (res.writableEnded) return
      while (i < stack.length) {
        const m = stack[i++]
        const pathOnly = url.split('?')[0]
        if (m.route && pathOnly !== m.route && !pathOnly.startsWith(m.route + '/')) continue
        const takesError = m.fn.length === 4
        if (!!err !== takesError) continue
        const saved = req.url
        if (m.route) req.url = url.slice(m.route.length) || '/'
        if (!req.url.startsWith('/')) req.url = '/' + req.url
        const done = (e) => {
          req.url = saved
          next(e)
        }
        try {
          const ret = err ? m.fn(err, req, res, done) : m.fn(req, res, done)
          if (ret && typeof ret.catch === 'function') ret.catch(done)
        } catch (e) {
          done(e)
        }
        return
      }
      if (err) {
        resolve({ status: 500, headers: [['content-type', 'text/plain']], body: Buffer.from(String(err?.stack ?? err)).toString('base64') })
      } else resolve(null)
    }
    next()
  })
}

// ---- startup ----

async function start() {
  let userConfig = (await import(pathToFileURL(configFile).href)).default
  if (typeof userConfig === 'function') userConfig = await userConfig(env)
  userConfig = (await userConfig) ?? {}
  config = { ...userConfig, plugins: undefined }
  all = await flatten(userConfig.plugins)
  all = all.filter((p) => {
    if (p.apply === 'build') return false
    if (typeof p.apply === 'function') return p.apply(userConfig, env)
    return true
  })
  all = [
    ...all.filter((p) => p.enforce === 'pre'),
    ...all.filter((p) => p.enforce !== 'pre' && p.enforce !== 'post'),
    ...all.filter((p) => p.enforce === 'post'),
  ]
  for (const h of sorted(all, 'config')) {
    try {
      const r = await h.fn.call(context(h.plugin), config, env)
      if (r) config = merge(config, r)
    } catch (e) {
      throw new Error(hookError(h.plugin, e))
    }
  }
  active = all.filter((p) => !isReplaced(p))
  const base = config.base ?? '/'
  resolved = {
    ...config,
    root,
    base,
    command: 'serve',
    mode,
    isProduction: false,
    isWorker: false,
    configFile,
    env: { BASE_URL: base, MODE: mode, DEV: true, PROD: false, SSR: false },
    plugins: all,
    server: config.server ?? {},
    preview: config.preview ?? {},
    build: config.build ?? {},
    resolve: config.resolve ?? {},
    optimizeDeps: config.optimizeDeps ?? {},
    experimental: config.experimental ?? {},
    css: config.css ?? {},
    logger,
    createResolver: () => async (id, importer) => (await builtinResolve(id, importer))?.[0],
  }
  const alias = config.resolve?.alias
  aliases = Array.isArray(alias)
    ? alias.filter((a) => a && a.find != null)
    : Object.entries(alias ?? {}).map(([find, replacement]) => ({ find, replacement }))
  for (const h of sorted(active, 'configResolved')) {
    try {
      await h.fn.call(context(h.plugin), resolved)
    } catch (e) {
      throw new Error(hookError(h.plugin, e))
    }
  }
  const post = []
  for (const h of sorted(active, 'configureServer')) {
    try {
      const after = await h.fn.call(context(h.plugin), server)
      if (typeof after === 'function') post.push(after)
    } catch (e) {
      throw new Error(hookError(h.plugin, e))
    }
  }
  for (const after of post) await after()
  // `options` (vite: packages/vite/src/node/server/pluginContainer.ts, run once before the
  // build starts): a hook may return new input options, and may change other hooks, such
  // as their filters, which are read again on every call.
  let inputOptions = config.build?.rolldownOptions ?? config.build?.rollupOptions ?? {}
  for (const h of sorted(active, 'options')) {
    try {
      inputOptions = (await h.fn.call(context(h.plugin), inputOptions)) || inputOptions
    } catch (e) {
      throw new Error(hookError(h.plugin, e))
    }
  }
  // buildStart is a parallel hook: handlers start together, except a `sequential: true`
  // one, which waits for those before it and finishes before the next start.
  let batch = []
  const flush = async () => {
    await Promise.all(batch)
    batch = []
  }
  for (const h of sorted(active, 'buildStart')) {
    const run = async () => {
      try {
        await h.fn.call(context(h.plugin), {})
      } catch (e) {
        throw new Error(hookError(h.plugin, e))
      }
    }
    if (h.plugin.buildStart?.sequential) {
      await flush()
      await run()
    } else batch.push(run())
  }
  await flush()
  const stages = (name) => new Set(sorted(active, name).map(stageOf))
  const resolveStages = stages('resolveId')
  const transformStages = stages('transform')
  const watching = ['change', 'add', 'unlink', 'all'].some((e) => watcher.listenerCount(e) > 0)
  const dev = devOptions()
  const jsx = config.oxc?.jsx ?? {}
  const esb = config.esbuild ?? {}
  const importSource = jsx.importSource ?? esb.jsxImportSource ?? null
  const runtime = jsx.runtime ?? (esb.jsx === 'transform' ? 'classic' : esb.jsx === 'automatic' ? 'automatic' : null)
  return {
    plugins: active.map((p) => p.name ?? 'unnamed'),
    replaced: all.filter(isReplaced).map((p) => p.name),
    hooks: {
      resolve_pre: resolveStages.has('pre') || aliases.length > 0,
      resolve_post: resolveStages.has('post'),
      load: sorted(active, 'load').length > 0,
      transform_pre: transformStages.has('pre'),
      transform_post: transformStages.has('post'),
      index_html: sorted(active, 'transformIndexHtml').length > 0,
      hot_update: sorted(active, 'handleHotUpdate').length + sorted(active, 'hotUpdate').length > 0 || watching,
      middleware: stack.length > 0,
      client_events: wsListeners.size > 0,
    },
    jsx: { importSource, runtime },
    optimizeInclude: [...new Set((config.optimizeDeps?.include ?? []).filter((s) => typeof s === 'string'))],
    dev,
    warnings,
  }
}

const ops = {
  resolve: opResolve,
  load: opLoad,
  transform: opTransform,
  html: opHtml,
  hot: opHot,
  middleware: opMiddleware,
  proxyRewrite: opProxyRewrite,
}

function handle(msg) {
  if (msg.op === 'event') {
    for (const cb of wsListeners.get(msg.event) ?? []) {
      try {
        cb(msg.data, { send: (p, d) => ws.send(p, d) })
      } catch (e) {
        console.error(`[ts-r-dev] ws listener for ${msg.event} failed: ${e?.message ?? e}`)
      }
    }
    return
  }
  const op = ops[msg.op]
  if (!op) {
    send({ id: msg.id, error: `unknown op ${msg.op}` })
    return
  }
  Promise.resolve()
    .then(() => op(msg))
    .then(
      (ok) => send({ id: msg.id, ok: ok ?? null }),
      (e) => send({ id: msg.id, error: e?.message ?? String(e) }),
    )
}

// Read stdin before the startup hooks run: a `buildStart` awaiting `this.resolve` needs its
// resolver reply. Other requests wait until the hooks are done.
const early = []
const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })
lines.on('line', (line) => {
  let msg
  try {
    msg = JSON.parse(line)
  } catch {
    return
  }
  if (msg.rid != null) {
    const cb = callbacks.get(msg.rid)
    callbacks.delete(msg.rid)
    cb?.(msg.result ?? null)
    return
  }
  if (ready) handle(msg)
  else early.push(msg)
})
// ts-r-dev exited (or closed the pipe): so do we.
lines.on('close', () => process.exit(0))

try {
  const manifest = await start()
  ready = true
  send({ ready: manifest })
} catch (e) {
  send({ fatal: e?.message ?? String(e) })
  process.exit(1)
}
for (const msg of early.splice(0)) handle(msg)
