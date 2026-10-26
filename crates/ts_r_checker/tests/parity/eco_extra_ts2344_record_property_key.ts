// Ecosystem check repro (was ecosystem/repros/check/extra-ts2344-record-property-key.d.ts): extra TS2344: Type '…' does not satisfy the constraint '…'.
// @filename: index.d.ts
declare function hasOwn<O, K extends PropertyKey, V = unknown>(o: O, p: K): o is O & Record<K, V>;
export = hasOwn;
