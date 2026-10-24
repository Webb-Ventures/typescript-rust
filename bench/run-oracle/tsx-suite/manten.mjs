// A stand-in for `manten` (the test runner tsx's suite is written in): the same exports
// (`describe`, `test`, `expect`, `skip`, `onTestFail`, `onTestFinish`, `onFinish`,
// `setProcessTimeout`), but it records each test's result for the oracle instead of printing a
// report, runs at most TS_R_ORACLE_JOBS tests at a time, times every test out, and skips tests
// the oracle can't compare (see `skipReason` in spec-runner.mjs). Written for the oracle; no manten code.

import { AsyncLocalStorage } from 'node:async_hooks';
import { expect } from 'expect';

export { expect };

const store = new AsyncLocalStorage();
const maxJobs = Math.max(1, Math.min(4, Number(process.env.TS_R_ORACLE_JOBS ?? 4)));
const minTimeoutMs = Number(process.env.TS_R_ORACLE_TEST_TIMEOUT_MS ?? 30_000);

/** Filled by spec-runner.mjs. */
export const runner = {
  results: [],
  /** (fullName, fn) -> skip reason or null */
  skipReason: () => null,
  /** fullName -> boolean */
  selected: () => true,
};

let active = 0;
const waiting = [];
const acquire = () => new Promise((resolve) => {
  if (active < maxJobs) {
    active++;
    resolve();
  } else {
    waiting.push(resolve);
  }
});
const release = () => {
  const next = waiting.shift();
  if (next) next();
  else active--;
};

const root = { kind: 'describe', names: [], children: [], finish: [] };
const current = () => store.getStore() ?? root;
const nearest = (kind) => {
  for (let c = current(); c; c = c.parent) if (c.kind === kind) return c;
  return null;
};

export function describe(name, fn) {
  const parent = current();
  const ctx = { kind: 'describe', parent, names: [...parent.names, name], children: [], finish: [] };
  const done = store.run(ctx, async () => {
    try {
      await fn({ describe, test, onFinish });
    } catch (error) {
      runner.results.push({ name: ctx.names.join(' > '), status: 'error', error: message(error) });
    }
    // Tests registered later by awaited children are in `children` too.
    for (let i = 0; i < ctx.children.length; i++) await ctx.children[i];
    for (const f of ctx.finish) await f().catch(() => {});
  });
  parent.children.push(done);
  return done;
}

export function test(name, fn, options) {
  const parent = current();
  const names = [...parent.names, name];
  const fullName = names.join(' > ');
  const given = typeof options === 'number' ? options : options?.timeout;
  const timeoutMs = Math.max(given ?? 0, minTimeoutMs);
  const done = (async () => {
    if (!runner.selected(fullName)) return;
    const skip = runner.skipReason(fullName, fn);
    if (skip) {
      runner.results.push({ name: fullName, status: 'skip', reason: skip });
      return;
    }
    await acquire();
    const ctx = { kind: 'test', parent, names, fail: [], finish: [] };
    const start = Date.now();
    let timer;
    let error = null;
    try {
      await store.run(ctx, () => Promise.race([
        Promise.resolve().then(() => fn({ onTestFail, onTestFinish })),
        new Promise((_, reject) => {
          timer = setTimeout(() => reject(new Error(`timed out after ${timeoutMs} ms`)), timeoutMs);
        }),
      ]));
    } catch (e) {
      error = e;
    } finally {
      clearTimeout(timer);
    }
    if (error) {
      for (const f of ctx.fail) {
        try { await f(error); } catch {}
      }
    }
    for (const f of ctx.finish) {
      try { await f(); } catch {}
    }
    release();
    const ms = Date.now() - start;
    if (error?.oracleSkip) {
      runner.results.push({ name: fullName, status: 'skip', reason: error.oracleSkip });
    } else {
      runner.results.push(error
        ? { name: fullName, status: 'fail', error: message(error), output: error?.oracleOutput, ms }
        : { name: fullName, status: 'pass', ms });
    }
  })();
  parent.children.push(done);
  return done;
}

export function onTestFail(fn) {
  nearest('test')?.fail.push(fn);
}

export function onTestFinish(fn) {
  const t = nearest('test');
  if (t) t.finish.push(fn);
  else current().finish?.push(async () => fn());
}

export function onFinish(fn) {
  (nearest('describe') ?? root).finish.push(async () => fn());
}

export function setProcessTimeout() {}

/** Skips the running test (the spec calls it for platform-specific cases). */
export function skip(reason) {
  throw Object.assign(new Error(`skipped: ${reason}`), { oracleSkip: `skipped by the spec: ${reason}` });
}

/** Waits for every top-level describe and test. */
export async function settle() {
  for (let i = 0; i < root.children.length; i++) await root.children[i];
  for (const f of root.finish) await f().catch(() => {});
}

function message(error) {
  const text = String(error?.message ?? error);
  // Jest's matcher messages carry ANSI colours; keep the first lines, plain.
  return text.replace(/\x1b\[[0-9;]*m/g, '').split('\n').slice(0, 12).join('\n').slice(0, 2000);
}
