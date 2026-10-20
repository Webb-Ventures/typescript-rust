// Functions that aren't exported have their throws set inferred; no tag needed.
function check(n: number): number {
  if (n < 0) throw new RangeError("negative");
  return n;
}
function double(n: number): number {
  return check(n) * 2;
}
export const value: number = double(1);
