// When this returns false, x can still be a string, but callers would narrow it to number.
function isString(x: string | number): x is string { // @expect TSR0702
  return x === "a";
}
function measure(v: string | number): number {
  return isString(v) ? v.length : v;
}
export const n: number = measure("b");
