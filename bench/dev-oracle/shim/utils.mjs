// Stands in for the playground's `~utils` (playground/test-utils.ts + vitestSetup.ts) with
// the same exports and behaviour, written from how those helpers behave. The worker sets
// the context (page, directories, URL) with `__set` before it imports a spec.

import fs from 'node:fs'
import path from 'node:path'
import { Unsupported } from '../cdp.mjs'
import { expect } from './vitest.mjs'

export let page
export let browser
export let testDir
export let rootDir
export let testPath
export let testName
export let viteTestUrl
export const serverLogs = []
export const browserLogs = []
export const browserErrors = []
export const isBuild = false
export const isServe = true
export const isBundledDev = false
export const isBundled = false
export const isWindows = process.platform === 'win32'
export const workspaceRoot = undefined
export const viteBinPath = undefined

let colorNames = {}
export function __set(ctx) {
  ;({ page, browser, testDir, rootDir, testPath, testName, viteTestUrl } = ctx)
  colorNames = ctx.colorNames ?? colorNames
}

// Accessing these is build-only or Vite-internal; the runner skips tests that do.
export const viteServer = new Proxy({}, { get: () => { throw new Unsupported('viteServer') } })

export const ports = {
  cli: 9510, 'cli-module': 9511, json: 9512, 'legacy/ssr': 9520, lib: 9521, 'optimize-missing-deps': 9522,
  'legacy/client-and-ssr': 9523, 'assets/encoded-base': 9554, 'assets/url-base': 9525, ssr: 9600,
  'ssr-deps': 9601, 'ssr-html': 9602, 'ssr-noexternal': 9603, 'ssr-pug': 9604, 'ssr-wasm': 9608,
  'ssr-webworker': 9605, 'proxy-bypass': 9606, 'proxy-bypass/non-existent-app': 9607, 'ssr-hmr': 9609,
  'proxy-hmr': 9616, 'proxy-hmr/other-app': 9617, 'ssr-conditions': 9620, 'css/postcss-caching': 5005,
  'css/postcss-plugins-different-dir': 5006, 'css/dynamic-import': 5007, 'css/lightningcss-proxy': 5008,
  'backend-integration': 5009, 'client-reload': 5010, 'client-reload/hmr-port': 5011,
  'client-reload/cross-origin': 5012,
}
export const hmrPorts = {
  'optimize-missing-deps': 24680, ssr: 24681, 'ssr-deps': 24682, 'ssr-html': 24683, 'ssr-noexternal': 24684,
  'ssr-pug': 24685, 'ssr-wasm': 24691, 'css/lightningcss-proxy': 24686, json: 24687, 'ssr-conditions': 24688,
  'client-reload/hmr-port': 24689, 'client-reload/cross-origin': 24690,
}

/** CSS named colors; the worker resolves each to its computed `rgb()` in Chrome. */
export const CSS_COLOR_NAMES = `aliceblue antiquewhite aqua aquamarine azure beige bisque black blanchedalmond blue
blueviolet brown burlywood cadetblue chartreuse chocolate coral cornflowerblue cornsilk crimson cyan darkblue
darkcyan darkgoldenrod darkgray darkgreen darkgrey darkkhaki darkmagenta darkolivegreen darkorange darkorchid darkred
darksalmon darkseagreen darkslateblue darkslategray darkslategrey darkturquoise darkviolet deeppink deepskyblue
dimgray dimgrey dodgerblue firebrick floralwhite forestgreen fuchsia gainsboro ghostwhite gold goldenrod gray green
greenyellow grey honeydew hotpink indianred indigo ivory khaki lavender lavenderblush lawngreen lemonchiffon
lightblue lightcoral lightcyan lightgoldenrodyellow lightgray lightgreen lightgrey lightpink lightsalmon
lightseagreen lightskyblue lightslategray lightslategrey lightsteelblue lightyellow lime limegreen linen magenta
maroon mediumaquamarine mediumblue mediumorchid mediumpurple mediumseagreen mediumslateblue mediumspringgreen
mediumturquoise mediumvioletred midnightblue mintcream mistyrose moccasin navajowhite navy oldlace olive olivedrab
orange orangered orchid palegoldenrod palegreen paleturquoise palevioletred papayawhip peachpuff peru pink plum
powderblue purple rebeccapurple red rosybrown royalblue saddlebrown salmon sandybrown seagreen seashell sienna
silver skyblue slateblue slategray slategrey snow springgreen steelblue tan teal thistle tomato turquoise violet
wheat white whitesmoke yellow yellowgreen`.split(/\s+/)

async function toEl(el) {
  if (typeof el === 'string') {
    const h = await page.$(el)
    if (h == null) throw new Error(`Cannot find element: "${el}"`)
    return h
  }
  return 'elementHandle' in el ? el.elementHandle() : el
}
const named = (rgb) => colorNames[rgb.replace(/\s+/g, '')] ?? rgb

export async function getColor(el) {
  return named(await (await toEl(el)).evaluate((e) => getComputedStyle(e).color))
}
export async function getBg(el) {
  return (await toEl(el)).evaluate((e) => getComputedStyle(e).backgroundImage)
}
export async function getBgColor(el) {
  return named(await (await toEl(el)).evaluate((e) => getComputedStyle(e).backgroundColor))
}
export async function getCssRuleBg(selector) {
  return page.evaluate((sel) => {
    for (const sheet of document.styleSheets) {
      try {
        for (const rule of sheet.cssRules) {
          if (rule instanceof CSSStyleRule && rule.selectorText === sel) return rule.style.backgroundImage
        }
      } catch {}
    }
  }, selector)
}

export function readFile(filename, encoding) {
  if (encoding === undefined) encoding = 'utf-8'
  return fs.readFileSync(path.resolve(testDir, filename), encoding)
}
export function editFile(filename, a, b) {
  filename = path.resolve(testDir, filename)
  const [encoding, replacer] = b ? [a, b] : ['utf-8', a]
  const content = fs.readFileSync(filename, encoding)
  const modified = replacer(content)
  if (Buffer.byteLength(modified) === Buffer.byteLength(content)) {
    throw new Error(`editFile("${filename}") did not change the file size`)
  }
  fs.writeFileSync(filename, modified)
}
export function addFile(filename, content) {
  const f = path.resolve(testDir, filename)
  fs.mkdirSync(path.dirname(f), { recursive: true })
  fs.writeFileSync(f, content)
}
export function removeFile(filename) {
  fs.unlinkSync(path.resolve(testDir, filename))
}

/** Serve mode without bundledDev: just navigate. */
export async function gotoLatestBuild(url) {
  await page.goto(url)
}

export function hotUpdatedLog(file) {
  return `[vite] hot updated: /${file}`
}

export async function untilBrowserLogAfter(operation, target, arg3, arg4) {
  const expectOrder = typeof arg3 === 'boolean' ? arg3 : false
  const callback = typeof arg3 === 'boolean' ? arg4 : arg3
  const promise = untilBrowserLog(target, expectOrder)
  await operation()
  const logs = await promise
  if (callback) await callback(logs)
  return logs
}

function untilBrowserLog(target, expectOrder = true) {
  const { promise, resolve, reject } = promiseWithResolvers()
  const logs = []
  const isMatch = (m) => (text) => (typeof m === 'string' ? text === m : m.test(text))
  let processMsg
  if (!target) processMsg = () => true
  else if (Array.isArray(target)) {
    if (expectOrder) {
      const remaining = [...target]
      processMsg = (text) => {
        expect(text).toMatch(remaining.shift())
        return remaining.length === 0
      }
    } else {
      const remaining = target.map(isMatch)
      processMsg = (text) => {
        const i = remaining.findIndex((m) => m(text))
        if (i >= 0) remaining.splice(i, 1)
        return remaining.length === 0
      }
    }
  } else processMsg = isMatch(target)
  const handleMsg = (msg) => {
    try {
      const text = msg.text()
      logs.push(text)
      if (processMsg(text)) resolve()
    } catch (err) {
      reject(err)
    }
  }
  const t = setTimeout(() => {
    const next = Array.isArray(target) ? (expectOrder ? target[0] : target.join(', ')) : target
    reject(new Error(`Timeout waiting for browser logs. Waiting for: ${next}`))
  }, 5000)
  page.on('console', handleMsg)
  return promise.finally(() => {
    clearTimeout(t)
    page.off('console', handleMsg)
  }).then(() => logs)
}

export function promiseWithResolvers() {
  let resolve
  let reject
  const promise = new Promise((a, b) => {
    resolve = a
    reject = b
  })
  return { promise, resolve, reject }
}

export function slash(p) {
  return p.replace(/\\/g, '/')
}

const buildOnly = (name) => () => {
  throw new Unsupported(`${name} (build only)`)
}
export const listAssets = buildOnly('listAssets')
export const findAssetFile = buildOnly('findAssetFile')
export const readManifest = buildOnly('readManifest')
export const readDepOptimizationMetadata = buildOnly('readDepOptimizationMetadata')
export const extractSourcemap = buildOnly('extractSourcemap')
export const formatSourcemapForSnapshot = buildOnly('formatSourcemapForSnapshot')
export const killProcess = buildOnly('killProcess')
export const notifyRebuildComplete = buildOnly('notifyRebuildComplete')
export const createInMemoryLogger = buildOnly('createInMemoryLogger')
export const sourcemapSnapshot = buildOnly('sourcemapSnapshot')
export const watcher = undefined
export function setViteUrl(url) {
  viteTestUrl = url
}
export const startDefaultServe = buildOnly('startDefaultServe')
