// `x === v` where `x` is `unknown` or has a `{}` member (`!!n` of an `unknown`): a
// primitive (or `object`) `v` narrows `x` to `v`'s type, another object type to `object`;
// a union, intersection or type parameter `v` filters as usual. Loose `==` does not.
declare const num: number;
declare const lit: 5;
declare const obj: { a: 1 };
declare const nonp: object;
declare const big: bigint;
declare const fn: () => void;
declare const arr: string[];
declare const sym: symbol;
declare const str: string | undefined;
const isPos = (n: unknown): boolean => !!n && n === Math.floor(n as number) && n > 0;
function f1(n: unknown) {
  if (n === obj) { const x: string = n; }
  if (n === fn) { const x: string = n; }
  if (n === nonp) { const x: string = n; }
  if (n === arr) { const x: string = n; }
  if (n === big) { const x: string = n; }
  if (n === sym) { const x: string = n; }
  if (n === str) { const x: string | undefined = n; }
}
function f2(n: {} | null | undefined) {
  if (n === obj) { const x: string = n; }
  if (n === nonp) { const x: string = n; }
  if (n === lit) { const x: string = n; }
  if (n === num) { const x: string = n; }
  if (n === str) { const x: string = n; }
  if (n == num) { const x: number = n; }
}
function f3(n: {} | string[]) {
  if (n === num) { const x: string = n; }
  if (n === obj) { const x: string = n; }
}
function f4<T>(n: {}, t: T) {
  if (n === t) { const x: string = n; }
}
// The head of an `unknown` source keeps the whole union target.
declare const u: unknown;
const x1: string | undefined = u;
const x2: string | null = u;
const x3: string | undefined = 5;
