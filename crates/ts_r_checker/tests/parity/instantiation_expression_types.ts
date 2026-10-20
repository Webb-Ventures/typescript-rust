// Instantiation expressions (`f<string>`, `typeof f<string>`) instantiate each generic
// signature taking that many type arguments and drop the others; properties stay.
declare function f<T>(x: T): T[];
declare function g<T>(x: T): T;
declare function g<T, U>(x: T, y: U): U;
declare const C: { new <T>(x: T): { v: T }; s: number };
const a = f<string>;
const b = g<number>;
const c = C<boolean>;
export const n: number = [a, b, c];
export const k: string = c.s;
export function createDeferred<TValue = void>() {
  return { promise: null! as Promise<TValue>, resolve: (v: TValue) => {} };
}
export type Deferred<TValue> = ReturnType<typeof createDeferred<TValue>>;
export function use<THead>(d: Deferred<THead>, h: THead) {
  d.resolve(h);
  d.resolve(1);
  const p: Promise<THead> = d.promise;
  const q: number = d;
}
declare const fd: typeof f<number>;
export const r: string[] = fd(1);
type QF = typeof f<string>;
declare const qf: QF;
export const s: number = qf;
type H = typeof g<string, number, boolean>;
const hh = f<string, number>;
declare const nf: number;
const nn = nf<string>;
