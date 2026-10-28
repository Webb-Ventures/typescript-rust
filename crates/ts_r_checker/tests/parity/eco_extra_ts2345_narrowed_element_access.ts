// Ecosystem check repro (was ecosystem/repros/check/extra-ts2345-narrowed-element-access.ts): extra TS2345: Argument of type '…' is not assignable to parameter of type '…'.
// @filename: index.ts
declare function numeric(s: string): number;

export function width(n: (string | undefined)[]): number {
  if (n[0] === undefined || n[1] === undefined) {
    return 0;
  }
  return numeric(n[0]) + numeric(n[1]);
}
