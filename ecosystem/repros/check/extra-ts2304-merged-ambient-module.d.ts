// Cause:    extra TS2304: Cannot find name '…'.
// Found in: @types/node@26.6.5 buffer.buffer.d.ts (WithImplicitCoercion is exported from the
//           `declare module "node:buffer"` block in buffer.d.ts)
// Owner:    ts_r_checker (binder: merged ambient module declarations)
// Expected: tsc 7 merges the two `declare module "lib"` blocks; members exported by one are
//           in scope in the other, so `Coerce` resolves. No error.
// Actual:   ts-r-check reports TS2304 "Cannot find name 'Coerce'."
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
