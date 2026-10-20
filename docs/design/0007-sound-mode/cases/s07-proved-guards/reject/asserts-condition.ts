// asserts cond must not return normally when cond is false.
function check(cond: boolean): asserts cond { // @expect TSR0703
  if (!cond) console.error("check failed");
}
function len(v: string | null): number {
  check(v !== null);
  return v.length;
}
export const n: number = len(null);
