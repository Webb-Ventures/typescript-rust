// trpc `setTimeout` with the DOM lib and `@types/node`: `declare global` blocks merge after
// script globals; calls try later declaration groups first (`Timeout`), while the last
// signature in declaration order is the augmentation's.
const a: null = g(1 as any);
const b: null = g(() => {});
declare const p: Parameters<typeof g>;
const c: null = p;
export {};
// @filename: m.d.ts
export {};
declare global {
  function g<T extends any[]>(cb: (...a: T) => void): 2;
  function g(cb: (_: void) => void): 3;
}
// @filename: s.d.ts
declare function g(x: string | Function): 1;
