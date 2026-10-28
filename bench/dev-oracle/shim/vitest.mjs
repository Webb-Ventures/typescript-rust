// The subset of vitest's API that Vite's playground specs use: test/it/describe with
// skipIf/runIf/skip, hooks, `expect` (with `.not`, `.poll`, asymmetric matchers) and
// `vi.waitFor`/`vi.waitUntil`. Written from vitest's documented behaviour. The runner in
// ../worker.mjs walks the collected tree.

import { isDeepStrictEqual, inspect } from 'node:util'
import { Unsupported } from '../cdp.mjs'

// ---------------------------------------------------------------------------------------
// Collection

const root = { name: '', children: [], beforeAll: [], afterAll: [], beforeEach: [], afterEach: [], skip: null }
let current = root
export const __root = root

function addTest(name, a, b, skip) {
  const fn = typeof a === 'function' ? a : b
  const opts = typeof a === 'object' && a ? a : typeof b === 'object' && b ? b : {}
  const timeout = typeof b === 'number' ? b : opts.timeout
  current.children.push({ kind: 'test', name: String(name), fn, timeout, skip: skip ?? (fn ? null : 'todo') })
}
function addSuite(name, a, b, skip) {
  const fn = typeof a === 'function' ? a : b
  const suite = { kind: 'suite', name: String(name), children: [], beforeAll: [], afterAll: [], beforeEach: [], afterEach: [], skip }
  current.children.push(suite)
  const parent = current
  current = suite
  try {
    fn?.()
  } finally {
    current = parent
  }
}

function withModifiers(add) {
  const f = (name, a, b) => add(name, a, b, null)
  f.skip = (name, a, b) => add(name, a, b, 'test.skip in the spec')
  f.todo = (name) => add(name, undefined, undefined, 'todo')
  f.only = f
  f.concurrent = f
  f.sequential = f
  f.fails = (name) => add(name, undefined, undefined, 'test.fails in the spec')
  // Chains like `describe.runIf(isBuild).skip(...)`: the result takes modifiers too.
  const conditional = (reason) => {
    const g = (name, a, b) => add(name, a, b, reason)
    g.skip = f.skip
    g.only = g
    g.concurrent = g
    g.sequential = g
    return g
  }
  f.skipIf = (cond) => conditional(cond ? 'skipIf(true) for dev serve' : null)
  f.runIf = (cond) => conditional(cond ? null : 'runIf(false) for dev serve')
  f.each = () => {
    throw new Unsupported('test.each')
  }
  return f
}

export const test = withModifiers(addTest)
export const it = test
export const describe = withModifiers(addSuite)
export const suite = describe
export const beforeAll = (fn) => current.beforeAll.push(fn)
export const afterAll = (fn) => current.afterAll.push(fn)
export const beforeEach = (fn) => current.beforeEach.push(fn)
export const afterEach = (fn) => current.afterEach.push(fn)
export const onTestFinished = () => {
  throw new Unsupported('onTestFinished')
}
export const inject = () => undefined

// ---------------------------------------------------------------------------------------
// expect

class Asym {
  constructor(name, test) {
    this.name = name
    this.test = test
  }
}
const isAsym = (x) => x instanceof Asym

function equals(a, b, strict) {
  if (isAsym(b)) return b.test(a)
  if (isAsym(a)) return a.test(b)
  if (Object.is(a, b)) return true
  if (a instanceof RegExp && b instanceof RegExp) return String(a) === String(b)
  if (typeof a !== 'object' || typeof b !== 'object' || !a || !b) return false
  if (Array.isArray(a) !== Array.isArray(b)) return false
  if (Array.isArray(a)) return a.length === b.length && a.every((x, i) => equals(x, b[i], strict))
  if (strict && Object.getPrototypeOf(a) !== Object.getPrototypeOf(b)) return false
  const keys = (o) => Object.keys(o).filter((k) => strict || o[k] !== undefined)
  const ka = keys(a)
  const kb = keys(b)
  if (ka.length !== kb.length) return false
  if (!ka.every((k) => Object.hasOwn(b, k) && equals(a[k], b[k], strict))) return false
  return strict ? isDeepStrictEqual(Object.keys(a).sort(), Object.keys(b).sort()) || true : true
}
function matchObject(a, b) {
  if (isAsym(b)) return b.test(a)
  if (typeof b !== 'object' || !b) return equals(a, b)
  if (typeof a !== 'object' || !a) return false
  if (Array.isArray(b)) return Array.isArray(a) && a.length === b.length && b.every((x, i) => matchObject(a[i], x))
  return Object.keys(b).every((k) => matchObject(a[k], b[k]))
}
const show = (v) => inspect(v, { depth: 4, breakLength: 120 }).slice(0, 600)

const matchers = {
  toBe: (a, b) => [Object.is(a, b), `expected ${show(a)} to be ${show(b)}`],
  toEqual: (a, b) => [equals(a, b, false), `expected ${show(a)} to deeply equal ${show(b)}`],
  toStrictEqual: (a, b) => [equals(a, b, true), `expected ${show(a)} to strictly equal ${show(b)}`],
  toMatchObject: (a, b) => [matchObject(a, b), `expected ${show(a)} to match object ${show(b)}`],
  toMatch: (a, b) => [
    typeof a === 'string' && (typeof b === 'string' ? a.includes(b) : b.test(a)),
    `expected ${show(a)} to match ${show(b)}`,
  ],
  toContain: (a, b) => [
    // A string and anything else: chai's `indexOf` (a RegExp is compared as its text).
    a != null && (typeof a === 'string' ? a.indexOf(b) !== -1 : [...a].includes(b)),
    `expected ${show(a)} to contain ${show(b)}`,
  ],
  toContainEqual: (a, b) => [a != null && [...a].some((x) => equals(x, b)), `expected ${show(a)} to contain equal ${show(b)}`],
  toHaveLength: (a, n) => [a?.length === n, `expected length ${a?.length} to be ${n}`],
  toBeTruthy: (a) => [!!a, `expected ${show(a)} to be truthy`],
  toBeFalsy: (a) => [!a, `expected ${show(a)} to be falsy`],
  toBeDefined: (a) => [a !== undefined, `expected value to be defined`],
  toBeUndefined: (a) => [a === undefined, `expected ${show(a)} to be undefined`],
  toBeNull: (a) => [a === null, `expected ${show(a)} to be null`],
  toBeNaN: (a) => [Number.isNaN(a), `expected ${show(a)} to be NaN`],
  toBeGreaterThan: (a, b) => [a > b, `expected ${a} > ${b}`],
  toBeGreaterThanOrEqual: (a, b) => [a >= b, `expected ${a} >= ${b}`],
  toBeLessThan: (a, b) => [a < b, `expected ${a} < ${b}`],
  toBeLessThanOrEqual: (a, b) => [a <= b, `expected ${a} <= ${b}`],
  toBeOneOf: (a, list) => [list.some((x) => equals(a, x)), `expected ${show(a)} to be one of ${show(list)}`],
  toBeInstanceOf: (a, c) => [a instanceof c, `expected ${show(a)} to be an instance of ${c?.name}`],
  toBeTypeOf: (a, t) => [typeof a === t, `expected typeof ${show(a)} to be ${t}`],
  toSatisfy: (a, f) => [!!f(a), `expected ${show(a)} to satisfy ${f}`],
  toHaveProperty: (a, k, v) => [
    a != null && k in Object(a) && (v === undefined || equals(a[k], v)),
    `expected ${show(a)} to have property ${k}`,
  ],
  toThrow: (f, m) => {
    try {
      // With `.rejects` the value is already the rejection.
      if (typeof f !== 'function') throw f
      f()
    } catch (e) {
      const ok = m === undefined || (typeof m === 'string' ? String(e?.message).includes(m) : m.test?.(e?.message))
      return [ok, `expected the thrown error to match ${show(m)}, got ${e?.message}`]
    }
    return [false, 'expected the function to throw']
  },
  toMatchInlineSnapshot: (a, snap) => {
    if (typeof a !== 'string' || snap === undefined) throw new Unsupported('toMatchInlineSnapshot of a non-string')
    return [a === inlineString(snap), `expected ${show(a)} to match inline snapshot ${show(snap)}`]
  },
}
matchers.toThrowError = matchers.toThrow

/** vitest's inline snapshot of a string: indented, wrapped in double quotes. */
function inlineString(snap) {
  const lines = snap.replace(/^\n/, '').replace(/\n\s*$/, '').split('\n')
  const indent = Math.min(...lines.filter((l) => l.trim()).map((l) => l.match(/^ */)[0].length))
  const text = lines.map((l) => l.slice(indent)).join('\n')
  return text.startsWith('"') && text.endsWith('"') ? text.slice(1, -1).replace(/\\"/g, '"') : text
}

class AssertionError extends Error {}

function build(get, { poll, timeout, interval } = {}) {
  const make = (negate, mode) =>
    new Proxy(
      {},
      {
        get(_, name) {
          if (name === 'not') return make(!negate, mode)
          if (name === 'resolves' || name === 'rejects') return make(negate, name)
          if (name === 'then') return undefined
          const m = matchers[name]
          if (!m) throw new Unsupported(`expect().${String(name)}`)
          return (...args) => {
            const once = async () => {
              let v = poll ? await get() : get()
              if (mode === 'resolves') v = await v
              if (mode === 'rejects') {
                try {
                  await v
                  throw new AssertionError('expected the promise to reject')
                } catch (e) {
                  if (e instanceof AssertionError) throw e
                  v = e
                }
              }
              const [ok, msg] = m(v, ...args)
              if (ok === negate) throw new AssertionError(negate ? `not: ${msg}` : msg)
            }
            if (!poll) {
              if (mode) return once()
              let v = get()
              const [ok, msg] = m(v, ...args)
              if (ok === negate) throw new AssertionError(negate ? `not: ${msg}` : msg)
              return
            }
            return (async () => {
              const end = Date.now() + timeout
              let last
              for (;;) {
                try {
                  await withTimeout(once(), Math.max(50, end - Date.now()))
                  return
                } catch (e) {
                  if (e instanceof Unsupported) throw e
                  last = e
                }
                if (Date.now() >= end) throw new AssertionError(`expect.poll timed out after ${timeout} ms: ${last?.message}`)
                await new Promise((r) => setTimeout(r, interval))
              }
            })()
          }
        },
      },
    )
  return make(false, null)
}

function withTimeout(p, ms) {
  let t
  return Promise.race([p, new Promise((_, rej) => (t = setTimeout(() => rej(new Error(`timed out after ${ms} ms`)), ms)))]).finally(() => clearTimeout(t))
}

/** expect.poll's timeout: vitest's default is 1 s; the oracle uses 5 s for both servers. */
export const POLL_TIMEOUT = 5000

export function expect(value) {
  return build(() => value)
}
expect.poll = (fn, opts = {}) =>
  build(fn, { poll: true, timeout: opts.timeout ?? POLL_TIMEOUT, interval: opts.interval ?? 50 })
expect.soft = expect
expect.stringContaining = (s) => new Asym('stringContaining', (x) => typeof x === 'string' && x.includes(s))
expect.stringMatching = (r) => new Asym('stringMatching', (x) => typeof x === 'string' && (typeof r === 'string' ? x.includes(r) : r.test(x)))
expect.arrayContaining = (arr) => new Asym('arrayContaining', (x) => Array.isArray(x) && arr.every((y) => x.some((z) => equals(z, y))))
expect.objectContaining = (o) => new Asym('objectContaining', (x) => matchObject(x, o))
expect.any = (c) =>
  new Asym('any', (x) => x != null && (x instanceof c || (c === String && typeof x === 'string') || (c === Number && typeof x === 'number') || (c === Boolean && typeof x === 'boolean') || (c === Function && typeof x === 'function')))
expect.anything = () => new Asym('anything', (x) => x != null)
expect.not = new Proxy({}, { get: () => { throw new Unsupported('expect.not.*') } })
expect.assertions = () => {}
expect.hasAssertions = () => {}

export const vi = {
  async waitFor(fn, opts = {}) {
    const timeout = typeof opts === 'number' ? opts : (opts.timeout ?? POLL_TIMEOUT)
    const interval = opts.interval ?? 50
    const end = Date.now() + timeout
    for (;;) {
      try {
        return await fn()
      } catch (e) {
        if (e instanceof Unsupported || Date.now() >= end) throw e
      }
      await new Promise((r) => setTimeout(r, interval))
    }
  },
  async waitUntil(fn, opts = {}) {
    const timeout = typeof opts === 'number' ? opts : (opts.timeout ?? POLL_TIMEOUT)
    const end = Date.now() + timeout
    for (;;) {
      const v = await fn()
      if (v) return v
      if (Date.now() >= end) throw new Error(`vi.waitUntil timed out after ${timeout} ms`)
      await new Promise((r) => setTimeout(r, opts.interval ?? 50))
    }
  },
  fn: () => {
    throw new Unsupported('vi.fn')
  },
  spyOn: () => {
    throw new Unsupported('vi.spyOn')
  },
}
