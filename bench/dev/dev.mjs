// Dev-server benchmark on a public app: ts-r-dev against Vite 8.3.4 + @vitejs/plugin-react
// 6.1.2 (pinned in bench/package.json), in headless Chrome driven over the DevTools protocol
// (Node's built-in WebSocket, no other dependencies).
//
// Apps are pinned in bench/repos.toml (`scenarios` lists `dev`) and fetched with
// `cargo xtask bench --fetch-repos --repos <app>`, which also copies bench/dev/<app>/ (entry,
// index.html, Vite config) to ts-r-bench-dev/ in the checkout. Both servers serve that
// directory with the same Vite config.
//
//   check: the page renders; editing the app's component hot-updates without a reload, and
//          React keeps the component (Fast Refresh: its DOM node survives the update).
//   bench: cold start to first render, warm start, save-to-update p50/p95 over N edits,
//          dev-server memory; the same steps for both servers.
//
// Usage (from the repo root):
//
//   scripts/limit.sh --mem-mb 1024 --time-s 1800 -- node bench/dev/dev.mjs --app excalidraw \
//     [--server ts-r|vite|both] [--mode check|bench] [--edits 50] [--runs 5] \
//     [--tools bench] [--tsr target/release/ts-r-dev] [--out results.json]
//
// --tools is a directory whose node_modules has vite and @vitejs/plugin-react (default bench/,
// installed by `cd bench && npm ci`). Each server and Chrome run under scripts/limit.sh
// (2048 MB) and are killed (process group) when the script ends. The edited file is restored.

import { spawn, spawnSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..')

/** Per-app settings. Paths are relative to the checkout. */
const APPS = {
  excalidraw: {
    checkout: 'target/bench-repos/excalidraw',
    // A plain function component on the empty canvas: the welcome screen's heading.
    edit: 'packages/excalidraw/components/welcome-screen/WelcomeScreen.Center.tsx',
    anchor: 'className="welcome-screen-center__heading welcome-screen-decor excalifont">',
    replace: (want) => `className="welcome-screen-center__heading welcome-screen-decor excalifont">{${JSON.stringify(want)}}`,
    // First render: the editor and the welcome screen are in the page.
    ready: `!!(document.querySelector('.excalidraw') && document.querySelector('.welcome-screen-center__heading'))`,
    // The edited component's DOM: React reuses it when Fast Refresh keeps the component.
    kept: '.welcome-screen-center__heading',
  },
}

const argv = process.argv.slice(2)
const opt = (name, def) => {
  const i = argv.indexOf(`--${name}`)
  return i >= 0 ? argv[i + 1] : def
}
const appName = opt('app', 'excalidraw')
const profile = APPS[appName]
if (!profile) {
  console.error(`unknown --app ${appName} (known: ${Object.keys(APPS).join(', ')})`)
  process.exit(2)
}
const checkout = path.join(repo, profile.checkout)
const root = path.join(checkout, 'ts-r-bench-dev')
const which = opt('server', 'ts-r')
const mode = opt('mode', 'check')
const edits = Number(opt('edits', '50'))
const runs = Number(opt('runs', '5'))
const tools = path.resolve(opt('tools', process.env.TSR_VITE_TOOLS ?? path.join(repo, 'bench')))
const tsr = path.resolve(opt('tsr', path.join(repo, 'target/release/ts-r-dev')))
const outFile = opt('out', '')
const chromePath = process.env.TS_R_CHROME ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
const limit = path.join(repo, 'scripts/limit.sh')
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'tsr-dev-bench-'))
const EDIT = path.join(checkout, profile.edit)
const threadsEnv = { RAYON_NUM_THREADS: '4', TS_R_THREADS: '4', UV_THREADPOOL_SIZE: '4' }

for (const [what, p] of [
  [`${appName} checkout with dependencies (cargo xtask bench --fetch-repos --repos ${appName})`, path.join(checkout, 'node_modules')],
  ['the harness (cargo xtask bench --fetch-repos copies bench/dev/<app>/)', path.join(root, 'vite.config.mjs')],
  ['vite in --tools (cd bench && npm ci)', path.join(tools, 'node_modules/vite/bin/vite.js')],
  ['ts-r-dev (cargo build --release -p ts_r_dev)', tsr],
]) {
  if (!fs.existsSync(p)) {
    console.error(`missing ${what}: ${p}`)
    process.exit(2)
  }
}
const original = fs.readFileSync(EDIT, 'utf8')
if (!original.includes(profile.anchor)) {
  console.error(`${EDIT} no longer contains ${profile.anchor}`)
  process.exit(2)
}

const now = () => performance.timeOrigin + performance.now()
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
const killers = []
const cleanup = () => {
  for (const k of killers.splice(0)) k()
  try {
    fs.writeFileSync(EDIT, original)
  } catch {}
  fs.rmSync(scratch, { recursive: true, force: true })
}
process.on('exit', cleanup)
for (const sig of ['SIGINT', 'SIGTERM']) process.on(sig, () => process.exit(130))

// ---------------------------------------------------------------------------------------
// Processes

const pgidOf = (pid) =>
  Number(spawnSync('ps', ['-o', 'pgid=', '-p', String(pid)], { encoding: 'utf8' }).stdout.trim()) || null
const ownGroup = pgidOf(process.pid)

/** Process-group id of the process listening on `port` (never this script's own group). */
function listenerGroup(port) {
  const r = spawnSync('lsof', ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN', '-t'], { encoding: 'utf8' })
  const pid = Number(r.stdout.trim().split('\n')[0])
  if (!pid) return null
  const g = pgidOf(pid)
  return g && g !== ownGroup ? g : null
}

/** Total RSS (MB) of a process group. */
function groupRssMb(pgid) {
  const r = spawnSync('ps', ['-axo', 'pgid=,rss='], { encoding: 'utf8' })
  let kb = 0
  for (const line of r.stdout.split('\n')) {
    const [g, rss] = line.trim().split(/\s+/).map(Number)
    if (g === pgid) kb += rss
  }
  return kb / 1024
}

/** Starts `cmd` under limit.sh; resolves when a `Local: <url>` line is printed. */
function startServer(name, cmd, args, env, port) {
  const t0 = now()
  const child = spawn(limit, ['--mem-mb', '2048', '--time-s', '900', '--', cmd, ...args], {
    cwd: root,
    env: { ...process.env, ...threadsEnv, ...env },
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  let log = ''
  let pgid = null
  const kill = () => {
    const g = pgid > 0 ? pgid : listenerGroup(port)
    if (g) {
      try {
        process.kill(-g, 'SIGKILL')
      } catch {}
    } else {
      spawnSync('pkill', ['-KILL', '-f', `--port ${port} `])
    }
    try {
      child.kill('SIGKILL')
    } catch {}
  }
  killers.push(kill)
  return new Promise((resolve, reject) => {
    const onData = (d) => {
      log += d
      const m = log.match(/Local:\s+(\S+)/)
      if (m && pgid === null) {
        pgid = listenerGroup(port) ?? -1
        resolve({ name, url: m[1].replace(/\/$/, ''), t0, child, kill, pgid, log: () => log })
      }
    }
    child.stdout.on('data', onData)
    child.stderr.on('data', onData)
    child.on('exit', (code) => reject(new Error(`${name} exited (${code}):\n${log}`)))
  })
}

const viteCache = path.join(checkout, 'node_modules/.vite-ts-r-bench')

function server(kind, { cold, port }) {
  const env = { TSR_VITE_TOOLS: tools }
  if (kind === 'ts-r') {
    const cacheDir = path.join(scratch, 'ts-r-cache')
    if (cold) fs.rmSync(cacheDir, { recursive: true, force: true })
    return startServer('ts-r-dev', tsr, ['.', '--port', String(port), '--strict-port', '--cache-dir', cacheDir], env, port)
  }
  if (cold) fs.rmSync(viteCache, { recursive: true, force: true })
  return startServer(
    'vite',
    process.execPath,
    [path.join(tools, 'node_modules/vite/bin/vite.js'), '--config', 'vite.config.mjs', '--port', String(port), '--host', '127.0.0.1'],
    env,
    port,
  )
}

// ---------------------------------------------------------------------------------------
// Chrome over the DevTools protocol

// One Chrome per page load: a long-lived browser grows past its memory cap over the
// dozens of loads a bench run makes.
let chromeRuns = 0
async function launchChrome() {
  const dir = path.join(scratch, `chrome-${chromeRuns++}`)
  fs.mkdirSync(dir, { recursive: true })
  chromeExit = ''
  const child = spawn(limit, ['--mem-mb', '3072', '--time-s', '1500', '--', chromePath, '--headless=new',
    '--remote-debugging-port=0', `--user-data-dir=${dir}`, '--no-first-run', '--no-default-browser-check',
    '--disable-extensions', '--disable-background-networking', '--window-size=1280,800', 'about:blank'], {
    stdio: ['ignore', 'ignore', 'pipe'],
  })
  let chromeLog = ''
  child.stderr.on('data', (d) => {
    chromeLog = (chromeLog + d).slice(-4000)
  })
  let stopping = false
  child.on('exit', (code, signal) => {
    if (!stopping) chromeExit = `Chrome exited (${code ?? signal}):\n${chromeLog}`
  })
  const kill = () => {
    stopping = true
    const port = fs.existsSync(path.join(dir, 'DevToolsActivePort'))
      ? Number(fs.readFileSync(path.join(dir, 'DevToolsActivePort'), 'utf8').split('\n')[0])
      : 0
    const g = port ? listenerGroup(port) : null
    if (g) {
      try {
        process.kill(-g, 'SIGKILL')
      } catch {}
    }
    try {
      child.kill('SIGKILL')
    } catch {}
  }
  killers.push(kill)
  const file = path.join(dir, 'DevToolsActivePort')
  for (let i = 0; i < 200 && !fs.existsSync(file); i++) await sleep(50)
  const [port, wsPath] = fs.readFileSync(file, 'utf8').trim().split('\n')
  const browser = await connect(`ws://127.0.0.1:${port}${wsPath}`)
  browser.kill = () => {
    kill()
    browser.ws.close()
  }
  return browser
}

let chromeExit = ''

function connect(url) {
  const ws = new WebSocket(url)
  let next = 1
  const pending = new Map()
  const listeners = new Set()
  ws.addEventListener('message', (e) => {
    const msg = JSON.parse(e.data)
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id)
      pending.delete(msg.id)
      if (msg.error) reject(new Error(JSON.stringify(msg.error)))
      else resolve(msg.result)
    } else {
      for (const l of listeners) l(msg)
    }
  })
  // A dead browser fails every pending and later call instead of hanging the run.
  // The error waits a second so Chrome's exit (and limit.sh's reason) can arrive first.
  let closed = false
  const fail = (reject) =>
    setTimeout(() => reject(new Error(chromeExit || 'the DevTools connection closed')), 1000)
  ws.addEventListener('close', () => {
    closed = true
    for (const { reject } of pending.values()) fail(reject)
    pending.clear()
  })
  const send = (method, params = {}, sessionId) =>
    new Promise((resolve, reject) => {
      if (closed) return fail(reject)
      const id = next++
      pending.set(id, { resolve, reject })
      ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }))
    })
  return new Promise((resolve) => ws.addEventListener('open', () => resolve({ send, listeners, ws })))
}

/** Observer installed in every document: first render time and update detection. */
const PAGE_SCRIPT = `
window.__tsrBench = { firstRender: 0, want: sessionStorage.getItem('tsrWant'), seen: 0 };
const check = () => {
  const b = window.__tsrBench;
  if (!b.firstRender && (${profile.ready})) {
    b.firstRender = performance.timeOrigin + performance.now();
  }
  if (b.want && !b.seen && document.body && document.body.textContent.includes(b.want)) {
    b.seen = performance.timeOrigin + performance.now();
  }
};
new MutationObserver(check).observe(document, { childList: true, subtree: true, characterData: true });
`

async function openPage(browser) {
  const { browserContextId } = await browser.send('Target.createBrowserContext')
  const { targetId } = await browser.send('Target.createTarget', { url: 'about:blank', browserContextId })
  const { sessionId } = await browser.send('Target.attachToTarget', { targetId, flatten: true })
  const send = (m, p) => browser.send(m, p, sessionId)
  const errors = []
  const listener = (msg) => {
    if (msg.sessionId !== sessionId) return
    if (msg.method === 'Runtime.consoleAPICalled' && (msg.params.type === 'error' || msg.params.type === 'assert')) {
      errors.push(`console.${msg.params.type}: ${msg.params.args.map((a) => a.value ?? a.description ?? '').join(' ')}`.slice(0, 400))
    } else if (msg.method === 'Runtime.exceptionThrown') {
      const d = msg.params.exceptionDetails
      errors.push(`exception: ${d.exception?.description ?? d.text}`.slice(0, 400))
    } else if (msg.method === 'Log.entryAdded' && msg.params.entry.level === 'error') {
      errors.push(`log: ${msg.params.entry.text} ${msg.params.entry.url ?? ''}`.slice(0, 400))
    }
  }
  browser.listeners.add(listener)
  await send('Runtime.enable')
  await send('Log.enable')
  await send('Page.enable')
  await send('Network.enable')
  await send('Network.setCacheDisabled', { cacheDisabled: false })
  await send('Page.addScriptToEvaluateOnNewDocument', { source: PAGE_SCRIPT })
  const evaluate = async (expression) => {
    const r = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true })
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? r.exceptionDetails.text)
    return r.result.value
  }
  const close = async () => {
    browser.listeners.delete(listener)
    await browser.send('Target.closeTarget', { targetId }).catch(() => {})
    await browser.send('Target.disposeBrowserContext', { browserContextId }).catch(() => {})
  }
  return { send, evaluate, errors, close }
}

async function waitFor(page, expression, timeoutMs, what) {
  const end = Date.now() + timeoutMs
  for (;;) {
    try {
      const v = await page.evaluate(expression)
      if (v) return v
    } catch (e) {
      // navigating / reloading; a dead browser is fatal
      if (chromeExit || /DevTools connection closed|Chrome exited/.test(String(e))) throw e
    }
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`)
    await sleep(5)
  }
}

// ---------------------------------------------------------------------------------------
// Steps

let nextPort = 5410
async function startAndRender(kind, cold) {
  const port = nextPort++
  const browser = await launchChrome()
  const srv = await server(kind, { cold, port })
  const page = await openPage(browser)
  page.browser = browser
  await page.send('Page.navigate', { url: `${srv.url}/` })
  let first
  try {
    first = await waitFor(page, 'window.__tsrBench && window.__tsrBench.firstRender', 120000, `${kind} first render`)
  } catch (e) {
    const body = await page.evaluate('document.body ? document.body.innerHTML.slice(0, 500) : ""').catch(() => '')
    throw new Error(
      `${e.message}\npage errors:\n  ${page.errors.slice(0, 20).join('\n  ')}\nbody: ${body}\nserver log:\n${srv.log().split('\n').slice(-30).join('\n')}`,
    )
  }
  return { srv, page, ms: first - srv.t0, pgid: srv.pgid }
}

async function stop(srv, page) {
  await page.close()
  page.browser.kill()
  srv.kill()
  await sleep(200)
}

const pct = (xs, p) => {
  const s = [...xs].sort((a, b) => a - b)
  return s[Math.min(s.length - 1, Math.floor((p / 100) * s.length))]
}
const r1 = (x) => Math.round(x * 10) / 10

/** Marks the page and the edited component's DOM node, then edits the file N times. */
async function editLoop(page, n, warmup) {
  const marked = await page.evaluate(`(() => {
    const el = document.querySelector(${JSON.stringify(profile.kept)});
    if (!el) return false;
    el.__tsrKept = true;
    window.__tsrMarker = 'kept';
    return true;
  })()`)
  if (!marked) throw new Error(`${profile.kept} not found`)
  const times = []
  let stateKept = 0
  let reloads = 0
  for (let i = 0; i < n + warmup; i++) {
    const want = `ts-r bench edit ${i}`
    // sessionStorage keeps the expected text across a full reload (then the update time
    // includes the reload).
    await page.evaluate(`sessionStorage.setItem('tsrWant', ${JSON.stringify(want)}); window.__tsrBench.want = ${JSON.stringify(want)}; window.__tsrBench.seen = 0; true`)
    const t = now()
    fs.writeFileSync(EDIT, original.replace(profile.anchor, profile.replace(want)))
    const seen = await waitFor(page, 'window.__tsrBench && window.__tsrBench.seen', 10000, `update ${i}`)
    const kept = await page.evaluate(`({
      marker: window.__tsrMarker === 'kept',
      node: !!document.querySelector(${JSON.stringify(profile.kept)})?.__tsrKept,
    })`)
    if (!kept.marker) reloads++
    if (kept.node) stateKept++
    if (i >= warmup) times.push(seen - t)
    await sleep(120)
  }
  fs.writeFileSync(EDIT, original)
  await sleep(300)
  return { times, stateKept, reloads, total: n + warmup }
}

async function check(kind) {
  const { srv, page, ms } = await startAndRender(kind, true)
  await sleep(1500)
  const loadErrors = [...page.errors]
  const html = await page.evaluate('document.getElementById("root").innerHTML.length')
  let r
  try {
    r = await editLoop(page, 3, 0)
  } catch (e) {
    r = { error: String(e), times: [] }
  }
  await sleep(300)
  const result = {
    app: appName,
    server: kind,
    firstRenderMs: r1(ms),
    renderedHtmlChars: html,
    consoleErrors: loadErrors,
    errorsDuringEdits: page.errors.slice(loadErrors.length),
    edits: r.total,
    editError: r.error,
    reloads: r.reloads,
    stateKept: r.stateKept,
    updateMs: r.times.map(r1),
    serverLog: srv.log().split('\n').slice(-15),
  }
  await stop(srv, page)
  return result
}

async function bench(kind) {
  const cold = []
  const warm = []
  for (let i = 0; i < runs; i++) {
    const c = await startAndRender(kind, true)
    cold.push(c.ms)
    await stop(c.srv, c.page)
    const w = await startAndRender(kind, false)
    warm.push(w.ms)
    await stop(w.srv, w.page)
  }
  // Edits and memory on a warm server.
  const { srv, page, pgid } = await startAndRender(kind, false)
  // Let late work settle (Vite's dependency discovery can still reload the page).
  await sleep(1500)
  let peak = 0
  const sampler = setInterval(() => {
    peak = Math.max(peak, groupRssMb(pgid))
  }, 100)
  const afterLoad = groupRssMb(pgid)
  const r = await editLoop(page, edits, 3)
  const afterEdits = groupRssMb(pgid)
  clearInterval(sampler)
  peak = Math.max(peak, afterEdits)
  const result = {
    app: appName,
    server: kind,
    coldMs: { median: r1(pct(cold, 50)), runs: cold.map(r1) },
    warmMs: { median: r1(pct(warm, 50)), runs: warm.map(r1) },
    update: { p50: r1(pct(r.times, 50)), p95: r1(pct(r.times, 95)), max: r1(Math.max(...r.times)), n: r.times.length },
    stateKept: `${r.stateKept}/${r.total}`,
    reloads: r.reloads,
    rssMb: { afterLoad: r1(afterLoad), afterEdits: r1(afterEdits), peak: r1(peak) },
    consoleErrors: page.errors,
  }
  await stop(srv, page)
  return result
}

const kinds = which === 'both' ? ['ts-r', 'vite'] : [which]
const results = []
try {
  for (const kind of kinds) {
    const r = mode === 'bench' ? await bench(kind) : await check(kind)
    console.log(JSON.stringify(r, null, 2))
    results.push(r)
  }
} catch (e) {
  console.error(e.stack ?? String(e))
  process.exitCode = 1
}
if (outFile) fs.writeFileSync(outFile, JSON.stringify(results, null, 2))
process.exit(process.exitCode ?? 0)
