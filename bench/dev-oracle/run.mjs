// The ts-r-dev oracle: runs Vite's playground e2e specs (target/upstream/vite/playground,
// synced by `cargo xtask upstream sync`) in headless Chrome against Vite's dev server and
// against ts-r-dev. A test is compared only when it passes against Vite.
//
// scripts/limit.sh --mem-mb 4096 --time-s 3600 -- node bench/dev-oracle/run.mjs \
//   [--playgrounds hmr,assets] [--server both|vite|ts-r] [--jobs 2] [--tsr target/release/ts-r-dev]
//   [--out target/dev-oracle/results.json] [--list]

import { spawn, spawnSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

const here = import.meta.dirname
const repo = path.resolve(here, '../..')
const bench = path.join(repo, 'bench')
const playground = path.join(repo, 'target/upstream/vite/playground')
const outDir = path.join(repo, 'target/dev-oracle')

const opts = { server: 'both', jobs: '2', tsr: path.join(repo, 'target/release/ts-r-dev'), out: path.join(outDir, 'results.json') }
for (let i = 2; i < process.argv.length; i++) {
  const k = process.argv[i].replace(/^--/, '')
  if (k === 'list' || k === 'keep') opts[k] = true
  else opts[k] = process.argv[++i]
}
const jobs = Math.min(2, Number(opts.jobs) || 1)
const servers = opts.server === 'both' ? ['vite', 'ts-r'] : [opts.server]
const chrome = process.env.TS_R_CHROME ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
if (!fs.existsSync(playground)) {
  console.error('missing target/upstream/vite/playground: run `cargo xtask upstream sync vite`')
  process.exit(1)
}
if (!fs.existsSync(path.join(bench, 'node_modules/vite'))) {
  console.error('missing bench/node_modules: run `npm ci` in bench/')
  process.exit(1)
}

// ---------------------------------------------------------------------------------------
// Which playgrounds run, and why the others don't.

/** Playgrounds skipped as a whole, with the reason. */
const SKIP = {
  'hmr-ssr': 'SSR (no browser page)',
  'ssr-resolve': 'SSR (no browser page)',
  'ssr-alias': 'SSR (no browser page)',
  'ssr-webworker': 'SSR (no browser page)',
  'legacy': 'build only (@vitejs/plugin-legacy)',
  'environment-react-ssr': 'SSR (no browser page)',
  'optimize-missing-deps': 'custom server',
  'backend-integration': 'custom server (backend HTML)',
  'hmr-full-bundle-mode': "Vite's experimental full-bundle dev mode (experimental.bundledDev)",
}

const exists = (p) => fs.existsSync(p)
const readJson = (p) => {
  try {
    return JSON.parse(fs.readFileSync(p, 'utf8').replace(/^\uFEFF/, ''))
  } catch {
    return {}
  }
}

/** Every package directory under `dir` by name, for workspace:* dependencies. */
function workspacePackages(dir, out = new Map(), depth = 0) {
  if (depth > 4) return out
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    if (!e.isDirectory() || e.name === 'node_modules' || e.name.startsWith('.')) continue
    const sub = path.join(dir, e.name)
    const pj = path.join(sub, 'package.json')
    if (exists(pj)) {
      const name = readJson(pj).name
      if (name && !out.has(name)) out.set(name, sub)
    }
    workspacePackages(sub, out, depth + 1)
  }
  return out
}
let workspace

/** Missing npm packages for a package directory (and its local deps, recursively). */
function missingDeps(pkgDir, seen = new Set()) {
  const pj = path.join(pkgDir, 'package.json')
  if (seen.has(pkgDir) || !exists(pj)) return []
  seen.add(pkgDir)
  const p = readJson(pj)
  const missing = []
  for (const [name, spec] of Object.entries({ ...p.dependencies, ...p.devDependencies })) {
    if (spec.startsWith('file:') || spec.startsWith('link:')) {
      missing.push(...missingDeps(path.resolve(pkgDir, spec.slice(5)), seen))
    } else if (spec.startsWith('workspace:')) {
      workspace ??= workspacePackages(playground)
      if (workspace.has(name)) missing.push(...missingDeps(workspace.get(name), seen))
      else if (!exists(path.join(bench, 'node_modules', name))) missing.push(name)
    } else if (!exists(path.join(bench, 'node_modules', name))) missing.push(name)
  }
  return [...new Set(missing)]
}

function playgrounds() {
  const out = []
  for (const name of fs.readdirSync(playground).sort()) {
    const tests = path.join(playground, name, '__tests__')
    if (!exists(tests)) continue
    const hasServe = (d) => exists(path.join(d, 'serve.ts')) || exists(path.join(d, 'serve.js'))
    const specs = fs.readdirSync(tests).filter((f) => f.endsWith('.spec.ts')).sort()
    // Variant specs (`__tests__/<variant>/*.spec.ts`) run with `vite.config-<variant>.js`, as
    // vitestSetup's loadConfig does; a variant with its own serve.ts starts a custom server.
    const variants = fs.readdirSync(tests, { withFileTypes: true }).filter((e) => e.isDirectory()).map((e) => e.name).sort()
    for (const v of variants) {
      if (hasServe(path.join(tests, v))) continue
      for (const f of fs.readdirSync(path.join(tests, v)).filter((f) => f.endsWith('.spec.ts')).sort()) specs.push(`${v}/${f}`)
    }
    let skip = SKIP[name] ?? null
    if (!skip && hasServe(tests)) skip = 'custom server (__tests__/serve.ts)'
    if (!skip && name.startsWith('ssr')) skip = 'SSR (no browser page)'
    if (!skip && specs.length === 0) skip = `only variant specs with a custom server (${variants.join(', ')})`
    if (!skip) {
      const missing = missingDeps(path.join(playground, name))
      if (missing.length) skip = `npm packages not in bench/node_modules: ${missing.join(', ')}`
    }
    out.push({ name, specs, variants, skip })
  }
  return out
}

// ---------------------------------------------------------------------------------------
// Work copies: the playground copied (as vitest's global setup copies it to playground-temp)
// with its local packages installed the way pnpm does (file: copied, link: and workspace:
// symlinked). npm packages resolve from bench/node_modules through a symlink above it.

function installDeps(pkgDir, root, depth = 0) {
  const pj = path.join(pkgDir, 'package.json')
  if (depth > 6 || !exists(pj)) return
  const p = readJson(pj)
  for (const [name, spec] of Object.entries({ ...p.dependencies, ...p.devDependencies })) {
    const dest = path.join(pkgDir, 'node_modules', name)
    if (exists(dest)) continue
    let target
    if (spec.startsWith('file:') || spec.startsWith('link:')) target = path.resolve(pkgDir, spec.slice(5))
    else if (spec.startsWith('workspace:')) {
      workspace ??= workspacePackages(playground)
      const src = workspace.get(name)
      if (src) target = path.join(root, path.relative(playground, src))
    }
    if (!target || !exists(target)) continue
    fs.mkdirSync(path.dirname(dest), { recursive: true })
    if (spec.startsWith('file:')) {
      fs.cpSync(target, dest, { recursive: true, dereference: false, filter: (s) => !s.includes(`${path.sep}node_modules${path.sep}`) })
      installDeps(dest, root, depth + 1)
    } else {
      fs.symlinkSync(target, dest, 'dir')
      installDeps(target, root, depth + 1)
    }
  }
}

function prepare(server, name) {
  const root = path.join(outDir, 'work', server)
  fs.mkdirSync(root, { recursive: true })
  for (const f of ['package.json', 'tsconfig.json', 'shims.d.ts']) fs.copyFileSync(path.join(playground, f), path.join(root, f))
  const dir = path.join(root, name)
  fs.rmSync(dir, { recursive: true, force: true })
  fs.cpSync(path.join(playground, name), dir, { recursive: true })
  installDeps(dir, root)
  return dir
}

// ---------------------------------------------------------------------------------------
// Running

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
const WORKER_TIMEOUT = 900_000

function runWorker(spec, server, dir, port, out) {
  const cacheDir = path.join(outDir, 'cache', `${server}-${port}`)
  fs.mkdirSync(cacheDir, { recursive: true })
  return new Promise((resolve) => {
    const child = spawn(process.execPath, ['--disable-warning=ExperimentalWarning', path.join(here, 'worker.mjs'),
      '--spec', spec, '--server', server, '--dir', dir, '--port', String(port), '--out', out,
      '--chrome', chrome, '--tsr', opts.tsr, '--cache-dir', cacheDir], {
      stdio: ['ignore', 'inherit', 'inherit'],
      env: { ...process.env, RAYON_NUM_THREADS: '4' },
    })
    const t = setTimeout(() => child.kill('SIGTERM'), WORKER_TIMEOUT)
    child.on('exit', () => {
      clearTimeout(t)
      resolve()
    })
  })
}

const read = (f) => (exists(f) ? readJson(f) : { error: 'worker wrote no result', tests: [] })

/** Compares one spec's two runs: only tests that pass against Vite count. */
function compare(vite, tsr) {
  const t = new Map((tsr?.tests ?? []).map((x) => [x.id, x]))
  return vite.tests.map(({ id, status, reason }) => {
    if (vite.error && !vite.tests.length) return { id, result: 'skip', reason: `Vite run failed: ${vite.error}` }
    if (status === 'skip') return { id, result: 'skip', reason }
    if (status === 'fail') return { id, result: 'skip', reason: `fails against Vite in the oracle: ${reason}` }
    if (!tsr) return { id, result: 'vite-only' }
    const r = t.get(id)
    if (!r) return { id, result: 'fail', reason: `no ts-r-dev result${tsr.error ? `: ${tsr.error}` : ''}` }
    if (r.status === 'pass') return { id, result: 'pass' }
    if (r.status === 'skip') return { id, result: 'skip', reason: `shim, ts-r-dev only: ${r.reason}` }
    return { id, result: 'fail', reason: r.reason }
  })
}

async function runPlayground(pg, slot) {
  const runs = {}
  for (const server of servers) {
    let dir = prepare(server, pg.name)
    runs[server] = {}
    for (const [i, spec] of pg.specs.entries()) {
      // A variant spec gets a fresh copy: the specs before it may have edited files.
      if (spec.includes('/') && i > 0) dir = prepare(server, pg.name)
      const port = 15300 + slot * 20 + i * 2 + (server === 'vite' ? 0 : 1)
      const out = path.join(outDir, 'runs', `${pg.name}.${spec.replaceAll('/', '__')}.${server}.json`)
      fs.mkdirSync(path.dirname(out), { recursive: true })
      fs.rmSync(out, { force: true })
      const start = Date.now()
      await runWorker(path.join(dir, '__tests__', spec), server, dir, port, out)
      runs[server][spec] = read(out)
      const r = runs[server][spec]
      const n = (s) => r.tests.filter((x) => x.status === s).length
      console.log(`${pg.name}/${spec} ${server}: ${n('pass')} pass, ${n('fail')} fail, ${n('skip')} skip${r.error ? ` (error: ${r.error.split('\n')[0].slice(0, 160)})` : ''} in ${((Date.now() - start) / 1000).toFixed(1)} s`)
    }
  }
  const specs = {}
  for (const spec of pg.specs) {
    const vite = runs.vite?.[spec]
    specs[spec] = vite ? compare(vite, runs['ts-r']?.[spec]) : (runs['ts-r'][spec].tests.map(({ id, status, reason }) => ({ id, result: status, reason })))
  }
  return { specs, errors: Object.fromEntries(Object.entries(runs).map(([s, r]) => [s, Object.fromEntries(Object.entries(r).filter(([, x]) => x.error).map(([k, x]) => [k, x.error]))])) }
}

const all = playgrounds()
if (opts.list) {
  for (const p of all) console.log(`${p.name.padEnd(28)} ${p.skip ? `skip: ${p.skip}` : p.specs.join(' ')}`)
  process.exit(0)
}
const wanted = opts.playgrounds ? new Set(opts.playgrounds.split(',')) : null
const todo = all.filter((p) => !p.skip && (!wanted || wanted.has(p.name)))
fs.rmSync(path.join(outDir, 'node_modules'), { force: true, recursive: true })
fs.mkdirSync(outDir, { recursive: true })
fs.symlinkSync(path.join(bench, 'node_modules'), path.join(outDir, 'node_modules'), 'dir')

const results = {}
let next = 0
await Promise.all(
  Array.from({ length: jobs }, async (_, slot) => {
    while (next < todo.length) {
      const pg = todo[next++]
      results[pg.name] = await runPlayground(pg, slot)
    }
  }),
)

// Anything a worker left behind: Chrome by its profile directory, servers by port.
spawnSync('pkill', ['-f', path.join(outDir, 'cache')])
await sleep(200)

const sum = (list, r) => list.filter((x) => x.result === r).length
const summary = { playgrounds: {}, total: { pass: 0, compared: 0, skip: 0 } }
for (const p of all) {
  if (wanted && !wanted.has(p.name)) continue
  if (p.skip) {
    summary.playgrounds[p.name] = { skip: p.skip }
    continue
  }
  const tests = Object.values(results[p.name].specs).flat()
  const row = { pass: sum(tests, 'pass'), compared: sum(tests, 'pass') + sum(tests, 'fail'), skip: sum(tests, 'skip') }
  summary.playgrounds[p.name] = row
  for (const k of ['pass', 'compared', 'skip']) summary.total[k] += row[k]
}
const version = (pkg) => readJson(path.join(bench, 'node_modules', pkg, 'package.json')).version
const meta = {
  commit: spawnSync('git', ['rev-parse', '--short', 'HEAD'], { cwd: repo, encoding: 'utf8' }).stdout.trim(),
  date: new Date().toISOString().slice(0, 10),
  machine: `${os.cpus()[0]?.model ?? os.arch()}, ${os.platform()} ${os.release()}`,
  node: process.version,
  vite: version('vite'),
  chrome: spawnSync(chrome, ['--version'], { encoding: 'utf8' }).stdout.trim(),
  servers,
}
const old = exists(opts.out) && wanted ? readJson(opts.out) : null
const merged = old
  ? { meta, summary: { playgrounds: { ...old.summary.playgrounds, ...summary.playgrounds } }, results: { ...old.results, ...results } }
  : { meta, summary, results }
if (old) {
  merged.summary.total = { pass: 0, compared: 0, skip: 0 }
  // A skipped playground's row is { skip: reason }: count only the numbers.
  for (const row of Object.values(merged.summary.playgrounds)) {
    for (const k of ['pass', 'compared', 'skip']) if (typeof row[k] === 'number') merged.summary.total[k] += row[k]
  }
}
fs.writeFileSync(opts.out, JSON.stringify(merged, null, 2) + '\n')
console.log('\nplayground                    pass / compared   skip')
for (const [name, row] of Object.entries(merged.summary.playgrounds)) {
  console.log(`${name.padEnd(28)} ${row.skip && typeof row.skip === 'string' ? `skip: ${row.skip}` : `${String(row.pass).padStart(4)} / ${String(row.compared).padEnd(8)} ${row.skip}`}`)
}
const t = merged.summary.total
console.log(`\ntotal: ${t.pass} of ${t.compared} compared tests pass (${t.compared ? ((100 * t.pass) / t.compared).toFixed(1) : '0'}%), ${t.skip} skipped`)
if (!opts.keep) fs.rmSync(path.join(outDir, 'work'), { recursive: true, force: true })
