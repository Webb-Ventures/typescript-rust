// typescript-eslint `no-unsafe-argument` oracle case: a tuple alias that refers to itself
// through an array element prints the forward reference by the alias name (it once
// recursed until the stack overflowed).
type T = [number, T[]];
declare function foo(t: T): void;
declare const t: T;
foo(t as any);
const s: string = t[1];
const n: number = t[1][0][0];
export {};
