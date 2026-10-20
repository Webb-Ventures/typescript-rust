// Cause:    extra TS2344: Type '…' does not satisfy the constraint '…'.
// Found in: hasown@2.0.4 index.d.ts
// Owner:    ts_r_checker
// Expected: tsc 7 reports nothing: `K extends PropertyKey` satisfies Record's
//           `K extends keyof any` (both are string | number | symbol).
// Actual:   ts-r-check reports TS2344 "Type 'K' does not satisfy the constraint
//           'string | number | symbol'."
declare function hasOwn<O, K extends PropertyKey, V = unknown>(o: O, p: K): o is O & Record<K, V>;
export = hasOwn;
