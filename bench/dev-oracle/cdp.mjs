// Headless Chrome over the DevTools protocol (Node's built-in WebSocket, no dependencies),
// and the subset of Playwright's `Page` / `Locator` / `ElementHandle` API that Vite's
// playground specs use. Written from Playwright's documented behaviour; unsupported calls
// throw `Unsupported`, which the runner records as a skip (for both servers).

import { spawn } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'

export class Unsupported extends Error {
  constructor(what) {
    super(`unsupported by the oracle's shim: ${what}`)
    this.unsupported = what
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

/** Playwright's default action timeout is 30 s; the oracle uses 5 s for both servers. */
export const ACTION_TIMEOUT = 5000

export async function launchChrome({ chromePath, limit, dir }) {
  fs.mkdirSync(dir, { recursive: true })
  const file = path.join(dir, 'DevToolsActivePort')
  fs.rmSync(file, { force: true })
  const child = spawn(limit, ['--mem-mb', '2048', '--time-s', '1200', '--', chromePath, '--headless=new',
    '--remote-debugging-port=0', `--user-data-dir=${dir}`, '--no-first-run', '--no-default-browser-check',
    '--disable-extensions', '--disable-background-networking', '--disable-renderer-backgrounding',
    '--disable-background-timer-throttling', '--disable-backgrounding-occluded-windows',
    '--window-size=1280,800', '--disable-gpu', '--renderer-process-limit=4', '--disable-component-update',
    '--disable-sync', '--mute-audio', '--use-mock-keychain', '--password-store=basic',
    '--disable-features=Translate,OptimizationHints,MediaRouter,DialMediaRouteProvider', 'about:blank'], { stdio: ['ignore', 'ignore', 'pipe'], detached: false })
  let log = ''
  child.stderr.on('data', (d) => (log = (log + d).slice(-4000)))
  let exited = ''
  child.on('exit', (c, s) => (exited = `Chrome exited (${c ?? s}): ${log}`))
  // limit.sh kills the whole Chrome process tree when it gets SIGTERM; SIGKILL would let
  // the tree escape.
  const kill = () => {
    try { child.kill('SIGTERM') } catch {}
  }
  for (let i = 0; i < 300 && !fs.existsSync(file); i++) {
    if (exited) break
    await sleep(50)
  }
  if (!fs.existsSync(file)) {
    kill()
    throw new Error(`Chrome did not start: ${exited || log}`)
  }
  const [port, wsPath] = fs.readFileSync(file, 'utf8').trim().split('\n')
  const conn = await connect(`ws://127.0.0.1:${port}${wsPath}`, () => exited)
  return {
    ...conn,
    kill: async () => {
      await Promise.race([conn.send('Browser.close').catch(() => {}), sleep(2000)])
      kill()
      try { conn.ws.close() } catch {}
    },
  }
}

function connect(url, exited) {
  const ws = new WebSocket(url)
  let next = 1
  const pending = new Map()
  const listeners = new Set()
  ws.addEventListener('message', (e) => {
    const msg = JSON.parse(e.data)
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id)
      pending.delete(msg.id)
      if (msg.error) reject(new Error(`CDP: ${msg.error.message}`))
      else resolve(msg.result)
    } else {
      for (const l of listeners) l(msg)
    }
  })
  let closed = false
  ws.addEventListener('close', () => {
    closed = true
    for (const { reject } of pending.values()) reject(new Error(exited() || 'DevTools connection closed'))
    pending.clear()
  })
  const send = (method, params = {}, sessionId) =>
    new Promise((resolve, reject) => {
      if (closed) return reject(new Error(exited() || 'DevTools connection closed'))
      const id = next++
      pending.set(id, { resolve, reject })
      ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }))
    })
  return new Promise((resolve, reject) => {
    ws.addEventListener('open', () => resolve({ send, listeners, ws }))
    ws.addEventListener('error', () => reject(new Error('DevTools connection failed')))
  })
}

/** Text of a console message the way Playwright's `ConsoleMessage.text()` joins arguments. */
function consoleText(args) {
  return args
    .map((a) => {
      if (a.type === 'string') return a.value
      if ('value' in a) return String(a.value)
      if (a.unserializableValue) return a.unserializableValue
      return a.description ?? (a.subtype === 'null' ? 'null' : `JSHandle@${a.type}`)
    })
    .join(' ')
}

/** Serializes a function (or expression string) and its argument for `Runtime.evaluate`. */
function fnSource(fn) {
  return typeof fn === 'function' ? fn.toString() : null
}

/** A Playwright-like page in its own browser context. */
export async function newPage(browser, { contextId, mapText = (t) => t } = {}) {
  const browserContextId = contextId ?? (await browser.send('Target.createBrowserContext')).browserContextId
  const { targetId } = await browser.send('Target.createTarget', { url: 'about:blank', browserContextId })
  const { sessionId } = await browser.send('Target.attachToTarget', { targetId, flatten: true })
  const send = (m, p) => browser.send(m, p, sessionId)
  const handlers = { console: new Set(), pageerror: new Set(), load: new Set(), framenavigated: new Set(), response: new Set() }
  let mainFrameId = null
  let docResponse = null
  let currentUrl = 'about:blank'
  const listener = (msg) => {
    if (msg.sessionId !== sessionId) return
    const p = msg.params
    switch (msg.method) {
      case 'Runtime.consoleAPICalled': {
        const text = mapText(consoleText(p.args))
        const m = { text: () => text, type: () => (p.type === 'warning' ? 'warning' : p.type), args: () => p.args }
        for (const h of [...handlers.console]) h(m)
        break
      }
      case 'Log.entryAdded': {
        // Playwright reports browser-generated entries ("Failed to load resource: ...") as
        // console messages too.
        // Playwright's Chromium doesn't fetch /favicon.ico; headless Chrome does.
        if (p.entry.url?.endsWith('/favicon.ico')) break
        const text = mapText(p.entry.text)
        const m = { text: () => text, type: () => p.entry.level, args: () => [] }
        for (const h of [...handlers.console]) h(m)
        break
      }
      case 'Runtime.exceptionThrown': {
        const d = p.exceptionDetails
        const e = new Error(d.exception?.description ?? d.text)
        for (const h of [...handlers.pageerror]) h(e)
        break
      }
      case 'Page.loadEventFired':
        for (const h of [...handlers.load]) h()
        break
      case 'Page.frameNavigated':
        if (!p.frame.parentId) {
          mainFrameId = p.frame.id
          currentUrl = p.frame.url + (p.frame.urlFragment ?? '')
          for (const h of [...handlers.framenavigated]) h(p.frame)
        }
        break
      case 'Page.navigatedWithinDocument':
        if (p.frameId === mainFrameId) currentUrl = p.url
        break
      case 'Network.responseReceived': {
        const r = { url: () => p.response.url, status: () => p.response.status, headers: () => lower(p.response.headers) }
        // The main frame's document: what `page.goto` resolves to.
        if (p.type === 'Document' && (p.frameId === mainFrameId || mainFrameId === null)) docResponse = r
        for (const h of [...handlers.response]) h(r)
        break
      }
    }
  }
  browser.listeners.add(listener)
  await send('Runtime.enable')
  await send('Page.enable')
  await send('Network.enable', { maxTotalBufferSize: 1_000_000, maxResourceBufferSize: 100_000 })
  await send('Log.enable')

  const evaluate = async (fn, arg) => {
    const src = fnSource(fn)
    const expression = src ? `(${src})(${arg === undefined ? '' : JSON.stringify(arg)})` : String(fn)
    const r = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true })
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? r.exceptionDetails.text)
    return r.result.value
  }
  const callOn = async (objectId, fn, arg) => {
    const r = await send('Runtime.callFunctionOn', {
      objectId,
      functionDeclaration: typeof fn === 'function' ? fn.toString() : fn,
      arguments: arg === undefined ? [] : [{ value: arg }],
      returnByValue: true,
      awaitPromise: true,
    })
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? r.exceptionDetails.text)
    return r.result.value
  }
  const query = async (selector) => {
    const r = await send('Runtime.evaluate', { expression: queryExpr(selector) })
    if (r.exceptionDetails) throw new Error(`bad selector ${selector}`)
    return r.result.objectId ? handle(r.result.objectId) : null
  }
  const waitFor = async (selector, { timeout = ACTION_TIMEOUT, state = 'attached' } = {}) => {
    queryExpr(selector)
    const end = Date.now() + timeout
    for (;;) {
      const h = await query(selector).catch(() => null)
      if (h && (state === 'attached' || (await h.isVisible()))) return h
      if (Date.now() > end) throw new Error(`Timeout ${timeout}ms waiting for selector "${selector}"`)
      await sleep(50)
    }
  }
  const handle = (objectId) => ({
    evaluate: (fn, arg) => callOn(objectId, `function(a){ return (${fnSource(fn)})(this, a) }`, arg),
    textContent: () => callOn(objectId, 'function(){ return this.textContent }'),
    innerHTML: () => callOn(objectId, 'function(){ return this.innerHTML }'),
    innerText: () => callOn(objectId, 'function(){ return this.innerText }'),
    getAttribute: (n) => callOn(objectId, 'function(n){ return this.getAttribute(n) }', n),
    isVisible: () => callOn(objectId, 'function(){ const r = this.getBoundingClientRect(); return !!(r.width || r.height) && getComputedStyle(this).visibility !== "hidden" }'),
    click: () => callOn(objectId, 'function(){ this.scrollIntoView(); this.click() }'),
    elementHandle: async () => handle(objectId),
    getProperty: async (n) => {
      const v = await callOn(objectId, 'function(n){ return this[n] }', n)
      return { jsonValue: async () => v }
    },
    boundingBox: () => callOn(objectId, 'function(){ const r = this.getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height } }'),
    // Like Playwright's, it looks inside the element's shadow root when it has one.
    $$eval: (sel, fn, arg) =>
      callOn(objectId, `function(a){ const root = this.shadowRoot || this; return (${fnSource(fn)})([...root.querySelectorAll(${JSON.stringify(sel)})], a) }`, arg),
  })
  const locator = (selector) => ({
    textContent: async () => (await waitFor(selector)).textContent(),
    innerHTML: async () => (await waitFor(selector)).innerHTML(),
    innerText: async () => (await waitFor(selector)).innerText(),
    getAttribute: async (n) => (await waitFor(selector)).getAttribute(n),
    click: async () => (await waitFor(selector, { state: 'visible' })).click(),
    evaluate: async (fn, arg) => (await waitFor(selector)).evaluate(fn, arg),
    elementHandle: () => waitFor(selector),
    boundingBox: async () => (await waitFor(selector)).boundingBox(),
    isVisible: async () => {
      const h = await query(selector).catch(() => null)
      return h ? h.isVisible() : false
    },
    count: () => evaluate(`document.querySelectorAll(${JSON.stringify(cssOf(selector))}).length`),
    waitFor: (o) => waitFor(selector, o),
    first: () => locator(selector),
    locator: (inner) => locator(`${selector} ${inner}`),
  })
  const nextLoad = (timeout = 10000) =>
    new Promise((resolve, reject) => {
      const t = setTimeout(() => {
        handlers.load.delete(done)
        reject(new Error(`page.waitForEvent: Timeout ${timeout}ms exceeded while waiting for event "load"`))
      }, timeout)
      const done = () => {
        clearTimeout(t)
        handlers.load.delete(done)
        resolve()
      }
      handlers.load.add(done)
    })
  let closed = false
  const page = {
    async goto(url) {
      const loaded = nextLoad(15000)
      docResponse = null
      const r = await send('Page.navigate', { url })
      if (r.errorText) {
        loaded.catch(() => {})
        throw new Error(`page.goto: ${r.errorText} at ${url}`)
      }
      await loaded
      return docResponse
    },
    async reload() {
      const loaded = nextLoad(15000)
      await send('Page.reload', {})
      await loaded
    },
    url: () => currentUrl,
    title: () => evaluate('document.title'),
    content: () => evaluate('document.documentElement.outerHTML'),
    evaluate,
    $: query,
    $$: () => {
      throw new Unsupported('page.$$')
    },
    $eval: async (sel, fn, arg) => (await waitFor(sel)).evaluate(fn, arg),
    textContent: (sel) => locator(sel).textContent(),
    innerHTML: (sel) => locator(sel).innerHTML(),
    innerText: (sel) => locator(sel).innerText(),
    getAttribute: (sel, n) => locator(sel).getAttribute(n),
    isVisible: (sel) => locator(sel).isVisible(),
    click: (sel) => locator(sel).click(),
    locator,
    getByTestId: (id) => locator(`[data-testid="${id}"]`),
    waitForSelector: (sel, o) => waitFor(sel, o),
    waitForTimeout: (ms) => sleep(ms),
    async waitForEvent(event, opts) {
      const timeout = (typeof opts === 'object' && opts?.timeout) || 10000
      if (event === 'load') return nextLoad(timeout)
      if (event !== 'console' && event !== 'pageerror' && event !== 'response') throw new Unsupported(`page.waitForEvent('${event}')`)
      const pred = typeof opts === 'function' ? opts : opts?.predicate
      return new Promise((resolve, reject) => {
        const t = setTimeout(() => {
          handlers[event].delete(h)
          reject(new Error(`page.waitForEvent: Timeout ${timeout}ms exceeded while waiting for event "${event}"`))
        }, timeout)
        const h = async (x) => {
          if (pred && !(await pred(x))) return
          clearTimeout(t)
          handlers[event].delete(h)
          resolve(x)
        }
        handlers[event].add(h)
      })
    },
    waitForNavigation: (o) => nextLoad(o?.timeout ?? 10000),
    async waitForResponse(pred, o) {
      const match = typeof pred === 'function' ? pred : (r) => r.url().includes(pred)
      return page.waitForEvent('response', { predicate: match, timeout: o?.timeout ?? 10000 })
    },
    async waitForFunction(fn, arg, o) {
      const end = Date.now() + (o?.timeout ?? ACTION_TIMEOUT)
      for (;;) {
        if (await evaluate(fn, arg).catch(() => false)) return true
        if (Date.now() > end) throw new Error('Timeout in waitForFunction')
        await sleep(50)
      }
    },
    on: (ev, h) => {
      if (!handlers[ev]) throw new Unsupported(`page.on('${ev}')`)
      handlers[ev].add(h)
      return page
    },
    off: (ev, h) => {
      handlers[ev]?.delete(h)
      return page
    },
    request: {
      async get(url) {
        const res = await fetch(new URL(url, currentUrl))
        const body = Buffer.from(await res.arrayBuffer())
        return {
          status: () => res.status,
          ok: () => res.ok,
          headers: () => Object.fromEntries(res.headers),
          text: async () => body.toString('utf8'),
          body: async () => body,
          json: async () => JSON.parse(body.toString('utf8')),
        }
      },
    },
    keyboard: {
      // Named keys only (what the specs press).
      async press(key) {
        const codes = { Escape: 27, Enter: 13, Tab: 9 }
        if (!(key in codes)) throw new Unsupported(`page.keyboard.press('${key}')`)
        const k = { key, code: key, windowsVirtualKeyCode: codes[key] }
        await send('Input.dispatchKeyEvent', { type: 'keyDown', ...k })
        await send('Input.dispatchKeyEvent', { type: 'keyUp', ...k })
      },
    },
    get mouse() { throw new Unsupported('page.mouse') },
    route: () => { throw new Unsupported('page.route') },
    // A same-origin child frame by name; only its `content()` is supported.
    frame: (name) => {
      if (typeof name !== 'string') throw new Unsupported('page.frame({...})')
      const doc = `document.querySelector('iframe[name=' + ${JSON.stringify(JSON.stringify(name))} + ']')?.contentDocument`
      return {
        content: () => evaluate(`(${doc})?.documentElement.outerHTML ?? ''`),
      }
    },
    frameLocator: () => { throw new Unsupported('page.frameLocator') },
    getByText: () => { throw new Unsupported('page.getByText') },
    getByRole: () => { throw new Unsupported('page.getByRole') },
    setViewportSize: () => { throw new Unsupported('page.setViewportSize') },
    isClosed: () => closed,
    async close() {
      if (closed) return
      closed = true
      browser.listeners.delete(listener)
      await browser.send('Target.closeTarget', { targetId }).catch(() => {})
      if (!contextId) await browser.send('Target.disposeBrowserContext', { browserContextId }).catch(() => {})
    },
    browserContextId,
  }
  return page
}

function lower(h) {
  return Object.fromEntries(Object.entries(h).map(([k, v]) => [k.toLowerCase(), v]))
}

/** Playwright selectors the specs use beyond CSS: `text=...` only in `query`. */
function cssOf(selector) {
  if (/^(text|xpath|id)=/.test(selector) || selector.includes('>>') || /:has-text\(|:text\(/.test(selector)) {
    throw new Unsupported(`selector ${selector}`)
  }
  return selector
}

/**
 * A page expression for the first element `selector` matches. `text=Foo` (unquoted) is
 * Playwright's text selector: the innermost element whose whitespace-normalized text
 * contains `Foo`, ignoring case.
 */
export function queryExpr(selector) {
  const m = /^text=([^"'].*)$/.exec(selector)
  if (!m) return `document.querySelector(${JSON.stringify(cssOf(selector))})`
  const want = JSON.stringify(m[1].trim().replace(/\s+/g, ' ').toLowerCase())
  return `(() => {
    const norm = (e) => (e.textContent || '').replace(/\\s+/g, ' ').toLowerCase()
    const hit = (e) => e.tagName !== 'SCRIPT' && e.tagName !== 'STYLE' && norm(e).includes(${want})
    for (const e of document.body ? document.body.querySelectorAll('*') : []) {
      if (hit(e) && ![...e.children].some(hit)) return e
    }
    return null
  })()`
}
