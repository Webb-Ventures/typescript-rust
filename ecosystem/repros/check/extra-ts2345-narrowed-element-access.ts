// Cause:    extra TS2345: Argument of type '…' is not assignable to parameter of type '…'.
// Found in: brace-expansion@5.0.12 src/index.ts:227 (tests stage: its `tsc` build, with
//           noUncheckedIndexedAccess), `if (n[0] === undefined) return; numeric(n[0])`
// Owner:    ts_r_checker (narrowing)
// Expected: tsc 7 narrows an element access with a literal index (`n[0]`) like a property
//           access: after the `=== undefined` guard, `n[0]` is `string`. No error.
// Actual:   ts-r-check reports TS2345 "Argument of type 'string | undefined' is not
//           assignable to parameter of type 'string'."
declare function numeric(s: string): number;

export function width(n: (string | undefined)[]): number {
  if (n[0] === undefined || n[1] === undefined) {
    return 0;
  }
  return numeric(n[0]) + numeric(n[1]);
}
