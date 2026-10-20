// A variable widens a unique symbol initializer only at the top level: in a union it stays
// unique, so narrowing `res === timeout` away leaves `Awaited<T>` (tRPC's `withTimeout`,
// stream/sse.ts). `let`/`const` of a bare unique symbol are `symbol`s.
const timeout = Symbol();
declare function race<T extends readonly unknown[] | []>(values: T): Promise<Awaited<T[number]>>;
async function withTimeout<T>(p: Promise<T>, t: Promise<typeof timeout>, on: () => Promise<T>): Promise<T> {
  const res = await race([p, t]);
  if (res === timeout) {
    return await on();
  }
  return res;
}
declare const u: typeof timeout | string;
const c1 = u;
let l1 = u;
let l2 = Math.random() ? timeout : 1;
if (typeof c1 !== "string") {
  const s: typeof timeout = c1;
}
if (typeof l1 !== "string") {
  const s: typeof timeout = l1;
}
if (typeof l2 !== "number") {
  const s: typeof timeout = l2;
}
const c2 = timeout;
let l3 = timeout;
const x1: typeof c2 = Symbol();
const x2: typeof l3 = Symbol();
