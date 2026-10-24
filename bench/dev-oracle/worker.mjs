// Runs one playground spec file against one dev server, the way Vite's playground setup does
// per spec file: start the server on the copied playground, open a page in headless Chrome,
// go to the server's URL, then run the spec's tests in order. Writes one JSON result file and
// stops everything it started.
//
// node worker.mjs --spec <file> --server vite|ts-r --dir <copied playground> --port <n>
//   --out <json> --chrome <path> --tsr <ts-r-dev> --cache-dir <dir>

import './shim/hooks.mjs'
import { spawn, spawnSync } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import { pathToFileURL } from 'node:url'
import { Unsupported, launchChrome, newPage } from './cdp.mjs'
import { __root, expect } from './shim/vitest.mjs'
import * as utils from './shim/utils.mjs'

const here = import.meta.dirname
const repo = path.resolve(here, '../..')
const limit = path.join(repo, 'scripts/limit.sh')
const args = {}
for (let i = 2; i < process.argv.length; i += 2) args[process.argv[i].replace(/^--/, '')] = process.argv[i + 1]

/** Vite's playground config gives each test 20 s. */
const TEST_TIMEOUT = 20000
const HOOK_TIMEOUT = 60000
const SERVER_START_TIMEOUT = 60000

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
const result = { spec: args.spec, server: args.server, url: null, error: null, tests: [], serverLog: '' }
const write = () => fs.writeFileSync(args.out, JSON.stringify(result, null, 2) + '\n')

function withTimeout(p, ms, what) {
  let t
  return Promise.race([
    Promise.resolve(p),
    new Promise((_, rej) => (t = setTimeout(() => rej(new Error(`${what} timed out after ${ms} ms`)), ms))),
  ]).finally(() => clearTimeout(t))
}

function startServer(testDir, rootDir) {
  const port = Number(args.port)
  const cmd =
    args.server === 'vite'
      ? [process.execPath, path.join(here, 'vite-serve.mjs'), rootDir, String(port)]
      : [args.tsr, rootDir, '--port', String(port), '--strict-port', '--cache-dir', args['cache-dir']]
  const child = spawn(limit, ['--mem-mb', '1024', '--time-s', '1200', '--', ...cmd], {
    cwd: testDir,
    env: { ...process.env, TS_R_CACHE_DIR: args['cache-dir'], VITE_INLINE: 'inline-serve', NO_COLOR: '1' },
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  let log = ''
  const onData = (d) => (log = (log + d).slice(-20000))
  child.stdout.on('data', onData)
  child.stderr.on('data', onData)
  const ready = new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(`server did not start in ${SERVER_START_TIMEOUT} ms: ${log.slice(-2000)}`)), SERVER_START_TIMEOUT)
    const check = () => {
      const m = log.match(/Local:\s+(\S+)/)
      if (m) {
        clearTimeout(t)
        resolve(m[1])
      }
    }
    child.stdout.on('data', check)
    child.on('exit', (code) => {
      clearTimeout(t)
      reject(new Error(`server exited (${code}): ${log.slice(-2000)}`))
    })
  })
  const stop = async () => {
    child.kill('SIGTERM')
    for (let i = 0; i < 60 && child.exitCode === null && child.signalCode === null; i++) await sleep(50)
    // Anything still listening on the port escaped the group: kill it.
    const pids = spawnSync('lsof', ['-ti', `tcp:${port}`, '-sTCP:LISTEN'], { encoding: 'utf8' }).stdout.trim()
    for (const pid of pids.split('\n').filter(Boolean)) {
      try {
        process.kill(Number(pid), 'SIGKILL')
      } catch {}
    }
  }
  return { ready, stop, log: () => log }
}

/** Resolves every CSS named color to its computed rgb(), like test-utils' hex-to-name map. */
async function colorNames(page) {
  return page.evaluate((names) => {
    const el = document.createElement('div')
    document.body.appendChild(el)
    const map = {}
    for (const n of names) {
      el.style.color = n
      map[getComputedStyle(el).color.replace(/\s+/g, '')] = n
    }
    el.remove()
    return map
  }, utils.CSS_COLOR_NAMES)
}

/** Tests that assert on Vite's terminal output or its server object can't run against ts-r-dev. */
function staticSkip(fn) {
  const src = String(fn)
  if (/\bserverLogs\b/.test(src)) return "asserts on the dev server's terminal output"
  if (/\bviteServer\b/.test(src)) return "uses Vite's server object"
  return null
}

const seen = new Map()
function record(id, status, reason, ms) {
  const n = (seen.get(id) ?? 0) + 1
  seen.set(id, n)
  result.tests.push({ id: n > 1 ? `${id} (${n})` : id, status, ...(reason ? { reason } : {}), ...(ms != null ? { ms } : {}) })
}
const message = (e) => String(e?.stack ?? e).split('\n').slice(0, 6).join('\n').slice(0, 1500)
const outcome = (e) => (e instanceof Unsupported || e?.unsupported ? ['skip', e.message] : ['fail', message(e)])

function allTests(suite, prefix, status, reason) {
  for (const c of suite.children) {
    const id = prefix ? `${prefix} > ${c.name}` : c.name
    if (c.kind === 'suite') allTests(c, id, status, reason)
    else record(id, status, reason)
  }
}

async function runSuite(suite, prefix, each) {
  if (suite.skip) return allTests(suite, prefix, 'skip', suite.skip)
  const hooks = { before: [...each.before, ...suite.beforeEach], after: [...suite.afterEach, ...each.after] }
  const teardowns = []
  for (const h of suite.beforeAll) {
    try {
      const r = await withTimeout(h(), HOOK_TIMEOUT, 'beforeAll')
      if (typeof r === 'function') teardowns.push(r)
    } catch (e) {
      const [status, reason] = outcome(e)
      return allTests(suite, prefix, status, `beforeAll: ${reason}`)
    }
  }
  for (const c of suite.children) {
    const id = prefix ? `${prefix} > ${c.name}` : c.name
    if (c.kind === 'suite') {
      await runSuite(c, id, hooks)
      continue
    }
    const skip = c.skip ?? staticSkip(c.fn)
    if (skip) {
      record(id, 'skip', skip)
      continue
    }
    const start = Date.now()
    try {
      for (const h of hooks.before) await withTimeout(h(), HOOK_TIMEOUT, 'beforeEach')
      await withTimeout(c.fn({ expect }), c.timeout ?? TEST_TIMEOUT, 'test')
      for (const h of hooks.after) await withTimeout(h(), HOOK_TIMEOUT, 'afterEach')
      record(id, 'pass', null, Date.now() - start)
    } catch (e) {
      const [status, reason] = outcome(e)
      record(id, status, status === 'fail' ? `${reason}\n(page: ${utils.page?.url()})` : reason, Date.now() - start)
    }
  }
  for (const h of [...suite.afterAll, ...teardowns.reverse()]) {
    await withTimeout(h(), HOOK_TIMEOUT, 'afterAll').catch(() => {})
  }
}

const testDir = args.dir
const rootDir = fs.existsSync(path.join(testDir, 'root')) ? path.join(testDir, 'root') : testDir
let server
let chrome
// vitest reports a stray rejection as an error and carries on; so does the oracle.
process.on('unhandledRejection', () => {})
process.on('SIGTERM', () => {
  result.error ??= 'worker timed out'
  cleanup()
})
try {
  // Collect first, as vitest does: the spec's top level runs before the server starts.
  try {
    await import(pathToFileURL(args.spec).href)
  } catch (e) {
    result.error = `spec does not load: ${message(e)}`
    throw e
  }
  server = startServer(testDir, rootDir)
  let url = await server.ready
  // vitestSetup strips the trailing slash when base is '/'.
  if (new URL(url).pathname === '/') url = url.replace(/\/$/, '')
  result.url = url
  chrome = await launchChrome({ chromePath: args.chrome, limit, dir: path.join(args['cache-dir'], `chrome-${args.port}`) })
  const mapText = (t) => t.replace(/^\[ts-r\]/, '[vite]')
  const page = await newPage(chrome, { mapText })
  page.on('console', (msg) => utils.browserLogs.push(msg.text()))
  page.on('pageerror', (e) => utils.browserErrors.push(e))
  await page.goto('about:blank')
  const names = await colorNames(page)
  const browser = { newPage: () => newPage(chrome, { mapText }), close: async () => {}, contexts: () => [] }
  utils.__set({ page, browser, testDir, rootDir, testPath: args.spec, testName: path.basename(testDir), viteTestUrl: url, colorNames: names })
  // vitestSetup's beforeAll: load the app before the spec's own hooks.
  __root.beforeAll.unshift(() => page.goto(url))
  await runSuite(__root, '', { before: [], after: [] })
} catch (e) {
  result.error ??= message(e)
} finally {
  await cleanup()
}

async function cleanup() {
  result.serverLog = server?.log().slice(-4000) ?? ''
  write()
  await chrome?.kill().catch(() => {})
  await server?.stop().catch(() => {})
  process.exit(0)
}
