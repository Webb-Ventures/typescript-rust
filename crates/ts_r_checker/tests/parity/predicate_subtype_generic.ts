// trpc `router.ts` (`isFunction(ctxOrCallback) ? ctxOrCallback() : ...` with
// `ctxOrCallback: TRoot['ctx'] | (() => MaybePromise<TRoot['ctx']>)`): type predicates
// relate members by subtype, so a generic `T['ctx']` (assignable from anything its
// constraint takes) is intersected with the candidate; with no member related, the
// candidate or the whole type wins when one fits the other. An aliased intersection is
// flattened into a larger one.
type AnyFn = ((...args: any[]) => unknown) & Record<keyof any, unknown>;
declare function isFunction(fn: unknown): fn is AnyFn;
interface Root { ctx: object }
function f<TRoot extends Root>(c: TRoot['ctx'] | (() => Promise<TRoot['ctx']>)) {
  if (isFunction(c)) {
    c();
    const n: null = c;
  }
}
function g<T extends object>(c: T | (() => T)) {
  if (isFunction(c)) {
    c();
    const n: null = c;
  }
}
function h(c: object | (() => object)) {
  if (isFunction(c)) {
    const n: null = c;
  }
}
declare const v: string | number | ReadonlyArray<string>;
if (Array.isArray(v)) {
  const n: null = v;
}
declare function isBool(x: unknown): x is boolean;
declare const w: string | number;
if (isBool(w)) {
  const n: null = w;
}
