// Ecosystem check repro (was ecosystem/repros/check/extra-ts2304-merged-ambient-module.d.ts): extra TS2304: Cannot find name '…'.
// @filename: index.d.ts
/// <reference path="a.d.ts" />
/// <reference path="b.d.ts" />
export {};
// @filename: a.d.ts
declare module 'lib' {
  export type Coerce<T> = T | { valueOf(): T };
}
// @filename: b.d.ts
declare module 'lib' {
  export function from(x: Coerce<number>): number;
}
